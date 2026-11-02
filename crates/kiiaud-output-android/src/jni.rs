// SPDX-License-Identifier: MIT OR Apache-2.0

//! Lightweight JNI bindings for Android Foreground Service and UI integration.
//!
//! Exposes runtime hot-plug USB DAC management and capabilities query to Kotlin without overhead.

use std::os::raw::{c_int, c_uchar, c_void};
use std::sync::atomic::{AtomicI32, Ordering};

/// Global active USB file descriptor registry.
/// Updated dynamically when USB DAC is attached or detached via Android UsbManager.
static ACTIVE_USB_FD: AtomicI32 = AtomicI32::new(-1);

/// Set the active USB device file descriptor dynamically at runtime.
pub fn set_active_usb_fd(fd: i32) {
    ACTIVE_USB_FD.store(fd, Ordering::SeqCst);
    tracing::info!(fd = fd, "active USB DAC file descriptor updated via JNI");
}

/// Clear the active USB device file descriptor on detach.
pub fn clear_active_usb_fd() {
    ACTIVE_USB_FD.store(-1, Ordering::SeqCst);
    tracing::info!("active USB DAC cleared via JNI");
}

/// Get the currently active USB device file descriptor, if any.
pub fn get_active_usb_fd() -> Option<i32> {
    let fd = ACTIVE_USB_FD.load(Ordering::SeqCst);
    if fd >= 0 { Some(fd) } else { None }
}

// ── Native JNI Functions ────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Java_org_kiiaud_KiiaudNative_setUsbDeviceFd(
    _env: *mut c_void,
    _class: *mut c_void,
    fd: c_int,
) {
    set_active_usb_fd(fd as i32);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Java_org_kiiaud_KiiaudNative_clearUsbDevice(
    _env: *mut c_void,
    _class: *mut c_void,
) {
    clear_active_usb_fd();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Java_org_kiiaud_KiiaudNative_getActiveUsbFd(
    _env: *mut c_void,
    _class: *mut c_void,
) -> c_int {
    get_active_usb_fd().unwrap_or(-1) as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Java_org_kiiaud_KiiaudNative_isUsbDacActive(
    _env: *mut c_void,
    _class: *mut c_void,
) -> c_uchar {
    if get_active_usb_fd().is_some() { 1 } else { 0 }
}
