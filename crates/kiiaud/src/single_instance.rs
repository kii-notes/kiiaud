// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cross-platform Single Instance Guard for `kiiaud`.
//!
//! Prevents multiple instances of the daemon from running concurrently,
//! avoiding port collisions (WebUI 7110, MPD 6600, IPC 7111), hardware audio device conflicts,
//! duplicate Jellyfin session registrations, and LMS Gateway rate-limit (HTTP 429) lockouts.

use anyhow::{Result, bail};

#[cfg(windows)]
pub struct SingleInstanceGuard {
    handle: windows::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl SingleInstanceGuard {
    pub fn acquire(name: &str) -> Result<Self> {
        use windows::core::{HSTRING, PCWSTR};
        use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
        use windows::Win32::System::Threading::CreateMutexW;

        // Clean name for mutex
        let sanitized = name.replace(|c: char| !c.is_alphanumeric() && c != '_' && c != '-', "_");
        let mutex_name = format!("Local\\kiiaud_single_instance_{sanitized}");
        let h_name = HSTRING::from(mutex_name);

        unsafe {
            let handle = CreateMutexW(None, true, PCWSTR(h_name.as_ptr()))
                .map_err(|e| anyhow::anyhow!("Failed to create Windows single-instance mutex: {e}"))?;

            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(handle);
                bail!(
                    "Another instance of kiiaud ('{}') is already running.\n\
                     Only one instance can run at a time to prevent audio and network conflicts.\n\
                     (Hint: Check Windows Task Manager or run 'Stop-Process -Name kiiaud' in PowerShell).",
                    name
                );
            }

            Ok(Self { handle })
        }
    }
}

#[cfg(windows)]
impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        unsafe {
            use windows::Win32::Foundation::CloseHandle;
            if !self.handle.is_invalid() {
                let _ = CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(unix)]
pub struct SingleInstanceGuard {
    file: std::fs::File,
    path: std::path::PathBuf,
}

#[cfg(unix)]
impl SingleInstanceGuard {
    pub fn acquire(name: &str) -> Result<Self> {
        use std::io::{Read, Write};
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::io::AsRawFd;

        let sanitized = name.replace(|c: char| !c.is_alphanumeric() && c != '_' && c != '-', "_");
        let lock_dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);

        let lock_path = lock_dir.join(format!("kiiaud_{sanitized}.lock"));

        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&lock_path)
            .map_err(|e| anyhow::anyhow!("Failed to open lock file '{}': {e}", lock_path.display()))?;

        let fd = file.as_raw_fd();
        let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
        if ret != 0 {
            let mut existing_pid = String::new();
            let _ = file.read_to_string(&mut existing_pid);
            let pid_info = if !existing_pid.trim().is_empty() {
                format!(" (PID: {})", existing_pid.trim())
            } else {
                String::new()
            };

            bail!(
                "Another instance of kiiaud ('{}') is already running{}.\n\
                 Only one instance can run at a time to prevent audio and network conflicts.\n\
                 (Lock file: {})",
                name,
                pid_info,
                lock_path.display()
            );
        }

        // Write current PID into lock file for easy troubleshooting
        let _ = file.set_len(0);
        let pid = std::process::id();
        let _ = writeln!(file, "{pid}");
        let _ = file.flush();

        Ok(Self {
            file,
            path: lock_path,
        })
    }
}

#[cfg(unix)]
impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        let fd = self.file.as_raw_fd();
        unsafe {
            let _ = libc::flock(fd, libc::LOCK_UN);
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(not(any(windows, unix)))]
pub struct SingleInstanceGuard;

#[cfg(not(any(windows, unix)))]
impl SingleInstanceGuard {
    pub fn acquire(_name: &str) -> Result<Self> {
        Ok(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_instance_guard_lifecycle() {
        let test_name = format!("test_kiiaud_inst_{}", std::process::id());

        // First acquisition should succeed
        let guard1 = SingleInstanceGuard::acquire(&test_name);
        assert!(guard1.is_ok(), "First acquisition should succeed");

        // Second acquisition with same name must fail
        let guard2 = SingleInstanceGuard::acquire(&test_name);
        assert!(guard2.is_err(), "Second acquisition while first is held must fail");

        // Drop the first guard
        drop(guard1);

        // Third acquisition after drop should succeed
        let guard3 = SingleInstanceGuard::acquire(&test_name);
        assert!(guard3.is_ok(), "Acquisition after drop should succeed");
    }
}
