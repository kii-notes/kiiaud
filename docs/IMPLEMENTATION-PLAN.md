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

## Phase 8 — Windows WASAPI Exclusive Mode & Loopback IPC
- Implement `kiiaud-output-wasapi` using `AUDCLNT_SHAREMODE_EXCLUSIVE` and MMCSS Pro Audio scheduling.
- Support event-driven streaming (`AUDCLNT_STREAMFLAGS_EVENTCALLBACK`) with zero jitter.
- Implement DoP v1.1 framing for DSD bitstreams over WASAPI Exclusive PCM frames.
- Adapt local IPC on Windows to loopback TCP (`127.0.0.1:7111`).

---

## Phase 9 — Steinberg ASIO 2.3 Output & Native 1-bit DSD
- Implement `kiiaud-output-asio` with COM FFI bindings to the Steinberg ASIO 2.3 driver interface.
- Scan Windows Registry (`HKLM\SOFTWARE\ASIO`) for automatic driver discovery and resolution.
- Support **Native 1-bit DSD** from DSD64 up to DSD1024 (`kAsioSetIoFormat`, `ASIOSTDSDInt8LSB1` / `MSB1`).
- Fallback to DoP v1.1 automatically if the ASIO driver lacks native DSD support.

---

## Phase 10 — Pure Rust Direct USB Audio Driver & Android Companion App
- Implement `kiiaud-output-android` direct USB engine bypassing AudioFlinger, AAudio, and kernel `snd-usb-audio`:
  - Isochronous URB ring buffer (80 URBs) with `USBDEVFS_URB_ISO_ASAP` scheduling.
  - UAC2 (High Speed, up to 32-bit/768kHz, Native DSD/DoP, clock entity parsing, and hardware PLL control).
  - UAC1 (Full Speed, up to 24-bit/96kHz) for vintage DAC compatibility.
  - Continuous asynchronous clock feedback (Q16.16) for drift-free playback.
- Build lightweight Kotlin companion app (`android/`) with USB DAC detection, permission management, and foreground audio service.

---

## Phase 11 — Lyrion Anywhere Gateway & DSD-First HELO Negotiation
- Implement remote WebSocket SlimProto tunnel (`AnywhereWs`, Protocol v2) to Lyrion Anywhere Gateway.
- Implement token authentication, pairing code exchange, and automatic access token refresh.
- Implement 256 KiB byte-acknowledgement flow control to advance the Gateway's replay buffer.
- Implement DSD-first capability ordering in SlimProto `HELO` packets to prioritize bit-perfect native DSD over PCM transcoding.

---

## Phase 12 — Embedded Hi-Fi WebUI & Real-Time Signal Path Inspection
- Implement zero-dependency single-binary web dashboard (`kiiaud-adapter-web`) on port `7110` (`ui.html`).
- Stream real-time events over Server-Sent Events (`/api/events`) with sub-second latency and zero polling.
- Expose audiophile signal path inspection (`Source -> Decoder -> DSP -> Output Hardware`).
- Implement zero-database directory browser (`/api/browse`), hardware DAC scanner, and live configuration editor.

---

## Phase 13 — Service Management, Single-Instance Lock & System Tray
- Implement built-in `kiiaud --service install | uninstall | start | stop | status`:
  - Windows: Task Scheduler user logon task (`schtasks.exe`), giving full access to audio DACs and ASIO drivers.
  - Linux: systemd user service unit (`systemctl --user`).
- Implement `SingleInstanceGuard` using Named Mutex on Windows and `flock` on Linux.
- Implement Windows System Tray integration (`--tray`, `--background`) with notification area icon, status tooltips, WebUI launcher, and console show/hide toggle.

---

## Phase 14 — Automated Multi-Architecture CI/CD Release Matrix
- Build and publish multi-platform releases on tag creation via GitHub Actions:
  - Linux x86_64 (`.tar.gz`)
  - Linux ARM64 (`.tar.gz` for Raspberry Pi / TV Boxes)
  - Windows x86_64 MSVC (`.zip` with ASIO and Tray support)
  - Android ARM64 (`.tar.gz` and companion APK)
- Automated SHA-256 checksum generation for release verification.

---

## Definition of Done

```text
[x] kiiaud builds and runs on Linux (Raspberry Pi 5 / ARM64 and x86_64)
[x] kiiaud builds and runs on Windows (MSVC x86_64 with ASIO, WASAPI, and System Tray)
[x] kiiaud builds and runs on Android (ARM64 with direct UAC1/UAC2 USB driver and companion app)
[x] No database or filesystem scanning is required
[x] Local and HTTP stream URLs can be ingested directly
[x] WavPack (.wv) PCM and DSD play bit-perfect
[x] FLAC, WAV, DSF, and DFF play bit-perfect
[x] ALSA, WASAPI Exclusive, ASIO, and Android USB outputs operate in bit-perfect mode
[x] Native DSD and DoP operate reliably across all hardware backends
[x] WebUI, Jellyfin, LMS (Local + Anywhere Gateway), MPD, and CLI adapters operate cleanly
[x] Disconnecting a controller does not disrupt active audio playback
[x] File errors/bad streams do not crash the daemon
[x] Built-in service management installs cleanly on Windows (Task Scheduler) and Linux (systemd)
[x] Automated CI/CD compiles, formats, tests, and publishes multi-architecture releases
```
