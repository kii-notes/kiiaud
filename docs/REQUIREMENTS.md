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
