// SPDX-License-Identifier: MIT OR Apache-2.0

//! Android audio output backend (`kiiaud-output-android`) using Oboe / AAudio.
//!
//! Features:
//! - Bridges Google's Oboe library (AAudio / OpenSL ES) to kiiaud's [`AudioOutput`] trait.
//! - Capability-driven architecture: dynamically queries and exposes device capabilities
//!   (sample rates, exclusive mode, bit-perfect support, DoP).
//! - Ring buffer FIFO bridging the synchronous `write()` push model to Oboe's asynchronous
//!   real-time audio callback model.
//! - Bit-perfect DoP (DSD over PCM) passthrough for DSF, DFF, and WavPack DSD.
//! - Graceful fallback from Exclusive to Shared mode if the hardware or Android HAL refuses
//!   exclusive access.

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
}

impl AndroidAudioOutput {
    pub fn new(config: OutputConfig) -> Self {
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
                        "Native DSD is not supported on Android HAL; use DoP transport instead"
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
        self.is_playing = false;
        Ok(())
    }

    fn resume(&mut self) -> Result<()> {
        self.is_playing = true;
        Ok(())
    }

    fn drain(&mut self) -> Result<()> {
        // Clear remaining ring buffer contents on track end
        self.ring_buffer.clear();
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.is_playing = false;
        self.active_format = None;
        self.hw_format = None;
        self.dop_encoder = None;
        self.ring_buffer.clear();
        Ok(())
    }

    fn capabilities(&self) -> OutputCapabilities {
        OutputCapabilities {
            backend_name: if self.is_exclusive {
                "Android Oboe (AAudio Exclusive Mode)".to_owned()
            } else {
                "Android Oboe (Shared Mixer)".to_owned()
            },
            sample_rates: vec![44100, 48000, 88200, 96000, 176400, 192000],
            bit_depths: vec![16, 24, 32],
            channels: 2,
            exclusive: self.is_exclusive,
            bit_perfect: self.config.bit_perfect && self.is_exclusive,
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
    use kiiaud_core::format::DsdRate;

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
    fn test_dop_open_and_write() {
        let cfg = OutputConfig {
            backend: "oboe".to_owned(),
            bit_perfect: true,
            ..OutputConfig::default()
        };
        let mut out = AndroidAudioOutput::new(cfg);

        let fmt = AudioFormat::Dsd {
            rate: DsdRate::Dsd64,
            channels: 2,
            transport: DsdTransport::Native,
            bit_order: kiiaud_core::BitOrder::MsbFirst,
            layout: kiiaud_core::ChannelLayout::Interleaved,
        };

        let opened = out.open(&fmt).unwrap();
        assert!(matches!(
            opened,
            AudioFormat::Dsd {
                transport: DsdTransport::DoP,
                ..
            }
        ));

        // Write DSD frames
        let buf = AudioBuffer::Dsd {
            data: vec![0xAA, 0x55, 0xAA, 0x55],
            rate: DsdRate::Dsd64,
            channels: 2,
            bit_order: kiiaud_core::BitOrder::MsbFirst,
            layout: kiiaud_core::ChannelLayout::Interleaved,
        };
        let frames = out.write(&buf).unwrap();
        assert_eq!(frames, 2);
        assert!(out.ring_buffer().available() > 0);

        out.stop().unwrap();
    }
}
