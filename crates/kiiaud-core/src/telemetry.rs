// SPDX-License-Identifier: MIT OR Apache-2.0

//! Lock-free / lightweight streaming buffer telemetry shared across decoder, playback, output, and adapters.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Real-time telemetry snapshot of the active streaming pre-buffer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamTelemetry {
    pub total_bytes_read: u64,
    pub buffer_occupancy: u32,
    pub buffer_capacity: u32,
}

impl StreamTelemetry {
    /// Fill percentage in `0.0..=100.0`, or `0.0` if capacity is zero.
    pub fn fill_percent(&self) -> f64 {
        if self.buffer_capacity == 0 {
            0.0
        } else {
            (self.buffer_occupancy as f64 / self.buffer_capacity as f64) * 100.0
        }
    }
}

static TOTAL_BYTES_READ: AtomicU64 = AtomicU64::new(0);
static BUFFER_OCCUPANCY: AtomicU32 = AtomicU32::new(0);
static BUFFER_CAPACITY: AtomicU32 = AtomicU32::new(0);

/// Configured HTTP/stream pre-buffer size in MiB (`0` = auto-scale by bitrate).
static CONFIGURED_STREAM_BUFFER_MB: AtomicU32 = AtomicU32::new(0);

/// Retrieve current real-time streaming buffer telemetry.
pub fn get_stream_telemetry() -> StreamTelemetry {
    StreamTelemetry {
        total_bytes_read: TOTAL_BYTES_READ.load(Ordering::Relaxed),
        buffer_occupancy: BUFFER_OCCUPANCY.load(Ordering::Relaxed),
        buffer_capacity: BUFFER_CAPACITY.load(Ordering::Relaxed),
    }
}

/// Update current real-time streaming buffer telemetry.
pub fn update_stream_telemetry(total_bytes_read: u64, occupancy: u32, capacity: u32) {
    TOTAL_BYTES_READ.store(total_bytes_read, Ordering::Relaxed);
    BUFFER_OCCUPANCY.store(occupancy, Ordering::Relaxed);
    BUFFER_CAPACITY.store(capacity, Ordering::Relaxed);
}

/// Reset stream telemetry (called when starting a new stream or flushing).
pub fn reset_stream_telemetry() {
    TOTAL_BYTES_READ.store(0, Ordering::Relaxed);
    BUFFER_OCCUPANCY.store(0, Ordering::Relaxed);
    BUFFER_CAPACITY.store(0, Ordering::Relaxed);
}

/// Set the configured HTTP/stream pre-buffer size in MiB (`0` = auto-scale).
pub fn set_configured_stream_buffer_mb(mb: u32) {
    CONFIGURED_STREAM_BUFFER_MB.store(mb, Ordering::Relaxed);
}

/// Get the configured HTTP/stream pre-buffer size in MiB (`0` = auto-scale).
pub fn get_configured_stream_buffer_mb() -> u32 {
    CONFIGURED_STREAM_BUFFER_MB.load(Ordering::Relaxed)
}
