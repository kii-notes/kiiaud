# kiiaud Architecture Specification

## 1. Architectural Layers

Recommended Rust workspace crate structure:

```text
kiiaud/
├── Cargo.toml
├── crates/
│   ├── kiiaud-core/              # Domain models, commands, events, AudioSource trait
│   ├── kiiaud-playback/          # Playback engine, in-memory queue, buffering state machine
│   ├── kiiaud-decoder/           # Decoder traits & decoders (WavPack DSD/PCM, FLAC, WAV, DSF, DFF)
│   ├── kiiaud-output/            # AudioOutput trait, DoP encoder & mock/sink backends
│   │   ├── kiiaud-output-alsa/   # ALSA hardware direct output for Linux (Bit-perfect, Native DSD, DoP)
│   │   ├── kiiaud-output-wasapi/ # WASAPI Exclusive Mode output for Windows (Bit-perfect, DoP, MMCSS)
│   │   └── kiiaud-output-android/# Android output via Oboe / AAudio (Capability-driven, Exclusive/Shared, DoP)
│   ├── kiiaud-adapter-jellyfin/  # Jellyfin "Play To" remote control adapter (WebSocket + REST)
│   ├── kiiaud-adapter-lms/       # LMS / Lyrion SlimProto client adapter (Squeezelite-like)
│   ├── kiiaud-adapter-mpd/       # MPD text protocol control adapter
│   ├── kiiaud-adapter-cli/       # Local CLI / IPC control client (Unix socket / Windows TCP)
│   ├── kiiaud-adapter-web/       # Embedded Hi-Fi WebUI & REST/SSE status adapter
│   └── kiiaud/                   # Main daemon binary, configuration loader & adapter coordinator
├── docs/
└── tests/
```

During early development, some crates may be grouped together, but the layer boundaries and dependency directions must remain clean and strictly decoupled.

---

## 2. Core Domain Model (`kiiaud-core`)

`kiiaud-core` defines protocol-agnostic domain objects, command channels, and event streams.

### Key Types

```rust
pub enum AudioSource {
    LocalFile(PathBuf),
    HttpStream {
        url: Url,
        headers: HeaderMap,
    },
    RawStream(Box<dyn AsyncRead + Send + Unpin>),
}

pub struct PlaybackItem {
    pub id: QueueId,
    pub source: AudioSource,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub duration: Option<Duration>,
}

pub enum PlaybackCommand {
    Load(AudioSource, bool /* autostart */),
    Enqueue(AudioSource),
    Clear,
    Remove(QueueId),
    Play,
    Pause,
    Resume,
    Stop,
    Next,
    Previous,
    Seek(Duration),
    SetVolume(u8), // Optional hardware/mixer control
}

pub enum PlaybackEvent {
    StateChanged(PlayerState),
    TrackStarted(PlaybackItem, AudioFormat),
    TrackEnded(QueueId),
    PositionUpdated(Duration),
    FormatChanged(AudioFormat),
    Error(PlaybackError),
}
```

External adapters (Jellyfin, LMS, MPD, CLI) MUST NOT leak their protocol-specific types into the core. Instead, they translate incoming network commands into `PlaybackCommand`s and consume `PlaybackEvent`s to report state back to their respective controllers.

---

## 3. Audio Source & Stream Abstraction

An audio source is not limited to local disk files. `kiiaud` supports:

1. **Local Files (`file://` or direct path)**:
   - Direct file handle supporting random access (`std::io::Seek`).
2. **HTTP/HTTPS Direct Streams**:
   - For Jellyfin "Play To" and internet radio streams.
   - Supports HTTP Range headers (`Range: bytes=X-Y`) to enable seeking within remote files without downloading the entire track.
3. **TCP Stream Chunks**:
   - For LMS / Lyrion SlimProto audio streaming.

---

## 4. In-Memory Runtime Queue

The queue is strictly runtime state:

- Maintained entirely in RAM;
- No SQLite, no persistent database on disk;
- A daemon restart clears the queue unless an optional state snapshot is explicitly configured.

---

## 5. Playback Engine (`kiiaud-playback`)

The playback engine coordinates:

- Resolving `AudioSource` into readable byte streams;
- Format detection & decoder selection;
- Managing audio frame buffers and pre-buffering for gapless transitions;
- Synchronizing playback position and reporting events;
- Driving the `AudioOutput` backend.

---

## 6. Decoder Layer (`kiiaud-decoder`)

Every decoder produces either uncompressed **PCM frames** or raw **DSD bitstreams**.

```rust
pub trait Decoder: Send {
    fn format(&self) -> AudioFormat;
    fn duration(&self) -> Option<Duration>;
    fn seek(&mut self, position: Duration) -> Result<()>;
    fn read_chunk(&mut self, buffer: &mut AudioBuffer) -> Result<usize>;
}
```

### Supported Formats & Priorities

1. **WavPack (`.wv`) — High Priority**:
   - **WavPack DSD**: Losslessly compressed DSD (saving ~50% disk space on DSF/DFF). Must decode directly to DSD bitstream.
   - **WavPack PCM**: Lossless PCM compression.
2. **FLAC (`.flac`)**: Standard lossless PCM up to 24-bit/192kHz+.
3. **WAV / AIFF (`.wav`, `.aif`)**: Uncompressed PCM.
4. **DSF / DFF (`.dsf`, `.dff`)**: Uncompressed native DSD bitstream (DSD64, DSD128, DSD256, DSD512).
5. **DST (Direct Stream Transfer)**: *Explicitly deferred* due to CPU complexity; DSF and WavPack DSD take immediate precedence.

---

## 7. Audio Format Model

```rust
pub enum AudioFormat {
    Pcm {
        sample_rate: u32,       // e.g. 44100, 96000, 192000
        channels: u16,          // e.g. 2
        sample_format: SampleFormat, // S16_LE, S24_3LE, S32_LE, F32_LE
    },
    Dsd {
        rate: DsdRate,          // Dsd64 (2.8MHz), Dsd128 (5.6MHz), Dsd256 (11.2MHz), Dsd512
        channels: u16,
        transport: DsdTransport, // Native or DoP
    },
}
```

DSD is a first-class citizen and is never silently downsampled or converted to PCM.

---

## 8. Output Abstraction (`kiiaud-output`)

```rust
pub trait AudioOutput: Send {
    fn open(&mut self, format: &AudioFormat) -> Result<AudioFormat>;
    fn write(&mut self, buffer: &AudioBuffer) -> Result<usize>;
    fn pause(&mut self) -> Result<()>;
    fn resume(&mut self) -> Result<()>;
    fn drain(&mut self) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
    fn capabilities(&self) -> OutputCapabilities;
}
```

- **ALSA Hardware Direct (`kiiaud-output-alsa`)**: The primary production output for Linux/RPi 5.
- **WASAPI Exclusive Mode (`kiiaud-output-wasapi`)**: Bit-perfect production output for Windows (bypasses Windows Audio Engine, event-driven streaming, MMCSS "Pro Audio", DoP for DSD).
- **Android Oboe / AAudio (`kiiaud-output-android`)**: Native Android output backend leveraging Google's Oboe library. Supports capability negotiation (Exclusive mode vs Shared mixer, dynamic sample rates, and DoP streaming).
- **Mock/Sink Output**: Used for unit tests and local development across platforms.
- **Extensibility**: Future backends (e.g. macOS CoreAudio, PipeWire) can be added without modifying the engine or decoders.

---

## 9. Hardware Output Backends & Capability Negotiation

In `kiiaud`, **bit-perfect playback is treated as a capability of the output backend and active hardware path**, rather than an assumed invariant. Backends report their capabilities (`OutputCapabilities`), enabling the WebUI signal path monitor and protocol adapters to accurately report whether the output is direct bit-perfect or running through a system mixer.

### 9.1 ALSA Output Pipeline (Linux)
- Opens hardware devices directly via ALSA `hw:CARD,DEV` (bypassing `plughw`, `dmix`, and software resamplers).
- Disables software volume attenuation (hardware volume or fixed 100% bit-transparent passthrough).
- Configures ALSA buffer and period sizes to guarantee jitter-free, underrun-free streaming.
- Re-configures ALSA hardware parameters on track sample rate/format changes.

### 9.2 WASAPI Exclusive Mode Pipeline (Windows)
- Opens audio endpoints in `AUDCLNT_SHAREMODE_EXCLUSIVE` mode, completely bypassing the Windows Audio Engine mixer, APOs, and software volume limiter.
- Operates in event-driven streaming mode (`AUDCLNT_STREAMFLAGS_EVENTCALLBACK`) tied to driver interrupts for zero jitter.
- Supports bit-perfect PCM: `S16_LE`, `S24_LE` (in 32-bit container or packed 24-bit), `S32_LE`, and `Float32_LE`.
- Supports bit-perfect DSD via **DoP (DSD over PCM)** (`176.4kHz` for DSD64, `352.8kHz` for DSD128, `705.6kHz` for DSD256).
- Registers audio threads with Windows Multimedia Class Scheduler Service (**MMCSS**) under `"Pro Audio"` characteristics.

### 9.3 Android Oboe / AAudio Pipeline (`kiiaud-output-android`)
- **Oboe / AAudio HAL**: Leverages AAudio (Android 8.1+) with fallback to OpenSL ES for low-latency streaming to USB DACs or internal audio hardware.
- **Push-to-Pull Bridge**: An internal lock-free ring buffer bridges the playback engine's synchronous `write()` thread to Oboe's asynchronous real-time audio callback thread.
- **Exclusive vs Shared Mode**: If configured for `bit_perfect = true`, requests `SharingMode::Exclusive` and `PerformanceMode::LowLatency`. If the Android device or HAL permits exclusive access (e.g. specialized Android DAP or USB DAC direct path), bit-perfect is maintained. If the HAL falls back to `SharingMode::Shared`, capability reporting flags `bit_perfect = false` and signals that the stream is routed through the system mixer.
- **DoP for DSD**: Converts DSF, DFF, and WavPack DSD tracks to DoP frames for bit-perfect transmission over PCM.
- **Two-Phase Architecture**:
  1. *Phase 1 — Native Daemon*: Binary built for `aarch64-linux-android`, runnable headless via ADB or Termux (ideal for Android TV Boxes as dedicated music endpoints).
  2. *Phase 2 — Android Foreground Service*: Thin wrapper app holding `WakeLock`/`WifiLock` and exposing the WebUI/APIs.

---

## 10. DSD Pipeline

```text
DSD Sources:
DSF / DFF / WavPack DSD (.wv)
   │
   ├── Native DSD mode (SND_PCM_FORMAT_DSD_U32_BE / LE)
   │   └── Sent directly to DACs supporting native DSD over USB/I2S
   │
   └── DoP mode (DSD over PCM)
       └── Packed into 24-bit PCM frames (16-bit DSD + 8-bit 0x05/0xFA markers)
```

If the DAC does not support native DSD or DoP at the required rate, `kiiaud` logs an explicit error or halts playback; it will never secretly downmix to PCM while claiming bit-perfect playback.

---

## 11. Controller & Protocol Adapters (Equal Citizens)

All adapters are modular frontends that communicate with the core engine via channels:

```text
┌────────────────────────┐      PlaybackCommand
│   Hi-Fi WebUI (7110)   ├──────────────────────────┐
└────────────────────────┘                          │
┌────────────────────────┐                          │
│   Jellyfin Adapter     ├──────────────────────────┤
└────────────────────────┘                          │
┌────────────────────────┐                          ▼
│   LMS/SlimProto Adapter├──────────────────► [ kiiaud-core ]
└────────────────────────┘                          ▲
┌────────────────────────┐                          │
│   MPD Adapter (6600)   ├──────────────────────────┤
└────────────────────────┘                          │
┌────────────────────────┐                          │
│   CLI / Local IPC      ├──────────────────────────┘
└────────────────────────┘
```

### 11.1 Jellyfin Adapter ("Play To" Remote Control)
- Registers a media playback session with the Jellyfin Server (`Client: kiiaud`, `Device: Raspberry Pi 5`).
- Listens to WebSocket events for remote control (`Play`, `Pause`, `Stop`, `Seek`).
- Receives direct audio stream URLs (`/Audio/{itemId}/stream?static=true`).
- Reports playback progress periodically (`/Sessions/Playing/Progress`) so the Jellyfin Web/Mobile UI displays real-time playback position.

### 11.2 LMS / Lyrion Music Server Adapter (SlimProto)
- Acts as a modern, lightweight Rust counterpart to `squeezelite`.
- Connects to LMS via SlimProto (TCP port 3483).
- Handles discovery, stream setup (`strm`), player status (`stat`), and receives bit-perfect PCM/DSD streams from LMS.
- Allows users who manage large collections in Lyrion/LMS to use `kiiaud` as a high-end audio endpoint.

### 11.3 MPD Protocol Adapter
- Minimal TCP server on port 6600.
- Implements playback control commands (`play`, `pause`, `stop`, `next`, `prev`, `seek`, `status`, `add`, `clear`).
- Accepts local file paths or stream URLs without needing an internal MPD music database.

### 11.4 CLI & Local IPC
- Direct command-line interface (`kiiaudctl`) and lightweight JSON RPC protocol for local automation, scripting, and debugging.
- Cross-platform transport: Unix domain socket (`/tmp/kiiaud.sock` or `/run/kiiaud/kiiaud.sock`) on Linux/macOS, and local loopback TCP socket (`127.0.0.1:7111`) on Windows.

### 11.5 Embedded Hi-Fi WebUI & REST/SSE Adapter (`kiiaud-adapter-web`)
- Single-binary embedded SPA (`ui.html`) served on port **`7110`** (`KII-0`), zero runtime asset dependencies, zero disk writes during playback.
- **Audiophile Signal Path Inspection**: Real-time visualization of audio source format, Symphonia decoder, bit-perfect status, and output hardware device.
- **Real-Time State Sync (SSE)**: Streams events (`/api/events`) directly to the browser upon `PlaybackEvent` broadcasts, keeping track elapsed time and playback status synchronized with zero polling.
- **In-Memory Queue & File Browser**: Interactive queue management and zero-database local audio file browsing (`/api/browse`).
- **Hardware DAC Scanner & Config Editor**: Discovers connected USB/I2S audio hardware and allows hot-reloading `kiiaud.toml` configurations.

---

## 12. Threading & Concurrency Model

```text
  Async Network Runtime (Tokio)                Dedicated OS Thread
┌─────────────────────────────────┐          ┌───────────────────────┐
│ - Jellyfin WebSocket / HTTP     │          │ Realtime Audio Thread │
│ - LMS SlimProto TCP Client      │          │                       │
│ - MPD TCP Listener              │          │ - Buffer fetch        │
│ - Security & Path Validation    │          │ - Decoder step        │
│                │                │          │ - Blocking ALSA write │
│                ▼                │          └───────────▲───────────┘
│       mpsc (Commands)           │                      │
│                └────────────────┼──────────────────────┘
└─────────────────────────────────┘      crossbeam / ringbuffer
```

Network operations never block the realtime audio thread, preventing audio dropouts, buffer underruns, or clock jitter.

---

## 13. Resource & Hardware Targets

- **Primary Target**: Raspberry Pi 5 / CM4 / low-power ARM64 TV Boxes running Linux.
- **Memory Footprint**: Target RSS < 30MB during active playback.
- **Disk I/O**: Zero persistent writes during continuous playback (protecting SD cards).
- **CPU**: < 2% CPU usage for PCM and native DSD on Raspberry Pi 5.

---

## 14. Configuration (`kiiaud.toml`)

```toml
[daemon]
name = "kiiaud-rpi5"

[output]
backend = "alsa"
device = "hw:CARD=DAC,DEV=0"
bit_perfect = true
dsd_mode = "auto" # "native", "dop", or "auto"

[security]
allowed_roots = ["/music", "/mnt/audio"]

[adapter.jellyfin]
enabled = true
server_url = "http://jellyfin.local:8096"
device_name = "kiiaud Endpoint"

[adapter.lms]
enabled = true
server_host = "lms.local"
player_name = "kiiaud Squeezebox"

[adapter.mpd]
enabled = true
listen = "0.0.0.0:6600"
```
