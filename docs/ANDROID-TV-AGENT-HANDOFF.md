# Engineering Handoff: Android & Android TV / Google TV Support for `kiiaud`

> **Target Reader:** AI Coding Agent (Antigravity IDE on AlmaLinux) maintaining the `kiiaud` repository (`https://github.com/kii-notes/kiiaud`).
> **Context:** Field testing results of `kiiaud-v0.1.13-android-armv7.tar.gz` on a non-rooted **Sony Bravia VU3 4K Google TV** (`BRAVIA_VU3`, Android 14 / SDK 34, SoC Realtek `rtd2885m`, 32-bit OS ABI `armeabi-v7a` / `armv7l`) connected to an external USB DAC (**C-Media Electronics Inc. Digital Hifi Audio**, VID:PID `0d8c:0171`, PCM 16/24-bit up to 192 kHz, no DSD).

---

## 1. Field Test Summary (`v0.1.13` on Sony Bravia VU3 Google TV)

### 1.1. What Worked Out of the Box
- Cross-compiled `armv7-linux-androideabi` binary (`kiiaud`) executed cleanly from `/data/local/tmp/kiiaud/kiiaud` (`chmod 755`).
- Embedded WebUI (`[adapter.web]` on `0.0.0.0:7110`) and MPD (`[adapter.mpd]` on `0.0.0.0:6600`) bound and served HTTP/TCP requests properly.
- LMS SlimProto LAN adapter (`[adapter.lms]`) connected immediately to Lyrion Music Server (`192.168.50.83:3483`), registered player `kiiaud KII-Private (LAN)` (`02:4b:49:49:41:57`, `max_sample_rate=192000`, `dsd_supported=false`), received `strm-s` commands, pre-buffered HTTP FLAC streams, and decoded `PCM 44100Hz 16-bit (S16_LE) 2ch`.

### 1.2. Five Concrete Issues Discovered During Testing

1. **Issue #1 — `rustls-platform-verifier` Panic on `https://` / `wss://` (`[adapter.lag]` & `[adapter.jellyfin]`):**
   - When connecting to `https://lms.kii.ipv64.net` (LAG) and `https://jellyfin.hexagram.ipv64.net` (Jellyfin), the Tokio worker threads panicked with:
     ```text
     thread 'tokio-rt-worker' panicked at .../rustls-platform-verifier-0.7.1/src/android.rs:90:10:
     Expect rustls-platform-verifier to be initialized
     ```
   - **Root Cause:** On `target_os = "android"`, `reqwest` / `tokio-tungstenite` with `rustls-tls-native-roots` uses `rustls-platform-verifier`, which panics unless `rustls_platform_verifier::android::init_hosted(...)` was initialized with a live JVM `JNIEnv` and Android `Context`. In standalone CLI mode (or if JNI did not call `init_hosted`), any TLS connection crashes the worker task.

2. **Issue #2 — `backend = "android"` (`Oboe/AAudio`) Is Currently an Unthrottled Stub Sink:**
   - Inspecting `readelf -d kiiaud` shows the binary only links `libc.so`, `libm.so`, and `libdl.so` (it does **not** link `libaaudio.so` or `libOpenSLES.so`).
   - In `crates/kiiaud-output-android/src/lib.rs`, `AndroidOutput` logs `opening Android audio output (Oboe/AAudio)` and then acts as a no-op sink whose `write()` returns `Ok(data.len())` immediately without writing to Android audio hardware or blocking on the sample clock. Consequently, `kiiaud` consumes an entire FLAC track in ~2 seconds with zero sound output and skips through the playlist.

3. **Issue #3 — `backend = "android-usb"` Cannot Open `/dev/bus/usb/*` Without an Android APK:**
   - `crates/kiiaud-output-android/src/usb_output.rs` implements real USB Audio Class 1.0/2.0 `USBDEVFS` isochronous transfers, which requires an open USB device file descriptor (`fd:<N>`).
   - On non-rooted Android / Google TV, `/dev/bus/usb/001/003` is owned by `root:usb` (`crw-rw----`), so neither `adb shell` (`uid=2000`) nor an unprivileged binary can open the path directly. Only an installed Android APK calling `UsbManager.requestPermission()` -> `UsbDeviceConnection.getFileDescriptor()` can obtain the `fd` and pass it via JNI (`kiiaud_output_android::jni`).

4. **Issue #4 — No Universal Android Companion `.apk` Artifact & Missing Google TV (`LEANBACK_LAUNCHER`) Support:**
   - Release `v0.1.13` only published `.tar.gz` CLI binaries (`android-armv7`, `android-arm64`), not an installable `.apk`.
   - Furthermore, for an Android app to appear on the **Google TV / Android TV home screen** (`com.google.android.apps.tv.launcherx`), the APK's `AndroidManifest.xml` **must** include `<category android:name="android.intent.category.LEANBACK_LAUNCHER" />` and `android:banner` (in addition to `android.intent.category.LAUNCHER` for phones/tablets).

5. **Issue #5 — Android `$HOME` Defaults to `/` (Read-Only) and SELinux Blocks `sock_file` in `/data/local/tmp`:**
   - In `adb shell`, `$HOME` is `/` (read-only rootfs), causing `Failed to create config directory '/.config/kiiaud': Read-only file system (os error 30)` unless `HOME` or `-c` is set.
   - Android 14 SELinux (`u:r:shell:s0` and `untrusted_app`) blocks creating filesystem Unix domain socket files in arbitrary directories (`Failed to bind IPC socket '/data/local/tmp/kiiaud/kiiaud.sock': Permission denied (os error 13)`).

---

## 2. Actionable Implementation Tasks for the AI Agent on AlmaLinux

Please implement the following 5 tasks in the `kiiaud` repository so that **a single codebase & a single Universal Android APK** works seamlessly on **both Android phones/tablets AND Android TV / Google TV** (plus standalone CLI mode):

### Task 1: Fix TLS Panic on Android (`webpki-roots` Instead of `rustls-platform-verifier`)
- Across all crates using `reqwest` and `tokio-tungstenite` (`kiiaud-decoder`, `kiiaud-adapter-jellyfin`, `kiiaud-adapter-lag`, etc.):
  - Replace `rustls-tls-native-roots` with **`rustls-tls-webpki-roots`** (or explicitly configure `rustls::ClientConfig` with `webpki_roots::TLS_SERVER_ROOTS`), **at least on `target_os = "android"`** (or globally across targets).
  - **Why:** Using `webpki-roots` embeds Mozilla's CA bundle directly in the Rust binary, completely eliminating the `Expect rustls-platform-verifier to be initialized` panic in both standalone CLI mode and JNI mode without needing Java CA certificate bridging.

### Task 2: Implement Real AAudio / Oboe Playback in `crates/kiiaud-output-android` (`backend = "android"`)
- Currently `crates/kiiaud-output-android/src/lib.rs` (`AndroidOutput`) is a dummy stub.
- Wire `AndroidOutput` (`backend = "android"` / `"oboe"`) to real Android native audio output using either:
  - `ndk::audio::AudioStreamBuilder` (binding to Android's native `libaaudio.so`, available on Android 8.0+ / API 26+), OR
  - `oboe` / `cpal` on Android (`AAudio` with OpenSL ES fallback).
- Configure the stream with:
  - Direction: Output
  - Performance mode: `PerformanceMode::LowLatency` (or `None` when large buffers are requested)
  - Sharing mode: Attempt `SharingMode::Exclusive` when `bit_perfect = true`, automatically falling back to `SharingMode::Shared` if the device HAL rejects exclusive mode.
  - Format: `I16` (`PCM_I16`) for 16-bit PCM, `I24_PACKED` / `I32` / `Float` for 24/32-bit PCM (with automatic fallback to `Float` or `I16` if the Android API level/HAL requires it), matching the stream `sample_rate` and `channels`.
- Ensure `write(&mut self, data: &[u8])` performs a **blocking write** (`stream.write(...)`) paced by the hardware audio clock so playback runs at real-time speed and outputs audible audio through Android's audio routing (which automatically routes to the connected USB DAC on Android TV).

### Task 3: Build a Single Universal Android Companion APK Supporting Both Mobile & Google TV
Do **NOT** split the codebase into separate Mobile and TV apps. Build **one single Universal APK** (`kiiaud-vX.Y.Z-android-universal.apk`) that bundles both **`armeabi-v7a`** (critical for 32-bit Smart TVs like Sony Bravia VU3 and TV boxes) and **`arm64-v8a`** (for modern phones/tablets):

1. **`AndroidManifest.xml` Requirements (Dual Mobile + Android TV / Google TV):**
   ```xml
   <!-- Optional hardware features so the same APK installs on both Phones and TVs -->
   <uses-feature android:name="android.software.leanback" android:required="false" />
   <uses-feature android:name="android.hardware.touchscreen" android:required="false" />
   <uses-feature android:name="android.hardware.usb.host" android:required="false" />

   <uses-permission android:name="android.permission.INTERNET" />
   <uses-permission android:name="android.permission.FOREGROUND_SERVICE" />
   <uses-permission android:name="android.permission.FOREGROUND_SERVICE_MEDIA_PLAYBACK" />
   <uses-permission android:name="android.permission.WAKE_LOCK" />
   <uses-permission android:name="android.permission.RECEIVE_BOOT_COMPLETED" />

   <application
       android:label="kiiaud"
       android:icon="@mipmap/ic_launcher"
       android:banner="@drawable/tv_banner"
       android:usesCleartextTraffic="true">

       <activity
           android:name=".MainActivity"
           android:banner="@drawable/tv_banner"
           android:exported="true"
           android:configChanges="keyboard|keyboardHidden|navigation|orientation|screenSize">
           <intent-filter>
               <action android:name="android.intent.action.MAIN" />
               <!-- Phone / Tablet Launcher -->
               <category android:name="android.intent.category.LAUNCHER" />
               <!-- Android TV / Google TV Home Screen Launcher -->
               <category android:name="android.intent.category.LEANBACK_LAUNCHER" />
           </intent-filter>
           <intent-filter>
               <action android:name="android.hardware.usb.action.USB_DEVICE_ATTACHED" />
           </intent-filter>
           <meta-data
               android:name="android.hardware.usb.action.USB_DEVICE_ATTACHED"
               android:resource="@xml/usb_device_filter" />
       </activity>

       <service
           android:name=".KiiaudForegroundService"
           android:foregroundServiceType="mediaPlayback"
           android:exported="false" />
   </application>
   ```
   - Provide a `320x180` px drawable resource `@drawable/tv_banner` so Google TV renders a clean horizontal card on the home screen.

2. **USB DAC Permission & Hand-off to `kiiaud` (`android-usb` vs `android`):**
   - In the APK's `ForegroundService` / `MainActivity`, enumerate connected USB devices (`UsbManager.getDeviceList()`) and detect any device having an interface with `UsbConstants.USB_CLASS_AUDIO` (`class == 1`).
   - Request permission via `UsbManager.requestPermission()`, open the device (`UsbManager.openDevice(device)`), extract `conn.getFileDescriptor()` (`fd`), and pass `fd:{fd}` to the Rust engine (`backend = "android-usb"`).
   - If the user prefers shared Android audio (so TV remote volume or other TV apps can share the DAC) or if no USB DAC is attached, allow selecting `backend = "android"` (AAudio/Oboe).

3. **TV Remote (D-Pad) Friendly UI + Config Management:**
   - Ensure all UI controls in `MainActivity` (Start/Stop daemon, Audio Backend selector `Android AAudio` vs `Direct USB Bit-Perfect`, Import/Edit `kiiaud.toml`, Open WebUI `:7110`) are navigable via Android TV D-Pad (`DPAD_UP/DOWN/LEFT/RIGHT/CENTER` — focusable buttons with visible focus highlight).
   - Store `config.toml` inside the app's internal files directory (`context.getFilesDir() + "/config.toml"`) and allow importing a `kiiaud.toml` file from `/sdcard/Download/kiiaud.toml` or editing key fields (Device Name, LMS IP, LAG URL/User/Pass, Jellyfin URL/User/Pass, Max Sample Rate, DSD Support) directly via the embedded WebUI at `http://127.0.0.1:7110`.

### Task 4: Resilient IPC Socket & Config Path Handling on Android
- On `target_os = "android"`:
  - If `HOME` is unset or `/` (read-only), fall back to `/data/local/tmp` (for CLI) or the app's `filesDir` (for APK) when resolving the default config path.
  - When binding the Unix domain IPC socket (`socket_path`), either use an **Linux Abstract Namespace Unix Socket** (`\0kiiaud.sock`, which does not create a filesystem `sock_file` and works under Android SELinux!) or gracefully disable the local CLI IPC listener with a `WARN` log if `bind()` returns `EACCES` / `EPERM`, without logging an `ERROR`.

### Task 5: CI/CD Release Artifacts
- Update the GitHub Actions release workflow (`.github/workflows/release.yml`) so each tagged release publishes:
  1. `kiiaud-vX.Y.Z-android-universal.apk` (signed universal APK containing both `armeabi-v7a` and `arm64-v8a` JNI libraries, ready to install via `adb install` or sideloading on both phones and Google TV).
  2. Existing CLI tarballs (`kiiaud-vX.Y.Z-android-armv7.tar.gz`, `kiiaud-vX.Y.Z-android-arm64.tar.gz`, etc.) with `webpki-roots` and real `libaaudio.so` support.
