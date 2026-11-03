// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cross-platform service and background task management for `kiiaud`.
//!
//! - **Windows**: Registers `kiiaud` in Windows Task Scheduler (`schtasks.exe`) to run at
//!   user logon in the background (`--background`) with highest privileges. Running in the
//!   interactive user session guarantees full access to ASIO drivers and WASAPI Exclusive mode
//!   (avoiding Windows Session 0 Isolation restrictions).
//! - **Linux**: Registers and manages a `systemd` user service unit (`~/.config/systemd/user/kiiaud.service`).

use anyhow::{Context, Result, bail};

/// Available service control actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ServiceAction {
    /// Install kiiaud as an auto-starting background service/task.
    Install,
    /// Uninstall and remove kiiaud background service/task.
    Uninstall,
    /// Query status of the background service/task.
    Status,
    /// Start the installed background service/task.
    Start,
    /// Stop the running background service/task.
    Stop,
}

/// Execute a service management action according to the current operating system.
pub fn handle_service_command(action: ServiceAction) -> Result<()> {
    match action {
        ServiceAction::Install => install_service(),
        ServiceAction::Uninstall => uninstall_service(),
        ServiceAction::Status => status_service(),
        ServiceAction::Start => start_service(),
        ServiceAction::Stop => stop_service(),
    }
}

// -----------------------------------------------------------------------------
// Windows Task Scheduler Implementation
// -----------------------------------------------------------------------------
#[cfg(windows)]
fn install_service() -> Result<()> {
    let exe_path = std::env::current_exe().context("Failed to get current executable path")?;
    let exe_str = exe_path.to_string_lossy();

    println!("=== kiiaud Windows Background Service Installation ===");
    println!("Target executable: {}", exe_str);
    println!("Registering scheduled task 'kiiaud' (runs at user logon in background)...");

    let run_cmd = format!("\"{}\" --background", exe_str);
    let output = std::process::Command::new("schtasks")
        .args([
            "/create",
            "/tn",
            "kiiaud",
            "/tr",
            run_cmd.as_str(),
            "/sc",
            "onlogon",
            "/rl",
            "highest",
            "/f",
        ])
        .output()
        .context("Failed to execute schtasks.exe (is Task Scheduler available?)")?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to register scheduled task: {}", err.trim());
    }

    println!("\n[OK] Successfully registered 'kiiaud' background task!");
    println!("Features:");
    println!("  - Auto-start: Starts automatically whenever you log into Windows.");
    println!(
        "  - Session-aware: Runs in user session with full ASIO / WASAPI Exclusive DAC access."
    );
    println!(
        "  - Headless/Tray: Runs hidden with a System Tray icon in the taskbar notification area."
    );
    println!("\nManagement Commands:");
    println!("  Start now:   kiiaud --service start  (or kiiaud --service-start)");
    println!("  Stop now:    kiiaud --service stop   (or kiiaud --service-stop)");
    println!("  Check state: kiiaud --service status (or kiiaud --service-status)");
    println!("  Uninstall:   kiiaud --service uninstall (or kiiaud --service-uninstall)");

    Ok(())
}

#[cfg(windows)]
fn uninstall_service() -> Result<()> {
    println!("=== kiiaud Windows Background Service Uninstallation ===");
    println!("Stopping background task if currently running...");
    let _ = std::process::Command::new("schtasks")
        .args(["/end", "/tn", "kiiaud"])
        .output();

    println!("Removing scheduled task 'kiiaud' from Windows Task Scheduler...");
    let output = std::process::Command::new("schtasks")
        .args(["/delete", "/tn", "kiiaud", "/f"])
        .output()
        .context("Failed to execute schtasks.exe")?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to delete scheduled task: {}", err.trim());
    }

    println!("[OK] Successfully uninstalled 'kiiaud' background task from Windows.");
    Ok(())
}

#[cfg(windows)]
fn status_service() -> Result<()> {
    println!("=== kiiaud Windows Background Service Status ===");
    let output = std::process::Command::new("schtasks")
        .args(["/query", "/tn", "kiiaud", "/fo", "list", "/v"])
        .output()
        .context("Failed to execute schtasks.exe")?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        println!(
            "Service task 'kiiaud' is NOT installed or not found: {}",
            err.trim()
        );
        println!("To install it, run: kiiaud --service install");
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    println!("{}", stdout);
    Ok(())
}

#[cfg(windows)]
fn start_service() -> Result<()> {
    println!("Triggering 'kiiaud' background task in Windows Task Scheduler...");
    let output = std::process::Command::new("schtasks")
        .args(["/run", "/tn", "kiiaud"])
        .output()
        .context("Failed to execute schtasks.exe")?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to start scheduled task: {}", err.trim());
    }

    println!("[OK] Task 'kiiaud' started successfully in the background.");
    println!("Look for the kiiaud icon in your system notification tray.");
    Ok(())
}

#[cfg(windows)]
fn stop_service() -> Result<()> {
    println!("Stopping 'kiiaud' background task in Windows Task Scheduler...");
    let output = std::process::Command::new("schtasks")
        .args(["/end", "/tn", "kiiaud"])
        .output()
        .context("Failed to execute schtasks.exe")?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to stop scheduled task: {}", err.trim());
    }

    println!("[OK] Task 'kiiaud' stopped successfully.");
    Ok(())
}

// -----------------------------------------------------------------------------
// Linux systemd User Service Implementation
// -----------------------------------------------------------------------------
#[cfg(target_os = "linux")]
fn install_service() -> Result<()> {
    let exe_path = std::env::current_exe().context("Failed to get current executable path")?;
    let home = std::env::var("HOME").context("HOME environment variable not set")?;
    let unit_dir = std::path::PathBuf::from(home).join(".config/systemd/user");
    std::fs::create_dir_all(&unit_dir).context("Failed to create systemd user directory")?;
    let unit_file = unit_dir.join("kiiaud.service");

    println!("=== kiiaud Linux systemd Service Installation ===");
    println!("Target executable: {}", exe_path.display());
    println!("Writing systemd user unit to {}", unit_file.display());

    let unit_content = format!(
        r#"[Unit]
Description=kiiaud — Bit-Perfect Network Audio Playback Endpoint (User Session)
Documentation=https://github.com/kii-notes/kiiaud
After=default.target sound.target

[Service]
Type=simple
ExecStart={}
Restart=on-failure
RestartSec=3
LimitRTPRIO=95
LimitMEMLOCK=infinity

[Install]
WantedBy=default.target
"#,
        exe_path.display()
    );

    std::fs::write(&unit_file, unit_content).context("Failed to write unit file")?;

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();

    let status = std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", "kiiaud"])
        .status()
        .context("Failed to run systemctl")?;

    if !status.success() {
        bail!(
            "systemctl --user enable --now kiiaud failed with exit code {:?}",
            status.code()
        );
    }

    println!("\n[OK] Successfully registered and started kiiaud systemd user service!");
    println!("Commands:");
    println!("  Check status: kiiaud --service status (or systemctl --user status kiiaud)");
    println!("  Stop service: kiiaud --service stop   (or systemctl --user stop kiiaud)");
    println!("  Start service: kiiaud --service start (or systemctl --user start kiiaud)");
    println!("  Uninstall:    kiiaud --service uninstall");

    Ok(())
}

#[cfg(target_os = "linux")]
fn uninstall_service() -> Result<()> {
    println!("=== kiiaud Linux systemd Service Uninstallation ===");
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "disable", "--now", "kiiaud"])
        .status();

    if let Ok(home) = std::env::var("HOME") {
        let unit_file = std::path::PathBuf::from(home).join(".config/systemd/user/kiiaud.service");
        if unit_file.exists() {
            let _ = std::fs::remove_file(&unit_file);
            println!("Removed unit file: {}", unit_file.display());
        }
    }

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();

    println!("[OK] Successfully uninstalled kiiaud systemd user service.");
    Ok(())
}

#[cfg(target_os = "linux")]
fn status_service() -> Result<()> {
    println!("=== kiiaud Linux systemd Service Status ===");
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "status", "kiiaud"])
        .status();
    Ok(())
}

#[cfg(target_os = "linux")]
fn start_service() -> Result<()> {
    println!("Starting kiiaud systemd user service...");
    let status = std::process::Command::new("systemctl")
        .args(["--user", "start", "kiiaud"])
        .status()
        .context("Failed to execute systemctl")?;
    if !status.success() {
        bail!("Failed to start service");
    }
    println!("[OK] Started kiiaud service.");
    Ok(())
}

#[cfg(target_os = "linux")]
fn stop_service() -> Result<()> {
    println!("Stopping kiiaud systemd user service...");
    let status = std::process::Command::new("systemctl")
        .args(["--user", "stop", "kiiaud"])
        .status()
        .context("Failed to execute systemctl")?;
    if !status.success() {
        bail!("Failed to stop service");
    }
    println!("[OK] Stopped kiiaud service.");
    Ok(())
}

// -----------------------------------------------------------------------------
// Fallback for Other Platforms
// -----------------------------------------------------------------------------
#[cfg(all(not(windows), not(target_os = "linux")))]
fn install_service() -> Result<()> {
    bail!(
        "Service installation is currently supported on Windows (Task Scheduler) and Linux (systemd)."
    );
}
#[cfg(all(not(windows), not(target_os = "linux")))]
fn uninstall_service() -> Result<()> {
    bail!("Service uninstallation is currently supported on Windows and Linux.");
}
#[cfg(all(not(windows), not(target_os = "linux")))]
fn status_service() -> Result<()> {
    bail!("Service status inspection is currently supported on Windows and Linux.");
}
#[cfg(all(not(windows), not(target_os = "linux")))]
fn start_service() -> Result<()> {
    bail!("Starting service is currently supported on Windows and Linux.");
}
#[cfg(all(not(windows), not(target_os = "linux")))]
fn stop_service() -> Result<()> {
    bail!("Stopping service is currently supported on Windows and Linux.");
}
