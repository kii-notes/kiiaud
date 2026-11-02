// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pure Rust USB Audio Class 1.0 (UAC1) and 2.0 (UAC2) control requests and interface management.
//!
//! Provides functions to claim/release interfaces, configure sample rates via Clock Source PLL (UAC2)
//! or Endpoint Frequency Control (UAC1), and reset USB endpoints.

use super::ioctl::*;
use kiiaud_core::{KiiaudError, Result};
use std::os::raw::c_uint;
use std::os::unix::io::RawFd;

// UAC Request Types
pub const USB_REQ_TYPE_CLASS_INTERFACE_OUT: u8 = 0x22;
pub const USB_REQ_TYPE_CLASS_INTERFACE_IN: u8 = 0xA2;
pub const USB_REQ_TYPE_CLASS_ENDPOINT_OUT: u8 = 0x22;
pub const USB_REQ_TYPE_CLASS_ENDPOINT_IN: u8 = 0xA2;

// UAC Class-Specific Request Codes
pub const UAC_CS_REQ_CUR: u8 = 0x01;
pub const UAC_CS_REQ_RANGE: u8 = 0x02;

// UAC1 Endpoint Control Selectors
pub const UAC1_SAMPLING_FREQ_CONTROL: u8 = 0x01;

// UAC2 Clock Source Control Selectors
pub const UAC2_CS_SAM_FREQ_CONTROL: u8 = 0x01;
pub const UAC2_CS_CLOCK_VALID_CONTROL: u8 = 0x02;

/// Default timeout for control transfers (milliseconds)
pub const USB_CTRL_TIMEOUT_MS: u32 = 1000;

/// Claim an interface via `USBDEVFS_CLAIMINTERFACE`.
/// This detaches any kernel driver (such as `snd-usb-audio`) from the interface.
pub fn claim_interface(fd: RawFd, interface_id: u8) -> Result<()> {
    let iface = interface_id as c_uint;
    let ret = unsafe { libc::ioctl(fd, USBDEVFS_CLAIMINTERFACE, &iface) };
    if ret < 0 {
        let err = std::io::Error::last_os_error();
        tracing::warn!(
            interface_id = interface_id,
            error = %err,
            "failed to claim USB interface via usbdevfs (may already be claimed by Android UsbManager)"
        );
    } else {
        tracing::debug!(interface_id = interface_id, "USB interface claimed successfully");
    }
    Ok(())
}

/// Release a claimed interface via `USBDEVFS_RELEASEINTERFACE`.
pub fn release_interface(fd: RawFd, interface_id: u8) -> Result<()> {
    let iface = interface_id as c_uint;
    let ret = unsafe { libc::ioctl(fd, USBDEVFS_RELEASEINTERFACE, &iface) };
    if ret < 0 {
        let err = std::io::Error::last_os_error();
        tracing::debug!(
            interface_id = interface_id,
            error = %err,
            "failed to release USB interface"
        );
    }
    Ok(())
}

/// Set interface alternate setting via `USBDEVFS_SETINTERFACE`.
/// Note: On Android, Bug 3 requires that the final streaming alt setting be activated via
/// Java `UsbDeviceConnection.setInterface()` to allocate ISO bandwidth. Native `set_interface(alt=0)`
/// is used to cleanly stop streaming without releasing the interface.
pub fn set_interface(fd: RawFd, interface_id: u8, alt_setting: u8) -> Result<()> {
    let mut param = UsbdevfsSetinterface {
        interface: interface_id as c_uint,
        altsetting: alt_setting as c_uint,
    };
    let ret = unsafe { libc::ioctl(fd, USBDEVFS_SETINTERFACE, &mut param) };
    if ret < 0 {
        let err = std::io::Error::last_os_error();
        return Err(KiiaudError::Output(format!(
            "failed to set USB interface {} alt {}: {}",
            interface_id, alt_setting, err
        )));
    }
    tracing::debug!(
        interface_id = interface_id,
        alt_setting = alt_setting,
        "USB interface alt setting selected"
    );
    Ok(())
}

/// Reset the USB device via `USBDEVFS_RESET`.
pub fn reset_device(fd: RawFd) -> Result<()> {
    let ret = unsafe { libc::ioctl(fd, USBDEVFS_RESET, 0) };
    if ret < 0 {
        let err = std::io::Error::last_os_error();
        tracing::warn!(error = %err, "USBDEVFS_RESET returned error");
    } else {
        tracing::info!("USB device reset successfully");
    }
    Ok(())
}

/// Set DAC sample rate for UAC2 via Clock Source SAM_FREQ_CONTROL (`SET_CUR`).
///
/// Targets the verified `clock_source_id` (Bug 1 fix).
pub fn set_uac2_sample_rate(
    fd: RawFd,
    control_iface: u8,
    clock_source_id: u8,
    sample_rate: u32,
) -> Result<()> {
    let mut rate_bytes = sample_rate.to_le_bytes();

    let mut transfer = UsbdevfsCtrltransfer {
        bRequestType: USB_REQ_TYPE_CLASS_INTERFACE_OUT,
        bRequest: UAC_CS_REQ_CUR,
        wValue: (UAC2_CS_SAM_FREQ_CONTROL as u16) << 8,
        wIndex: ((clock_source_id as u16) << 8) | (control_iface as u16),
        wLength: 4,
        timeout: USB_CTRL_TIMEOUT_MS,
        data: rate_bytes.as_mut_ptr() as *mut _,
    };

    let ret = unsafe { libc::ioctl(fd, USBDEVFS_CONTROL, &mut transfer) };
    if ret < 0 {
        let err = std::io::Error::last_os_error();
        return Err(KiiaudError::Output(format!(
            "failed to set UAC2 sample rate {} Hz on clock ID 0x{:02X}: {}",
            sample_rate, clock_source_id, err
        )));
    }

    tracing::info!(
        sample_rate = sample_rate,
        clock_id = clock_source_id,
        "configured UAC2 DAC sample rate via Clock Source SET_CUR"
    );
    Ok(())
}

/// Set DAC sample rate for UAC1 via Endpoint Sampling Frequency Control (`SET_CUR`).
///
/// UAC1 transmits a 3-byte sample rate directly to the Isochronous OUT endpoint.
pub fn set_uac1_sample_rate(
    fd: RawFd,
    endpoint: u8,
    sample_rate: u32,
) -> Result<()> {
    let mut rate_bytes = [
        (sample_rate & 0xFF) as u8,
        ((sample_rate >> 8) & 0xFF) as u8,
        ((sample_rate >> 16) & 0xFF) as u8,
    ];

    let mut transfer = UsbdevfsCtrltransfer {
        bRequestType: USB_REQ_TYPE_CLASS_ENDPOINT_OUT,
        bRequest: UAC_CS_REQ_CUR,
        wValue: (UAC1_SAMPLING_FREQ_CONTROL as u16) << 8,
        wIndex: endpoint as u16,
        wLength: 3,
        timeout: USB_CTRL_TIMEOUT_MS,
        data: rate_bytes.as_mut_ptr() as *mut _,
    };

    let ret = unsafe { libc::ioctl(fd, USBDEVFS_CONTROL, &mut transfer) };
    if ret < 0 {
        let err = std::io::Error::last_os_error();
        return Err(KiiaudError::Output(format!(
            "failed to set UAC1 sample rate {} Hz on endpoint 0x{:02X}: {}",
            sample_rate, endpoint, err
        )));
    }

    tracing::info!(
        sample_rate = sample_rate,
        endpoint = endpoint,
        "configured UAC1 DAC sample rate via Endpoint SET_CUR"
    );
    Ok(())
}
