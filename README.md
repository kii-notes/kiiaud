# kiiaud — Application Specification

## 1. Purpose

`kiiaud` is a lightweight, headless, database-free network audio playback daemon written in Rust.

Its primary purpose is to turn a Linux device (especially low-power SBCs such as Raspberry Pi / TV Boxes) or Windows PC into a remotely controlled, bit-perfect audiophile endpoint.

The daemon is an **Audio Playback Endpoint / Renderer**, not a music server.

The central design principle is:

> A controller or stream source tells `kiiaud` what to play. `kiiaud` does not need to manage a music library database. It accepts local audio files or direct remote stream URLs and renders them to ALSA hardware on Linux, ASIO / WASAPI Exclusive on Windows, or direct UAC1/UAC2 USB / AAudio on Android with dynamic capability reporting.

Controllers and stream sources may include:
- **Hi-Fi WebUI**: Embedded browser-based control dashboard and signal path monitor on port `7110`;
- **Jellyfin**: Remote Control / "Play To" endpoint;
- **LMS / Lyrion Music Server (LAN)**: SlimProto client for local network (`DirectTcp` port 3483, functioning similarly to `squeezelite`);
- **Lyrion Anywhere (Gateway)**: Independent remote internet streaming adapter (`[adapter.lag]`) connecting to **[Lyrion Anywhere Gateway](https://github.com/JameZUK/lyrion-anywhere)** via WebSocket tunnel (`WSS /v1/player`) with bit-perfect `original` passthrough;
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
                             │  Service & Tray Mgt │
                             └──────────┬──────────┘
                                        │
                                        ▼
                             ┌────────────────────────┐
                             │   Output Abstraction   │
                             │  (ALSA on Linux,       │
                             │   ASIO/WASAPI on Win,  │
                             │   UAC1/UAC2 on Android,│
                             │   Native DSD / DoP)    │
                             └──────────┬─────────────┘
                                        │
                                        ▼
                                    Hardware
                                (USB DAC / I2S)
```

The playback engine and audio pipeline form the core product.

Jellyfin, LMS (LAN), Lyrion Anywhere Gateway (LAG), MPD, WebUI, and CLI are modular protocol and source adapters. None of them defines the identity of `kiiaud`.

## 4. Key properties

- **Rust implementation**: Memory-safe, high concurrency, zero-cost abstractions.
- **Cross-platform & Headless**: Engineered for Linux headless appliances (Raspberry Pi OS, Armbian, Alpine), Windows audiophile workstations, and Android endpoints (TV Boxes, tablets, phones).
- **Capability-driven audio output & Bit-perfect path**:
  - **Linux**: ALSA hardware direct (`hw:`), avoiding `dmix`, software resampling, normalization, or software volume DSP.
  - **Windows**: ASIO (`kiiaud-output-asio`) for direct hardware streaming with **Native 1-bit DSD** (DSD64 to DSD1024) support, and WASAPI Exclusive Mode (`AUDCLNT_SHAREMODE_EXCLUSIVE`) with MMCSS Pro Audio scheduling.
  - **Android**: Dedicated **Pure Rust Direct USB Audio driver** (`android-usb` backend) bypassing AudioFlinger, AAudio, and kernel ALSA drivers via Linux `usbdevfs` ioctl for true bit-perfect UAC1/UAC2 streaming, plus Oboe / AAudio fallback (`oboe` / `android`).
  - **Bit-perfect Capability Reporting**: Signal path state and WebUI accurately report whether playback is direct bit-perfect or operating via a shared mixer.
- **Service & Desktop Integration**:
  - **Built-in Service Management**: Single command `kiiaud --service install/start/stop/status/uninstall`. On Windows, registers an auto-starting user logon task via Task Scheduler (`schtasks.exe`), providing full access to audio DACs and ASIO drivers (which ordinary Windows Service Session 0 lacks). On Linux, registers a systemd user unit (`systemctl --user`).
  - **Windows System Tray & Background Mode**: Headless console execution (`--background`) with a notification area tray icon (`--tray`), real-time DAC status tooltip, console show/hide toggle, and one-click WebUI launcher.
  - **Single-Instance Guard**: Guarantees only one instance accesses the DAC at a time using Named Windows Mutex (`Local\kiiaud_single_instance_{name}`) and Unix file locks (`flock`).
- **First-class DSD support**: Native DSD (Direct Stream Digital) and DoP (DSD over PCM). Never converts DSD to PCM silently.
- **Audio format coverage**:
  - **WavPack (`.wv`)**: First-class priority for both **WavPack DSD** (lossless compression of DSF/DFF) and WavPack PCM.
  - **PCM**: FLAC, WAV, AIFF, WavPack.
  - **DSD**: DSF, DFF (uncompressed), WavPack DSD. *(DST decompression is explicitly deferred)*.
- **Multiple Source Types**: Local file paths (`file://` or `/music/...`) and network streams (`http://`, `https://` direct play streams from Jellyfin, LMS, or web radio).
- **Multi-Adapter Support**:
  - **WebUI & REST/SSE (`kiiaud-adapter-web`)**: Embedded Hi-Fi dashboard on port 7110 for real-time signal path inspection, live adapter hot-reloading, and playback controls.
  - **Jellyfin**: "Play To" client (WebSocket remote control + REST progress reports).
  - **LMS (Lyrion Music Server & Lyrion Anywhere Gateway)**: SlimProto client supporting both direct LAN (`DirectTcp`) and remote HTTPS/WSS gateway tunneling (`AnywhereWs`, Protocol v2) with interactive WebUI pairing/login, DSD-first capability negotiation, and bit-perfect `original` stream passthrough.
  - **MPD**: Lightweight control protocol for standard MPD clients without needing an MPD database.
  - **CLI & Local IPC**: Cross-platform control (`kiiaudctl`) via Unix domain sockets on Linux and local loopback TCP on Windows.
- **Native Android Companion App (`android/`)**: Lightweight Kotlin application managing USB permissions, USB DAC hot-plugging, wake locks, and foreground audio service.
- **Database-free & Minimal Storage I/O**: Zero database transactions during playback; protects SD cards on embedded hardware.
- **In-memory Runtime Queue**: Queue and player state reside purely in memory.
- **Gapless Playback**: Pre-buffering track transitions to eliminate gaps between contiguous tracks.

## 5. Acknowledgements & Reference Projects

`kiiaud` stands on the shoulders of giants in the open-source audio ecosystem. Sincere gratitude is extended to the authors and communities behind the following pioneering projects:

- **[rmpd](https://github.com/M0Rf30/rmpd)** & **[Symphonia fork](https://github.com/M0Rf30/Symphonia)** (Gianluca Boiano / M0Rf30, Philip Deljanov):
  - *Dual-licensed under MIT and Apache-2.0.*
  - `rmpd-player` provides a stellar pure-Rust reference implementation for bit-perfect ALSA output, native DSD (`DSD_U32_BE/LE`), and DoP framing. `kiiaud` adapts the audio output pipeline and buffering logic while purposefully omitting database and indexing components.
  - The Symphonia audio decoding fork delivers outstanding support for WavPack PCM, WavPack DSD, and DSF/DFF bitstreams.
- **[squeezelite](https://github.com/ralph-irving/squeezelite)** (Ralph Irving, Adrian Smith, Triode) & **[Lyrion Anywhere](https://github.com/JameZUK/lyrion-anywhere)** (JameZUK):
  - `squeezelite` (*GPL-3.0*) is the undisputed gold standard for Logitech Media Server / Lyrion Music Server SlimProto client playback, buffer timing, and DSD playback control. `kiiaud` implements a clean-room Rust adapter (`kiiaud-adapter-lms`) adhering to SlimProto specifications to act as a lightweight, zero-dependency endpoint.
  - `lyrion-anywhere` defines the remote WebSocket SlimProto tunnel (`WS /v1/player`, Protocol v2), token/pairing authentication, and byte-acknowledged HTTP streaming (`GET /v1/stream/<session>/<seq>`), enabling `kiiaud` to operate seamlessly as a remote bit-perfect Lyrion Anywhere client over the Internet.
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
- [Android Direct USB Audio (Pure Rust UAC1 + UAC2 Bit-Perfect)](docs/android-usb-audio.md)
- [LMS / SlimProto Troubleshooting & Sequence Analysis](docs/TROUBLESHOOTING-LMS.md)
- [MPD User Guide & Playlist Operations](docs/USER-GUIDE-MPD.md)

## 8. Build, Service & Quick Usage

### Pre-built Binaries (GitHub Releases)
Automated GitHub Actions CI/CD publishes multi-arch packages for every release:
- **Linux x86_64**: `kiiaud-<version>-linux-x86_64.tar.gz`
- **Linux ARM64**: `kiiaud-<version>-linux-arm64.tar.gz` (Raspberry Pi 4/5, Armbian, Radxa)
- **Windows x86_64**: `kiiaud-<version>-windows-x86_64.zip` (MSVC build with ASIO, WASAPI & System Tray)
- **Android ARM64**: `kiiaud-<version>-android-arm64.tar.gz` (`aarch64-linux-android`) & Android companion APK

### Local Build
```bash
# Standard host build:
cargo build --release

# Android UAC cdylib:
cargo build --target aarch64-linux-android --release -p kiiaud-output-android
```

### Built-in Background Service Management
`kiiaud` includes native service lifecycle commands for both Linux and Windows:

```bash
# Install auto-starting background service:
# - On Windows: Registers a user logon task via Task Scheduler (guarantees ASIO & WASAPI DAC access)
# - On Linux: Registers a systemd user unit (~/.config/systemd/user/kiiaud.service)
kiiaud --service install

# Check status:
kiiaud --service status

# Start or stop service:
kiiaud --service start
kiiaud --service stop

# Uninstall service:
kiiaud --service uninstall
```

### Windows Background Mode & System Tray
```bash
# Run headless in background with system tray notification icon:
kiiaud --background --tray

# Or disable system tray if running purely headless:
kiiaud --background --tray false
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

# Run on Android with Pure Rust Direct USB UAC2 driver (bypassing AudioFlinger):
kiiaud -b android-usb -d fd:42

# Connect remotely to a Lyrion Anywhere Gateway with a one-time pairing code (Bit-Perfect Passthrough):
kiiaud --lms --lms-gateway "https://music.example.com" --lms-pair-code "ABCDEFGHJK" --lms-quality original
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
