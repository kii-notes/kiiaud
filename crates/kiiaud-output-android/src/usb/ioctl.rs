// SPDX-License-Identifier: MIT OR Apache-2.0

//! Linux `usbdevfs` ioctl definitions and data structures for Pure Rust direct USB audio streaming.
//!
//! Compatible with Linux kernel `/dev/bus/usb` and Android USB Host API (`UsbDeviceConnection.getFileDescriptor()`).

#![allow(non_camel_case_types, non_snake_case, dead_code)]

use std::os::raw::{c_int, c_uchar, c_uint, c_ulong, c_void};

// ── URB Constants ────────────────────────────────────────────────────────────

/// Isochronous transfer type
pub const USBDEVFS_URB_TYPE_ISO: c_uchar = 0;
pub const USBDEVFS_URB_TYPE_INTERRUPT: c_uchar = 1;
pub const USBDEVFS_URB_TYPE_CONTROL: c_uchar = 2;
pub const USBDEVFS_URB_TYPE_BULK: c_uchar = 3;

/// Critical flag for Isochronous URBs: Schedule packets as soon as possible.
/// Without this flag, packets with `start_frame = 0` are scheduled in the past,
/// causing the xHCI host controller to silently drop all audio packets.
pub const USBDEVFS_URB_ISO_ASAP: c_uint = 0x02;

// ── Linux usbdevfs Structures ────────────────────────────────────────────────

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct UsbdevfsIsoPacketDesc {
    pub length: c_uint,
    pub actual_length: c_uint,
    pub status: c_uint,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct UsbdevfsCtrltransfer {
    pub bRequestType: u8,
    pub bRequest: u8,
    pub wValue: u16,
    pub wIndex: u16,
    pub wLength: u16,
    pub timeout: u32, // milliseconds
    pub data: *mut c_void,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct UsbdevfsSetinterface {
    pub interface: c_uint,
    pub altsetting: c_uint,
}

/// Linux `usbdevfs_urb` structure.
/// In 64-bit systems this is 56 bytes + flexible array `iso_frame_desc`.
#[repr(C)]
pub struct UsbdevfsUrb {
    pub urb_type: c_uchar,
    pub endpoint: c_uchar,
    pub status: c_int,
    pub flags: c_uint,
    pub buffer: *mut c_void,
    pub buffer_length: c_int,
    pub actual_length: c_int,
    pub start_frame: c_int,
    /// Union of number_of_packets (for ISO) or stream_id (for bulk streams)
    pub number_of_packets: c_int,
    pub error_count: c_int,
    pub signr: c_uint,
    pub usercontext: *mut c_void,
    // Flexible array member: struct usbdevfs_iso_packet_desc iso_frame_desc[]
}

// ── Ioctl Encoding Helpers ───────────────────────────────────────────────────

const IOC_NRBITS: u32 = 8;
const IOC_TYPEBITS: u32 = 8;
const IOC_SIZEBITS: u32 = 14;
const IOC_DIRBITS: u32 = 2;

const IOC_NRSHIFT: u32 = 0;
const IOC_TYPESHIFT: u32 = IOC_NRSHIFT + IOC_NRBITS;
const IOC_SIZESHIFT: u32 = IOC_TYPESHIFT + IOC_TYPEBITS;
const IOC_DIRSHIFT: u32 = IOC_SIZESHIFT + IOC_SIZEBITS;

const IOC_NONE: u32 = 0;
const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;

const fn _ioc(dir: u32, typ: u8, nr: u8, size: usize) -> c_ulong {
    ((dir << IOC_DIRSHIFT)
        | ((size as u32) << IOC_SIZESHIFT)
        | ((typ as u32) << IOC_TYPESHIFT)
        | (nr as u32)) as c_ulong
}

const fn _io(typ: u8, nr: u8) -> c_ulong {
    _ioc(IOC_NONE, typ, nr, 0)
}

const fn _ior<T>(typ: u8, nr: u8) -> c_ulong {
    _ioc(IOC_READ, typ, nr, std::mem::size_of::<T>())
}

const fn _iow<T>(typ: u8, nr: u8) -> c_ulong {
    _ioc(IOC_WRITE, typ, nr, std::mem::size_of::<T>())
}

const fn _iowr<T>(typ: u8, nr: u8) -> c_ulong {
    _ioc(IOC_READ | IOC_WRITE, typ, nr, std::mem::size_of::<T>())
}

// ── Ioctl Codes ──────────────────────────────────────────────────────────────

pub const USBDEVFS_CONTROL: c_ulong = _iowr::<UsbdevfsCtrltransfer>(b'U', 0);
pub const USBDEVFS_SETINTERFACE: c_ulong = _ior::<UsbdevfsSetinterface>(b'U', 4);
pub const USBDEVFS_CLAIMINTERFACE: c_ulong = _ior::<c_uint>(b'U', 15);
pub const USBDEVFS_RELEASEINTERFACE: c_ulong = _ior::<c_uint>(b'U', 16);
pub const USBDEVFS_SUBMITURB: c_ulong = _ior::<UsbdevfsUrb>(b'U', 10);
pub const USBDEVFS_DISCARDURB: c_ulong = _io(b'U', 11);
pub const USBDEVFS_REAPURB: c_ulong = _iow::<*mut c_void>(b'U', 12);
pub const USBDEVFS_REAPURBNDELAY: c_ulong = _iow::<*mut c_void>(b'U', 13);
pub const USBDEVFS_RESET: c_ulong = _io(b'U', 20);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ioctl_values_match_linux_headers() {
        assert_eq!(USBDEVFS_CONTROL, 0xc0185500);
        assert_eq!(USBDEVFS_SETINTERFACE, 0x80085504);
        assert_eq!(USBDEVFS_CLAIMINTERFACE, 0x8004550f);
        assert_eq!(USBDEVFS_RELEASEINTERFACE, 0x80045510);
        assert_eq!(USBDEVFS_SUBMITURB, 0x8038550a);
        assert_eq!(USBDEVFS_DISCARDURB, 0x0000550b);
        assert_eq!(USBDEVFS_REAPURB, 0x4008550c);
        assert_eq!(USBDEVFS_REAPURBNDELAY, 0x4008550d);
        assert_eq!(USBDEVFS_RESET, 0x00005514);
    }
}
