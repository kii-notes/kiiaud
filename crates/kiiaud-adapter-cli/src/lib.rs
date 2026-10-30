// SPDX-License-Identifier: MIT OR Apache-2.0

//! Local IPC adapter for CLI control and scripting (`kiiaud-adapter-cli`).
//!
//! Exposes a lightweight newline-delimited JSON RPC protocol:
//! - On Unix / Linux: Unix domain socket (`/tmp/kiiaud.sock` or `/run/kiiaud/kiiaud.sock`) (`R-020`).
//! - On Windows: Local loopback TCP socket (`127.0.0.1:7111`) for native Windows scripting and control.

use kiiaud_core::{AudioSource, KiiaudError, PlaybackCommand, PlaybackItem, PlayerStatus, Result};
use kiiaud_playback::EngineHandle;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};

#[cfg(windows)]
use tokio::net::{TcpListener, TcpStream};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum IpcRequest {
    Play { target: Option<String> },
    Enqueue { target: String },
    Pause,
    Resume,
    Stop,
    Next,
    Prev,
    Seek { secs: f64 },
    Clear,
    Status,
    Queue,
    Move { from: u32, to: u32 },
    Swap { pos1: u32, pos2: u32 },
    Shuffle,
    /// Query detected DAC hardware capabilities, supported rates, and bit-perfect profile.
    Dac,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcResponse {
    Ok { message: String },
    Status { status: PlayerStatus },
    Queue { items: Vec<PlaybackItem> },
    Dac {
        capabilities: kiiaud_core::OutputCapabilities,
        current_format: Option<kiiaud_core::AudioFormat>,
        active_stream: Option<String>,
    },
    Error { message: String },
}

/// Start the local IPC server as an independent background task (`R-021`).
#[cfg(unix)]
pub async fn run_ipc_server(socket_path: PathBuf, engine: EngineHandle) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
    }

    let listener = UnixListener::bind(&socket_path).map_err(|e| {
        KiiaudError::Io(format!(
            "Failed to bind IPC socket '{}': {e}",
            socket_path.display()
        ))
    })?;

    tracing::info!(socket = %socket_path.display(), "local CLI IPC server listening (unix domain socket)");

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let eng = engine.clone();
                tokio::spawn(async move {
                    let (reader, writer) = stream.into_split();
                    if let Err(e) = handle_ipc_stream(reader, writer, eng).await {
                        tracing::debug!("IPC client error: {e}");
                    }
                });
            }
            Err(e) => {
                tracing::warn!("IPC accept error: {e}");
            }
        }
    }
}

/// Start the local IPC server on Windows (listening on local loopback TCP `127.0.0.1:7111`).
#[cfg(windows)]
pub async fn run_ipc_server(_socket_path: PathBuf, engine: EngineHandle) -> Result<()> {
    let addr = "127.0.0.1:7111";
    let listener = TcpListener::bind(addr).await.map_err(|e| {
        KiiaudError::Io(format!(
            "Failed to bind Windows CLI IPC TCP socket on '{addr}': {e}"
        ))
    })?;

    tracing::info!(listen = %addr, "local CLI IPC server listening (Windows loopback TCP)");

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let eng = engine.clone();
                tokio::spawn(async move {
                    let (reader, writer) = stream.into_split();
                    if let Err(e) = handle_ipc_stream(reader, writer, eng).await {
                        tracing::debug!("IPC client error: {e}");
                    }
                });
            }
            Err(e) => {
                tracing::warn!("IPC accept error: {e}");
            }
        }
    }
}

async fn handle_ipc_stream<R, W>(reader: R, mut writer: W, engine: EngineHandle) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut lines = BufReader::new(reader).lines();

    while let Some(line) = lines.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let resp = match serde_json::from_str::<IpcRequest>(trimmed) {
            Ok(req) => process_ipc_request(req, &engine).await,
            Err(e) => IpcResponse::Error {
                message: format!("Invalid IPC JSON request: {e}"),
            },
        };

        let mut payload = serde_json::to_string(&resp).unwrap_or_else(|_| {
            r#"{"status":"error","message":"serialization failed"}"#.to_owned()
        });
        payload.push('\n');
        writer.write_all(payload.as_bytes()).await?;
    }

    Ok(())
}

async fn process_ipc_request(req: IpcRequest, engine: &EngineHandle) -> IpcResponse {
    let res = match req {
        IpcRequest::Play { target: Some(uri) } => {
            let source = AudioSource::from_uri_or_path(&uri);
            engine
                .send(PlaybackCommand::Load {
                    source,
                    title: None,
                    artist: None,
                    autostart: true,
                })
                .await
        }
        IpcRequest::Play { target: None } => engine.send(PlaybackCommand::Play(None)).await,
        IpcRequest::Enqueue { target } => {
            let p = Path::new(&target);
            if p.is_dir() {
                let files = kiiaud_core::collect_audio_files_recursive(p);
                if files.is_empty() {
                    return IpcResponse::Error {
                        message: "No supported audio files found in directory".to_owned(),
                    };
                }
                let count = files.len();
                let batch = files
                    .into_iter()
                    .map(|p| (AudioSource::LocalFile(p), None, None))
                    .collect();
                match engine.send(PlaybackCommand::EnqueueBatch(batch)).await {
                    Ok(()) => {
                        return IpcResponse::Ok {
                            message: format!("Enqueued {count} tracks from directory"),
                        };
                    }
                    Err(e) => {
                        return IpcResponse::Error {
                            message: e.to_string(),
                        };
                    }
                }
            } else {
                let source = AudioSource::from_uri_or_path(&target);
                engine
                    .send(PlaybackCommand::Enqueue {
                        source,
                        title: None,
                        artist: None,
                    })
                    .await
            }
        }
        IpcRequest::Move { from, to } => {
            engine.send(PlaybackCommand::MovePos { from, to }).await
        }
        IpcRequest::Swap { pos1, pos2 } => {
            engine.send(PlaybackCommand::SwapPos { pos1, pos2 }).await
        }
        IpcRequest::Shuffle => engine.send(PlaybackCommand::Shuffle).await,
        IpcRequest::Pause => engine.send(PlaybackCommand::Pause).await,
        IpcRequest::Resume => engine.send(PlaybackCommand::Resume).await,
        IpcRequest::Stop => engine.send(PlaybackCommand::Stop).await,
        IpcRequest::Next => engine.send(PlaybackCommand::Next).await,
        IpcRequest::Prev => engine.send(PlaybackCommand::Previous).await,
        IpcRequest::Seek { secs } => {
            engine
                .send(PlaybackCommand::Seek(Duration::from_secs_f64(
                    secs.max(0.0),
                )))
                .await
        }
        IpcRequest::Clear => engine.send(PlaybackCommand::Clear).await,
        IpcRequest::Status => {
            return IpcResponse::Status {
                status: engine.status(),
            };
        }
        IpcRequest::Queue => {
            return IpcResponse::Queue {
                items: engine.queue_items(),
            };
        }
        IpcRequest::Dac => {
            let st = engine.status();
            let capabilities = st.output_capabilities.unwrap_or_default();
            let current_format = st.format;
            let active_stream = st.current_item.as_ref().map(|i| i.source.display_uri());
            return IpcResponse::Dac {
                capabilities,
                current_format,
                active_stream,
            };
        }
    };

    match res {
        Ok(()) => IpcResponse::Ok {
            message: "OK".to_owned(),
        },
        Err(e) => IpcResponse::Error {
            message: e.to_string(),
        },
    }
}

/// Send a single [`IpcRequest`] to a running `kiiaud` daemon and await its [`IpcResponse`].
#[cfg(unix)]
pub async fn send_ipc_request(socket_path: &Path, req: &IpcRequest) -> Result<IpcResponse> {
    let stream = UnixStream::connect(socket_path).await.map_err(|e| {
        KiiaudError::Io(format!(
            "Failed to connect to kiiaud socket '{}': {e}",
            socket_path.display()
        ))
    })?;

    let (reader, mut writer) = stream.into_split();
    let mut payload =
        serde_json::to_string(req).map_err(|e| KiiaudError::Io(format!("JSON error: {e}")))?;
    payload.push('\n');
    writer.write_all(payload.as_bytes()).await?;

    let mut lines = BufReader::new(reader).lines();
    let Some(line) = lines.next_line().await? else {
        return Err(KiiaudError::Io("Empty response from daemon".to_owned()));
    };

    serde_json::from_str(&line)
        .map_err(|e| KiiaudError::Io(format!("Invalid JSON response from daemon: {e}")))
}

/// Send a single [`IpcRequest`] on Windows over local loopback TCP `127.0.0.1:7111`.
#[cfg(windows)]
pub async fn send_ipc_request(_socket_path: &Path, req: &IpcRequest) -> Result<IpcResponse> {
    let addr = "127.0.0.1:7111";
    let stream = TcpStream::connect(addr).await.map_err(|e| {
        KiiaudError::Io(format!(
            "Failed to connect to kiiaud Windows IPC on '{addr}': {e}"
        ))
    })?;

    let (reader, mut writer) = stream.into_split();
    let mut payload =
        serde_json::to_string(req).map_err(|e| KiiaudError::Io(format!("JSON error: {e}")))?;
    payload.push('\n');
    writer.write_all(payload.as_bytes()).await?;

    let mut lines = BufReader::new(reader).lines();
    let Some(line) = lines.next_line().await? else {
        return Err(KiiaudError::Io("Empty response from daemon".to_owned()));
    };

    serde_json::from_str(&line)
        .map_err(|e| KiiaudError::Io(format!("Invalid JSON response from daemon: {e}")))
}
