// SPDX-License-Identifier: MIT OR Apache-2.0

//! Lightweight database-free MPD protocol control adapter (`kiiaud-adapter-mpd`, Phase 5C).
//!
//! Listens on TCP port 6600 (configurable) and implements the MPD playback & queue control subset:
//! - Connection & Info: `status`, `currentsong`, `stats`, `ping`, `close`, `commands`, `decoders`, `outputs`
//! - Queue Management: `clear`, `add <path_or_url>`, `addid <path_or_url>`, `delete <pos>`, `deleteid <id>`, `playlistinfo`, `plchanges`
//! - Playback Control: `play [pos]`, `playid [id]`, `pause [state]`, `stop`, `next`, `previous`, `seek <pos> <time>`, `seekid <id> <time>`, `seekcur <time>`
//! - Playback Options: `consume`, `repeat`, `random`, `single`, `setvol`, `volume`
//! - Idle: `idle`, `noidle`
//! - Command lists: `command_list_begin`, `command_list_ok_begin`, `command_list_end`
//! - Database commands (`find`, `search`, `list`, `update`, `rescan`, `lsinfo`) return explicit non-fatal empty/unsupported responses (`docs/ADAPTERS.md` Section 3).

use kiiaud_core::{
    AudioSource, KiiaudError, PlaybackCommand, PlaybackEvent, PlaybackItem, PlayerStatus, Result,
};
use kiiaud_playback::EngineHandle;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

pub const MPD_GREETING: &str = "OK MPD 0.23.5\n";

/// Start the MPD TCP server on `listen_addr` without explicit music directory root.
pub async fn run_mpd_server(listen_addr: String, engine: EngineHandle) -> Result<()> {
    run_mpd_server_with_dir(listen_addr, None, engine).await
}

/// Start the MPD TCP server with an optional `music_directory` used for direct filesystem traversal.
pub async fn run_mpd_server_with_dir(
    listen_addr: String,
    music_dir: Option<PathBuf>,
    engine: EngineHandle,
) -> Result<()> {
    let listener = TcpListener::bind(&listen_addr).await.map_err(|e| {
        KiiaudError::Io(format!(
            "Failed to bind MPD adapter on '{listen_addr}': {e}"
        ))
    })?;

    tracing::info!(
        listen = %listen_addr,
        music_dir = ?music_dir,
        "MPD control adapter listening (database-free mode)"
    );

    let music_dir_arc = music_dir.map(std::sync::Arc::new);

    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                tracing::debug!(%peer, "MPD client connected");
                let eng = engine.clone();
                let mdir = music_dir_arc.clone();
                tokio::spawn(async move {
                    if let Err(e) =
                        handle_mpd_client(stream, eng, mdir.as_deref().map(|p| p.as_path())).await
                    {
                        tracing::debug!(%peer, "MPD client session ended: {e}");
                    }
                });
            }
            Err(e) => {
                tracing::warn!("MPD accept error: {e}");
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListMode {
    None,
    Normal,
    OkList,
}

async fn handle_mpd_client(
    stream: TcpStream,
    engine: EngineHandle,
    music_dir: Option<&Path>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    writer.write_all(MPD_GREETING.as_bytes()).await?;

    let mut lines = BufReader::new(reader).lines();
    let mut list_mode = ListMode::None;
    let mut queued_commands: Vec<String> = Vec::new();

    while let Some(line) = lines.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match trimmed {
            "command_list_begin" => {
                list_mode = ListMode::Normal;
                queued_commands.clear();
                continue;
            }
            "command_list_ok_begin" => {
                list_mode = ListMode::OkList;
                queued_commands.clear();
                continue;
            }
            "command_list_end" => {
                let mode = list_mode;
                list_mode = ListMode::None;
                let cmds = std::mem::take(&mut queued_commands);
                let mut out_buf = String::new();
                let mut should_close = false;

                for (idx, cmd_line) in cmds.iter().enumerate() {
                    match execute_mpd_line_with_dir(cmd_line, &engine, music_dir, idx).await {
                        Ok(CmdOutcome::Reply(text)) => {
                            out_buf.push_str(&text);
                            if mode == ListMode::OkList {
                                out_buf.push_str("list_OK\n");
                            }
                        }
                        Ok(CmdOutcome::Close) => {
                            should_close = true;
                            break;
                        }
                        Err(ack) => {
                            out_buf.push_str(&ack);
                            break;
                        }
                    }
                }
                if !should_close && !out_buf.ends_with('\n') || !out_buf.contains("ACK [") {
                    if !should_close {
                        out_buf.push_str("OK\n");
                    }
                }
                writer.write_all(out_buf.as_bytes()).await?;
                if should_close {
                    break;
                }
                continue;
            }
            _ => {}
        }

        if list_mode != ListMode::None {
            queued_commands.push(trimmed.to_owned());
            continue;
        }

        // Special handling for `idle` (waits for an event or `noidle`)
        if trimmed == "idle" || trimmed.starts_with("idle ") {
            let mut event_rx = engine.subscribe();
            tokio::select! {
                ev = event_rx.recv() => {
                    let subsystem = match ev {
                        Ok(PlaybackEvent::QueueUpdated { .. }) => "changed: playlist\nOK\n",
                        Ok(PlaybackEvent::VolumeChanged(_)) => "changed: mixer\nOK\n",
                        Ok(PlaybackEvent::OptionsChanged) => "changed: options\nOK\n",
                        _ => "changed: player\nOK\n",
                    };
                    writer.write_all(subsystem.as_bytes()).await?;
                }
                next_line = lines.next_line() => {
                    match next_line? {
                        Some(l) if l.trim() == "noidle" => {
                            writer.write_all(b"OK\n").await?;
                        }
                        Some(_) => {
                            writer.write_all(b"OK\n").await?;
                        }
                        None => break,
                    }
                }
            }
            continue;
        }

        match execute_mpd_line_with_dir(trimmed, &engine, music_dir, 0).await {
            Ok(CmdOutcome::Reply(mut text)) => {
                text.push_str("OK\n");
                writer.write_all(text.as_bytes()).await?;
            }
            Ok(CmdOutcome::Close) => break,
            Err(ack) => {
                writer.write_all(ack.as_bytes()).await?;
            }
        }
    }

    Ok(())
}

#[derive(Debug)]
pub enum CmdOutcome {
    Reply(String),
    Close,
}

pub use kiiaud_core::is_supported_audio_extension;

/// Recursively collect all supported audio files within `dir`, sorted alphabetically.
pub fn collect_audio_files_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
    out.extend(kiiaud_core::collect_audio_files_recursive(dir));
}

/// Resolve a URI or path relative to the optional `music_directory`.
pub fn resolve_music_path(uri: &str, music_dir: Option<&Path>) -> PathBuf {
    if uri.starts_with("http://") || uri.starts_with("https://") || uri.starts_with("tcp://") {
        return PathBuf::from(uri);
    }
    let p = Path::new(uri);
    if p.is_absolute() && p.exists() {
        return p.to_path_buf();
    }
    if let Some(base) = music_dir {
        let joined = base.join(uri);
        if joined.exists() {
            return joined;
        }
    }
    if p.exists() {
        return p.to_path_buf();
    }
    if let Some(base) = music_dir {
        base.join(uri)
    } else {
        p.to_path_buf()
    }
}

/// Execute a single MPD command line (backwards-compatible wrapper without `music_dir`).
pub async fn execute_mpd_line(
    line: &str,
    engine: &EngineHandle,
    list_idx: usize,
) -> std::result::Result<CmdOutcome, String> {
    execute_mpd_line_with_dir(line, engine, None, list_idx).await
}

/// Execute a single MPD command line with an optional `music_dir` for file traversal.
pub async fn execute_mpd_line_with_dir(
    line: &str,
    engine: &EngineHandle,
    music_dir: Option<&Path>,
    list_idx: usize,
) -> std::result::Result<CmdOutcome, String> {
    let args = split_mpd_args(line);
    let Some((cmd_raw, rest)) = args.split_first() else {
        return Ok(CmdOutcome::Reply(String::new()));
    };
    let cmd = cmd_raw.to_ascii_lowercase();

    match cmd.as_str() {
        "ping" | "clearerror" | "tagtypes" => Ok(CmdOutcome::Reply(String::new())),
        "close" => Ok(CmdOutcome::Close),
        "status" => {
            let st = engine.status();
            Ok(CmdOutcome::Reply(format_mpd_status(&st)))
        }
        "stats" => {
            let st = engine.status();
            let mut out = String::new();
            let _ = writeln!(out, "artists: 0");
            let _ = writeln!(out, "albums: 0");
            let _ = writeln!(out, "songs: {}", st.queue_length);
            let _ = writeln!(out, "uptime: 1");
            let _ = writeln!(out, "db_playtime: 0");
            let _ = writeln!(out, "db_update: 0");
            let _ = writeln!(out, "playtime: {}", st.elapsed.as_secs());
            Ok(CmdOutcome::Reply(out))
        }
        "currentsong" => {
            let st = engine.status();
            let mut out = String::new();
            if let Some(item) = &st.current_item {
                format_mpd_song(&mut out, item);
            }
            Ok(CmdOutcome::Reply(out))
        }
        "playlistinfo" | "plchanges" => {
            let items = engine.queue_items();
            let mut out = String::new();
            for item in &items {
                format_mpd_song(&mut out, item);
            }
            Ok(CmdOutcome::Reply(out))
        }
        "outputs" => {
            let mut out = String::new();
            let _ = writeln!(out, "outputid: 0");
            let _ = writeln!(out, "outputname: ALSA Bit-Perfect Output");
            let _ = writeln!(out, "plugin: alsa");
            let _ = writeln!(out, "outputenabled: 1");
            Ok(CmdOutcome::Reply(out))
        }
        "decoders" => {
            let mut out = String::new();
            let _ = writeln!(
                out,
                "plugin: symphonia\nsuffix: wv\nsuffix: flac\nsuffix: wav\nsuffix: aiff\nsuffix: dsf\nsuffix: dff\nsuffix: mp3\nsuffix: ogg\nsuffix: opus"
            );
            Ok(CmdOutcome::Reply(out))
        }
        "commands" => {
            let mut out = String::new();
            for c in [
                "add",
                "addid",
                "clear",
                "close",
                "commands",
                "consume",
                "currentsong",
                "decoders",
                "delete",
                "deleteid",
                "idle",
                "lsinfo",
                "move",
                "moveid",
                "next",
                "outputs",
                "pause",
                "ping",
                "play",
                "playid",
                "playlistinfo",
                "plchanges",
                "previous",
                "random",
                "repeat",
                "seek",
                "seekcur",
                "seekid",
                "setvol",
                "shuffle",
                "single",
                "stats",
                "status",
                "stop",
                "swap",
                "swapid",
            ] {
                let _ = writeln!(out, "command: {c}");
            }
            Ok(CmdOutcome::Reply(out))
        }
        "clear" => {
            engine
                .send(PlaybackCommand::Clear)
                .await
                .map_err(|e| mpd_ack(50, list_idx, &cmd, &e.to_string()))?;
            Ok(CmdOutcome::Reply(String::new()))
        }
        "add" => {
            let Some(uri) = rest.first() else {
                return Err(mpd_ack(2, list_idx, &cmd, "Missing URI argument"));
            };
            let resolved = resolve_music_path(uri, music_dir);
            if resolved.is_dir() {
                let mut files = Vec::new();
                collect_audio_files_recursive(&resolved, &mut files);
                if files.is_empty() {
                    return Err(mpd_ack(
                        50,
                        list_idx,
                        &cmd,
                        "No supported audio files found in directory",
                    ));
                }
                let batch = files
                    .into_iter()
                    .map(|p| (AudioSource::LocalFile(p), None, None))
                    .collect();
                engine
                    .send(PlaybackCommand::EnqueueBatch(batch))
                    .await
                    .map_err(|e| mpd_ack(50, list_idx, &cmd, &e.to_string()))?;
            } else {
                let source = if resolved.exists() {
                    AudioSource::LocalFile(resolved)
                } else {
                    AudioSource::from_uri_or_path(uri)
                };
                engine
                    .send(PlaybackCommand::Enqueue {
                        source,
                        title: None,
                        artist: None,
                    })
                    .await
                    .map_err(|e| mpd_ack(50, list_idx, &cmd, &e.to_string()))?;
            }
            Ok(CmdOutcome::Reply(String::new()))
        }
        "addid" => {
            let Some(uri) = rest.first() else {
                return Err(mpd_ack(2, list_idx, &cmd, "Missing URI argument"));
            };
            let resolved = resolve_music_path(uri, music_dir);
            if resolved.is_dir() {
                let mut files = Vec::new();
                collect_audio_files_recursive(&resolved, &mut files);
                if files.is_empty() {
                    return Err(mpd_ack(
                        50,
                        list_idx,
                        &cmd,
                        "No supported audio files found in directory",
                    ));
                }
                let count = files.len();
                let batch = files
                    .into_iter()
                    .map(|p| (AudioSource::LocalFile(p), None, None))
                    .collect();
                engine
                    .send(PlaybackCommand::EnqueueBatch(batch))
                    .await
                    .map_err(|e| mpd_ack(50, list_idx, &cmd, &e.to_string()))?;
                tokio::task::yield_now().await;
                let items = engine.queue_items();
                let mut out = String::new();
                let start_idx = items.len().saturating_sub(count);
                for item in &items[start_idx..] {
                    let _ = writeln!(out, "Id: {}", item.id);
                }
                Ok(CmdOutcome::Reply(out))
            } else {
                let source = if resolved.exists() {
                    AudioSource::LocalFile(resolved)
                } else {
                    AudioSource::from_uri_or_path(uri)
                };
                engine
                    .send(PlaybackCommand::Enqueue {
                        source,
                        title: None,
                        artist: None,
                    })
                    .await
                    .map_err(|e| mpd_ack(50, list_idx, &cmd, &e.to_string()))?;
                // Wait briefly for queue update or read latest item id
                tokio::task::yield_now().await;
                let id = engine.queue_items().last().map(|i| i.id).unwrap_or(1);
                Ok(CmdOutcome::Reply(format!("Id: {id}\n")))
            }
        }
        "move" => {
            let Some(from_str) = rest.first() else {
                return Err(mpd_ack(2, list_idx, &cmd, "Missing from position"));
            };
            let Some(to_str) = rest.get(1) else {
                return Err(mpd_ack(2, list_idx, &cmd, "Missing to position"));
            };
            let from: u32 = from_str
                .parse()
                .map_err(|_| mpd_ack(2, list_idx, &cmd, "Invalid from position"))?;
            let to: u32 = to_str
                .parse()
                .map_err(|_| mpd_ack(2, list_idx, &cmd, "Invalid to position"))?;
            engine
                .send(PlaybackCommand::MovePos { from, to })
                .await
                .map_err(|e| mpd_ack(50, list_idx, &cmd, &e.to_string()))?;
            Ok(CmdOutcome::Reply(String::new()))
        }
        "moveid" => {
            let Some(id_str) = rest.first() else {
                return Err(mpd_ack(2, list_idx, &cmd, "Missing song id"));
            };
            let Some(to_str) = rest.get(1) else {
                return Err(mpd_ack(2, list_idx, &cmd, "Missing to position"));
            };
            let id: u32 = id_str
                .parse()
                .map_err(|_| mpd_ack(2, list_idx, &cmd, "Invalid song id"))?;
            let to: u32 = to_str
                .parse()
                .map_err(|_| mpd_ack(2, list_idx, &cmd, "Invalid to position"))?;
            engine
                .send(PlaybackCommand::MoveId { id, to })
                .await
                .map_err(|e| mpd_ack(50, list_idx, &cmd, &e.to_string()))?;
            Ok(CmdOutcome::Reply(String::new()))
        }
        "swap" => {
            let Some(pos1_str) = rest.first() else {
                return Err(mpd_ack(2, list_idx, &cmd, "Missing pos1"));
            };
            let Some(pos2_str) = rest.get(1) else {
                return Err(mpd_ack(2, list_idx, &cmd, "Missing pos2"));
            };
            let pos1: u32 = pos1_str
                .parse()
                .map_err(|_| mpd_ack(2, list_idx, &cmd, "Invalid pos1"))?;
            let pos2: u32 = pos2_str
                .parse()
                .map_err(|_| mpd_ack(2, list_idx, &cmd, "Invalid pos2"))?;
            engine
                .send(PlaybackCommand::SwapPos { pos1, pos2 })
                .await
                .map_err(|e| mpd_ack(50, list_idx, &cmd, &e.to_string()))?;
            Ok(CmdOutcome::Reply(String::new()))
        }
        "swapid" => {
            let Some(id1_str) = rest.first() else {
                return Err(mpd_ack(2, list_idx, &cmd, "Missing id1"));
            };
            let Some(id2_str) = rest.get(1) else {
                return Err(mpd_ack(2, list_idx, &cmd, "Missing id2"));
            };
            let id1: u32 = id1_str
                .parse()
                .map_err(|_| mpd_ack(2, list_idx, &cmd, "Invalid id1"))?;
            let id2: u32 = id2_str
                .parse()
                .map_err(|_| mpd_ack(2, list_idx, &cmd, "Invalid id2"))?;
            engine
                .send(PlaybackCommand::SwapId { id1, id2 })
                .await
                .map_err(|e| mpd_ack(50, list_idx, &cmd, &e.to_string()))?;
            Ok(CmdOutcome::Reply(String::new()))
        }
        "shuffle" => {
            engine
                .send(PlaybackCommand::Shuffle)
                .await
                .map_err(|e| mpd_ack(50, list_idx, &cmd, &e.to_string()))?;
            Ok(CmdOutcome::Reply(String::new()))
        }
        "delete" => {
            let Some(pos_str) = rest.first() else {
                return Err(mpd_ack(2, list_idx, &cmd, "Missing position"));
            };
            let pos: u32 = pos_str
                .parse()
                .map_err(|_| mpd_ack(2, list_idx, &cmd, "Invalid position"))?;
            engine
                .send(PlaybackCommand::RemovePos(pos))
                .await
                .map_err(|e| mpd_ack(50, list_idx, &cmd, &e.to_string()))?;
            Ok(CmdOutcome::Reply(String::new()))
        }
        "deleteid" => {
            let Some(id_str) = rest.first() else {
                return Err(mpd_ack(2, list_idx, &cmd, "Missing song id"));
            };
            let id: u32 = id_str
                .parse()
                .map_err(|_| mpd_ack(2, list_idx, &cmd, "Invalid song id"))?;
            engine
                .send(PlaybackCommand::RemoveId(id))
                .await
                .map_err(|e| mpd_ack(50, list_idx, &cmd, &e.to_string()))?;
            Ok(CmdOutcome::Reply(String::new()))
        }
        "play" => {
            let pos_opt = match rest.first() {
                Some(s) => Some(
                    s.parse::<u32>()
                        .map_err(|_| mpd_ack(2, list_idx, &cmd, "Invalid songpos"))?,
                ),
                None => None,
            };
            engine
                .send(PlaybackCommand::Play(pos_opt))
                .await
                .map_err(|e| mpd_ack(50, list_idx, &cmd, &e.to_string()))?;
            Ok(CmdOutcome::Reply(String::new()))
        }
        "playid" => {
            match rest.first() {
                Some(s) => {
                    let id: u32 = s
                        .parse()
                        .map_err(|_| mpd_ack(2, list_idx, &cmd, "Invalid songid"))?;
                    engine
                        .send(PlaybackCommand::PlayId(id))
                        .await
                        .map_err(|e| mpd_ack(50, list_idx, &cmd, &e.to_string()))?;
                }
                None => {
                    engine
                        .send(PlaybackCommand::Play(None))
                        .await
                        .map_err(|e| mpd_ack(50, list_idx, &cmd, &e.to_string()))?;
                }
            }
            Ok(CmdOutcome::Reply(String::new()))
        }
        "pause" => {
            let cmd_to_send = match rest.first().map(|s| s.as_str()) {
                Some("1") => PlaybackCommand::Pause,
                Some("0") => PlaybackCommand::Resume,
                None => PlaybackCommand::TogglePause,
                _ => return Err(mpd_ack(2, list_idx, &cmd, "Boolean (0/1) expected")),
            };
            engine
                .send(cmd_to_send)
                .await
                .map_err(|e| mpd_ack(50, list_idx, &cmd, &e.to_string()))?;
            Ok(CmdOutcome::Reply(String::new()))
        }
        "stop" => {
            engine
                .send(PlaybackCommand::Stop)
                .await
                .map_err(|e| mpd_ack(50, list_idx, &cmd, &e.to_string()))?;
            Ok(CmdOutcome::Reply(String::new()))
        }
        "next" => {
            engine
                .send(PlaybackCommand::Next)
                .await
                .map_err(|e| mpd_ack(50, list_idx, &cmd, &e.to_string()))?;
            Ok(CmdOutcome::Reply(String::new()))
        }
        "previous" => {
            engine
                .send(PlaybackCommand::Previous)
                .await
                .map_err(|e| mpd_ack(50, list_idx, &cmd, &e.to_string()))?;
            Ok(CmdOutcome::Reply(String::new()))
        }
        "seek" | "seekid" => {
            if rest.len() < 2 {
                return Err(mpd_ack(2, list_idx, &cmd, "Missing arguments"));
            }
            let secs: f64 = rest[1]
                .parse()
                .map_err(|_| mpd_ack(2, list_idx, &cmd, "Invalid time"))?;
            engine
                .send(PlaybackCommand::Seek(Duration::from_secs_f64(
                    secs.max(0.0),
                )))
                .await
                .map_err(|e| mpd_ack(50, list_idx, &cmd, &e.to_string()))?;
            Ok(CmdOutcome::Reply(String::new()))
        }
        "seekcur" => {
            let Some(t_str) = rest.first() else {
                return Err(mpd_ack(2, list_idx, &cmd, "Missing time argument"));
            };
            let secs: f64 = t_str
                .parse()
                .map_err(|_| mpd_ack(2, list_idx, &cmd, "Invalid time"))?;
            engine
                .send(PlaybackCommand::Seek(Duration::from_secs_f64(
                    secs.max(0.0),
                )))
                .await
                .map_err(|e| mpd_ack(50, list_idx, &cmd, &e.to_string()))?;
            Ok(CmdOutcome::Reply(String::new()))
        }
        "repeat" => {
            let b = parse_mpd_bool(rest.first(), list_idx, &cmd)?;
            engine
                .send(PlaybackCommand::SetRepeat(b))
                .await
                .map_err(|e| mpd_ack(50, list_idx, &cmd, &e.to_string()))?;
            Ok(CmdOutcome::Reply(String::new()))
        }
        "random" => {
            let b = parse_mpd_bool(rest.first(), list_idx, &cmd)?;
            engine
                .send(PlaybackCommand::SetRandom(b))
                .await
                .map_err(|e| mpd_ack(50, list_idx, &cmd, &e.to_string()))?;
            Ok(CmdOutcome::Reply(String::new()))
        }
        "single" => {
            let b = parse_mpd_bool(rest.first(), list_idx, &cmd)?;
            engine
                .send(PlaybackCommand::SetSingle(b))
                .await
                .map_err(|e| mpd_ack(50, list_idx, &cmd, &e.to_string()))?;
            Ok(CmdOutcome::Reply(String::new()))
        }
        "consume" => {
            let b = parse_mpd_bool(rest.first(), list_idx, &cmd)?;
            engine
                .send(PlaybackCommand::SetConsume(b))
                .await
                .map_err(|e| mpd_ack(50, list_idx, &cmd, &e.to_string()))?;
            Ok(CmdOutcome::Reply(String::new()))
        }
        "setvol" | "volume" => {
            let Some(v_str) = rest.first() else {
                return Err(mpd_ack(2, list_idx, &cmd, "Missing volume"));
            };
            let vol: u8 = v_str.parse::<i32>().unwrap_or(100).clamp(0, 100) as u8;
            engine
                .send(PlaybackCommand::SetVolume(vol))
                .await
                .map_err(|e| mpd_ack(50, list_idx, &cmd, &e.to_string()))?;
            Ok(CmdOutcome::Reply(String::new()))
        }
        "lsinfo" => {
            let target_subpath = rest.first().map(|s| s.as_str()).unwrap_or("");
            // Disallow directory traversal
            let path_obj = Path::new(target_subpath);
            if path_obj
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err(mpd_ack(
                    2,
                    list_idx,
                    &cmd,
                    "Malformed path (traversal forbidden)",
                ));
            }

            let base_dir = match music_dir {
                Some(dir) => dir.to_path_buf(),
                None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            };

            let search_dir = if target_subpath.is_empty() {
                base_dir.clone()
            } else {
                base_dir.join(target_subpath)
            };

            let mut out = String::new();
            if let Ok(entries) = std::fs::read_dir(&search_dir) {
                let mut sorted_entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
                sorted_entries.sort_by_key(|e| e.file_name());

                for entry in sorted_entries {
                    let entry_path = entry.path();
                    let file_name = entry.file_name().to_string_lossy().into_owned();
                    let rel_str = if target_subpath.is_empty() {
                        file_name.clone()
                    } else {
                        format!("{}/{}", target_subpath.trim_end_matches('/'), file_name)
                    };

                    if entry_path.is_dir() {
                        let _ = writeln!(out, "directory: {rel_str}");
                    } else if let Some(ext) = entry_path.extension().and_then(|e| e.to_str()) {
                        if is_supported_audio_extension(ext) {
                            let _ = writeln!(out, "file: {rel_str}");
                            if let Ok(meta) = entry.metadata() {
                                let _ = writeln!(out, "size: {}", meta.len());
                            }
                        }
                    }
                }
            }
            Ok(CmdOutcome::Reply(out))
        }
        // Database-oriented commands return an explicit, non-fatal unsupported response (`docs/ADAPTERS.md` Section 3)
        "find" | "search" | "list" | "update" | "rescan" | "listall" | "listallinfo" => {
            Err(mpd_ack(
                5,
                list_idx,
                &cmd,
                "kiiaud is a database-free playback endpoint; use add <path_or_url>",
            ))
        }
        _ => Err(mpd_ack(
            5,
            list_idx,
            &cmd,
            &format!("unknown command \"{cmd}\""),
        )),
    }
}

fn parse_mpd_bool(
    arg: Option<&String>,
    list_idx: usize,
    cmd: &str,
) -> std::result::Result<bool, String> {
    match arg.map(|s| s.as_str()) {
        Some("1") => Ok(true),
        Some("0") => Ok(false),
        _ => Err(mpd_ack(2, list_idx, cmd, "Boolean (0/1) expected")),
    }
}

fn mpd_ack(code: u32, idx: usize, cmd: &str, msg: &str) -> String {
    format!("ACK [{code}@{idx}] {{{cmd}}} {msg}\n")
}

fn format_mpd_status(st: &PlayerStatus) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "volume: {}", st.volume);
    let _ = writeln!(out, "repeat: {}", u8::from(st.repeat));
    let _ = writeln!(out, "random: {}", u8::from(st.random));
    let _ = writeln!(out, "single: {}", u8::from(st.single));
    let _ = writeln!(out, "consume: {}", u8::from(st.consume));
    let _ = writeln!(out, "playlist: {}", st.queue_version);
    let _ = writeln!(out, "playlistlength: {}", st.queue_length);
    let _ = writeln!(out, "mixrampdb: 0.000000");
    let _ = writeln!(out, "state: {}", st.state);

    if let Some(pos) = st.current_pos {
        let _ = writeln!(out, "song: {pos}");
    }
    if let Some(item) = &st.current_item {
        let _ = writeln!(out, "songid: {}", item.id);
    }
    if st.state != kiiaud_core::PlayerState::Stop {
        let elapsed_secs = st.elapsed.as_secs_f64();
        let dur_secs = st.duration.map(|d| d.as_secs_f64()).unwrap_or(0.0);
        let _ = writeln!(
            out,
            "time: {}:{}",
            elapsed_secs.round() as u64,
            dur_secs.round() as u64
        );
        let _ = writeln!(out, "elapsed: {elapsed_secs:.3}");
        if let Some(d) = st.duration {
            let _ = writeln!(out, "duration: {:.3}", d.as_secs_f64());
        }
        let _ = writeln!(out, "bitrate: {}", st.bitrate_kbps.unwrap_or(0));
    }
    if let Some(fmt) = &st.format {
        let _ = writeln!(out, "audio: {}", fmt.mpd_audio_string());
    }
    if let Some(err) = &st.last_error {
        let _ = writeln!(out, "error: {err}");
    }
    out
}

fn format_mpd_song(out: &mut String, item: &PlaybackItem) {
    let _ = writeln!(out, "file: {}", item.source.display_uri());
    if let Some(title) = &item.title {
        let _ = writeln!(out, "Title: {title}");
    } else {
        let _ = writeln!(out, "Title: {}", item.display_title());
    }
    if let Some(artist) = &item.artist {
        let _ = writeln!(out, "Artist: {artist}");
    }
    if let Some(album) = &item.album {
        let _ = writeln!(out, "Album: {album}");
    }
    if let Some(dur) = item.duration {
        let _ = writeln!(out, "Time: {}", dur.as_secs());
        let _ = writeln!(out, "duration: {:.3}", dur.as_secs_f64());
    }
    let _ = writeln!(out, "Pos: {}", item.position);
    let _ = writeln!(out, "Id: {}", item.id);
}

/// Split an MPD command line respecting double-quoted strings and backslash escapes.
pub fn split_mpd_args(line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '\\' if in_quotes => {
                if let Some(escaped) = chars.next() {
                    current.push(escaped);
                }
            }
            '"' => {
                in_quotes = !in_quotes;
            }
            c if c.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            other => current.push(other),
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiiaud_core::PathPolicy;
    use kiiaud_output::MockAudioOutput;
    use kiiaud_playback::PlaybackEngine;
    use std::fs;

    #[test]
    fn test_split_mpd_args() {
        assert_eq!(
            split_mpd_args(r#"add "/music/My Album/01 Track.wv""#),
            vec!["add", "/music/My Album/01 Track.wv"]
        );
        assert_eq!(split_mpd_args("seek 0 42.5"), vec!["seek", "0", "42.5"]);
    }

    #[test]
    fn test_supported_audio_extensions() {
        assert!(is_supported_audio_extension("flac"));
        assert!(is_supported_audio_extension("FLAC"));
        assert!(is_supported_audio_extension("wv"));
        assert!(is_supported_audio_extension("dsf"));
        assert!(is_supported_audio_extension("dff"));
        assert!(is_supported_audio_extension("wav"));
        assert!(is_supported_audio_extension("mp3"));
        assert!(is_supported_audio_extension("opus"));
        assert!(!is_supported_audio_extension("txt"));
        assert!(!is_supported_audio_extension("jpg"));
    }

    #[tokio::test]
    async fn test_mpd_directory_scan_and_playlist_commands() {
        let temp = tempfile::tempdir().unwrap();
        let album_dir = temp.path().join("Album");
        fs::create_dir(&album_dir).unwrap();
        let track1 = album_dir.join("01_song.flac");
        let track2 = album_dir.join("02_song.wv");
        let non_audio = album_dir.join("cover.jpg");
        fs::write(&track1, b"mock audio 1").unwrap();
        fs::write(&track2, b"mock audio 2").unwrap();
        fs::write(&non_audio, b"image data").unwrap();

        let mock_out = MockAudioOutput::new();
        let policy = PathPolicy::new(vec![temp.path().to_path_buf()]);
        let engine = PlaybackEngine::with_output(Box::new(mock_out), policy);
        let handle = engine.handle();

        // Spawn engine event loop
        tokio::spawn(async move {
            engine.run().await;
        });

        // 1. Test `lsinfo` on root
        let res_ls = execute_mpd_line_with_dir("lsinfo", &handle, Some(temp.path()), 0)
            .await
            .unwrap();
        if let CmdOutcome::Reply(txt) = res_ls {
            assert!(txt.contains("directory: Album"));
        } else {
            panic!("Expected reply from lsinfo");
        }

        // 2. Test `lsinfo Album`
        let res_ls_sub = execute_mpd_line_with_dir("lsinfo Album", &handle, Some(temp.path()), 0)
            .await
            .unwrap();
        if let CmdOutcome::Reply(txt) = res_ls_sub {
            assert!(txt.contains("file: Album/01_song.flac"));
            assert!(txt.contains("file: Album/02_song.wv"));
            assert!(!txt.contains("cover.jpg"));
        } else {
            panic!("Expected reply from lsinfo Album");
        }

        // 3. Test `add "Album"` recursive directory scan
        let res_add = execute_mpd_line_with_dir(
            &format!("add \"{}\"", album_dir.display()),
            &handle,
            Some(temp.path()),
            0,
        )
        .await
        .unwrap();
        assert!(matches!(res_add, CmdOutcome::Reply(_)));

        tokio::task::yield_now().await;
        let items = handle.queue_items();
        assert_eq!(
            items.len(),
            2,
            "Expected 2 audio tracks added from directory"
        );

        // 4. Test `swap 0 1`
        let res_swap = execute_mpd_line("swap 0 1", &handle, 0).await.unwrap();
        assert!(matches!(res_swap, CmdOutcome::Reply(_)));
        tokio::task::yield_now().await;
        let swapped = handle.queue_items();
        assert_eq!(swapped[0].position, 0);
        assert_eq!(swapped[1].position, 1);
        assert_eq!(
            swapped[0].source.display_uri(),
            track2.display().to_string()
        );
        assert_eq!(
            swapped[1].source.display_uri(),
            track1.display().to_string()
        );

        // 5. Test `move 0 1`
        let res_move = execute_mpd_line("move 0 1", &handle, 0).await.unwrap();
        assert!(matches!(res_move, CmdOutcome::Reply(_)));
        tokio::task::yield_now().await;
        let moved = handle.queue_items();
        assert_eq!(moved[0].source.display_uri(), track1.display().to_string());
        assert_eq!(moved[1].source.display_uri(), track2.display().to_string());

        // 6. Test `shuffle`
        let res_shuffle = execute_mpd_line("shuffle", &handle, 0).await.unwrap();
        assert!(matches!(res_shuffle, CmdOutcome::Reply(_)));
    }
}
