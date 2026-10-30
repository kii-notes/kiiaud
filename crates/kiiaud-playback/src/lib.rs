// SPDX-License-Identifier: MIT OR Apache-2.0

//! Playback engine and queue coordinator (`kiiaud-playback`).
//!
//! Coordinates:
//! - Command processing (`PlaybackCommand`) from all protocol adapters.
//! - In-memory runtime `Queue` management (`R-005`).
//! - Dedicated realtime OS thread for decoding & ALSA output (`docs/ARCHITECTURE.md` Section 12).
//! - Gapless track transitions (`R-015`) by pre-opening the next track in the queue and reusing
//!   the active hardware output stream when sample rate / format match.
//! - Fault tolerance (`R-024`): unreadable/corrupt tracks emit `PlaybackEvent::Error` and advance
//!   cleanly without crashing the daemon.

use kiiaud_core::{
    AudioFormat, CommandSender, EventReceiver, EventSender, KiiaudConfig, KiiaudError, PathPolicy,
    PlaybackCommand, PlaybackEvent, PlaybackItem, PlayerState, PlayerStatus, Queue,
};
use kiiaud_decoder::{Decoder, SymphoniaAudioDecoder};
use kiiaud_output::{AudioOutput, MockAudioOutput};
use kiiaud_output_alsa::AlsaAudioOutput;
use kiiaud_output_android::AndroidAudioOutput;
use kiiaud_output_wasapi::WasapiAudioOutput;
use parking_lot::RwLock;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc};

/// Internal commands sent from the async coordinator to the dedicated OS audio thread.
enum WorkerCommand {
    PlayTrack {
        item: PlaybackItem,
        next_candidate: Option<PlaybackItem>,
    },
    UpdateNextCandidate(Option<PlaybackItem>),
    Pause,
    Resume,
    Seek(Duration),
    Stop,
    Shutdown,
}

/// Notifications sent from the dedicated OS audio thread back to the async coordinator.
enum WorkerNotice {
    TrackStarted {
        item: PlaybackItem,
        format: AudioFormat,
        duration: Option<Duration>,
    },
    Position {
        elapsed: Duration,
        duration: Option<Duration>,
    },
    GaplessTransitioned {
        item: PlaybackItem,
        format: AudioFormat,
        duration: Option<Duration>,
    },
    TrackFinished {
        id: u32,
    },
    TrackFailed {
        id: u32,
        error: KiiaudError,
    },
}

/// Shared handle used by protocol adapters (MPD, Jellyfin, LMS, CLI) to send commands,
/// subscribe to events, and inspect the current `PlayerStatus` and `Queue` without blocking.
#[derive(Clone)]
pub struct EngineHandle {
    cmd_tx: CommandSender,
    event_tx: EventSender,
    status: Arc<RwLock<PlayerStatus>>,
    queue: Arc<RwLock<Queue>>,
}

impl EngineHandle {
    pub async fn send(&self, cmd: PlaybackCommand) -> std::result::Result<(), KiiaudError> {
        self.cmd_tx
            .send(cmd)
            .await
            .map_err(|_| KiiaudError::InvalidState("Playback engine channel closed".to_owned()))
    }

    pub fn try_send(&self, cmd: PlaybackCommand) -> std::result::Result<(), KiiaudError> {
        self.cmd_tx.try_send(cmd).map_err(|e| {
            KiiaudError::InvalidState(format!("Playback engine channel full/closed: {e}"))
        })
    }

    pub fn subscribe(&self) -> EventReceiver {
        self.event_tx.subscribe()
    }

    pub fn status(&self) -> PlayerStatus {
        self.status.read().clone()
    }

    pub fn queue_items(&self) -> Vec<PlaybackItem> {
        self.queue.read().items().to_vec()
    }
}

/// Main playback engine coordinator.
pub struct PlaybackEngine {
    handle: EngineHandle,
    cmd_rx: mpsc::Receiver<PlaybackCommand>,
    worker_tx: std_mpsc::Sender<WorkerCommand>,
    notice_rx: mpsc::UnboundedReceiver<WorkerNotice>,
    stop_flag: Arc<AtomicBool>,
    worker_join: Option<JoinHandle<()>>,
}

impl PlaybackEngine {
    /// Create a new `PlaybackEngine` using the configured output backend (`"alsa"` or `"mock"`).
    pub fn new(config: &KiiaudConfig) -> Self {
        let output: Box<dyn AudioOutput> = if config.output.backend.eq_ignore_ascii_case("mock")
            || config.output.backend.eq_ignore_ascii_case("null")
        {
            Box::new(MockAudioOutput::new())
        } else if config.output.backend.eq_ignore_ascii_case("wasapi") {
            Box::new(WasapiAudioOutput::new(config.output.clone()))
        } else if config.output.backend.eq_ignore_ascii_case("android")
            || config.output.backend.eq_ignore_ascii_case("oboe")
        {
            Box::new(AndroidAudioOutput::new(config.output.clone()))
        } else if config.output.backend.eq_ignore_ascii_case("alsa") {
            Box::new(AlsaAudioOutput::new(config.output.clone()))
        } else {
            #[cfg(target_os = "windows")]
            {
                Box::new(WasapiAudioOutput::new(config.output.clone()))
            }
            #[cfg(target_os = "android")]
            {
                Box::new(AndroidAudioOutput::new(config.output.clone()))
            }
            #[cfg(all(not(target_os = "windows"), not(target_os = "android")))]
            {
                Box::new(AlsaAudioOutput::new(config.output.clone()))
            }
        };
        let policy = PathPolicy::new(config.security.allowed_roots.clone());
        Self::with_output(output, policy)
    }

    /// Create a new `PlaybackEngine` with an explicit `AudioOutput` implementation (e.g. `MockAudioOutput` in tests).
    pub fn with_output(output: Box<dyn AudioOutput>, policy: PathPolicy) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel(256);
        let (event_tx, _) = broadcast::channel(256);
        let (worker_tx, worker_rx) = std_mpsc::channel();
        let (notice_tx, notice_rx) = mpsc::unbounded_channel();

        let initial_caps = output.capabilities();
        let status = Arc::new(RwLock::new(PlayerStatus {
            output_capabilities: Some(initial_caps),
            ..PlayerStatus::default()
        }));
        let queue = Arc::new(RwLock::new(Queue::new()));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let worker_stop_flag = Arc::clone(&stop_flag);

        let worker_join = thread::Builder::new()
            .name("kiiaud-audio-rt".to_owned())
            .spawn(move || {
                audio_worker_loop(output, policy, worker_rx, notice_tx, worker_stop_flag);
            })
            .expect("failed to spawn kiiaud-audio-rt thread");

        let handle = EngineHandle {
            cmd_tx,
            event_tx,
            status,
            queue,
        };

        Self {
            handle,
            cmd_rx,
            worker_tx,
            notice_rx,
            stop_flag,
            worker_join: Some(worker_join),
        }
    }

    pub fn handle(&self) -> EngineHandle {
        self.handle.clone()
    }

    /// Run the async command and event loop until the command channel closes.
    pub async fn run(mut self) {
        loop {
            tokio::select! {
                Some(cmd) = self.cmd_rx.recv() => {
                    self.handle_command(cmd);
                }
                Some(notice) = self.notice_rx.recv() => {
                    self.handle_worker_notice(notice);
                }
                else => break,
            }
        }
        self.stop_flag.store(true, Ordering::SeqCst);
        let _ = self.worker_tx.send(WorkerCommand::Shutdown);
        if let Some(join) = self.worker_join.take() {
            let _ = join.join();
        }
    }

    fn handle_command(&mut self, cmd: PlaybackCommand) {
        match cmd {
            PlaybackCommand::Load {
                source,
                title,
                artist,
                autostart,
            } => {
                let item = {
                    let mut q = self.handle.queue.write();
                    let item = q.load_single(source, title, artist);
                    let mut st = self.handle.status.write();
                    st.queue_version = q.version();
                    st.queue_length = q.len() as u32;
                    st.current_item = Some(item.clone());
                    st.current_pos = Some(0);
                    item
                };
                self.emit_queue_updated();
                if autostart {
                    self.start_playback(item);
                } else {
                    self.stop_playback();
                }
            }
            PlaybackCommand::Enqueue {
                source,
                title,
                artist,
            } => {
                {
                    let mut q = self.handle.queue.write();
                    q.enqueue(source, title, artist);
                    let mut st = self.handle.status.write();
                    st.queue_version = q.version();
                    st.queue_length = q.len() as u32;
                }
                self.emit_queue_updated();
                self.refresh_next_candidate();
            }
            PlaybackCommand::EnqueueBatch(items) => {
                {
                    let mut q = self.handle.queue.write();
                    q.enqueue_batch(items);
                    let mut st = self.handle.status.write();
                    st.queue_version = q.version();
                    st.queue_length = q.len() as u32;
                }
                self.emit_queue_updated();
                self.refresh_next_candidate();
            }
            PlaybackCommand::MovePos { from, to } => {
                let changed = {
                    let mut q = self.handle.queue.write();
                    let ok = q.move_pos(from, to);
                    if ok {
                        let mut st = self.handle.status.write();
                        st.queue_version = q.version();
                        st.current_pos = q.current_index().map(|i| i as u32);
                    }
                    ok
                };
                if changed {
                    self.emit_queue_updated();
                    self.refresh_next_candidate();
                }
            }
            PlaybackCommand::MoveId { id, to } => {
                let changed = {
                    let mut q = self.handle.queue.write();
                    let ok = q.move_id(id, to);
                    if ok {
                        let mut st = self.handle.status.write();
                        st.queue_version = q.version();
                        st.current_pos = q.current_index().map(|i| i as u32);
                    }
                    ok
                };
                if changed {
                    self.emit_queue_updated();
                    self.refresh_next_candidate();
                }
            }
            PlaybackCommand::SwapPos { pos1, pos2 } => {
                let changed = {
                    let mut q = self.handle.queue.write();
                    let ok = q.swap_pos(pos1, pos2);
                    if ok {
                        let mut st = self.handle.status.write();
                        st.queue_version = q.version();
                        st.current_pos = q.current_index().map(|i| i as u32);
                    }
                    ok
                };
                if changed {
                    self.emit_queue_updated();
                    self.refresh_next_candidate();
                }
            }
            PlaybackCommand::SwapId { id1, id2 } => {
                let changed = {
                    let mut q = self.handle.queue.write();
                    let ok = q.swap_id(id1, id2);
                    if ok {
                        let mut st = self.handle.status.write();
                        st.queue_version = q.version();
                        st.current_pos = q.current_index().map(|i| i as u32);
                    }
                    ok
                };
                if changed {
                    self.emit_queue_updated();
                    self.refresh_next_candidate();
                }
            }
            PlaybackCommand::Shuffle => {
                {
                    let mut q = self.handle.queue.write();
                    q.shuffle();
                    let mut st = self.handle.status.write();
                    st.queue_version = q.version();
                    st.current_pos = q.current_index().map(|i| i as u32);
                }
                self.emit_queue_updated();
                self.refresh_next_candidate();
            }
            PlaybackCommand::Clear => {
                self.stop_playback();
                {
                    let mut q = self.handle.queue.write();
                    q.clear();
                    let mut st = self.handle.status.write();
                    st.queue_version = q.version();
                    st.queue_length = 0;
                    st.current_item = None;
                    st.current_pos = None;
                }
                self.emit_queue_updated();
            }
            PlaybackCommand::RemoveId(id) => {
                let was_playing_removed = self
                    .handle
                    .status
                    .read()
                    .current_item
                    .as_ref()
                    .is_some_and(|i| i.id == id);
                {
                    let mut q = self.handle.queue.write();
                    q.remove_id(id);
                    let mut st = self.handle.status.write();
                    st.queue_version = q.version();
                    st.queue_length = q.len() as u32;
                }
                self.emit_queue_updated();
                if was_playing_removed {
                    self.stop_playback();
                } else {
                    self.refresh_next_candidate();
                }
            }
            PlaybackCommand::RemovePos(pos) => {
                let was_playing_removed = self.handle.status.read().current_pos == Some(pos);
                {
                    let mut q = self.handle.queue.write();
                    q.remove_pos(pos);
                    let mut st = self.handle.status.write();
                    st.queue_version = q.version();
                    st.queue_length = q.len() as u32;
                }
                self.emit_queue_updated();
                if was_playing_removed {
                    self.stop_playback();
                } else {
                    self.refresh_next_candidate();
                }
            }
            PlaybackCommand::Play(pos_opt) => {
                let item_opt = {
                    let mut q = self.handle.queue.write();
                    match pos_opt {
                        Some(pos) => q.select_pos(pos).cloned(),
                        None => q.current_or_first().cloned(),
                    }
                };
                if let Some(item) = item_opt {
                    self.start_playback(item);
                }
            }
            PlaybackCommand::PlayId(id) => {
                let item_opt = self.handle.queue.write().select_id(id).cloned();
                if let Some(item) = item_opt {
                    self.start_playback(item);
                }
            }
            PlaybackCommand::Pause => {
                let state = self.handle.status.read().state;
                if state == PlayerState::Play {
                    let _ = self.worker_tx.send(WorkerCommand::Pause);
                    self.handle.status.write().state = PlayerState::Pause;
                    let _ = self
                        .handle
                        .event_tx
                        .send(PlaybackEvent::StateChanged(PlayerState::Pause));
                }
            }
            PlaybackCommand::Resume => {
                let state = self.handle.status.read().state;
                if state == PlayerState::Pause {
                    let _ = self.worker_tx.send(WorkerCommand::Resume);
                    self.handle.status.write().state = PlayerState::Play;
                    let _ = self
                        .handle
                        .event_tx
                        .send(PlaybackEvent::StateChanged(PlayerState::Play));
                } else if state == PlayerState::Stop {
                    let item_opt = self.handle.queue.write().current_or_first().cloned();
                    if let Some(item) = item_opt {
                        self.start_playback(item);
                    }
                }
            }
            PlaybackCommand::TogglePause => {
                let state = self.handle.status.read().state;
                match state {
                    PlayerState::Play => self.handle_command(PlaybackCommand::Pause),
                    PlayerState::Pause | PlayerState::Stop => {
                        self.handle_command(PlaybackCommand::Resume)
                    }
                }
            }
            PlaybackCommand::Stop => {
                self.stop_playback();
            }
            PlaybackCommand::Next => {
                let (repeat, single, consume) = {
                    let st = self.handle.status.read();
                    (st.repeat, st.single, st.consume)
                };
                let next_opt = {
                    let mut q = self.handle.queue.write();
                    let next = q.advance_next(repeat, single, consume);
                    let mut st = self.handle.status.write();
                    st.queue_version = q.version();
                    st.queue_length = q.len() as u32;
                    next
                };
                if let Some(next_item) = next_opt {
                    self.start_playback(next_item);
                } else {
                    self.stop_playback();
                }
            }
            PlaybackCommand::Previous => {
                let repeat = self.handle.status.read().repeat;
                let prev_opt = self.handle.queue.write().step_previous(repeat);
                if let Some(prev_item) = prev_opt {
                    self.start_playback(prev_item);
                }
            }
            PlaybackCommand::Seek(target) => {
                let _ = self.worker_tx.send(WorkerCommand::Seek(target));
            }
            PlaybackCommand::SetVolume(vol) => {
                let clamped = vol.min(100);
                self.handle.status.write().volume = clamped;
                let _ = self
                    .handle
                    .event_tx
                    .send(PlaybackEvent::VolumeChanged(clamped));
            }
            PlaybackCommand::SetRepeat(val) => {
                self.handle.status.write().repeat = val;
                self.refresh_next_candidate();
                let _ = self.handle.event_tx.send(PlaybackEvent::OptionsChanged);
            }
            PlaybackCommand::SetRandom(val) => {
                self.handle.status.write().random = val;
                let _ = self.handle.event_tx.send(PlaybackEvent::OptionsChanged);
            }
            PlaybackCommand::SetSingle(val) => {
                self.handle.status.write().single = val;
                self.refresh_next_candidate();
                let _ = self.handle.event_tx.send(PlaybackEvent::OptionsChanged);
            }
            PlaybackCommand::SetConsume(val) => {
                self.handle.status.write().consume = val;
                let _ = self.handle.event_tx.send(PlaybackEvent::OptionsChanged);
            }
        }
    }

    fn start_playback(&mut self, item: PlaybackItem) {
        self.stop_flag.store(true, Ordering::SeqCst);
        let next_candidate = self.peek_next_item();
        {
            let mut st = self.handle.status.write();
            st.state = PlayerState::Play;
            st.current_item = Some(item.clone());
            st.current_pos = Some(item.position);
            st.elapsed = Duration::ZERO;
            st.last_error = None;
        }
        let _ = self.worker_tx.send(WorkerCommand::PlayTrack {
            item,
            next_candidate,
        });
    }

    fn stop_playback(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        let _ = self.worker_tx.send(WorkerCommand::Stop);
        {
            let mut st = self.handle.status.write();
            st.state = PlayerState::Stop;
            st.elapsed = Duration::ZERO;
            st.format = None;
        }
        let _ = self
            .handle
            .event_tx
            .send(PlaybackEvent::StateChanged(PlayerState::Stop));
    }

    fn peek_next_item(&self) -> Option<PlaybackItem> {
        let st = self.handle.status.read();
        self.handle
            .queue
            .read()
            .peek_next(st.repeat, st.single)
            .cloned()
    }

    fn refresh_next_candidate(&self) {
        let next = self.peek_next_item();
        let _ = self
            .worker_tx
            .send(WorkerCommand::UpdateNextCandidate(next));
    }

    fn emit_queue_updated(&self) {
        let st = self.handle.status.read();
        let _ = self.handle.event_tx.send(PlaybackEvent::QueueUpdated {
            version: st.queue_version,
            length: st.queue_length,
        });
    }

    fn handle_worker_notice(&mut self, notice: WorkerNotice) {
        match notice {
            WorkerNotice::TrackStarted {
                mut item,
                format,
                duration,
            } => {
                item.duration = duration;
                self.handle
                    .queue
                    .write()
                    .update_item_metadata(item.id, duration, None, None, None);
                {
                    let mut st = self.handle.status.write();
                    st.state = PlayerState::Play;
                    st.current_item = Some(item.clone());
                    st.current_pos = Some(item.position);
                    st.duration = duration;
                    st.elapsed = Duration::ZERO;
                    st.format = Some(format);
                }
                let _ = self
                    .handle
                    .event_tx
                    .send(PlaybackEvent::StateChanged(PlayerState::Play));
                let _ = self
                    .handle
                    .event_tx
                    .send(PlaybackEvent::FormatChanged(format));
                let _ = self
                    .handle
                    .event_tx
                    .send(PlaybackEvent::TrackStarted { item, format });
            }
            WorkerNotice::Position { elapsed, duration } => {
                {
                    let mut st = self.handle.status.write();
                    st.elapsed = elapsed;
                    if duration.is_some() {
                        st.duration = duration;
                    }
                }
                let _ = self
                    .handle
                    .event_tx
                    .send(PlaybackEvent::PositionUpdated { elapsed, duration });
            }
            WorkerNotice::GaplessTransitioned {
                mut item,
                format,
                duration,
            } => {
                // Advance queue cursor to match the gapless transition
                let (repeat, single, consume) = {
                    let st = self.handle.status.read();
                    (st.repeat, st.single, st.consume)
                };
                {
                    let mut q = self.handle.queue.write();
                    if let Some(prev_cur) = q.current() {
                        let _ = self
                            .handle
                            .event_tx
                            .send(PlaybackEvent::TrackEnded(prev_cur.id));
                    }
                    let _ = q.advance_next(repeat, single, consume);
                    q.update_item_metadata(item.id, duration, None, None, None);
                    item.duration = duration;
                    let mut st = self.handle.status.write();
                    st.queue_version = q.version();
                    st.queue_length = q.len() as u32;
                    st.current_item = Some(item.clone());
                    st.current_pos = q.current().map(|c| c.position);
                    st.duration = duration;
                    st.elapsed = Duration::ZERO;
                    st.format = Some(format);
                }
                self.refresh_next_candidate();
                let _ = self
                    .handle
                    .event_tx
                    .send(PlaybackEvent::TrackStarted { item, format });
            }
            WorkerNotice::TrackFinished { id } => {
                let _ = self.handle.event_tx.send(PlaybackEvent::TrackEnded(id));
                // Check if there is a next track to play
                let (repeat, single, consume) = {
                    let st = self.handle.status.read();
                    (st.repeat, st.single, st.consume)
                };
                let next_opt = {
                    let mut q = self.handle.queue.write();
                    let next = q.advance_next(repeat, single, consume);
                    let mut st = self.handle.status.write();
                    st.queue_version = q.version();
                    st.queue_length = q.len() as u32;
                    next
                };
                if let Some(next_item) = next_opt {
                    self.start_playback(next_item);
                } else {
                    let mut st = self.handle.status.write();
                    st.state = PlayerState::Stop;
                    st.elapsed = Duration::ZERO;
                    st.format = None;
                    drop(st);
                    let _ = self
                        .handle
                        .event_tx
                        .send(PlaybackEvent::StateChanged(PlayerState::Stop));
                }
            }
            WorkerNotice::TrackFailed { id, error } => {
                tracing::error!(track_id = id, %error, "track playback failed; isolating fault (R-024)");
                self.handle.status.write().last_error = Some(error.to_string());
                let _ = self.handle.event_tx.send(PlaybackEvent::Error(error));

                // Fault tolerance (R-024): try advancing to next track if any, or stop cleanly without crashing
                let (repeat, single, consume) = {
                    let st = self.handle.status.read();
                    (st.repeat, st.single, st.consume)
                };
                let next_opt = self
                    .handle
                    .queue
                    .write()
                    .advance_next(repeat, single, consume);
                if let Some(next_item) = next_opt
                    && next_item.id != id
                {
                    self.start_playback(next_item);
                } else {
                    let mut st = self.handle.status.write();
                    st.state = PlayerState::Stop;
                    st.format = None;
                    drop(st);
                    let _ = self
                        .handle
                        .event_tx
                        .send(PlaybackEvent::StateChanged(PlayerState::Stop));
                }
            }
        }
    }
}

/// Dedicated OS thread for audio decoding and blocking ALSA output (`docs/ARCHITECTURE.md` Section 12).
fn audio_worker_loop(
    mut output: Box<dyn AudioOutput>,
    policy: PathPolicy,
    worker_rx: std_mpsc::Receiver<WorkerCommand>,
    notice_tx: mpsc::UnboundedSender<WorkerNotice>,
    stop_flag: Arc<AtomicBool>,
) {
    let mut current_decoder: Option<(PlaybackItem, SymphoniaAudioDecoder, AudioFormat)> = None;
    let mut next_candidate: Option<PlaybackItem> = None;
    let mut prebuffered_next: Option<(PlaybackItem, SymphoniaAudioDecoder)> = None;
    let mut is_paused = false;
    let mut elapsed_frames: u64 = 0;
    let mut last_pos_report = Instant::now();

    loop {
        // When idle or paused, block waiting for the next command
        if current_decoder.is_none() || is_paused {
            match worker_rx.recv() {
                Ok(cmd) => {
                    if !handle_worker_cmd(
                        cmd,
                        &mut output,
                        &policy,
                        &notice_tx,
                        &stop_flag,
                        &mut current_decoder,
                        &mut next_candidate,
                        &mut prebuffered_next,
                        &mut is_paused,
                        &mut elapsed_frames,
                    ) {
                        break;
                    }
                    continue;
                }
                Err(_) => break,
            }
        }

        // Poll any pending commands non-blockingly while playing
        while let Ok(cmd) = worker_rx.try_recv() {
            if !handle_worker_cmd(
                cmd,
                &mut output,
                &policy,
                &notice_tx,
                &stop_flag,
                &mut current_decoder,
                &mut next_candidate,
                &mut prebuffered_next,
                &mut is_paused,
                &mut elapsed_frames,
            ) {
                return;
            }
        }

        if is_paused || current_decoder.is_none() {
            continue;
        }

        // Pre-open next track for gapless transition (`R-015`) if not yet opened
        if prebuffered_next.is_none()
            && let Some(cand) = next_candidate.as_ref()
            && matches!(cand.source, kiiaud_core::AudioSource::LocalFile(_))
        {
            if let Ok(dec) = SymphoniaAudioDecoder::open_source(&cand.source, &policy) {
                prebuffered_next = Some((cand.clone(), dec));
            } else {
                // Avoid retrying a broken next candidate in a tight loop
                next_candidate = None;
            }
        }

        // Decode and write one chunk
        let Some((cur_item, dec, cur_fmt)) = current_decoder.as_mut() else {
            continue;
        };

        match dec.read_chunk() {
            Ok(Some(chunk)) => {
                let chunk_frames = chunk.frames() as u64;
                if let Err(err) = output.write(&chunk) {
                    let failed_id = cur_item.id;
                    current_decoder = None;
                    let _ = output.stop();
                    let _ = notice_tx.send(WorkerNotice::TrackFailed {
                        id: failed_id,
                        error: err,
                    });
                    continue;
                }

                elapsed_frames = elapsed_frames.saturating_add(chunk_frames);
                if last_pos_report.elapsed() >= Duration::from_millis(250) {
                    last_pos_report = Instant::now();
                    let rate_hz = match cur_fmt {
                        AudioFormat::Pcm { sample_rate, .. } => *sample_rate,
                        AudioFormat::Dsd { rate, .. } => rate.byte_rate_hz(),
                    }
                    .max(1);
                    let elapsed = Duration::from_secs_f64(elapsed_frames as f64 / rate_hz as f64);
                    let _ = notice_tx.send(WorkerNotice::Position {
                        elapsed,
                        duration: dec.duration(),
                    });
                }
            }
            Ok(None) => {
                // End of current track! Try seamless gapless transition (`R-015`) if prebuffered_next is ready
                let finished_id = cur_item.id;
                if let Some((next_item, next_dec)) = prebuffered_next.take() {
                    next_candidate = None;
                    match output.open(&next_dec.format()) {
                        Ok(neg_fmt) => {
                            let dur = next_dec.duration();
                            elapsed_frames = 0;
                            last_pos_report = Instant::now();
                            let _ = notice_tx.send(WorkerNotice::GaplessTransitioned {
                                item: next_item.clone(),
                                format: neg_fmt,
                                duration: dur,
                            });
                            current_decoder = Some((next_item, next_dec, neg_fmt));
                            continue;
                        }
                        Err(err) => {
                            let failed_id = next_item.id;
                            current_decoder = None;
                            let _ = output.stop();
                            let _ = notice_tx.send(WorkerNotice::TrackFailed {
                                id: failed_id,
                                error: err,
                            });
                            continue;
                        }
                    }
                }

                // No prebuffered gapless track: drain hardware buffer and notify coordinator
                let _ = output.drain();
                current_decoder = None;
                let _ = notice_tx.send(WorkerNotice::TrackFinished { id: finished_id });
            }
            Err(err) => {
                let failed_id = cur_item.id;
                current_decoder = None;
                let _ = output.stop();
                let _ = notice_tx.send(WorkerNotice::TrackFailed {
                    id: failed_id,
                    error: err,
                });
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_worker_cmd(
    cmd: WorkerCommand,
    output: &mut Box<dyn AudioOutput>,
    policy: &PathPolicy,
    notice_tx: &mpsc::UnboundedSender<WorkerNotice>,
    stop_flag: &Arc<AtomicBool>,
    current_decoder: &mut Option<(PlaybackItem, SymphoniaAudioDecoder, AudioFormat)>,
    next_candidate: &mut Option<PlaybackItem>,
    prebuffered_next: &mut Option<(PlaybackItem, SymphoniaAudioDecoder)>,
    is_paused: &mut bool,
    elapsed_frames: &mut u64,
) -> bool {
    match cmd {
        WorkerCommand::PlayTrack {
            item,
            next_candidate: next_cand,
        } => {
            stop_flag.store(false, Ordering::SeqCst);
            *is_paused = false;
            *elapsed_frames = 0;
            *next_candidate = next_cand;
            *prebuffered_next = None;

            match SymphoniaAudioDecoder::open_source(&item.source, policy) {
                Ok(dec) => match output.open(&dec.format()) {
                    Ok(neg_fmt) => {
                        let duration = dec.duration();
                        let _ = notice_tx.send(WorkerNotice::TrackStarted {
                            item: item.clone(),
                            format: neg_fmt,
                            duration,
                        });
                        *current_decoder = Some((item, dec, neg_fmt));
                    }
                    Err(err) => {
                        *current_decoder = None;
                        let _ = output.stop();
                        let _ = notice_tx.send(WorkerNotice::TrackFailed {
                            id: item.id,
                            error: err,
                        });
                    }
                },
                Err(err) => {
                    *current_decoder = None;
                    let _ = notice_tx.send(WorkerNotice::TrackFailed {
                        id: item.id,
                        error: err,
                    });
                }
            }
            true
        }
        WorkerCommand::UpdateNextCandidate(cand) => {
            *next_candidate = cand;
            *prebuffered_next = None;
            true
        }
        WorkerCommand::Pause => {
            *is_paused = true;
            let _ = output.pause();
            true
        }
        WorkerCommand::Resume => {
            *is_paused = false;
            let _ = output.resume();
            true
        }
        WorkerCommand::Seek(target) => {
            if let Some((_, dec, fmt)) = current_decoder.as_mut() {
                if let Err(err) = dec.seek(target) {
                    tracing::warn!(%err, "seek failed");
                } else {
                    let rate_hz = match fmt {
                        AudioFormat::Pcm { sample_rate, .. } => *sample_rate,
                        AudioFormat::Dsd { rate, .. } => rate.byte_rate_hz(),
                    }
                    .max(1);
                    *elapsed_frames = (target.as_secs_f64() * rate_hz as f64) as u64;
                    let _ = notice_tx.send(WorkerNotice::Position {
                        elapsed: target,
                        duration: dec.duration(),
                    });
                }
            }
            true
        }
        WorkerCommand::Stop => {
            *is_paused = false;
            *current_decoder = None;
            *next_candidate = None;
            *prebuffered_next = None;
            *elapsed_frames = 0;
            let _ = output.stop();
            true
        }
        WorkerCommand::Shutdown => {
            let _ = output.stop();
            false
        }
    }
}
