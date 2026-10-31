// SPDX-License-Identifier: MIT OR Apache-2.0

//! Low-level ASIO (Audio Stream Input/Output) C/COM type definitions and constants.
//!
//! Complies with Steinberg ASIO 2.3 specification and DSD-over-ASIO (Native DSD) extensions.

#![allow(non_snake_case, non_camel_case_types, non_upper_case_globals, dead_code)]

use std::ffi::c_void;

/// ASIO return error codes (`ASIOError`).
pub type ASIOError = i32;

pub const ASE_OK: ASIOError = 0;
pub const ASE_SUCCESS: ASIOError = 0x3f484c2c;
pub const ASE_NotPresent: ASIOError = -1000;
pub const ASE_HWMalfunction: ASIOError = -999;
pub const ASE_InvalidParameter: ASIOError = -998;
pub const ASE_InvalidMode: ASIOError = -997;
pub const ASE_SPNotAdvancing: ASIOError = -996;
pub const ASE_NoClock: ASIOError = -995;
pub const ASE_NoMemory: ASIOError = -994;

/// ASIO boolean (`ASIOBool`).
pub type ASIOBool = i32;
pub const ASIO_FALSE: ASIOBool = 0;
pub const ASIO_TRUE: ASIOBool = 1;

/// ASIO sample types (`ASIOSampleType`).
pub type ASIOSampleType = i32;

pub const ASIOSTInt16MSB: ASIOSampleType = 0;
pub const ASIOSTInt24MSB: ASIOSampleType = 1;
pub const ASIOSTInt32MSB: ASIOSampleType = 2;
pub const ASIOSTFloat32MSB: ASIOSampleType = 3;
pub const ASIOSTFloat64MSB: ASIOSampleType = 4;
pub const ASIOSTInt32MSB16: ASIOSampleType = 8;
pub const ASIOSTInt32MSB18: ASIOSampleType = 9;
pub const ASIOSTInt32MSB20: ASIOSampleType = 10;
pub const ASIOSTInt32MSB24: ASIOSampleType = 11;
pub const ASIOSTInt16LSB: ASIOSampleType = 16;
pub const ASIOSTInt24LSB: ASIOSampleType = 17;
pub const ASIOSTInt32LSB: ASIOSampleType = 18;
pub const ASIOSTFloat32LSB: ASIOSampleType = 19;
pub const ASIOSTFloat64LSB: ASIOSampleType = 20;
pub const ASIOSTInt32LSB16: ASIOSampleType = 24;
pub const ASIOSTInt32LSB18: ASIOSampleType = 25;
pub const ASIOSTInt32LSB20: ASIOSampleType = 26;
pub const ASIOSTInt32LSB24: ASIOSampleType = 27;

/// Native DSD Sample Types (Steinberg / DSD-over-ASIO standard).
/// DSD 1-bit data, 8 samples per byte.
pub const ASIOSTDSDInt8LSB1: ASIOSampleType = 32;
pub const ASIOSTDSDInt8MSB1: ASIOSampleType = 33;
pub const ASIOSTDSDInt8NER8: ASIOSampleType = 34;

/// ASIO Future Selectors.
pub const kAsioEnableTimeCodeRead: i32 = 1;
pub const kAsioDisableTimeCodeRead: i32 = 2;
pub const kAsioSetInputMonitor: i32 = 3;
pub const kAsioResetRequest: i32 = 4;
pub const kAsioBufferSizeChange: i32 = 5;
pub const kAsioResyncRequest: i32 = 6;
pub const kAsioCanDoIoFormat: i32 = 0x23111960;
pub const kAsioSetIoFormat: i32 = 0x23111961;
pub const kAsioCanDoTimeInfo: i32 = 0x23111962;
pub const kAsioCanDoTimeCode: i32 = 0x23111963;

/// ASIO IO Format types for `kAsioSetIoFormat` and `kAsioCanDoIoFormat`.
pub const kAsioFormatInvalid: i32 = 0;
pub const kAsioFormatPCM: i32 = 1;
pub const kAsioFormatDSD: i32 = 2;

/// ASIO IO Format parameter structure passed to `future(kAsioSetIoFormat, ...)`.
#[repr(C)]
pub struct ASIOIoFormat {
    pub FormatType: i32,
    pub future: [u8; 512],
}

impl Default for ASIOIoFormat {
    fn default() -> Self {
        Self {
            FormatType: kAsioFormatInvalid,
            future: [0; 512],
        }
    }
}

/// Buffer half-pointers allocated by the ASIO driver (`ASIOBufferInfo`).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ASIOBufferInfo {
    /// 0 = output channel, 1 = input channel.
    pub isInput: ASIOBool,
    /// Channel index (0-based).
    pub channelNum: i32,
    /// Dual buffer half-pointers for double buffering (index 0 and 1).
    pub buffers: [*mut c_void; 2],
}

impl Default for ASIOBufferInfo {
    fn default() -> Self {
        Self {
            isInput: 0,
            channelNum: 0,
            buffers: [std::ptr::null_mut(), std::ptr::null_mut()],
        }
    }
}

unsafe impl Send for ASIOBufferInfo {}
unsafe impl Sync for ASIOBufferInfo {}


/// Channel metadata returned by `getChannelInfo`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ASIOChannelInfo {
    pub channel: i32,
    pub isInput: ASIOBool,
    pub isActive: ASIOBool,
    pub channelGroup: i32,
    pub sampleType: ASIOSampleType,
    pub name: [u8; 32],
}

impl Default for ASIOChannelInfo {
    fn default() -> Self {
        Self {
            channel: 0,
            isInput: 0,
            isActive: 0,
            channelGroup: 0,
            sampleType: 0,
            name: [0; 32],
        }
    }
}

/// ASIO Host Callbacks passed to `createBuffers`.
#[repr(C)]
pub struct ASIOCallbacks {
    pub bufferSwitch: Option<unsafe extern "system" fn(doubleBufferIndex: i32, directProcess: ASIOBool)>,
    pub sampleRateDidChange: Option<unsafe extern "system" fn(sRate: f64)>,
    pub asioMessage: Option<
        unsafe extern "system" fn(
            selector: i32,
            value: i32,
            message: *mut c_void,
            opt: *mut f64,
        ) -> i32,
    >,
    pub bufferSwitchTimeInfo: Option<
        unsafe extern "system" fn(
            params: *mut c_void,
            doubleBufferIndex: i32,
            directProcess: ASIOBool,
        ) -> *mut c_void,
    >,
}

/// Full COM Virtual Table for the Steinberg `IASIO` interface.
#[repr(C)]
pub struct IASIOVtbl {
    // Standard IUnknown methods
    pub QueryInterface: unsafe extern "system" fn(
        this: *mut c_void,
        riid: *const c_void,
        ppv: *mut *mut c_void,
    ) -> i32,
    pub AddRef: unsafe extern "system" fn(this: *mut c_void) -> u32,
    pub Release: unsafe extern "system" fn(this: *mut c_void) -> u32,

    // IASIO methods
    pub init: unsafe extern "system" fn(this: *mut c_void, sysHandle: *mut c_void) -> ASIOBool,
    pub getDriverName: unsafe extern "system" fn(this: *mut c_void, name: *mut u8),
    pub getDriverVersion: unsafe extern "system" fn(this: *mut c_void) -> i32,
    pub getErrorMessage: unsafe extern "system" fn(this: *mut c_void, string: *mut u8),
    pub start: unsafe extern "system" fn(this: *mut c_void) -> ASIOError,
    pub stop: unsafe extern "system" fn(this: *mut c_void) -> ASIOError,
    pub getChannels: unsafe extern "system" fn(
        this: *mut c_void,
        numInputChannels: *mut i32,
        numOutputChannels: *mut i32,
    ) -> ASIOError,
    pub getLatencies: unsafe extern "system" fn(
        this: *mut c_void,
        inputLatency: *mut i32,
        outputLatency: *mut i32,
    ) -> ASIOError,
    pub getBufferSize: unsafe extern "system" fn(
        this: *mut c_void,
        minSize: *mut i32,
        maxSize: *mut i32,
        preferredSize: *mut i32,
        granularity: *mut i32,
    ) -> ASIOError,
    pub canSampleRate:
        unsafe extern "system" fn(this: *mut c_void, sampleRate: f64) -> ASIOError,
    pub getSampleRate:
        unsafe extern "system" fn(this: *mut c_void, sampleRate: *mut f64) -> ASIOError,
    pub setSampleRate:
        unsafe extern "system" fn(this: *mut c_void, sampleRate: f64) -> ASIOError,
    pub getClockSources: unsafe extern "system" fn(
        this: *mut c_void,
        clocks: *mut c_void,
        numSources: *mut i32,
    ) -> ASIOError,
    pub setClockSource: unsafe extern "system" fn(this: *mut c_void, reference: i32) -> ASIOError,
    pub getSamplePosition: unsafe extern "system" fn(
        this: *mut c_void,
        sPos: *mut c_void,
        tStamp: *mut c_void,
    ) -> ASIOError,
    pub getChannelInfo: unsafe extern "system" fn(
        this: *mut c_void,
        info: *mut ASIOChannelInfo,
    ) -> ASIOError,
    pub createBuffers: unsafe extern "system" fn(
        this: *mut c_void,
        bufferInfos: *mut ASIOBufferInfo,
        numChannels: i32,
        bufferSize: i32,
        callbacks: *mut ASIOCallbacks,
    ) -> ASIOError,
    pub disposeBuffers: unsafe extern "system" fn(this: *mut c_void) -> ASIOError,
    pub controlPanel: unsafe extern "system" fn(this: *mut c_void) -> ASIOError,
    pub future: unsafe extern "system" fn(this: *mut c_void, selector: i32, opt: *mut c_void) -> ASIOError,
}

/// COM Interface Pointer to `IASIO`.
#[repr(C)]
pub struct IASIO {
    pub lpVtbl: *const IASIOVtbl,
}
