# Android Audio, Direct USB (UAC1/UAC2) & Android TV / Google TV

## 1. Overview

Android's standard audio pipeline (`AudioFlinger`) typically resamples and quantizes audio to a fixed HAL rate/format (often `16-bit / 48 kHz` on many Android TV and mobile devices, even when an application submits `32-bit Float / 192 kHz`).

`kiiaud` supports **two** Android output backends plus a **Universal Android Companion APK** (for phones, tablets, and Android TV / Google TV) and standalone CLI execution:

1. **Pure Rust Direct USB Audio (`android-usb` / `usb`)** — completely bypasses `AudioFlinger`, `AudioTrack`, `AAudio`, `Oboe`, and the kernel `snd-usb-audio` driver by writing PCM and DSD frames directly to the external USB DAC over Linux `usbdevfs` isochronous transfers:
   - **USB Audio Class 2.0 (UAC2)**: USB 2.0 High Speed (125µs microframes), up to 32-bit / 768 kHz, Native DSD, and DoP.
   - **USB Audio Class 1.0 (UAC1)**: USB 1.1 Full Speed (1ms frames), up to 24-bit / 96 kHz, with endpoint sampling frequency control. Compatible with vintage DACs and Android 5.0+ devices.
2. **Native AAudio (`android` / `oboe`)** — dynamically binds `libaaudio.so` (Android 8.0+ / API 26+) in `crates/kiiaud-output-android/src/aaudio.rs`. Requests `SharingMode::Exclusive` when `bit_perfect = true` (falling back to `SharingMode::Shared` if the HAL rejects exclusive mode), supports `PCM_I16`, `PCM_I24_PACKED`, `PCM_I32`, and `PCM_FLOAT`, and performs hardware-clock-paced blocking writes. Useful when no external USB DAC is attached or when sharing audio with other Android/TV apps (e.g., TV remote volume control).


---

## 2. Architecture & Data Flow

```text
 ┌─────────────────────────────────────────────────────────────┐
 │ Android Application / Service (Kotlin / Java)               │
 │                                                             │
 │  1. UsbManager.requestPermission(usbDevice)                 │
 │  2. connection = UsbManager.openDevice(usbDevice)           │
 │  3. connection.claimInterface(controlIface, force = true)   │
 │  4. connection.claimInterface(streamingIface, force = true) │
 │  5. connection.setInterface(streamingIface, altSetting)     │
 │     *(Allocates ISO bandwidth in xHCI host controller)*     │
 │  6. fd = connection.fileDescriptor                         │
 └──────────────────────────────┬──────────────────────────────┘
                                │ Passes "fd:<number>" or socket
                                ▼
 ┌─────────────────────────────────────────────────────────────┐
 │ kiiaud Daemon (Pure Rust Engine)                            │
 │                                                             │
 │  ┌───────────────────────────────────────────────────────┐  │
 │  │ ParsedUsbAudioDevice                                  │  │
 │  │  - Auto-parses CLOCK_SOURCE entity ID                 │  │
 │  │  - Auto-selects 32-bit / 24-bit PCM & DSD alts        │  │
 │  │  - Extracts Isochronous OUT & Feedback IN endpoints   │  │
 │  └───────────────────────────┬───────────────────────────┘  │
 │                              ▼                              │
 │  ┌───────────────────────────────────────────────────────┐  │
 │  │ UAC2 Control & Rate Switching (SET_CUR)               │  │
 │  │  - Sets hardware PLL frequency on verified Clock ID   │  │
 │  └───────────────────────────┬───────────────────────────┘  │
 │                              ▼                              │
 │  ┌───────────────────────────────────────────────────────┐  │
 │  │ UsbAudioPipeline (80 URBs Ring Buffer)                │  │
 │  │  - Zero-alloc in streaming loop (MTE-safe)            │  │
 │  │  - USBDEVFS_URB_ISO_ASAP microframe scheduling (125µs)│  │
 │  │  - Continuous Async Feedback (Q16.16 clock tracking)  │  │
 │  │  - Bit-exact integer packing (S16, S24_3, S24_in_32)  │  │
 │  │  - DSD DoP (0x05/0xFA markers) & Native 1-bit DSD     │  │
 │  └───────────────────────────┬───────────────────────────┘  │
 └──────────────────────────────┼──────────────────────────────┘
                                │ ioctl(USBDEVFS_SUBMITURB / REAPURB)
                                ▼
 ┌─────────────────────────────────────────────────────────────┐
 │ Linux Kernel usbdevfs -> xHCI Controller -> External USB DAC│
 └─────────────────────────────────────────────────────────────┘
```

---

## 3. The Five Critical Issues Resolved

Based on exhaustive hardware analysis and lessons from the `decent-player` project:

1. **Clock Source Entity Detection (Bug 1 Fix)**:
   Many DACs (like Cayin RU7, XMOS, Amanero) accept `SET_CUR` transfers on nonexistent entity IDs without returning STALL. `kiiaud` inspects the raw USB descriptors at runtime to extract the actual `CLOCK_SOURCE` ID (`bDescriptorSubtype = 0x0A`), guaranteeing the DAC PLL locks to the exact sample rate.
2. **`USBDEVFS_URB_ISO_ASAP` Flag (Bug 2 Fix)**:
   All Isochronous URBs set flag `0x02`. Without this, kernel zeroed start-frames are scheduled in the past and silently dropped by the xHCI host controller.
3. **ISO Bandwidth Allocation (Bug 3 Fix)**:
   On Android, native `ioctl(USBDEVFS_SETINTERFACE)` activates the alt setting but may not allocate isochronous bandwidth in the xHCI host controller scheduler (`#Iso=0`). Calling Android Java `UsbDeviceConnection.setInterface()` completes the required kernel configuration.
4. **80-URB Pipeline (Bug 4 Fix)**:
   A synchronous single-URB submit-reap loop introduces micro-gaps between microframes, producing silence. `kiiaud` pre-primes 80 silence URBs and maintains continuous FIFO streaming (~80ms pipeline), with URB reaping providing hardware DAC clock backpressure.
5. **Continuous Async Feedback Endpoint**:
   Isochronous feedback (Q16.16) is continuously collected in a dedicated URB to update `calibrated_fpmf` (frames per microframe) in real time, preventing buffer underflow/overflow pops from clock drift.

---

## 4. Configuration

In `kiiaud.toml`:

```toml
[output]
backend = "android-usb"       # Or "usb"
device = "fd:42"              # When started by Android Activity / Service
bit_perfect = true
dsd_mode = "dop"              # "dop" (broadest compatibility) or "native"
buffer_size_frames = 4096
```

From CLI:

```bash
kiiaud -b android-usb -d fd:42
# Or for rooted / Termux direct device node:
kiiaud -b android-usb -d /dev/bus/usb/001/002
```

---

## 5. Lightweight Native Android App (`android/`)

The repository includes a dedicated ultra-lightweight Native Android application located in `/android`:

### Key Components:
- **`KiiaudService.kt`**: Foreground Service managing high-performance WakeLock, WifiLock, and permanent notification with Bit-Perfect indicators. Listens for USB hot-plug events (`USB_DEVICE_ATTACHED` / `USB_DEVICE_DETACHED`).
- **`MainActivity.kt`**: Single-page minimal control UI:
  - Displays engine status (`● RUNNING`) and device local IP (`http://192.168.x.x:7110`).
  - **USB DAC Scanner & Picker**: Scans all connected DACs, requests USB permissions, displays DAC capabilities, and switches the active DAC with one tap.
  - Quick action buttons to open Web Dashboard or start/stop the service.
- **`KiiaudNative.kt`**: Zero-overhead JNI bridge connecting Kotlin directly to `libkiiaud_output_android.so`.
- **`BootReceiver.kt`**: Automatically launches `KiiaudService` upon device boot (ideal for dedicated Android TV Boxes / headless audio appliances).

### Building the Android APK:
```bash
# 1. Build Rust cdylib for Android targets (e.g. aarch64)
cargo build --target aarch64-linux-android --release -p kiiaud-output-android
mkdir -p android/app/src/main/jniLibs/arm64-v8a
cp target/aarch64-linux-android/release/libkiiaud_output_android.so android/app/src/main/jniLibs/arm64-v8a/

# 2. Build APK via Gradle
cd android
./gradlew assembleRelease
```

> **Compatibility**: Supports all Android versions from **Android 5.0 (API 21)** up to **Android 15/16** (`android-usb`), and **Android 8.0+ (API 26)** for native `AAudio` (`android` / `oboe`), making it ideal for phones, tablets, Android TV / Google TV, and vintage audiophile DAPs.

---

## 6. Android TV / Google TV & Universal APK

### 6.1. 32-bit (`armeabi-v7a`) vs. 64-bit (`arm64-v8a`) ABI
Many Smart TVs and Google TV devices (even with 64-bit ARM Cortex-A55 SoCs) run Android TV OS in **32-bit userland mode (`armeabi-v7a` / `armv7l`)** to conserve RAM. Installing an `arm64-v8a`-only APK fails with `INSTALL_FAILED_NO_MATCHING_ABIS`.
- The Universal APK (`kiiaud-vX.Y.Z-android-universal.apk`) bundles **both** `armeabi-v7a` and `arm64-v8a` JNI libraries so a single package installs on both Smart TVs and modern phones/tablets.

### 6.2. Google TV Launcher & D-Pad Navigation
- `AndroidManifest.xml` declares both `android.intent.category.LAUNCHER` (mobile) and `android.intent.category.LEANBACK_LAUNCHER` (Android TV / Google TV home screen) alongside `@drawable/tv_banner` (`320x180`), with `android.software.leanback` and `android.hardware.touchscreen` marked `required="false"`.
- `MainActivity` provides D-Pad focusable controls with visible focus highlights (`tv_button_bg.xml`) for TV remotes (`DPAD_UP/DOWN/LEFT/RIGHT/CENTER`).

### 6.3. AudioFlinger vs. Direct USB (`android-usb`) on Android TV
Inspection via `dumpsys media.audio_flinger` and `dumpsys media.audio_policy` on Android TV HALs shows that system MixPorts (`primary`, `deep_buffer`, `direct`) are frequently locked to `AUDIO_FORMAT_PCM_16_BIT` at `44100` / `48000 Hz`. Even if a player pushes `PCM_FLOAT @ 192 kHz` or `PCM_16 @ 96 kHz`, `AudioFlinger` quantizes and downsamples it to `16-bit / 48 kHz` before reaching the USB DAC.
- Using `backend = "android-usb"` via the Companion APK (`UsbManager.requestPermission()` -> `UsbDeviceConnection.setInterface()` -> `getFileDescriptor()`) bypasses `AudioFlinger` completely for true bit-perfect PCM and DSD output.
- When inspecting UAC1/UAC2 USB descriptors (`dumpsys usb`), DAC `alternate_settings` reveal exact supported sample rates and bit depths via `max_packet_size` (for example, `24-bit` packets are `1.5x` the size of `16-bit` packets at the same rate, and SPDIF-clocked DACs often expose `44.1 / 48 / 96 / 192 kHz` integer PCM modes without `88.2 / 176.4 kHz` or float).

---

## 7. Standalone CLI Mode & TLS on Android

- **Executable Storage & `$HOME` Fallback:** On non-rooted Android (`uid=2000` via `adb shell`), `/` is read-only (`$HOME` defaults to `/`) and `/sdcard` is mounted `noexec`. Place standalone CLI binaries (`kiiaud`, `kiiaudctl`) in `/data/local/tmp/kiiaud/` (`chmod 755`). When `$HOME` is unset or `/`, `kiiaud` falls back safely instead of failing on `/.config/kiiaud`.
- **SELinux & Abstract Namespace Unix Socket:** Android 14+ SELinux (`u:r:shell:s0`, `untrusted_app`) blocks `bind()` for filesystem Unix socket files (`sock_file`) in `/data/local/tmp`. On `target_os = "android"`, `kiiaud` defaults `socket_path` to `@kiiaud.sock` (Linux Abstract Namespace Unix Socket `\0kiiaud.sock`), which requires no filesystem node; if a filesystem socket bind fails with `EACCES`/`EPERM`, it degrades gracefully without aborting.
- **Embedded TLS Root CAs (`webpki-roots`):** On Android, `rustls-platform-verifier` panics if `init_hosted(JNIEnv, Context)` has not been called (e.g., standalone CLI mode). `kiiaud` uses `rustls-tls-webpki-roots` (`crates/kiiaud-core/src/tls.rs`) across HTTP/WebSocket clients (`kiiaud-decoder`, `kiiaud-adapter-jellyfin`, `kiiaud-adapter-lms` / `lag`) so `https://` and `wss://` streams work in both CLI and APK modes without JVM CA bridging.



