# kiiaud Requirements

## Functional requirements

### R-001
The application shall run as a headless Linux daemon.

### R-002
The application shall play audio from local filesystem paths and remote network URLs (e.g., HTTP/HTTPS direct streams from Jellyfin, LMS, or network sources).

### R-003
The application shall not require a music database.

### R-004
The application shall not scan the music filesystem as a prerequisite for playback.

### R-005
The application shall maintain a runtime playback queue in memory.

### R-006
The application shall expose playback state.

### R-007
The application shall support play, pause, stop, next, previous, and seek operations.

### R-008
The application shall provide an ALSA hardware-direct output backend.

### R-009
The application shall preserve the source audio format without resampling or DSP when the selected hardware/output path permits it (bit-perfect).

### R-010
The application shall support PCM playback (including FLAC, WAV, AIFF, and WavPack PCM).

### R-011
The application shall support DSD playback (including DSF, DFF, and WavPack-compressed DSD).

### R-012
The application should support native DSD output when the ALSA device supports it.

### R-013
The application should support DoP (DSD over PCM) where native DSD is unavailable but DoP is supported.

### R-014
The application shall not silently convert DSD to PCM while claiming bit-perfect DSD playback.

### R-015
The application should support gapless playback where technically possible.

### R-016 — Jellyfin Adapter
The application shall support acting as a remote control playback endpoint ("Play To") for Jellyfin via WebSocket events and REST progress reporting.

### R-017 — LMS / Lyrion (SlimProto) Adapter
The application shall support acting as an audio endpoint for Lyrion Music Server (LMS / Squeezebox) via the SlimProto protocol, receiving and rendering audio streams bit-perfect in a manner similar to `squeezelite`.

### R-018 — MPD Control Interface
The application shall provide an MPD-compatible control interface for standard MPD clients, usable without requiring an internal MPD music database.

### R-019 — Direct Path & URI Ingestion
The control interfaces shall be capable of receiving direct local file paths or remote stream URLs.

### R-020 — CLI & Local Control
The application should provide a CLI and IPC mechanism suitable for diagnostics and local control (Unix domain socket on Linux/macOS, loopback TCP on Windows).

### R-021 — Modular Adapter Independence
All adapters (Jellyfin, LMS, MPD, CLI) shall be modular, independent, and optional. Disconnecting or disabling any adapter shall not interrupt ongoing audio playback.

### R-022 — Diagnostic Logging
The application shall provide structured diagnostic logging exposing DAC sample rate, bit depth, PCM/DSD mode, and underrun warnings.

### R-023 — Security & Path Policy
The application shall have an allowed-root security mechanism for network-originated filesystem paths to prevent arbitrary filesystem traversal.

### R-024 — Fault Tolerance
A bad or unreadable track shall produce a recoverable error rather than terminating the daemon.

### R-025 — Hardware-Independent Testing
Protocol and core engine unit tests shall not require physical ALSA hardware (supporting mock/sink audio output).

### R-026 — WavPack Priority
The application shall prioritize WavPack (`.wv`) containers for both DSD (WavPack DSD) and PCM formats.

### R-027 — Embedded Hi-Fi WebUI & Real-time SSE
The application shall provide a zero-dependency, single-binary embedded web dashboard on port `7110` exposing real-time signal path inspection, playback controls, queue management, and directory browsing without persistent database writes.

### R-028 — Windows ASIO 2.3 Driver & Native 1-bit DSD
The application shall support direct hardware audio output on Windows using Steinberg ASIO 2.3 drivers, supporting **Native 1-bit DSD** bitstreams from DSD64 up to DSD1024 without mandatory DoP conversion, with automatic driver discovery in the Windows Registry.

### R-029 — Android Direct USB Audio (Pure Rust UAC1 + UAC2)
The application shall provide a dedicated pure Rust direct USB audio backend (`android-usb`) writing isochronous transfers directly over Linux `usbdevfs` ioctls using an allocated USB file descriptor from Android USB Host API, bypassing Android AudioFlinger, AAudio, and kernel ALSA drivers for true bit-perfect playback.

### R-030 — Built-in Background Service Management
The application shall provide built-in command-line actions (`--service install | uninstall | start | stop | status`) to manage its background execution, leveraging Windows Task Scheduler interactive logon tasks on Windows (to retain access to audio DACs and ASIO drivers) and systemd user services on Linux.

### R-031 — Single-Instance Hardware Guard
The application shall enforce single-instance mutual exclusion using platform-appropriate synchronization primitives (Named Mutex on Windows, non-blocking `flock` on Linux) to prevent multiple daemon instances from conflicting over hardware DAC endpoints or IPC sockets.

### R-032 — Windows System Tray & Headless Execution
The application shall support running in the background with hidden terminal console on Windows (`--background`), providing an interactive notification area tray icon (`--tray`) for status monitoring, WebUI launching, console toggling, and clean shutdown.

### R-033 — Lyrion Anywhere Gateway Remote Tunneling
The application shall support tunneling SlimProto binary commands and audio streaming over WebSocket (`AnywhereWs`) to a remote Lyrion Anywhere Gateway over the Internet with token authentication and byte-acknowledged flow control.

### R-034 — DSD-First Capability Ordering in SlimProto HELO
When the active DAC supports DSD, the SlimProto adapter shall advertise DSD formats (`dsf,dff`) first in the `HELO` packet to guarantee that the server streams lossless original DSD bitstreams rather than transcoding to PCM.

---

## Non-functional requirements

### NFR-001 — Low I/O
Normal playback shall not require continuous writes to persistent storage, protecting flash memory and SD cards.

### NFR-002 — Low memory
Memory consumption shall be bounded primarily by decoder and audio ringbuffers (Target RSS < 30MB).

### NFR-003 — Low CPU
No periodic library indexing or metadata scanning shall occur during idle operation.

### NFR-004 — Deterministic audio path
The audio path shall be inspectable from decoder output to hardware audio output (ALSA on Linux, WASAPI Exclusive Mode on Windows).

### NFR-005 — Modular protocols
Protocol adapters shall not contain playback-engine logic.

### NFR-006 — Modular output
The playback engine shall not depend directly on any specific protocol adapter, and the output layer shall remain decoupled from the decoders.

### NFR-007 — Failure isolation
A controller connection failure or network disconnect shall not stop ongoing playback.

### NFR-008 — Controller independence
The daemon shall continue playing uninterrupted if a controller disconnects.

### NFR-009 — Restart simplicity
The daemon shall be able to start instantly with no database migration, scan, or index-building step.

### NFR-010 — Deployment simplicity
The application should be suitable for systemd deployment on low-power Linux systems (e.g. Raspberry Pi 5).

---

## Explicitly deferred

The following are not part of the initial MVP:

- DST (Direct Stream Transfer) decompression (deferred; uncompressed DSF/DFF and WavPack DSD take priority);
- SACD ISO direct image extraction;
- Server-side music library search and album browsing;
- Persistent music database;
- Transcoding server;
- Heavyweight web UI;
- Cloud service dependency.
