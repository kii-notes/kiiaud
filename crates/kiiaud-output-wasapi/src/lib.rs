// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bit-perfect WASAPI Exclusive Mode audio output backend (`kiiaud-output-wasapi`).
//!
//! Features:
//! - Hardware-direct access bypassing Windows Audio Engine (`AUDCLNT_SHAREMODE_EXCLUSIVE`).
//! - Event-driven streaming (`AUDCLNT_STREAMFLAGS_EVENTCALLBACK`) for glitch-free low latency.
//! - Bit-perfect PCM playback (16-bit, 24-bit in 32-bit container, 24-bit packed, 32-bit float, 32-bit int).
//! - DoP (DSD over PCM) bit-perfect transport for DSF, DFF, and WavPack DSD audio.
//! - MMCSS (Multimedia Class Scheduler Service) "Pro Audio" registration for real-time priority.

#[cfg(any(test, target_os = "windows"))]
use kiiaud_core::SampleFormat;
use kiiaud_core::config::OutputConfig;
use kiiaud_core::{AudioBuffer, AudioFormat, KiiaudError, OutputCapabilities, Result};
#[cfg(target_os = "windows")]
use kiiaud_core::{DsdMode, DsdTransport};
use kiiaud_output::AudioOutput;
use kiiaud_output::dop::DopEncoder;

#[cfg(target_os = "windows")]
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
        Media::Audio::{
            AUDCLNT_SHAREMODE_EXCLUSIVE, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, IAudioClient,
            IAudioRenderClient, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX,
            WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0, eConsole, eRender,
        },
        System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx},
        System::Threading::{
            AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW,
            WaitForSingleObject,
        },
    },
    core::{PCWSTR, w},
};

#[cfg(target_os = "windows")]
const KSDATAFORMAT_SUBTYPE_PCM: windows::core::GUID =
    windows::core::GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71);

#[cfg(target_os = "windows")]
const KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: windows::core::GUID =
    windows::core::GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WasapiFormat {
    S16LE,
    S24_3LE,
    S24In32LE,
    S32LE,
    Float32LE,
}

impl WasapiFormat {
    pub fn bytes_per_sample(self) -> usize {
        match self {
            Self::S16LE => 2,
            Self::S24_3LE => 3,
            Self::S24In32LE | Self::S32LE | Self::Float32LE => 4,
        }
    }
}

#[allow(dead_code)]
pub struct WasapiAudioOutput {
    config: OutputConfig,
    #[cfg(target_os = "windows")]
    client: Option<IAudioClient>,
    #[cfg(target_os = "windows")]
    render_client: Option<IAudioRenderClient>,
    #[cfg(target_os = "windows")]
    event_handle: Option<HANDLE>,
    #[cfg(target_os = "windows")]
    mmcss_handle: Option<HANDLE>,
    #[cfg(target_os = "windows")]
    hw_format: Option<WasapiFormat>,
    #[cfg(target_os = "windows")]
    buffer_frames: u32,
    #[cfg(target_os = "windows")]
    bytes_per_frame: usize,
    #[cfg(target_os = "windows")]
    is_playing: bool,

    active_format: Option<AudioFormat>,
    dop_encoder: Option<DopEncoder>,
    dop_scratch: Vec<i32>,
    byte_scratch: Vec<u8>,
    fifo_buffer: Vec<u8>,
}

impl WasapiAudioOutput {
    pub fn new(config: OutputConfig) -> Self {
        Self {
            config,
            #[cfg(target_os = "windows")]
            client: None,
            #[cfg(target_os = "windows")]
            render_client: None,
            #[cfg(target_os = "windows")]
            event_handle: None,
            #[cfg(target_os = "windows")]
            mmcss_handle: None,
            #[cfg(target_os = "windows")]
            hw_format: None,
            #[cfg(target_os = "windows")]
            buffer_frames: 0,
            #[cfg(target_os = "windows")]
            bytes_per_frame: 0,
            #[cfg(target_os = "windows")]
            is_playing: false,

            active_format: None,
            dop_encoder: None,
            dop_scratch: Vec::new(),
            byte_scratch: Vec::new(),
            fifo_buffer: Vec::new(),
        }
    }
}

// Safe because WasapiAudioOutput is exclusively used on the dedicated audio worker thread
unsafe impl Send for WasapiAudioOutput {}

#[cfg(target_os = "windows")]
impl AudioOutput for WasapiAudioOutput {
    fn open(&mut self, format: &AudioFormat) -> Result<AudioFormat> {
        // Gapless reuse if format matches exactly
        if self.client.is_some() && self.active_format.as_ref() == Some(format) {
            return Ok(*format);
        }

        // Close any prior stream
        let _ = self.stop();

        // 1. Initialize COM on the current thread
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }

        // 2. Obtain device enumerator & endpoint device
        let enumerator: IMMDeviceEnumerator = unsafe {
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
        }
        .map_err(|e| KiiaudError::Output(format!("Failed to create MMDeviceEnumerator: {e}")))?;

        let device: IMMDevice = if self.config.device.is_empty()
            || self.config.device.eq_ignore_ascii_case("default")
        {
            unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }.map_err(|e| {
                KiiaudError::Output(format!("Failed to get default audio endpoint: {e}"))
            })?
        } else {
            // Check if device is a wide-character endpoint ID string
            let dev_wide: Vec<u16> = self
                .config
                .device
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            unsafe { enumerator.GetDevice(PCWSTR(dev_wide.as_ptr())) }.unwrap_or_else(|_| {
                tracing::warn!(
                    device = %self.config.device,
                    "Device endpoint ID not found directly; falling back to default audio endpoint"
                );
                unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
                    .expect("Failed to get default audio endpoint")
            })
        };

        // 3. Activate IAudioClient
        let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }
            .map_err(|e| KiiaudError::Output(format!("Failed to activate IAudioClient: {e}")))?;

        // 4. Negotiate bit-perfect format
        let (negotiated_format, selected_fmt, wave_ext) = match *format {
            AudioFormat::Pcm {
                sample_rate,
                channels,
                sample_format,
                bits_per_sample,
            } => {
                let candidates = match sample_format {
                    SampleFormat::S16Le => [
                        WasapiFormat::S16LE,
                        WasapiFormat::S24In32LE,
                        WasapiFormat::S32LE,
                        WasapiFormat::Float32LE,
                    ],
                    SampleFormat::S24Le | SampleFormat::S24_3Le => [
                        WasapiFormat::S24In32LE,
                        WasapiFormat::S24_3LE,
                        WasapiFormat::S32LE,
                        WasapiFormat::Float32LE,
                    ],
                    SampleFormat::S32Le => [
                        WasapiFormat::S32LE,
                        WasapiFormat::S24In32LE,
                        WasapiFormat::Float32LE,
                        WasapiFormat::S16LE,
                    ],
                    SampleFormat::F32Le => [
                        WasapiFormat::Float32LE,
                        WasapiFormat::S32LE,
                        WasapiFormat::S24In32LE,
                        WasapiFormat::S16LE,
                    ],
                };

                let mut matched = None;
                for cand in candidates {
                    let wfx = make_waveformat(cand, sample_rate, channels);
                    let res = unsafe {
                        client.IsFormatSupported(
                            AUDCLNT_SHAREMODE_EXCLUSIVE,
                            &wfx.Format as *const _,
                            None,
                        )
                    };
                    if res.is_ok() {
                        matched = Some((cand, wfx));
                        break;
                    }
                }

                let (cand, wfx) = matched.ok_or_else(|| {
                    KiiaudError::UnsupportedFormat(format!(
                        "No compatible bit-perfect WASAPI format found for {sample_format} at {sample_rate} Hz"
                    ))
                })?;

                self.dop_encoder = None;

                let hw_sf = match cand {
                    WasapiFormat::S16LE => SampleFormat::S16Le,
                    WasapiFormat::S24_3LE => SampleFormat::S24_3Le,
                    WasapiFormat::S24In32LE => SampleFormat::S24Le,
                    WasapiFormat::S32LE => SampleFormat::S32Le,
                    WasapiFormat::Float32LE => SampleFormat::F32Le,
                };

                let actual = AudioFormat::Pcm {
                    sample_rate,
                    channels,
                    sample_format: hw_sf,
                    bits_per_sample,
                };
                (actual, cand, wfx)
            }
            AudioFormat::Dsd {
                rate,
                channels,
                bit_order,
                layout,
                ..
            } => {
                if self.config.dsd_mode == DsdMode::Native {
                    return Err(KiiaudError::UnsupportedFormat(
                        "WASAPI does not support Native DSD stream tagging; set dsd_mode = 'dop' or 'auto' for bit-perfect DSD playback via DoP on Windows".to_owned(),
                    ));
                }

                let dop_rate = rate.dop_pcm_rate_hz();
                let candidates = [
                    WasapiFormat::S24In32LE,
                    WasapiFormat::S32LE,
                    WasapiFormat::S24_3LE,
                ];

                let mut matched = None;
                for cand in candidates {
                    let wfx = make_waveformat(cand, dop_rate, channels);
                    let res = unsafe {
                        client.IsFormatSupported(
                            AUDCLNT_SHAREMODE_EXCLUSIVE,
                            &wfx.Format as *const _,
                            None,
                        )
                    };
                    if res.is_ok() {
                        matched = Some((cand, wfx));
                        break;
                    }
                }

                let (cand, wfx) = matched.ok_or_else(|| {
                    KiiaudError::UnsupportedFormat(format!(
                        "DAC does not support DoP for {rate} ({dop_rate} Hz) in WASAPI Exclusive Mode"
                    ))
                })?;

                let enc = DopEncoder::new(rate, channels as usize, layout, bit_order)?;
                self.dop_encoder = Some(enc);

                let actual = AudioFormat::Dsd {
                    rate,
                    channels,
                    transport: DsdTransport::DoP,
                    bit_order,
                    layout,
                };
                (actual, cand, wfx)
            }
        };

        // 5. Query device periodicity
        let mut default_period: i64 = 0;
        let mut min_period: i64 = 0;
        unsafe { client.GetDevicePeriod(Some(&mut default_period), Some(&mut min_period)) }
            .map_err(|e| KiiaudError::Output(format!("Failed to get device period: {e}")))?;

        // Duration in 100ns units:
        let requested_period = (self.config.period_time_ms as i64 * 10_000).max(min_period);

        // 6. Initialize AudioClient with Exclusive Event-Driven mode
        let init_res = unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_EXCLUSIVE,
                AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                requested_period,
                requested_period,
                &wave_ext.Format as *const _,
                None,
            )
        };

        if let Err(e) = init_res {
            return Err(KiiaudError::Output(format!(
                "Failed to initialize WASAPI Exclusive mode on device '{}': {e}",
                self.config.device
            )));
        }

        // 7. Create event and bind to client
        let event = unsafe { CreateEventW(None, false, false, None) }
            .map_err(|e| KiiaudError::Output(format!("Failed to create Win32 event: {e}")))?;

        unsafe { client.SetEventHandle(event) }
            .map_err(|e| KiiaudError::Output(format!("Failed to set event handle: {e}")))?;

        // 8. Obtain render service & buffer size
        let render_client: IAudioRenderClient = unsafe { client.GetService() }
            .map_err(|e| KiiaudError::Output(format!("Failed to get IAudioRenderClient: {e}")))?;

        let buffer_frames = unsafe { client.GetBufferSize() }
            .map_err(|e| KiiaudError::Output(format!("Failed to get buffer size: {e}")))?;

        let channels = negotiated_format.channels();
        let bytes_per_frame = (channels as usize) * selected_fmt.bytes_per_sample();

        // 9. Register thread with MMCSS "Pro Audio"
        let mut task_index = 0u32;
        let mmcss = unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task_index) };
        let mmcss_handle = mmcss.ok();

        // 10. Start playback stream
        unsafe { client.Start() }
            .map_err(|e| KiiaudError::Output(format!("Failed to start WASAPI client: {e}")))?;

        let rate_hz = match negotiated_format {
            AudioFormat::Pcm { sample_rate, .. } => sample_rate,
            AudioFormat::Dsd { rate, .. } => rate.dop_pcm_rate_hz(),
        };

        tracing::info!(
            device = %self.config.device,
            wasapi_format = ?selected_fmt,
            sample_rate = rate_hz,
            channels,
            buffer_frames,
            bytes_per_frame,
            bit_perfect = true,
            "WASAPI Exclusive Mode stream opened and started"
        );

        self.client = Some(client);
        self.render_client = Some(render_client);
        self.event_handle = Some(event);
        self.mmcss_handle = mmcss_handle;
        self.hw_format = Some(selected_fmt);
        self.buffer_frames = buffer_frames;
        self.bytes_per_frame = bytes_per_frame;
        self.is_playing = true;
        self.active_format = Some(negotiated_format);
        self.fifo_buffer.clear();

        Ok(negotiated_format)
    }

    fn write(&mut self, buffer: &AudioBuffer) -> Result<usize> {
        let (Some(render_client), Some(event_handle), Some(hw_fmt)) = (
            self.render_client.as_ref(),
            self.event_handle,
            self.hw_format,
        ) else {
            return Err(KiiaudError::Output("WASAPI output not opened".to_owned()));
        };

        // Pack buffer to bytes according to hardware format
        self.byte_scratch.clear();
        match buffer {
            AudioBuffer::PcmI32 { samples, .. } => {
                pack_i32_to_wasapi_bytes(samples, hw_fmt, &mut self.byte_scratch)?;
            }
            AudioBuffer::PcmF32 { samples, .. } => {
                pack_f32_to_wasapi_bytes(samples, hw_fmt, &mut self.byte_scratch)?;
            }
            AudioBuffer::Dsd { data, .. } => {
                if let Some(dop_enc) = self.dop_encoder.as_mut() {
                    dop_enc.encode(data, &mut self.dop_scratch);
                    pack_i32_to_wasapi_bytes(&self.dop_scratch, hw_fmt, &mut self.byte_scratch)?;
                } else {
                    return Err(KiiaudError::Output(
                        "DSD playback without DoP encoder on WASAPI is unsupported".to_owned(),
                    ));
                }
            }
        }

        self.fifo_buffer.extend_from_slice(&self.byte_scratch);

        let period_bytes = (self.buffer_frames as usize) * self.bytes_per_frame;
        if period_bytes == 0 {
            return Ok(buffer.frames());
        }

        while self.fifo_buffer.len() >= period_bytes {
            // Wait for WASAPI driver event (timeout 2 seconds)
            let wait_res = unsafe { WaitForSingleObject(event_handle, 2000) };
            if wait_res != WAIT_OBJECT_0 {
                tracing::warn!(
                    wait_code = wait_res.0,
                    "WASAPI WaitForSingleObject timeout or interruption"
                );
            }

            let p_buffer = unsafe { render_client.GetBuffer(self.buffer_frames) }.map_err(|e| {
                KiiaudError::Output(format!(
                    "WASAPI GetBuffer({}) failed: {e}",
                    self.buffer_frames
                ))
            })?;

            unsafe {
                std::ptr::copy_nonoverlapping(self.fifo_buffer.as_ptr(), p_buffer, period_bytes);
            }

            unsafe { render_client.ReleaseBuffer(self.buffer_frames, 0) }
                .map_err(|e| KiiaudError::Output(format!("WASAPI ReleaseBuffer failed: {e}")))?;

            self.fifo_buffer.drain(0..period_bytes);
        }

        Ok(buffer.frames())
    }

    fn pause(&mut self) -> Result<()> {
        if let Some(client) = self.client.as_ref() {
            if self.is_playing {
                let _ = unsafe { client.Stop() };
                self.is_playing = false;
            }
        }
        Ok(())
    }

    fn resume(&mut self) -> Result<()> {
        if let Some(client) = self.client.as_ref() {
            if !self.is_playing {
                let _ = unsafe { client.Start() };
                self.is_playing = true;
            }
        }
        Ok(())
    }

    fn drain(&mut self) -> Result<()> {
        let (Some(render_client), Some(event_handle)) =
            (self.render_client.as_ref(), self.event_handle)
        else {
            return Ok(());
        };

        let period_bytes = (self.buffer_frames as usize) * self.bytes_per_frame;
        if period_bytes > 0 && !self.fifo_buffer.is_empty() {
            // Pad remaining frames with zeros
            if self.fifo_buffer.len() < period_bytes {
                self.fifo_buffer.resize(period_bytes, 0);
            }

            let _ = unsafe { WaitForSingleObject(event_handle, 2000) };
            if let Ok(p_buffer) = unsafe { render_client.GetBuffer(self.buffer_frames) } {
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        self.fifo_buffer.as_ptr(),
                        p_buffer,
                        period_bytes,
                    );
                    let _ = render_client.ReleaseBuffer(self.buffer_frames, 0);
                }
            }
            self.fifo_buffer.clear();
        }

        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        if let Some(client) = self.client.take() {
            if self.is_playing {
                let _ = unsafe { client.Stop() };
                let _ = unsafe { client.Reset() };
                self.is_playing = false;
            }
        }

        if let Some(mmcss) = self.mmcss_handle.take() {
            let _ = unsafe { AvRevertMmThreadCharacteristics(mmcss) };
        }

        if let Some(event) = self.event_handle.take() {
            let _ = unsafe { CloseHandle(event) };
        }

        self.render_client = None;
        self.hw_format = None;
        self.active_format = None;
        self.dop_encoder = None;
        self.fifo_buffer.clear();
        Ok(())
    }

    fn capabilities(&self) -> OutputCapabilities {
        OutputCapabilities {
            backend_name: "WASAPI Exclusive Mode".to_owned(),
            device_name: "WASAPI Audio Device".to_owned(),
            sample_rates: vec![44100, 48000, 88200, 96000, 176400, 192000, 352800, 384000],
            unsupported_rates: Vec::new(),
            bit_depths: vec![16, 24, 32],
            channels: 2,
            exclusive: true,
            bit_perfect: self.config.bit_perfect,
            adaptive_fallback: true,
            dsd_native: false,
            dsd_dop: true,
        }
    }
}

#[cfg(target_os = "windows")]
fn make_waveformat(fmt: WasapiFormat, sample_rate: u32, channels: u16) -> WAVEFORMATEXTENSIBLE {
    let (bits_per_sample, valid_bits, subformat) = match fmt {
        WasapiFormat::S16LE => (16, 16, KSDATAFORMAT_SUBTYPE_PCM),
        WasapiFormat::S24_3LE => (24, 24, KSDATAFORMAT_SUBTYPE_PCM),
        WasapiFormat::S24In32LE => (32, 24, KSDATAFORMAT_SUBTYPE_PCM),
        WasapiFormat::S32LE => (32, 32, KSDATAFORMAT_SUBTYPE_PCM),
        WasapiFormat::Float32LE => (32, 32, KSDATAFORMAT_SUBTYPE_IEEE_FLOAT),
    };

    let block_align = channels * (bits_per_sample / 8);
    let avg_bytes_per_sec = sample_rate * (block_align as u32);

    let channel_mask = match channels {
        1 => 0x4,   // SPEAKER_FRONT_CENTER
        2 => 0x3,   // SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT
        4 => 0x33,  // 4.0
        6 => 0x3F,  // 5.1
        8 => 0x63F, // 7.1
        _ => 0,
    };

    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: 0xFFFE, // WAVE_FORMAT_EXTENSIBLE
            nChannels: channels,
            nSamplesPerSec: sample_rate,
            nAvgBytesPerSec: avg_bytes_per_sec,
            nBlockAlign: block_align,
            wBitsPerSample: bits_per_sample,
            cbSize: 22,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 {
            wValidBitsPerSample: valid_bits,
        },
        dwChannelMask: channel_mask,
        SubFormat: subformat,
    }
}

/// Convert MSB-aligned 32-bit signed integers (`i32`) into exact WASAPI byte layout.
#[allow(dead_code)]
fn pack_i32_to_wasapi_bytes(samples: &[i32], fmt: WasapiFormat, out: &mut Vec<u8>) -> Result<()> {
    match fmt {
        WasapiFormat::S16LE => {
            out.reserve(samples.len() * 2);
            for &s in samples {
                let s16 = (s >> 16) as i16;
                out.extend_from_slice(&s16.to_le_bytes());
            }
        }
        WasapiFormat::S24_3LE => {
            out.reserve(samples.len() * 3);
            for &s in samples {
                let b = s.to_le_bytes();
                out.extend_from_slice(&[b[1], b[2], b[3]]);
            }
        }
        WasapiFormat::S24In32LE | WasapiFormat::S32LE => {
            out.reserve(samples.len() * 4);
            for &s in samples {
                out.extend_from_slice(&s.to_le_bytes());
            }
        }
        WasapiFormat::Float32LE => {
            out.reserve(samples.len() * 4);
            for &s in samples {
                let f = (s as f64 / 2_147_483_648.0) as f32;
                out.extend_from_slice(&f.to_le_bytes());
            }
        }
    }
    Ok(())
}

/// Convert 32-bit floating point audio samples (`f32`) into exact WASAPI byte layout.
#[allow(dead_code)]
fn pack_f32_to_wasapi_bytes(samples: &[f32], fmt: WasapiFormat, out: &mut Vec<u8>) -> Result<()> {
    match fmt {
        WasapiFormat::Float32LE => {
            out.reserve(samples.len() * 4);
            for &s in samples {
                out.extend_from_slice(&s.to_le_bytes());
            }
        }
        WasapiFormat::S32LE | WasapiFormat::S24In32LE => {
            out.reserve(samples.len() * 4);
            for &s in samples {
                let clamped = s.clamp(-1.0, 1.0);
                let i = (clamped * 2_147_483_647.0) as i32;
                out.extend_from_slice(&i.to_le_bytes());
            }
        }
        WasapiFormat::S24_3LE => {
            out.reserve(samples.len() * 3);
            for &s in samples {
                let clamped = s.clamp(-1.0, 1.0);
                let i = (clamped * 2_147_483_647.0) as i32;
                let b = i.to_le_bytes();
                out.extend_from_slice(&[b[1], b[2], b[3]]);
            }
        }
        WasapiFormat::S16LE => {
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

#[cfg(not(target_os = "windows"))]
impl AudioOutput for WasapiAudioOutput {
    fn open(&mut self, _format: &AudioFormat) -> Result<AudioFormat> {
        Err(KiiaudError::Output(
            "WASAPI output is only supported on Windows".to_owned(),
        ))
    }

    fn write(&mut self, _buffer: &AudioBuffer) -> Result<usize> {
        Err(KiiaudError::Output(
            "WASAPI output is only supported on Windows".to_owned(),
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
        OutputCapabilities {
            backend_name: "WASAPI Exclusive Mode".to_owned(),
            device_name: "WASAPI Audio Device".to_owned(),
            sample_rates: vec![44100, 48000, 88200, 96000, 176400, 192000, 352800, 384000],
            unsupported_rates: Vec::new(),
            bit_depths: vec![16, 24, 32],
            channels: 2,
            exclusive: true,
            bit_perfect: self.config.bit_perfect,
            adaptive_fallback: true,
            dsd_native: false,
            dsd_dop: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_s16_le_packing_is_bit_exact() {
        // 16-bit value 0x1234 left-aligned in i32 is 0x12340000
        let sample: i32 = 0x1234_0000;
        let mut out = Vec::new();
        pack_i32_to_wasapi_bytes(&[sample], WasapiFormat::S16LE, &mut out).unwrap();
        assert_eq!(out, vec![0x34, 0x12]);
    }

    #[test]
    fn test_s24_3le_packing_is_bit_exact() {
        // 24-bit value 0x123456 left-aligned in i32 is 0x12345600
        let sample: i32 = 0x1234_5600;
        let mut out = Vec::new();
        pack_i32_to_wasapi_bytes(&[sample], WasapiFormat::S24_3LE, &mut out).unwrap();
        assert_eq!(out, vec![0x56, 0x34, 0x12]);
    }

    #[test]
    fn test_s24_in_32le_packing_is_bit_exact() {
        // In WAVEFORMATEXTENSIBLE with wValidBitsPerSample = 24 and wBitsPerSample = 32:
        // Samples are left-justified (MSB aligned: 0x12345600)
        let sample: i32 = 0x1234_5600;
        let mut out = Vec::new();
        pack_i32_to_wasapi_bytes(&[sample], WasapiFormat::S24In32LE, &mut out).unwrap();
        assert_eq!(out, vec![0x00, 0x56, 0x34, 0x12]);
    }

    #[test]
    fn test_wasapi_format_bytes_per_sample() {
        assert_eq!(WasapiFormat::S16LE.bytes_per_sample(), 2);
        assert_eq!(WasapiFormat::S24_3LE.bytes_per_sample(), 3);
        assert_eq!(WasapiFormat::S24In32LE.bytes_per_sample(), 4);
        assert_eq!(WasapiFormat::S32LE.bytes_per_sample(), 4);
        assert_eq!(WasapiFormat::Float32LE.bytes_per_sample(), 4);
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn test_non_windows_open_fails_gracefully() {
        let mut out = WasapiAudioOutput::new(OutputConfig::default());
        let fmt = AudioFormat::Pcm {
            sample_rate: 44100,
            channels: 2,
            sample_format: SampleFormat::S16Le,
            bits_per_sample: 16,
        };
        let err = out.open(&fmt).unwrap_err();
        assert!(
            err.to_string()
                .contains("WASAPI output is only supported on Windows")
        );
    }
}
