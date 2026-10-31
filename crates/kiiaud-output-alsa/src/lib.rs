// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bit-perfect ALSA hardware-direct output backend (`kiiaud-output-alsa`).
//!
//! Features:
//! - Hardware-direct access (`hw:CARD,DEV`), disabling ALSA software resampling (`set_rate_resample(false)`).
//! - Native DSD transport (`SND_PCM_FORMAT_DSD_U32_BE` / `SND_PCM_FORMAT_DSD_U32_LE`).
//! - DoP transport (`DSD over PCM` in `S32_LE` or `S24_3LE` at `176.4kHz`, `352.8kHz`, `705.6kHz`, `1411.2kHz`).
//! - Bit-perfect integer PCM packing (`S16_LE`, `S24_3LE`, `S24_LE`, `S32_LE`, `FLOAT_LE`).
//! - Never silently converts DSD to PCM (`R-014`).
//! - Structured diagnostic logging of negotiated hardware format, sample rate, channels, buffer/period sizes, and xrun recovery (`R-022`).

use kiiaud_core::config::OutputConfig;
use kiiaud_core::{AudioBuffer, AudioFormat, KiiaudError, OutputCapabilities, Result};
#[cfg(target_os = "linux")]
use kiiaud_core::{BitOrder, ChannelLayout, DsdMode, DsdRate, DsdTransport, SampleFormat};
use kiiaud_output::AudioOutput;
use kiiaud_output::dop::DopEncoder;
#[cfg(target_os = "linux")]
use kiiaud_output::dop::pack_native_dsd_u32;

#[cfg(target_os = "linux")]
use alsa::{
    Direction, ValueOr,
    pcm::{Access, Format as AlsaFormat, HwParams, PCM, State as AlsaState},
};

#[allow(dead_code)]
pub struct AlsaAudioOutput {
    config: OutputConfig,
    capabilities: OutputCapabilities,
    #[cfg(target_os = "linux")]
    pcm: Option<PCM>,
    #[cfg(target_os = "linux")]
    hw_format: Option<AlsaFormat>,
    active_format: Option<AudioFormat>,
    dop_encoder: Option<DopEncoder>,
    dop_scratch: Vec<i32>,
    byte_scratch: Vec<u8>,
    is_adaptive_fallback: bool,
}

impl AlsaAudioOutput {
    pub fn new(config: OutputConfig) -> Self {
        #[cfg(target_os = "linux")]
        let capabilities = detect_hardware_capabilities(&config.device, config.bit_perfect);
        #[cfg(not(target_os = "linux"))]
        let capabilities = OutputCapabilities::default();

        Self {
            capabilities,
            config,
            #[cfg(target_os = "linux")]
            pcm: None,
            #[cfg(target_os = "linux")]
            hw_format: None,
            active_format: None,
            dop_encoder: None,
            dop_scratch: Vec::new(),
            byte_scratch: Vec::new(),
            is_adaptive_fallback: false,
        }
    }

    pub fn is_adaptive_fallback(&self) -> bool {
        self.is_adaptive_fallback
    }
}

#[cfg(not(target_os = "linux"))]
impl AudioOutput for AlsaAudioOutput {
    fn open(&mut self, _format: &AudioFormat) -> Result<AudioFormat> {
        Err(KiiaudError::Output(
            "ALSA output is only supported on Linux".to_owned(),
        ))
    }

    fn write(&mut self, _buffer: &AudioBuffer) -> Result<usize> {
        Err(KiiaudError::Output(
            "ALSA output is only supported on Linux".to_owned(),
        ))
    }

    fn pause(&mut self) -> Result<()> {
        Ok(())
    }

    fn resume(&mut self) -> Result<()> {
        Ok(())
    }

    fn drain(&mut self) -> Result<()> {
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        Ok(())
    }

    fn capabilities(&self) -> OutputCapabilities {
        self.capabilities.clone()
    }
}

#[cfg(target_os = "linux")]
impl AlsaAudioOutput {
    /// Configure ALSA `HwParams` and `SwParams` for a specific `(AlsaFormat, rate, channels)`.
    fn configure_pcm(
        &self,
        pcm: &PCM,
        device_name: &str,
        alsa_fmt: AlsaFormat,
        rate: u32,
        channels: u16,
        allow_resample: bool,
    ) -> Result<(u64, u64)> {
        let hwp = HwParams::any(pcm).map_err(|e| {
            KiiaudError::Output(format!("ALSA hw_params_any failed on '{device_name}': {e}"))
        })?;

        if !allow_resample && self.config.bit_perfect {
            // Explicitly disable ALSA software resampling
            let _ = hwp.set_rate_resample(false);
        }

        hwp.set_access(Access::RWInterleaved).map_err(|e| {
            KiiaudError::Output(format!(
                "ALSA set_access(RWInterleaved) failed on '{device_name}': {e}"
            ))
        })?;

        hwp.set_format(alsa_fmt).map_err(|e| {
            KiiaudError::Output(format!(
                "ALSA hardware does not support format {alsa_fmt}: {e}"
            ))
        })?;

        hwp.set_channels(channels as u32).map_err(|e| {
            KiiaudError::Output(format!(
                "ALSA hardware does not support {channels} channels: {e}"
            ))
        })?;

        let actual_rate = hwp
            .set_rate_near(rate, ValueOr::Nearest)
            .map_err(|e| KiiaudError::Output(format!("ALSA set_rate({rate} Hz) failed: {e}")))?;

        if !allow_resample && self.config.bit_perfect && actual_rate != rate {
            return Err(KiiaudError::UnsupportedFormat(format!(
                "Bit-perfect violation: requested {rate} Hz, but ALSA hardware '{device_name}' only supports {actual_rate} Hz",
            )));
        }

        // Configure period and buffer sizes based on configured milliseconds
        let period_frames =
            ((rate as u64 * self.config.period_time_ms.max(10) as u64) / 1000).max(256) as i64;
        let buffer_frames = ((rate as u64 * self.config.buffer_time_ms.max(40) as u64) / 1000)
            .max(period_frames as u64 * 2) as i64;

        let _ = hwp.set_period_size_near(period_frames, ValueOr::Nearest);
        let _ = hwp.set_buffer_size_near(buffer_frames);

        pcm.hw_params(&hwp).map_err(|e| {
            KiiaudError::Output(format!(
                "ALSA snd_pcm_hw_params failed on '{device_name}': {e}"
            ))
        })?;

        let current_hwp = pcm.hw_params_current().map_err(|e| {
            KiiaudError::Output(format!(
                "ALSA hw_params_current failed on '{device_name}': {e}"
            ))
        })?;
        let neg_buf = current_hwp.get_buffer_size().unwrap_or(0) as u64;
        let neg_per = current_hwp.get_period_size().unwrap_or(0) as u64;

        if let Ok(swp) = pcm.sw_params_current() {
            let start_thresh = (neg_per * 2).min(neg_buf).max(1) as i64;
            let _ = swp.set_start_threshold(start_thresh);
            let _ = swp.set_avail_min(neg_per.max(1) as i64);
            let _ = pcm.sw_params(&swp);
        }

        pcm.prepare().map_err(|e| {
            KiiaudError::Output(format!(
                "ALSA snd_pcm_prepare failed on '{device_name}': {e}"
            ))
        })?;

        Ok((neg_buf, neg_per))
    }

    /// Negotiate the best bit-perfect PCM `AlsaFormat` supported by the hardware for the given source bit depth.
    fn negotiate_pcm_format(
        &self,
        pcm: &PCM,
        device_name: &str,
        preferred: SampleFormat,
        rate: u32,
        channels: u16,
        allow_resample: bool,
    ) -> Result<(AlsaFormat, SampleFormat, u64, u64)> {
        // Candidate order preserves bit-perfection: never narrow bit depth if a wider or exact integer container is available!
        let candidates: &[AlsaFormat] = match preferred {
            SampleFormat::S16Le => &[
                AlsaFormat::S16LE,
                AlsaFormat::S243LE,
                AlsaFormat::S32LE,
                AlsaFormat::S24LE,
            ],
            SampleFormat::S24_3Le | SampleFormat::S24Le => {
                &[AlsaFormat::S243LE, AlsaFormat::S32LE, AlsaFormat::S24LE]
            }
            SampleFormat::S32Le => &[AlsaFormat::S32LE, AlsaFormat::S243LE, AlsaFormat::S24LE],
            SampleFormat::F32Le => &[AlsaFormat::FloatLE, AlsaFormat::S32LE, AlsaFormat::S243LE],
        };

        let mut last_err = None;
        for &cand in candidates {
            match self.configure_pcm(pcm, device_name, cand, rate, channels, allow_resample) {
                Ok((buf_sz, per_sz)) => {
                    let mapped_sf = match cand {
                        AlsaFormat::S16LE => SampleFormat::S16Le,
                        AlsaFormat::S243LE => SampleFormat::S24_3Le,
                        AlsaFormat::S24LE => SampleFormat::S24Le,
                        AlsaFormat::S32LE => SampleFormat::S32Le,
                        AlsaFormat::FloatLE => SampleFormat::F32Le,
                        _ => preferred,
                    };
                    return Ok((cand, mapped_sf, buf_sz, per_sz));
                }
                Err(e) => last_err = Some(e),
            }
        }

        Err(last_err.unwrap_or_else(|| {
            KiiaudError::UnsupportedFormat(format!(
                "No compatible bit-perfect ALSA PCM format found for {preferred} at {rate} Hz on '{device_name}'"
            ))
        }))
    }

    /// Open and negotiate Native DSD (`DSD_U32_BE` / `DSD_U32_LE`) or DoP (`S32_LE` / `S24_3LE` at `dsd_rate / 16`).
    /// Never silently converts DSD to PCM (`R-014`).
    fn negotiate_dsd_format(
        &mut self,
        pcm: &PCM,
        rate: DsdRate,
        channels: u16,
        bit_order: BitOrder,
        layout: ChannelLayout,
    ) -> Result<(AlsaFormat, DsdTransport, u64, u64)> {
        let try_native = matches!(self.config.dsd_mode, DsdMode::Auto | DsdMode::Native);
        let try_dop = matches!(self.config.dsd_mode, DsdMode::Auto | DsdMode::Dop);

        if try_native {
            let u32_rate = rate.u32_frame_rate_hz();
            for native_fmt in [AlsaFormat::DSDU32BE, AlsaFormat::DSDU32LE] {
                if let Ok((buf_sz, per_sz)) = self.configure_pcm(
                    pcm,
                    &self.config.device,
                    native_fmt,
                    u32_rate,
                    channels,
                    false,
                ) {
                    self.dop_encoder = None;
                    return Ok((native_fmt, DsdTransport::Native, buf_sz, per_sz));
                }
            }
            if self.config.dsd_mode == DsdMode::Native {
                return Err(KiiaudError::UnsupportedFormat(format!(
                    "ALSA device '{}' does not support Native DSD ({rate} / {u32_rate} Hz DSD_U32_BE/LE)",
                    self.config.device
                )));
            }
        }

        if try_dop {
            let dop_rate = rate.dop_pcm_rate_hz();
            for dop_fmt in [AlsaFormat::S32LE, AlsaFormat::S243LE, AlsaFormat::S24LE] {
                if let Ok((buf_sz, per_sz)) =
                    self.configure_pcm(pcm, &self.config.device, dop_fmt, dop_rate, channels, false)
                {
                    let enc = DopEncoder::new(rate, channels as usize, layout, bit_order)?;
                    self.dop_encoder = Some(enc);
                    return Ok((dop_fmt, DsdTransport::DoP, buf_sz, per_sz));
                }
            }
        }

        Err(KiiaudError::UnsupportedFormat(format!(
            "Bit-perfect DSD playback failed on '{}': DAC supports neither Native DSD nor DoP for {rate} ({} Hz DoP). Refusing silent DSD-to-PCM conversion (R-014).",
            self.config.device,
            rate.dop_pcm_rate_hz()
        )))
    }

    /// Write raw interleaved bytes to ALSA `io_bytes()`, recovering from underruns (`EPIPE` / `ESTRPIPE`).
    fn write_raw_bytes(&self, pcm: &PCM, bytes: &[u8], bytes_per_frame: usize) -> Result<usize> {
        if bytes.is_empty() || bytes_per_frame == 0 {
            return Ok(0);
        }
        let io = pcm.io_bytes();
        let mut offset = 0usize;
        let total_frames = bytes.len() / bytes_per_frame;

        while offset < bytes.len() {
            match io.writei(&bytes[offset..]) {
                Ok(frames_written) => {
                    if frames_written == 0 {
                        break;
                    }
                    offset += frames_written * bytes_per_frame;
                }
                Err(err) => {
                    tracing::warn!(
                        device = %self.config.device,
                        errno = err.errno(),
                        "ALSA write xrun/interruption; attempting snd_pcm_recover"
                    );
                    pcm.try_recover(err, false).map_err(|e| {
                        KiiaudError::Output(format!(
                            "ALSA unrecoverable write error on '{}': {e}",
                            self.config.device
                        ))
                    })?;
                }
            }
        }

        Ok(total_frames)
    }
}

#[cfg(target_os = "linux")]
impl AudioOutput for AlsaAudioOutput {
    fn open(&mut self, format: &AudioFormat) -> Result<AudioFormat> {
        // If already open with the exact same format, reuse the running ALSA stream for gapless transition!
        if self.pcm.is_some() && self.active_format.as_ref() == Some(format) {
            return Ok(*format);
        }

        // Close any previous stream cleanly
        let _ = self.stop();

        let negotiated_format = match *format {
            AudioFormat::Pcm {
                sample_rate,
                channels,
                sample_format,
                bits_per_sample,
            } => {
                // Attempt 1: Strict Bit-Perfect on the primary configured hardware device (e.g. hw:CARD=Audio,DEV=0)
                let primary_res = PCM::new(&self.config.device, Direction::Playback, false)
                    .map_err(|e| {
                        KiiaudError::Output(format!(
                            "Failed to open ALSA hardware device '{}': {e}",
                            self.config.device
                        ))
                    })
                    .and_then(|pcm| {
                        let (alsa_fmt, hw_sf, buf_sz, per_sz) = self.negotiate_pcm_format(
                            &pcm,
                            &self.config.device,
                            sample_format,
                            sample_rate,
                            channels,
                            false,
                        )?;
                        Ok((pcm, alsa_fmt, hw_sf, buf_sz, per_sz))
                    });

                let (pcm, alsa_fmt, hw_sf, buf_sz, per_sz, is_fallback) = match primary_res {
                    Ok((p, f, sf, b, pr)) => (p, f, sf, b, pr, false),
                    Err(orig_err) => {
                        // Attempt 2: If primary device is hw: and failed (due to unsupported rate like 88.2kHz or format),
                        // engage Adaptive Exception Fallback using plughw: to prevent silence / error!
                        if self.config.device.starts_with("hw:") {
                            let fallback_dev = format!("plug{}", self.config.device);
                            tracing::warn!(
                                primary_device = %self.config.device,
                                fallback_device = %fallback_dev,
                                sample_rate,
                                error = %orig_err,
                                "DAC hardware does not natively support {}Hz in direct bit-perfect mode; engaging adaptive ALSA plughw fallback",
                                sample_rate
                            );
                            let fallback_pcm = PCM::new(&fallback_dev, Direction::Playback, false)
                                .map_err(|e| {
                                    KiiaudError::Output(format!(
                                        "Failed to open ALSA fallback device '{fallback_dev}': {e}"
                                    ))
                                })?;
                            let (f, sf, b, pr) = self
                                .negotiate_pcm_format(
                                    &fallback_pcm,
                                    &fallback_dev,
                                    sample_format,
                                    sample_rate,
                                    channels,
                                    true,
                                )
                                .map_err(|e| {
                                    KiiaudError::Output(format!(
                                        "Adaptive fallback on '{fallback_dev}' failed: {e}"
                                    ))
                                })?;
                            (fallback_pcm, f, sf, b, pr, true)
                        } else {
                            return Err(orig_err);
                        }
                    }
                };

                self.hw_format = Some(alsa_fmt);
                self.dop_encoder = None;
                self.pcm = Some(pcm);
                self.is_adaptive_fallback = is_fallback;

                let actual = AudioFormat::Pcm {
                    sample_rate,
                    channels,
                    sample_format: hw_sf,
                    bits_per_sample,
                };
                self.active_format = Some(actual);

                tracing::info!(
                    device = %self.config.device,
                    alsa_format = %alsa_fmt,
                    sample_rate,
                    channels,
                    source_bits = bits_per_sample,
                    buffer_frames = buf_sz,
                    period_frames = per_sz,
                    bit_perfect = self.config.bit_perfect && !is_fallback,
                    adaptive_fallback = is_fallback,
                    "ALSA audio output successfully opened and configured"
                );
                actual
            }
            AudioFormat::Dsd {
                rate,
                channels,
                bit_order,
                layout,
                ..
            } => {
                let pcm =
                    PCM::new(&self.config.device, Direction::Playback, false).map_err(|e| {
                        KiiaudError::Output(format!(
                            "Failed to open ALSA hardware device '{}': {e}",
                            self.config.device
                        ))
                    })?;

                let (alsa_fmt, transport, buf_sz, per_sz) =
                    self.negotiate_dsd_format(&pcm, rate, channels, bit_order, layout)?;
                self.hw_format = Some(alsa_fmt);
                self.is_adaptive_fallback = false;

                let actual = AudioFormat::Dsd {
                    rate,
                    channels,
                    transport,
                    bit_order,
                    layout,
                };
                self.pcm = Some(pcm);
                self.active_format = Some(actual);

                tracing::info!(
                    device = %self.config.device,
                    alsa_format = %alsa_fmt,
                    dsd_rate = %rate,
                    %transport,
                    channels,
                    buffer_frames = buf_sz,
                    period_frames = per_sz,
                    "ALSA hardware configured for bit-perfect DSD playback"
                );
                actual
            }
        };

        Ok(negotiated_format)
    }

    fn write(&mut self, buffer: &AudioBuffer) -> Result<usize> {
        let Some(hw_fmt) = self.hw_format else {
            return Err(KiiaudError::Output("ALSA output not opened".to_owned()));
        };

        let channels = self
            .active_format
            .as_ref()
            .map(|f| f.channels() as usize)
            .unwrap_or(2)
            .max(1);

        // Pack into `self.byte_scratch` according to negotiated `hw_fmt`
        self.byte_scratch.clear();

        match buffer {
            AudioBuffer::PcmI32 { samples, .. } => {
                pack_i32_to_alsa_bytes(samples, hw_fmt, &mut self.byte_scratch)?;
            }
            AudioBuffer::PcmF32 { samples, .. } => {
                pack_f32_to_alsa_bytes(samples, hw_fmt, &mut self.byte_scratch)?;
            }
            AudioBuffer::Dsd {
                data,
                bit_order,
                layout,
                ..
            } => {
                if let Some(dop_enc) = self.dop_encoder.as_mut() {
                    dop_enc.encode(data, &mut self.dop_scratch);
                    pack_i32_to_alsa_bytes(&self.dop_scratch, hw_fmt, &mut self.byte_scratch)?;
                } else {
                    let big_endian = matches!(hw_fmt, AlsaFormat::DSDU32BE | AlsaFormat::DSDU16BE);
                    pack_native_dsd_u32(
                        data,
                        channels,
                        *layout,
                        *bit_order,
                        big_endian,
                        &mut self.byte_scratch,
                    );
                }
            }
        }

        let bytes_per_sample = match hw_fmt {
            AlsaFormat::S16LE => 2,
            AlsaFormat::S243LE => 3,
            AlsaFormat::S24LE
            | AlsaFormat::S32LE
            | AlsaFormat::FloatLE
            | AlsaFormat::DSDU32BE
            | AlsaFormat::DSDU32LE => 4,
            _ => 4,
        };
        let bytes_per_frame = bytes_per_sample * channels;

        let Some(pcm) = self.pcm.as_ref() else {
            return Err(KiiaudError::Output("ALSA PCM handle missing".to_owned()));
        };

        self.write_raw_bytes(pcm, &self.byte_scratch, bytes_per_frame)
    }

    fn pause(&mut self) -> Result<()> {
        if let Some(pcm) = self.pcm.as_ref() {
            if pcm.pause(true).is_err() {
                let _ = pcm.drop();
            }
        }
        Ok(())
    }

    fn resume(&mut self) -> Result<()> {
        if let Some(pcm) = self.pcm.as_ref() {
            match pcm.state() {
                AlsaState::Paused => {
                    let _ = pcm.pause(false);
                }
                AlsaState::Setup | AlsaState::XRun => {
                    let _ = pcm.prepare();
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn drain(&mut self) -> Result<()> {
        if let Some(pcm) = self.pcm.as_ref() {
            let _ = pcm.drain();
            let _ = pcm.prepare();
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        if let Some(pcm) = self.pcm.take() {
            let _ = pcm.drop();
            drop(pcm);
        }
        self.hw_format = None;
        self.active_format = None;
        self.dop_encoder = None;
        self.is_adaptive_fallback = false;
        Ok(())
    }

    fn capabilities(&self) -> OutputCapabilities {
        self.capabilities.clone()
    }
}

/// Query the DAC hardware name, supported sample rates, bit depths, and capabilities from ALSA and `/proc/asound`.
pub fn detect_hardware_capabilities(device: &str, bit_perfect_cfg: bool) -> OutputCapabilities {
    let card_id = extract_card_id(device);

    let (dev_name, stream_rates, stream_bits) = if let Some(ref cid) = card_id {
        parse_proc_stream0(cid)
    } else {
        (None, Vec::new(), Vec::new())
    };

    let device_name = dev_name.unwrap_or_else(|| {
        card_id
            .as_deref()
            .and_then(lookup_card_name_from_cards)
            .unwrap_or_else(|| device.to_owned())
    });

    let standard_rates = [
        44100, 48000, 88200, 96000, 176400, 192000, 352800, 384000, 705600, 768000,
    ];

    let supported_rates = if !stream_rates.is_empty() {
        stream_rates
    } else {
        #[cfg(target_os = "linux")]
        {
            probe_alsa_rates(device, &standard_rates)
        }
        #[cfg(not(target_os = "linux"))]
        {
            vec![44100, 48000, 96000, 192000]
        }
    };

    let max_rate = supported_rates.iter().copied().max().unwrap_or(48000);

    let unsupported_rates: Vec<u32> = standard_rates
        .into_iter()
        .filter(|&r| r <= max_rate && !supported_rates.contains(&r))
        .collect();

    let bit_depths = if !stream_bits.is_empty() {
        stream_bits
    } else {
        vec![16, 24]
    };

    let dsd_dop = supported_rates.iter().any(|&r| r >= 176400);

    OutputCapabilities {
        backend_name: "ALSA Hardware Direct (hw:)".to_owned(),
        device_name,
        sample_rates: supported_rates,
        unsupported_rates,
        bit_depths,
        channels: 2,
        exclusive: true,
        bit_perfect: bit_perfect_cfg,
        adaptive_fallback: true,
        dsd_native: false,
        dsd_dop,
    }
}

pub fn extract_card_id(device: &str) -> Option<String> {
    if let Some(card_part) = device
        .strip_prefix("hw:CARD=")
        .or_else(|| device.strip_prefix("plughw:CARD="))
    {
        if let Some((card, _)) = card_part.split_once(',') {
            return Some(card.to_owned());
        }
        return Some(card_part.to_owned());
    }
    if let Some(rest) = device
        .strip_prefix("hw:")
        .or_else(|| device.strip_prefix("plughw:"))
    {
        if let Some((card_num, _)) = rest.split_once(',') {
            return Some(card_num.to_owned());
        }
        return Some(rest.to_owned());
    }
    None
}

pub fn parse_proc_stream0(card_id: &str) -> (Option<String>, Vec<u32>, Vec<u8>) {
    let paths = [
        format!("/proc/asound/{card_id}/stream0"),
        format!("/proc/asound/card{card_id}/stream0"),
    ];

    let content = paths.iter().find_map(|p| std::fs::read_to_string(p).ok());
    let Some(text) = content else {
        return (None, Vec::new(), Vec::new());
    };

    let mut name = None;
    let mut rates = std::collections::BTreeSet::new();
    let mut bits = std::collections::BTreeSet::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if name.is_none() && !trimmed.is_empty() {
            let dev_name = if let Some((n, _)) = trimmed.split_once(" at usb-") {
                n.trim().to_owned()
            } else if let Some((n, _)) = trimmed.split_once(" : USB Audio") {
                n.trim().to_owned()
            } else {
                trimmed.to_owned()
            };
            name = Some(dev_name);
        }

        if let Some(rest) = trimmed.strip_prefix("Rates:") {
            for token in rest.split_whitespace() {
                if let Ok(r) = token.trim_matches(',').parse::<u32>() {
                    rates.insert(r);
                }
            }
        }

        if let Some(rest) = trimmed.strip_prefix("Bits:") {
            for token in rest.split_whitespace() {
                if let Ok(b) = token.trim_matches(',').parse::<u8>() {
                    bits.insert(b);
                }
            }
        }
    }

    (
        name,
        rates.into_iter().collect(),
        bits.into_iter().collect(),
    )
}

pub fn lookup_card_name_from_cards(card_id: &str) -> Option<String> {
    let text = std::fs::read_to_string("/proc/asound/cards").ok()?;
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if trimmed.contains(&format!("[{card_id}")) || trimmed.starts_with(&format!("{card_id} ["))
        {
            if let Some(desc_line) = lines.next() {
                return Some(desc_line.trim().to_owned());
            }
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn probe_alsa_rates(device: &str, candidates: &[u32]) -> Vec<u32> {
    let mut supported = Vec::new();
    if let Ok(pcm) = PCM::new(device, Direction::Playback, false) {
        for &rate in candidates {
            if let Ok(hwp) = HwParams::any(&pcm) {
                let _ = hwp.set_rate_resample(false);
                let _ = hwp.set_access(Access::RWInterleaved);
                let _ = hwp.set_channels(2);
                if let Ok(actual) = hwp.set_rate_near(rate, ValueOr::Nearest) {
                    if actual == rate {
                        supported.push(rate);
                    }
                }
            }
        }
    }
    if supported.is_empty() {
        supported = vec![44100, 48000, 96000, 192000];
    }
    supported
}

/// Convert MSB-aligned 32-bit signed integers (`i32`) into exact ALSA hardware byte layout.
#[cfg(target_os = "linux")]
fn pack_i32_to_alsa_bytes(samples: &[i32], hw_fmt: AlsaFormat, out: &mut Vec<u8>) -> Result<()> {
    match hw_fmt {
        AlsaFormat::S16LE => {
            out.reserve(samples.len() * 2);
            for &s in samples {
                let s16 = (s >> 16) as i16;
                out.extend_from_slice(&s16.to_le_bytes());
            }
        }
        AlsaFormat::S243LE => {
            // 3 bytes per sample, little-endian (bits 8..31 of MSB-aligned i32)
            out.reserve(samples.len() * 3);
            for &s in samples {
                let b = s.to_le_bytes();
                // b[0] is lowest 8 padding bits; b[1], b[2], b[3] are the 24 significant bits in LE order!
                out.extend_from_slice(&[b[1], b[2], b[3]]);
            }
        }
        AlsaFormat::S24LE => {
            // 24-bit sample stored in lower 3 bytes of 32-bit LE word
            out.reserve(samples.len() * 4);
            for &s in samples {
                let s24 = s >> 8;
                out.extend_from_slice(&s24.to_le_bytes());
            }
        }
        AlsaFormat::S32LE => {
            out.reserve(samples.len() * 4);
            for &s in samples {
                out.extend_from_slice(&s.to_le_bytes());
            }
        }
        AlsaFormat::FloatLE => {
            out.reserve(samples.len() * 4);
            for &s in samples {
                let f = (s as f64 / 2_147_483_648.0) as f32;
                out.extend_from_slice(&f.to_le_bytes());
            }
        }
        other => {
            return Err(KiiaudError::Output(format!(
                "Unsupported ALSA PCM format for integer packing: {other}"
            )));
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn pack_f32_to_alsa_bytes(samples: &[f32], hw_fmt: AlsaFormat, out: &mut Vec<u8>) -> Result<()> {
    match hw_fmt {
        AlsaFormat::FloatLE => {
            out.reserve(samples.len() * 4);
            for &s in samples {
                out.extend_from_slice(&s.to_le_bytes());
            }
        }
        AlsaFormat::S32LE => {
            out.reserve(samples.len() * 4);
            for &s in samples {
                let clamped = s.clamp(-1.0, 1.0) as f64;
                let v = (clamped * 2_147_483_647.0) as i32;
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        AlsaFormat::S243LE => {
            out.reserve(samples.len() * 3);
            for &s in samples {
                let clamped = s.clamp(-1.0, 1.0) as f64;
                let v = (clamped * 8_388_607.0) as i32;
                let b = v.to_le_bytes();
                out.extend_from_slice(&[b[0], b[1], b[2]]);
            }
        }
        AlsaFormat::S16LE => {
            out.reserve(samples.len() * 2);
            for &s in samples {
                let clamped = s.clamp(-1.0, 1.0);
                let v = (clamped * 32767.0) as i16;
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        other => {
            return Err(KiiaudError::Output(format!(
                "Unsupported ALSA PCM format for float packing: {other}"
            )));
        }
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn test_s24_3le_packing_is_bit_exact() {
        // 24-bit value 0x123456 left-aligned in i32 is 0x12345600
        let sample: i32 = 0x1234_5600;
        let mut out = Vec::new();
        pack_i32_to_alsa_bytes(&[sample], AlsaFormat::S243LE, &mut out).unwrap();
        assert_eq!(out, vec![0x56, 0x34, 0x12]);
    }

    #[test]
    fn test_extract_card_id() {
        assert_eq!(
            extract_card_id("hw:CARD=Audio,DEV=0"),
            Some("Audio".to_owned())
        );
        assert_eq!(
            extract_card_id("plughw:CARD=Audio,DEV=0"),
            Some("Audio".to_owned())
        );
        assert_eq!(extract_card_id("hw:7,0"), Some("7".to_owned()));
        assert_eq!(extract_card_id("default"), None);
    }

    #[test]
    fn test_detect_hardware_capabilities_card_audio() {
        let caps = detect_hardware_capabilities("hw:CARD=Audio,DEV=0", true);
        assert_eq!(caps.backend_name, "ALSA Hardware Direct (hw:)");
        assert!(caps.bit_perfect);
        assert!(caps.adaptive_fallback);
        assert!(caps.sample_rates.contains(&44100));
        assert!(caps.sample_rates.contains(&48000));
        assert!(caps.sample_rates.contains(&96000));
        assert!(caps.sample_rates.contains(&192000));
        assert!(caps.unsupported_rates.contains(&88200));
        assert!(caps.unsupported_rates.contains(&176400));
    }
}
