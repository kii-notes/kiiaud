// SPDX-License-Identifier: MIT OR Apache-2.0

//! `kiiaudctl` — Command-line control client for the `kiiaud` audio daemon (`Phase 4`).

use clap::{Parser, Subcommand};
use kiiaud_adapter_cli::{IpcRequest, IpcResponse, send_ipc_request};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "kiiaudctl",
    version,
    about = "Control client for the kiiaud bit-perfect audio playback daemon"
)]
struct Cli {
    /// Path to the kiiaud Unix domain socket.
    #[arg(short, long, default_value = "/tmp/kiiaud.sock")]
    socket: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Play a local file path or remote stream URL immediately, or resume queue playback.
    Play {
        /// Optional local file path (`/music/test.wv`) or URL (`http://.../stream`).
        target: Option<String>,
    },
    /// Append a local file path or remote stream URL to the queue.
    Add { target: String },
    /// Pause playback.
    Pause,
    /// Resume playback.
    Resume,
    /// Stop playback.
    Stop,
    /// Advance to the next track in the queue.
    Next,
    /// Step back to the previous track in the queue.
    Prev,
    /// Seek to position in seconds.
    Seek { seconds: f64 },
    /// Clear the playback queue.
    Clear,
    /// Display current playback status and negotiated DAC format.
    Status,
    /// List items in the current in-memory queue.
    Queue,
    /// Move a track from position `from` to position `to`.
    Move { from: u32, to: u32 },
    /// Swap two tracks by position.
    Swap { pos1: u32, pos2: u32 },
    /// Shuffle all tracks in the current queue.
    Shuffle,
    /// Display detected DAC hardware capabilities, supported sample rates, and bit-perfect profile.
    Dac,
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cli = Cli::parse();

    let req = match cli.command {
        Commands::Play { target } => IpcRequest::Play { target },
        Commands::Add { target } => IpcRequest::Enqueue { target },
        Commands::Pause => IpcRequest::Pause,
        Commands::Resume => IpcRequest::Resume,
        Commands::Stop => IpcRequest::Stop,
        Commands::Next => IpcRequest::Next,
        Commands::Prev => IpcRequest::Prev,
        Commands::Seek { seconds } => IpcRequest::Seek { secs: seconds },
        Commands::Clear => IpcRequest::Clear,
        Commands::Status => IpcRequest::Status,
        Commands::Queue => IpcRequest::Queue,
        Commands::Move { from, to } => IpcRequest::Move { from, to },
        Commands::Swap { pos1, pos2 } => IpcRequest::Swap { pos1, pos2 },
        Commands::Shuffle => IpcRequest::Shuffle,
        Commands::Dac => IpcRequest::Dac,
    };

    match send_ipc_request(&cli.socket, &req).await {
        Ok(IpcResponse::Ok { message }) => {
            println!("{message}");
        }
        Ok(IpcResponse::Dac {
            capabilities,
            current_format,
            active_stream,
        }) => {
            println!("=== kiiaud DAC & Output Device Info ===");
            if !capabilities.device_name.is_empty() {
                println!("Device:           {}", capabilities.device_name);
            }
            println!("Backend:          {}", capabilities.backend_name);
            println!(
                "Bit-Perfect:      {} (Adaptive Fallback: {})",
                if capabilities.bit_perfect {
                    "Enabled"
                } else {
                    "Disabled"
                },
                if capabilities.adaptive_fallback {
                    "Active (No Silent Errors)"
                } else {
                    "Off"
                }
            );

            let native_rates: Vec<String> = capabilities
                .sample_rates
                .iter()
                .map(|r| format!("{:.1} kHz", *r as f64 / 1000.0))
                .collect();
            println!("Native Rates:     {}", native_rates.join(", "));

            if !capabilities.unsupported_rates.is_empty() {
                let unsupp_rates: Vec<String> = capabilities
                    .unsupported_rates
                    .iter()
                    .map(|r| format!("{:.1} kHz", *r as f64 / 1000.0))
                    .collect();
                println!(
                    "Adaptive Rates:   {} (Auto Fallback Resampled)",
                    unsupp_rates.join(", ")
                );
            }

            let bit_strs: Vec<String> = capabilities
                .bit_depths
                .iter()
                .map(|b| format!("{b}-bit"))
                .collect();
            println!("Bit Depths:       {}", bit_strs.join(", "));
            println!(
                "Channels:         {} channels (Stereo)",
                capabilities.channels
            );
            println!(
                "DSD Support:      Native DSD: {} | DoP (DSD over PCM): {}",
                if capabilities.dsd_native { "Yes" } else { "No" },
                if capabilities.dsd_dop { "Yes" } else { "No" }
            );

            if let Some(fmt) = current_format {
                println!("Current Stream:   {fmt} [Active]");
            } else {
                println!("Current Stream:   (Idle / Stopped)");
            }
            if let Some(uri) = active_stream {
                println!("Active Source:    {uri}");
            }
            println!("=======================================");
        }
        Ok(IpcResponse::Status { status }) => {
            println!("State:    {}", status.state);
            if let Some(item) = &status.current_item {
                println!("Track:    [{}] {}", item.id, item.source.display_uri());
                if let Some(title) = &item.title {
                    println!("Title:    {title}");
                }
            }
            if let Some(fmt) = &status.format {
                println!("Format:   {fmt}");
            }
            let dur_str = status
                .duration
                .map(|d| format!("{:.1}s", d.as_secs_f64()))
                .unwrap_or_else(|| "live/unknown".to_owned());
            println!(
                "Position: {:.1}s / {}",
                status.elapsed.as_secs_f64(),
                dur_str
            );
            println!(
                "Queue:    {} tracks (version {})",
                status.queue_length, status.queue_version
            );
            if let Some(err) = &status.last_error {
                println!("LastErr:  {err}");
            }
        }
        Ok(IpcResponse::Queue { items }) => {
            if items.is_empty() {
                println!("(queue is empty)");
            } else {
                for item in items {
                    println!(
                        "{}: [id={}] {}",
                        item.position,
                        item.id,
                        item.source.display_uri()
                    );
                }
            }
        }
        Ok(IpcResponse::Error { message }) => {
            eprintln!("Error: {message}");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Failed to communicate with kiiaud daemon: {e}");
            std::process::exit(1);
        }
    }
}
