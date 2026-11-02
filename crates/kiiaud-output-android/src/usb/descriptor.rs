// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pure Rust USB Audio Class 1.0 (UAC1) and 2.0 (UAC2) descriptor parser.
//!
//! Automatically determines whether the DAC operates on UAC1 (USB 1.1 Full Speed)
//! or UAC2 (USB 2.0 High Speed), extracting sample rates, clock source ID,
//! alternate settings, endpoint addresses, and bit depth.

use kiiaud_core::Result;

// USB Standard Descriptor Types
pub const USB_DESC_TYPE_DEVICE: u8 = 0x01;
pub const USB_DESC_TYPE_CONFIGURATION: u8 = 0x02;
pub const USB_DESC_TYPE_INTERFACE: u8 = 0x04;
pub const USB_DESC_TYPE_ENDPOINT: u8 = 0x05;
pub const USB_DESC_TYPE_CS_INTERFACE: u8 = 0x24;
pub const USB_DESC_TYPE_CS_ENDPOINT: u8 = 0x25;

// USB Audio Class & Subclass Codes
pub const USB_CLASS_AUDIO: u8 = 0x01;
pub const USB_SUBCLASS_AUDIOCONTROL: u8 = 0x01;
pub const USB_SUBCLASS_AUDIOSTREAMING: u8 = 0x02;

// Audio Class Protocols
pub const USB_PROTOCOL_UAC1: u8 = 0x00;
pub const USB_PROTOCOL_UAC2: u8 = 0x20;

// AudioControl CS_INTERFACE Subtypes
pub const UAC_AC_HEADER: u8 = 0x01;
pub const UAC_INPUT_TERMINAL: u8 = 0x02;
pub const UAC_OUTPUT_TERMINAL: u8 = 0x03;
pub const UAC2_CLOCK_SOURCE: u8 = 0x0A;
pub const UAC2_CLOCK_SELECTOR: u8 = 0x0B;

// AudioStreaming CS_INTERFACE Subtypes
pub const UAC_AS_GENERAL: u8 = 0x01;
pub const UAC_FORMAT_TYPE: u8 = 0x02;

/// Format Type I format tags
pub const UAC_FORMAT_TYPE_I: u8 = 0x01;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioClassVersion {
    Uac1,
    Uac2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AltSettingInfo {
    pub alt_setting: u8,
    pub format_type: u8,
    pub subslot_size: u8,   // 2, 3, or 4 bytes
    pub bit_resolution: u8, // 16, 24, 32 bits
    pub channels: u8,
    pub endpoint_out: u8,              // e.g. 0x01, 0x02
    pub max_packet_size: u16,          // Isochronous OUT max packet size
    pub endpoint_feedback: Option<u8>, // e.g. 0x81, 0x82
    pub is_dsd_native: bool,
    pub sample_rates: Vec<u32>, // Discovered sample rates (esp. for UAC1)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUsbAudioDevice {
    pub protocol: AudioClassVersion,
    pub clock_source_id: u8, // UAC2 only (defaults to 0x05)
    pub control_interface: u8,
    pub streaming_interface: u8,
    pub alt_settings: Vec<AltSettingInfo>,
    pub best_pcm_alt: Option<AltSettingInfo>,
    pub dsd_native_alt: Option<AltSettingInfo>,
    pub supported_sample_rates: Vec<u32>,
}

impl ParsedUsbAudioDevice {
    /// Parse raw USB descriptor hierarchy for both UAC1 and UAC2 audio devices.
    pub fn parse(raw: &[u8]) -> Result<Self> {
        let mut i = 0;
        let mut in_audio_control = false;
        let mut in_audio_streaming = false;

        let mut current_alt = 0u8;
        let mut control_iface = 0u8;
        let mut streaming_iface = 1u8;
        let mut protocol = AudioClassVersion::Uac2; // Default to UAC2
        let mut clock_source_id: Option<u8> = None;

        let mut alt_settings: Vec<AltSettingInfo> = Vec::new();

        // Temporary accumulator for the current alt setting being parsed
        let mut cur_format_type = 0u8;
        let mut cur_subslot_size = 0u8;
        let mut cur_bit_resolution = 0u8;
        let mut cur_channels = 2u8;
        let mut cur_ep_out = 0u8;
        let mut cur_max_packet = 0u16;
        let mut cur_ep_feedback: Option<u8> = None;
        let mut cur_is_dsd = false;
        let mut cur_sample_rates: Vec<u32> = Vec::new();

        while i + 1 < raw.len() {
            let length = raw[i] as usize;
            if length < 2 || i + length > raw.len() {
                break;
            }
            let desc_type = raw[i + 1];

            match desc_type {
                USB_DESC_TYPE_INTERFACE => {
                    if length >= 9 {
                        // Record previous streaming alt setting if valid
                        if in_audio_streaming && current_alt > 0 && cur_ep_out != 0 {
                            alt_settings.push(AltSettingInfo {
                                alt_setting: current_alt,
                                format_type: cur_format_type,
                                subslot_size: cur_subslot_size,
                                bit_resolution: cur_bit_resolution,
                                channels: cur_channels,
                                endpoint_out: cur_ep_out,
                                max_packet_size: cur_max_packet,
                                endpoint_feedback: cur_ep_feedback,
                                is_dsd_native: cur_is_dsd,
                                sample_rates: cur_sample_rates.clone(),
                            });
                        }

                        let current_iface = raw[i + 2];
                        current_alt = raw[i + 3];
                        let iface_class = raw[i + 5];
                        let iface_subclass = raw[i + 6];
                        let iface_protocol = raw[i + 7];

                        in_audio_control = iface_class == USB_CLASS_AUDIO
                            && iface_subclass == USB_SUBCLASS_AUDIOCONTROL;
                        if in_audio_control {
                            control_iface = current_iface;
                            // Check protocol: 0x00 = UAC1, 0x20 = UAC2
                            if iface_protocol == USB_PROTOCOL_UAC1 {
                                protocol = AudioClassVersion::Uac1;
                                tracing::info!("detected USB Audio Class 1.0 (UAC1) device");
                            } else if iface_protocol == USB_PROTOCOL_UAC2 {
                                protocol = AudioClassVersion::Uac2;
                                tracing::info!("detected USB Audio Class 2.0 (UAC2) device");
                            }
                        }

                        in_audio_streaming = iface_class == USB_CLASS_AUDIO
                            && iface_subclass == USB_SUBCLASS_AUDIOSTREAMING;
                        if in_audio_streaming {
                            streaming_iface = current_iface;
                            // Reset per-alt tracking
                            cur_format_type = 0;
                            cur_subslot_size = 0;
                            cur_bit_resolution = 0;
                            cur_channels = 2;
                            cur_ep_out = 0;
                            cur_max_packet = 0;
                            cur_ep_feedback = None;
                            cur_is_dsd = false;
                            cur_sample_rates.clear();
                        }
                    }
                }
                USB_DESC_TYPE_CS_INTERFACE => {
                    if in_audio_control && length >= 5 {
                        let subtype = raw[i + 2];
                        // UAC2: CLOCK_SOURCE entity descriptor
                        if subtype == UAC2_CLOCK_SOURCE && clock_source_id.is_none() {
                            let clk_id = raw[i + 3];
                            tracing::info!(
                                clock_id = clk_id,
                                "found UAC2 CLOCK_SOURCE entity in descriptor"
                            );
                            clock_source_id = Some(clk_id);
                        }
                    } else if in_audio_streaming && length >= 6 {
                        let subtype = raw[i + 2];
                        if subtype == UAC_FORMAT_TYPE {
                            let fmt_type = raw[i + 3];
                            cur_format_type = fmt_type;

                            if protocol == AudioClassVersion::Uac2 {
                                // UAC2 Format Type Descriptor
                                cur_subslot_size = raw[i + 4];
                                cur_bit_resolution = raw[i + 5];

                                if current_alt >= 4 || fmt_type != UAC_FORMAT_TYPE_I {
                                    cur_is_dsd = true;
                                }
                            } else {
                                // UAC1 Format Type Descriptor:
                                // offset 4: bNrChannels
                                // offset 5: bSubFrameSize
                                // offset 6: bBitResolution
                                // offset 7: bSamFreqType (0 = continuous, n = discrete count)
                                if length >= 7 {
                                    cur_channels = raw[i + 4];
                                    cur_subslot_size = raw[i + 5];
                                    cur_bit_resolution = raw[i + 6];
                                }
                                if length >= 8 {
                                    let sam_freq_type = raw[i + 7] as usize;
                                    if sam_freq_type > 0 && i + 8 + sam_freq_type * 3 <= raw.len() {
                                        for s in 0..sam_freq_type {
                                            let off = i + 8 + s * 3;
                                            let rate = (raw[off] as u32)
                                                | ((raw[off + 1] as u32) << 8)
                                                | ((raw[off + 2] as u32) << 16);
                                            if rate > 0 && !cur_sample_rates.contains(&rate) {
                                                cur_sample_rates.push(rate);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                USB_DESC_TYPE_ENDPOINT => {
                    if in_audio_streaming && length >= 7 {
                        let ep_addr = raw[i + 2];
                        let ep_attr = raw[i + 3];
                        let max_pkt = (raw[i + 4] as u16) | ((raw[i + 5] as u16) << 8);
                        let synch_addr = if length >= 9 { raw[i + 8] } else { 0 };

                        let is_isoc = (ep_attr & 0x03) == 0x01;
                        let is_out = (ep_addr & 0x80) == 0;
                        let is_in = (ep_addr & 0x80) != 0;

                        if is_isoc && is_out {
                            cur_ep_out = ep_addr;
                            cur_max_packet = max_pkt;
                            if synch_addr != 0 {
                                cur_ep_feedback = Some(synch_addr);
                            }
                        } else if (is_isoc || (ep_attr & 0x0C) == 0x04) && is_in {
                            // Dedicated feedback IN endpoint
                            cur_ep_feedback = Some(ep_addr);
                        }
                    }
                }
                _ => {}
            }

            i += length;
        }

        // Flush last alt setting if needed
        if in_audio_streaming && current_alt > 0 && cur_ep_out != 0 {
            alt_settings.push(AltSettingInfo {
                alt_setting: current_alt,
                format_type: cur_format_type,
                subslot_size: cur_subslot_size,
                bit_resolution: cur_bit_resolution,
                channels: cur_channels,
                endpoint_out: cur_ep_out,
                max_packet_size: cur_max_packet,
                endpoint_feedback: cur_ep_feedback,
                is_dsd_native: cur_is_dsd,
                sample_rates: cur_sample_rates.clone(),
            });
        }

        let resolved_clock_id = clock_source_id.unwrap_or(0x05);

        // Best PCM alt setting
        let best_pcm = alt_settings
            .iter()
            .filter(|a| !a.is_dsd_native)
            .max_by_key(|a| (a.bit_resolution, a.subslot_size))
            .cloned();

        let dsd_alt = alt_settings.iter().find(|a| a.is_dsd_native).cloned();

        // Collect all supported sample rates
        let mut supported_rates = Vec::new();
        for alt in &alt_settings {
            for &r in &alt.sample_rates {
                if !supported_rates.contains(&r) {
                    supported_rates.push(r);
                }
            }
        }
        if supported_rates.is_empty() {
            // Default UAC2 rates
            supported_rates = vec![44100, 48000, 88200, 96000, 176400, 192000, 352800, 384000];
        } else {
            supported_rates.sort_unstable();
        }

        Ok(ParsedUsbAudioDevice {
            protocol,
            clock_source_id: resolved_clock_id,
            control_interface: control_iface,
            streaming_interface: streaming_iface,
            alt_settings,
            best_pcm_alt: best_pcm,
            dsd_native_alt: dsd_alt,
            supported_sample_rates: supported_rates,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sample_uac2_descriptors() {
        let mut raw = Vec::new();
        // 1. Interface AudioControl (protocol=0x20 UAC2)
        raw.extend_from_slice(&[
            9,
            USB_DESC_TYPE_INTERFACE,
            0,
            0,
            0,
            USB_CLASS_AUDIO,
            USB_SUBCLASS_AUDIOCONTROL,
            USB_PROTOCOL_UAC2,
            0,
        ]);
        // 2. CS_INTERFACE CLOCK_SOURCE (bClockID=0x05)
        raw.extend_from_slice(&[
            8,
            USB_DESC_TYPE_CS_INTERFACE,
            UAC2_CLOCK_SOURCE,
            0x05,
            0x03,
            0x07,
            0x00,
            0x00,
        ]);
        // 3. Interface AudioStreaming Alt 0
        raw.extend_from_slice(&[
            9,
            USB_DESC_TYPE_INTERFACE,
            1,
            0,
            0,
            USB_CLASS_AUDIO,
            USB_SUBCLASS_AUDIOSTREAMING,
            USB_PROTOCOL_UAC2,
            0,
        ]);
        // 4. Interface AudioStreaming Alt 1 (32-bit)
        raw.extend_from_slice(&[
            9,
            USB_DESC_TYPE_INTERFACE,
            1,
            1,
            2,
            USB_CLASS_AUDIO,
            USB_SUBCLASS_AUDIOSTREAMING,
            USB_PROTOCOL_UAC2,
            0,
        ]);
        raw.extend_from_slice(&[
            6,
            USB_DESC_TYPE_CS_INTERFACE,
            UAC_FORMAT_TYPE,
            UAC_FORMAT_TYPE_I,
            4,
            32,
        ]);
        raw.extend_from_slice(&[
            9,
            USB_DESC_TYPE_ENDPOINT,
            0x01,
            0x01,
            0x00,
            0x04,
            1,
            0,
            0x81,
        ]);

        let parsed = ParsedUsbAudioDevice::parse(&raw).unwrap();
        assert_eq!(parsed.protocol, AudioClassVersion::Uac2);
        assert_eq!(parsed.clock_source_id, 0x05);
        assert_eq!(parsed.alt_settings.len(), 1);
        assert_eq!(parsed.best_pcm_alt.unwrap().bit_resolution, 32);
    }

    #[test]
    fn test_parse_sample_uac1_descriptors() {
        let mut raw = Vec::new();
        // 1. Interface AudioControl (protocol=0x00 UAC1)
        raw.extend_from_slice(&[
            9,
            USB_DESC_TYPE_INTERFACE,
            0,
            0,
            0,
            USB_CLASS_AUDIO,
            USB_SUBCLASS_AUDIOCONTROL,
            USB_PROTOCOL_UAC1,
            0,
        ]);
        // 2. Interface AudioStreaming Alt 0
        raw.extend_from_slice(&[
            9,
            USB_DESC_TYPE_INTERFACE,
            1,
            0,
            0,
            USB_CLASS_AUDIO,
            USB_SUBCLASS_AUDIOSTREAMING,
            USB_PROTOCOL_UAC1,
            0,
        ]);
        // 3. Interface AudioStreaming Alt 1 (UAC1 Format Type I: 2 channels, subslot=2, bits=16, 2 discrete rates: 44.1k and 48k)
        raw.extend_from_slice(&[
            9,
            USB_DESC_TYPE_INTERFACE,
            1,
            1,
            1,
            USB_CLASS_AUDIO,
            USB_SUBCLASS_AUDIOSTREAMING,
            USB_PROTOCOL_UAC1,
            0,
        ]);
        // Format Type I: length=14, subtype=0x02, format=0x01, channels=2, subslot=2, bits=16, samFreqType=2
        // rate 1: 44100 (0x44, 0xAC, 0x00), rate 2: 48000 (0x80, 0xBB, 0x00)
        raw.extend_from_slice(&[
            14,
            USB_DESC_TYPE_CS_INTERFACE,
            UAC_FORMAT_TYPE,
            UAC_FORMAT_TYPE_I,
            2,
            2,
            16,
            2,
            0x44,
            0xAC,
            0x00,
            0x80,
            0xBB,
            0x00,
        ]);
        // Endpoint 0x01 OUT (Isochronous, max_pkt=192)
        raw.extend_from_slice(&[9, USB_DESC_TYPE_ENDPOINT, 0x01, 0x01, 192, 0, 1, 0, 0]);

        let parsed = ParsedUsbAudioDevice::parse(&raw).unwrap();
        assert_eq!(parsed.protocol, AudioClassVersion::Uac1);
        assert_eq!(parsed.alt_settings.len(), 1);
        let best = parsed.best_pcm_alt.unwrap();
        assert_eq!(best.bit_resolution, 16);
        assert_eq!(best.subslot_size, 2);
        assert_eq!(best.sample_rates, vec![44100, 48000]);
        assert_eq!(parsed.supported_sample_rates, vec![44100, 48000]);
    }
}
