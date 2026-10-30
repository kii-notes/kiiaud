// SPDX-License-Identifier: MIT OR Apache-2.0

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, KiiaudError>;

#[derive(Debug, Error, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KiiaudError {
    #[error("Audio source error: {0}")]
    Source(String),

    #[error("Decoder error: {0}")]
    Decoder(String),

    #[error("Output error: {0}")]
    Output(String),

    #[error("Unsupported audio format: {0}")]
    UnsupportedFormat(String),

    #[error("Security policy violation: {0}")]
    SecurityViolation(String),

    #[error("Invalid state or operation: {0}")]
    InvalidState(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("I/O error: {0}")]
    Io(String),
}

impl From<std::io::Error> for KiiaudError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.to_string())
    }
}
