// SPDX-License-Identifier: MIT OR Apache-2.0

//! Android Direct USB Audio Class 2.0 Output Backend (`AndroidUsbAudioOutput`).
//!
//! Provides 100% Bit-Perfect audio streaming directly to external USB DACs via Linux `usbdevfs`,
//! completely bypassing Android AudioFlinger, AAudio, Oboe, ALSA user-space, and kernel `snd-usb-audio`.

use crate::usb::UsbAudioStream;
use kiiaud_core::config::OutputConfig;
use kiiaud_core::{
    AudioBuffer, AudioFormat, KiiaudError, OutputCapabilities, Result,
};
use kiiaud_output::AudioOutput;

pub struct AndroidUsbAudioOutput {
    config: OutputConfig,
    stream: Option<UsbAudioStream>,
    active_format: Option<AudioFormat>,
    is_playing: bool,
    mock_caps: OutputCapabilities,
}

impl AndroidUsbAudioOutput {
    pub fn new(config: OutputConfig) -> Self {
        let sample_rates = vec![
            44100, 48000, 88200, 96000, 176400, 192000, 352800, 384000, 705600, 768000,
        ];
        let bit_depths = vec![16, 24, 32];

        let mock_caps = OutputCapabilities {
            backend_name: "Android Direct USB Audio (Pure Rust UAC2 Bit-Perfect)".to_owned(),
            device_name: if config.device.is_empty() {
                "Android USB DAC".to_owned()
            } else {
                config.device.clone()
            },
            sample_rates,
            unsupported_rates: Vec::new(),
            bit_depths,
            channels: 2,
            exclusive: true,
            bit_perfect: true,
            adaptive_fallback: false,
            dsd_native: true,
            dsd_dop: true,
        };

        Self {
            config,
            stream: None,
            active_format: None,
            is_playing: false,
            mock_caps,
        }
    }

    /// Try to initialize the USB stream if not already open
    fn ensure_stream_open(&mut self) -> Result<&mut UsbAudioStream> {
        if self.stream.is_none() {
            let dev_spec = if self.config.device.is_empty() || self.config.device == "default" {
                "fd:0" // Default placeholder if no specific fd or path provided
            } else {
                &self.config.device
            };

            tracing::info!(
                device = dev_spec,
                "initializing Pure Rust Android Direct USB stream"
            );

            let stream = UsbAudioStream::open_device(dev_spec)?;
            self.stream = Some(stream);
        }

        Ok(self.stream.as_mut().unwrap())
    }
}

// Safe because playback worker thread has exclusive ownership during audio playback
unsafe impl Send for AndroidUsbAudioOutput {}

impl AudioOutput for AndroidUsbAudioOutput {
    fn open(&mut self, format: &AudioFormat) -> Result<AudioFormat> {
        self.stop()?;

        tracing::info!(
            target_format = %format,
            device = %self.config.device,
            bit_perfect = true,
            "opening Android Direct USB Audio (Pure Rust UAC2 Bit-Perfect)"
        );

        // Try to open hardware stream if a real USB device or fd is specified
        if !self.config.device.is_empty()
            && (self.config.device.starts_with("fd:") || self.config.device.starts_with('/'))
        {
            let dsd_mode = self.config.dsd_mode;
            let stream = self.ensure_stream_open()?;
            let negotiated = stream.configure(format, dsd_mode)?;
            self.active_format = Some(negotiated);
        } else {
            // Emulated/Test mode when running unit tests without physical USB DAC attached
            self.active_format = Some(*format);
        }

        self.is_playing = true;
        Ok(self.active_format.unwrap())
    }

    fn write(&mut self, buffer: &AudioBuffer) -> Result<usize> {
        if !self.is_playing {
            return Err(KiiaudError::Output("USB output is paused or stopped".to_owned()));
        }

        if let Some(stream) = self.stream.as_mut() {
            stream.write(buffer)
        } else {
            // In mock/test mode without hardware attached, return frame count directly
            Ok(buffer.frames())
        }
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
        if let Some(stream) = self.stream.as_mut() {
            stream.drain()?;
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.is_playing = false;
        if let Some(stream) = self.stream.as_mut() {
            let _ = stream.stop();
        }
        self.active_format = None;
        Ok(())
    }

    fn capabilities(&self) -> OutputCapabilities {
        if let Some(stream) = self.stream.as_ref() {
            let info = stream.device_info();
            let mut bit_depths = Vec::new();
            for alt in &info.alt_settings {
                if !bit_depths.contains(&(alt.bit_resolution as u8)) {
                    bit_depths.push(alt.bit_resolution as u8);
                }
            }
            if bit_depths.is_empty() {
                bit_depths = vec![16, 24, 32];
            }

            let protocol_str = match info.protocol {
                crate::usb::descriptor::AudioClassVersion::Uac1 => "UAC1",
                crate::usb::descriptor::AudioClassVersion::Uac2 => "UAC2",
            };

            let device_label = match info.protocol {
                crate::usb::descriptor::AudioClassVersion::Uac1 => {
                    format!("USB Audio DAC (UAC1 Endpoint 0x{:02X})", info.best_pcm_alt.as_ref().map(|a| a.endpoint_out).unwrap_or(1))
                }
                crate::usb::descriptor::AudioClassVersion::Uac2 => {
                    format!("USB Audio DAC (UAC2 Clock ID 0x{:02X}, Iface {})", info.clock_source_id, info.streaming_interface)
                }
            };

            OutputCapabilities {
                backend_name: format!("Android Direct USB Audio (Pure Rust {} Bit-Perfect)", protocol_str),
                device_name: device_label,
                sample_rates: info.supported_sample_rates.clone(),
                unsupported_rates: Vec::new(),
                bit_depths,
                channels: 2,
                exclusive: true,
                bit_perfect: true,
                adaptive_fallback: false,
                dsd_native: info.dsd_native_alt.is_some(),
                dsd_dop: info.protocol == crate::usb::descriptor::AudioClassVersion::Uac2,
            }
        } else {
            self.mock_caps.clone()
        }
    }
}
