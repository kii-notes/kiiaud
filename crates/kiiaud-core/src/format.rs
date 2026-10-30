// SPDX-License-Identifier: MIT OR Apache-2.0

use serde::{Deserialize, Serialize};
use std::fmt;

/// Base DSD64 rate in Hz (44.1 kHz * 64).
pub const DSD64_RATE_HZ: u32 = 2_822_400;

/// PCM sample bit format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SampleFormat {
    /// Signed 16-bit integer, little-endian.
    S16Le,
    /// Signed 24-bit integer packed in 3 bytes, little-endian (`S24_3LE`).
    S24_3Le,
    /// Signed 24-bit integer in 32-bit container, little-endian (`S24_LE` / left-aligned `S32_LE`).
    S24Le,
    /// Signed 32-bit integer, little-endian (`S32_LE`).
    S32Le,
    /// 32-bit IEEE float, little-endian (`FLOAT_LE`).
    F32Le,
}

impl SampleFormat {
    pub fn bits_per_sample(&self) -> u8 {
        match self {
            Self::S16Le => 16,
            Self::S24_3Le | Self::S24Le => 24,
            Self::S32Le | Self::F32Le => 32,
        }
    }

    pub fn bytes_per_sample(&self) -> usize {
        match self {
            Self::S16Le => 2,
            Self::S24_3Le => 3,
            Self::S24Le | Self::S32Le | Self::F32Le => 4,
        }
    }
}

impl fmt::Display for SampleFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::S16Le => f.write_str("S16_LE"),
            Self::S24_3Le => f.write_str("S24_3LE"),
            Self::S24Le => f.write_str("S24_LE"),
            Self::S32Le => f.write_str("S32_LE"),
            Self::F32Le => f.write_str("F32_LE"),
        }
    }
}

/// Standard DSD rates (multiples of 44.1 kHz).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DsdRate {
    /// DSD64 (2.8224 MHz)
    Dsd64,
    /// DSD128 (5.6448 MHz)
    Dsd128,
    /// DSD256 (11.2896 MHz)
    Dsd256,
    /// DSD512 (22.5792 MHz)
    Dsd512,
    /// DSD1024 (45.1584 MHz)
    Dsd1024,
}

impl DsdRate {
    /// Parse a DSD bit rate in Hz into a [`DsdRate`].
    pub fn from_hz(hz: u32) -> Option<Self> {
        match hz {
            2_822_400 => Some(Self::Dsd64),
            5_644_800 => Some(Self::Dsd128),
            11_289_600 => Some(Self::Dsd256),
            22_579_200 => Some(Self::Dsd512),
            45_158_400 => Some(Self::Dsd1024),
            _ => None,
        }
    }

    /// Bit rate in Hz (bits per second per channel).
    pub fn hz(&self) -> u32 {
        match self {
            Self::Dsd64 => 2_822_400,
            Self::Dsd128 => 5_644_800,
            Self::Dsd256 => 11_289_600,
            Self::Dsd512 => 22_579_200,
            Self::Dsd1024 => 45_158_400,
        }
    }

    /// Byte rate in Hz (1 byte = 8 DSD bits per channel).
    pub fn byte_rate_hz(&self) -> u32 {
        self.hz() / 8
    }

    /// Equivalent 32-bit Native DSD container frame rate (`DSD_U32_BE`/`LE` uses 32 bits = 4 bytes per frame).
    pub fn u32_frame_rate_hz(&self) -> u32 {
        self.hz() / 32
    }

    /// Equivalent DoP 24-bit PCM frame rate (16 DSD bits per 24-bit PCM frame).
    pub fn dop_pcm_rate_hz(&self) -> u32 {
        self.hz() / 16
    }
}

impl fmt::Display for DsdRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dsd64 => f.write_str("DSD64"),
            Self::Dsd128 => f.write_str("DSD128"),
            Self::Dsd256 => f.write_str("DSD256"),
            Self::Dsd512 => f.write_str("DSD512"),
            Self::Dsd1024 => f.write_str("DSD1024"),
        }
    }
}

/// DSD hardware transport mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DsdTransport {
    /// Native DSD stream (`SND_PCM_FORMAT_DSD_U32_BE` / `LE`).
    #[default]
    Native,
    /// DSD over PCM (24-bit frames with 0x05/0xFA markers).
    DoP,
}

impl fmt::Display for DsdTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Native => f.write_str("Native"),
            Self::DoP => f.write_str("DoP"),
        }
    }
}

/// User-configured DSD output preference (`[output].dsd_mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DsdMode {
    /// Try Native DSD first, then fall back to DoP if hardware supports DoP rate. Never silently converts to PCM.
    #[default]
    Auto,
    /// Require Native DSD (`DSD_U32_BE`/`LE`).
    Native,
    /// Require DoP (DSD over PCM).
    Dop,
}

impl std::str::FromStr for DsdMode {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Ok(DsdMode::Auto),
            "native" => Ok(DsdMode::Native),
            "dop" => Ok(DsdMode::Dop),
            other => Err(format!(
                "Invalid DSD mode '{other}'. Expected 'auto', 'native', or 'dop'"
            )),
        }
    }
}

impl std::fmt::Display for DsdMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DsdMode::Auto => write!(f, "auto"),
            DsdMode::Native => write!(f, "native"),
            DsdMode::Dop => write!(f, "dop"),
        }
    }
}

/// Bit ordering within each DSD byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum BitOrder {
    /// Least significant bit played first (common in DSF).
    LsbFirst,
    /// Most significant bit played first (common in DFF, WavPack DSD, DoP, and `DSD_U32_BE`).
    #[default]
    MsbFirst,
}

/// Channel layout of raw DSD bytes from the decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ChannelLayout {
    /// Planar: all bytes for channel 0 in the packet, followed by channel 1, etc.
    Planar,
    /// Byte-interleaved: `[ch0_byte0, ch1_byte0, ch0_byte1, ch1_byte1, ...]`.
    #[default]
    Interleaved,
}

/// First-class representation of PCM and DSD audio formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioFormat {
    Pcm {
        sample_rate: u32,
        channels: u16,
        sample_format: SampleFormat,
        bits_per_sample: u8,
    },
    Dsd {
        rate: DsdRate,
        channels: u16,
        transport: DsdTransport,
        bit_order: BitOrder,
        layout: ChannelLayout,
    },
}

impl AudioFormat {
    pub fn channels(&self) -> u16 {
        match self {
            Self::Pcm { channels, .. } | Self::Dsd { channels, .. } => *channels,
        }
    }

    pub fn is_dsd(&self) -> bool {
        matches!(self, Self::Dsd { .. })
    }

    /// Format string suitable for MPD `audio:` status line (e.g. `44100:16:2` or `dsd64:2`).
    pub fn mpd_audio_string(&self) -> String {
        match self {
            Self::Pcm {
                sample_rate,
                channels,
                bits_per_sample,
                ..
            } => format!("{sample_rate}:{bits_per_sample}:{channels}"),
            Self::Dsd { rate, channels, .. } => {
                let dsd_name = match rate {
                    DsdRate::Dsd64 => "dsd64",
                    DsdRate::Dsd128 => "dsd128",
                    DsdRate::Dsd256 => "dsd256",
                    DsdRate::Dsd512 => "dsd512",
                    DsdRate::Dsd1024 => "dsd1024",
                };
                format!("{dsd_name}:{channels}")
            }
        }
    }
}

impl fmt::Display for AudioFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pcm {
                sample_rate,
                channels,
                sample_format,
                bits_per_sample,
            } => write!(
                f,
                "PCM {sample_rate}Hz {bits_per_sample}-bit ({sample_format}) {channels}ch"
            ),
            Self::Dsd {
                rate,
                channels,
                transport,
                ..
            } => write!(f, "{rate} ({}Hz) {channels}ch [{transport}]", rate.hz()),
        }
    }
}

/// Decoded audio chunk produced by a decoder and consumed by the output pipeline.
#[derive(Debug, Clone)]
pub enum AudioBuffer {
    /// Interleaved 32-bit signed integer PCM samples.
    ///
    /// For 16-bit or 24-bit integer sources, values are stored in their exact integer representation
    /// shifted to 32-bit MSB-aligned (`S32_LE`) or native bit depth, preserving every bit without
    /// float round-trip loss.
    PcmI32 {
        /// Interleaved samples (`[L0, R0, L1, R1, ...]`), MSB-aligned in `i32` (so 16-bit is `<< 16`, 24-bit is `<< 8`).
        samples: Vec<i32>,
        channels: u16,
        sample_rate: u32,
        bits_per_sample: u8,
    },
    /// Interleaved 32-bit float PCM samples (`[-1.0, 1.0]`).
    PcmF32 {
        samples: Vec<f32>,
        channels: u16,
        sample_rate: u32,
    },
    /// Raw DSD bitstream bytes.
    Dsd {
        data: Vec<u8>,
        rate: DsdRate,
        channels: u16,
        bit_order: BitOrder,
        layout: ChannelLayout,
    },
}

impl AudioBuffer {
    pub fn is_empty(&self) -> bool {
        match self {
            Self::PcmI32 { samples, .. } => samples.is_empty(),
            Self::PcmF32 { samples, .. } => samples.is_empty(),
            Self::Dsd { data, .. } => data.is_empty(),
        }
    }

    /// Number of frames per channel in this buffer.
    pub fn frames(&self) -> usize {
        match self {
            Self::PcmI32 {
                samples, channels, ..
            } => {
                let ch = (*channels as usize).max(1);
                samples.len() / ch
            }
            Self::PcmF32 {
                samples, channels, ..
            } => {
                let ch = (*channels as usize).max(1);
                samples.len() / ch
            }
            Self::Dsd { data, channels, .. } => {
                let ch = (*channels as usize).max(1);
                data.len() / ch
            }
        }
    }
}

/// Detailed audio hardware and driver capabilities for an output backend.
///
/// In `kiiaud`, bit-perfect playback is treated as a capability of the hardware path
/// and driver, rather than an assumed invariant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputCapabilities {
    /// Human-readable backend name (e.g. "ALSA Hardware Direct (hw:)", "WASAPI Exclusive Mode", "Android Oboe (AAudio)", "Mock / Null").
    pub backend_name: String,
    /// List of supported sample rates in Hz (e.g. [44100, 48000, 96000, 192000]).
    pub sample_rates: Vec<u32>,
    /// List of supported bit depths in bits (e.g. [16, 24, 32]).
    pub bit_depths: Vec<u8>,
    /// Maximum channels supported.
    pub channels: u8,
    /// Whether exclusive mode (bypassing the OS software mixer) is supported.
    pub exclusive: bool,
    /// Whether the output pipeline delivers bit-perfect audio to the DAC.
    pub bit_perfect: bool,
    /// Whether Native DSD streaming is supported.
    pub dsd_native: bool,
    /// Whether DoP (DSD over PCM) streaming is supported.
    pub dsd_dop: bool,
}

impl Default for OutputCapabilities {
    fn default() -> Self {
        Self {
            backend_name: "Generic Output".to_owned(),
            sample_rates: vec![44100, 48000],
            bit_depths: vec![16],
            channels: 2,
            exclusive: false,
            bit_perfect: false,
            dsd_native: false,
            dsd_dop: false,
        }
    }
}

impl OutputCapabilities {
    pub fn supports_sample_rate(&self, rate: u32) -> bool {
        self.sample_rates.contains(&rate)
    }

    pub fn supports_bit_depth(&self, bits: u8) -> bool {
        self.bit_depths.contains(&bits)
    }
}
