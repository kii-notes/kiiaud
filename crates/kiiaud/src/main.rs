// SPDX-License-Identifier: MIT OR Apache-2.0

//! `kiiaud` — Lightweight, headless, database-free network audio playback daemon.
//!
//! Coordinates the playback engine (`kiiaud-playback`), ALSA hardware-direct output (`kiiaud-output-alsa`),
//! and modular protocol adapters (`kiiaud-adapter-cli`, `kiiaud-adapter-mpd`, `kiiaud-adapter-jellyfin`, `kiiaud-adapter-lms`).

use clap::Parser;
use kiiaud_adapter_cli::run_ipc_server;
use kiiaud_adapter_jellyfin::run_jellyfin_adapter;
use kiiaud_adapter_lms::run_lms_adapter;
use kiiaud_adapter_mpd::run_mpd_server_with_dir;
use kiiaud_adapter_web::run_web_server;
use kiiaud_core::{AudioSource, DsdMode, KiiaudConfig, PlaybackCommand};
use kiiaud_playback::PlaybackEngine;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

fn parse_mac_address(s: &str) -> std::result::Result<[u8; 6], String> {
    let clean = s.trim();
    let parts: Vec<&str> = if clean.contains(':') {
        clean.split(':').collect()
    } else if clean.contains('-') {
        clean.split('-').collect()
    } else if clean.contains(',') {
        clean.split(',').collect()
    } else if clean.len() == 12 {
        let mut out = [0u8; 6];
        for i in 0..6 {
            out[i] = u8::from_str_radix(&clean[i * 2..i * 2 + 2], 16)
                .map_err(|e| format!("Invalid hex byte in MAC address: {e}"))?;
        }
        return Ok(out);
    } else {
        return Err("MAC address must be in format '02:4B:49:49:41:55', '02-4B-49-49-41-55', or 12 hex digits".to_owned());
    };

    if parts.len() != 6 {
        return Err(format!("MAC address must have 6 octets, found {}", parts.len()));
    }

    let mut out = [0u8; 6];
    for (i, p) in parts.iter().enumerate() {
        let p_trimmed = p.trim();
        out[i] = u8::from_str_radix(p_trimmed, 16)
            .or_else(|_| p_trimmed.parse::<u8>())
            .map_err(|_| format!("Invalid octet '{p_trimmed}' in MAC address"))?;
    }
    Ok(out)
}

#[derive(Parser)]
#[command(
    name = "kiiaud",
    version,
    about = "Lightweight, headless, database-free bit-perfect network audio playback daemon"
)]
struct Args {
    // -------------------------------------------------------------------------
    // Configuration Discovery & Management
    // -------------------------------------------------------------------------
    /// Explicit path to configuration file (overrides hierarchy auto-discovery).
    #[arg(short, long, help_heading = "Configuration")]
    config: Option<PathBuf>,

    /// Run in system-wide mode (strictly loads /etc/kiiaud/config.toml, ignoring per-user config).
    #[arg(long, help_heading = "Configuration")]
    system: bool,

    /// Generate a well-commented default configuration file at [PATH] and exit.
    /// If [PATH] is omitted, defaults to ~/.config/kiiaud/config.toml (or /etc/kiiaud/config.toml with --system).
    #[arg(long, value_name = "PATH", num_args = 0..=1, help_heading = "Configuration")]
    generate_config: Option<Option<PathBuf>>,

    /// Overwrite destination file when using --generate-config.
    #[arg(long, help_heading = "Configuration")]
    force: bool,

    /// Validate the resolved configuration and exit with status code 0 (valid) or 1 (invalid).
    #[arg(long, help_heading = "Configuration")]
    check_config: bool,

    /// Print resolved configuration (in TOML format) to standard output and exit.
    #[arg(long, help_heading = "Configuration")]
    print_config: bool,

    /// Do not automatically generate a default configuration file if none is found on first run.
    #[arg(long, help_heading = "Configuration")]
    no_auto_generate: bool,

    /// Optional initial file or URL to play immediately on startup.
    #[arg(short, long, help_heading = "Playback")]
    play: Option<String>,

    // -------------------------------------------------------------------------
    // Daemon Options (Overrides [daemon])
    // -------------------------------------------------------------------------
    /// Override friendly daemon/endpoint name.
    #[arg(long, help_heading = "Daemon Options")]
    name: Option<String>,

    /// Override Unix domain socket path for CLI IPC control (`kiiaudctl`).
    #[arg(long, help_heading = "Daemon Options")]
    socket_path: Option<PathBuf>,

    /// Override log verbosity level (`trace`, `debug`, `info`, `warn`, `error`).
    #[arg(short = 'l', long, help_heading = "Daemon Options")]
    log_level: Option<String>,

    // -------------------------------------------------------------------------
    // Audio Output & Bit-Perfect Options (Overrides [output])
    // -------------------------------------------------------------------------
    /// Override output backend (`alsa`, `wasapi`, `android` / `oboe`, or `mock`).
    #[arg(short, long, help_heading = "Audio Output Options")]
    backend: Option<String>,

    /// Override audio output device (e.g. `hw:CARD=Audio,DEV=0` on Linux, or device name on Windows).
    #[arg(short, long, help_heading = "Audio Output Options")]
    device: Option<String>,

    /// Enforce bit-perfect hardware output (`true` or `false`).
    #[arg(
        long,
        value_name = "BOOL",
        num_args = 0..=1,
        default_missing_value = "true",
        help_heading = "Audio Output Options"
    )]
    bit_perfect: Option<bool>,

    /// Override DSD transport mode (`auto`, `native`, or `dop`).
    #[arg(long, help_heading = "Audio Output Options")]
    dsd_mode: Option<DsdMode>,

    /// Override hardware ring buffer size in milliseconds.
    #[arg(long, help_heading = "Audio Output Options")]
    buffer_time_ms: Option<u32>,

    /// Override ALSA period time (hardware interrupt interval) in milliseconds.
    #[arg(long, help_heading = "Audio Output Options")]
    period_time_ms: Option<u32>,

    /// Override hardware mixer control name (e.g. `PCM`, `Master`, or `none` to disable).
    #[arg(long, help_heading = "Audio Output Options")]
    hardware_mixer: Option<String>,

    // -------------------------------------------------------------------------
    // Security Options (Overrides [security])
    // -------------------------------------------------------------------------
    /// Allowed filesystem root directory for local playback (can be specified multiple times).
    #[arg(long = "allowed-root", help_heading = "Security Options")]
    allowed_root: Option<Vec<PathBuf>>,

    // -------------------------------------------------------------------------
    // WebUI & REST Adapter Options (Overrides [adapter.web])
    // -------------------------------------------------------------------------
    /// Enable or disable embedded WebUI & REST server (`true` or `false`).
    #[arg(
        long,
        value_name = "BOOL",
        num_args = 0..=1,
        default_missing_value = "true",
        help_heading = "WebUI Adapter Options"
    )]
    web: Option<bool>,

    /// Override WebUI server listen address (e.g. `0.0.0.0:7110`).
    #[arg(long, help_heading = "WebUI Adapter Options")]
    web_listen: Option<String>,

    // -------------------------------------------------------------------------
    // MPD Adapter Options (Overrides [adapter.mpd])
    // -------------------------------------------------------------------------
    /// Enable or disable MPD protocol server (`true` or `false`).
    #[arg(
        long,
        value_name = "BOOL",
        num_args = 0..=1,
        default_missing_value = "true",
        help_heading = "MPD Adapter Options"
    )]
    mpd: Option<bool>,

    /// Override MPD server listen address (e.g. `0.0.0.0:6600`).
    #[arg(long, help_heading = "MPD Adapter Options")]
    mpd_listen: Option<String>,

    // -------------------------------------------------------------------------
    // Jellyfin Adapter Options (Overrides [adapter.jellyfin])
    // -------------------------------------------------------------------------
    /// Enable or disable Jellyfin Remote Control Endpoint adapter (`true` or `false`).
    #[arg(
        long,
        value_name = "BOOL",
        num_args = 0..=1,
        default_missing_value = "true",
        help_heading = "Jellyfin Adapter Options"
    )]
    jellyfin: Option<bool>,

    /// Override Jellyfin server URL (e.g. `http://jellyfin.local:8096`).
    #[arg(long, help_heading = "Jellyfin Adapter Options")]
    jellyfin_url: Option<String>,

    /// Override Jellyfin API key.
    #[arg(long, help_heading = "Jellyfin Adapter Options")]
    jellyfin_api_key: Option<String>,

    /// Override Jellyfin login username.
    #[arg(long, help_heading = "Jellyfin Adapter Options")]
    jellyfin_username: Option<String>,

    /// Override Jellyfin login password.
    #[arg(long, help_heading = "Jellyfin Adapter Options")]
    jellyfin_password: Option<String>,

    /// Override Jellyfin client device ID.
    #[arg(long, help_heading = "Jellyfin Adapter Options")]
    jellyfin_device_id: Option<String>,

    /// Override Jellyfin client display name in Cast menu.
    #[arg(long, help_heading = "Jellyfin Adapter Options")]
    jellyfin_device_name: Option<String>,

    // -------------------------------------------------------------------------
    // LMS / SlimProto Adapter Options (Overrides [adapter.lms])
    // -------------------------------------------------------------------------
    /// Enable or disable LMS (SlimProto) client adapter (`true` or `false`).
    #[arg(
        long,
        value_name = "BOOL",
        num_args = 0..=1,
        default_missing_value = "true",
        help_heading = "LMS Adapter Options"
    )]
    lms: Option<bool>,

    /// Override LMS server host IP or hostname.
    #[arg(long, help_heading = "LMS Adapter Options")]
    lms_host: Option<String>,

    /// Override LMS server SlimProto port (default 3483).
    #[arg(long, help_heading = "LMS Adapter Options")]
    lms_port: Option<u16>,

    /// Override LMS server HTTP audio streaming port (default 9000).
    #[arg(long, help_heading = "LMS Adapter Options")]
    lms_http_port: Option<u16>,

    /// Override LMS player display name.
    #[arg(long, help_heading = "LMS Adapter Options")]
    lms_player_name: Option<String>,

    /// Override LMS virtual MAC address (e.g. `02:4B:49:49:41:55`).
    #[arg(long, value_parser = parse_mac_address, help_heading = "LMS Adapter Options")]
    lms_mac: Option<[u8; 6]>,
}

impl Args {
    /// Apply command-line flags to override any corresponding configuration settings.
    /// Command-line arguments always take highest precedence over all configuration files.
    pub fn apply_to_config(&self, config: &mut KiiaudConfig) {
        // [daemon] overrides
        if let Some(ref n) = self.name {
            config.daemon.name = n.clone();
        }
        if let Some(ref sock) = self.socket_path {
            config.daemon.socket_path = sock.clone();
        }
        if let Some(ref lvl) = self.log_level {
            config.daemon.log_level = lvl.clone();
        }

        // [output] overrides
        if let Some(ref b) = self.backend {
            config.output.backend = b.clone();
        }
        if let Some(ref d) = self.device {
            config.output.device = d.clone();
        }
        if let Some(bp) = self.bit_perfect {
            config.output.bit_perfect = bp;
        }
        if let Some(dsd) = self.dsd_mode {
            config.output.dsd_mode = dsd;
        }
        if let Some(buf) = self.buffer_time_ms {
            config.output.buffer_time_ms = buf;
        }
        if let Some(p) = self.period_time_ms {
            config.output.period_time_ms = p;
        }
        if let Some(ref mixer) = self.hardware_mixer {
            if mixer.eq_ignore_ascii_case("none") || mixer.is_empty() {
                config.output.hardware_mixer = None;
            } else {
                config.output.hardware_mixer = Some(mixer.clone());
            }
        }

        // [security] overrides
        if let Some(ref roots) = self.allowed_root {
            config.security.allowed_roots = roots.clone();
        }

        // [adapter.web] overrides
        if let Some(web_en) = self.web {
            config.adapter.web.enabled = web_en;
        }
        if let Some(ref listen) = self.web_listen {
            config.adapter.web.listen = listen.clone();
        }

        // [adapter.mpd] overrides
        if let Some(mpd_en) = self.mpd {
            config.adapter.mpd.enabled = mpd_en;
        }
        if let Some(ref listen) = self.mpd_listen {
            config.adapter.mpd.listen = listen.clone();
        }

        // [adapter.jellyfin] overrides
        if let Some(jf_en) = self.jellyfin {
            config.adapter.jellyfin.enabled = jf_en;
        }
        if let Some(ref url) = self.jellyfin_url {
            config.adapter.jellyfin.server_url = url.clone();
        }
        if let Some(ref key) = self.jellyfin_api_key {
            config.adapter.jellyfin.api_key = Some(key.clone());
        }
        if let Some(ref u) = self.jellyfin_username {
            config.adapter.jellyfin.username = Some(u.clone());
        }
        if let Some(ref p) = self.jellyfin_password {
            config.adapter.jellyfin.password = Some(p.clone());
        }
        if let Some(ref id) = self.jellyfin_device_id {
            config.adapter.jellyfin.device_id = id.clone();
        }
        if let Some(ref name) = self.jellyfin_device_name {
            config.adapter.jellyfin.device_name = name.clone();
        }

        // [adapter.lms] overrides
        if let Some(lms_en) = self.lms {
            config.adapter.lms.enabled = lms_en;
        }
        if let Some(ref host) = self.lms_host {
            config.adapter.lms.server_host = host.clone();
        }
        if let Some(port) = self.lms_port {
            config.adapter.lms.server_port = port;
        }
        if let Some(http) = self.lms_http_port {
            config.adapter.lms.http_port = http;
        }
        if let Some(ref name) = self.lms_player_name {
            config.adapter.lms.player_name = name.clone();
        }
        if let Some(mac) = self.lms_mac {
            config.adapter.lms.mac_address = mac;
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Handle --generate-config command
    if let Some(target_opt) = args.generate_config {
        let dest = match target_opt {
            Some(path) => path,
            None => {
                if args.system {
                    KiiaudConfig::system_config_path()
                } else {
                    KiiaudConfig::user_config_path()
                        .unwrap_or_else(|| PathBuf::from("kiiaud.toml"))
                }
            }
        };

        KiiaudConfig::generate_default_config_file(&dest, args.force)?;
        println!("Successfully generated default configuration file at:");
        println!("  {}", dest.display());
        println!("Edit this file to customize your DAC device, bit-perfect settings, or protocol adapters.");
        return Ok(());
    }

    // Resolve configuration following hierarchy:
    // explicit --config > system-only (--system) > merged (system + user) > ./kiiaud.toml > auto-generate user config
    let (mut config, source) = KiiaudConfig::resolve_config(
        args.config.as_deref(),
        args.system,
        !args.no_auto_generate,
    )?;

    // Command-line options take ultimate precedence over any configuration files
    args.apply_to_config(&mut config);

    // Handle --check-config command
    if args.check_config {
        println!("Configuration status: OK");
        println!("Resolved source:    {}", source);
        println!("Daemon name:        {}", config.daemon.name);
        println!("IPC socket:         {}", config.daemon.socket_path.display());
        println!("Output backend:     {}", config.output.backend);
        println!("Audio device:       {}", config.output.device);
        println!("Bit-perfect:        {}", config.output.bit_perfect);
        println!("DSD transport:      {:?}", config.output.dsd_mode);
        println!(
            "Adapters:           WebUI: {} ({}) | MPD: {} ({}) | Jellyfin: {} | LMS: {}",
            if config.adapter.web.enabled { "on" } else { "off" },
            config.adapter.web.listen,
            if config.adapter.mpd.enabled { "on" } else { "off" },
            config.adapter.mpd.listen,
            if config.adapter.jellyfin.enabled { "on" } else { "off" },
            if config.adapter.lms.enabled { "on" } else { "off" }
        );
        return Ok(());
    }

    // Handle --print-config command
    if args.print_config {
        println!("{}", config.to_toml_string()?);
        return Ok(());
    }

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&config.daemon.log_level));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    tracing::info!(
        source = %source,
        name = %config.daemon.name,
        backend = %config.output.backend,
        device = %config.output.device,
        bit_perfect = config.output.bit_perfect,
        dsd_mode = ?config.output.dsd_mode,
        "starting kiiaud audio playback endpoint"
    );

    let engine = PlaybackEngine::new(&config);
    let handle = engine.handle();

    // Spawn core playback engine loop
    tokio::spawn(async move {
        engine.run().await;
    });

    // Spawn Local CLI / IPC Unix socket server (always active)
    {
        let sock_path = config.daemon.socket_path.clone();
        let h = handle.clone();
        tokio::spawn(async move {
            if let Err(e) = run_ipc_server(sock_path, h).await {
                tracing::error!("CLI IPC server error: {e}");
            }
        });
    }

    // Spawn MPD protocol adapter if enabled
    if config.adapter.mpd.enabled {
        let listen = config.adapter.mpd.listen.clone();
        let music_dir = config.adapter.mpd.music_directory.clone();
        let h = handle.clone();
        tokio::spawn(async move {
            if let Err(e) = run_mpd_server_with_dir(listen, music_dir, h).await {
                tracing::error!("MPD adapter error: {e}");
            }
        });
    }

    // Spawn Jellyfin "Play To" adapter if enabled
    if config.adapter.jellyfin.enabled {
        let jf_cfg = config.adapter.jellyfin.clone();
        let h = handle.clone();
        tokio::spawn(async move {
            if let Err(e) = run_jellyfin_adapter(jf_cfg, h).await {
                tracing::error!("Jellyfin adapter error: {e}");
            }
        });
    }

    // Spawn LMS / Lyrion SlimProto adapter if enabled
    if config.adapter.lms.enabled {
        let lms_cfg = config.adapter.lms.clone();
        let h = handle.clone();
        tokio::spawn(async move {
            if let Err(e) = run_lms_adapter(lms_cfg, h).await {
                tracing::error!("LMS adapter error: {e}");
            }
        });
    }

    // Spawn Embedded Hi-Fi WebUI & REST/SSE server if enabled
    if config.adapter.web.enabled {
        let listen = config.adapter.web.listen.clone();
        let h = handle.clone();
        let cfg_clone = config.clone();
        let cfg_path = source.effective_save_path();
        tokio::spawn(async move {
            if let Err(e) = run_web_server(listen, h, cfg_clone, cfg_path).await {
                tracing::error!("WebUI adapter error: {e}");
            }
        });
    }

    // If `--play <path_or_url>` was passed on CLI, start playing immediately
    if let Some(target) = args.play {
        let source = AudioSource::from_uri_or_path(&target);
        let _ = handle
            .send(PlaybackCommand::Load {
                source,
                title: None,
                artist: None,
                autostart: true,
            })
            .await;
    }

    // Wait for SIGINT / Ctrl+C
    tokio::signal::ctrl_c().await?;
    tracing::info!("received shutdown signal; stopping playback cleanly");
    let _ = handle.send(PlaybackCommand::Stop).await;

    Ok(())
}
