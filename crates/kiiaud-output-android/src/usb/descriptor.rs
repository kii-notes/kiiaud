// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pure Rust USB Audio Class 2.0 (UAC2) descriptor parser.
//!
//! Extracts clock source ID, audio streaming interfaces, alternate settings,
//! endpoint addresses (isochronous OUT and asynchronous feedback IN), and optimal bit depth.

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

// AudioControl CS_INTERFACE Subtypes (UAC2)
pub const UAC2_AC_HEADER: u8 = 0x01;
pub const UAC2_INPUT_TERMINAL: u8 = 0x02;
pub const UAC2_OUTPUT_TERMINAL: u8 = 0x03;
pub const UAC2_CLOCK_SOURCE: u8 = 0x0A;
pub const UAC2_CLOCK_SELECTOR: u8 = 0x0B;

// AudioStreaming CS_INTERFACE Subtypes
pub const UAC2_AS_GENERAL: u8 = 0x01;
pub const UAC2_FORMAT_TYPE: u8 = 0x02;

/// Format Type I format tags
pub const UAC2_FORMAT_TYPE_I: u8 = 0x01;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AltSettingInfo {
    pub alt_setting: u8,
    pub format_type: u8,
    pub subslot_size: u8,      // 2, 3, or 4 bytes
    pub bit_resolution: u8,    // 16, 24, 32 bits
    pub channels: u8,
    pub endpoint_out: u8,      // e.g. 0x01, 0x02
    pub max_packet_size: u16,  // Isochronous OUT max packet size
    pub endpoint_feedback: Option<u8>, // e.g. 0x81, 0x82 (asynchronous feedback IN)
    pub is_dsd_native: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUsbAudioDevice {
    pub clock_source_id: u8,
    pub control_interface: u8,
    pub streaming_interface: u8,
    pub alt_settings: Vec<AltSettingInfo>,
    pub best_pcm_alt: Option<AltSettingInfo>,
    pub dsd_native_alt: Option<AltSettingInfo>,
}

impl ParsedUsbAudioDevice {
    /// Parse raw USB descriptor hierarchy (as returned by `UsbDeviceConnection.getRawDescriptors()`
    /// or `/sys/bus/usb/devices/.../descriptors`).
    pub fn parse(raw: &[u8]) -> Result<Self> {
        let mut i = 0;
        let mut in_audio_control = false;
        let mut in_audio_streaming = false;

        let mut current_alt = 0u8;

        let mut control_iface = 0u8;
        let mut streaming_iface = 1u8;
        let mut clock_source_id: Option<u8> = None;

        let mut alt_settings: Vec<AltSettingInfo> = Vec::new();

        // Temporary accumulator for the current alt setting being parsed
        let mut cur_format_type = 0u8;
        let mut cur_subslot_size = 0u8;
        let mut cur_bit_resolution = 0u8;
        let mut cur_ep_out = 0u8;
        let mut cur_max_packet = 0u16;
        let mut cur_ep_feedback: Option<u8> = None;
        let mut cur_is_dsd = false;

        while i + 1 < raw.len() {
            let length = raw[i] as usize;
            if length < 2 || i + length > raw.len() {
                break;
            }
            let desc_type = raw[i + 1];

            match desc_type {
                USB_DESC_TYPE_INTERFACE => {
                    if length >= 9 {
                        // If we were parsing a streaming alt setting with endpoints, record it
                        if in_audio_streaming && current_alt > 0 && cur_ep_out != 0 {
                            alt_settings.push(AltSettingInfo {
                                alt_setting: current_alt,
                                format_type: cur_format_type,
                                subslot_size: cur_subslot_size,
                                bit_resolution: cur_bit_resolution,
                                channels: 2, // Default stereo unless specified
                                endpoint_out: cur_ep_out,
                                max_packet_size: cur_max_packet,
                                endpoint_feedback: cur_ep_feedback,
                                is_dsd_native: cur_is_dsd,
                            });
                        }

                        let current_iface = raw[i + 2];
                        current_alt = raw[i + 3];
                        let iface_class = raw[i + 5];
                        let iface_subclass = raw[i + 6];

                        in_audio_control = iface_class == USB_CLASS_AUDIO
                            && iface_subclass == USB_SUBCLASS_AUDIOCONTROL;
                        if in_audio_control {
                            control_iface = current_iface;
                        }

                        in_audio_streaming = iface_class == USB_CLASS_AUDIO
                            && iface_subclass == USB_SUBCLASS_AUDIOSTREAMING;
                        if in_audio_streaming {
                            streaming_iface = current_iface;
                            // Reset per-alt tracking
                            cur_format_type = 0;
                            cur_subslot_size = 0;
                            cur_bit_resolution = 0;
                            cur_ep_out = 0;
                            cur_max_packet = 0;
                            cur_ep_feedback = None;
                            cur_is_dsd = false;
                        }
                    }
                }
                USB_DESC_TYPE_CS_INTERFACE => {
                    if in_audio_control && length >= 5 {
                        let subtype = raw[i + 2];
                        // Bug 1 Fix: Parse CLOCK_SOURCE entity descriptor to get the true clock ID
                        if subtype == UAC2_CLOCK_SOURCE && clock_source_id.is_none() {
                            let clk_id = raw[i + 3];
                            tracing::info!(clock_id = clk_id, "found UAC2 CLOCK_SOURCE entity in descriptor");
                            clock_source_id = Some(clk_id);
                        }
                    } else if in_audio_streaming && length >= 6 {
                        let subtype = raw[i + 2];
                        if subtype == UAC2_FORMAT_TYPE {
                            let fmt_type = raw[i + 3];
                            cur_format_type = fmt_type;
                            cur_subslot_size = raw[i + 4];
                            cur_bit_resolution = raw[i + 5];

                            // Cayin RU7 and similar DACs use Alt 4 with subslot=4, bits=32 or raw stream for Native DSD
                            if current_alt >= 4 || fmt_type != UAC2_FORMAT_TYPE_I {
                                cur_is_dsd = true;
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
                channels: 2,
                endpoint_out: cur_ep_out,
                max_packet_size: cur_max_packet,
                endpoint_feedback: cur_ep_feedback,
                is_dsd_native: cur_is_dsd,
            });
        }

        // Clock source ID: fallback to 0x05 if not detected (standard for UAC2 XMOS/Amanero chips)
        let resolved_clock_id = clock_source_id.unwrap_or(0x05);

        // Best PCM alt setting: choose highest bit_resolution and largest subslot_size
        let best_pcm = alt_settings
            .iter()
            .filter(|a| !a.is_dsd_native)
            .max_by_key(|a| (a.bit_resolution, a.subslot_size))
            .cloned();

        let dsd_alt = alt_settings.iter().find(|a| a.is_dsd_native).cloned();

        Ok(ParsedUsbAudioDevice {
            clock_source_id: resolved_clock_id,
            control_interface: control_iface,
            streaming_interface: streaming_iface,
            alt_settings,
            best_pcm_alt: best_pcm,
            dsd_native_alt: dsd_alt,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sample_uac2_descriptors() {
        // Construct a synthetic UAC2 raw descriptor stream:
        // 1. Interface AudioControl (class=1, subclass=1)
        // 2. CS_INTERFACE CLOCK_SOURCE (subtype=0x0A, bClockID=0x05)
        // 3. Interface AudioStreaming Alt 0 (zero bandwidth)
        // 4. Interface AudioStreaming Alt 1 (16-bit)
        // 5. CS_INTERFACE FORMAT_TYPE (subslot=2, bits=16)
        // 6. Endpoint OUT (addr=0x01, max_pkt=1024, feedback=0x81)
        // 7. Interface AudioStreaming Alt 2 (32-bit / 24-in-32)
        // 8. CS_INTERFACE FORMAT_TYPE (subslot=4, bits=32)
        // 9. Endpoint OUT (addr=0x01, max_pkt=1024, feedback=0x81)
        let mut raw = Vec::new();

        // 1. Interface AudioControl
        raw.extend_from_slice(&[9, USB_DESC_TYPE_INTERFACE, 0, 0, 0, USB_CLASS_AUDIO, USB_SUBCLASS_AUDIOCONTROL, 0x20, 0]);
        // 2. CS_INTERFACE CLOCK_SOURCE
        raw.extend_from_slice(&[8, USB_DESC_TYPE_CS_INTERFACE, UAC2_CLOCK_SOURCE, 0x05, 0x03, 0x07, 0x00, 0x00]);

        // 3. Interface AudioStreaming Alt 0
        raw.extend_from_slice(&[9, USB_DESC_TYPE_INTERFACE, 1, 0, 0, USB_CLASS_AUDIO, USB_SUBCLASS_AUDIOSTREAMING, 0x20, 0]);

        // 4. Interface AudioStreaming Alt 1
        raw.extend_from_slice(&[9, USB_DESC_TYPE_INTERFACE, 1, 1, 2, USB_CLASS_AUDIO, USB_SUBCLASS_AUDIOSTREAMING, 0x20, 0]);
        // 5. CS FORMAT_TYPE Alt 1 (16-bit)
        raw.extend_from_slice(&[6, USB_DESC_TYPE_CS_INTERFACE, UAC2_FORMAT_TYPE, UAC2_FORMAT_TYPE_I, 2, 16]);
        // 6. Endpoint OUT
        raw.extend_from_slice(&[9, USB_DESC_TYPE_ENDPOINT, 0x01, 0x01, 0x00, 0x04, 1, 0, 0x81]);

        // 7. Interface AudioStreaming Alt 2
        raw.extend_from_slice(&[9, USB_DESC_TYPE_INTERFACE, 1, 2, 2, USB_CLASS_AUDIO, USB_SUBCLASS_AUDIOSTREAMING, 0x20, 0]);
        // 8. CS FORMAT_TYPE Alt 2 (32-bit)
        raw.extend_from_slice(&[6, USB_DESC_TYPE_CS_INTERFACE, UAC2_FORMAT_TYPE, UAC2_FORMAT_TYPE_I, 4, 32]);
        // 9. Endpoint OUT
        raw.extend_from_slice(&[9, USB_DESC_TYPE_ENDPOINT, 0x01, 0x01, 0x00, 0x04, 1, 0, 0x81]);

        let parsed = ParsedUsbAudioDevice::parse(&raw).unwrap();
        assert_eq!(parsed.clock_source_id, 0x05);
        assert_eq!(parsed.control_interface, 0);
        assert_eq!(parsed.streaming_interface, 1);
        assert_eq!(parsed.alt_settings.len(), 2);

        let best = parsed.best_pcm_alt.unwrap();
        assert_eq!(best.alt_setting, 2);
        assert_eq!(best.bit_resolution, 32);
        assert_eq!(best.subslot_size, 4);
        assert_eq!(best.endpoint_out, 0x01);
        assert_eq!(best.endpoint_feedback, Some(0x81));
    }
}
