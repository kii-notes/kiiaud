// SPDX-License-Identifier: MIT OR Apache-2.0
// Adapted from `rmpd-player/src/dop.rs` (Copyright (c) 2026 Gianluca Boiano, MIT OR Apache-2.0)

//! DoP (DSD over PCM) encoder and Native DSD `U32` packet packer.
//!
//! Packs raw DSD bitstreams into:
//! 1. **DoP (DSD over PCM)**: 24-bit PCM frames with alternating `0x05` / `0xFA` markers (`DSD64 -> 176.4kHz`, `DSD128 -> 352.8kHz`, `DSD256 -> 705.6kHz`, `DSD512 -> 1411.2kHz`).
//! 2. **Native DSD (`DSD_U32_BE` / `DSD_U32_LE`)**: 32-bit (4-byte) DSD words per channel per frame for ALSA native DSD transport.

use kiiaud_core::{BitOrder, ChannelLayout, DsdRate, KiiaudError, Result};

pub const DOP_MARKER_1: u8 = 0x05;
pub const DOP_MARKER_2: u8 = 0xFA;

/// Compile-time lookup table for bit reversal (LSB-first <-> MSB-first).
const BIT_REVERSE_TABLE: [u8; 256] = {
    let mut table = [0u8; 256];
    let mut i = 0u16;
    while i < 256 {
        let mut result = 0u8;
        let mut b = i as u8;
        let mut bit = 0;
        while bit < 8 {
            result = (result << 1) | (b & 1);
            b >>= 1;
            bit += 1;
        }
        table[i as usize] = result;
        i += 1;
    }
    table
};

#[inline]
pub const fn reverse_bits(byte: u8) -> u8 {
    BIT_REVERSE_TABLE[byte as usize]
}

/// DoP encoder that converts raw DSD bytes into MSB-aligned 32-bit (`S32_LE` / `S24_3LE`) DoP PCM samples.
///
/// Retains leftover DSD bytes across calls when packet sizes are not an exact multiple of 2 bytes per channel.
pub struct DopEncoder {
    rate: DsdRate,
    channels: usize,
    marker_toggle: bool,
    layout: ChannelLayout,
    bit_order: BitOrder,
}

impl DopEncoder {
    pub fn new(
        rate: DsdRate,
        channels: usize,
        layout: ChannelLayout,
        bit_order: BitOrder,
    ) -> Result<Self> {
        if channels == 0 {
            return Err(KiiaudError::Output(
                "Invalid channel count 0 for DoP".to_owned(),
            ));
        }
        Ok(Self {
            rate,
            channels,
            marker_toggle: false,
            layout,
            bit_order,
        })
    }

    /// Resulting PCM sample rate in Hz (`dsd_rate / 16`, e.g. `176400` for `DSD64`).
    pub fn pcm_sample_rate(&self) -> u32 {
        self.rate.dop_pcm_rate_hz()
    }

    /// Encode raw DSD bytes into left-aligned 32-bit DoP samples: `[marker][dsd_byte1][dsd_byte2][0x00]`.
    pub fn encode(&mut self, dsd_data: &[u8], output: &mut Vec<i32>) {
        let dsd_bytes_per_ch_per_frame = 2;

        match self.layout {
            ChannelLayout::Planar => {
                let bytes_per_channel = dsd_data.len() / self.channels;
                let num_frames = bytes_per_channel / dsd_bytes_per_ch_per_frame;
                output.clear();
                output.reserve(num_frames * self.channels);

                for frame_idx in 0..num_frames {
                    let marker = if self.marker_toggle {
                        DOP_MARKER_2
                    } else {
                        DOP_MARKER_1
                    };
                    self.marker_toggle = !self.marker_toggle;

                    for ch in 0..self.channels {
                        let ch_offset = ch * bytes_per_channel;
                        let offset = ch_offset + frame_idx * dsd_bytes_per_ch_per_frame;
                        if offset + 1 >= dsd_data.len() {
                            break;
                        }
                        let b1 = dsd_data[offset];
                        let b2 = dsd_data[offset + 1];
                        let (byte1, byte2) = if self.bit_order == BitOrder::LsbFirst {
                            (reverse_bits(b1), reverse_bits(b2))
                        } else {
                            (b1, b2)
                        };
                        let sample = ((marker as i32) << 24)
                            | ((byte1 as i32) << 16)
                            | ((byte2 as i32) << 8);
                        output.push(sample);
                    }
                }
            }
            ChannelLayout::Interleaved => {
                // Note: Symphonia's WavPack DSD decoder produces 1-byte-interleaved frames (`[L0, R0, L1, R1, ...]`),
                // whereas some DFF readers produce 1-byte-interleaved or 2-byte-interleaved.
                // Let's handle standard 1-byte-per-channel interleaved (`[L0, R0, L1, R1]`)!
                // Wait: in `rmpd-player/src/dop.rs`, let's check how `Interleaved` was indexed vs how `WavPackDecoder` interleaves!
                // In `WavPackDecoder`, each frame `idx` has 1 byte per channel: `[ch0_b0, ch1_b0, ch0_b1, ch1_b1, ...]`.
                // So for frame `frame_idx` (which needs 2 DSD bytes per channel = 2 DSD byte-frames):
                // byte1 of channel `ch` is at `(frame_idx * 2) * channels + ch`,
                // byte2 of channel `ch` is at `(frame_idx * 2 + 1) * channels + ch`!
                let bytes_per_dop_frame = self.channels * dsd_bytes_per_ch_per_frame;
                let num_frames = dsd_data.len() / bytes_per_dop_frame;
                output.clear();
                output.reserve(num_frames * self.channels);

                for frame_idx in 0..num_frames {
                    let marker = if self.marker_toggle {
                        DOP_MARKER_2
                    } else {
                        DOP_MARKER_1
                    };
                    self.marker_toggle = !self.marker_toggle;

                    let base = frame_idx * bytes_per_dop_frame;
                    for ch in 0..self.channels {
                        let idx1 = base + ch;
                        let idx2 = base + self.channels + ch;
                        if idx2 >= dsd_data.len() {
                            break;
                        }
                        let b1 = dsd_data[idx1];
                        let b2 = dsd_data[idx2];
                        let (byte1, byte2) = if self.bit_order == BitOrder::LsbFirst {
                            (reverse_bits(b1), reverse_bits(b2))
                        } else {
                            (b1, b2)
                        };
                        let sample = ((marker as i32) << 24)
                            | ((byte1 as i32) << 16)
                            | ((byte2 as i32) << 8);
                        output.push(sample);
                    }
                }
            }
        }
    }
}

/// Repack raw DSD bytes into ALSA Native DSD 32-bit container frames (`SND_PCM_FORMAT_DSD_U32_BE` or `LE`).
///
/// Each ALSA frame consists of 4 bytes (32 DSD bits) per channel, MSB-first.
pub fn pack_native_dsd_u32(
    dsd_data: &[u8],
    channels: usize,
    layout: ChannelLayout,
    bit_order: BitOrder,
    big_endian_words: bool,
    out_bytes: &mut Vec<u8>,
) {
    out_bytes.clear();
    if channels == 0 || dsd_data.is_empty() {
        return;
    }

    let map_byte = |b: u8| -> u8 {
        if bit_order == BitOrder::LsbFirst {
            reverse_bits(b)
        } else {
            b
        }
    };

    match layout {
        ChannelLayout::Planar => {
            let bytes_per_ch = dsd_data.len() / channels;
            let num_u32_frames = bytes_per_ch / 4;
            out_bytes.reserve(num_u32_frames * channels * 4);

            for f in 0..num_u32_frames {
                for ch in 0..channels {
                    let off = ch * bytes_per_ch + f * 4;
                    let b0 = map_byte(dsd_data[off]);
                    let b1 = map_byte(dsd_data[off + 1]);
                    let b2 = map_byte(dsd_data[off + 2]);
                    let b3 = map_byte(dsd_data[off + 3]);
                    if big_endian_words {
                        out_bytes.extend_from_slice(&[b0, b1, b2, b3]);
                    } else {
                        out_bytes.extend_from_slice(&[b3, b2, b1, b0]);
                    }
                }
            }
        }
        ChannelLayout::Interleaved => {
            // Byte-interleaved: `[ch0_b0, ch1_b0, ch0_b1, ch1_b1, ch0_b2, ch1_b2, ch0_b3, ch1_b3]`
            let stride = channels * 4;
            let num_u32_frames = dsd_data.len() / stride;
            out_bytes.reserve(num_u32_frames * stride);

            for f in 0..num_u32_frames {
                let base = f * stride;
                for ch in 0..channels {
                    let b0 = map_byte(dsd_data[base + ch]);
                    let b1 = map_byte(dsd_data[base + channels + ch]);
                    let b2 = map_byte(dsd_data[base + 2 * channels + ch]);
                    let b3 = map_byte(dsd_data[base + 3 * channels + ch]);
                    if big_endian_words {
                        out_bytes.extend_from_slice(&[b0, b1, b2, b3]);
                    } else {
                        out_bytes.extend_from_slice(&[b3, b2, b1, b0]);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dop_encoder_planar_and_markers() {
        let mut enc =
            DopEncoder::new(DsdRate::Dsd64, 2, ChannelLayout::Planar, BitOrder::MsbFirst).unwrap();
        assert_eq!(enc.pcm_sample_rate(), 176_400);

        // 2 frames planar stereo = 4 bytes left + 4 bytes right = 8 bytes
        let dsd = vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
        let mut out = Vec::new();
        enc.encode(&dsd, &mut out);

        assert_eq!(out.len(), 4);
        assert_eq!(out[0], (DOP_MARKER_1 as i32) << 24 | 0x11 << 16 | 0x22 << 8);
        assert_eq!(out[1], (DOP_MARKER_1 as i32) << 24 | 0x55 << 16 | 0x66 << 8);
        assert_eq!(out[2], (DOP_MARKER_2 as i32) << 24 | 0x33 << 16 | 0x44 << 8);
        assert_eq!(out[3], (DOP_MARKER_2 as i32) << 24 | 0x77 << 16 | 0x88 << 8);
    }

    #[test]
    fn test_dop_encoder_interleaved_and_lsb_reversal() {
        let mut enc = DopEncoder::new(
            DsdRate::Dsd128,
            2,
            ChannelLayout::Interleaved,
            BitOrder::LsbFirst,
        )
        .unwrap();
        assert_eq!(enc.pcm_sample_rate(), 352_800);

        // 1 DoP frame interleaved = [L0, R0, L1, R1]
        // 0xAA reversed is 0x55
        let dsd = vec![0xAA, 0x00, 0xFF, 0xAA];
        let mut out = Vec::new();
        enc.encode(&dsd, &mut out);

        assert_eq!(out.len(), 2);
        assert_eq!(out[0], (DOP_MARKER_1 as i32) << 24 | 0x55 << 16 | 0xFF << 8);
        assert_eq!(out[1], (DOP_MARKER_1 as i32) << 24 | 0x00 << 16 | 0x55 << 8);
    }

    #[test]
    fn test_pack_native_dsd_u32_be() {
        // 1 U32 frame stereo planar = 4 bytes L + 4 bytes R
        let dsd = vec![1, 2, 3, 4, 5, 6, 7, 8];
        let mut out = Vec::new();
        pack_native_dsd_u32(
            &dsd,
            2,
            ChannelLayout::Planar,
            BitOrder::MsbFirst,
            true,
            &mut out,
        );
        assert_eq!(out, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }
}
