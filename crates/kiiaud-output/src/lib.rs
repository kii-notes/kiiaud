// SPDX-License-Identifier: MIT OR Apache-2.0

//! Audio output abstraction and mock/sink backend (`kiiaud-output`).
//!
//! Defines the [`AudioOutput`] trait (`docs/ARCHITECTURE.md` Section 8) and provides
//! [`MockAudioOutput`] for hardware-independent unit testing (`R-025`).

pub mod dop;

use kiiaud_core::{AudioBuffer, AudioFormat, OutputCapabilities, Result};
use parking_lot::Mutex;
use std::sync::Arc;

/// Output abstraction (`docs/ARCHITECTURE.md` Section 8).
pub trait AudioOutput: Send {
    /// Open or reconfigure the output hardware for the given [`AudioFormat`].
    /// Returns the actual hardware [`AudioFormat`] negotiated (e.g. if `DsdMode::Auto` selected `DoP` or `Native`).
    fn open(&mut self, format: &AudioFormat) -> Result<AudioFormat>;

    /// Write a decoded [`AudioBuffer`] to the output device.
    /// Returns the number of frames written.
    fn write(&mut self, buffer: &AudioBuffer) -> Result<usize>;

    /// Pause hardware output.
    fn pause(&mut self) -> Result<()>;

    /// Resume hardware output.
    fn resume(&mut self) -> Result<()>;

    /// Drain remaining buffered audio at the end of a track.
    fn drain(&mut self) -> Result<()>;

    /// Stop output immediately and release or reset the hardware device.
    fn stop(&mut self) -> Result<()>;

    /// Return the capabilities of this audio output backend and active hardware device.
    fn capabilities(&self) -> OutputCapabilities {
        OutputCapabilities::default()
    }
}

/// Recorded state of [`MockAudioOutput`] for inspection in unit and integration tests.
#[derive(Debug, Default, Clone)]
pub struct MockOutputState {
    pub is_open: bool,
    pub is_paused: bool,
    pub active_format: Option<AudioFormat>,
    pub open_count: usize,
    pub chunks_written: usize,
    pub frames_written: usize,
    pub dsd_bytes_written: usize,
    pub drain_count: usize,
    pub stop_count: usize,
}

/// Hardware-independent mock/sink output backend (`R-025`).
#[derive(Clone, Default)]
pub struct MockAudioOutput {
    state: Arc<Mutex<MockOutputState>>,
}

impl MockAudioOutput {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> MockOutputState {
        self.state.lock().clone()
    }
}

impl AudioOutput for MockAudioOutput {
    fn open(&mut self, format: &AudioFormat) -> Result<AudioFormat> {
        let mut st = self.state.lock();
        st.is_open = true;
        st.is_paused = false;
        st.active_format = Some(*format);
        st.open_count += 1;
        tracing::debug!(%format, "mock audio output opened");
        Ok(*format)
    }

    fn write(&mut self, buffer: &AudioBuffer) -> Result<usize> {
        let mut st = self.state.lock();
        let frames = buffer.frames();
        st.chunks_written += 1;
        st.frames_written += frames;
        if let AudioBuffer::Dsd { data, .. } = buffer {
            st.dsd_bytes_written += data.len();
        }
        Ok(frames)
    }

    fn pause(&mut self) -> Result<()> {
        self.state.lock().is_paused = true;
        Ok(())
    }

    fn resume(&mut self) -> Result<()> {
        self.state.lock().is_paused = false;
        Ok(())
    }

    fn drain(&mut self) -> Result<()> {
        self.state.lock().drain_count += 1;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        let mut st = self.state.lock();
        st.is_open = false;
        st.is_paused = false;
        st.stop_count += 1;
        Ok(())
    }

    fn capabilities(&self) -> OutputCapabilities {
        OutputCapabilities {
            backend_name: "Mock / Null Audio Output".to_owned(),
            sample_rates: vec![
                44100, 48000, 88200, 96000, 176400, 192000, 352800, 384000, 705600, 768000,
            ],
            bit_depths: vec![16, 24, 32],
            channels: 2,
            exclusive: true,
            bit_perfect: true,
            dsd_native: true,
            dsd_dop: true,
        }
    }
}
