// SPDX-License-Identifier: MIT OR Apache-2.0

//! Lightweight embedded Hi-Fi WebUI & REST/SSE control adapter (`kiiaud-adapter-web`).
//!
//! Features:
//! - Single-binary embedded SPA (`ui.html`) on default port `7110` (`KII-0`), zero external runtime files or disk writes during playback.
//! - Real-time Server-Sent Events (`/api/events`) subscribed to `PlaybackEvent` broadcast channel.
//! - Audiophile Signal Path inspection (`Source -> Symphonia Decoder -> Bit-Perfect Pipeline -> ALSA Hardware`).
//! - Runtime Queue management & zero-database local filesystem audio browser (`/api/browse`).
//! - ALSA hardware playback device scanner (`/proc/asound/cards` & `/proc/asound/pcm`) and `kiiaud.toml` configuration editor (`/api/config`).

use kiiaud_core::{
    AudioFormat, AudioSource, KiiaudConfig, KiiaudError, PathPolicy, PlaybackCommand, PlaybackItem,
    PlayerStatus, Result,
};
use kiiaud_playback::EngineHandle;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

const INDEX_HTML: &str = include_str!("ui.html");

const AUDIO_EXTENSIONS: &[&str] = &[
    "wv", "flac", "dsf", "dff", "wav", "aif", "aiff", "mp3", "m4a", "aac", "ogg", "opus",
];

#[derive(Clone)]
pub struct WebServerContext {
    pub engine: EngineHandle,
    pub config: Arc<RwLock<KiiaudConfig>>,
    pub config_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebStateSnapshot {
    pub status: PlayerStatus,
    pub elapsed_secs: f64,
    pub duration_secs: Option<f64>,
    pub format_badge: String,
    pub current_track: Option<WebQueueItem>,
    pub signal_path: SignalPathInfo,
    pub queue: Vec<WebQueueItem>,
    pub config: KiiaudConfig,
    pub alsa_devices: Vec<AlsaDeviceInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebQueueItem {
    pub id: u32,
    pub position: u32,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub uri: String,
    pub duration_secs: Option<f64>,
}

impl From<&PlaybackItem> for WebQueueItem {
    fn from(item: &PlaybackItem) -> Self {
        Self {
            id: item.id,
            position: item.position,
            title: item.display_title(),
            artist: item.artist.clone(),
            album: item.album.clone(),
            uri: item.source.display_uri(),
            duration_secs: item.duration.map(|d| d.as_secs_f64()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalPathInfo {
    pub source: String,
    pub decoder: String,
    pub dsp: String,
    pub output: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AlsaDeviceInfo {
    pub card_index: u32,
    pub dev_index: u32,
    pub card_id: String,
    pub device_id: String,
    pub name: String,
    pub is_usb: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ControlRequest {
    pub action: String,
    pub uri: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub id: Option<u32>,
    pub pos: Option<u32>,
    pub secs: Option<f64>,
    pub volume: Option<u8>,
    pub option: Option<String>,
    pub value: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowseEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowseResponse {
    pub path: String,
    pub entries: Vec<BrowseEntry>,
}

/// Start the embedded WebUI HTTP & SSE server on `listen_addr` (default `"0.0.0.0:7110"`).
pub async fn run_web_server(
    listen_addr: String,
    engine: EngineHandle,
    config: KiiaudConfig,
    config_path: Option<PathBuf>,
) -> Result<()> {
    let listener = TcpListener::bind(&listen_addr).await.map_err(|e| {
        KiiaudError::Io(format!(
            "Failed to bind WebUI server on '{listen_addr}': {e}"
        ))
    })?;

    tracing::info!(
        listen = %listen_addr,
        "kiiaud Hi-Fi WebUI & REST/SSE server listening (http://{listen_addr})"
    );

    let ctx = WebServerContext {
        engine,
        config: Arc::new(RwLock::new(config)),
        config_path,
    };

    loop {
        match listener.accept().await {
            Ok((stream, _peer)) => {
                let c = ctx.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_http_connection(stream, c).await {
                        tracing::debug!("WebUI HTTP connection closed: {e}");
                    }
                });
            }
            Err(e) => {
                tracing::warn!("WebUI accept error: {e}");
            }
        }
    }
}

pub fn build_state_snapshot(ctx: &WebServerContext) -> WebStateSnapshot {
    let st = ctx.engine.status();
    let cfg = ctx.config.read().clone();
    let queue_items: Vec<WebQueueItem> = ctx
        .engine
        .queue_items()
        .iter()
        .map(WebQueueItem::from)
        .collect();
    let current_track = st.current_item.as_ref().map(WebQueueItem::from);

    let format_badge = match st.format {
        Some(AudioFormat::Pcm {
            sample_rate,
            bits_per_sample,
            sample_format,
            ..
        }) => format!(
            "PCM {:.1}kHz / {}b ({})",
            sample_rate as f64 / 1000.0,
            bits_per_sample,
            sample_format
        ),
        Some(AudioFormat::Dsd {
            rate, transport, ..
        }) => format!("{rate} [{transport}]"),
        None => "IDLE".to_owned(),
    };

    let source_desc = match st.current_item.as_ref().map(|i| &i.source) {
        Some(AudioSource::LocalFile(p)) => {
            let ext = p
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("raw")
                .to_ascii_uppercase();
            format!("Local File ({ext}) • {}", p.display())
        }
        Some(AudioSource::HttpStream { url, .. }) => {
            format!("HTTP Direct Stream • {}", kiiaud_core::strip_uri_auth(url))
        }
        Some(AudioSource::TcpStream { host, port, .. }) => {
            format!("LMS SlimProto TCP Stream • {host}:{port}")
        }
        None => "Idle (Waiting for stream or local file)".to_owned(),
    };

    let decoder_desc = match st.format {
        Some(AudioFormat::Pcm {
            sample_rate,
            channels,
            sample_format,
            bits_per_sample,
        }) => format!(
            "PCM {sample_rate} Hz • {bits_per_sample}-bit -> {sample_format} • {channels}ch"
        ),
        Some(AudioFormat::Dsd {
            rate,
            channels,
            bit_order,
            layout,
            ..
        }) => format!(
            "{rate} ({:.4} MHz) • {channels}ch • {:?} / {:?}",
            rate.hz() as f64 / 1_000_000.0,
            bit_order,
            layout
        ),
        None => "No active decoder stream".to_owned(),
    };

    let dsp_desc = if let Some(caps) = &st.output_capabilities {
        if caps.bit_perfect {
            format!(
                "Bit-Perfect Passthrough ({} • Resampling OFF • Software Volume DSP OFF)",
                caps.backend_name
            )
        } else {
            format!(
                "Standard Output Path ({} • Shared Mixer • System Resampling Active)",
                caps.backend_name
            )
        }
    } else if cfg.output.bit_perfect {
        "Bit-Perfect Passthrough (ALSA Resampling OFF • Software Volume DSP OFF)".to_owned()
    } else {
        "Standard Pipeline (Bit-Perfect enforcement disabled in config)".to_owned()
    };

    let output_desc = match st.format {
        Some(AudioFormat::Dsd {
            rate, transport, ..
        }) => format!(
            "{} ({}) • {} {} • Buffer {}ms / Period {}ms",
            cfg.output.backend.to_ascii_uppercase(),
            cfg.output.device,
            rate,
            transport,
            cfg.output.buffer_time_ms,
            cfg.output.period_time_ms
        ),
        Some(AudioFormat::Pcm { sample_format, .. }) => format!(
            "{} ({}) • Hardware Format {} • Buffer {}ms / Period {}ms",
            cfg.output.backend.to_ascii_uppercase(),
            cfg.output.device,
            sample_format,
            cfg.output.buffer_time_ms,
            cfg.output.period_time_ms
        ),
        None => format!(
            "{} ({}) • Ready (DSD mode: {:?})",
            cfg.output.backend.to_ascii_uppercase(),
            cfg.output.device,
            cfg.output.dsd_mode
        ),
    };

    WebStateSnapshot {
        elapsed_secs: st.elapsed.as_secs_f64(),
        duration_secs: st.duration.map(|d| d.as_secs_f64()),
        status: st,
        format_badge,
        current_track,
        signal_path: SignalPathInfo {
            source: source_desc,
            decoder: decoder_desc,
            dsp: dsp_desc,
            output: output_desc,
        },
        queue: queue_items,
        config: cfg,
        alsa_devices: scan_alsa_devices(),
    }
}

async fn handle_http_connection(mut stream: TcpStream, ctx: WebServerContext) -> Result<()> {
    let (read_half, mut write_half) = stream.split();
    let mut reader = BufReader::new(read_half);

    let mut request_line = String::new();
    if reader.read_line(&mut request_line).await? == 0 {
        return Ok(());
    }

    let parts: Vec<&str> = request_line.trim().split_whitespace().collect();
    if parts.len() < 2 {
        return Ok(());
    }
    let method = parts[0];
    let raw_target = parts[1];
    let (path, query) = match raw_target.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (raw_target, None),
    };

    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((k, v)) = line.split_once(':')
            && k.trim().eq_ignore_ascii_case("content-length")
        {
            content_length = v.trim().parse::<usize>().unwrap_or(0);
        }
    }

    let mut body = vec![0u8; content_length.min(256 * 1024)];
    if content_length > 0 {
        reader.read_exact(&mut body).await?;
    }

    match (method, path) {
        ("GET", "/") | ("GET", "/index.html") => {
            write_response(
                &mut write_half,
                "200 OK",
                "text/html; charset=utf-8",
                INDEX_HTML.as_bytes(),
            )
            .await?;
        }
        ("GET", "/api/state") => {
            let snap = build_state_snapshot(&ctx);
            let json = serde_json::to_vec(&snap).unwrap_or_default();
            write_response(&mut write_half, "200 OK", "application/json", &json).await?;
        }
        ("GET", "/api/alsa-devices") => {
            let devs = scan_alsa_devices();
            let json = serde_json::to_vec(&devs).unwrap_or_default();
            write_response(&mut write_half, "200 OK", "application/json", &json).await?;
        }
        ("GET", "/api/dac") => {
            let st = ctx.engine.status();
            let caps = st.output_capabilities.unwrap_or_default();
            let json = serde_json::to_vec(&serde_json::json!({
                "capabilities": caps,
                "current_format": st.format.map(|f| f.to_string()),
                "bit_perfect": caps.bit_perfect,
            }))
            .unwrap_or_default();
            write_response(&mut write_half, "200 OK", "application/json", &json).await?;
        }
        ("GET", "/api/config") => {
            let cfg = ctx.config.read().clone();
            let json = serde_json::to_vec(&cfg).unwrap_or_default();
            write_response(&mut write_half, "200 OK", "application/json", &json).await?;
        }
        ("POST", "/api/config") => match serde_json::from_slice::<KiiaudConfig>(&body) {
            Ok(new_cfg) => {
                *ctx.config.write() = new_cfg.clone();
                let msg = if let Some(ref p) = ctx.config_path {
                    match new_cfg.save_to_file(p) {
                        Ok(()) => format!(
                            "Saved configuration to '{}'. Restart kiiaud if hardware/adapter ports changed.",
                            p.display()
                        ),
                        Err(e) => {
                            let err_json = serde_json::json!({ "error": e.to_string() });
                            write_response(
                                &mut write_half,
                                "500 Internal Server Error",
                                "application/json",
                                err_json.to_string().as_bytes(),
                            )
                            .await?;
                            return Ok(());
                        }
                    }
                } else {
                    "Updated runtime configuration in memory.".to_owned()
                };
                let ok_json = serde_json::json!({ "ok": true, "message": msg });
                write_response(
                    &mut write_half,
                    "200 OK",
                    "application/json",
                    ok_json.to_string().as_bytes(),
                )
                .await?;
            }
            Err(e) => {
                let err_json = serde_json::json!({ "error": format!("Invalid config JSON: {e}") });
                write_response(
                    &mut write_half,
                    "400 Bad Request",
                    "application/json",
                    err_json.to_string().as_bytes(),
                )
                .await?;
            }
        },
        ("POST", "/api/control") => {
            let resp = match serde_json::from_slice::<ControlRequest>(&body) {
                Ok(req) => handle_control_request(req, &ctx.engine).await,
                Err(e) => Err(KiiaudError::Config(format!("Invalid control JSON: {e}"))),
            };
            match resp {
                Ok(()) => {
                    write_response(
                        &mut write_half,
                        "200 OK",
                        "application/json",
                        br#"{"ok":true}"#,
                    )
                    .await?;
                }
                Err(e) => {
                    let err_json = serde_json::json!({ "error": e.to_string() });
                    write_response(
                        &mut write_half,
                        "400 Bad Request",
                        "application/json",
                        err_json.to_string().as_bytes(),
                    )
                    .await?;
                }
            }
        }
        ("GET", "/api/browse") => {
            let requested_path =
                parse_query_param(query.unwrap_or(""), "path").unwrap_or_else(|| "/".to_owned());
            let allowed_roots = ctx.config.read().security.allowed_roots.clone();
            match browse_directory(&requested_path, &allowed_roots) {
                Ok(res) => {
                    let json = serde_json::to_vec(&res).unwrap_or_default();
                    write_response(&mut write_half, "200 OK", "application/json", &json).await?;
                }
                Err(e) => {
                    let err_json = serde_json::json!({ "error": e.to_string() });
                    write_response(
                        &mut write_half,
                        "400 Bad Request",
                        "application/json",
                        err_json.to_string().as_bytes(),
                    )
                    .await?;
                }
            }
        }
        ("GET", "/api/events") => {
            let headers = concat!(
                "HTTP/1.1 200 OK\r\n",
                "Content-Type: text/event-stream\r\n",
                "Cache-Control: no-cache\r\n",
                "Connection: keep-alive\r\n",
                "Access-Control-Allow-Origin: *\r\n\r\n"
            );
            write_half.write_all(headers.as_bytes()).await?;

            let mut rx = ctx.engine.subscribe();
            let initial = build_state_snapshot(&ctx);
            if let Ok(json) = serde_json::to_string(&initial) {
                let chunk = format!("data: {json}\n\n");
                write_half.write_all(chunk.as_bytes()).await?;
                write_half.flush().await?;
            }

            loop {
                match tokio::time::timeout(Duration::from_secs(15), rx.recv()).await {
                    Ok(Ok(_ev)) => {
                        let snap = build_state_snapshot(&ctx);
                        if let Ok(json) = serde_json::to_string(&snap) {
                            let chunk = format!("data: {json}\n\n");
                            if write_half.write_all(chunk.as_bytes()).await.is_err() {
                                break;
                            }
                            let _ = write_half.flush().await;
                        }
                    }
                    Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                    Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
                    Err(_) => {
                        // Send heartbeat comment every 15s to keep SSE connection alive
                        if write_half.write_all(b": keepalive\n\n").await.is_err() {
                            break;
                        }
                        let _ = write_half.flush().await;
                    }
                }
            }
        }
        _ => {
            write_response(
                &mut write_half,
                "404 Not Found",
                "application/json",
                br#"{"error":"Not Found"}"#,
            )
            .await?;
        }
    }

    Ok(())
}

async fn write_response(
    writer: &mut tokio::net::tcp::WriteHalf<'_>,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> Result<()> {
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
        body.len()
    );
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(body).await?;
    writer.flush().await?;
    Ok(())
}

async fn handle_control_request(req: ControlRequest, engine: &EngineHandle) -> Result<()> {
    match req.action.as_str() {
        "play_uri" => {
            let uri = req
                .uri
                .ok_or_else(|| KiiaudError::Config("Missing 'uri' for play_uri".to_owned()))?;
            let source = AudioSource::from_uri_or_path(&uri);
            engine
                .send(PlaybackCommand::Load {
                    source,
                    title: req.title,
                    artist: req.artist,
                    autostart: true,
                })
                .await
        }
        "enqueue" => {
            let uri = req
                .uri
                .ok_or_else(|| KiiaudError::Config("Missing 'uri' for enqueue".to_owned()))?;
            let source = AudioSource::from_uri_or_path(&uri);
            engine
                .send(PlaybackCommand::Enqueue {
                    source,
                    title: req.title,
                    artist: req.artist,
                })
                .await
        }
        "play" => engine.send(PlaybackCommand::Play(req.pos)).await,
        "play_id" => {
            let id = req
                .id
                .ok_or_else(|| KiiaudError::Config("Missing 'id' for play_id".to_owned()))?;
            engine.send(PlaybackCommand::PlayId(id)).await
        }
        "pause" => engine.send(PlaybackCommand::Pause).await,
        "resume" => engine.send(PlaybackCommand::Resume).await,
        "toggle" => engine.send(PlaybackCommand::TogglePause).await,
        "stop" => engine.send(PlaybackCommand::Stop).await,
        "next" => engine.send(PlaybackCommand::Next).await,
        "prev" => engine.send(PlaybackCommand::Previous).await,
        "clear" => engine.send(PlaybackCommand::Clear).await,
        "remove_id" => {
            let id = req
                .id
                .ok_or_else(|| KiiaudError::Config("Missing 'id' for remove_id".to_owned()))?;
            engine.send(PlaybackCommand::RemoveId(id)).await
        }
        "remove_pos" => {
            let pos = req
                .pos
                .ok_or_else(|| KiiaudError::Config("Missing 'pos' for remove_pos".to_owned()))?;
            engine.send(PlaybackCommand::RemovePos(pos)).await
        }
        "seek" => {
            let secs = req.secs.unwrap_or(0.0).max(0.0);
            engine
                .send(PlaybackCommand::Seek(Duration::from_secs_f64(secs)))
                .await
        }
        "volume" => {
            let vol = req.volume.unwrap_or(100).min(100);
            engine.send(PlaybackCommand::SetVolume(vol)).await
        }
        "set_option" => {
            let opt = req.option.unwrap_or_default();
            let val = req.value.unwrap_or(false);
            match opt.as_str() {
                "repeat" => engine.send(PlaybackCommand::SetRepeat(val)).await,
                "random" => engine.send(PlaybackCommand::SetRandom(val)).await,
                "single" => engine.send(PlaybackCommand::SetSingle(val)).await,
                "consume" => engine.send(PlaybackCommand::SetConsume(val)).await,
                other => Err(KiiaudError::Config(format!("Unknown option '{other}'"))),
            }
        }
        other => Err(KiiaudError::Config(format!(
            "Unsupported control action '{other}'"
        ))),
    }
}

/// Scan `/proc/asound/cards` and `/proc/asound/pcm` on Linux to discover ALSA hardware playback devices.
pub fn scan_alsa_devices() -> Vec<AlsaDeviceInfo> {
    let cards_txt = std::fs::read_to_string("/proc/asound/cards").unwrap_or_default();
    let pcm_txt = std::fs::read_to_string("/proc/asound/pcm").unwrap_or_default();
    parse_proc_asound(&cards_txt, &pcm_txt)
}

/// Parse `/proc/asound/cards` and `/proc/asound/pcm` text into structured `AlsaDeviceInfo` entries.
pub fn parse_proc_asound(cards_txt: &str, pcm_txt: &str) -> Vec<AlsaDeviceInfo> {
    // Map card_index -> (card_id, card_description, is_usb)
    let mut cards: HashMap<u32, (String, String, bool)> = HashMap::new();

    for line in cards_txt.lines() {
        let trimmed = line.trim();
        // Format: ` 7 [Audio          ]: USB-Audio - Digital Hifi Audio`
        if let Some(bracket_start) = trimmed.find('[')
            && let Some(bracket_end) = trimmed.find("]:")
            && bracket_start < bracket_end
        {
            let idx_str = trimmed[..bracket_start].trim();
            if let Ok(card_idx) = idx_str.parse::<u32>() {
                let card_id = trimmed[bracket_start + 1..bracket_end].trim().to_owned();
                let desc = trimmed[bracket_end + 2..].trim().to_owned();
                let is_usb = desc.to_ascii_lowercase().contains("usb");
                cards.insert(card_idx, (card_id, desc, is_usb));
            }
        }
    }

    let mut devices = Vec::new();
    for line in pcm_txt.lines() {
        // Format: `07-00: USB Audio : USB Audio : playback 1`
        if !line.contains("playback") {
            continue;
        }
        let parts: Vec<&str> = line.split(':').map(str::trim).collect();
        if parts.len() < 2 {
            continue;
        }
        if let Some((c_str, d_str)) = parts[0].split_once('-')
            && let (Ok(c_idx), Ok(d_idx)) = (c_str.parse::<u32>(), d_str.parse::<u32>())
        {
            let pcm_name = parts[1];
            let (card_id, card_desc, is_usb) = cards
                .get(&c_idx)
                .cloned()
                .unwrap_or_else(|| (c_idx.to_string(), pcm_name.to_owned(), false));
            let device_id = format!("hw:CARD={card_id},DEV={d_idx}");
            let name = format!("{card_desc} ({pcm_name})");
            devices.push(AlsaDeviceInfo {
                card_index: c_idx,
                dev_index: d_idx,
                card_id,
                device_id,
                name,
                is_usb,
            });
        }
    }

    // Sort USB DACs first, then by card_index and dev_index
    devices.sort_by(|a, b| {
        b.is_usb
            .cmp(&a.is_usb)
            .then_with(|| a.card_index.cmp(&b.card_index))
            .then_with(|| a.dev_index.cmp(&b.dev_index))
    });
    devices
}

fn browse_directory(raw_path: &str, allowed_roots: &[PathBuf]) -> Result<BrowseResponse> {
    let target = if raw_path.trim().is_empty() {
        allowed_roots
            .first()
            .cloned()
            .unwrap_or_else(|| PathBuf::from("/"))
    } else {
        PathBuf::from(raw_path.trim())
    };

    let canonical = std::fs::canonicalize(&target).map_err(|e| {
        KiiaudError::Io(format!(
            "Cannot access directory '{}': {e}",
            target.display()
        ))
    })?;

    if !allowed_roots.is_empty() {
        let policy = PathPolicy::new(allowed_roots.to_vec());
        // Verify canonical directory is within one of allowed_roots
        let mut inside_allowed = false;
        for root in allowed_roots {
            if let Ok(canon_root) = std::fs::canonicalize(root)
                && canonical.starts_with(&canon_root)
            {
                inside_allowed = true;
                break;
            }
        }
        if !inside_allowed {
            // Also allow if policy validates it
            let _ = policy;
            return Err(KiiaudError::SecurityViolation(format!(
                "Directory '{}' is outside configured allowed_roots",
                canonical.display()
            )));
        }
    }

    let read_dir = std::fs::read_dir(&canonical).map_err(|e| {
        KiiaudError::Io(format!(
            "Failed to read directory '{}': {e}",
            canonical.display()
        ))
    })?;

    let mut dirs = Vec::new();
    let mut files = Vec::new();

    for entry in read_dir.flatten() {
        let path = entry.path();
        let Some(name_os) = path.file_name() else {
            continue;
        };
        let name = name_os.to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if let Ok(ft) = entry.file_type() {
            if ft.is_dir() {
                dirs.push(BrowseEntry {
                    name,
                    path: path.to_string_lossy().into_owned(),
                    is_dir: true,
                });
            } else if ft.is_file() && is_supported_audio_file(&path) {
                files.push(BrowseEntry {
                    name,
                    path: path.to_string_lossy().into_owned(),
                    is_dir: false,
                });
            }
        }
    }

    dirs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    dirs.extend(files);

    Ok(BrowseResponse {
        path: canonical.to_string_lossy().into_owned(),
        entries: dirs,
    })
}

fn is_supported_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            AUDIO_EXTENSIONS
                .iter()
                .any(|&allowed| allowed.eq_ignore_ascii_case(ext))
        })
}

fn parse_query_param(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=')
            && k == key
        {
            return Some(percent_decode(v));
        }
    }
    None
}

fn percent_decode(input: &str) -> String {
    let mut out = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (from_hex(bytes[i + 1]), from_hex(bytes[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        } else if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_proc_asound_prioritizes_usb_dac() {
        let cards = "\
 0 [vc4hdmi0       ]: vc4-hdmi - vc4-hdmi-0
                      vc4-hdmi-0
 1 [vc4hdmi1       ]: vc4-hdmi - vc4-hdmi-1
                      vc4-hdmi-1
 7 [Audio          ]: USB-Audio - Digital Hifi Audio
                      C-Media Electronics Inc. Digital Hifi Audio at usb-xhci-hcd.0-1, high speed
";
        let pcm = "\
00-00: MAI PCM i2s-hifi-0 : MAI PCM i2s-hifi-0 : playback 1
01-00: MAI PCM i2s-hifi-0 : MAI PCM i2s-hifi-0 : playback 1
07-00: USB Audio : USB Audio : playback 1
";
        let devs = parse_proc_asound(cards, pcm);
        assert_eq!(devs.len(), 3);
        assert_eq!(devs[0].device_id, "hw:CARD=Audio,DEV=0");
        assert!(devs[0].is_usb);
        assert!(devs[0].name.contains("Digital Hifi Audio"));
        assert_eq!(devs[1].device_id, "hw:CARD=vc4hdmi0,DEV=0");
    }

    #[test]
    fn test_percent_decode() {
        assert_eq!(
            percent_decode("%2Fmusic%2FDSD%20Album%2F01.wv"),
            "/music/DSD Album/01.wv"
        );
    }
}
