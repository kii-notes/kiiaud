# kiiaud Design Decisions

## D-001 — Not a music server
`kiiaud` is an audio playback endpoint / renderer. It does not own or manage a music library database.

## D-002 — No database
The daemon must not require SQLite, PostgreSQL, Redis, or another database for ordinary playback.
Reason:
- low-resource devices (Raspberry Pi, TV Box);
- SD-card wear prevention;
- avoid unnecessary I/O;
- Jellyfin, LMS, or the local filesystem own library metadata.

## D-003 — File path or Stream URI is sufficient
The minimum information required to play a track is a playable source identifier (local path or network stream URL).
Example:
```text
/music/Artist/Album/01.flac
/music/DSD/Album/01.wv
http://jellyfin.local:8096/Audio/item-id/stream?static=true
```
The daemon does not need server-side catalog tables to decode and output audio.

## D-004 — Adapters are equal, modular frontends
MPD, Jellyfin, and LMS (Lyrion) are all modular frontends. None of them is the "master" or primary identity of `kiiaud`:
- **Jellyfin**: "Play To" remote control endpoint.
- **LMS (Lyrion)**: SlimProto client (like Squeezelite).
- **MPD**: Lightweight control interface for MPD clients.
- **CLI/IPC**: Local control and scripting.

## D-005 — Core engine is protocol-agnostic
The core playback engine must not know whether a command originated from MPD, Jellyfin, LMS, CLI, or an HTTP API. All adapters translate external protocol messages into standard `PlaybackCommand`s and read `PlaybackEvent`s.

## D-006 — rmpd is not a runtime process dependency
`kiiaud` does not run `rmpd` as an external subprocess. Instead, it embeds its own playback engine.

## D-007 — rmpd code reuse and license compliance
`rmpd` (M0Rf30/rmpd) is dual-licensed under **MIT** and **Apache-2.0**.
Its playback engine (`rmpd-player`) contains established ALSA bit-perfect output, DSD (native/DoP), and buffering logic.
`kiiaud` may adapt or reuse components from `rmpd-player`, retaining standard MIT/Apache attribution notices while discarding all of `rmpd`'s database/library code.

## D-008 — Bit-perfect is a fundamental design constraint
The default audio path avoids:
- software volume;
- resampling;
- DSP;
- normalization;
- unnecessary format conversion.
If hardware requires conversion or if conversion is explicitly requested, it must be explicitly reflected in diagnostics.

## D-009 — DSD is first-class
DSD is not abstracted as PCM. The daemon explicitly identifies whether output is:
- native DSD (e.g. `SND_PCM_FORMAT_DSD_U32_BE` or `LE`);
- DoP (DSD over PCM frames);
- PCM conversion (only if explicitly enabled as fallback).

## D-010 — Runtime state is enough for MVP
Queue and playback state reside entirely in memory. Persistence is deferred, avoiding any embedded database.

## D-011 — Pluggable output architecture
ALSA hardware-direct is the primary bit-perfect output for Linux/RPi 5. However, `AudioOutput` is an abstract trait, permitting mock outputs (for Windows/CI testing) and future sinks (PipeWire, stdout).

## D-012 — WavPack is a first-class container for DSD and PCM
WavPack (`.wv`) provides lossless compression for both PCM and DSD audio streams (WavPack 5 DSD mode). Because large DSD libraries are often stored in `.wv` to save disk space while preserving bit-perfection, WavPack decoding is prioritized alongside FLAC and uncompressed DSF/DFF.

## D-013 — DST decompression is explicitly deferred
Direct Stream Transfer (DST) requires intensive CPU processing and complex decompression routines. For initial phases, playback focuses on uncompressed DSF/DFF and WavPack-compressed DSD. DST support is deferred until the core engine and WavPack/DSF playback are stable.

## D-014 — Jellyfin MPV Shim reference for remote control protocol
`jellyfin-mpv-shim` serves as a behavioral reference for client session registration, WebSocket events, and REST progress reporting (`Sessions/Playing/Progress`) with the Jellyfin server. `kiiaud` implements its own clean-room Rust adapter for this workflow.

## D-015 — LMS / Squeezelite SlimProto reference
`ralph-irving/squeezelite` (https://github.com/ralph-irving/squeezelite) serves as the primary technical and protocol reference for SlimProto packet handling, stream synchronization, and DSD playback control. `kiiaud` implements an independent clean-room Rust adapter connecting to LMS on port 3483, allowing Lyrion Music Server to recognize and stream to `kiiaud` as a high-end audiophile endpoint.

## D-016 — Cross-platform Output: Linux ALSA & Windows WASAPI Exclusive Mode
`kiiaud` extends its bit-perfect rendering engine to Windows systems through `kiiaud-output-wasapi`:
- **WASAPI Exclusive Mode (`AUDCLNT_SHAREMODE_EXCLUSIVE`)**: Completely bypasses the Windows Audio Engine mixer, APO (Audio Processing Objects), and software volume DSP.
- **Event-Driven Streaming (`AUDCLNT_STREAMFLAGS_EVENTCALLBACK`)**: Uses hardware interrupt-driven buffer replenishment with auto-reset Win32 events for low latency and zero dropouts.
- **DoP for DSD on Windows**: Because standard WASAPI does not define a native 1-bit DSD stream format, DSD is transported bit-perfect using DoP (DSD over PCM frames at `176.4kHz`, `352.8kHz`, `705.6kHz`), matching the DAC's DoP decoder seamlessly.
- **MMCSS Pro Audio Scheduling**: Registers worker threads with Windows Multimedia Class Scheduler Service under `"Pro Audio"` to prevent playback glitching during system load.

## D-017 — Cross-platform Local CLI IPC (Unix Sockets & Windows Loopback TCP)
Local IPC for `kiiaudctl` and diagnostic scripting adapts dynamically to the host OS:
- **Linux/Unix**: Unix domain socket (`/tmp/kiiaud.sock` or `/run/kiiaud/kiiaud.sock`).
- **Windows**: Local loopback TCP socket (`127.0.0.1:7111`).
Both transports share the exact same newline-delimited JSON-RPC protocol, allowing `kiiaudctl` and scripting tools to control the daemon uniformly across all supported operating systems.

## D-018 — Embedded Hi-Fi WebUI & Real-time Audiophile Signal Path
While `kiiaud` remains headless and database-free (D-001, D-002), users require immediate visibility into bit-perfect hardware status and a way to control playback from any browser on the local network without installing separate apps:
- **Zero-Dependency Single-Binary**: The SPA frontend (`ui.html`) is compiled directly into the binary using `include_str!`. It requires no Node.js runtime, no npm packages, no separate web server (Nginx/Caddy), and zero external asset directories.
- **Dedicated Port `7110` (`KII-0`)**: Binds by default to port 7110, completely independent of MPD (`6600`) or LMS (`3483`).
- **Real-Time Server-Sent Events (`/api/events`)**: Connects directly to the internal Tokio broadcast channel for `PlaybackEvent`s. When track positions change, tracks advance, or volume changes occur, events stream instantaneously to the browser with zero HTTP polling overhead.
- **Audiophile Signal Path Inspection**: Exposes the complete signal path in real time (`Source -> Decoder -> Bit-Perfect Pipeline -> Output Hardware`), proving bit-perfect passthrough without hidden resampling.
- **Zero-Database Filesystem Browsing (`/api/browse`)**: Allows users to navigate local directory trees and enqueue files on demand without background filesystem scanning or database persistence.
