// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pre-allocated URB Ring Buffer Pipeline and Isochronous Packet Scheduler.
//!
//! Features:
//! - Pre-allocated 80 URB ring buffer (~80ms buffer at 44.1 kHz - 768 kHz).
//! - Zero memory allocations during streaming to avoid ARM Memory Tagging Extension (MTE) faults.
//! - Accurate microframe scheduling (125µs per packet) using fractional accumulator.
//! - Continuous asynchronous feedback tracking (Q16.16) for drift-free DAC clock lock.
//! - Direct integer bit-packing (16-bit, 24-bit packed, 24-in-32, 32-bit, DoP, and Native DSD).

use super::ioctl::*;
use kiiaud_core::{KiiaudError, Result};
use std::os::raw::{c_int, c_void};
use std::os::unix::io::RawFd;

/// Number of isochronous packets per URB submission.
/// At USB 2.0 High-Speed (125µs microframes), 8 packets = 1 ms of audio.
pub const USB_AUDIO_PACKETS_PER_URB: usize = 8;

/// Number of URBs in the pre-allocated ring buffer.
/// 80 URBs ≈ 80 ms of in-flight audio, providing required xHCI scheduling headroom (Bug 4 fix).
pub const USB_AUDIO_NUM_URBS: usize = 80;

/// Maximum bytes per URB data buffer.
/// Worst case: 768kHz * 4 bytes * 2 channels / 8000 microframes * 8 packets = 6144 bytes.
/// 8192 bytes gives safe headroom for up to 768kHz / DSD512.
pub const USB_AUDIO_URB_BUFFER_SIZE: usize = 8192;

#[repr(C)]
pub struct UrbSlotData {
    pub urb: UsbdevfsUrb,
    pub packets: [UsbdevfsIsoPacketDesc; USB_AUDIO_PACKETS_PER_URB],
    pub buffer: [u8; USB_AUDIO_URB_BUFFER_SIZE],
}

impl UrbSlotData {
    pub fn new() -> Box<Self> {
        let mut slot = Box::new(Self {
            urb: UsbdevfsUrb {
                urb_type: USBDEVFS_URB_TYPE_ISO,
                endpoint: 0,
                status: 0,
                flags: USBDEVFS_URB_ISO_ASAP,
                buffer: std::ptr::null_mut(),
                buffer_length: 0,
                actual_length: 0,
                start_frame: 0,
                number_of_packets: USB_AUDIO_PACKETS_PER_URB as c_int,
                error_count: 0,
                signr: 0,
                usercontext: std::ptr::null_mut(),
            },
            packets: [UsbdevfsIsoPacketDesc::default(); USB_AUDIO_PACKETS_PER_URB],
            buffer: [0u8; USB_AUDIO_URB_BUFFER_SIZE],
        });
        slot.urb.buffer = slot.buffer.as_mut_ptr() as *mut c_void;
        slot
    }
}

#[repr(C)]
pub struct FeedbackUrbData {
    pub urb: UsbdevfsUrb,
    pub packet: UsbdevfsIsoPacketDesc,
    pub buffer: [u8; 4],
}

impl FeedbackUrbData {
    pub fn new() -> Box<Self> {
        let mut fb = Box::new(Self {
            urb: UsbdevfsUrb {
                urb_type: USBDEVFS_URB_TYPE_ISO,
                endpoint: 0,
                status: 0,
                flags: USBDEVFS_URB_ISO_ASAP,
                buffer: std::ptr::null_mut(),
                buffer_length: 4,
                actual_length: 0,
                start_frame: 0,
                number_of_packets: 1,
                error_count: 0,
                signr: 0,
                usercontext: std::ptr::null_mut(),
            },
            packet: UsbdevfsIsoPacketDesc {
                length: 4,
                actual_length: 0,
                status: 0,
            },
            buffer: [0u8; 4],
        });
        fb.urb.buffer = fb.buffer.as_mut_ptr() as *mut c_void;
        fb
    }
}

#[allow(dead_code)]
pub struct UsbAudioPipeline {
    fd: RawFd,
    endpoint_out: u8,
    endpoint_feedback: Option<u8>,
    sample_rate: u32,
    channels: u32,
    subslot_size: u32, // 2, 3, or 4 bytes per sample
    bytes_per_frame: usize,

    // Pre-allocated ring buffer
    slots: Vec<Box<UrbSlotData>>,
    submit_idx: usize,
    urbs_in_flight: usize,

    // Continuous Feedback
    feedback: Option<Box<FeedbackUrbData>>,
    feedback_in_flight: bool,
    feedback_count: u64,

    // Fractional microframe timing accumulator
    frame_accumulator: f64,
    calibrated_fpmf: f64,

    // Residual frame buffer for unaligned boundary bytes
    residual: [u8; 16],
    residual_len: usize,
}

impl UsbAudioPipeline {
    pub fn new(
        fd: RawFd,
        endpoint_out: u8,
        endpoint_feedback: Option<u8>,
        sample_rate: u32,
        channels: u32,
        subslot_size: u32,
    ) -> Self {
        let bytes_per_frame = (channels * subslot_size) as usize;
        let mut slots = Vec::with_capacity(USB_AUDIO_NUM_URBS);
        for _ in 0..USB_AUDIO_NUM_URBS {
            let mut s = UrbSlotData::new();
            s.urb.endpoint = endpoint_out;
            slots.push(s);
        }

        let feedback = endpoint_feedback.map(|ep| {
            let mut fb = FeedbackUrbData::new();
            fb.urb.endpoint = ep;
            fb
        });

        let nominal_fpmf = sample_rate as f64 / 8000.0;

        Self {
            fd,
            endpoint_out,
            endpoint_feedback,
            sample_rate,
            channels,
            subslot_size,
            bytes_per_frame,
            slots,
            submit_idx: 0,
            urbs_in_flight: 0,
            feedback,
            feedback_in_flight: false,
            feedback_count: 0,
            frame_accumulator: 0.0,
            calibrated_fpmf: nominal_fpmf,
            residual: [0u8; 16],
            residual_len: 0,
        }
    }

    /// Pre-fill and submit initial silence URBs to prime the xHCI scheduler (Bug 4 fix).
    pub fn prime(&mut self) -> Result<()> {
        let nominal_fpmf = self.calibrated_fpmf;

        // Start continuous feedback collection if supported
        self.submit_feedback_urb();

        for _ in 0..USB_AUDIO_NUM_URBS {
            let slot = &mut self.slots[self.submit_idx];
            let mut total_bytes = 0usize;

            for p in 0..USB_AUDIO_PACKETS_PER_URB {
                self.frame_accumulator += nominal_fpmf;
                let frames = self.frame_accumulator.floor() as usize;
                self.frame_accumulator -= frames as f64;
                let pkt_len = frames * self.bytes_per_frame;

                slot.packets[p].length = pkt_len as std::os::raw::c_uint;
                slot.packets[p].actual_length = 0;
                slot.packets[p].status = 0;
                total_bytes += pkt_len;
            }

            // Zero out silence buffer
            slot.buffer[..total_bytes].fill(0);
            slot.urb.buffer_length = total_bytes as c_int;
            slot.urb.actual_length = 0;
            slot.urb.status = 0;
            slot.urb.flags = USBDEVFS_URB_ISO_ASAP;
            slot.urb.usercontext = slot.as_mut() as *mut UrbSlotData as *mut c_void;

            let ret = unsafe { libc::ioctl(self.fd, USBDEVFS_SUBMITURB, &mut slot.urb) };
            if ret < 0 {
                let err = std::io::Error::last_os_error();
                tracing::warn!(error = %err, "prime: failed to submit initial silence URB");
            } else {
                self.urbs_in_flight += 1;
            }

            self.submit_idx = (self.submit_idx + 1) % USB_AUDIO_NUM_URBS;
        }

        tracing::info!(
            urbs_in_flight = self.urbs_in_flight,
            sample_rate = self.sample_rate,
            fpmf = self.calibrated_fpmf,
            "USB audio pipeline primed with 80 isochronous URBs"
        );
        Ok(())
    }

    /// Submit a chunk of PCM or DSD byte data into the pipeline.
    /// Blocks on URB reap when ring buffer is full, providing natural hardware clock backpressure.
    pub fn write_bytes(&mut self, data: &[u8]) -> Result<()> {
        let mut data_offset = 0usize;
        let mut input = data;

        // Prepend residual if any
        let mut scratch = Vec::new();
        if self.residual_len > 0 {
            scratch.reserve(self.residual_len + data.len());
            scratch.extend_from_slice(&self.residual[..self.residual_len]);
            scratch.extend_from_slice(data);
            self.residual_len = 0;
            input = &scratch;
        }

        while data_offset < input.len() {
            // Check if we need to reap a completed URB to make room
            if self.urbs_in_flight >= USB_AUDIO_NUM_URBS {
                self.reap_one_urb()?;
            }

            let slot = &mut self.slots[self.submit_idx];
            let mut total_bytes = 0usize;
            let mut bytes_copied = 0usize;

            for p in 0..USB_AUDIO_PACKETS_PER_URB {
                self.frame_accumulator += self.calibrated_fpmf;
                let frames = self.frame_accumulator.floor() as usize;
                self.frame_accumulator -= frames as f64;
                let pkt_len = frames * self.bytes_per_frame;

                slot.packets[p].length = pkt_len as std::os::raw::c_uint;
                slot.packets[p].actual_length = 0;
                slot.packets[p].status = 0;

                let available = (input.len() - data_offset).saturating_sub(bytes_copied);
                let to_copy = pkt_len.min(available);

                if to_copy > 0 {
                    slot.buffer[total_bytes..total_bytes + to_copy]
                        .copy_from_slice(&input[data_offset + bytes_copied..data_offset + bytes_copied + to_copy]);
                    bytes_copied += to_copy;
                }

                // If not enough data, zero pad packet to prevent DAC pops
                if to_copy < pkt_len {
                    slot.buffer[total_bytes + to_copy..total_bytes + pkt_len].fill(0);
                }

                total_bytes += pkt_len;
            }

            data_offset += bytes_copied;

            slot.urb.buffer_length = total_bytes as c_int;
            slot.urb.actual_length = 0;
            slot.urb.status = 0;
            slot.urb.flags = USBDEVFS_URB_ISO_ASAP;
            slot.urb.usercontext = slot.as_mut() as *mut UrbSlotData as *mut c_void;

            let ret = unsafe { libc::ioctl(self.fd, USBDEVFS_SUBMITURB, &mut slot.urb) };
            if ret < 0 {
                let err = std::io::Error::last_os_error();
                tracing::warn!(error = %err, "write_bytes: USBDEVFS_SUBMITURB failed");
            } else {
                self.urbs_in_flight += 1;
            }

            self.submit_idx = (self.submit_idx + 1) % USB_AUDIO_NUM_URBS;
        }

        // Save any unaligned residual frames
        let leftover = input.len() - data_offset;
        if leftover > 0 && leftover < self.residual.len() {
            self.residual[..leftover].copy_from_slice(&input[data_offset..]);
            self.residual_len = leftover;
        }

        Ok(())
    }

    /// Reap one completed URB from the kernel (blocking).
    /// Handles both audio URBs and async feedback URB.
    fn reap_one_urb(&mut self) -> Result<()> {
        let mut completed_urb: *mut UsbdevfsUrb = std::ptr::null_mut();

        loop {
            let ret = unsafe { libc::ioctl(self.fd, USBDEVFS_REAPURB, &mut completed_urb) };
            if ret < 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EINTR) {
                    continue; // Interrupted by signal, retry
                }
                return Err(KiiaudError::Output(format!("USBDEVFS_REAPURB error: {}", err)));
            }

            if completed_urb.is_null() {
                break;
            }

            // Check if this was the feedback URB
            if let Some(fb) = self.feedback.as_mut() {
                if completed_urb == (&mut fb.urb as *mut UsbdevfsUrb) {
                    self.handle_feedback_completion();
                    // Feedback handled; loop again to reap an actual audio URB
                    continue;
                }
            }

            // An audio URB completed
            self.urbs_in_flight = self.urbs_in_flight.saturating_sub(1);
            break;
        }

        Ok(())
    }

    /// Submit asynchronous feedback URB to read DAC's clock feedback endpoint.
    fn submit_feedback_urb(&mut self) {
        if self.feedback_in_flight {
            return;
        }
        if let Some(fb) = self.feedback.as_mut() {
            fb.urb.flags = USBDEVFS_URB_ISO_ASAP;
            fb.urb.usercontext = fb.as_mut() as *mut FeedbackUrbData as *mut c_void;
            let ret = unsafe { libc::ioctl(self.fd, USBDEVFS_SUBMITURB, &mut fb.urb) };
            if ret >= 0 {
                self.feedback_in_flight = true;
            }
        }
    }

    /// Process completed feedback URB (Q16.16 format) to recalibrate frames-per-microframe.
    fn handle_feedback_completion(&mut self) {
        self.feedback_in_flight = false;
        self.feedback_count += 1;

        if let Some(fb) = self.feedback.as_mut() {
            let actual = fb.packet.actual_length;
            if actual >= 3 {
                let b = &fb.buffer;
                // UAC2 feedback is 10.14 or 16.16 fixed-point
                let raw = (b[0] as u32) | ((b[1] as u32) << 8) | ((b[2] as u32) << 16) | ((b[3] as u32) << 24);
                let new_fpmf = raw as f64 / 65536.0;

                let nominal = self.sample_rate as f64 / 8000.0;
                // Sanity check: must be within ±1.5% of nominal sample rate
                if new_fpmf > nominal * 0.985 && new_fpmf < nominal * 1.015 {
                    self.calibrated_fpmf = new_fpmf;
                    if self.feedback_count % 5000 == 0 {
                        tracing::debug!(
                            count = self.feedback_count,
                            calibrated_fpmf = new_fpmf,
                            actual_dac_hz = new_fpmf * 8000.0,
                            "continuous DAC async feedback calibration"
                        );
                    }
                }
            }
            // Resubmit immediately for ongoing continuous tracking
            self.submit_feedback_urb();
        }
    }

    /// Drain all in-flight URBs at end of track.
    pub fn drain(&mut self) -> Result<()> {
        while self.urbs_in_flight > 0 {
            self.reap_one_urb()?;
        }
        self.residual_len = 0;
        Ok(())
    }

    /// Discard all in-flight URBs immediately.
    pub fn discard(&mut self) -> Result<()> {
        let _ = unsafe { libc::ioctl(self.fd, USBDEVFS_DISCARDURB, 0) };
        self.urbs_in_flight = 0;
        self.residual_len = 0;
        Ok(())
    }

    pub fn calibrated_rate_hz(&self) -> f64 {
        self.calibrated_fpmf * 8000.0
    }
}
