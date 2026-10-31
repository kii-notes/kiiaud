// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::error::KiiaudError;
use crate::format::{AudioFormat, OutputCapabilities};
use crate::queue::{PlaybackItem, QueueId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};

/// An audio source identifier accepted by `kiiaud`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioSource {
    /// Local filesystem path (`/music/album/track.wv`).
    LocalFile(PathBuf),
    /// Remote HTTP/HTTPS stream URL (Jellyfin direct stream, LMS HTTP stream, or web radio).
    HttpStream {
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
    /// Raw TCP stream endpoint (`host:port` + initial request payload, e.g. LMS SlimProto raw stream).
    TcpStream {
        host: String,
        port: u16,
        request: Vec<u8>,
        format_hint: Option<String>,
    },
}

impl AudioSource {
    /// Parse a string path or URI into an [`AudioSource`].
    pub fn from_uri_or_path(input: &str) -> Self {
        let trimmed = input.trim();
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            Self::HttpStream {
                url: trimmed.to_owned(),
                headers: HashMap::new(),
            }
        } else if let Some(rest) = trimmed.strip_prefix("file://") {
            Self::LocalFile(PathBuf::from(rest))
        } else {
            Self::LocalFile(PathBuf::from(trimmed))
        }
    }

    /// Return a human-readable URI or path string (with credentials stripped if HTTP).
    pub fn display_uri(&self) -> String {
        match self {
            Self::LocalFile(p) => p.to_string_lossy().into_owned(),
            Self::HttpStream { url, .. } => strip_uri_auth(url),
            Self::TcpStream { host, port, .. } => format!("tcp://{host}:{port}"),
        }
    }
}

impl fmt::Display for AudioSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display_uri())
    }
}

/// Check if a file extension corresponds to a known supported audio format in `kiiaud`.
pub fn is_supported_audio_extension(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "flac"
            | "wv"
            | "dsf"
            | "dff"
            | "wav"
            | "aiff"
            | "aif"
            | "mp3"
            | "ogg"
            | "opus"
            | "aac"
            | "m4a"
            | "alac"
    )
}

/// Recursively scan a local directory and return all supported audio file paths, sorted alphabetically.
pub fn collect_audio_files_recursive(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_audio_files_inner(dir, &mut out);
    out
}

fn collect_audio_files_inner(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut sorted: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        sorted.sort_by_key(|e| e.path());
        for entry in sorted {
            let path = entry.path();
            if path.is_dir() {
                collect_audio_files_inner(&path, out);
            } else if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                if is_supported_audio_extension(ext) {
                    out.push(path);
                }
            }
        }
    }
}

/// Remove `user:password@` from a URL so credentials never leak into logs or status responses.
pub fn strip_uri_auth(uri: &str) -> String {
    if let Some(scheme_end) = uri.find("://") {
        let rest = &uri[scheme_end + 3..];
        let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        if let Some(at) = rest[..authority_end].rfind('@') {
            return format!("{}{}", &uri[..scheme_end + 3], &rest[at + 1..]);
        }
    }
    uri.to_owned()
}

/// Player transport state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PlayerState {
    #[default]
    Stop,
    Play,
    Pause,
}

impl fmt::Display for PlayerState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stop => f.write_str("stop"),
            Self::Play => f.write_str("play"),
            Self::Pause => f.write_str("pause"),
        }
    }
}

/// Snapshot of the player's runtime status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerStatus {
    pub state: PlayerState,
    pub volume: u8,
    pub repeat: bool,
    pub random: bool,
    pub single: bool,
    pub consume: bool,
    pub current_item: Option<PlaybackItem>,
    pub current_pos: Option<u32>,
    pub elapsed: Duration,
    pub duration: Option<Duration>,
    pub bitrate_kbps: Option<u32>,
    pub format: Option<AudioFormat>,
    #[serde(default)]
    pub output_capabilities: Option<OutputCapabilities>,
    pub queue_version: u32,
    pub queue_length: u32,
    pub last_error: Option<String>,
}

impl Default for PlayerStatus {
    fn default() -> Self {
        Self {
            state: PlayerState::Stop,
            volume: 100,
            repeat: false,
            random: false,
            single: false,
            consume: false,
            current_item: None,
            current_pos: None,
            elapsed: Duration::ZERO,
            duration: None,
            bitrate_kbps: None,
            format: None,
            output_capabilities: None,
            queue_version: 0,
            queue_length: 0,
            last_error: None,
        }
    }
}

/// Protocol-agnostic commands sent from adapters (Jellyfin, LMS, MPD, CLI) to the playback engine.
#[derive(Debug, Clone)]
pub enum PlaybackCommand {
    /// Replace or load a single source and optionally start playback immediately.
    Load {
        source: AudioSource,
        title: Option<String>,
        artist: Option<String>,
        autostart: bool,
    },
    /// Append an audio source to the in-memory runtime queue.
    Enqueue {
        source: AudioSource,
        title: Option<String>,
        artist: Option<String>,
    },
    /// Enqueue multiple audio sources in a single batch.
    EnqueueBatch(Vec<(AudioSource, Option<String>, Option<String>)>),
    /// Move a track from position `from` to position `to`.
    MovePos { from: u32, to: u32 },
    /// Move a track by its `QueueId` to position `to`.
    MoveId { id: QueueId, to: u32 },
    /// Swap two tracks by their 0-based queue positions.
    SwapPos { pos1: u32, pos2: u32 },
    /// Swap two tracks by their unique `QueueId`s.
    SwapId { id1: QueueId, id2: QueueId },
    /// Shuffle the entire queue.
    Shuffle,
    /// Clear the runtime queue and stop playback.
    Clear,
    /// Remove a track by its unique `QueueId`.
    RemoveId(QueueId),
    /// Remove a track by its 0-based queue position.
    RemovePos(u32),
    /// Start playing at optional 0-based queue position (or current/first track if `None`).
    Play(Option<u32>),
    /// Start playing a track by its unique `QueueId`.
    PlayId(QueueId),
    /// Pause active playback.
    Pause,
    /// Resume paused playback.
    Resume,
    /// Toggle between Play and Pause.
    TogglePause,
    /// Stop playback immediately.
    Stop,
    /// Advance to the next track in the queue.
    Next,
    /// Return to the previous track in the queue.
    Previous,
    /// Seek within the currently playing track.
    Seek(Duration),
    /// Set hardware volume (0..=100) if hardware mixer is enabled; ignored in strict fixed-volume mode.
    SetVolume(u8),
    /// Set repeat mode.
    SetRepeat(bool),
    /// Set random mode.
    SetRandom(bool),
    /// Set single mode.
    SetSingle(bool),
    /// Set consume mode.
    SetConsume(bool),
}

/// Events broadcast by the playback engine to all subscribed adapters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PlaybackEvent {
    StateChanged(PlayerState),
    TrackStarted {
        item: PlaybackItem,
        format: AudioFormat,
    },
    TrackEnded(QueueId),
    PositionUpdated {
        elapsed: Duration,
        duration: Option<Duration>,
    },
    FormatChanged(AudioFormat),
    QueueUpdated {
        version: u32,
        length: u32,
    },
    OptionsChanged,
    VolumeChanged(u8),
    Error(KiiaudError),
}

pub type CommandSender = mpsc::Sender<PlaybackCommand>;
pub type EventSender = broadcast::Sender<PlaybackEvent>;
pub type EventReceiver = broadcast::Receiver<PlaybackEvent>;
