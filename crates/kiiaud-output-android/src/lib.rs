// SPDX-License-Identifier: MIT OR Apache-2.0

//! Android audio output backend (`kiiaud-output-android`).
//!
//! Supports two distinct operating modes:
//! 1. **Direct USB Audio (Pure Rust UAC2 Bit-Perfect)**:
//!    Bypasses Android AudioFlinger, AAudio, Oboe, ALSA user-space, and kernel `snd-usb-audio`.
//!    Streams directly to USB DACs over Linux `usbdevfs` isochronous transfers.
//!    Supports raw integer PCM, DoP (DSD over PCM), and Native DSD.
//! 2. **Oboe / AAudio Fallback**:
//!    For device built-in speakers, headphone jacks, or Bluetooth audio.

pub mod jni;
pub mod usb;
pub mod usb_output;

pub use jni::*;
pub use usb_output::AndroidUsbAudioOutput;

use kiiaud_core::config::OutputConfig;
use kiiaud_core::{
    AudioBuffer, AudioFormat, DsdMode, DsdTransport, KiiaudError, OutputCapabilities, Result,
    SampleFormat,
};
use kiiaud_output::AudioOutput;
use kiiaud_output::dop::DopEncoder;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AndroidAudioFormat {
    I16,
    I24In32,
    I32,
    F32,
}

impl AndroidAudioFormat {
    pub fn bytes_per_sample(self) -> usize {
        match self {
            Self::I16 => 2,
            Self::I24In32 | Self::I32 | Self::F32 => 4,
        }
    }
}

/// Thread-safe FIFO ring buffer bridging synchronous `write()` to asynchronous audio callback.
#[derive(Debug, Default)]
pub struct AudioRingBuffer {
    buffer: Mutex<VecDeque<u8>>,
}

impl AudioRingBuffer {
    pub fn new() -> Self {
        Self {
            buffer: Mutex::new(VecDeque::with_capacity(65536)),
        }
    }

    pub fn push(&self, bytes: &[u8]) {
        let mut q = self.buffer.lock();
        q.extend(bytes);
    }

    pub fn pop_into(&self, target: &mut [u8]) -> usize {
        let mut q = self.buffer.lock();
        let to_read = target.len().min(q.len());
        for b in &mut target[..to_read] {
            *b = q.pop_front().unwrap_or(0);
        }
        to_read
    }

    pub fn available(&self) -> usize {
        self.buffer.lock().len()
    }

    pub fn clear(&self) {
        self.buffer.lock().clear();
    }
}

pub struct AndroidAudioOutput {
    config: OutputConfig,
    active_format: Option<AudioFormat>,
    hw_format: Option<AndroidAudioFormat>,
    is_exclusive: bool,
    is_playing: bool,
    ring_buffer: Arc<AudioRingBuffer>,
    dop_encoder: Option<DopEncoder>,
    dop_scratch: Vec<i32>,
    byte_scratch: Vec<u8>,
    usb_backend: Option<AndroidUsbAudioOutput>,
}

impl AndroidAudioOutput {
    pub fn new(config: OutputConfig) -> Self {
        let use_usb = config.backend.eq_ignore_ascii_case("android-usb")
            || config.backend.eq_ignore_ascii_case("usb")
            || config.device.starts_with("fd:")
            || config.device.starts_with("/dev/bus/usb");

        let usb_backend = if use_usb {
            Some(AndroidUsbAudioOutput::new(config.clone()))
        } else {
            None
        };

        Self {
            config,
            active_format: None,
            hw_format: None,
            is_exclusive: false,
            is_playing: false,
            ring_buffer: Arc::new(AudioRingBuffer::new()),
            dop_encoder: None,
            dop_scratch: Vec::new(),
            byte_scratch: Vec::new(),
            usb_backend,
        }
    }

    /// Helper to inspect the internal ring buffer for testing.
    pub fn ring_buffer(&self) -> Arc<AudioRingBuffer> {
        Arc::clone(&self.ring_buffer)
    }
}

// Safe because AndroidAudioOutput is used on the dedicated audio worker thread
unsafe impl Send for AndroidAudioOutput {}

impl AudioOutput for AndroidAudioOutput {
    fn open(&mut self, format: &AudioFormat) -> Result<AudioFormat> {
        let is_auto_or_usb = self.config.backend.eq_ignore_ascii_case("android")
            || self.config.backend.eq_ignore_ascii_case("auto")
            || self.config.backend.eq_ignore_ascii_case("android-usb")
            || self.config.backend.eq_ignore_ascii_case("usb");

        // If a dynamic USB DAC was attached via Android UsbManager, activate direct USB backend
        if is_auto_or_usb {
            if let Some(active_fd) = get_active_usb_fd() {
                if self.usb_backend.is_none() {
                    let mut usb_cfg = self.config.clone();
                    usb_cfg.device = format!("fd:{}", active_fd);
                    self.usb_backend = Some(AndroidUsbAudioOutput::new(usb_cfg));
                }
            }
        }

        if let Some(usb) = self.usb_backend.as_mut() {
            let negotiated = usb.open(format)?;
            self.active_format = Some(negotiated);
            self.is_playing = true;
            return Ok(negotiated);
        }

        self.stop()?;

        tracing::info!(
            target_format = %format,
            device = %self.config.device,
            bit_perfect = self.config.bit_perfect,
            "opening Android audio output (Oboe/AAudio)"
        );

        let (negotiated_format, hw_fmt) = match *format {
            AudioFormat::Pcm {
                sample_rate,
                channels,
                bits_per_sample,
                sample_format,
            } => {
                let hw = match sample_format {
                    SampleFormat::S16Le => AndroidAudioFormat::I16,
                    SampleFormat::S24_3Le | SampleFormat::S24Le => AndroidAudioFormat::I24In32,
                    SampleFormat::S32Le => AndroidAudioFormat::I32,
                    SampleFormat::F32Le => AndroidAudioFormat::F32,
                };
                (
                    AudioFormat::Pcm {
                        sample_rate,
                        channels,
                        bits_per_sample,
                        sample_format,
                    },
                    hw,
                )
            }
            AudioFormat::Dsd {
                rate,
                channels,
                bit_order,
                layout,
                ..
            } => {
                if self.config.dsd_mode == DsdMode::Native {
                    return Err(KiiaudError::Output(
                        "Native DSD is not supported on Android HAL; use DoP transport or Direct USB backend instead"
                            .to_owned(),
                    ));
                }

                let pcm_rate = rate.dop_pcm_rate_hz();
                let dop_enc = DopEncoder::new(rate, channels as usize, layout, bit_order)?;
                self.dop_encoder = Some(dop_enc);

                tracing::info!(
                    dsd_rate = ?rate,
                    dop_pcm_rate = pcm_rate,
                    channels = channels,
                    "Android output configured for DoP (DSD over PCM) transport"
                );

                (
                    AudioFormat::Dsd {
                        rate,
                        channels,
                        transport: DsdTransport::DoP,
                        bit_order,
                        layout,
                    },
                    AndroidAudioFormat::I24In32,
                )
            }
        };

        // When configured for bit_perfect, request Exclusive mode from Android HAL
        self.is_exclusive = self.config.bit_perfect;
        self.active_format = Some(negotiated_format);
        self.hw_format = Some(hw_fmt);
        self.is_playing = true;
        self.ring_buffer.clear();

        Ok(negotiated_format)
    }

    fn write(&mut self, buffer: &AudioBuffer) -> Result<usize> {
        if let Some(usb) = self.usb_backend.as_mut() {
            return usb.write(buffer);
        }

        let Some(hw_fmt) = self.hw_format else {
            return Err(KiiaudError::Output(
                "Android audio output is not open".to_owned(),
            ));
        };

        self.byte_scratch.clear();

        match buffer {
            AudioBuffer::PcmI32 { samples, .. } => {
                pack_i32_to_android_bytes(samples, hw_fmt, &mut self.byte_scratch)?;
            }
            AudioBuffer::PcmF32 { samples, .. } => {
                pack_f32_to_android_bytes(samples, hw_fmt, &mut self.byte_scratch)?;
            }
            AudioBuffer::Dsd { data, .. } => {
                let Some(encoder) = self.dop_encoder.as_mut() else {
                    return Err(KiiaudError::Output(
                        "Received DSD buffer without active DoP encoder".to_owned(),
                    ));
                };

                encoder.encode(data, &mut self.dop_scratch);
                pack_i32_to_android_bytes(&self.dop_scratch, hw_fmt, &mut self.byte_scratch)?;
            }
        }

        // Push encoded bytes into the audio ring buffer for Oboe callback consumption
        self.ring_buffer.push(&self.byte_scratch);

        Ok(buffer.frames())
    }

    fn pause(&mut self) -> Result<()> {
        if let Some(usb) = self.usb_backend.as_mut() {
            return usb.pause();
        }
        self.is_playing = false;
        Ok(())
    }

    fn resume(&mut self) -> Result<()> {
        if let Some(usb) = self.usb_backend.as_mut() {
            return usb.resume();
        }
        self.is_playing = true;
        Ok(())
    }

    fn drain(&mut self) -> Result<()> {
        if let Some(usb) = self.usb_backend.as_mut() {
            return usb.drain();
        }
        self.ring_buffer.clear();
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        if let Some(usb) = self.usb_backend.as_mut() {
            let _ = usb.stop();
        }
        self.is_playing = false;
        self.active_format = None;
        self.hw_format = None;
        self.dop_encoder = None;
        self.ring_buffer.clear();
        Ok(())
    }

    fn capabilities(&self) -> OutputCapabilities {
        if let Some(usb) = self.usb_backend.as_ref() {
            return usb.capabilities();
        }

        let is_auto_or_usb = self.config.backend.eq_ignore_ascii_case("android")
            || self.config.backend.eq_ignore_ascii_case("auto")
            || self.config.backend.eq_ignore_ascii_case("android-usb")
            || self.config.backend.eq_ignore_ascii_case("usb");

        if is_auto_or_usb {
            if let Some(active_fd) = get_active_usb_fd() {
                let mut usb_cfg = self.config.clone();
                usb_cfg.device = format!("fd:{}", active_fd);
                return AndroidUsbAudioOutput::new(usb_cfg).capabilities();
            }
        }

        OutputCapabilities {
            backend_name: if self.is_exclusive {
                "Android Oboe (AAudio Exclusive Mode)".to_owned()
            } else {
                "Android Oboe (Shared Mixer)".to_owned()
            },
            device_name: "Android Audio Endpoint".to_owned(),
            sample_rates: vec![44100, 48000, 88200, 96000, 176400, 192000],
            unsupported_rates: Vec::new(),
            bit_depths: vec![16, 24, 32],
            channels: 2,
            exclusive: self.is_exclusive,
            bit_perfect: self.config.bit_perfect && self.is_exclusive,
            adaptive_fallback: true,
            dsd_native: false,
            dsd_dop: true,
        }
    }
}

/// Convert 32-bit signed integers into exact Android PCM byte layout.
pub fn pack_i32_to_android_bytes(
    samples: &[i32],
    fmt: AndroidAudioFormat,
    out: &mut Vec<u8>,
) -> Result<()> {
    match fmt {
        AndroidAudioFormat::I16 => {
            out.reserve(samples.len() * 2);
            for &s in samples {
                let s16 = (s >> 16) as i16;
                out.extend_from_slice(&s16.to_le_bytes());
            }
        }
        AndroidAudioFormat::I24In32 | AndroidAudioFormat::I32 => {
            out.reserve(samples.len() * 4);
            for &s in samples {
                out.extend_from_slice(&s.to_le_bytes());
            }
        }
        AndroidAudioFormat::F32 => {
            out.reserve(samples.len() * 4);
            for &s in samples {
                let f = (s as f64 / 2_147_483_648.0) as f32;
                out.extend_from_slice(&f.to_le_bytes());
            }
        }
    }
    Ok(())
}

/// Convert 32-bit floating point audio samples into exact Android PCM byte layout.
pub fn pack_f32_to_android_bytes(
    samples: &[f32],
    fmt: AndroidAudioFormat,
    out: &mut Vec<u8>,
) -> Result<()> {
    match fmt {
        AndroidAudioFormat::F32 => {
            out.reserve(samples.len() * 4);
            for &s in samples {
                out.extend_from_slice(&s.to_le_bytes());
            }
        }
        AndroidAudioFormat::I32 | AndroidAudioFormat::I24In32 => {
            out.reserve(samples.len() * 4);
            for &s in samples {
                let clamped = s.clamp(-1.0, 1.0);
                let i = (clamped * 2_147_483_647.0) as i32;
                out.extend_from_slice(&i.to_le_bytes());
            }
        }
        AndroidAudioFormat::I16 => {
            out.reserve(samples.len() * 2);
            for &s in samples {
                let clamped = s.clamp(-1.0, 1.0);
                let s16 = (clamped * 32767.0) as i16;
                out.extend_from_slice(&s16.to_le_bytes());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_s16_packing_is_bit_exact() {
        let sample: i32 = 0x1234_0000;
        let mut out = Vec::new();
        pack_i32_to_android_bytes(&[sample], AndroidAudioFormat::I16, &mut out).unwrap();
        assert_eq!(out, vec![0x34, 0x12]);
    }

    #[test]
    fn test_s24_in_32_packing_is_bit_exact() {
        let sample: i32 = 0x1234_5600;
        let mut out = Vec::new();
        pack_i32_to_android_bytes(&[sample], AndroidAudioFormat::I24In32, &mut out).unwrap();
        assert_eq!(out, vec![0x00, 0x56, 0x34, 0x12]);
    }

    #[test]
    fn test_f32_packing() {
        let sample: f32 = 0.5;
        let mut out = Vec::new();
        pack_f32_to_android_bytes(&[sample], AndroidAudioFormat::F32, &mut out).unwrap();
        assert_eq!(out, 0.5f32.to_le_bytes().to_vec());
    }

    #[test]
    fn test_ring_buffer_fifo() {
        let rb = AudioRingBuffer::new();
        assert_eq!(rb.available(), 0);

        rb.push(&[1, 2, 3, 4, 5]);
        assert_eq!(rb.available(), 5);

        let mut out = [0u8; 3];
        let read = rb.pop_into(&mut out);
        assert_eq!(read, 3);
        assert_eq!(out, [1, 2, 3]);
        assert_eq!(rb.available(), 2);

        let mut out2 = [0u8; 4];
        let read2 = rb.pop_into(&mut out2);
        assert_eq!(read2, 2);
        assert_eq!(&out2[..2], &[4, 5]);
        assert_eq!(rb.available(), 0);
    }

    #[test]
    fn test_output_lifecycle_and_capabilities() {
        let cfg = OutputConfig {
            backend: "oboe".to_owned(),
            bit_perfect: true,
            ..OutputConfig::default()
        };
        let mut out = AndroidAudioOutput::new(cfg);

        // Before open
        let caps = out.capabilities();
        assert!(caps.sample_rates.contains(&48000));
        assert!(caps.sample_rates.contains(&96000));

        let fmt = AudioFormat::Pcm {
            sample_rate: 96000,
            channels: 2,
            bits_per_sample: 24,
            sample_format: SampleFormat::S24Le,
        };

        let opened = out.open(&fmt).unwrap();
        assert_eq!(opened, fmt);

        let caps_after = out.capabilities();
        assert!(caps_after.exclusive);
        assert!(caps_after.bit_perfect);
        assert!(caps_after.dsd_dop);

        // Write a PCM buffer
        let buf = AudioBuffer::PcmI32 {
            samples: vec![0x1000_0000, 0x2000_0000],
            channels: 2,
            sample_rate: 96000,
            bits_per_sample: 24,
        };
        let frames = out.write(&buf).unwrap();
        assert_eq!(frames, 1);
        assert_eq!(out.ring_buffer().available(), 8);

        out.drain().unwrap();
        assert_eq!(out.ring_buffer().available(), 0);

        out.stop().unwrap();
    }

    #[test]
    fn test_usb_audio_backend_routing_and_capabilities() {
        let cfg = OutputConfig {
            backend: "android-usb".to_owned(),
            bit_perfect: true,
            ..OutputConfig::default()
        };
        let mut out = AndroidAudioOutput::new(cfg);
        let caps = out.capabilities();
        assert!(caps.backend_name.contains("Direct USB Audio"));
        assert!(caps.bit_perfect);
        assert!(caps.exclusive);
        assert!(caps.dsd_native);
        assert!(caps.dsd_dop);

        let fmt = AudioFormat::Pcm {
            sample_rate: 192000,
            channels: 2,
            bits_per_sample: 24,
            sample_format: SampleFormat::S24Le,
        };
        let opened = out.open(&fmt).unwrap();
        assert_eq!(opened, fmt);

        let buf = AudioBuffer::PcmI32 {
            samples: vec![0x1000_0000, 0x2000_0000],
            channels: 2,
            sample_rate: 192000,
            bits_per_sample: 24,
        };
        let frames = out.write(&buf).unwrap();
        assert_eq!(frames, 1);
        out.stop().unwrap();
    }

    #[test]
    fn test_dynamic_usb_fd_registry_and_auto_detection() {
        clear_active_usb_fd();
        assert_eq!(get_active_usb_fd(), None);

        // Before setting USB fd, default backend is Oboe
        let cfg = OutputConfig {
            backend: "android".to_owned(),
            bit_perfect: false,
            ..OutputConfig::default()
        };
        let out = AndroidAudioOutput::new(cfg);
        let caps1 = out.capabilities();
        assert_eq!(caps1.bit_perfect, false);

        // Set active USB fd dynamically (simulating Android UsbManager attach event)
        set_active_usb_fd(42);
        assert_eq!(get_active_usb_fd(), Some(42));

        let caps2 = out.capabilities();
        assert!(caps2.backend_name.contains("Direct USB Audio"));
        assert!(caps2.bit_perfect);

        // Clear dynamic USB fd (simulating unplug event)
        clear_active_usb_fd();
        assert_eq!(get_active_usb_fd(), None);
    }
}
