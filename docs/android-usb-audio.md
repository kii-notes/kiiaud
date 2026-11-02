# Android Direct USB Audio (Pure Rust UAC2 Bit-Perfect)

## 1. Overview

Android's standard audio pipeline resamples audio to a fixed rate (typically 48 kHz or Samsung UHQA 384 kHz) and passes it through `AudioFlinger`, float software mixers, and kernel ALSA drivers (`snd-usb-audio`).

`kiiaud` provides a dedicated **Pure Rust Direct USB Audio Backend** (`AndroidUsbAudioOutput`) that completely bypasses:
- **AudioFlinger** and Android float mixers
- **AudioTrack**, **AAudio**, and **Oboe**
- **Kernel ALSA `snd-usb-audio` driver**

The engine writes PCM and DSD frames directly to the external USB DAC over Linux `usbdevfs` isochronous transfers, with full auto-detection and support for both:
- **USB Audio Class 2.0 (UAC2)**: USB 2.0 High Speed (125µs microframes), up to 32-bit / 768 kHz, Native DSD, and DoP.
- **USB Audio Class 1.0 (UAC1)**: USB 1.1 Full Speed (1ms frames), up to 24-bit / 96 kHz, with endpoint sampling frequency control. Compatible with vintage DACs and Android 5.0+ devices.


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

> **Compatibility**: Supports all Android versions from **Android 5.0 (API 21)** up to **Android 15/16**, making it ideal for repurposing older Android TV Boxes and vintage audiophile DAPs.



