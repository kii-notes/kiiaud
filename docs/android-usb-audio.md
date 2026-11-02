# Android Direct USB Audio (Pure Rust UAC2 Bit-Perfect)

## 1. Overview

Android's standard audio pipeline resamples audio to a fixed rate (typically 48 kHz or Samsung UHQA 384 kHz) and passes it through `AudioFlinger`, float software mixers, and kernel ALSA drivers (`snd-usb-audio`).

`kiiaud` provides a dedicated **Pure Rust Direct USB Audio Class 2.0 (UAC2) Backend** (`AndroidUsbAudioOutput`) that completely bypasses:
- **AudioFlinger** and Android float mixers
- **AudioTrack**, **AAudio**, and **Oboe**
- **Kernel ALSA `snd-usb-audio` driver**

The engine writes PCM and DSD frames directly to the external USB DAC over Linux `usbdevfs` isochronous transfers.

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

## 5. Android Kotlin Bridge Example

```kotlin
class UsbAudioBridge(private val context: Context) {
    private val usbManager = context.getSystemService(Context.USB_SERVICE) as UsbManager

    fun connectDac(device: UsbDevice, onFdReady: (Int) -> Unit) {
        val connection = usbManager.openDevice(device) ?: return

        // 1. Claim AudioControl & AudioStreaming interfaces
        for (i in 0 until device.interfaceCount) {
            val iface = device.getInterface(i)
            connection.claimInterface(iface, true)
        }

        // 2. Set active Alt Setting (e.g., Alt 2 or 3 for 24/32-bit PCM)
        // This triggers kernel ISO bandwidth allocation in xHCI host controller!
        connection.setInterface(streamingIface, bestAltSetting)

        // 3. Hand fileDescriptor to kiiaud backend
        val fd = connection.fileDescriptor
        onFdReady(fd)
    }
}
```

