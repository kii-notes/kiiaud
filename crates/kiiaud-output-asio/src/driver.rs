// SPDX-License-Identifier: MIT OR Apache-2.0

//! Windows ASIO Driver Registry Enumeration and Discovery.

use serde::{Deserialize, Serialize};

/// Discovered ASIO driver registered in Windows Registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsioDriverInfo {
    /// Registry key name (e.g. `"SXW UAD ASIO"`, `"Universal Audio Thunderbolt"`).
    pub name: String,
    /// COM Class ID GUID string (e.g. `"{4582E5A1-6549-43A2-9E9C-A0F12406AE87}"`).
    pub clsid: String,
    /// Optional human-readable driver description.
    pub description: Option<String>,
}

#[cfg(target_os = "windows")]
use windows::{
    Win32::Foundation::ERROR_SUCCESS,
    Win32::System::Registry::{
        HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RegCloseKey, RegEnumKeyExW, RegOpenKeyExW,
        RegQueryValueExW,
    },
    core::{PCWSTR, w},
};

/// Scan Windows Registry for all installed ASIO audio drivers.
///
/// Looks under `HKEY_LOCAL_MACHINE\SOFTWARE\ASIO` (and 32-bit `WOW6432Node\ASIO`).
pub fn scan_asio_drivers() -> Vec<AsioDriverInfo> {
    #[cfg(target_os = "windows")]
    {
        let mut drivers = Vec::new();
        unsafe {
            // Scan HKLM\SOFTWARE\ASIO
            scan_registry_key(HKEY_LOCAL_MACHINE, w!("SOFTWARE\\ASIO"), &mut drivers);

            // Also check WOW6432Node if running on 64-bit OS
            scan_registry_key(
                HKEY_LOCAL_MACHINE,
                w!("SOFTWARE\\WOW6432Node\\ASIO"),
                &mut drivers,
            );
        }
        drivers
    }
    #[cfg(not(target_os = "windows"))]
    {
        Vec::new()
    }
}

#[cfg(target_os = "windows")]
unsafe fn scan_registry_key(root: HKEY, subkey_path: PCWSTR, out: &mut Vec<AsioDriverInfo>) {
    let mut hkey = HKEY::default();
    if RegOpenKeyExW(root, subkey_path, 0, KEY_READ, &mut hkey) != ERROR_SUCCESS {
        return;
    }

    let mut index = 0u32;
    loop {
        let mut name_buf = [0u16; 256];
        let mut name_len = name_buf.len() as u32;

        let status = RegEnumKeyExW(
            hkey,
            index,
            windows::core::PWSTR(name_buf.as_mut_ptr()),
            &mut name_len,
            None,
            windows::core::PWSTR(std::ptr::null_mut()),
            None,
            None,
        );

        if status != ERROR_SUCCESS {
            break;
        }

        let driver_name = String::from_utf16_lossy(&name_buf[..name_len as usize]);

        // Open the driver's subkey to read CLSID and Description
        let mut driver_hkey = HKEY::default();
        let sub_pcwstr: Vec<u16> = driver_name
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        if RegOpenKeyExW(
            hkey,
            PCWSTR(sub_pcwstr.as_ptr()),
            0,
            KEY_READ,
            &mut driver_hkey,
        ) == ERROR_SUCCESS
        {
            let mut clsid_buf = [0u8; 512];
            let mut clsid_len = clsid_buf.len() as u32;

            let clsid_str = if RegQueryValueExW(
                driver_hkey,
                w!("CLSID"),
                None,
                None,
                Some(clsid_buf.as_mut_ptr()),
                Some(&mut clsid_len),
            ) == ERROR_SUCCESS
            {
                let u16_slice = std::slice::from_raw_parts(
                    clsid_buf.as_ptr() as *const u16,
                    (clsid_len as usize) / 2,
                );
                String::from_utf16_lossy(u16_slice)
                    .trim_matches(char::from(0))
                    .to_string()
            } else {
                String::new()
            };

            let mut desc_buf = [0u8; 512];
            let mut desc_len = desc_buf.len() as u32;
            let description = if RegQueryValueExW(
                driver_hkey,
                w!("Description"),
                None,
                None,
                Some(desc_buf.as_mut_ptr()),
                Some(&mut desc_len),
            ) == ERROR_SUCCESS
            {
                let u16_slice = std::slice::from_raw_parts(
                    desc_buf.as_ptr() as *const u16,
                    (desc_len as usize) / 2,
                );
                let s = String::from_utf16_lossy(u16_slice)
                    .trim_matches(char::from(0))
                    .to_string();
                if s.is_empty() { None } else { Some(s) }
            } else {
                None
            };

            let _ = RegCloseKey(driver_hkey);

            if !clsid_str.is_empty() && !out.iter().any(|d| d.name == driver_name) {
                out.push(AsioDriverInfo {
                    name: driver_name,
                    clsid: clsid_str,
                    description,
                });
            }
        }

        index += 1;
    }

    let _ = RegCloseKey(hkey);
}

/// Find an ASIO driver matching the target string (`"default"`, exact name, or case-insensitive substring).
pub fn find_asio_driver(target: &str) -> Option<AsioDriverInfo> {
    let drivers = scan_asio_drivers();
    if drivers.is_empty() {
        return None;
    }

    if target.is_empty() || target.eq_ignore_ascii_case("default") {
        return drivers.into_iter().next();
    }

    let target_lower = target.trim().to_lowercase();

    // 1. Exact match
    if let Some(d) = drivers.iter().find(|d| d.name.eq_ignore_ascii_case(target.trim())) {
        return Some(d.clone());
    }

    // 2. Substring match
    if let Some(d) = drivers.iter().find(|d| {
        let name_lower = d.name.to_lowercase();
        name_lower.contains(&target_lower) || target_lower.contains(&name_lower)
    }) {
        return Some(d.clone());
    }

    None
}

