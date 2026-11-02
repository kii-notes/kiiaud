// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pure Rust USB Audio Class 2.0 (UAC2) Direct Bit-Perfect Backend for Android.
//!
//! Directly communicates with USB DACs via Linux `usbdevfs` ioctls, bypassing
//! Android AudioFlinger, AudioTrack, AAudio/Oboe, ALSA user-space, and kernel `snd-usb-audio`.

pub mod control;
pub mod descriptor;
pub mod ioctl;
pub mod pipeline;

use control::*;
use descriptor::*;
use pipeline::*;

use kiiaud_core::{AudioBuffer, AudioFormat, DsdMode, DsdTransport, KiiaudError, Result, SampleFormat};
use kiiaud_output::dop::DopEncoder;
use std::fs::File;
use std::os::unix::io::{FromRawFd, IntoRawFd, RawFd};

pub struct UsbAudioStream {
    fd: RawFd,
    owns_fd: bool,
    device_info: ParsedUsbAudioDevice,
    active_alt: AltSettingInfo,
    pipeline: Option<UsbAudioPipeline>,
    dop_encoder: Option<DopEncoder>,
    dop_scratch: Vec<i32>,
    byte_scratch: Vec<u8>,
}

impl UsbAudioStream {
    /// Open a USB Audio stream from an existing file descriptor (e.g. from Android `UsbDeviceConnection`)
    /// or a raw USB device node path (`/dev/bus/usb/xxx/yyy`).
    pub fn open_device(device_spec: &str) -> Result<Self> {
        let (fd, owns_fd) = if let Some(fd_str) = device_spec.strip_prefix("fd:") {
            let parsed_fd: RawFd = fd_str
                .parse()
                .map_err(|e| KiiaudError::Output(format!("Invalid file descriptor '{}': {}", fd_str, e)))?;
            (parsed_fd, false)
        } else if device_spec.starts_with('/') {
            // Raw devfs path
            let file = File::options()
                .read(true)
                .write(true)
                .open(device_spec)
                .map_err(|e| KiiaudError::Output(format!("Failed to open USB device '{}': {}", device_spec, e)))?;
            (file.into_raw_fd(), true)
        } else {
            // Default fallback: scan /dev/bus/usb or return error
            return Err(KiiaudError::Output(format!(
                "USB device specification '{}' not recognized. Use 'fd:<number>' or '/dev/bus/usb/xxx/yyy'",
                device_spec
            )));
        };

        // Read raw descriptors using USBDEVFS_CONTROL GET_DESCRIPTOR
        let raw_descriptors = read_raw_descriptors(fd)?;
        let device_info = ParsedUsbAudioDevice::parse(&raw_descriptors)?;

        let best_alt = device_info.best_pcm_alt.clone().ok_or_else(|| {
            KiiaudError::Output("No suitable PCM AudioStreaming alternate setting found on USB DAC".to_owned())
        })?;

        tracing::info!(
            clock_id = device_info.clock_source_id,
            control_iface = device_info.control_interface,
            streaming_iface = device_info.streaming_interface,
            alt_setting = best_alt.alt_setting,
            bits = best_alt.bit_resolution,
            subslot = best_alt.subslot_size,
            ep_out = best_alt.endpoint_out,
            ep_feedback = ?best_alt.endpoint_feedback,
            "USB DAC descriptors parsed successfully for direct bit-perfect output"
        );

        // Claim interfaces
        let _ = claim_interface(fd, device_info.control_interface);
        let _ = claim_interface(fd, device_info.streaming_interface);

        Ok(Self {
            fd,
            owns_fd,
            device_info,
            active_alt: best_alt,
            pipeline: None,
            dop_encoder: None,
            dop_scratch: Vec::new(),
            byte_scratch: Vec::new(),
        })
    }

    /// Configure and start the audio stream for the given format.
    pub fn configure(&mut self, format: &AudioFormat, dsd_mode: DsdMode) -> Result<AudioFormat> {
        let (sample_rate, channels, is_dsd) = match *format {
            AudioFormat::Pcm { sample_rate, channels, .. } => (sample_rate, channels as u32, false),
            AudioFormat::Dsd { rate, channels, bit_order, layout, .. } => {
                if dsd_mode == DsdMode::Native && self.device_info.dsd_native_alt.is_some() {
                    // Native DSD via dedicated alt setting
                    (rate.hz(), channels as u32, true)
                } else {
                    // DoP (DSD over PCM) transport
                    let dop_rate = rate.dop_pcm_rate_hz();
                    let enc = DopEncoder::new(rate, channels as usize, layout, bit_order)?;
                    self.dop_encoder = Some(enc);
                    (dop_rate, channels as u32, false)
                }
            }
        };

        // 1. Set interface to alt 0 (zero-bandwidth idle) before clock change
        let _ = set_interface(self.fd, self.device_info.streaming_interface, 0);

        // 2. Set hardware clock frequency on DAC PLL (Bug 1 fix)
        set_uac2_sample_rate(
            self.fd,
            self.device_info.control_interface,
            self.device_info.clock_source_id,
            sample_rate,
        )?;

        // 3. Switch to active streaming alt setting
        let target_alt = if is_dsd {
            self.device_info
                .dsd_native_alt
                .as_ref()
                .unwrap_or(&self.active_alt)
                .clone()
        } else {
            self.active_alt.clone()
        };

        set_interface(self.fd, self.device_info.streaming_interface, target_alt.alt_setting)?;

        // 4. Create and prime the 80 URB pipeline (Bug 4 fix)
        let mut pipe = UsbAudioPipeline::new(
            self.fd,
            target_alt.endpoint_out,
            target_alt.endpoint_feedback,
            sample_rate,
            channels,
            target_alt.subslot_size as u32,
        );
        pipe.prime()?;
        self.pipeline = Some(pipe);

        let negotiated = if is_dsd {
            AudioFormat::Dsd {
                rate: match format {
                    AudioFormat::Dsd { rate, .. } => *rate,
                    _ => kiiaud_core::format::DsdRate::Dsd64,
                },
                channels: channels as u16,
                transport: DsdTransport::Native,
                bit_order: kiiaud_core::BitOrder::MsbFirst,
                layout: kiiaud_core::ChannelLayout::Interleaved,
            }
        } else if self.dop_encoder.is_some() {
            AudioFormat::Dsd {
                rate: match format {
                    AudioFormat::Dsd { rate, .. } => *rate,
                    _ => kiiaud_core::format::DsdRate::Dsd64,
                },
                channels: channels as u16,
                transport: DsdTransport::DoP,
                bit_order: kiiaud_core::BitOrder::MsbFirst,
                layout: kiiaud_core::ChannelLayout::Interleaved,
            }
        } else {
            AudioFormat::Pcm {
                sample_rate,
                channels: channels as u16,
                bits_per_sample: target_alt.bit_resolution,
                sample_format: match target_alt.bit_resolution {
                    16 => SampleFormat::S16Le,
                    24 if target_alt.subslot_size == 3 => SampleFormat::S24_3Le,
                    24 => SampleFormat::S24Le,
                    _ => SampleFormat::S32Le,
                },
            }
        };

        Ok(negotiated)
    }

    /// Write an audio buffer (PCM or DSD) to the USB output.
    pub fn write(&mut self, buffer: &AudioBuffer) -> Result<usize> {
        let pipe = self
            .pipeline
            .as_mut()
            .ok_or_else(|| KiiaudError::Output("USB pipeline is not active".to_owned()))?;

        self.byte_scratch.clear();

        match buffer {
            AudioBuffer::PcmI32 { samples, .. } => {
                pack_i32_to_usb_bytes(samples, self.active_alt.subslot_size, &mut self.byte_scratch);
            }
            AudioBuffer::PcmF32 { samples, .. } => {
                pack_f32_to_usb_bytes(samples, self.active_alt.subslot_size, &mut self.byte_scratch);
            }
            AudioBuffer::Dsd { data, .. } => {
                if let Some(enc) = self.dop_encoder.as_mut() {
                    enc.encode(data, &mut self.dop_scratch);
                    pack_i32_to_usb_bytes(
                        &self.dop_scratch,
                        self.active_alt.subslot_size,
                        &mut self.byte_scratch,
                    );
                } else {
                    // Raw DSD native bitstream
                    self.byte_scratch.extend_from_slice(data);
                }
            }
        }

        pipe.write_bytes(&self.byte_scratch)?;
        Ok(buffer.frames())
    }

    pub fn drain(&mut self) -> Result<()> {
        if let Some(pipe) = self.pipeline.as_mut() {
            pipe.drain()?;
        }
        Ok(())
    }

    pub fn stop(&mut self) -> Result<()> {
        if let Some(mut pipe) = self.pipeline.take() {
            let _ = pipe.discard();
        }
        let _ = set_interface(self.fd, self.device_info.streaming_interface, 0);
        self.dop_encoder = None;
        Ok(())
    }

    pub fn device_info(&self) -> &ParsedUsbAudioDevice {
        &self.device_info
    }
}

impl Drop for UsbAudioStream {
    fn drop(&mut self) {
        let _ = self.stop();
        let _ = release_interface(self.fd, self.device_info.streaming_interface);
        let _ = release_interface(self.fd, self.device_info.control_interface);
        if self.owns_fd {
            unsafe {
                let _ = File::from_raw_fd(self.fd);
            }
        }
    }
}

// ── Raw Descriptor Reader ───────────────────────────────────────────────────

fn read_raw_descriptors(fd: RawFd) -> Result<Vec<u8>> {
    // Read USB Configuration Descriptor (0x0200)
    let mut buf = vec![0u8; 4096];
    let mut transfer = ioctl::UsbdevfsCtrltransfer {
        bRequestType: 0x80, // Device to Host, Standard, Device
        bRequest: 0x06,     // GET_DESCRIPTOR
        wValue: 0x0200,     // Configuration descriptor 0
        wIndex: 0,
        wLength: buf.len() as u16,
        timeout: 1000,
        data: buf.as_mut_ptr() as *mut _,
    };

    let ret = unsafe { libc::ioctl(fd, ioctl::USBDEVFS_CONTROL, &mut transfer) };
    if ret < 0 {
        let err = std::io::Error::last_os_error();
        return Err(KiiaudError::Output(format!(
            "Failed to read USB configuration descriptors via ioctl: {}",
            err
        )));
    }

    let actual_len = ret as usize;
    buf.truncate(actual_len);
    Ok(buf)
}

// ── Bit-Exact Sample Packing ────────────────────────────────────────────────

fn pack_i32_to_usb_bytes(samples: &[i32], subslot_size: u8, out: &mut Vec<u8>) {
    match subslot_size {
        2 => {
            out.reserve(samples.len() * 2);
            for &s in samples {
                let s16 = (s >> 16) as i16;
                out.extend_from_slice(&s16.to_le_bytes());
            }
        }
        3 => {
            out.reserve(samples.len() * 3);
            for &s in samples {
                let s24 = s >> 8;
                out.push((s24 & 0xFF) as u8);
                out.push(((s24 >> 8) & 0xFF) as u8);
                out.push(((s24 >> 16) & 0xFF) as u8);
            }
        }
        _ => {
            // 4 bytes: S24_IN_32 or S32
            out.reserve(samples.len() * 4);
            for &s in samples {
                out.extend_from_slice(&s.to_le_bytes());
            }
        }
    }
}

fn pack_f32_to_usb_bytes(samples: &[f32], subslot_size: u8, out: &mut Vec<u8>) {
    match subslot_size {
        2 => {
            out.reserve(samples.len() * 2);
            for &s in samples {
                let clamped = s.clamp(-1.0, 1.0);
                let i = (clamped * 32767.0) as i16;
                out.extend_from_slice(&i.to_le_bytes());
            }
        }
        3 => {
            out.reserve(samples.len() * 3);
            for &s in samples {
                let clamped = s.clamp(-1.0, 1.0);
                let i = (clamped * 8_388_607.0) as i32;
                out.push((i & 0xFF) as u8);
                out.push(((i >> 8) & 0xFF) as u8);
                out.push(((i >> 16) & 0xFF) as u8);
            }
        }
        _ => {
            out.reserve(samples.len() * 4);
            for &s in samples {
                let clamped = s.clamp(-1.0, 1.0);
                let i = (clamped as f64 * 2_147_483_647.0) as i32;
                out.extend_from_slice(&i.to_le_bytes());
            }
        }
    }
}
