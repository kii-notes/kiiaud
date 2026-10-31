// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bit-perfect ASIO audio output backend (`kiiaud-output-asio`) with Native DSD support.
//!
//! Features:
//! - Direct, hardware-level access bypassing Windows Audio Engine and WASAPI.
//! - Full support for Steinberg ASIO 2.3 and DSD-over-ASIO specifications.
//! - **Native 1-bit DSD Playback** (DSD64, DSD128, DSD256, DSD512, DSD1024) without DoP overhead.
//! - Automatic driver discovery and matching via Windows Registry (`HKLM\SOFTWARE\ASIO`).
//! - Bit-perfect PCM playback (16-bit, 24-bit, 32-bit integer, and 32-bit float).
//! - Double-buffered asynchronous real-time streaming with zero jitter.

pub mod driver;
pub mod sys;

pub use driver::{AsioDriverInfo, find_asio_driver, scan_asio_drivers};

use kiiaud_core::config::OutputConfig;
use kiiaud_core::{
    AudioBuffer, AudioFormat, BitOrder, ChannelLayout, KiiaudError, OutputCapabilities, Result,
};
#[cfg(target_os = "windows")]
use kiiaud_core::{DsdMode, DsdTransport};
use kiiaud_output::AudioOutput;
use kiiaud_output::dop::DopEncoder;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

#[cfg(target_os = "windows")]
use windows::{
    Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED, CoInitializeEx},
    Win32::System::Console::GetConsoleWindow,
    Win32::UI::WindowsAndMessaging::{GetDesktopWindow, GetForegroundWindow},
    core::GUID,
};

#[cfg(target_os = "windows")]
#[link(name = "ole32")]
unsafe extern "system" {
    fn CoCreateInstance(
        rclsid: *const windows::core::GUID,
        punkouter: *mut std::ffi::c_void,
        dwclscontext: u32,
        riid: *const windows::core::GUID,
        ppv: *mut *mut std::ffi::c_void,
    ) -> windows::core::HRESULT;
}

#[cfg(target_os = "windows")]
fn parse_guid(s: &str) -> std::result::Result<GUID, String> {
    let s = s.trim().trim_start_matches('{').trim_end_matches('}');
    if s.len() != 36 {
        return Err(format!("Invalid GUID length: {}", s.len()));
    }
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 5 {
        return Err("GUID does not have 5 parts".to_string());
    }
    let data1 = u32::from_str_radix(parts[0], 16).map_err(|e| e.to_string())?;
    let data2 = u16::from_str_radix(parts[1], 16).map_err(|e| e.to_string())?;
    let data3 = u16::from_str_radix(parts[2], 16).map_err(|e| e.to_string())?;
    if parts[3].len() != 4 || parts[4].len() != 12 {
        return Err("Invalid GUID part lengths".to_string());
    }
    let p3 = u16::from_str_radix(parts[3], 16).map_err(|e| e.to_string())?;
    let p4_0 = u16::from_str_radix(&parts[4][0..4], 16).map_err(|e| e.to_string())?;
    let p4_1 = u32::from_str_radix(&parts[4][4..12], 16).map_err(|e| e.to_string())?;

    let d4 = [
        (p3 >> 8) as u8,
        (p3 & 0xFF) as u8,
        (p4_0 >> 8) as u8,
        (p4_0 & 0xFF) as u8,
        (p4_1 >> 24) as u8,
        ((p4_1 >> 16) & 0xFF) as u8,
        ((p4_1 >> 8) & 0xFF) as u8,
        (p4_1 & 0xFF) as u8,
    ];

    Ok(GUID::from_values(data1, data2, data3, d4))
}

/// Internal hardware audio format configured on the ASIO driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsioHwFormat {
    /// 16-bit signed integer little-endian (`ASIOSTInt16LSB`).
    Int16LSB,
    /// Packed 24-bit signed integer little-endian (`ASIOSTInt24LSB`).
    Int24LSB,
    /// 32-bit signed integer little-endian (`ASIOSTInt32LSB`).
    Int32LSB,
    /// 32-bit signed integer with 24-bit data in LSB (`ASIOSTInt32LSB24`).
    Int32LSB24,
    /// 32-bit IEEE float little-endian (`ASIOSTFloat32LSB`).
    Float32LSB,
    /// Native 1-bit DSD with LSB played first (`ASIOSTDSDInt8LSB1`).
    DsdInt8LSB1,
    /// Native 1-bit DSD with MSB played first (`ASIOSTDSDInt8MSB1`).
    DsdInt8MSB1,
}

impl AsioHwFormat {
    pub fn is_dsd(self) -> bool {
        matches!(self, Self::DsdInt8LSB1 | Self::DsdInt8MSB1)
    }

    pub fn bytes_per_sample(self) -> usize {
        match self {
            Self::Int16LSB => 2,
            Self::Int24LSB => 3,
            Self::Int32LSB | Self::Int32LSB24 | Self::Float32LSB => 4,
            Self::DsdInt8LSB1 | Self::DsdInt8MSB1 => 1,
        }
    }
}

/// Map Steinberg ASIO sample type ID to [`AsioHwFormat`].
pub fn map_asio_sample_type(sample_type: sys::ASIOSampleType) -> AsioHwFormat {
    match sample_type {
        sys::ASIOSTInt16LSB => AsioHwFormat::Int16LSB,
        sys::ASIOSTInt24LSB => AsioHwFormat::Int24LSB,
        sys::ASIOSTInt32LSB => AsioHwFormat::Int32LSB,
        sys::ASIOSTInt32LSB24 => AsioHwFormat::Int32LSB24,
        sys::ASIOSTFloat32LSB => AsioHwFormat::Float32LSB,
        sys::ASIOSTDSDInt8LSB1 => AsioHwFormat::DsdInt8LSB1,
        sys::ASIOSTDSDInt8MSB1 => AsioHwFormat::DsdInt8MSB1,
        other => {
            tracing::warn!(
                sample_type = other,
                "Unknown or unhandled ASIO sample type, defaulting to Int32LSB"
            );
            AsioHwFormat::Int32LSB
        }
    }
}

/// Synchronized dual-channel FIFO audio queue feeding ASIO ping-pong buffer callbacks.
#[derive(Debug, Default)]
pub struct AsioRingBufferInner {
    pub left: VecDeque<u8>,
    pub right: VecDeque<u8>,
    pub is_dsd: bool,
}

/// Thread-safe ring buffer with single-lock stereo synchronization preventing inter-channel phase drift.
#[derive(Debug, Default)]
pub struct AsioRingBuffer {
    pub inner: Mutex<AsioRingBufferInner>,
}

impl AsioRingBuffer {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(AsioRingBufferInner {
                left: VecDeque::with_capacity(131072),
                right: VecDeque::with_capacity(131072),
                is_dsd: false,
            }),
        }
    }

    pub fn clear(&self) {
        let mut inner = self.inner.lock();
        inner.left.clear();
        inner.right.clear();
    }

    pub fn set_dsd(&self, dsd: bool) {
        self.inner.lock().is_dsd = dsd;
    }

    pub fn is_dsd(&self) -> bool {
        self.inner.lock().is_dsd
    }

    pub fn len_bytes(&self) -> usize {
        self.inner.lock().left.len()
    }

    pub fn push(&self, left_data: &[u8], right_data: &[u8]) {
        let mut inner = self.inner.lock();
        inner.left.extend(left_data);
        inner.right.extend(right_data);
    }
}

/// Global context pointer shared with Steinberg's static C callback functions.
#[allow(dead_code)]
struct AsioCallbackContext {
    ring_buffer: Arc<AsioRingBuffer>,
    buffer_infos: Vec<sys::ASIOBufferInfo>,
    buffer_size: usize,
    bytes_per_sample: usize,
}

unsafe impl Send for AsioCallbackContext {}
unsafe impl Sync for AsioCallbackContext {}

static ACTIVE_ASIO_CONTEXT: Mutex<Option<Arc<AsioCallbackContext>>> = Mutex::new(None);

#[cfg(target_os = "windows")]
unsafe extern "system" fn asio_buffer_switch_callback(
    double_buffer_index: i32,
    _direct: sys::ASIOBool,
) {
    let ctx_opt = ACTIVE_ASIO_CONTEXT.lock().clone();
    let Some(ctx) = ctx_opt else { return };

    let idx = if double_buffer_index == 0 { 0 } else { 1 };
    let req_bytes = ctx.buffer_size * ctx.bytes_per_sample;

    let mut inner = ctx.ring_buffer.inner.lock();
    let silence_byte = if inner.is_dsd { 0x69 } else { 0x00 };

    // Synchronized read: both Left and Right channels drain the exact same number of bytes
    let avail = inner.left.len().min(inner.right.len()).min(req_bytes);

    // Fill Channel 0 (Left)
    if let Some(info) = ctx.buffer_infos.first() {
        let dest = info.buffers[idx] as *mut u8;
        if !dest.is_null() {
            let mut copied = 0;
            let (s1, s2) = inner.left.as_slices();
            let c1 = s1.len().min(avail);
            if c1 > 0 {
                unsafe { std::ptr::copy_nonoverlapping(s1.as_ptr(), dest, c1) };
                copied += c1;
            }
            let c2 = s2.len().min(avail - copied);
            if c2 > 0 {
                unsafe { std::ptr::copy_nonoverlapping(s2.as_ptr(), dest.add(copied), c2) };
                copied += c2;
            }
            inner.left.drain(..copied);
            if copied < req_bytes {
                unsafe { std::ptr::write_bytes(dest.add(copied), silence_byte, req_bytes - copied) };
            }
        }
    }

    // Fill Channel 1 (Right)
    if ctx.buffer_infos.len() > 1 {
        let info = &ctx.buffer_infos[1];
        let dest = info.buffers[idx] as *mut u8;
        if !dest.is_null() {
            let mut copied = 0;
            let (s1, s2) = inner.right.as_slices();
            let c1 = s1.len().min(avail);
            if c1 > 0 {
                unsafe { std::ptr::copy_nonoverlapping(s1.as_ptr(), dest, c1) };
                copied += c1;
            }
            let c2 = s2.len().min(avail - copied);
            if c2 > 0 {
                unsafe { std::ptr::copy_nonoverlapping(s2.as_ptr(), dest.add(copied), c2) };
                copied += c2;
            }
            inner.right.drain(..copied);
            if copied < req_bytes {
                unsafe { std::ptr::write_bytes(dest.add(copied), silence_byte, req_bytes - copied) };
            }
        }
    }
}

#[cfg(target_os = "windows")]
unsafe extern "system" fn asio_sample_rate_changed(_srate: f64) {}

#[cfg(target_os = "windows")]
unsafe extern "system" fn asio_message(
    _sel: i32,
    _val: i32,
    _msg: *mut std::ffi::c_void,
    _opt: *mut f64,
) -> i32 {
    0
}

#[cfg(target_os = "windows")]
unsafe extern "system" fn asio_buffer_switch_time_info(
    _params: *mut std::ffi::c_void,
    double_buffer_index: i32,
    direct: sys::ASIOBool,
) -> *mut std::ffi::c_void {
    unsafe {
        asio_buffer_switch_callback(double_buffer_index, direct);
    }
    std::ptr::null_mut()
}

/// Bit-perfect ASIO Audio Output Driver.
pub struct AsioAudioOutput {
    config: OutputConfig,
    active_format: Option<AudioFormat>,
    hw_format: Option<AsioHwFormat>,
    is_playing: bool,
    ring_buffer: Arc<AsioRingBuffer>,
    dop_encoder: Option<DopEncoder>,
    dop_scratch: Vec<i32>,
    left_scratch: Vec<u8>,
    right_scratch: Vec<u8>,
    selected_driver: Option<AsioDriverInfo>,
    #[cfg(target_os = "windows")]
    asio_driver: Option<*mut sys::IASIO>,
    #[cfg(target_os = "windows")]
    buffer_infos: Vec<sys::ASIOBufferInfo>,
    buffer_size: usize,
}

// Safe because ASIO output engine runs exclusively on the dedicated playback worker thread
unsafe impl Send for AsioAudioOutput {}

impl AsioAudioOutput {
    pub fn new(config: OutputConfig) -> Self {
        Self {
            config,
            active_format: None,
            hw_format: None,
            is_playing: false,
            ring_buffer: Arc::new(AsioRingBuffer::new()),
            dop_encoder: None,
            dop_scratch: Vec::new(),
            left_scratch: Vec::new(),
            right_scratch: Vec::new(),
            selected_driver: None,
            #[cfg(target_os = "windows")]
            asio_driver: None,
            #[cfg(target_os = "windows")]
            buffer_infos: Vec::new(),
            buffer_size: 2048,
        }
    }

    /// Access ring buffer for testing.
    pub fn ring_buffer(&self) -> Arc<AsioRingBuffer> {
        Arc::clone(&self.ring_buffer)
    }
}

#[cfg(target_os = "windows")]
impl AsioAudioOutput {
    fn initialize_driver(&mut self) -> Result<*mut sys::IASIO> {
        if let Some(drv) = self.asio_driver {
            return Ok(drv);
        }

        let driver_info = find_asio_driver(&self.config.device).ok_or_else(|| {
            KiiaudError::Output(format!(
                "No ASIO driver matching '{}' found in Windows Registry",
                self.config.device
            ))
        })?;

        tracing::info!(
            driver = %driver_info.name,
            clsid = %driver_info.clsid,
            "Loading ASIO driver via Windows COM"
        );

        unsafe {
            // ASIO drivers require Single-Threaded Apartment (STA) COM threading for internal
            // UI message pumps, event dispatchers, and device notifications. Attempt STA first,
            // falling back to MTA if already initialized on this thread.
            let coinit_res = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            if coinit_res.is_err() {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            let clsid = parse_guid(&driver_info.clsid).map_err(|e| {
                KiiaudError::Output(format!(
                    "Invalid CLSID GUID '{}' for ASIO driver '{}': {e}",
                    driver_info.clsid, driver_info.name
                ))
            })?;

            let mut instance: *mut std::ffi::c_void = std::ptr::null_mut();
            let mut hr = CoCreateInstance(
                &clsid,
                std::ptr::null_mut(),
                1, /* CLSCTX_INPROC_SERVER */
                &clsid,
                &mut instance,
            );
            if hr.is_err() || instance.is_null() {
                // Fallback to IUnknown interface ID: {00000000-0000-0000-C000-000000000046}
                const IID_IUNKNOWN: GUID = GUID::from_values(
                    0x00000000,
                    0x0000,
                    0x0000,
                    [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
                );
                hr = CoCreateInstance(
                    &clsid,
                    std::ptr::null_mut(),
                    1,
                    &IID_IUNKNOWN,
                    &mut instance,
                );
            }

            if hr.is_err() || instance.is_null() {
                return Err(KiiaudError::Output(format!(
                    "Failed to create COM instance for ASIO driver '{}' (hr=0x{:08X})",
                    driver_info.name, hr.0 as u32
                )));
            }

            let asio_ptr = instance as *mut sys::IASIO;
            let vtbl = &*(*asio_ptr).lpVtbl;

            // Steinberg ASIO SDK specification: sysHandle on Windows must be a valid application HWND
            // (e.g. active/foreground window, console window, or desktop window). Many drivers (such
            // as Universal Audio / SXW UAD ASIO) fail IASIO::init() if passed NULL / 0.
            let sys_handle: *mut std::ffi::c_void = {
                let fg = GetForegroundWindow();
                if fg.0 as usize != 0 {
                    fg.0 as *mut std::ffi::c_void
                } else {
                    let con = GetConsoleWindow();
                    if con.0 as usize != 0 {
                        con.0 as *mut std::ffi::c_void
                    } else {
                        let dt = GetDesktopWindow();
                        dt.0 as *mut std::ffi::c_void
                    }
                }
            };

            let mut init_ok = (vtbl.init)(asio_ptr as *mut std::ffi::c_void, sys_handle);
            if init_ok != sys::ASIO_TRUE && !sys_handle.is_null() {
                tracing::debug!("IASIO::init with window handle failed, retrying with NULL sysHandle");
                init_ok = (vtbl.init)(asio_ptr as *mut std::ffi::c_void, std::ptr::null_mut());
            }

            if init_ok != sys::ASIO_TRUE {
                let mut err_buf = [0u8; 512];
                (vtbl.getErrorMessage)(asio_ptr as *mut std::ffi::c_void, err_buf.as_mut_ptr());
                let err_detail = String::from_utf8_lossy(&err_buf)
                    .trim_matches(char::from(0))
                    .trim()
                    .to_string();

                let _ = (vtbl.Release)(asio_ptr as *mut std::ffi::c_void);
                return Err(KiiaudError::Output(if err_detail.is_empty() {
                    format!("ASIO driver '{}' failed IASIO::init()", driver_info.name)
                } else {
                    format!(
                        "ASIO driver '{}' failed IASIO::init(): {}",
                        driver_info.name, err_detail
                    )
                }));
            }

            self.selected_driver = Some(driver_info);
            self.asio_driver = Some(asio_ptr);
            Ok(asio_ptr)
        }
    }
}

impl AudioOutput for AsioAudioOutput {
    #[allow(unused_variables)]
    fn open(&mut self, format: &AudioFormat) -> Result<AudioFormat> {
        self.stop()?;

        #[cfg(target_os = "windows")]
        unsafe {
            let asio = self.initialize_driver()?;
            let vtbl = &*(*asio).lpVtbl;

            let (negotiated, hw_fmt) = match *format {
                AudioFormat::Pcm {
                    sample_rate,
                    channels,
                    bits_per_sample,
                    sample_format,
                } => {
                    // Reset IO format to standard PCM first in case previous playback was DSD
                    let mut io_format = sys::ASIOIoFormat {
                        FormatType: sys::kAsioFormatPCM,
                        ..Default::default()
                    };
                    let _ = (vtbl.future)(
                        asio as *mut std::ffi::c_void,
                        sys::kAsioSetIoFormat,
                        &mut io_format as *mut _ as *mut std::ffi::c_void,
                    );

                    // Configure PCM sample rate
                    let rate_f64 = sample_rate as f64;
                    let can_res = (vtbl.canSampleRate)(asio as *mut std::ffi::c_void, rate_f64);
                    if can_res != sys::ASE_OK && can_res != sys::ASE_SUCCESS {
                        // Some drivers return an error for canSampleRate, but setSampleRate succeeds
                        let set_res = (vtbl.setSampleRate)(asio as *mut std::ffi::c_void, rate_f64);
                        if set_res != sys::ASE_OK && set_res != sys::ASE_SUCCESS {
                            return Err(KiiaudError::UnsupportedFormat(format!(
                                "ASIO driver does not support PCM sample rate {sample_rate} Hz (canSampleRate: {can_res}, setSampleRate: {set_res})"
                            )));
                        }
                    } else {
                        let set_res = (vtbl.setSampleRate)(asio as *mut std::ffi::c_void, rate_f64);
                        if set_res != sys::ASE_OK && set_res != sys::ASE_SUCCESS {
                            tracing::warn!(
                                rate = sample_rate,
                                "ASIO setSampleRate returned {set_res}, proceeding"
                            );
                        }
                    }

                    // Query native hardware channel format from ASIO driver
                    let mut ch_info = sys::ASIOChannelInfo {
                        channel: 0,
                        isInput: 0,
                        ..Default::default()
                    };
                    let hw = if (vtbl.getChannelInfo)(
                        asio as *mut std::ffi::c_void,
                        &mut ch_info,
                    ) == sys::ASE_OK
                    {
                        let mapped = map_asio_sample_type(ch_info.sampleType);
                        if mapped == AsioHwFormat::DsdInt8LSB1 || mapped == AsioHwFormat::DsdInt8MSB1 {
                            tracing::warn!(
                                sample_type = ch_info.sampleType,
                                "ASIO driver reported DSD format for PCM stream, forcing fallback to Int32LSB"
                            );
                            AsioHwFormat::Int32LSB
                        } else {
                            mapped
                        }
                    } else {
                        AsioHwFormat::Int32LSB
                    };

                    self.dop_encoder = None;
                    self.ring_buffer.set_dsd(false);

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
                    let try_native =
                        matches!(self.config.dsd_mode, DsdMode::Auto | DsdMode::Native);
                    let mut native_success = false;
                    let mut chosen_hw = AsioHwFormat::DsdInt8LSB1;

                    if try_native {
                        // 1. Attempt Native DSD format switch via ASIOFuture
                        let mut io_format = sys::ASIOIoFormat {
                            FormatType: sys::kAsioFormatDSD,
                            ..Default::default()
                        };

                        let can_dsd = (vtbl.future)(
                            asio as *mut std::ffi::c_void,
                            sys::kAsioCanDoIoFormat,
                            &mut io_format as *mut _ as *mut std::ffi::c_void,
                        );

                        let set_dsd = if can_dsd == sys::ASE_SUCCESS || can_dsd == sys::ASE_OK {
                            (vtbl.future)(
                                asio as *mut std::ffi::c_void,
                                sys::kAsioSetIoFormat,
                                &mut io_format as *mut _ as *mut std::ffi::c_void,
                            )
                        } else {
                            // Drivers that don't implement canDoIoFormat might still accept setIoFormat
                            (vtbl.future)(
                                asio as *mut std::ffi::c_void,
                                sys::kAsioSetIoFormat,
                                &mut io_format as *mut _ as *mut std::ffi::c_void,
                            )
                        };

                        if set_dsd == sys::ASE_SUCCESS || set_dsd == sys::ASE_OK {
                            // Set DSD sample rate
                            let dsd_hz = rate.hz() as f64;
                            let _ = (vtbl.setSampleRate)(asio as *mut std::ffi::c_void, dsd_hz);
                            native_success = true;

                            // Query channel sample type
                            let mut ch_info = sys::ASIOChannelInfo {
                                channel: 0,
                                isInput: 0,
                                ..Default::default()
                            };
                            if (vtbl.getChannelInfo)(
                                asio as *mut std::ffi::c_void,
                                &mut ch_info,
                            ) == sys::ASE_OK
                            {
                                if ch_info.sampleType == sys::ASIOSTDSDInt8MSB1 {
                                    chosen_hw = AsioHwFormat::DsdInt8MSB1;
                                } else if ch_info.sampleType == sys::ASIOSTDSDInt8LSB1 {
                                    chosen_hw = AsioHwFormat::DsdInt8LSB1;
                                }
                            }
                        }
                    }

                    if native_success {
                        tracing::info!(
                            rate = %rate,
                            hw = ?chosen_hw,
                            "ASIO Native DSD negotiated successfully"
                        );
                        self.dop_encoder = None;
                        self.ring_buffer.set_dsd(true);

                        (
                            AudioFormat::Dsd {
                                rate,
                                channels,
                                transport: DsdTransport::Native,
                                bit_order,
                                layout,
                            },
                            chosen_hw,
                        )
                    } else if self.config.dsd_mode == DsdMode::Native {
                        return Err(KiiaudError::UnsupportedFormat(format!(
                            "ASIO driver does not support Native DSD for {rate}"
                        )));
                    } else {
                        // Fall back to DoP (DSD over PCM)
                        let mut io_format = sys::ASIOIoFormat {
                            FormatType: sys::kAsioFormatPCM,
                            ..Default::default()
                        };
                        let _ = (vtbl.future)(
                            asio as *mut std::ffi::c_void,
                            sys::kAsioSetIoFormat,
                            &mut io_format as *mut _ as *mut std::ffi::c_void,
                        );

                        let dop_rate = rate.dop_pcm_rate_hz();
                        let _ =
                            (vtbl.setSampleRate)(asio as *mut std::ffi::c_void, dop_rate as f64);
                        let enc = DopEncoder::new(rate, channels as usize, layout, bit_order)?;
                        self.dop_encoder = Some(enc);
                        self.ring_buffer.set_dsd(false);

                        // Query channel info for DoP PCM container format
                        let mut ch_info = sys::ASIOChannelInfo {
                            channel: 0,
                            isInput: 0,
                            ..Default::default()
                        };
                        let dop_hw = if (vtbl.getChannelInfo)(
                            asio as *mut std::ffi::c_void,
                            &mut ch_info,
                        ) == sys::ASE_OK
                        {
                            map_asio_sample_type(ch_info.sampleType)
                        } else {
                            AsioHwFormat::Int32LSB24
                        };

                        (
                            AudioFormat::Dsd {
                                rate,
                                channels,
                                transport: DsdTransport::DoP,
                                bit_order,
                                layout,
                            },
                            dop_hw,
                        )
                    }
                }
            };

            // Query buffer size requirements
            let mut min_sz = 0;
            let mut max_sz = 0;
            let mut pref_sz = 0;
            let mut gran = 0;
            let _ = (vtbl.getBufferSize)(
                asio as *mut std::ffi::c_void,
                &mut min_sz,
                &mut max_sz,
                &mut pref_sz,
                &mut gran,
            );

            let buffer_sz = if pref_sz > 0 { pref_sz as usize } else { 2048 };
            self.buffer_size = buffer_sz;

            // Prepare buffer infos for stereo channels
            let mut buffer_infos = vec![
                sys::ASIOBufferInfo {
                    isInput: 0,
                    channelNum: 0,
                    buffers: [std::ptr::null_mut(), std::ptr::null_mut()],
                },
                sys::ASIOBufferInfo {
                    isInput: 0,
                    channelNum: 1,
                    buffers: [std::ptr::null_mut(), std::ptr::null_mut()],
                },
            ];

            let mut callbacks = sys::ASIOCallbacks {
                bufferSwitch: Some(asio_buffer_switch_callback),
                sampleRateDidChange: Some(asio_sample_rate_changed),
                asioMessage: Some(asio_message),
                bufferSwitchTimeInfo: Some(asio_buffer_switch_time_info),
            };

            let create_res = (vtbl.createBuffers)(
                asio as *mut std::ffi::c_void,
                buffer_infos.as_mut_ptr(),
                buffer_infos.len() as i32,
                buffer_sz as i32,
                &mut callbacks,
            );

            if create_res != sys::ASE_OK {
                return Err(KiiaudError::Output(format!(
                    "ASIO createBuffers failed with code {create_res}"
                )));
            }

            self.buffer_infos = buffer_infos.clone();

            // Post-buffer creation channel format verification
            let mut final_hw = hw_fmt;
            let mut post_info = sys::ASIOChannelInfo {
                channel: 0,
                isInput: 0,
                ..Default::default()
            };
            if (vtbl.getChannelInfo)(asio as *mut std::ffi::c_void, &mut post_info) == sys::ASE_OK {
                let mapped = map_asio_sample_type(post_info.sampleType);
                if matches!(negotiated, AudioFormat::Pcm { .. })
                    && (mapped == AsioHwFormat::DsdInt8LSB1 || mapped == AsioHwFormat::DsdInt8MSB1)
                {
                    tracing::warn!(
                        sample_type = post_info.sampleType,
                        "Post-buffer ASIO driver returned DSD sample type for PCM playback, maintaining PCM hardware format {:?}",
                        hw_fmt
                    );
                } else {
                    final_hw = mapped;
                }
            }

            tracing::info!(
                driver_sample_type = post_info.sampleType,
                hw_format = ?final_hw,
                buffer_size = buffer_sz,
                "ASIO hardware stream configured"
            );

            // Set global callback context
            *ACTIVE_ASIO_CONTEXT.lock() = Some(Arc::new(AsioCallbackContext {
                ring_buffer: Arc::clone(&self.ring_buffer),
                buffer_infos,
                buffer_size: buffer_sz,
                bytes_per_sample: final_hw.bytes_per_sample(),
            }));

            // Start hardware streaming
            let start_res = (vtbl.start)(asio as *mut std::ffi::c_void);
            if start_res != sys::ASE_OK {
                return Err(KiiaudError::Output(format!(
                    "ASIO start failed with code {start_res}"
                )));
            }

            self.hw_format = Some(final_hw);
            self.active_format = Some(negotiated);
            self.is_playing = true;
            self.ring_buffer.clear();

            Ok(negotiated)
        }

        #[cfg(not(target_os = "windows"))]
        {
            Err(KiiaudError::Output(
                "ASIO output backend is only supported on Windows".to_owned(),
            ))
        }
    }

    fn write(&mut self, buffer: &AudioBuffer) -> Result<usize> {
        let Some(hw_fmt) = self.hw_format else {
            return Err(KiiaudError::Output(
                "ASIO audio output is not open".to_owned(),
            ));
        };

        self.left_scratch.clear();
        self.right_scratch.clear();

        match buffer {
            AudioBuffer::PcmI32 {
                samples, channels, ..
            } => {
                pack_pcm_i32_to_asio(
                    samples,
                    *channels as usize,
                    hw_fmt,
                    &mut self.left_scratch,
                    &mut self.right_scratch,
                )?;
            }
            AudioBuffer::PcmF32 {
                samples, channels, ..
            } => {
                pack_pcm_f32_to_asio(
                    samples,
                    *channels as usize,
                    hw_fmt,
                    &mut self.left_scratch,
                    &mut self.right_scratch,
                )?;
            }
            AudioBuffer::Dsd {
                data,
                channels,
                bit_order,
                layout,
                ..
            } => {
                if let Some(encoder) = self.dop_encoder.as_mut() {
                    encoder.encode(data, &mut self.dop_scratch);
                    pack_pcm_i32_to_asio(
                        &self.dop_scratch,
                        *channels as usize,
                        hw_fmt,
                        &mut self.left_scratch,
                        &mut self.right_scratch,
                    )?;
                } else {
                    // Bit-perfect Native DSD splitting
                    pack_native_dsd_to_asio(
                        data,
                        *channels as usize,
                        *layout,
                        *bit_order,
                        hw_fmt,
                        &mut self.left_scratch,
                        &mut self.right_scratch,
                    )?;
                }
            }
        }

        // Push into synchronized stereo ring buffer
        self.ring_buffer.push(&self.left_scratch, &self.right_scratch);

        // Buffer flow control: pace decoding with DAC playback
        while self.is_playing
            && self.ring_buffer.len_bytes() > (self.buffer_size * hw_fmt.bytes_per_sample() * 16)
        {
            std::thread::sleep(Duration::from_millis(5));
        }

        Ok(buffer.frames())
    }

    fn pause(&mut self) -> Result<()> {
        self.is_playing = false;
        #[cfg(target_os = "windows")]
        if let Some(asio) = self.asio_driver {
            unsafe {
                let vtbl = &*(*asio).lpVtbl;
                let _ = (vtbl.stop)(asio as *mut std::ffi::c_void);
            }
        }
        Ok(())
    }

    fn resume(&mut self) -> Result<()> {
        #[cfg(target_os = "windows")]
        if let Some(asio) = self.asio_driver {
            unsafe {
                let vtbl = &*(*asio).lpVtbl;
                let _ = (vtbl.start)(asio as *mut std::ffi::c_void);
            }
        }
        self.is_playing = true;
        Ok(())
    }

    fn drain(&mut self) -> Result<()> {
        let max_wait = Duration::from_millis(1500);
        let start = std::time::Instant::now();
        while self.ring_buffer.len_bytes() > 0 && start.elapsed() < max_wait {
            std::thread::sleep(Duration::from_millis(10));
        }
        self.ring_buffer.clear();
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        #[cfg(target_os = "windows")]
        if let Some(asio) = self.asio_driver {
            unsafe {
                let vtbl = &*(*asio).lpVtbl;
                let _ = (vtbl.stop)(asio as *mut std::ffi::c_void);
                let _ = (vtbl.disposeBuffers)(asio as *mut std::ffi::c_void);

                // Reset IO format to standard PCM so device clock isn't left locked in Native DSD
                let mut io_format = sys::ASIOIoFormat {
                    FormatType: sys::kAsioFormatPCM,
                    ..Default::default()
                };
                let _ = (vtbl.future)(
                    asio as *mut std::ffi::c_void,
                    sys::kAsioSetIoFormat,
                    &mut io_format as *mut _ as *mut std::ffi::c_void,
                );
            }
        }
        *ACTIVE_ASIO_CONTEXT.lock() = None;
        self.ring_buffer.clear();
        self.is_playing = false;
        self.hw_format = None;
        self.active_format = None;
        self.dop_encoder = None;
        Ok(())
    }

    fn capabilities(&self) -> OutputCapabilities {
        let name = self
            .selected_driver
            .as_ref()
            .map(|d| d.name.clone())
            .unwrap_or_else(|| self.config.device.clone());

        OutputCapabilities {
            backend_name: "ASIO (Audio Stream Input/Output)".to_owned(),
            device_name: if name.is_empty() || name == "default" {
                "ASIO Audio Device".to_owned()
            } else {
                name
            },
            sample_rates: vec![
                44100, 48000, 88200, 96000, 176400, 192000, 352800, 384000, 705600, 768000,
            ],
            unsupported_rates: Vec::new(),
            bit_depths: vec![16, 24, 32],
            channels: 2,
            exclusive: true,
            bit_perfect: true,
            adaptive_fallback: false,
            dsd_native: true, // Native DSD supported via ASIO
            dsd_dop: true,
        }
    }
}

/// Pack interleaved `PcmI32` samples into deinterleaved Left and Right channel buffers.
pub fn pack_pcm_i32_to_asio(
    samples: &[i32],
    channels: usize,
    hw_fmt: AsioHwFormat,
    left: &mut Vec<u8>,
    right: &mut Vec<u8>,
) -> Result<()> {
    let ch = channels.max(1);
    let frames = samples.len() / ch;
    let b_per_s = hw_fmt.bytes_per_sample();
    left.reserve(frames * b_per_s);
    right.reserve(frames * b_per_s);

    for chunk in samples.chunks(ch) {
        let l_sample = chunk[0];
        let r_sample = if chunk.len() > 1 { chunk[1] } else { l_sample };

        match hw_fmt {
            AsioHwFormat::Int16LSB => {
                let l_s16 = (l_sample >> 16) as i16;
                let r_s16 = (r_sample >> 16) as i16;
                left.extend_from_slice(&l_s16.to_le_bytes());
                right.extend_from_slice(&r_s16.to_le_bytes());
            }
            AsioHwFormat::Int24LSB => {
                let l_bytes = (l_sample >> 8).to_le_bytes();
                let r_bytes = (r_sample >> 8).to_le_bytes();
                left.extend_from_slice(&l_bytes[0..3]);
                right.extend_from_slice(&r_bytes[0..3]);
            }
            AsioHwFormat::Int32LSB => {
                left.extend_from_slice(&l_sample.to_le_bytes());
                right.extend_from_slice(&r_sample.to_le_bytes());
            }
            AsioHwFormat::Int32LSB24 => {
                let l_s24 = l_sample >> 8;
                let r_s24 = r_sample >> 8;
                left.extend_from_slice(&l_s24.to_le_bytes());
                right.extend_from_slice(&r_s24.to_le_bytes());
            }
            AsioHwFormat::Float32LSB => {
                let l_f = (l_sample as f32) / 2_147_483_648.0;
                let r_f = (r_sample as f32) / 2_147_483_648.0;
                left.extend_from_slice(&l_f.to_le_bytes());
                right.extend_from_slice(&r_f.to_le_bytes());
            }
            AsioHwFormat::DsdInt8LSB1 | AsioHwFormat::DsdInt8MSB1 => {
                return Err(KiiaudError::Output(
                    "Attempted to pack PCM samples into Native DSD format".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

/// Pack interleaved `PcmF32` samples into deinterleaved Left and Right channel buffers.
pub fn pack_pcm_f32_to_asio(
    samples: &[f32],
    channels: usize,
    hw_fmt: AsioHwFormat,
    left: &mut Vec<u8>,
    right: &mut Vec<u8>,
) -> Result<()> {
    let ch = channels.max(1);
    let frames = samples.len() / ch;
    let b_per_s = hw_fmt.bytes_per_sample();
    left.reserve(frames * b_per_s);
    right.reserve(frames * b_per_s);

    for chunk in samples.chunks(ch) {
        let l_sample = chunk[0];
        let r_sample = if chunk.len() > 1 { chunk[1] } else { l_sample };

        match hw_fmt {
            AsioHwFormat::Float32LSB => {
                left.extend_from_slice(&l_sample.to_le_bytes());
                right.extend_from_slice(&r_sample.to_le_bytes());
            }
            AsioHwFormat::Int32LSB => {
                let l_i = (l_sample.clamp(-1.0, 1.0) * 2_147_483_647.0) as i32;
                let r_i = (r_sample.clamp(-1.0, 1.0) * 2_147_483_647.0) as i32;
                left.extend_from_slice(&l_i.to_le_bytes());
                right.extend_from_slice(&r_i.to_le_bytes());
            }
            AsioHwFormat::Int32LSB24 => {
                let l_i = (l_sample.clamp(-1.0, 1.0) * 8_388_607.0) as i32;
                let r_i = (r_sample.clamp(-1.0, 1.0) * 8_388_607.0) as i32;
                left.extend_from_slice(&l_i.to_le_bytes());
                right.extend_from_slice(&r_i.to_le_bytes());
            }
            AsioHwFormat::Int16LSB => {
                let l_i = (l_sample.clamp(-1.0, 1.0) * 32_767.0) as i16;
                let r_i = (r_sample.clamp(-1.0, 1.0) * 32_767.0) as i16;
                left.extend_from_slice(&l_i.to_le_bytes());
                right.extend_from_slice(&r_i.to_le_bytes());
            }
            AsioHwFormat::Int24LSB => {
                let l_i = (l_sample.clamp(-1.0, 1.0) * 8_388_607.0) as i32;
                let r_i = (r_sample.clamp(-1.0, 1.0) * 8_388_607.0) as i32;
                left.extend_from_slice(&l_i.to_le_bytes()[0..3]);
                right.extend_from_slice(&r_i.to_le_bytes()[0..3]);
            }
            AsioHwFormat::DsdInt8LSB1 | AsioHwFormat::DsdInt8MSB1 => {
                return Err(KiiaudError::Output(
                    "Attempted to pack PCM float samples into Native DSD format".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

/// Pack raw DSD bitstream bytes into deinterleaved Left and Right channel buffers for Native DSD ASIO output.
pub fn pack_native_dsd_to_asio(
    data: &[u8],
    channels: usize,
    layout: ChannelLayout,
    bit_order: BitOrder,
    hw_fmt: AsioHwFormat,
    left: &mut Vec<u8>,
    right: &mut Vec<u8>,
) -> Result<()> {
    let reverse_bits = match (bit_order, hw_fmt) {
        (BitOrder::LsbFirst, AsioHwFormat::DsdInt8MSB1) => true,
        (BitOrder::MsbFirst, AsioHwFormat::DsdInt8LSB1) => true,
        _ => false,
    };

    let ch = channels.max(1);

    match layout {
        ChannelLayout::Planar => {
            let per_channel = data.len() / ch;
            let l_slice = &data[0..per_channel];
            let r_slice = if ch > 1 {
                &data[per_channel..per_channel * 2]
            } else {
                l_slice
            };

            if reverse_bits {
                left.extend(l_slice.iter().map(|b| b.reverse_bits()));
                right.extend(r_slice.iter().map(|b| b.reverse_bits()));
            } else {
                left.extend_from_slice(l_slice);
                right.extend_from_slice(r_slice);
            }
        }
        ChannelLayout::Interleaved => {
            for chunk in data.chunks(ch) {
                let l = chunk[0];
                let r = if chunk.len() > 1 { chunk[1] } else { l };

                if reverse_bits {
                    left.push(l.reverse_bits());
                    right.push(r.reverse_bits());
                } else {
                    left.push(l);
                    right.push(r);
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pack_pcm_i32_s16le() {
        // Interleaved L/R 16-bit MSB-aligned in i32
        let samples = vec![0x1234_0000, 0x5678_0000];
        let mut left = Vec::new();
        let mut right = Vec::new();
        pack_pcm_i32_to_asio(&samples, 2, AsioHwFormat::Int16LSB, &mut left, &mut right).unwrap();

        assert_eq!(left, vec![0x34, 0x12]);
        assert_eq!(right, vec![0x78, 0x56]);
    }

    #[test]
    fn test_pack_pcm_i32_s24_3le() {
        // Interleaved L/R 24-bit MSB-aligned in i32 (0x12345600)
        let samples = vec![0x1234_5600, 0x789A_BC00];
        let mut left = Vec::new();
        let mut right = Vec::new();
        pack_pcm_i32_to_asio(&samples, 2, AsioHwFormat::Int24LSB, &mut left, &mut right).unwrap();

        assert_eq!(left, vec![0x56, 0x34, 0x12]);
        assert_eq!(right, vec![0xBC, 0x9A, 0x78]);
    }

    #[test]
    fn test_pack_pcm_i32_s32le() {
        let samples = vec![0x1234_5678, 0x1122_3344];
        let mut left = Vec::new();
        let mut right = Vec::new();
        pack_pcm_i32_to_asio(&samples, 2, AsioHwFormat::Int32LSB, &mut left, &mut right).unwrap();

        assert_eq!(left, vec![0x78, 0x56, 0x34, 0x12]);
        assert_eq!(right, vec![0x44, 0x33, 0x22, 0x11]);
    }

    #[test]
    fn test_pack_pcm_i32_s32lsb24() {
        // Samples MSB-aligned in i32: 0x1234_5600 -> shifted >> 8 becomes 0x0012_3456
        let samples = vec![0x1234_5600, 0x789A_BC00];
        let mut left = Vec::new();
        let mut right = Vec::new();
        pack_pcm_i32_to_asio(&samples, 2, AsioHwFormat::Int32LSB24, &mut left, &mut right).unwrap();

        assert_eq!(left, vec![0x56, 0x34, 0x12, 0x00]);
        assert_eq!(right, vec![0xBC, 0x9A, 0x78, 0x00]);
    }

    #[test]
    fn test_map_asio_sample_types() {
        assert_eq!(map_asio_sample_type(sys::ASIOSTInt16LSB), AsioHwFormat::Int16LSB);
        assert_eq!(map_asio_sample_type(sys::ASIOSTInt24LSB), AsioHwFormat::Int24LSB);
        assert_eq!(map_asio_sample_type(sys::ASIOSTInt32LSB), AsioHwFormat::Int32LSB);
        assert_eq!(map_asio_sample_type(sys::ASIOSTInt32LSB24), AsioHwFormat::Int32LSB24);
        assert_eq!(map_asio_sample_type(sys::ASIOSTFloat32LSB), AsioHwFormat::Float32LSB);
        assert_eq!(map_asio_sample_type(sys::ASIOSTDSDInt8LSB1), AsioHwFormat::DsdInt8LSB1);
        assert_eq!(map_asio_sample_type(sys::ASIOSTDSDInt8MSB1), AsioHwFormat::DsdInt8MSB1);
    }

    #[test]
    fn test_ring_buffer_push_clear() {
        let rb = AsioRingBuffer::new();
        rb.push(&[1, 2, 3], &[4, 5, 6]);
        assert_eq!(rb.len_bytes(), 3);
        rb.clear();
        assert_eq!(rb.len_bytes(), 0);
    }

    #[test]
    fn test_pack_native_dsd_planar() {
        // Planar DSD: 4 bytes channel 0, 4 bytes channel 1
        let data = vec![0xAA, 0xBB, 0xCC, 0xDD, 0x11, 0x22, 0x33, 0x44];
        let mut left = Vec::new();
        let mut right = Vec::new();

        pack_native_dsd_to_asio(
            &data,
            2,
            ChannelLayout::Planar,
            BitOrder::MsbFirst,
            AsioHwFormat::DsdInt8MSB1,
            &mut left,
            &mut right,
        )
        .unwrap();

        assert_eq!(left, vec![0xAA, 0xBB, 0xCC, 0xDD]);
        assert_eq!(right, vec![0x11, 0x22, 0x33, 0x44]);
    }

    #[test]
    fn test_pack_native_dsd_interleaved_with_bit_reversal() {
        // Interleaved DSD: [L0, R0, L1, R1]
        let data = vec![0b1000_0001, 0b1111_0000];
        let mut left = Vec::new();
        let mut right = Vec::new();

        // Source is MsbFirst, HW expects LsbFirst -> bits must be reversed
        pack_native_dsd_to_asio(
            &data,
            2,
            ChannelLayout::Interleaved,
            BitOrder::MsbFirst,
            AsioHwFormat::DsdInt8LSB1,
            &mut left,
            &mut right,
        )
        .unwrap();

        assert_eq!(left, vec![0b1000_0001]);
        assert_eq!(right, vec![0b0000_1111]);
    }

    #[test]
    fn test_asio_capabilities_reports_native_dsd() {
        let out = AsioAudioOutput::new(OutputConfig::default());
        let caps = out.capabilities();
        assert!(caps.dsd_native);
        assert!(caps.dsd_dop);
        assert!(caps.exclusive);
        assert_eq!(caps.backend_name, "ASIO (Audio Stream Input/Output)");
    }
}
