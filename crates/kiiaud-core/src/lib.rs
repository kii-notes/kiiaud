// SPDX-License-Identifier: MIT OR Apache-2.0

//! Core domain model for `kiiaud`.
//!
//! Defines protocol-agnostic audio sources, formats (PCM & DSD), playback commands,
//! events, runtime queue, configuration, and security path validation.

pub mod config;
pub mod error;
pub mod format;
pub mod queue;
pub mod security;
pub mod state;

pub use config::KiiaudConfig;
pub use error::{KiiaudError, Result};
pub use format::{
    AudioBuffer, AudioFormat, BitOrder, ChannelLayout, DsdMode, DsdRate, DsdTransport,
    OutputCapabilities, SampleFormat,
};
pub use queue::{PlaybackItem, Queue, QueueId};
pub use security::PathPolicy;
pub use state::{
    AudioSource, CommandSender, EventReceiver, EventSender, PlaybackCommand, PlaybackEvent,
    PlayerState, PlayerStatus, collect_audio_files_recursive, is_supported_audio_extension,
    strip_uri_auth,
};
