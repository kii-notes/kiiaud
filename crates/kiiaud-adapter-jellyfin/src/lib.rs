// SPDX-License-Identifier: MIT OR Apache-2.0

//! Jellyfin "Play To" remote control adapter (`kiiaud-adapter-jellyfin`, Phase 5A).
//!
//! Reference workflow (`jellyfin-mpv-shim` / `R-016` / `D-014` / `docs/ADAPTERS.md` Section 1):
//! 1. **Session & Capabilities Registration**:
//!    - Registers `POST /Sessions/Capabilities/Full` with `PlayableMediaTypes: ["Audio"]`,
//!      `SupportsMediaControl: true`, and `SupportedCommands: ["Play", "Playstate", "Pause", "Unpause", "Stop", "Seek", "NextTrack", "PreviousTrack", "SetVolume"]`.
//! 2. **WebSocket Remote Control**:
//!    - Connects to `ws://<jellyfin-host>/socket?api_key=<token>&deviceId=<device_id>`.
//!    - Handles `"Play"` messages (`ItemIds`, `StartIndex`, `StartPositionTicks`) by resolving direct stream URLs:
//!      `http://<jellyfin-host>/Audio/<itemId>/stream?static=true&api_key=<token>`
//!      and dispatching `PlaybackCommand::Load` / `PlaybackCommand::Enqueue` to `kiiaud-core`.
//!    - Handles `"Playstate"` messages (`Pause`, `Unpause`, `PlayPause`, `Stop`, `Seek`, `NextTrack`, `PreviousTrack`).
//! 3. **Progress & State Reporting**:
//!    - Listens to `PlaybackEvent`s and reports `POST /Sessions/Playing`,
//!      `POST /Sessions/Playing/Progress` (`PositionTicks = elapsed_ns / 100`),
//!      and `POST /Sessions/Playing/Stopped` back to the Jellyfin server.

use futures_util::{SinkExt, StreamExt};
use kiiaud_core::config::JellyfinAdapterConfig;
use kiiaud_core::{AudioSource, PlaybackCommand, PlaybackEvent, PlayerState, Result};
use kiiaud_playback::EngineHandle;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

/// 1 Jellyfin tick = 100 nanoseconds (10,000,000 ticks per second).
pub const TICKS_PER_SECOND: u64 = 10_000_000;

/// Build the Jellyfin `Authorization` & `X-Emby-Authorization` header string.
pub fn build_auth_header(cfg: &JellyfinAdapterConfig, token_override: Option<&str>) -> String {
    let token = token_override.or(cfg.api_key.as_deref());
    match token {
        Some(tok) if !tok.is_empty() => format!(
            "MediaBrowser Client=\"kiiaud\", Device=\"{}\", DeviceId=\"{}\", Version=\"1.0.0\", Token=\"{}\"",
            cfg.device_name, cfg.device_id, tok
        ),
        _ => format!(
            "MediaBrowser Client=\"kiiaud\", Device=\"{}\", DeviceId=\"{}\", Version=\"1.0.0\"",
            cfg.device_name, cfg.device_id
        ),
    }
}

/// Construct a direct-play stream URL for a Jellyfin Audio `ItemId` (`docs/ADAPTERS.md` Section 1).
pub fn build_direct_stream_url(server_url: &str, item_id: &str, api_key: Option<&str>) -> String {
    let base = server_url.trim_end_matches('/');
    match api_key.filter(|k| !k.is_empty()) {
        Some(key) => format!("{base}/Audio/{item_id}/stream?static=true&api_key={key}"),
        None => format!("{base}/Audio/{item_id}/stream?static=true"),
    }
}

/// Construct the WebSocket endpoint URL for a Jellyfin server.
pub fn build_websocket_url(cfg: &JellyfinAdapterConfig, token_override: Option<&str>) -> String {
    let base = cfg.server_url.trim_end_matches('/');
    let ws_base = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        format!("ws://{base}")
    };
    let token = token_override.or(cfg.api_key.as_deref()).unwrap_or("");
    format!(
        "{ws_base}/socket?api_key={token}&deviceId={}",
        cfg.device_id
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct JellyfinWsEnvelope {
    pub message_type: String,
    #[serde(default)]
    pub data: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct JellyfinPlayData {
    #[serde(default)]
    pub item_ids: Vec<String>,
    pub start_index: Option<usize>,
    pub start_position_ticks: Option<u64>,
    pub play_command: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct JellyfinPlaystateData {
    pub command: String,
    pub seek_position_ticks: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct JellyfinGeneralCommandData {
    pub name: String,
    #[serde(default)]
    pub arguments: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTrack {
    pub id: String,
    pub title: Option<String>,
    pub artist: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum WsAction {
    None,
    SendKeepAlive,
    TrackStarted(String),
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
struct ProgressReport<'a> {
    item_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    playlist_item_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    media_source_id: Option<&'a str>,
    position_ticks: u64,
    is_paused: bool,
    is_muted: bool,
    volume_level: u8,
    can_seek: bool,
    play_method: &'static str,
}

/// Start the Jellyfin "Play To" remote control adapter with automatic reconnect (`NFR-007`).
pub async fn run_jellyfin_adapter(
    config: JellyfinAdapterConfig,
    engine: EngineHandle,
) -> Result<()> {
    if !config.enabled {
        return Ok(());
    }

    tracing::info!(
        server = %config.server_url,
        device_name = %config.device_name,
        "starting Jellyfin 'Play To' remote control adapter"
    );

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("failed to build reqwest client");

    loop {
        if let Err(e) = run_jellyfin_session(&client, &config, &engine).await {
            tracing::warn!(
                server = %config.server_url,
                "Jellyfin session disconnected: {e}; reconnecting in 5s"
            );
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn run_jellyfin_session(
    client: &reqwest::Client,
    config: &JellyfinAdapterConfig,
    engine: &EngineHandle,
) -> std::result::Result<(), String> {
    // 0. Resolve token (from api_key or by authenticating with username & password)
    let active_token: Option<String> =
        if let Some(key) = config.api_key.as_deref().filter(|k| !k.is_empty()) {
            Some(key.to_owned())
        } else if let (Some(username), Some(password)) = (&config.username, &config.password) {
            if !username.is_empty() {
                let auth_url = format!(
                    "{}/Users/AuthenticateByName",
                    config.server_url.trim_end_matches('/')
                );
                let unauth_header = build_auth_header(config, None);
                let resp = client
                    .post(&auth_url)
                    .header("Authorization", &unauth_header)
                    .header("X-Emby-Authorization", &unauth_header)
                    .json(&json!({
                        "Username": username,
                        "Pw": password,
                    }))
                    .send()
                    .await
                    .map_err(|e| format!("Jellyfin authentication request error: {e}"))?;

                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    return Err(format!(
                        "Jellyfin authentication failed (HTTP {status}): {body}"
                    ));
                }

                let auth_data: serde_json::Value = resp
                    .json()
                    .await
                    .map_err(|e| format!("Failed to parse authentication response: {e}"))?;

                let access_token = auth_data["AccessToken"]
                    .as_str()
                    .ok_or_else(|| "No AccessToken in authentication response".to_string())?
                    .to_owned();

                tracing::info!(user = %username, "authenticated successfully with Jellyfin server");
                Some(access_token)
            } else {
                None
            }
        } else {
            None
        };

    let token_ref = active_token.as_deref();
    let auth = build_auth_header(config, token_ref);

    // 1. Register capabilities immediately with valid GeneralCommandType enum values
    post_capabilities(client, config, &auth).await;

    // 2. Connect WebSocket with full authorization headers
    let ws_url = build_websocket_url(config, token_ref);
    let mut ws_req = ws_url
        .into_client_request()
        .map_err(|e| format!("Invalid WS request: {e}"))?;
    if let Ok(hdr_val) = auth.parse() {
        ws_req.headers_mut().insert("Authorization", hdr_val);
    }
    if let Ok(hdr_val) = auth.parse() {
        ws_req.headers_mut().insert("X-Emby-Authorization", hdr_val);
    }
    let (ws_stream, _) = connect_async(ws_req)
        .await
        .map_err(|e| format!("WebSocket connect failed: {e}"))?;

    tracing::info!(
        server = %config.server_url,
        device_name = %config.device_name,
        "connected to Jellyfin WebSocket as 'Play To' target"
    );

    // Re-affirm capabilities after WebSocket connects (reference: jellyfin-mpv-shim)
    post_capabilities(client, config, &auth).await;

    let (mut ws_write, mut ws_read) = ws_stream.split();
    let mut event_rx = engine.subscribe();
    let mut current_jellyfin_item_id: Option<String> = None;
    let mut last_progress_sent = tokio::time::Instant::now();

    // Heartbeat ticker to keep Jellyfin WebSocket alive (Jellyfin drops inactive sockets after 60s)
    let mut keepalive_ticker = tokio::time::interval(Duration::from_secs(15));

    loop {
        tokio::select! {
            _ = keepalive_ticker.tick() => {
                let ka_msg = json!({ "MessageType": "KeepAlive" }).to_string();
                if let Err(e) = ws_write.send(Message::Text(ka_msg.into())).await {
                    return Err(format!("KeepAlive send error: {e}"));
                }
                let _ = ws_write.send(Message::Ping(vec![].into())).await;
            }
            msg_opt = ws_read.next() => {
                let Some(msg_res) = msg_opt else {
                    return Err("WebSocket stream closed".to_owned());
                };
                let msg = msg_res.map_err(|e| format!("WebSocket read error: {e}"))?;
                match msg {
                    Message::Text(text) => {
                        let action = handle_ws_text_message(&text, client, config, token_ref, engine).await;
                        match action {
                            WsAction::SendKeepAlive => {
                                let ka_msg = json!({ "MessageType": "KeepAlive" }).to_string();
                                let _ = ws_write.send(Message::Text(ka_msg.into())).await;
                            }
                            WsAction::TrackStarted(active_id) => {
                                current_jellyfin_item_id = Some(active_id);
                            }
                            WsAction::None => {}
                        }
                    }
                    Message::Ping(payload) => {
                        let _ = ws_write.send(Message::Pong(payload)).await;
                    }
                    Message::Close(_) => return Ok(()),
                    _ => {}
                }
            }
            Ok(ev) = event_rx.recv() => {
                match &ev {
                    PlaybackEvent::TrackStarted { item, .. } => {
                        if let AudioSource::HttpStream { url, .. } = &item.source {
                            if let Some(new_id) = extract_jellyfin_item_id(url) {
                                current_jellyfin_item_id = Some(new_id);
                            }
                        }
                        if let Some(item_id) = current_jellyfin_item_id.as_deref() {
                            report_event_to_jellyfin(client, config, &auth, item_id, &ev, engine).await;
                            last_progress_sent = tokio::time::Instant::now();
                        }
                    }
                    PlaybackEvent::PositionUpdated { .. } => {
                        if last_progress_sent.elapsed() >= Duration::from_millis(1000) {
                            if let Some(item_id) = current_jellyfin_item_id.as_deref() {
                                report_event_to_jellyfin(client, config, &auth, item_id, &ev, engine).await;
                                last_progress_sent = tokio::time::Instant::now();
                            }
                        }
                    }
                    PlaybackEvent::StateChanged(_) => {
                        if let Some(item_id) = current_jellyfin_item_id.as_deref() {
                            report_event_to_jellyfin(client, config, &auth, item_id, &ev, engine).await;
                            last_progress_sent = tokio::time::Instant::now();
                        }
                    }
                    PlaybackEvent::TrackEnded(_) => {
                        if let Some(item_id) = current_jellyfin_item_id.as_deref() {
                            report_event_to_jellyfin(client, config, &auth, item_id, &ev, engine).await;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Register session capabilities with Jellyfin (`POST /Sessions/Capabilities/Full`).
pub async fn post_capabilities(
    client: &reqwest::Client,
    config: &JellyfinAdapterConfig,
    auth: &str,
) {
    let cap_url = format!(
        "{}/Sessions/Capabilities/Full",
        config.server_url.trim_end_matches('/')
    );
    // GeneralCommandType enum names must match Jellyfin server schema exactly
    let cap_body = json!({
        "PlayableMediaTypes": ["Audio", "Video"],
        "SupportsMediaControl": true,
        "SupportsPersistentIdentifier": true,
        "SupportedCommands": [
            "Play",
            "PlayState",
            "PlayNext",
            "SetVolume",
            "VolumeUp",
            "VolumeDown",
            "Mute",
            "Unmute",
            "ToggleMute",
            "DisplayContent",
            "SetRepeatMode",
            "SetShuffleQueue",
            "SetPlaybackOrder"
        ]
    });

    match client
        .post(&cap_url)
        .header("Authorization", auth)
        .header("X-Emby-Authorization", auth)
        .json(&cap_body)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            tracing::info!(
                server = %config.server_url,
                device = %config.device_name,
                "registered session capabilities successfully with Jellyfin"
            );
        }
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(status = %status, %body, "Jellyfin capabilities registration rejected");
        }
        Err(e) => {
            tracing::warn!("capabilities registration network error: {}", e.without_url());
        }
    }
}

/// Extract track title and primary artist name from a Jellyfin Item JSON object.
fn extract_title_artist(item: &serde_json::Value) -> (Option<String>, Option<String>) {
    let title = item.get("Name").and_then(|v| v.as_str()).map(str::to_owned);
    let artist = item
        .get("Artists")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .or_else(|| {
            item.get("AlbumArtist")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        });
    (title, artist)
}

/// Extract the Jellyfin audio item ID from a direct-stream URL (`/Audio/<itemId>/stream`).
pub fn extract_jellyfin_item_id(url: &str) -> Option<String> {
    let after_audio = url.split("/Audio/").nth(1)?;
    let item_id = after_audio.split('/').next()?;
    if !item_id.is_empty() {
        Some(item_id.to_owned())
    } else {
        None
    }
}

/// Resolve item IDs from a Play command into full tracklists (expanding albums/folders if applicable).
pub async fn resolve_play_items(
    client: &reqwest::Client,
    server_url: &str,
    item_ids: &[String],
    start_index: Option<usize>,
    auth: &str,
) -> (Vec<ResolvedTrack>, usize) {
    if item_ids.is_empty() {
        return (Vec::new(), 0);
    }

    let base = server_url.trim_end_matches('/');

    // Single item passed (e.g. user clicked a track or an album/playlist)
    if item_ids.len() == 1 {
        let first_id = &item_ids[0];
        let url = format!("{base}/Items/{first_id}");
        let item_json: Option<serde_json::Value> = match client
            .get(&url)
            .header("Authorization", auth)
            .header("X-Emby-Authorization", auth)
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => r.json().await.ok(),
            _ => None,
        };

        if let Some(item) = item_json {
            let item_type = item.get("Type").and_then(|v| v.as_str()).unwrap_or("");
            let is_folder = item.get("IsFolder").and_then(|v| v.as_bool()).unwrap_or(false);

            // Container / Folder (e.g. MusicAlbum, Playlist, Folder)
            if is_folder
                || matches!(
                    item_type,
                    "MusicAlbum" | "Playlist" | "MusicArtist" | "Folder" | "BoxSet"
                )
            {
                let children_url =
                    format!("{base}/Items?ParentId={first_id}&SortBy=IndexNumber,SortName");
                if let Ok(resp) = client
                    .get(&children_url)
                    .header("Authorization", auth)
                    .header("X-Emby-Authorization", auth)
                    .send()
                    .await
                {
                    if let Ok(container) = resp.json::<serde_json::Value>().await {
                        if let Some(items_arr) = container.get("Items").and_then(|v| v.as_array()) {
                            let tracks: Vec<ResolvedTrack> = items_arr
                                .iter()
                                .filter_map(|it| {
                                    let id = it.get("Id")?.as_str()?.to_owned();
                                    let (title, artist) = extract_title_artist(it);
                                    Some(ResolvedTrack { id, title, artist })
                                })
                                .collect();

                            if !tracks.is_empty() {
                                let selected_idx =
                                    start_index.unwrap_or(0).min(tracks.len() - 1);
                                return (tracks, selected_idx);
                            }
                        }
                    }
                }
            }

            // Single audio item: check if it belongs to an album/playlist
            let (title, artist) = extract_title_artist(&item);
            let parent_id = item
                .get("AlbumId")
                .and_then(|v| v.as_str())
                .or_else(|| item.get("ParentId").and_then(|v| v.as_str()));

            if let Some(pid) = parent_id {
                let album_url = format!("{base}/Items?ParentId={pid}&SortBy=IndexNumber,SortName");
                if let Ok(resp) = client
                    .get(&album_url)
                    .header("Authorization", auth)
                    .header("X-Emby-Authorization", auth)
                    .send()
                    .await
                {
                    if let Ok(container) = resp.json::<serde_json::Value>().await {
                        if let Some(items_arr) = container.get("Items").and_then(|v| v.as_array()) {
                            let tracks: Vec<ResolvedTrack> = items_arr
                                .iter()
                                .filter_map(|it| {
                                    let id = it.get("Id")?.as_str()?.to_owned();
                                    let (t, a) = extract_title_artist(it);
                                    Some(ResolvedTrack {
                                        id,
                                        title: t,
                                        artist: a,
                                    })
                                })
                                .collect();

                            if let Some(idx) = tracks.iter().position(|t| t.id == *first_id) {
                                return (tracks, idx);
                            }
                        }
                    }
                }
            }

            // Fallback: single item without parent expansion
            return (
                vec![ResolvedTrack {
                    id: first_id.clone(),
                    title,
                    artist,
                }],
                0,
            );
        }

        return (
            vec![ResolvedTrack {
                id: first_id.clone(),
                title: None,
                artist: None,
            }],
            0,
        );
    }

    // Multiple item IDs passed: fetch metadata in a single batch query
    let ids_param = item_ids.join(",");
    let batch_url = format!("{base}/Items?Ids={ids_param}");
    let mut meta_map: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();

    if let Ok(resp) = client
        .get(&batch_url)
        .header("Authorization", auth)
        .header("X-Emby-Authorization", auth)
        .send()
        .await
    {
        if let Ok(container) = resp.json::<serde_json::Value>().await {
            if let Some(items_arr) = container.get("Items").and_then(|v| v.as_array()) {
                for it in items_arr {
                    if let Some(id) = it.get("Id").and_then(|v| v.as_str()) {
                        let (title, artist) = extract_title_artist(it);
                        meta_map.insert(id.to_owned(), (title, artist));
                    }
                }
            }
        }
    }

    let tracks: Vec<ResolvedTrack> = item_ids
        .iter()
        .map(|id| {
            let (title, artist) = meta_map.remove(id).unwrap_or((None, None));
            ResolvedTrack {
                id: id.clone(),
                title,
                artist,
            }
        })
        .collect();

    let selected_idx = start_index.unwrap_or(0).min(tracks.len() - 1);
    (tracks, selected_idx)
}

/// Process a JSON text message received over the Jellyfin WebSocket.
pub async fn handle_ws_text_message(
    text: &str,
    client: &reqwest::Client,
    config: &JellyfinAdapterConfig,
    active_token: Option<&str>,
    engine: &EngineHandle,
) -> WsAction {
    let env: JellyfinWsEnvelope = match serde_json::from_str(text) {
        Ok(env) => env,
        Err(_) => return WsAction::None,
    };

    match env.message_type.as_str() {
        "ForceKeepAlive" => {
            tracing::debug!("received ForceKeepAlive from Jellyfin server; replying KeepAlive");
            WsAction::SendKeepAlive
        }
        "KeepAlive" => {
            tracing::trace!("received KeepAlive ping acknowledgment from Jellyfin server");
            WsAction::None
        }
        "Play" => {
            let play_data: JellyfinPlayData = match serde_json::from_value(env.data) {
                Ok(pd) => pd,
                Err(e) => {
                    tracing::warn!("failed to deserialize Play command data: {e}");
                    return WsAction::None;
                }
            };
            if play_data.item_ids.is_empty() {
                return WsAction::None;
            }

            let auth = build_auth_header(config, active_token);

            let (resolved_tracks, start_idx) = resolve_play_items(
                client,
                &config.server_url,
                &play_data.item_ids,
                play_data.start_index,
                &auth,
            )
            .await;

            if resolved_tracks.is_empty() {
                return WsAction::None;
            }

            let mut headers = HashMap::new();
            headers.insert("Authorization".to_owned(), auth.clone());
            headers.insert("X-Emby-Authorization".to_owned(), auth.clone());

            let is_enqueue = play_data.play_command.as_deref().is_some_and(|c| {
                c.eq_ignore_ascii_case("PlayLast") || c.eq_ignore_ascii_case("PlayNext")
            });

            if !is_enqueue {
                let _ = engine.send(PlaybackCommand::Clear).await;
            }

            let mut active_id = None;

            for (idx, track) in resolved_tracks.iter().enumerate() {
                let url = build_direct_stream_url(
                    &config.server_url,
                    &track.id,
                    active_token.or(config.api_key.as_deref()),
                );
                let source = AudioSource::HttpStream {
                    url,
                    headers: headers.clone(),
                };

                let title = track
                    .title
                    .clone()
                    .or_else(|| Some(format!("Jellyfin Item {}", track.id)));
                let artist = track.artist.clone();

                let _ = engine
                    .send(PlaybackCommand::Enqueue {
                        source,
                        title,
                        artist,
                    })
                    .await;
                if idx == start_idx {
                    active_id = Some(track.id.clone());
                }
            }

            if !is_enqueue {
                let _ = engine
                    .send(PlaybackCommand::Play(Some(start_idx as u32)))
                    .await;
                if let Some(ticks) = play_data.start_position_ticks.filter(|&t| t > 0) {
                    let secs = ticks as f64 / TICKS_PER_SECOND as f64;
                    let _ = engine
                        .send(PlaybackCommand::Seek(Duration::from_secs_f64(secs)))
                        .await;
                }
            }
            match active_id {
                Some(id) => WsAction::TrackStarted(id),
                None => WsAction::None,
            }
        }
        "Playstate" | "PlayState" => {
            let ps: JellyfinPlaystateData = match serde_json::from_value(env.data) {
                Ok(ps) => ps,
                Err(e) => {
                    tracing::warn!("failed to deserialize Playstate command: {e}");
                    return WsAction::None;
                }
            };
            match ps.command.as_str() {
                "Pause" => {
                    let _ = engine.send(PlaybackCommand::Pause).await;
                }
                "Unpause" => {
                    let _ = engine.send(PlaybackCommand::Resume).await;
                }
                "PlayPause" => {
                    let _ = engine.send(PlaybackCommand::TogglePause).await;
                }
                "Stop" => {
                    let _ = engine.send(PlaybackCommand::Stop).await;
                }
                "NextTrack" => {
                    let _ = engine.send(PlaybackCommand::Next).await;
                }
                "PreviousTrack" => {
                    let _ = engine.send(PlaybackCommand::Previous).await;
                }
                "Seek" => {
                    if let Some(ticks) = ps.seek_position_ticks {
                        let secs = ticks as f64 / TICKS_PER_SECOND as f64;
                        let _ = engine
                            .send(PlaybackCommand::Seek(Duration::from_secs_f64(secs)))
                            .await;
                    }
                }
                cmd => {
                    tracing::debug!(cmd, "unhandled Playstate command");
                }
            }
            WsAction::None
        }
        "GeneralCommand" => {
            let gc: JellyfinGeneralCommandData = match serde_json::from_value(env.data) {
                Ok(gc) => gc,
                Err(e) => {
                    tracing::warn!("failed to deserialize GeneralCommand: {e}");
                    return WsAction::None;
                }
            };
            match gc.name.as_str() {
                "SetVolume" => {
                    if let Some(vol_val) = gc.arguments.get("Volume") {
                        let vol = vol_val
                            .as_u64()
                            .or_else(|| vol_val.as_str().and_then(|s| s.parse().ok()))
                            .unwrap_or(100) as u8;
                        let _ = engine.send(PlaybackCommand::SetVolume(vol)).await;
                    }
                }
                "VolumeUp" => {
                    let cur = engine.status().volume;
                    let _ = engine
                        .send(PlaybackCommand::SetVolume(cur.saturating_add(5).min(100)))
                        .await;
                }
                "VolumeDown" => {
                    let cur = engine.status().volume;
                    let _ = engine
                        .send(PlaybackCommand::SetVolume(cur.saturating_sub(5)))
                        .await;
                }
                "Mute" => {
                    let _ = engine.send(PlaybackCommand::SetVolume(0)).await;
                }
                "Unmute" => {
                    let _ = engine.send(PlaybackCommand::SetVolume(100)).await;
                }
                "ToggleMute" => {
                    let cur = engine.status().volume;
                    let new_vol = if cur == 0 { 100 } else { 0 };
                    let _ = engine.send(PlaybackCommand::SetVolume(new_vol)).await;
                }
                "SetRepeatMode" => {
                    if let Some(rm_val) = gc.arguments.get("RepeatMode").and_then(|v| v.as_str()) {
                        match rm_val {
                            "RepeatAll" => {
                                let _ = engine.send(PlaybackCommand::SetRepeat(true)).await;
                                let _ = engine.send(PlaybackCommand::SetSingle(false)).await;
                            }
                            "RepeatOne" => {
                                let _ = engine.send(PlaybackCommand::SetRepeat(true)).await;
                                let _ = engine.send(PlaybackCommand::SetSingle(true)).await;
                            }
                            "RepeatNone" => {
                                let _ = engine.send(PlaybackCommand::SetRepeat(false)).await;
                                let _ = engine.send(PlaybackCommand::SetSingle(false)).await;
                            }
                            _ => {}
                        }
                    }
                }
                "SetShuffleQueue" | "SetPlaybackOrder" => {
                    if let Some(sm_val) = gc
                        .arguments
                        .get("ShuffleMode")
                        .or_else(|| gc.arguments.get("PlaybackOrder"))
                        .and_then(|v| v.as_str())
                    {
                        match sm_val {
                            "Shuffle" => {
                                let _ = engine.send(PlaybackCommand::SetRandom(true)).await;
                            }
                            "Sorted" | "Default" => {
                                let _ = engine.send(PlaybackCommand::SetRandom(false)).await;
                            }
                            _ => {}
                        }
                    }
                }
                name => {
                    tracing::debug!(name, "unhandled GeneralCommand");
                }
            }
            WsAction::None
        }
        _ => WsAction::None,
    }
}

async fn report_event_to_jellyfin(
    client: &reqwest::Client,
    config: &JellyfinAdapterConfig,
    auth: &str,
    item_id: &str,
    ev: &PlaybackEvent,
    engine: &EngineHandle,
) {
    let base = config.server_url.trim_end_matches('/');
    let st = engine.status();
    let ticks = (st.elapsed.as_secs_f64() * TICKS_PER_SECOND as f64) as u64;
    let report = ProgressReport {
        item_id,
        playlist_item_id: Some(item_id),
        media_source_id: Some(item_id),
        position_ticks: ticks,
        is_paused: st.state == PlayerState::Pause,
        is_muted: false,
        volume_level: st.volume,
        can_seek: true,
        play_method: "DirectStream",
    };

    let endpoint = match ev {
        PlaybackEvent::TrackStarted { .. } => format!("{base}/Sessions/Playing"),
        PlaybackEvent::PositionUpdated { .. } | PlaybackEvent::StateChanged(_) => {
            if st.state == PlayerState::Stop {
                format!("{base}/Sessions/Playing/Stopped")
            } else {
                format!("{base}/Sessions/Playing/Progress")
            }
        }
        PlaybackEvent::TrackEnded(_) => format!("{base}/Sessions/Playing/Stopped"),
        _ => return,
    };

    let _ = client
        .post(&endpoint)
        .header("Authorization", auth)
        .header("X-Emby-Authorization", auth)
        .json(&report)
        .send()
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_direct_stream_url_generation() {
        let url = build_direct_stream_url(
            "http://jellyfin.local:8096/",
            "abc12345",
            Some("secret_key"),
        );
        assert_eq!(
            url,
            "http://jellyfin.local:8096/Audio/abc12345/stream?static=true&api_key=secret_key"
        );
    }

    #[test]
    fn test_websocket_url_generation() {
        let cfg = JellyfinAdapterConfig {
            enabled: true,
            server_url: "https://media.example.com".to_owned(),
            api_key: Some("tok123".to_owned()),
            username: None,
            password: None,
            device_id: "rpi5".to_owned(),
            device_name: "kiiaud".to_owned(),
        };
        assert_eq!(
            build_websocket_url(&cfg, None),
            "wss://media.example.com/socket?api_key=tok123&deviceId=rpi5"
        );
    }

    #[tokio::test]
    async fn test_ws_force_keepalive_handling() {
        let engine = kiiaud_playback::PlaybackEngine::with_output(
            Box::new(kiiaud_output::MockAudioOutput::new()),
            kiiaud_core::PathPolicy::default(),
        );
        let handle = engine.handle();
        let client = reqwest::Client::new();
        let cfg = JellyfinAdapterConfig {
            enabled: true,
            server_url: "https://media.example.com".to_owned(),
            api_key: Some("tok123".to_owned()),
            username: None,
            password: None,
            device_id: "rpi5".to_owned(),
            device_name: "kiiaud".to_owned(),
        };

        let raw_msg = r#"{"MessageId":"123","MessageType":"ForceKeepAlive","Data":60}"#;
        let action = handle_ws_text_message(raw_msg, &client, &cfg, Some("tok123"), &handle).await;
        assert_eq!(action, WsAction::SendKeepAlive);
    }

    #[tokio::test]
    async fn test_ws_general_command_volume_handling() {
        let engine = kiiaud_playback::PlaybackEngine::with_output(
            Box::new(kiiaud_output::MockAudioOutput::new()),
            kiiaud_core::PathPolicy::default(),
        );
        let handle = engine.handle();
        let client = reqwest::Client::new();
        let cfg = JellyfinAdapterConfig {
            enabled: true,
            server_url: "https://media.example.com".to_owned(),
            api_key: Some("tok123".to_owned()),
            username: None,
            password: None,
            device_id: "rpi5".to_owned(),
            device_name: "kiiaud".to_owned(),
        };

        let raw_msg = r#"{"MessageId":"124","MessageType":"GeneralCommand","Data":{"Name":"SetVolume","Arguments":{"Volume":65}}}"#;
        let action = handle_ws_text_message(raw_msg, &client, &cfg, Some("tok123"), &handle).await;
        assert_eq!(action, WsAction::None);
    }

    #[test]
    fn test_extract_jellyfin_item_id() {
        assert_eq!(
            extract_jellyfin_item_id("http://localhost:8096/Audio/abc12345/stream?static=true"),
            Some("abc12345".to_string())
        );
        assert_eq!(
            extract_jellyfin_item_id("https://media.domain.net/Audio/def67890/stream"),
            Some("def67890".to_string())
        );
        assert_eq!(
            extract_jellyfin_item_id("https://media.domain.net/Videos/123/stream"),
            None
        );
    }

    #[test]
    fn test_extract_title_artist() {
        let val = json!({
            "Name": "Sample Track",
            "Artists": ["Test Artist 1", "Test Artist 2"]
        });
        let (title, artist) = extract_title_artist(&val);
        assert_eq!(title, Some("Sample Track".to_string()));
        assert_eq!(artist, Some("Test Artist 1".to_string()));

        let val_album_artist = json!({
            "Name": "Solo Track",
            "AlbumArtist": "Solo Artist"
        });
        let (title2, artist2) = extract_title_artist(&val_album_artist);
        assert_eq!(title2, Some("Solo Track".to_string()));
        assert_eq!(artist2, Some("Solo Artist".to_string()));
    }

    #[tokio::test]
    async fn test_ws_general_command_repeat_and_shuffle() {
        let engine = kiiaud_playback::PlaybackEngine::with_output(
            Box::new(kiiaud_output::MockAudioOutput::new()),
            kiiaud_core::PathPolicy::default(),
        );
        let handle = engine.handle();
        let client = reqwest::Client::new();
        let cfg = JellyfinAdapterConfig {
            enabled: true,
            server_url: "https://media.example.com".to_owned(),
            api_key: Some("tok123".to_owned()),
            username: None,
            password: None,
            device_id: "rpi5".to_owned(),
            device_name: "kiiaud".to_owned(),
        };

        let repeat_msg = r#"{"MessageId":"125","MessageType":"GeneralCommand","Data":{"Name":"SetRepeatMode","Arguments":{"RepeatMode":"RepeatAll"}}}"#;
        let action = handle_ws_text_message(repeat_msg, &client, &cfg, Some("tok123"), &handle).await;
        assert_eq!(action, WsAction::None);

        let shuffle_msg = r#"{"MessageId":"126","MessageType":"GeneralCommand","Data":{"Name":"SetShuffleQueue","Arguments":{"ShuffleMode":"Shuffle"}}}"#;
        let action2 = handle_ws_text_message(shuffle_msg, &client, &cfg, Some("tok123"), &handle).await;
        assert_eq!(action2, WsAction::None);
    }
}
