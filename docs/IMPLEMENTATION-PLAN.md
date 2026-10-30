# kiiaud Implementation Plan

This document guides the step-by-step development of `kiiaud`.

---

## Phase 0 — Repository Bootstrap & Architecture Setup
- Initialize Cargo workspace structure.
- Configure dependencies (`tokio`, `thiserror`, `tracing`, `serde`, etc.).
- Set up `docs/`, `LICENSE` (MIT / Apache-2.0 dual license), and `.gitignore`.
- Provide mock/sink audio output backend to allow compilation and unit testing on non-Linux hosts (Windows / macOS / CI).

---

## Phase 1 — Core Playback Model (`kiiaud-core`)
- Define `AudioSource` (Local file path, HTTP stream URL, TCP stream chunk).
- Define `PlaybackCommand` (`Load`, `Enqueue`, `Clear`, `Play`, `Pause`, `Resume`, `Stop`, `Next`, `Previous`, `Seek`).
- Define `PlaybackEvent` (`TrackStarted`, `PositionUpdated`, `FormatChanged`, `StateChanged`, `Error`).
- Implement in-memory runtime `Queue`.
- Set up command & event channels (`tokio::sync::mpsc`, `broadcast`).
- Build unit tests for queue operations and state machine transitions.

---

## Phase 2 — File & Stream Decoders (`kiiaud-decoder`)
Create the `Decoder` trait and implement core audio decoders:
1. **WavPack (`.wv`)**:
   - **WavPack DSD** (extracting raw DSD bitstream losslessly).
   - **WavPack PCM** (unpacking bit-perfect PCM samples).
2. **FLAC (`.flac`)**: High-res PCM decoding.
3. **WAV / AIFF (`.wav`, `.aif`)**: Raw uncompressed PCM.
4. **DSF / DFF (`.dsf`, `.dff`)**: Uncompressed native DSD frames.

*(Note: DST decompression is deferred).*

---

## Phase 3 — ALSA Output Pipeline (`kiiaud-output-alsa`)
Adapt the robust audio output pipeline from `rmpd-player` (M0Rf30/rmpd):
- Hardware-direct access (`hw:CARD,DEV`), avoiding `dmix` and software mixers.
- Automatic hardware configuration on sample rate or format change.
- Native DSD transport (`SND_PCM_FORMAT_DSD_U32_BE/LE`).
- DoP transport (DSD over PCM 24/176.4kHz+).
- Dedicated realtime playback thread to isolate audio rendering from network latency.
- Bit-perfect verification diagnostics (logging exact hardware rate, format, and channel mapping).

---

## Phase 4 — Local Control & Testing (`kiiaud-adapter-cli`)
- Implement a minimal CLI control tool (`kiiaudctl`) and IPC socket.
- Support basic verification commands:
  ```text
  kiiaudctl play /music/test.wv
  kiiaudctl play http://remote/audio.flac
  kiiaudctl pause
  kiiaudctl status
  ```
- Validate audio playback using mock/sink output on local workstation and ALSA on Raspberry Pi 5.

---

## Phase 5 — Modular Adapters

Each adapter is an independent, optional crate.

### 5A — Jellyfin Adapter (`kiiaud-adapter-jellyfin`)
- Connects to Jellyfin server via WebSocket and REST API.
- Registers playback session as a "Play To" cast target.
- Receives remote commands: `Play`, `Pause`, `Stop`, `Seek`.
- Streams audio via direct stream URL (`/Audio/{itemId}/stream?static=true`).
- Reports progress back to Jellyfin (`/Sessions/Playing/Progress`).

### 5B — LMS / Lyrion Adapter (`kiiaud-adapter-lms`)
- Connects to Lyrion Music Server (LMS) on TCP port 3483 (SlimProto protocol).
- Functions as an ultra-clean Rust alternative to [ralph-irving/squeezelite](https://github.com/ralph-irving/squeezelite).
- Handles discovery, stream commands (`strm`), player status (`stat`), and synchronizes playback.
- Decodes and outputs bit-perfect streams dispatched from LMS.

### 5C — MPD Protocol Adapter (`kiiaud-adapter-mpd`)
- Lightweight MPD TCP server on port 6600.
- Implements playback control subset (`play`, `pause`, `stop`, `next`, `prev`, `seek`, `status`, `add`, `clear`, `currentsong`).
- Accepts raw file paths and stream URLs without requiring an internal MPD music library database.
- Verifiable with standard tools (`mpc`, Cantata, etc.).

---

## Phase 6 — Security & File Policy
- Path traversal prevention for network-supplied paths.
- Configurable `allowed_roots` whitelist (`/music`, `/mnt/...`).
- Safe symlink resolution and rejection of invalid file types or non-audio binaries.

---

## Phase 7 — Systemd Packaging & RPi 5 Hardware Verification
- Write `kiiaud.service` for systemd integration on Raspberry Pi OS / Armbian.
- Run end-to-end tests with real USB DAC connected to Raspberry Pi 5:
  - Verify PCM bit-perfect rates (44.1kHz, 96kHz, 192kHz).
  - Verify WavPack DSD & uncompressed DSF native DSD output.
  - Verify DoP fallback.
  - Verify Jellyfin "Play To" control and LMS Squeezebox playback.

---

## Definition of Done for MVP

```text
[ ] kiiaud builds and runs on Linux (Raspberry Pi 5 / ARM64 and x86_64)
[ ] No database or filesystem scanning is required
[ ] Local and HTTP stream URLs can be ingested directly
[ ] WavPack (.wv) PCM and DSD play bit-perfect
[ ] FLAC, WAV, and DSF play bit-perfect
[ ] ALSA output operates in bit-perfect mode (Native DSD and DoP verified on USB DAC)
[ ] At least one remote adapter (Jellyfin or MPD or LMS) operates cleanly
[ ] Disconnecting a controller does not disrupt active audio playback
[ ] File errors/bad streams do not crash the daemon
[ ] systemd service unit runs the daemon as a headless appliance
```
