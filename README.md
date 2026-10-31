# kiiaud — Application Specification

## 1. Purpose

`kiiaud` is a lightweight, headless, database-free network audio playback daemon written in Rust.

Its primary purpose is to turn a Linux device (especially low-power SBCs such as Raspberry Pi / TV Boxes) or Windows PC into a remotely controlled, bit-perfect audiophile endpoint.

The daemon is an **Audio Playback Endpoint / Renderer**, not a music server.

The central design principle is:

> A controller or stream source tells `kiiaud` what to play. `kiiaud` does not need to manage a music library database. It accepts local audio files or direct remote stream URLs and renders them to ALSA hardware on Linux, WASAPI Exclusive Mode on Windows, or Oboe/AAudio on Android with dynamic capability reporting.

Controllers and stream sources may include:
- **Hi-Fi WebUI**: Embedded browser-based control dashboard and signal path monitor on port `7110`;
- **Jellyfin**: Remote Control / "Play To" endpoint;
- **LMS / Lyrion Music Server**: SlimProto client (functioning similarly to `squeezelite`);
- **MPD clients**: `mpc`, Cantata, MAF, Ymuse...;
- **CLI / Local IPC / REST**: local scripting and automation (`kiiaudctl`).

## 2. Non-goals

`kiiaud` must not require:

- a music database;
- a library scanner or indexing daemon;
- persistent metadata indexing;
- an embedded server-side album/artist catalogue;
- a full Jellyfin or MPD server;
- a separate `rmpd` or `squeezelite` process;
- a heavyweight, database-driven web UI (a zero-dependency embedded SPA is provided for diagnostics and direct control);
- persistent storage writes during normal playback.

Local filesystems or remote servers (Jellyfin, LMS) remain the source of truth for audio files and library metadata.

## 3. Conceptual architecture

```text
                               Controllers & Source Adapters
             ┌─────────────────┬─────────────────┬─────────────────┬─────────────────┬─────────────────┐
             │                 │                 │                 │                 │                 │
         Hi-Fi WebUI       Jellyfin          LMS/Lyrion       MPD Client          CLI/IPC            Future
         (Port 7110)       (Play To)        (SlimProto)       (Control)       (Socket/TCP)        Adapters
             │                 │                 │                 │                 │                 │
             └─────────────────┴─────────────────┼─────────────────┴─────────────────┴─────────────────┘
                                                 │
                                                 ▼
                             ┌─────────────────────┐
                             │       kiiaud        │
                             │                     │
                             │  Audio Source / URI │
                             │  Command & Queue    │
                             │  Playback Engine    │
                             │  Decoders (WavPack, │
                             │   FLAC, DSF, DFF...)│
                             │  PCM / DSD Pipeline │
                             └──────────┬──────────┘
                                        │
                                        ▼
                             ┌────────────────────────┐
                             │   Output Abstraction   │
                             │  (ALSA on Linux,       │
                             │   WASAPI on Windows,   │
                             │   Oboe on Android,     │
                             │   Native DSD / DoP)    │
                             └──────────┬─────────────┘
                                        │
                                        ▼
                                    Hardware
                                (USB DAC / I2S)
```

The playback engine and audio pipeline form the core product.

Jellyfin, LMS (Lyrion), MPD, WebUI, and CLI are modular protocol and source adapters. None of them defines the identity of `kiiaud`.

## 4. Key properties

- **Rust implementation**: Memory-safe, high concurrency, zero-cost abstractions.
- **Cross-platform & Headless**: Engineered for Linux headless appliances (Raspberry Pi OS, Armbian, Alpine), Windows audiophile systems, and Android endpoints (TV Boxes, tablets, phones).
- **Capability-driven audio output & Bit-perfect path**:
  - **Linux**: ALSA hardware direct (`hw:`), avoiding `dmix`, software resampling, normalization, or software volume DSP.
  - **Windows**: WASAPI Exclusive Mode (`AUDCLNT_SHAREMODE_EXCLUSIVE`) with MMCSS Pro Audio scheduling, and ASIO (`kiiaud-output-asio`) for direct hardware streaming with **Native 1-bit DSD** (DSD64 to DSD1024) support.
  - **Android**: Oboe / AAudio (`kiiaud-output-android`) with capability negotiation (Exclusive mode vs Shared mixer, dynamic sample rates, and DoP).
  - **Bit-perfect Capability Reporting**: Signal path state and WebUI accurately report whether playback is direct bit-perfect or operating via a shared mixer.
- **First-class DSD support**: Native DSD (Direct Stream Digital) and DoP (DSD over PCM). Never converts DSD to PCM silently.
- **Audio format coverage**:
  - **WavPack (`.wv`)**: First-class priority for both **WavPack DSD** (lossless compression of DSF/DFF) and WavPack PCM.
  - **PCM**: FLAC, WAV, AIFF, WavPack.
  - **DSD**: DSF, DFF (uncompressed), WavPack DSD. *(DST decompression is explicitly deferred)*.
- **Multiple Source Types**: Local file paths (`file://` or `/music/...`) and network streams (`http://`, `https://` direct play streams from Jellyfin, LMS, or web radio).
- **Multi-Adapter Support**:
  - **WebUI & REST/SSE (`kiiaud-adapter-web`)**: Embedded Hi-Fi dashboard on port 7110 for real-time signal path inspection and playback controls.
  - **Jellyfin**: "Play To" client (WebSocket remote control + REST progress reports).
  - **LMS (Lyrion Music Server)**: SlimProto client (acting as an ultra-clean Rust-based alternative to Squeezelite).
  - **MPD**: Lightweight control protocol for standard MPD clients without needing an MPD database.
  - **CLI & Local IPC**: Cross-platform control (`kiiaudctl`) via Unix domain sockets on Linux and local loopback TCP on Windows.
- **Database-free & Minimal Storage I/O**: Zero database transactions during playback; protects SD cards on embedded hardware.
- **In-memory Runtime Queue**: Queue and player state reside purely in memory.
- **Gapless Playback**: Pre-buffering track transitions to eliminate gaps between contiguous tracks.

## 5. Acknowledgements & Reference Projects

`kiiaud` stands on the shoulders of giants in the open-source audio ecosystem. Sincere gratitude is extended to the authors and communities behind the following pioneering projects:

- **[rmpd](https://github.com/M0Rf30/rmpd)** & **[Symphonia fork](https://github.com/M0Rf30/Symphonia)** (Gianluca Boiano / M0Rf30, Philip Deljanov):
  - *Dual-licensed under MIT and Apache-2.0.*
  - `rmpd-player` provides a stellar pure-Rust reference implementation for bit-perfect ALSA output, native DSD (`DSD_U32_BE/LE`), and DoP framing. `kiiaud` adapts the audio output pipeline and buffering logic while purposefully omitting database and indexing components.
  - The Symphonia audio decoding fork delivers outstanding support for WavPack PCM, WavPack DSD, and DSF/DFF bitstreams.
- **[squeezelite](https://github.com/ralph-irving/squeezelite)** (Ralph Irving, Adrian Smith, Triode):
  - *GPL-3.0.*
  - The undisputed gold standard for Logitech Media Server / Lyrion Music Server SlimProto client playback, buffer timing, and DSD playback control. `kiiaud` implements a clean-room Rust adapter (`kiiaud-adapter-lms`) adhering to SlimProto specifications to act as a lightweight, zero-dependency endpoint.
- **[jellyfin-mpv-shim](https://github.com/jellyfin/jellyfin-mpv-shim)** (Ian Walton & the Jellyfin Community):
  - Serves as the key reference for Jellyfin client WebSocket remote control protocol, session lifecycle registration, Direct Play stream negotiation, and playback timeline reporting.
- **[Music Player Daemon (MPD)](https://www.musicpd.org/)**:
  - Inspiration for timeless client-server audio architecture and ubiquitous protocol compatibility across desktop and mobile clients.
- **Rust Audio & Async Ecosystem**:
  - Warm thanks to maintainers and contributors of `tokio`, `alsa-rs`, `windows-rs`, `oboe-rs`, `reqwest`, and `clap`.

## 6. Design Principles

Strictly separate these layers:

1. **Protocol / Controller Adapters**: Translates external commands and streams (WebUI, Jellyfin, LMS, MPD, CLI) into uniform `PlaybackCommand`s and `AudioSource`s.
2. **Audio Source & Stream Layer**: Resolves and buffers audio from local files, HTTP streams, or raw network sockets (`Read + Seek`).
3. **Playback Engine & Queue**: Manages the in-memory queue, track progression, state machine, and decoder coordination.
4. **Decoder Layer**: Demuxes and decodes frames into raw PCM or DSD bitstreams.
5. **Output Abstraction**: Feeds audio blocks to ALSA hardware on Linux or WASAPI Exclusive on Windows (or mock/sink output during testing), maintaining bit-perfect integrity.

## 7. Documentation

Detailed specifications and architecture documents are organized in [`docs/`](docs/):

- [Architecture Specification](docs/ARCHITECTURE.md)
- [Configuration Reference](docs/CONFIGURATION.md)
- [Functional & Non-Functional Requirements](docs/REQUIREMENTS.md)
- [Design Decisions](docs/DESIGN-DECISIONS.md)
- [Implementation Plan](docs/IMPLEMENTATION-PLAN.md)
- [Adapter Specifications (WebUI, MPD, Jellyfin, LMS, CLI)](docs/ADAPTERS.md)

## 8. Build & Quick Installation

### Local Build
```bash
cargo build --release
```

### Install for Current User (`~/.local/bin`):
```bash
make install-user
# Creates ~/.config/kiiaud/config.toml if not present
# Installs systemd user unit to ~/.config/systemd/user/kiiaud.service
```

### Install System-wide (`/usr/local/bin`):
```bash
sudo make install-system
# Installs to /usr/local/bin and /etc/kiiaud/config.toml
# Installs systemd system unit to /etc/systemd/system/kiiaud.service
```

### Configuration & Quick Usage
```bash
# Generate default config with comments:
kiiaud --generate-config

# Validate configuration:
kiiaud --check-config

# Run with Windows ASIO output backend (Native 1-bit DSD to DSD1024):
kiiaud -b asio -d "SXW UAD ASIO" --dsd-mode auto

# Inspect DAC hardware capabilities and registered ASIO drivers:
kiiaud -b asio --dac-info
```
See [Configuration Reference](docs/CONFIGURATION.md) for full details on hierarchy, options, and security.

## 9. Author's Note & Contributing

Dự án này được khởi xướng và phát triển xuất phát từ nhu cầu và sở thích trải nghiệm âm thanh cá nhân: tìm kiếm một giải pháp bit-perfect audio endpoint thuần túy, siêu nhẹ, không gánh nặng cơ sở dữ liệu và hoạt động bền bỉ, tin cậy trên các bo mạch nhúng (Raspberry Pi, SBCs) cũng như máy tính cá nhân.

`kiiaud` được phát hành công khai với tinh thần học hỏi, chia sẻ và ủng hộ văn hóa mã nguồn mở.

- **Đóng góp & Phản hồi (Issues / Pull Requests)**: Mọi ý kiến đóng góp, phản hồi trải nghiệm, báo lỗi hay gợi ý cải tiến đều rất được trân trọng.
- **Thời gian hỗ trợ**: Vì đây là dự án cá nhân được duy trì trong thời gian rảnh rỗi, tác giả sẽ cố gắng xem xét, thảo luận và hỗ trợ xử lý khi có thể sắp xếp thời gian hợp lý. Rất mong nhận được sự thông cảm từ cộng đồng!

*(This project was created to fulfill personal audio streaming needs and is shared freely in the spirit of open source. Issues and PRs are warmly welcomed and will be addressed as personal spare time permits).*

## 10. License

`kiiaud` is dual-licensed under either:

- **MIT License** ([LICENSE-MIT](LICENSE-MIT))
- **Apache License, Version 2.0** ([LICENSE-APACHE](LICENSE-APACHE))

at your option.
