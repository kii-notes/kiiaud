// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runtime-loaded Android AAudio (`libaaudio.so`) output stream.
//!
//! Uses `dlopen("libaaudio.so")` so binaries compiled against NDK API 24 still link cleanly
//! while supporting full hardware-paced AAudio playback on Android 8.0+ (API 26+).

use crate::AndroidAudioFormat;
use kiiaud_core::{KiiaudError, Result};
use std::ffi::{CStr, c_char, c_void};
use std::ptr;
use std::sync::OnceLock;

const AAUDIO_OK: i32 = 0;
const AAUDIO_DIRECTION_OUTPUT: i32 = 0;

const AAUDIO_FORMAT_PCM_I16: i32 = 1;
const AAUDIO_FORMAT_PCM_FLOAT: i32 = 2;
const AAUDIO_FORMAT_PCM_I24_PACKED: i32 = 3;
const AAUDIO_FORMAT_PCM_I32: i32 = 4;

const AAUDIO_SHARING_MODE_EXCLUSIVE: i32 = 0;
const AAUDIO_SHARING_MODE_SHARED: i32 = 1;

const AAUDIO_PERFORMANCE_MODE_NONE: i32 = 10;
const AAUDIO_PERFORMANCE_MODE_LOW_LATENCY: i32 = 12;

#[repr(C)]
struct AAudioStreamBuilder {
    _private: [u8; 0],
}

#[repr(C)]
struct AAudioStream {
    _private: [u8; 0],
}

struct AAudioApi {
    create_stream_builder: unsafe extern "C" fn(*mut *mut AAudioStreamBuilder) -> i32,
    builder_delete: unsafe extern "C" fn(*mut AAudioStreamBuilder) -> i32,
    builder_set_direction: unsafe extern "C" fn(*mut AAudioStreamBuilder, i32),
    builder_set_sample_rate: unsafe extern "C" fn(*mut AAudioStreamBuilder, i32),
    builder_set_channel_count: unsafe extern "C" fn(*mut AAudioStreamBuilder, i32),
    builder_set_format: unsafe extern "C" fn(*mut AAudioStreamBuilder, i32),
    builder_set_sharing_mode: unsafe extern "C" fn(*mut AAudioStreamBuilder, i32),
    builder_set_performance_mode: unsafe extern "C" fn(*mut AAudioStreamBuilder, i32),
    builder_set_buffer_capacity_in_frames: unsafe extern "C" fn(*mut AAudioStreamBuilder, i32),
    builder_open_stream:
        unsafe extern "C" fn(*mut AAudioStreamBuilder, *mut *mut AAudioStream) -> i32,
    stream_close: unsafe extern "C" fn(*mut AAudioStream) -> i32,
    stream_request_start: unsafe extern "C" fn(*mut AAudioStream) -> i32,
    stream_request_pause: unsafe extern "C" fn(*mut AAudioStream) -> i32,
    stream_request_stop: unsafe extern "C" fn(*mut AAudioStream) -> i32,
    stream_request_flush: unsafe extern "C" fn(*mut AAudioStream) -> i32,
    stream_write: unsafe extern "C" fn(*mut AAudioStream, *const c_void, i32, i64) -> i32,
    stream_get_sample_rate: unsafe extern "C" fn(*mut AAudioStream) -> i32,
    stream_get_channel_count: unsafe extern "C" fn(*mut AAudioStream) -> i32,
    stream_get_format: unsafe extern "C" fn(*mut AAudioStream) -> i32,
    stream_get_sharing_mode: unsafe extern "C" fn(*mut AAudioStream) -> i32,
    convert_result_to_text: unsafe extern "C" fn(i32) -> *const c_char,
}

unsafe impl Send for AAudioApi {}
unsafe impl Sync for AAudioApi {}

static AAUDIO_API: OnceLock<std::result::Result<AAudioApi, String>> = OnceLock::new();

unsafe fn load_sym<T>(handle: *mut c_void, name: &[u8]) -> std::result::Result<T, String> {
    let sym = unsafe { libc::dlsym(handle, name.as_ptr().cast::<c_char>()) };
    if sym.is_null() {
        let name_str = std::str::from_utf8(&name[..name.len().saturating_sub(1)]).unwrap_or("?");
        return Err(format!("missing AAudio symbol '{name_str}'"));
    }
    Ok(unsafe { std::mem::transmute_copy(&sym) })
}

fn get_api() -> Result<&'static AAudioApi> {
    let res = AAUDIO_API.get_or_init(|| unsafe {
        let handle = libc::dlopen(c"libaaudio.so".as_ptr(), libc::RTLD_NOW);
        if handle.is_null() {
            return Err("Failed to load libaaudio.so (Android 8.0+ / API 26+ required)".to_owned());
        }
        Ok(AAudioApi {
            create_stream_builder: load_sym(handle, b"AAudio_createStreamBuilder\0")?,
            builder_delete: load_sym(handle, b"AAudioStreamBuilder_delete\0")?,
            builder_set_direction: load_sym(handle, b"AAudioStreamBuilder_setDirection\0")?,
            builder_set_sample_rate: load_sym(handle, b"AAudioStreamBuilder_setSampleRate\0")?,
            builder_set_channel_count: load_sym(handle, b"AAudioStreamBuilder_setChannelCount\0")?,
            builder_set_format: load_sym(handle, b"AAudioStreamBuilder_setFormat\0")?,
            builder_set_sharing_mode: load_sym(handle, b"AAudioStreamBuilder_setSharingMode\0")?,
            builder_set_performance_mode: load_sym(
                handle,
                b"AAudioStreamBuilder_setPerformanceMode\0",
            )?,
            builder_set_buffer_capacity_in_frames: load_sym(
                handle,
                b"AAudioStreamBuilder_setBufferCapacityInFrames\0",
            )?,
            builder_open_stream: load_sym(handle, b"AAudioStreamBuilder_openStream\0")?,
            stream_close: load_sym(handle, b"AAudioStream_close\0")?,
            stream_request_start: load_sym(handle, b"AAudioStream_requestStart\0")?,
            stream_request_pause: load_sym(handle, b"AAudioStream_requestPause\0")?,
            stream_request_stop: load_sym(handle, b"AAudioStream_requestStop\0")?,
            stream_request_flush: load_sym(handle, b"AAudioStream_requestFlush\0")?,
            stream_write: load_sym(handle, b"AAudioStream_write\0")?,
            stream_get_sample_rate: load_sym(handle, b"AAudioStream_getSampleRate\0")?,
            stream_get_channel_count: load_sym(handle, b"AAudioStream_getChannelCount\0")?,
            stream_get_format: load_sym(handle, b"AAudioStream_getFormat\0")?,
            stream_get_sharing_mode: load_sym(handle, b"AAudioStream_getSharingMode\0")?,
            convert_result_to_text: load_sym(handle, b"AAudio_convertResultToText\0")?,
        })
    });

    res.as_ref().map_err(|e| KiiaudError::Output(e.clone()))
}

fn result_text(api: &AAudioApi, code: i32) -> String {
    unsafe {
        let ptr = (api.convert_result_to_text)(code);
        if ptr.is_null() {
            format!("code {code}")
        } else {
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    }
}

fn to_aaudio_format(fmt: AndroidAudioFormat) -> i32 {
    match fmt {
        AndroidAudioFormat::I16 => AAUDIO_FORMAT_PCM_I16,
        AndroidAudioFormat::I24Packed => AAUDIO_FORMAT_PCM_I24_PACKED,
        AndroidAudioFormat::I24In32 | AndroidAudioFormat::I32 => AAUDIO_FORMAT_PCM_I32,
        AndroidAudioFormat::F32 => AAUDIO_FORMAT_PCM_FLOAT,
    }
}

fn from_aaudio_format(raw: i32) -> AndroidAudioFormat {
    match raw {
        AAUDIO_FORMAT_PCM_I16 => AndroidAudioFormat::I16,
        AAUDIO_FORMAT_PCM_I24_PACKED => AndroidAudioFormat::I24Packed,
        AAUDIO_FORMAT_PCM_I32 => AndroidAudioFormat::I32,
        _ => AndroidAudioFormat::F32,
    }
}

pub struct AAudioStreamHandle {
    api: &'static AAudioApi,
    stream: *mut AAudioStream,
    pub sample_rate: u32,
    pub channels: u16,
    pub hw_format: AndroidAudioFormat,
    pub exclusive: bool,
}

unsafe impl Send for AAudioStreamHandle {}

impl AAudioStreamHandle {
    pub fn open(
        sample_rate: u32,
        channels: u16,
        preferred_format: AndroidAudioFormat,
        prefer_exclusive: bool,
        buffer_time_ms: u32,
    ) -> Result<Self> {
        let api = get_api()?;

        let sharing_candidates: &[i32] = if prefer_exclusive {
            &[AAUDIO_SHARING_MODE_EXCLUSIVE, AAUDIO_SHARING_MODE_SHARED]
        } else {
            &[AAUDIO_SHARING_MODE_SHARED]
        };

        let format_candidates: Vec<AndroidAudioFormat> = match preferred_format {
            AndroidAudioFormat::I16 => vec![AndroidAudioFormat::I16, AndroidAudioFormat::F32],
            AndroidAudioFormat::I24Packed | AndroidAudioFormat::I24In32 => vec![
                AndroidAudioFormat::I24Packed,
                AndroidAudioFormat::I32,
                AndroidAudioFormat::F32,
                AndroidAudioFormat::I16,
            ],
            AndroidAudioFormat::I32 => vec![
                AndroidAudioFormat::I32,
                AndroidAudioFormat::F32,
                AndroidAudioFormat::I16,
            ],
            AndroidAudioFormat::F32 => vec![AndroidAudioFormat::F32, AndroidAudioFormat::I16],
        };

        let perf_mode = if buffer_time_ms >= 100 {
            AAUDIO_PERFORMANCE_MODE_NONE
        } else {
            AAUDIO_PERFORMANCE_MODE_LOW_LATENCY
        };

        let mut last_err = String::from("no AAudio configuration succeeded");

        for &sharing in sharing_candidates {
            for &fmt in &format_candidates {
                match Self::try_open(
                    api,
                    sample_rate,
                    channels,
                    fmt,
                    sharing,
                    perf_mode,
                    buffer_time_ms,
                ) {
                    Ok(handle) => return Ok(handle),
                    Err(e) => {
                        last_err = e;
                    }
                }
            }
        }

        Err(KiiaudError::Output(format!(
            "Failed to open AAudio stream ({}Hz {}ch): {last_err}",
            sample_rate, channels
        )))
    }

    fn try_open(
        api: &'static AAudioApi,
        sample_rate: u32,
        channels: u16,
        fmt: AndroidAudioFormat,
        sharing_mode: i32,
        perf_mode: i32,
        buffer_time_ms: u32,
    ) -> std::result::Result<Self, String> {
        unsafe {
            let mut builder: *mut AAudioStreamBuilder = ptr::null_mut();
            let rc = (api.create_stream_builder)(&mut builder);
            if rc != AAUDIO_OK || builder.is_null() {
                return Err(format!(
                    "AAudio_createStreamBuilder failed: {}",
                    result_text(api, rc)
                ));
            }

            (api.builder_set_direction)(builder, AAUDIO_DIRECTION_OUTPUT);
            (api.builder_set_sample_rate)(builder, sample_rate as i32);
            (api.builder_set_channel_count)(builder, channels as i32);
            (api.builder_set_format)(builder, to_aaudio_format(fmt));
            (api.builder_set_sharing_mode)(builder, sharing_mode);
            (api.builder_set_performance_mode)(builder, perf_mode);

            if buffer_time_ms > 0 {
                let cap_frames = ((sample_rate as u64 * buffer_time_ms as u64) / 1000)
                    .max(256)
                    .min(i32::MAX as u64) as i32;
                (api.builder_set_buffer_capacity_in_frames)(builder, cap_frames);
            }

            let mut stream: *mut AAudioStream = ptr::null_mut();
            let open_rc = (api.builder_open_stream)(builder, &mut stream);
            (api.builder_delete)(builder);

            if open_rc != AAUDIO_OK || stream.is_null() {
                return Err(format!(
                    "openStream(fmt={fmt:?}, sharing={sharing_mode}) failed: {}",
                    result_text(api, open_rc)
                ));
            }

            let start_rc = (api.stream_request_start)(stream);
            if start_rc != AAUDIO_OK {
                (api.stream_close)(stream);
                return Err(format!(
                    "requestStart failed: {}",
                    result_text(api, start_rc)
                ));
            }

            let actual_rate = (api.stream_get_sample_rate)(stream).max(1) as u32;
            let actual_channels = (api.stream_get_channel_count)(stream).max(1) as u16;
            let actual_fmt = from_aaudio_format((api.stream_get_format)(stream));
            let actual_sharing = (api.stream_get_sharing_mode)(stream);

            tracing::info!(
                requested_rate = sample_rate,
                actual_rate,
                actual_channels,
                actual_fmt = ?actual_fmt,
                exclusive = (actual_sharing == AAUDIO_SHARING_MODE_EXCLUSIVE),
                "AAudio output stream started"
            );

            Ok(Self {
                api,
                stream,
                sample_rate: actual_rate,
                channels: actual_channels,
                hw_format: actual_fmt,
                exclusive: actual_sharing == AAUDIO_SHARING_MODE_EXCLUSIVE,
            })
        }
    }

    /// Blocking write of packed PCM bytes to AAudio, paced by the hardware audio clock.
    pub fn write_all(&mut self, bytes: &[u8]) -> Result<usize> {
        let frame_bytes = self.hw_format.bytes_per_sample() * self.channels as usize;
        if frame_bytes == 0 || bytes.is_empty() {
            return Ok(0);
        }

        let total_frames = bytes.len() / frame_bytes;
        let mut written_frames = 0usize;
        let timeout_ns: i64 = 1_000_000_000; // 1 second per write call

        while written_frames < total_frames {
            let offset = written_frames * frame_bytes;
            let remaining = (total_frames - written_frames) as i32;
            let rc = unsafe {
                (self.api.stream_write)(
                    self.stream,
                    bytes[offset..].as_ptr().cast::<c_void>(),
                    remaining,
                    timeout_ns,
                )
            };
            if rc < 0 {
                return Err(KiiaudError::Output(format!(
                    "AAudioStream_write failed: {}",
                    result_text(self.api, rc)
                )));
            }
            if rc == 0 {
                return Err(KiiaudError::Output(
                    "AAudioStream_write timed out with 0 frames written".to_owned(),
                ));
            }
            written_frames += rc as usize;
        }

        Ok(written_frames)
    }

    pub fn pause(&mut self) -> Result<()> {
        let rc = unsafe { (self.api.stream_request_pause)(self.stream) };
        if rc != AAUDIO_OK {
            return Err(KiiaudError::Output(format!(
                "AAudioStream_requestPause failed: {}",
                result_text(self.api, rc)
            )));
        }
        Ok(())
    }

    pub fn resume(&mut self) -> Result<()> {
        let rc = unsafe { (self.api.stream_request_start)(self.stream) };
        if rc != AAUDIO_OK {
            return Err(KiiaudError::Output(format!(
                "AAudioStream_requestStart failed: {}",
                result_text(self.api, rc)
            )));
        }
        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        let rc = unsafe { (self.api.stream_request_flush)(self.stream) };
        if rc != AAUDIO_OK {
            return Err(KiiaudError::Output(format!(
                "AAudioStream_requestFlush failed: {}",
                result_text(self.api, rc)
            )));
        }
        Ok(())
    }
}

impl Drop for AAudioStreamHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = (self.api.stream_request_stop)(self.stream);
            let _ = (self.api.stream_close)(self.stream);
        }
    }
}
