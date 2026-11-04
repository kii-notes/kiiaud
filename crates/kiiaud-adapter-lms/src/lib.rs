// SPDX-License-Identifier: MIT OR Apache-2.0

//! Lyrion Music Server (LMS / Squeezebox) SlimProto client adapter (`kiiaud-adapter-lms`, Phase 5B).
//!
//! Supports two transport modes with dynamic runtime hot-reload:
//! 1. **`DirectTcp` (LAN Direct)**:
//!    - Connects to LMS on TCP port 3483.
//!    - Sends `HELO` advertising capabilities (`pcm`, `flc`, `wav`, `aif`, `wvp`, `dsf`, `dff`, `mp3`, `ogg`, `ops`) and player MAC address.
//!    - Sends `SETD` player name packet so LMS displays the configured `player_name`.
//!    - Pulls audio streams directly from `http://<lms_host>:<http_port>/stream.mp3?...`.
//! 2. **`AnywhereWs` (Remote Lyrion Anywhere Gateway)**:
//!    - Authenticates with a Lyrion Anywhere Gateway (`/v1/auth/login`, `/v1/auth/pair`, `/v1/auth/refresh`).
//!    - Opens the SlimProto WebSocket tunnel (`WS /v1/player`, Protocol v2) with `{"token","protocol":2,"quality":...}`.
//!    - Adopts the Gateway-assigned player MAC address (`02:...`) and device name from the Greeting message.
//!    - Replies to `setd` name queries (does not send unsolicited `SETD` at startup per Gateway policy).
//!    - Fetches audio streams from `GET /v1/stream/<session>/<seq>` with `Authorization: Bearer <access_token>`.
//!    - Emits periodic byte-acknowledgement control messages `{"ack":{"stream":<seq>,"bytes":<total>}}` every 256 KiB.
//!    - Parses Gateway `{"stream":{...}}` metadata messages to expose the remote served codec and relay mode in the Signal Path.

use futures_util::{SinkExt, StreamExt};
use kiiaud_core::config::{LagAdapterConfig, LmsAdapterConfig, parse_mac_address};
use kiiaud_core::{AudioSource, PlaybackCommand, PlaybackEvent, PlayerState, Result};
use kiiaud_decoder::stream::{
    ANYWHERE_FORMAT_HINT_HEADER, ANYWHERE_STREAM_META_HEADER, set_anywhere_ack_callback,
};
use kiiaud_playback::EngineHandle;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

/// Device ID 12 = `squeezelite` in SlimProto `HELO`.
const SLIMPROTO_DEVICE_ID_SQUEEZELITE: u8 = 12;
const SLIMPROTO_REVISION: u8 = 0;

/// Lyrion Anywhere player protocol version spoken by `kiiaud`.
pub const ANYWHERE_PLAYER_PROTOCOL: u32 = 2;

/// Global watch channel sender for hot-reloading `LmsAdapterConfig` at runtime (e.g. from WebUI).
static LMS_CONFIG_WATCH: OnceLock<watch::Sender<LmsAdapterConfig>> = OnceLock::new();

/// Global runtime status of the LMS LAN connection for inspection in the WebUI.
static LMS_RUNTIME_STATUS: OnceLock<RwLock<LmsRuntimeStatus>> = OnceLock::new();

/// Global watch channel sender for hot-reloading `LagAdapterConfig` at runtime (e.g. from WebUI).
static LAG_CONFIG_WATCH: OnceLock<watch::Sender<LagAdapterConfig>> = OnceLock::new();

/// Global runtime status of the Lyrion Anywhere Gateway (lag) connection for inspection in the WebUI.
static LAG_RUNTIME_STATUS: OnceLock<RwLock<LagRuntimeStatus>> = OnceLock::new();

/// Global channel forwarding byte-acknowledgements `(session_id, stream_seq, total_bytes)` from
/// `kiiaud-decoder::stream` back to the active Lyrion Anywhere WebSocket session.
static ANYWHERE_ACK_TX: OnceLock<RwLock<Option<mpsc::UnboundedSender<(String, u64, u64)>>>> =
    OnceLock::new();

/// Snapshot of the LMS (LAN) adapter connection state.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LmsRuntimeStatus {
    pub connected: bool,
    pub endpoint: String,
    pub assigned_mac: Option<String>,
    pub assigned_name: Option<String>,
    pub last_error: Option<String>,
}

/// Snapshot of the Lyrion Anywhere Gateway (lag) adapter connection state.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LagRuntimeStatus {
    pub connected: bool,
    pub endpoint: String,
    pub assigned_mac: Option<String>,
    pub assigned_name: Option<String>,
    pub gateway_authenticated: bool,
    pub quality: String,
    pub last_stream_info: Option<String>,
    pub last_error: Option<String>,
}

/// Get the current runtime status of the LMS LAN adapter.
pub fn get_lms_runtime_status() -> LmsRuntimeStatus {
    LMS_RUNTIME_STATUS
        .get_or_init(|| RwLock::new(LmsRuntimeStatus::default()))
        .read()
        .clone()
}

fn update_lms_runtime_status<F: FnOnce(&mut LmsRuntimeStatus)>(f: F) {
    let lock = LMS_RUNTIME_STATUS.get_or_init(|| RwLock::new(LmsRuntimeStatus::default()));
    let mut st = lock.write();
    f(&mut st);
}

/// Get the current runtime status of the Lyrion Anywhere Gateway (lag) adapter.
pub fn get_lag_runtime_status() -> LagRuntimeStatus {
    LAG_RUNTIME_STATUS
        .get_or_init(|| RwLock::new(LagRuntimeStatus::default()))
        .read()
        .clone()
}

fn update_lag_runtime_status<F: FnOnce(&mut LagRuntimeStatus)>(f: F) {
    let lock = LAG_RUNTIME_STATUS.get_or_init(|| RwLock::new(LagRuntimeStatus::default()));
    let mut st = lock.write();
    f(&mut st);
}

/// Trigger a live hot-reload of the LMS LAN adapter with a new `LmsAdapterConfig`.
pub fn reload_lms_adapter(new_config: LmsAdapterConfig) {
    if let Some(tx) = LMS_CONFIG_WATCH.get() {
        let _ = tx.send(new_config);
    }
}

/// Trigger a live hot-reload of the Lyrion Anywhere Gateway (lag) adapter with a new `LagAdapterConfig`.
pub fn reload_lag_adapter(new_config: LagAdapterConfig) {
    if let Some(tx) = LAG_CONFIG_WATCH.get() {
        let _ = tx.send(new_config);
    }
}

/// Result of authenticating or pairing with a Lyrion Anywhere Gateway.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnywhereAuthResult {
    pub gateway_url: String,
    pub access_token: String,
    pub refresh_token: String,
    pub device_name: String,
}

/// Parse a `lyrion-anywhere://pair?server=...&code=...` URI or a raw pairing code string.
/// Returns `(Option<server_url>, cleaned_code)`.
pub fn parse_pairing_input(input: &str) -> std::result::Result<(Option<String>, String), String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Pairing code cannot be empty".to_owned());
    }

    if let Some(query) = trimmed.strip_prefix("lyrion-anywhere://pair?") {
        let mut server_opt: Option<String> = None;
        let mut code_opt: Option<String> = None;
        for part in query.split('&') {
            if let Some((k, v)) = part.split_once('=') {
                match k {
                    "server" => {
                        let decoded = percent_decode_str(v);
                        if !decoded.is_empty() {
                            server_opt = Some(decoded.trim_end_matches('/').to_owned());
                        }
                    }
                    "code" => {
                        let clean: String = v
                            .chars()
                            .filter(|c| !c.is_whitespace() && *c != '-')
                            .collect();
                        if !clean.is_empty() {
                            code_opt = Some(clean.to_ascii_uppercase());
                        }
                    }
                    _ => {}
                }
            }
        }
        let code =
            code_opt.ok_or_else(|| "Pairing link is missing 'code=' parameter".to_owned())?;
        return Ok((server_opt, code));
    }

    let clean: String = trimmed
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect();
    if clean.len() < 4 || clean.len() > 32 || !clean.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("Invalid pairing code (expected 4-32 alphanumeric characters or lyrion-anywhere://pair link)".to_owned());
    }
    Ok((None, clean.to_ascii_uppercase()))
}

fn percent_decode_str(input: &str) -> String {
    let mut out = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push((h << 4) | l);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Normalize and validate a Lyrion Anywhere Gateway base URL.
pub fn normalize_gateway_url(raw: &str) -> std::result::Result<String, String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("Gateway URL cannot be empty".to_owned());
    }
    if !trimmed.starts_with("https://") && !trimmed.starts_with("http://") {
        return Err(
            "Gateway URL must start with https:// (or http:// for trusted private networks)"
                .to_owned(),
        );
    }
    Ok(trimmed.to_owned())
}

/// Authenticate with a Lyrion Anywhere Gateway using username + password (+ optional TOTP code).
pub async fn gateway_login(
    gateway_url: &str,
    username: &str,
    password: &str,
    totp: Option<&str>,
    device_name: &str,
) -> std::result::Result<AnywhereAuthResult, String> {
    let base = normalize_gateway_url(gateway_url)?;
    let url = format!("{base}/v1/auth/login");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    let dev_name = if device_name.trim().is_empty() {
        "kiiaud Hi-Fi"
    } else {
        device_name.trim()
    };

    let mut body = json!({
        "username": username.trim(),
        "password": password,
        "device_name": dev_name,
        "refresh": "body"
    });
    if let Some(code) = totp.map(str::trim).filter(|s| !s.is_empty()) {
        body["totp"] = json!(code);
    }

    let resp = client
        .post(&url)
        .header("X-Anywhere-Client", "1")
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Gateway login request failed: {}", e.without_url()))?;

    let status = resp.status();
    let v: serde_json::Value = resp.json().await.unwrap_or_default();
    if !status.is_success() {
        if v.get("totp_required").and_then(|b| b.as_bool()) == Some(true) {
            return Err(
                "TOTP_REQUIRED: Two-step verification code (6 digits) is required".to_owned(),
            );
        }
        let err_msg = v
            .get("error")
            .and_then(|s| s.as_str())
            .unwrap_or("login refused by gateway");
        return Err(format!("HTTP {status}: {err_msg}"));
    }

    let access_token = v
        .get("access_token")
        .and_then(|s| s.as_str())
        .ok_or_else(|| "Gateway response missing access_token".to_owned())?
        .to_owned();
    let refresh_token = v
        .get("refresh_token")
        .and_then(|s| s.as_str())
        .ok_or_else(|| "Gateway response missing refresh_token".to_owned())?
        .to_owned();

    Ok(AnywhereAuthResult {
        gateway_url: base,
        access_token,
        refresh_token,
        device_name: dev_name.to_owned(),
    })
}

/// Pair with a Lyrion Anywhere Gateway using a one-time pairing code or `lyrion-anywhere://pair?...` link.
pub async fn gateway_pair(
    gateway_url_opt: Option<&str>,
    pairing_input: &str,
    device_name: &str,
) -> std::result::Result<AnywhereAuthResult, String> {
    let (link_server, code) = parse_pairing_input(pairing_input)?;
    let chosen_base = link_server
        .as_deref()
        .or_else(|| gateway_url_opt.filter(|s| !s.trim().is_empty()))
        .ok_or_else(|| "Gateway URL is required when pairing with a bare code".to_owned())?;

    let base = normalize_gateway_url(chosen_base)?;
    let url = format!("{base}/v1/auth/pair");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    let dev_name = if device_name.trim().is_empty() {
        "kiiaud Hi-Fi"
    } else {
        device_name.trim()
    };

    let body = json!({
        "code": code,
        "device_name": dev_name
    });

    let resp = client
        .post(&url)
        .header("X-Anywhere-Client", "1")
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Gateway pairing request failed: {}", e.without_url()))?;

    let status = resp.status();
    let v: serde_json::Value = resp.json().await.unwrap_or_default();
    if !status.is_success() {
        let err_msg = v
            .get("error")
            .and_then(|s| s.as_str())
            .unwrap_or("pairing refused by gateway");
        return Err(format!("HTTP {status}: {err_msg}"));
    }

    let access_token = v
        .get("access_token")
        .and_then(|s| s.as_str())
        .ok_or_else(|| "Gateway pairing response missing access_token".to_owned())?
        .to_owned();
    let refresh_token = v
        .get("refresh_token")
        .and_then(|s| s.as_str())
        .ok_or_else(|| "Gateway pairing response missing refresh_token".to_owned())?
        .to_owned();

    Ok(AnywhereAuthResult {
        gateway_url: base,
        access_token,
        refresh_token,
        device_name: dev_name.to_owned(),
    })
}

/// Exchange a `refresh_token` for a new `(access_token, rotated_refresh_token)` on Lyrion Anywhere Gateway.
pub async fn gateway_refresh_token(
    gateway_url: &str,
    refresh_token: &str,
    reason: &str,
) -> std::result::Result<(String, String), String> {
    let base = normalize_gateway_url(gateway_url)?;
    let url = format!("{base}/v1/auth/refresh");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    let resp = client
        .post(&url)
        .header("X-Anywhere-Client", "1")
        .header("X-Anywhere-Refresh-Reason", reason)
        .header("Content-Type", "application/json")
        .json(&json!({ "refresh_token": refresh_token }))
        .send()
        .await
        .map_err(|e| format!("Gateway token refresh failed: {}", e.without_url()))?;

    let status = resp.status();
    let v: serde_json::Value = resp.json().await.unwrap_or_default();
    if !status.is_success() {
        let err_msg = v
            .get("error")
            .and_then(|s| s.as_str())
            .unwrap_or("refresh refused");
        return Err(format!("HTTP {status}: {err_msg}"));
    }

    let access = v
        .get("access_token")
        .and_then(|s| s.as_str())
        .ok_or_else(|| "Refresh response missing access_token".to_owned())?
        .to_owned();
    let new_refresh = v
        .get("refresh_token")
        .and_then(|s| s.as_str())
        .ok_or_else(|| "Refresh response missing refresh_token".to_owned())?
        .to_owned();

    Ok((access, new_refresh))
}

/// Start the LMS (LAN SlimProto) adapter supervisor loop with hot-reload and automatic reconnect (`NFR-007`).
pub async fn run_lms_adapter(initial_config: LmsAdapterConfig, engine: EngineHandle) -> Result<()> {
    let (tx, mut rx) = watch::channel(initial_config);
    let _ = LMS_CONFIG_WATCH.set(tx);

    loop {
        let config = rx.borrow_and_update().clone();

        if !config.enabled {
            update_lms_runtime_status(|st| {
                st.connected = false;
                st.endpoint.clear();
            });
            tracing::debug!("LMS LAN adapter is disabled; waiting for configuration change");
            if rx.changed().await.is_err() {
                return Ok(());
            }
            continue;
        }

        tokio::select! {
            _ = rx.changed() => {
                tracing::info!("LMS LAN adapter configuration updated at runtime; hot-reloading session");
                continue;
            }
            session_res = connect_and_run_direct_session(&config, &engine) => {
                match session_res {
                    Ok(()) => {
                        tracing::info!("LMS LAN session closed cleanly; reconnecting in 5s");
                    }
                    Err(e) => {
                        update_lms_runtime_status(|st| {
                            st.connected = false;
                            st.last_error = Some(e.clone());
                        });
                        tracing::warn!("LMS LAN session error: {e}; retrying in 5s");
                    }
                }
                tokio::select! {
                    _ = rx.changed() => {
                        tracing::info!("LMS LAN adapter configuration changed during backoff; reconnecting immediately");
                    }
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                }
            }
        }
    }
}

/// Start the Lyrion Anywhere Gateway (lag) adapter supervisor loop with hot-reload and automatic reconnect (`NFR-007`).
pub async fn run_lag_adapter(initial_config: LagAdapterConfig, engine: EngineHandle) -> Result<()> {
    // Register the global byte-ack callback from `kiiaud-decoder` once
    set_anywhere_ack_callback(|session, stream_seq, total_bytes| {
        if let Some(lock) = ANYWHERE_ACK_TX.get()
            && let Some(tx) = lock.read().as_ref()
        {
            let _ = tx.send((session.to_owned(), stream_seq, total_bytes));
        }
    });

    let (tx, mut rx) = watch::channel(initial_config);
    let _ = LAG_CONFIG_WATCH.set(tx);

    loop {
        let config = rx.borrow_and_update().clone();

        if !config.enabled {
            update_lag_runtime_status(|st| {
                st.connected = false;
                st.endpoint.clear();
            });
            tracing::debug!("Lyrion Anywhere (Gateway) adapter is disabled; waiting for configuration change");
            if rx.changed().await.is_err() {
                return Ok(());
            }
            continue;
        }

        tokio::select! {
            _ = rx.changed() => {
                tracing::info!("Lyrion Anywhere (Gateway) adapter configuration updated at runtime; hot-reloading session");
                continue;
            }
            session_res = connect_and_run_anywhere_session(&config, &engine) => {
                match session_res {
                    Ok(()) => {
                        tracing::info!("Lyrion Anywhere (Gateway) session closed cleanly; reconnecting in 5s");
                    }
                    Err(e) => {
                        update_lag_runtime_status(|st| {
                            st.connected = false;
                            st.last_error = Some(e.clone());
                        });
                        tracing::warn!("Lyrion Anywhere (Gateway) session error: {e}; retrying in 5s");
                    }
                }
                tokio::select! {
                    _ = rx.changed() => {
                        tracing::info!("Lyrion Anywhere (Gateway) adapter configuration changed during backoff; reconnecting immediately");
                    }
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                }
            }
        }
    }
}

/// Run a direct LAN TCP SlimProto session (`DirectTcp` on port 3483).
async fn connect_and_run_direct_session(
    config: &LmsAdapterConfig,
    engine: &EngineHandle,
) -> std::result::Result<(), String> {
    let addr = format!("{}:{}", config.server_host, config.server_port);
    update_lms_runtime_status(|st| {
        st.connected = false;
        st.endpoint = addr.clone();
        st.assigned_mac = Some(format_mac(&config.mac_address));
        st.assigned_name = Some(config.player_name.clone());
    });

    let mut stream = TcpStream::connect(&addr)
        .await
        .map_err(|e| format!("connect to {addr} failed: {e}"))?;
    let _ = stream.set_nodelay(true);

    let (max_sample_rate, dsd_supported) =
        resolve_hardware_capabilities(config.max_sample_rate, config.dsd_support, engine);
    let start_epoch = Instant::now();

    // 1. Send HELO packet with hardware-informed capabilities
    let helo_pkt = build_helo_packet(
        &config.mac_address,
        &config.player_name,
        max_sample_rate,
        dsd_supported,
    );
    stream
        .write_all(&helo_pkt)
        .await
        .map_err(|e| format!("HELO write failed: {e}"))?;

    // 2. Send SETD (id=0: player name)
    let setd_pkt = build_setd_name_packet(&config.player_name);
    stream
        .write_all(&setd_pkt)
        .await
        .map_err(|e| format!("SETD write failed: {e}"))?;

    update_lms_runtime_status(|st| {
        st.connected = true;
        st.last_error = None;
    });

    tracing::info!(
        server = %addr,
        player_name = %config.player_name,
        max_sample_rate,
        dsd_supported,
        "connected to LMS server (DirectTcp) and sent HELO"
    );

    let mut event_rx = engine.subscribe();
    let mut tick_1s = tokio::time::interval(Duration::from_secs(1));
    let mut len_buf = [0u8; 2];

    loop {
        tokio::select! {
            read_res = stream.read_exact(&mut len_buf) => {
                read_res.map_err(|e| format!("read packet length failed: {e}"))?;
                let pkt_len = u16::from_be_bytes(len_buf) as usize;
                if !(4..=65535).contains(&pkt_len) {
                    return Err(format!("invalid SlimProto packet length: {pkt_len}"));
                }
                let mut payload = vec![0u8; pkt_len];
                stream
                    .read_exact(&mut payload)
                    .await
                    .map_err(|e| format!("read packet payload failed: {e}"))?;

                let replies = handle_slimproto_server_packet(
                    &payload,
                    Some((&config.server_host, config.http_port)),
                    &config.player_name,
                    engine,
                    start_epoch,
                    None,
                )
                .await;
                for pkt in replies {
                    stream
                        .write_all(&pkt)
                        .await
                        .map_err(|e| format!("write packet failed: {e}"))?;
                }
            }
            Ok(ev) = event_rx.recv() => {
                if let Some(pkt) = build_event_stat_packet(&ev, engine, start_epoch) {
                    let _ = stream.write_all(&pkt).await;
                }
            }
            _ = tick_1s.tick() => {
                if engine.status().state == PlayerState::Play {
                    let stat = build_stat_packet(b"STMt", engine, start_epoch, 0);
                    let _ = stream.write_all(&stat).await;
                }
            }
        }
    }
}

/// Context for resolving audio streams through a Lyrion Anywhere Gateway.
struct AnywhereSessionCtx {
    base_url: String,
    session_id: String,
    access_token: Arc<RwLock<String>>,
    stream_seq: u64,
}

/// Run a remote Lyrion Anywhere Gateway session (`AnywhereWs` over `WS /v1/player` + `GET /v1/stream/...`).
async fn connect_and_run_anywhere_session(
    config: &LagAdapterConfig,
    engine: &EngineHandle,
) -> std::result::Result<(), String> {
    let raw_gw = config
        .gateway_url
        .as_deref()
        .ok_or_else(|| "gateway_url is not configured".to_owned())?;
    let base_url = normalize_gateway_url(raw_gw)?;

    update_lag_runtime_status(|st| {
        st.connected = false;
        st.endpoint = base_url.clone();
        st.quality = config.gateway_quality.clone();
    });

    // 1. Obtain access_token + refresh_token (via stored refresh_token, pairing_code, or username/password)
    let (initial_access, initial_refresh) = resolve_anywhere_tokens(config, &base_url).await?;
    let access_token = Arc::new(RwLock::new(initial_access.clone()));
    let refresh_token_holder = Arc::new(RwLock::new(initial_refresh.clone()));

    update_lag_runtime_status(|st| {
        st.gateway_authenticated = true;
    });

    // Persist rotated refresh_token into in-memory config watch if changed
    persist_rotated_refresh_token(&initial_refresh);

    // 2. Open WebSocket `/v1/player`
    let ws_url = build_gateway_ws_url(&base_url)?;
    let ws_req = ws_url
        .into_client_request()
        .map_err(|e| format!("invalid gateway WS request: {e}"))?;

    let (ws_stream, _) = connect_async(ws_req)
        .await
        .map_err(|e| format!("gateway WebSocket handshake failed: {e}"))?;

    let (mut ws_write, mut ws_read) = ws_stream.split();

    // 3. Send Protocol 2 sign-in text message within 5s
    let quality = match config.gateway_quality.to_ascii_lowercase().as_str() {
        "high" => "high",
        "low" => "low",
        _ => "original",
    };
    let sign_in_msg = json!({
        "token": initial_access,
        "protocol": ANYWHERE_PLAYER_PROTOCOL,
        "quality": quality,
        "keepalive": true
    });
    ws_write
        .send(Message::Text(sign_in_msg.to_string().into()))
        .await
        .map_err(|e| format!("failed to send gateway sign-in message: {e}"))?;

    // 4. Wait for Gateway Greeting text message:
    //    `{"session":"<32 hex>","secret":"<32 hex>","mac":"02:...","name":"...","resumed":false,"grace":60}`
    let greeting_val: serde_json::Value = loop {
        let Some(msg_res) = ws_read.next().await else {
            return Err("gateway closed WebSocket before greeting".to_owned());
        };
        match msg_res.map_err(|e| format!("gateway greeting read error: {e}"))? {
            Message::Text(txt) => {
                let v: serde_json::Value = serde_json::from_str(&txt)
                    .map_err(|e| format!("invalid JSON greeting from gateway: {e}"))?;
                break v;
            }
            Message::Ping(payload) => {
                let _ = ws_write.send(Message::Pong(payload)).await;
            }
            Message::Close(frame) => {
                let reason = frame
                    .map(|f| format!("{} (code {})", f.reason, u16::from(f.code)))
                    .unwrap_or_else(|| "closed".to_owned());
                return Err(format!("gateway rejected player sign-in: {reason}"));
            }
            _ => {}
        }
    };

    let session_id = greeting_val
        .get("session")
        .and_then(|s| s.as_str())
        .ok_or_else(|| "gateway greeting missing 'session'".to_owned())?
        .to_owned();
    let mac_str = greeting_val
        .get("mac")
        .and_then(|s| s.as_str())
        .ok_or_else(|| "gateway greeting missing 'mac'".to_owned())?;
    let assigned_mac = parse_mac_address(mac_str)
        .map_err(|e| format!("gateway assigned invalid MAC '{mac_str}': {e}"))?;
    let assigned_name = greeting_val
        .get("name")
        .and_then(|s| s.as_str())
        .unwrap_or(&config.player_name)
        .to_owned();

    // 5. Send HELO with the Gateway-assigned MAC and Device ID 12 (software player).
    //    Per Lyrion Anywhere protocol §5.1, do NOT send unsolicited SETD at startup;
    //    only send SETD in response to a server `setd` id=0 query.
    let (max_sample_rate, dsd_supported) =
        resolve_hardware_capabilities(config.max_sample_rate, config.dsd_support, engine);
    let start_epoch = Instant::now();
    let helo_pkt = build_helo_packet(
        &assigned_mac,
        &assigned_name,
        max_sample_rate,
        dsd_supported,
    );
    ws_write
        .send(Message::Binary(helo_pkt.into()))
        .await
        .map_err(|e| format!("failed to send HELO over gateway WS: {e}"))?;

    update_lag_runtime_status(|st| {
        st.connected = true;
        st.assigned_mac = Some(format_mac(&assigned_mac));
        st.assigned_name = Some(assigned_name.clone());
        st.last_error = None;
    });

    tracing::info!(
        gateway = %base_url,
        session = %session_id,
        assigned_name = %assigned_name,
        assigned_mac = %format_mac(&assigned_mac),
        quality,
        max_sample_rate,
        dsd_supported,
        "connected to Lyrion Anywhere Gateway (WS /v1/player) and sent HELO"
    );

    // Set up channel for byte acknowledgements from the HTTP stream reader
    let (ack_tx, mut ack_rx) = mpsc::unbounded_channel::<(String, u64, u64)>();
    {
        let lock = ANYWHERE_ACK_TX.get_or_init(|| RwLock::new(None));
        *lock.write() = Some(ack_tx);
    }

    let mut gw_ctx = AnywhereSessionCtx {
        base_url: base_url.clone(),
        session_id: session_id.clone(),
        access_token: Arc::clone(&access_token),
        stream_seq: 0,
    };

    let mut event_rx = engine.subscribe();
    let mut tick_1s = tokio::time::interval(Duration::from_secs(1));
    // Access tokens last 15 minutes; refresh every 10 minutes
    let mut refresh_ticker = tokio::time::interval(Duration::from_secs(10 * 60));
    refresh_ticker.tick().await; // consume immediate tick

    loop {
        tokio::select! {
            msg_opt = ws_read.next() => {
                let Some(msg_res) = msg_opt else {
                    return Err("gateway WebSocket stream closed".to_owned());
                };
                match msg_res.map_err(|e| format!("gateway WS read error: {e}"))? {
                    Message::Binary(bin) => {
                        // Server -> client binary message carries a length-prefixed SlimProto frame (`[2B len][payload]`)
                        let mut offset = 0usize;
                        while offset + 2 <= bin.len() {
                            let pkt_len = u16::from_be_bytes([bin[offset], bin[offset + 1]]) as usize;
                            offset += 2;
                            if offset + pkt_len > bin.len() {
                                break;
                            }
                            let payload = &bin[offset..offset + pkt_len];
                            offset += pkt_len;

                            let replies = handle_slimproto_server_packet(
                                payload,
                                None,
                                &assigned_name,
                                engine,
                                start_epoch,
                                Some(&mut gw_ctx),
                            )
                            .await;

                            for pkt in replies {
                                ws_write
                                    .send(Message::Binary(pkt.into()))
                                    .await
                                    .map_err(|e| format!("gateway WS write error: {e}"))?;
                            }
                        }
                    }
                    Message::Text(txt) => {
                        handle_gateway_text_message(&txt);
                    }
                    Message::Ping(payload) => {
                        let _ = ws_write.send(Message::Pong(payload)).await;
                    }
                    Message::Close(frame) => {
                        let reason = frame
                            .map(|f| format!("{} (code {})", f.reason, u16::from(f.code)))
                            .unwrap_or_else(|| "closed".to_owned());
                        return Err(format!("gateway closed session: {reason}"));
                    }
                    _ => {}
                }
            }
            Some((sess, seq, bytes)) = ack_rx.recv() => {
                if sess == gw_ctx.session_id {
                    let ack_msg = json!({
                        "ack": {
                            "stream": seq,
                            "bytes": bytes
                        }
                    });
                    let _ = ws_write.send(Message::Text(ack_msg.to_string().into())).await;
                }
            }
            Ok(ev) = event_rx.recv() => {
                if let Some(pkt) = build_event_stat_packet(&ev, engine, start_epoch) {
                    let _ = ws_write.send(Message::Binary(pkt.into())).await;
                }
            }
            _ = tick_1s.tick() => {
                // In Lyrion Anywhere Gateway mode, the gateway requires a client STAT heartbeat
                // at least every 8s even when idle/stopped; otherwise it treats the socket as silent
                // and takes over reporting on our behalf. Sending STMt every 1s in all states keeps
                // the player session live and responsive.
                let stat = build_stat_packet(b"STMt", engine, start_epoch, 0);
                let _ = ws_write.send(Message::Binary(stat.into())).await;
            }
            _ = refresh_ticker.tick() => {
                let cur_refresh = refresh_token_holder.read().clone();
                match gateway_refresh_token(&base_url, &cur_refresh, "timer").await {
                    Ok((new_access, new_refresh)) => {
                        *access_token.write() = new_access;
                        *refresh_token_holder.write() = new_refresh.clone();
                        persist_rotated_refresh_token(&new_refresh);
                        tracing::debug!("refreshed Lyrion Anywhere access token in background");
                    }
                    Err(e) => {
                        tracing::warn!("failed to refresh Lyrion Anywhere token: {e}");
                    }
                }
            }
        }
    }
}

/// Handle text messages sent by Lyrion Anywhere Gateway (`stream`, `quality`, `keepalive`).
fn handle_gateway_text_message(txt: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(txt) else {
        return;
    };
    if let Some(stream_obj) = v.get("stream") {
        let seq = stream_obj.get("seq").and_then(|n| n.as_u64()).unwrap_or(0);
        let codec = stream_obj
            .pointer("/served/codec")
            .and_then(|s| s.as_str())
            .unwrap_or("audio")
            .to_ascii_uppercase();
        let rate = stream_obj.pointer("/served/rate").and_then(|n| n.as_u64());
        let bits = stream_obj.pointer("/served/bits").and_then(|n| n.as_u64());
        let mode = stream_obj
            .pointer("/sent/mode")
            .and_then(|s| s.as_str())
            .unwrap_or("passthrough");
        let kbps = stream_obj.pointer("/sent/kbps").and_then(|n| n.as_u64());

        let served_desc = match (rate, bits) {
            (Some(r), Some(b)) => format!("{codec} {:.1}kHz/{b}b", r as f64 / 1000.0),
            (Some(r), None) => format!("{codec} {:.1}kHz", r as f64 / 1000.0),
            _ => codec,
        };
        let sent_desc = if mode == "passthrough" {
            "Bit-Perfect Passthrough".to_owned()
        } else if let Some(k) = kbps {
            format!("Transcoded {} {}kbps", mode.to_ascii_uppercase(), k)
        } else {
            mode.to_owned()
        };
        let summary = format!("Stream #{seq}: {served_desc} ({sent_desc})");
        tracing::info!(%summary, "Lyrion Anywhere Gateway stream metadata received");
        update_lag_runtime_status(|st| {
            st.last_stream_info = Some(summary);
        });
    }
}

async fn resolve_anywhere_tokens(
    config: &LagAdapterConfig,
    base_url: &str,
) -> std::result::Result<(String, String), String> {
    // 1. Try existing refresh_token first
    if let Some(refresh) = config
        .gateway_refresh_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        match gateway_refresh_token(base_url, refresh, "player").await {
            Ok((acc, new_ref)) => return Ok((acc, new_ref)),
            Err(e) => {
                tracing::warn!(
                    "stored gateway_refresh_token rejected ({e}); attempting fallback credentials"
                );
            }
        }
    }

    // 2. Try pairing code if provided
    if let Some(pair_code) = config
        .gateway_pairing_code
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let res = gateway_pair(Some(base_url), pair_code, &config.player_name).await?;
        return Ok((res.access_token, res.refresh_token));
    }

    // 3. Try username + password if provided
    if let (Some(user), Some(pass)) = (
        config
            .gateway_username
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
        config.gateway_password.as_deref().filter(|s| !s.is_empty()),
    ) {
        let res = gateway_login(base_url, user, pass, None, &config.player_name).await?;
        return Ok((res.access_token, res.refresh_token));
    }

    Err(
        "Lyrion Anywhere Gateway requires authentication (Sign In or Pair Device in WebUI Config)"
            .to_owned(),
    )
}

fn persist_rotated_refresh_token(new_refresh: &str) {
    if let Some(tx) = LAG_CONFIG_WATCH.get() {
        tx.send_if_modified(|cfg| {
            if cfg.gateway_refresh_token.as_deref() != Some(new_refresh) {
                cfg.gateway_refresh_token = Some(new_refresh.to_owned());
                // Do not trigger a reconnect loop when only rotating the refresh token
                false
            } else {
                false
            }
        });
    }
}

/// Build the `ws://` or `wss://` player endpoint URL from a Gateway base URL.
pub fn build_gateway_ws_url(base_url: &str) -> std::result::Result<String, String> {
    let base = normalize_gateway_url(base_url)?;
    if let Some(rest) = base.strip_prefix("https://") {
        Ok(format!("wss://{rest}/v1/player"))
    } else if let Some(rest) = base.strip_prefix("http://") {
        Ok(format!("ws://{rest}/v1/player"))
    } else {
        Err("Invalid gateway URL scheme".to_owned())
    }
}

pub fn format_mac(mac: &[u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

fn build_event_stat_packet(
    ev: &PlaybackEvent,
    engine: &EngineHandle,
    start_epoch: Instant,
) -> Option<Vec<u8>> {
    match ev {
        PlaybackEvent::TrackStarted { .. } => {
            Some(build_stat_packet(b"STMs", engine, start_epoch, 0))
        }
        PlaybackEvent::TrackEnded(_) => Some(build_stat_packet(b"STMd", engine, start_epoch, 0)),
        PlaybackEvent::StateChanged(PlayerState::Stop) => {
            Some(build_stat_packet(b"STMu", engine, start_epoch, 0))
        }
        _ => None,
    }
}

/// Resolve max sample rate and DSD support based on config override or detected DAC output capabilities.
pub fn resolve_hardware_capabilities(
    max_sample_rate: Option<u32>,
    dsd_support: Option<bool>,
    engine: &EngineHandle,
) -> (u32, bool) {
    let st = engine.status();
    let out_caps = st.output_capabilities.as_ref();

    let max_rate = max_sample_rate.unwrap_or_else(|| {
        out_caps
            .and_then(|c| c.sample_rates.iter().copied().max())
            .unwrap_or(192_000)
    });

    let dsd = dsd_support
        .unwrap_or_else(|| out_caps.is_some_and(|c| c.dsd_native || c.dsd_dop));

    (max_rate, dsd)
}

/// Build client-to-server `HELO` packet:
/// `[4B "HELO"][4B length BE][1B dev_id][1B rev][6B mac][16B uuid][2B wlan_ch][8B bytes_rx][2B lang][capabilities...]`
pub fn build_helo_packet(
    mac: &[u8; 6],
    player_name: &str,
    max_sample_rate: u32,
    dsd_supported: bool,
) -> Vec<u8> {
    let mut codecs = String::new();
    if dsd_supported {
        codecs.push_str("dsf,dff,");
    }
    codecs.push_str("wvp,flc,wav,aif,pcm,mp3,ogg,ops");
    // Sanitize ModelName so it has no commas or control chars (required by Lyrion Anywhere HELO capability filter)
    let clean_model_name: String = player_name
        .chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .map(|c| if c == ',' { '_' } else { c })
        .take(48)
        .collect();
    let model_name = if clean_model_name.trim().is_empty() {
        "kiiaud"
    } else {
        clean_model_name.trim()
    };

    let caps = format!(
        "CanHTTPS=1,Model=squeezelite,AccuratePlayPoints=1,HasDigitalOut=1,HasPolarityInversion=1,ModelName={model_name},MaxSampleRate={max_sample_rate},{codecs}"
    );
    let body_len = 36 + caps.len();
    let mut pkt = Vec::with_capacity(8 + body_len);
    pkt.extend_from_slice(b"HELO");
    pkt.extend_from_slice(&(body_len as u32).to_be_bytes());
    pkt.push(SLIMPROTO_DEVICE_ID_SQUEEZELITE);
    pkt.push(SLIMPROTO_REVISION);
    pkt.extend_from_slice(mac);
    pkt.extend_from_slice(&[0u8; 16]); // UUID
    pkt.extend_from_slice(&0u16.to_be_bytes()); // wlan channel list
    pkt.extend_from_slice(&0u64.to_be_bytes()); // bytes received
    pkt.extend_from_slice(&[0u8; 2]); // language / padding
    pkt.extend_from_slice(caps.as_bytes());
    pkt
}

/// Build client-to-server `RESP` packet acknowledging HTTP response headers.
pub fn build_resp_packet(headers: &str) -> Vec<u8> {
    let header_bytes = headers.as_bytes();
    let body_len = header_bytes.len();
    let mut pkt = Vec::with_capacity(8 + body_len);
    pkt.extend_from_slice(b"RESP");
    pkt.extend_from_slice(&(body_len as u32).to_be_bytes());
    pkt.extend_from_slice(header_bytes);
    pkt
}

/// Build client-to-server `SETD` packet announcing the player name (`id = 0`).
pub fn build_setd_name_packet(name: &str) -> Vec<u8> {
    let name_bytes = name.as_bytes();
    let body_len = 1 + name_bytes.len();
    let mut pkt = Vec::with_capacity(8 + body_len);
    pkt.extend_from_slice(b"SETD");
    pkt.extend_from_slice(&(body_len as u32).to_be_bytes());
    pkt.push(0); // id 0 = player name
    pkt.extend_from_slice(name_bytes);
    pkt
}

/// Build client-to-server `STAT` packet (53 bytes body).
pub fn build_stat_packet(
    event_code: &[u8; 4],
    engine: &EngineHandle,
    start_epoch: Instant,
    server_timestamp: u32,
) -> Vec<u8> {
    let st = engine.status();
    let elapsed_ms = st.elapsed.as_millis() as u32;
    let elapsed_secs = st.elapsed.as_secs() as u32;
    let jiffies = start_epoch.elapsed().as_millis() as u32;

    let telemetry = kiiaud_decoder::stream::get_stream_telemetry();

    let bytes_received = if telemetry.total_bytes_read > 0 {
        telemetry.total_bytes_read
    } else {
        match st.state {
            PlayerState::Play => (elapsed_ms as u64).saturating_mul(176),
            _ => 0,
        }
    };

    let (stream_buf_fullness, stream_buf_size) = if telemetry.buffer_capacity > 0 {
        (telemetry.buffer_occupancy, telemetry.buffer_capacity)
    } else {
        match st.state {
            PlayerState::Play => (1024 * 1024u32, 8 * 1024 * 1024u32),
            PlayerState::Pause => (1024 * 1024u32, 8 * 1024 * 1024u32),
            PlayerState::Stop => (0u32, 8 * 1024 * 1024u32),
        }
    };

    let output_buf_fullness = match st.state {
        PlayerState::Play => 512 * 1024u32,
        PlayerState::Pause => 0u32,
        PlayerState::Stop => 0u32,
    };

    let mut body = Vec::with_capacity(53);
    body.extend_from_slice(event_code); // 0..4: event code ("STMs", "STMt", etc.)
    body.push(0); // 4: num_crlf
    body.push(0); // 5: mas_initialized
    body.push(0); // 6: mas_mode
    body.extend_from_slice(&stream_buf_size.to_be_bytes()); // 7..11: stream buffer size
    body.extend_from_slice(&stream_buf_fullness.to_be_bytes()); // 11..15: stream buffer fullness
    body.extend_from_slice(&bytes_received.to_be_bytes()); // 15..23: bytes received
    body.extend_from_slice(&0xffffu16.to_be_bytes()); // 23..25: signal strength (0xffff = Ethernet/wired in Squeezelite)
    body.extend_from_slice(&jiffies.to_be_bytes()); // 25..29: jiffies
    body.extend_from_slice(&(1024 * 1024u32).to_be_bytes()); // 29..33: output buffer size
    body.extend_from_slice(&output_buf_fullness.to_be_bytes()); // 33..37: output buffer fullness
    body.extend_from_slice(&elapsed_secs.to_be_bytes()); // 37..41: elapsed seconds
    body.extend_from_slice(&0u16.to_be_bytes()); // 41..43: voltage
    body.extend_from_slice(&elapsed_ms.to_be_bytes()); // 43..47: elapsed milliseconds
    body.extend_from_slice(&server_timestamp.to_be_bytes()); // 47..51: server timestamp
    body.extend_from_slice(&0u16.to_be_bytes()); // 51..53: error code

    let mut pkt = Vec::with_capacity(8 + body.len());
    pkt.extend_from_slice(b"STAT");
    pkt.extend_from_slice(&(body.len() as u32).to_be_bytes());
    pkt.extend_from_slice(&body);
    pkt
}

/// Process a server-to-client SlimProto packet (`strm`, `setd`, `audg`, `aude`) and return any response packets to send.
async fn handle_slimproto_server_packet(
    payload: &[u8],
    default_http_target: Option<(&str, u16)>,
    effective_player_name: &str,
    engine: &EngineHandle,
    start_epoch: Instant,
    anywhere_ctx: Option<&mut AnywhereSessionCtx>,
) -> Vec<Vec<u8>> {
    let mut replies = Vec::new();
    if payload.len() < 4 {
        return replies;
    }
    let op = &payload[0..4];
    let data = &payload[4..];

    match op {
        b"strm" => {
            if data.len() < 24 {
                return replies;
            }
            let command = data[0];
            let _autostart = data[1];
            let format_byte = data[2];
            let replay_gain_or_ts = u32::from_be_bytes([data[14], data[15], data[16], data[17]]);
            let server_port = u16::from_be_bytes([data[18], data[19]]);
            let server_ip_raw = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
            let http_request_bytes = &data[24..];

            match command {
                b's' => {
                    let source = if let Some(gw) = anywhere_ctx {
                        // In Lyrion Anywhere Gateway mode, every `strm s` increments the session's
                        // 1-based stream counter (`seq`). If the gateway refused the stream (zeroed IP
                        // and empty HTTP request), report STMf + STMu instead of fetching.
                        gw.stream_seq += 1;
                        let seq = gw.stream_seq;
                        if http_request_bytes.is_empty() && server_ip_raw == 0 {
                            tracing::warn!(
                                seq,
                                "Lyrion Anywhere Gateway refused non-local stream; skipping"
                            );
                            replies.push(build_stat_packet(b"STMf", engine, start_epoch, 0));
                            replies.push(build_stat_packet(b"STMu", engine, start_epoch, 0));
                            return replies;
                        }

                        let url = format!("{}/v1/stream/{}/{}", gw.base_url, gw.session_id, seq);
                        let token = gw.access_token.read().clone();
                        let mut headers = HashMap::new();
                        headers.insert("Authorization".to_owned(), format!("Bearer {token}"));
                        headers.insert(
                            ANYWHERE_STREAM_META_HEADER.to_owned(),
                            format!("{}:{seq}", gw.session_id),
                        );
                        if let Some(hint) = format_byte_to_ext(format_byte) {
                            headers.insert(ANYWHERE_FORMAT_HINT_HEADER.to_owned(), hint.to_owned());
                        }
                        AudioSource::HttpStream { url, headers }
                    } else {
                        let host = if server_ip_raw == 0 {
                            default_http_target.map(|(h, _)| h).unwrap_or("127.0.0.1").to_owned()
                        } else {
                            Ipv4Addr::from(server_ip_raw).to_string()
                        };
                        let port = if server_port == 0 {
                            default_http_target.map(|(_, p)| p).unwrap_or(9000)
                        } else {
                            server_port
                        };
                        parse_lms_strm_source(&host, port, format_byte, http_request_bytes)
                    };

                    tracing::info!(
                        %source,
                        format_byte = %(format_byte as char),
                        "LMS SlimProto strm-s start command received"
                    );

                    kiiaud_decoder::stream::reset_stream_telemetry();
                    let _ = engine
                        .send(PlaybackCommand::Load {
                            source,
                            title: None,
                            artist: None,
                            autostart: true,
                        })
                        .await;

                    replies.push(build_stat_packet(b"STMf", engine, start_epoch, 0));
                    replies.push(build_stat_packet(b"STMc", engine, start_epoch, 0));
                    replies.push(build_resp_packet(
                        "HTTP/1.0 200 OK\r\nContent-Type: audio/x-flac\r\n\r\n",
                    ));
                }
                b'p' => {
                    let _ = engine.send(PlaybackCommand::Pause).await;
                    replies.push(build_stat_packet(b"STMp", engine, start_epoch, 0));
                }
                b'u' => {
                    let _ = engine.send(PlaybackCommand::Resume).await;
                    replies.push(build_stat_packet(b"STMr", engine, start_epoch, 0));
                }
                b'q' => {
                    let _ = engine.send(PlaybackCommand::Stop).await;
                    replies.push(build_stat_packet(b"STMf", engine, start_epoch, 0));
                }
                b't' => {
                    replies.push(build_stat_packet(
                        b"STMt",
                        engine,
                        start_epoch,
                        replay_gain_or_ts,
                    ));
                }
                b'f' => {
                    kiiaud_decoder::stream::reset_stream_telemetry();
                    replies.push(build_stat_packet(b"STMf", engine, start_epoch, 0));
                }
                _ => {}
            }
        }
        b"setd" => {
            if data.first() == Some(&0) && data.len() == 1 {
                // Server querying our player name (`setd` id 0 with no data)
                replies.push(build_setd_name_packet(effective_player_name));
            }
        }
        b"audg" | b"aude" => {
            // In bit-perfect mode, software volume attenuation is intentionally bypassed (`D-008`).
        }
        _ => {}
    }

    replies
}

fn format_byte_to_ext(format_byte: u8) -> Option<&'static str> {
    match format_byte {
        b'f' => Some("flac"),
        b'w' => Some("wv"),
        b'p' => Some("wav"),
        b'a' => Some("aiff"),
        b'd' => Some("dsf"),
        b'm' => Some("mp3"),
        b'o' => Some("ogg"),
        b'u' => Some("opus"),
        _ => None,
    }
}

/// Convert the `strm` HTTP request header bytes into an [`AudioSource`].
pub fn parse_lms_strm_source(
    host: &str,
    port: u16,
    format_byte: u8,
    http_request_bytes: &[u8],
) -> AudioSource {
    let hint = format_byte_to_ext(format_byte).map(str::to_owned);

    let req_str = String::from_utf8_lossy(http_request_bytes);
    if let Some(first_line) = req_str.lines().next()
        && let Some(rest) = first_line.strip_prefix("GET ")
        && let Some((path_part, _)) = rest.split_once(" HTTP/")
    {
        let mut headers = HashMap::new();
        for line in req_str.lines().skip(1) {
            if let Some((k, v)) = line.split_once(':') {
                let k_trim = k.trim();
                let v_trim = v.trim();
                if !k_trim.is_empty() && !k_trim.eq_ignore_ascii_case("host") {
                    headers.insert(k_trim.to_owned(), v_trim.to_owned());
                }
            }
        }
        if let Some(h) = hint {
            headers.insert(ANYWHERE_FORMAT_HINT_HEADER.to_owned(), h);
        }
        let url = format!("http://{host}:{port}{}", path_part.trim());
        return AudioSource::HttpStream { url, headers };
    }

    AudioSource::TcpStream {
        host: host.to_owned(),
        port,
        request: http_request_bytes.to_vec(),
        format_hint: hint,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_helo_packet_structure() {
        let mac = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];
        let pkt = build_helo_packet(&mac, "kiiaud", 192_000, false);
        assert_eq!(&pkt[0..4], b"HELO");
        let len = u32::from_be_bytes([pkt[4], pkt[5], pkt[6], pkt[7]]) as usize;
        assert_eq!(pkt.len(), 8 + len);
        assert_eq!(pkt[8], SLIMPROTO_DEVICE_ID_SQUEEZELITE);
        assert_eq!(&pkt[10..16], &mac);
        let caps = String::from_utf8_lossy(&pkt[8 + 36..]);
        assert!(caps.contains("ModelName=kiiaud"));
        assert!(caps.contains("MaxSampleRate=192000"));
        assert!(!caps.contains("dsf"));

        let pkt_dsd = build_helo_packet(&mac, "kiiaud", 768_000, true);
        let caps_dsd = String::from_utf8_lossy(&pkt_dsd[8 + 36..]);
        assert!(caps_dsd.contains("MaxSampleRate=768000"));
        assert!(caps_dsd.contains("dsf,dff"));
    }

    #[test]
    fn test_build_resp_packet() {
        let headers = "HTTP/1.0 200 OK\r\nContent-Type: audio/x-flac\r\n\r\n";
        let pkt = build_resp_packet(headers);
        assert_eq!(&pkt[0..4], b"RESP");
        let len = u32::from_be_bytes([pkt[4], pkt[5], pkt[6], pkt[7]]) as usize;
        assert_eq!(len, headers.len());
        assert_eq!(&pkt[8..], headers.as_bytes());
    }

    #[test]
    fn test_parse_lms_strm_get_request() {
        let req = b"GET /stream.mp3?player=02:11:22:33:44:55 HTTP/1.0\r\nAuthorization: Basic abc\r\n\r\n";
        let src = parse_lms_strm_source("192.168.1.100", 9000, b'f', req);
        match src {
            AudioSource::HttpStream { url, headers } => {
                assert_eq!(
                    url,
                    "http://192.168.1.100:9000/stream.mp3?player=02:11:22:33:44:55"
                );
                assert_eq!(headers.get("Authorization").unwrap(), "Basic abc");
                assert_eq!(headers.get(ANYWHERE_FORMAT_HINT_HEADER).unwrap(), "flac");
            }
            _ => panic!("expected HttpStream"),
        }
    }

    #[test]
    fn test_parse_pairing_input_code_and_uri() {
        let (srv, code) = parse_pairing_input(" abcd-efgh-jk ").unwrap();
        assert_eq!(srv, None);
        assert_eq!(code, "ABCDEFGHJK");

        let (srv2, code2) = parse_pairing_input(
            "lyrion-anywhere://pair?server=https%3A%2F%2Fmusic.example.com%2F&code=23456789ab",
        )
        .unwrap();
        assert_eq!(srv2.as_deref(), Some("https://music.example.com"));
        assert_eq!(code2, "23456789AB");
    }

    #[test]
    fn test_build_gateway_ws_url() {
        assert_eq!(
            build_gateway_ws_url("https://music.example.com/").unwrap(),
            "wss://music.example.com/v1/player"
        );
        assert_eq!(
            build_gateway_ws_url("http://10.8.0.1:9443").unwrap(),
            "ws://10.8.0.1:9443/v1/player"
        );
    }

    #[tokio::test]
    async fn test_anywhere_strm_s_rewrites_url_and_increments_seq() {
        use kiiaud_core::config::KiiaudConfig;
        use kiiaud_playback::PlaybackEngine;

        let mut cfg = KiiaudConfig::default();
        cfg.output.backend = "mock".to_owned();
        let engine = PlaybackEngine::new(&cfg);
        let handle = engine.handle();

        let mut gw_ctx = AnywhereSessionCtx {
            base_url: "https://music.example.com".to_owned(),
            session_id: "0123456789abcdef0123456789abcdef".to_owned(),
            access_token: Arc::new(RwLock::new("tok_secret".to_owned())),
            stream_seq: 0,
        };

        // Construct a 24-byte `strm` `'s'` header + HTTP request
        let mut payload = Vec::new();
        payload.extend_from_slice(b"strm");
        payload.push(b's'); // command = 's'
        payload.push(b'1'); // autostart
        payload.push(b'f'); // format = FLAC
        payload.extend_from_slice(&[0u8; 21]); // rest of 24B strm header
        payload.extend_from_slice(b"GET /stream.mp3?player=02:00:00:00:00:01 HTTP/1.0\r\n\r\n");

        let replies = handle_slimproto_server_packet(
            &payload,
            None,
            "kiiaud Gateway Player",
            &handle,
            Instant::now(),
            Some(&mut gw_ctx),
        )
        .await;

        assert_eq!(gw_ctx.stream_seq, 1);
        assert_eq!(replies.len(), 3); // STMf, STMc, RESP
    }

    #[test]
    fn test_build_stat_packet_structure_and_idle() {
        use kiiaud_core::config::KiiaudConfig;
        use kiiaud_playback::PlaybackEngine;

        let mut config = KiiaudConfig::default();
        config.output.backend = "mock".to_owned();
        let engine = PlaybackEngine::new(&config);
        let handle = engine.handle();

        let start_epoch = Instant::now();
        let stat_pkt = build_stat_packet(b"STMt", &handle, start_epoch, 12345);

        assert_eq!(&stat_pkt[0..4], b"STAT");
        let body_len =
            u32::from_be_bytes([stat_pkt[4], stat_pkt[5], stat_pkt[6], stat_pkt[7]]) as usize;
        assert_eq!(body_len, 53);
        assert_eq!(stat_pkt.len(), 8 + 53);

        let body = &stat_pkt[8..];
        assert_eq!(&body[0..4], b"STMt");

        let stream_fullness = u32::from_be_bytes([body[11], body[12], body[13], body[14]]);
        let output_fullness = u32::from_be_bytes([body[33], body[34], body[35], body[36]]);
        assert_eq!(stream_fullness, 0);
        assert_eq!(output_fullness, 0);

        let echoed_ts = u32::from_be_bytes([body[47], body[48], body[49], body[50]]);
        assert_eq!(echoed_ts, 12345);
    }
}
