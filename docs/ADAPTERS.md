# kiiaud Adapter Specifications

`kiiaud` is designed with a pluggable adapter architecture. Each adapter translates external remote control protocols or streaming sessions into internal `PlaybackCommand`s and reports `PlaybackEvent`s back to the remote server or client.

---

## 1. Jellyfin Adapter ("Play To" Remote Control)

### Purpose
Allows users browsing media in Jellyfin (Web, Mobile App, TV) to select `kiiaud` from the "Play On / Cast" menu and stream audio bit-perfect directly to the endpoint.

### Workflow & Protocol
1. **Registration**:
   - The adapter establishes a WebSocket connection and registers an active session with the Jellyfin Server (`/Sessions/Capabilities`).
   - Declares device properties: `DeviceName: kiiaud`, `PlayableMediaTypes: ["Audio"]`, `SupportedCommands: ["Play", "Pause", "Stop", "Seek"]`.
2. **Remote Playback Command**:
   - Jellyfin sends a WebSocket command with an `ItemId` and playback parameters.
   - The adapter resolves the Direct Stream URL:
     ```text
     http://<jellyfin-host>:8096/Audio/<itemId>/stream?static=true
     ```
   - Dispatches `PlaybackCommand::Load(AudioSource::HttpStream(...))` to `kiiaud-core`.
3. **Progress & State Reporting**:
   - Periodically sends playback progress reports (`/Sessions/Playing/Progress`) with the current position (`PositionTicks`), volume, and pause state.
   - On track completion, sends `/Sessions/Playing/Stopped`.

---

## 2. LMS / Lyrion Music Server Adapter (SlimProto LAN)

### Purpose
Allows `kiiaud` to act as an audiophile-grade Squeezebox endpoint (analogous to a modern, pure-Rust implementation of [ralph-irving/squeezelite](https://github.com/ralph-irving/squeezelite)), controlled directly by Lyrion Music Server (LMS) on the local LAN (`DirectTcp`).

### Operating Mode & Protocol (`DirectTcp`)
- Connects directly to LMS on TCP port 3483 and fetches audio streams from `http://<lms_host>:<http_port>/stream.mp3?...`.
- Sends `HELO` with configured `mac_address` and `SETD` with `player_name`.
- Configured in `[adapter.lms]` (`enabled = true / false`).
- Hot-reloadable at runtime via `reload_lms_adapter()`.

### Workflow & Protocol (SlimProto)
1. **Discovery & Hello (`HELO`)**:
   - Connects to LMS on port 3483.
   - Sends a `HELO` packet advertising capabilities: MAC address, supported formats, buffer sizes, and flags (`CanHTTPS=1,AccuratePlayPoints=1,HasDigitalOut=1,HasPolarityInversion=1,ModelName={player_name}`).
   - **DSD-First Format Ordering**: When the active output backend supports DSD, format capabilities are ordered with DSD formats first (`dsf,dff,flc,mp3,pcm,...`). This ensures LMS does not transcode DSF/DFF tracks to PCM when serving audio to `kiiaud`, delivering bit-perfect native DSD streams directly.
2. **Stream Command (`strm`) & Response Protocol**:
   - LMS commands the player to start streaming a track:
     - `strm-s`: Start streaming audio data over an HTTP or raw TCP connection from LMS. The adapter immediately acknowledges with `STMf` (flush), `STMc` (connected), and a `RESP` frame containing HTTP 200 headers to clear the server's `connecting` lock.
     - `strm-p`: Pause playback (`STMp`).
     - `strm-u`: Unpause / resume playback (`STMr`).
     - `strm-q`: Flush buffers and stop (`STMf`).
     - `strm-t`: Timestamp query, echoed back via `STMt` with the requested timestamp.
3. **Status Reporting (`stat`)**:
   - Sends `STMs` once audio output starts (`TrackStarted`), `STMd` (DECODE_READY) on end of track for gapless transitions, and `STMu` on underrun/stop.
   - Reports progressive `bytes_received` and 1-second interval `STMt` during active playback (`Play`), maintaining clock synchronization without idle spamming.
4. **Audio Data**:
   - Audio chunks delivered from LMS are decoded and queued into `kiiaud`'s playback pipeline for bit-perfect output.

---

## 2B. Lyrion Anywhere Gateway Adapter (`lag`)

### Purpose
Provides remote access to LMS servers over the Internet via [Lyrion Anywhere Gateway](https://github.com/JameZUK/lyrion-anywhere) (`AnywhereWs` Protocol v2). It runs completely independently from the local LMS adapter, allowing users to enable or disable either service as needed, or run both concurrently.

### Architecture & Capabilities
- **Independent Configuration (`[adapter.lag]`)**:
  - `enabled = true / false`: Toggles remote Gateway connectivity without affecting local LAN LMS settings.
- **Authentication (`POST /api/lag/gateway-auth`)**:
  - Supports one-time pairing codes (`ABCDEFGHJK`), `lyrion-anywhere://pair?...` links, or username + password (+ optional 2FA/TOTP).
  - Exchanges credentials for a 90-day `gateway_refresh_token` and automatically rotates access tokens every 10 minutes via `POST /v1/auth/refresh`.
- **WebSocket SlimProto Tunnel (`WS /v1/player`)**:
  - Sends `{"token","protocol":2,"quality":"original"|"high"|"low","keepalive":true}`, receives the Gateway Greeting (`session`, `mac`, `name`), and adopts the Gateway-assigned virtual MAC in `HELO`.
- **Flow Control & Audio Streaming**:
  - Rewrites `strm-s` requests to `GET <gateway_url>/v1/stream/<session>/<seq>` with Bearer authentication.
  - Emits `{"ack":{"stream":<seq>,"bytes":<total>}}` flow-control packets every 256 KiB read so the Gateway's 4 MiB ring-buffer window advances smoothly.
- **Audiophile Signal Path Reporting**:
  - Emits real-time stream metadata indicating codec, rate, and whether `Bit-Perfect Passthrough` mode (`original`) is active.
- **Hot-Reloadable**:
  - Changes applied via WebUI or API take effect immediately via `reload_lag_adapter()`.

> [!NOTE]
> For a detailed technical post-mortem on LMS connection stability and SlimProto sequence alignment with Squeezelite, refer to [TROUBLESHOOTING-LMS.md](TROUBLESHOOTING-LMS.md).

---

## 3. MPD Protocol Adapter (Universal Playlist & Transport Controller)

### Purpose
Provides a lightweight MPD-compatible TCP server on port 6600. It functions as an audiophile **Universal Remote & Playlist Controller** for the active playback session, completely free of disk-heavy database indexing or SQLite overhead.

### Supported Command Set
- **Connection & Info**: `status`, `currentsong`, `stats`, `ping`, `close`, `commands`, `decoders`, `outputs`
- **Zero-DB File Browsing**: `lsinfo [path]` (direct filesystem traversal relative to configured `music_directory` without requiring a metadata database)
- **Queue & Playlist Ingestion**:
  - `add <path_or_url>`: adds a single file, URL, or **scans an entire directory recursively** (adding all valid FLAC, WavPack, DSD, WAV, MP3, Opus tracks sorted alphabetically).
  - `addid <path_or_url>`: same as `add`, returning allocated song `Id: <id>` lines.
  - `delete <pos>`, `deleteid <id>`, `clear`
- **Playlist Manipulation**:
  - `move <from_pos> <to_pos>`, `moveid <id> <to_pos>`: reorder tracks in the active queue.
  - `swap <pos1> <pos2>`, `swapid <id1> <id2>`: swap track positions.
  - `shuffle`: shuffle queue tracks preserving active playback cursor.
  - `playlistinfo`, `plchanges`: list queue items and metadata.
- **Playback Control**: `play [songpos]`, `playid [id]`, `pause [state]`, `stop`, `next`, `previous`, `seek <pos> <time>`, `seekid <id> <time>`, `seekcur <time>`
- **Playback Options**: `consume`, `repeat`, `random`, `single`, `setvol <vol>`, `volume <vol>`
- **Event Notification**: `idle`, `noidle` (`playlist`, `player`, `mixer`, `options`)
- **Command Batching**: `command_list_begin`, `command_list_ok_begin`, `command_list_end`

### Configuration
```toml
[adapter.mpd]
enabled = true
listen = "0.0.0.0:6600"
# Optional root directory for 'lsinfo' browsing and relative 'add' commands
# music_directory = "/mnt/music"
```

---

## 4. Local CLI & IPC Adapter

### Purpose
Provides local scripting, headless system management, and debugging without needing network controllers.

### Commands (`kiiaudctl`)
```bash
kiiaudctl play /music/album/track.wv
kiiaudctl play http://remote/track.flac
kiiaudctl pause
kiiaudctl resume
kiiaudctl stop
kiiaudctl next
kiiaudctl prev
kiiaudctl status
```
Communicates with the running `kiiaud` daemon over a cross-platform IPC transport:
- **Linux/Unix**: Unix domain socket (`/tmp/kiiaud.sock` or `/run/kiiaud/kiiaud.sock`).
- **Windows**: Local loopback TCP socket (`127.0.0.1:7111`).
Both transports deliver the identical newline-delimited JSON-RPC protocol.

---

## 5. Embedded Hi-Fi WebUI & REST/SSE Adapter (`kiiaud-adapter-web`)

### Purpose
Provides a zero-dependency, single-binary embedded web dashboard on port **`7110`** (`KII-0`) for:
- Real-time **Audiophile Signal Path** & Bit-Perfect hardware inspection (`Source -> Symphonia Decoder -> Bit-Perfect Pipeline -> Hardware Output`).
- **In-Memory Runtime Queue** management, transport controls (`Play`, `Pause`, `Stop`, `Next`, `Prev`, `Seek`), and a zero-database local filesystem browser.
- **Hardware DAC Scanner** (`/proc/asound/cards` & `/proc/asound/pcm` on Linux, WASAPI endpoints on Windows) and live configuration editor for `kiiaud.toml`.

### Architecture
- **Single-Binary Delivery**: The SPA HTML/CSS/JS is embedded into the compiled binary via `include_str!("ui.html")`. No Node.js, npm, or static file directories are needed at runtime.
- **Reactive Streaming via SSE**: Subscribes to Tokio broadcast channels (`PlaybackEvent`). When track positions, track changes, or status updates occur, an event frame is pushed over Server-Sent Events (`/api/events`), updating the UI with sub-second latency and zero polling.

### API Endpoints

#### 1. `GET /`
Serves the embedded Hi-Fi Single-Page Application (`ui.html`).

#### 2. `GET /api/state`
Returns a complete JSON snapshot of current playback state, audio format badge, signal path, queue, and detected audio output devices:
```json
{
  "status": {
    "state": "playing",
    "volume": 100,
    "repeat": false,
    "consume": false,
    "random": false,
    "single": false
  },
  "elapsed_secs": 42.5,
  "duration_secs": 284.0,
  "format_badge": "DSD64 (DoP)",
  "current_track": {
    "id": 1,
    "position": 0,
    "title": "01. Symphony No. 5",
    "artist": "Beethoven",
    "uri": "/music/DSD/beethoven_5.wv",
    "duration_secs": 284.0
  },
  "signal_path": {
    "source": "WavPack DSD (.wv)",
    "decoder": "Symphonia (Native DSD Passthrough)",
    "dsp": "None (Bit-Perfect Passthrough)",
    "output": "ALSA hw:CARD=Audio,DEV=0 (DoP 176.4kHz / S32_LE)"
  },
  "queue": [...],
  "config": {...},
  "lms_runtime": {
    "enabled": false,
    "connected": false,
    "endpoint": "192.168.1.100:3483",
    "active_mac": "02:4b:49:49:41:55",
    "active_name": "kiiaud Squeezebox",
    "last_error": null
  },
  "lag_runtime": {
    "enabled": true,
    "connected": true,
    "endpoint": "https://music.example.com",
    "active_mac": "02:a1:b2:c3:d4:e5",
    "active_name": "kiiaud Office DAC",
    "gateway_session": "sess_9f8e7d6c",
    "gateway_quality": "original",
    "stream_meta": {
      "stream": 1,
      "quality": "original",
      "served_format": "flac",
      "passthrough": true
    },
    "has_refresh_token": true,
    "last_error": null
  },
  "alsa_devices": [...]
}
```

#### 3. `GET /api/events`
Server-Sent Events (SSE) stream emitting real-time event updates:
```text
event: state
data: {"status":{"state":"playing",...},"elapsed_secs":43.0,...}
```

#### 4. `POST /api/control`
Executes transport and queue actions:
```json
// Play / Pause / Resume / Stop / Next / Previous
{"action": "play"}
{"action": "pause"}
{"action": "resume"}
{"action": "stop"}
{"action": "next"}
{"action": "prev"}

// Seek to timestamp (seconds)
{"action": "seek", "secs": 120.0}

// Set volume (0-100)
{"action": "volume", "volume": 85}

// Load track and start playing immediately
{"action": "load", "uri": "/music/album/01.flac", "title": "Track 1", "artist": "Artist"}

// Enqueue track to playback queue
{"action": "enqueue", "uri": "/music/album/02.flac", "title": "Track 2"}

// Queue manipulation
{"action": "delete", "pos": 2}
{"action": "clear"}

// Player mode options (repeat, consume, random, single)
{"action": "set_option", "option": "repeat", "value": true}
```

#### 5. `GET /api/browse?path=/music`
Browses audio files and folders within configured `security.allowed_roots` without requiring a persistent database:
```json
{
  "path": "/music",
  "entries": [
    {"name": "Classical", "path": "/music/Classical", "is_dir": true},
    {"name": "01.track.flac", "path": "/music/01.track.flac", "is_dir": false},
    {"name": "02.dsd_sample.wv", "path": "/music/02.dsd_sample.wv", "is_dir": false}
  ]
}
```

#### 6. `GET /api/config` & `POST /api/config`
Reads or persists changes to `kiiaud.toml` (e.g. updating output device, DSD mode, buffer time, or enabling/disabling protocol adapters).
- **Live LMS & LAG Hot-Reload**: Saving changes to `[adapter.lms]` or `[adapter.lag]` via `POST /api/config` immediately calls `reload_lms_adapter()` and `reload_lag_adapter()` respectively, updating hosts, tokens, or stream quality on the fly without restarting the `kiiaud` daemon.

#### 7. `POST /api/lag/gateway-auth` *(or `/api/lms/gateway-auth` for legacy compatibility)*
Performs interactive Lyrion Anywhere Gateway authentication and device pairing right from the WebUI configuration screen, persists the resulting `gateway_refresh_token` to `kiiaud.toml` (under `[adapter.lag]`), and hot-reloads the Gateway adapter:
```json
// 1. Pair using a 10-character one-time code or lyrion-anywhere://pair?... URL
{
  "action": "pair",
  "gateway_url": "https://music.example.com",
  "pairing_code": "ABCDEFGHJK",
  "device_name": "kiiaud Office DAC",
  "gateway_quality": "original"
}

// 2. Sign in with Gateway Username + Password (+ optional 2FA/TOTP code)
{
  "action": "login",
  "gateway_url": "https://music.example.com",
  "username": "alice",
  "password": "secret-password",
  "totp": "123456",
  "device_name": "kiiaud Office DAC",
  "gateway_quality": "original"
}

// 3. Clear stored Gateway refresh token and disconnect
{
  "action": "logout"
}
```
```



