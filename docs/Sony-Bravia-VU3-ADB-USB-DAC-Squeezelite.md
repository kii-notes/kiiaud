---
aliases:
  - Sony Bravia VU3 ADB & USB DAC
  - Cấu hình Squeezelite & USB DAC trên Sony Bravia VU3
tags:
  - note/kỹ-thuật
  - note/audio
  - note/android-tv
  - device/sony-bravia-vu3
created: 2026-11-15
updated: 2026-11-15
status: active
---

# Sony BRAVIA VU3 — Quản Trị ADB, Giải Mã USB DAC & Patch Squeezelite Google TV

Tài liệu này ghi nhận toàn bộ thông số phần cứng, kiến trúc âm thanh USB (ALSA / AudioFlinger), giải mã bảng descriptor USB Audio Class (UAC) của DAC C-Media và quy trình nâng cấp + vá `AndroidManifest.xml` (`LEANBACK_LAUNCHER`) cho ứng dụng **Squeezelite** trên Smart TV **Sony BRAVIA VU3**.

---

## 1. Thông Số Phần Cứng & Hệ Thống TV

- **Tên thiết bị / Model:** `Sony BRAVIA VU3` (`BRAVIA_VU3_4K` / `BRAVIA_VU3`)
- **Địa chỉ IP nội bộ (LAN):** `192.168.10.136` (Cổng ADB TCP/IP: `5555`)
- **Hệ điều hành:** Android TV **14** (SDK / API Level **34**) — Mã Build: `UKR1.240726.001`
- **Vi xử lý (SoC):** **Realtek `rtd2885m`** (4 nhân ARM Cortex-A55, `CPU part: 0xd05`)
- **Kiến trúc thực thi (ABI):** **32-bit mode (`armeabi-v7a`, `armeabi` / Kernel `armv7l`)**
  - *Lưu ý quan trọng khi cài APK (Sideload):* Hệ điều hành chạy ở chế độ 32-bit để tối ưu RAM, do đó **không cài được** các gói APK chỉ chứa thư viện `arm64-v8a` thuần (sẽ báo lỗi `INSTALL_FAILED_NO_MATCHING_ABIS`). Luôn chọn bản **`armeabi-v7a`** hoặc **Universal APK**.
- **Bộ nhớ RAM:** `2.0 GB` + `900 MB` ZRAM Swap
- **Bộ nhớ lưu trữ (`/data`):** `8.7 GB` khả dụng
- **Độ phân giải:** Tấm nền vật lý `3840x2160` (4K) \| Giao diện hệ thống (UI) render ở `1920x1080` (`320 dpi`)

### Các lệnh kết nối ADB cơ bản từ máy trạm Windows

Công cụ `platform-tools` được đặt tại `D:\Apps\System\platform-tools\adb.exe` (và `scrcpy` tại `D:\Apps\System\scrcpy-win64-v5.0.1\scrcpy.exe`):

```powershell
# Kết nối ADB qua mạng LAN
D:\Apps\System\platform-tools\adb.exe connect 192.168.10.136:5555

# Kiểm tra danh sách thiết bị
D:\Apps\System\platform-tools\adb.exe devices -l

# Truy cập shell của TV
D:\Apps\System\platform-tools\adb.exe -s 192.168.10.136:5555 shell

# Mirror và điều khiển màn hình TV từ PC bằng scrcpy
D:\Apps\System\scrcpy-win64-v5.0.1\scrcpy.exe --tcpip=192.168.10.136:5555
```

---

## 2. Cấu Hình Bus USB & Thiết Bị Ngoại Vi (`dumpsys usb`)

Kiểm tra qua lệnh `adb shell dumpsys usb` ghi nhận 3 thiết bị hoạt động trên chế độ USB Host (`host_manager`):

| Bus / Địa chỉ | VID : PID | Nhà sản xuất | Tên thiết bị | ALSA Card | Chức năng chi tiết |
| :--- | :--- | :--- | :--- | :--- | :--- |
| `/dev/bus/usb/003/002` | `0bda:c82c` (`3034:51244`) | Realtek | `802.11ac NIC` | — | Module Wi-Fi 802.11ac + Bluetooth Radio gắn nội bộ trên bo mạch (`max_power=500mA`). |
| `/dev/bus/usb/001/002` | `1915:1025` (`6421:4133`) | 123 COM | `Smart Control` | `card=2, device=0` (`has_input=true`) | Đầu thu USB 2.4GHz cho chuột bay / điều khiển từ xa tích hợp Micro thu giọng nói (`USB-Audio - Smart Control`). |
| `/dev/bus/usb/001/003` | `0d8c:0171` (`3468:369`) | C-Media Electronics Inc. | `Digital Hifi Audio` (v1.01) | `card=3, device=0` (`has_output=true`) | Bộ giải mã âm thanh **USB DAC** (`USB-Audio - Digital Hifi Audio`, `max_power=200mA`). |

---

## 3. Phân Tích Kỹ Thuật USB DAC (`C-Media 0d8c:0171`) & Luồng Âm Thanh

### 3.1. Giải mã bảng `Alternate Settings` của DAC (Endpoint 5 Isochronous)

Trên Interface 1 (`SPDIF` / USB Audio Streaming, Class 1, Subclass 2), DAC khai báo 9 cấu hình `alternate_settings` với `interval = 4` (chu kỳ truyền `1 ms` trên USB 2.0). Dựa vào tỷ lệ băng thông `max_packet_size` giữa **16-bit (2 bytes/mẫu)** và **24-bit (3 bytes/mẫu — gấp $1.5\times$ 16-bit)**, cấu trúc phần cứng của DAC được xác định như sau:

| Alt Setting | `max_packet_size` | Tỷ lệ 24-bit / 16-bit | Tần số lấy mẫu & Độ phân giải tương ứng |
| :---: | :---: | :---: | :--- |
| **Alt 1** | `228 bytes` | Chuẩn gốc (16-bit) | **44.1 kHz / 16-bit** Stereo |
| **Alt 2** | `342 bytes` | $228 \times 1.5 = 342$ | **44.1 kHz / 24-bit** Stereo |
| **Alt 3** | `248 bytes` | Chuẩn gốc (16-bit) | **48.0 kHz / 16-bit** Stereo |
| **Alt 4** | `372 bytes` | $248 \times 1.5 = 372$ | **48.0 kHz / 24-bit** Stereo |
| **Alt 5** | `496 bytes` | $248 \times 2.0 = 496$ | **96.0 kHz / 16-bit** Stereo |
| **Alt 6** | `744 bytes` | $496 \times 1.5 = 744$ | **96.0 kHz / 24-bit** Stereo |
| **Alt 7** | `996 bytes` | $\approx 496 \times 2.0$ | **192.0 kHz / 16-bit** Stereo |
| **Alt 8** | `2792 bytes` | High-bandwidth | **192.0 kHz / 24-bit** (hoặc Đa kênh 5.1/7.1) |
| **Alt 9** | `248 bytes` | Luồng SPDIF/AC3 | **48.0 kHz / 16-bit** (Digital Passthrough) |

- **Đặc điểm xung nhịp (Clock):** Phần cứng C-Media `0d8c:0171` khóa xung nhịp ở 4 mức chuẩn SPDIF là **44.1 / 48 / 96 / 192 kHz** (không có cặp Alt riêng `456 / 684 bytes` cho `88.2 kHz` hay `176.4 kHz`) và chỉ nhận PCM số nguyên (**16-bit / 24-bit Integer**), không nhận trực tiếp `32-bit Float`.

### 3.2. Đường đi tín hiệu qua `AudioFlinger` của Android TV

Kiểm tra thực tế bằng `dumpsys media.audio_flinger` và `dumpsys media.audio_policy`:

1. **Giới hạn của Android Audio HAL (`/vendor/etc/audio_policy_configuration.xml`):**
   - Toàn bộ các MixPort hệ thống (`primary output`, `deep buffer`, `direct`) trên bo mạch Realtek `rtd2885m` được nhà sản xuất cố định ở `AUDIO_FORMAT_PCM_16_BIT (0x1)` với tần số `44100, 48000 Hz`.
2. **Xử lý luồng phát từ các ứng dụng:**
   - **Squeezelite (`org.lyrion.squeezelite`):** Giải mã và đẩy luồng âm thanh vào `AudioOut_D (PRIMARY)` ở định dạng **`AUDIO_FORMAT_PCM_FLOAT` (32-bit Float) @ `192,000 Hz` Stereo**. Bộ trộn `AudioOut_D` xử lý nội bộ (DSP/Volume/Mixing) ở `32-bit Float` để bảo toàn dải động, sau đó lượng tử hóa và hạ tần số mẫu (downsample) về **`PCM 16-bit / 48,000 Hz`** khi xuất qua HAL xuống DAC (`0x4000: card=3;device=0`).
   - **Jellyfin (`org.jellyfin.androidtv`):** Đẩy luồng âm thanh vào `AudioOut_15 (DEEP_BUFFER)` ở **`PCM 16-bit` @ `96,000 Hz` Stereo**, sau đó được đưa về **`PCM 16-bit / 48,000 Hz`** khi xuất ra DAC.
   - *Lưu ý:* Nếu muốn xuất Bit-Perfect (`24-bit / 96–192 kHz`) bỏ qua `AudioFlinger`, cần dùng trình phát có trình điều khiển USB Audio riêng (như *USB Audio Player PRO* hoặc *HibyMusic*).

---

## 4. Nâng Cấp & Vá `AndroidManifest.xml` Cho Squeezelite (`0.10.1`)

### 4.1. Vấn đề gốc

- Bản **Squeezelite** cài sẵn trên TV là `0.8.0` (`versionCode=800`). Bản mới nhất từ nhánh chính thức `CDrummond/squeezelite` là **`0.10.1`** (`versionCode=1001`, bổ sung tùy chọn ép dùng OpenSL ES, sửa lỗi tự tắt tiến trình ngầm và lỗi đồng bộ).
- Tuy nhiên, cả bản `0.8.0` lẫn `0.10.1` gốc chỉ khai báo `<category android:name="android.intent.category.LAUNCHER"/>` (dành cho điện thoại) mà **thiếu `<category android:name="android.intent.category.LEANBACK_LAUNCHER"/>`**, khiến biểu tượng ứng dụng bị ẩn khỏi màn hình chính Google TV (`com.google.android.apps.tv.launcherx`).

### 4.2. Quy trình chỉnh sửa Manifest & đóng gói lại APK

1. Giải nén tài nguyên và `AndroidManifest.xml` từ bản gốc `squeezelite-0.10.1.apk` (giữ nguyên `classes.dex` bằng cờ `-s`):

   ```powershell
   java -jar apktool.jar d -s -f squeezelite-0.10.1.apk -o squeezelite_src
   ```

2. Bổ sung khai báo Leanback và Banner TV vào `squeezelite_src\AndroidManifest.xml`:

   ```xml
   <uses-feature android:name="android.software.leanback" android:required="false"/>
   <uses-feature android:name="android.hardware.touchscreen" android:required="false"/>
   <application ... android:banner="@mipmap/ic_launcher">
       ...
       <activity android:banner="@mipmap/ic_launcher" ... android:name="org.lyrion.squeezelite.MainActivity">
           <intent-filter>
               <action android:name="android.intent.action.MAIN"/>
               <category android:name="android.intent.category.LAUNCHER"/>
               <category android:name="android.intent.category.LEANBACK_LAUNCHER"/>
           </intent-filter>
       </activity>
   ```

3. Đóng gói lại bằng `apktool` và căn chỉnh `zipalign` + ký số (`v1, v2, v3`) bằng `uber-apk-signer`:

   ```powershell
   java -jar apktool.jar b squeezelite_src -o squeezelite-0.10.1-tv.apk
   java -jar uber-apk-signer.jar --apks squeezelite-0.10.1-tv.apk --out signed
   ```

4. Gỡ bản cũ, cài đặt bản `squeezelite-0.10.1-tv-aligned-debugSigned.apk` lên TV và gỡ bỏ ứng dụng Launcher phụ (`com.kfaraj.launcher` v3.1.6) không còn cần thiết:

   ```powershell
   D:\Apps\System\platform-tools\adb.exe -s 192.168.10.136:5555 uninstall org.lyrion.squeezelite
   D:\Apps\System\platform-tools\adb.exe -s 192.168.10.136:5555 install signed\squeezelite-0.10.1-tv-aligned-debugSigned.apk
   D:\Apps\System\platform-tools\adb.exe -s 192.168.10.136:5555 uninstall com.kfaraj.launcher
   ```

### 4.3. Cấu hình kết nối Lyrion Music Server (LMS)

Sau khi cài đặt lại, ứng dụng đã hiển thị trực tiếp trên màn hình chính Google TV và được khôi phục cấu hình:

- **Lyrion Music Server (LMS) Host/IP:** `192.168.50.83` (SlimProto port `3483`, Web/JSON-RPC port `9000`)
- **Tên Player (`Name`):** `KII - Private`
- **Định danh MAC mới của Player trên LMS:** `cd:ef:07:62:65:19` (`modelname: SqueezeLiteAndroid`)
- **Đồng bộ âm lượng (`Volume control`):** Đồng bộ hai chiều giữa Lyrion và thiết bị Android.

---

## 5. Thử nghiệm triển khai `kiiaud` v0.1.13 (Android ARMv7) & Đặc tả kỹ thuật cho AI Agent (AlmaLinux)

### 5.1. Quá trình cài đặt thực tế bản CLI `kiiaud-v0.1.13-android-armv7.tar.gz` lên TV

1. **Tải và giải nén gói phát hành:**
   - Nguồn: `https://github.com/kii-notes/kiiaud/releases/download/v0.1.13/kiiaud-v0.1.13-android-armv7.tar.gz`
   - Thành phần bên trong gồm 2 binary ELF 32-bit (`armv7-linux-androideabi`): `kiiaud` (7.94 MB), `kiiaudctl` (805 KB) và file cấu hình mẫu `kiiaud.toml.example`.
2. **Đẩy file lên phân vùng thực thi được của Android (`/data/local/tmp/kiiaud`):**
   - Trên Android TV không root (`uid=2000(shell)`), phân vùng `/` là `read-only` (và `$HOME` mặc định trỏ về `/`), còn `/sdcard` được mount với cờ `noexec`. Do đó, vị trí duy nhất cho phép `shell` vừa ghi vừa cấp quyền thực thi (`chmod 755`) là `/data/local/tmp/kiiaud`:

   ```powershell
   $adb = "D:\Apps\System\platform-tools\adb.exe"
   & $adb -s 192.168.10.136:5555 shell "mkdir -p /data/local/tmp/kiiaud/.config/kiiaud"
   & $adb -s 192.168.10.136:5555 push kiiaud kiiaudctl config.toml start.sh stop.sh /data/local/tmp/kiiaud/
   & $adb -s 192.168.10.136:5555 shell "chmod 755 /data/local/tmp/kiiaud/kiiaud /data/local/tmp/kiiaud/kiiaudctl /data/local/tmp/kiiaud/start.sh /data/local/tmp/kiiaud/stop.sh"
   ```

3. **Khởi chạy với biến môi trường `HOME` và `TMPDIR`:**
   - Vì `$HOME` mặc định của `adb shell` là `/`, nếu không truyền `HOME=/data/local/tmp/kiiaud` và `-c /data/local/tmp/kiiaud/config.toml`, `kiiaud` sẽ báo lỗi `Failed to create config directory '/.config/kiiaud': Read-only file system (os error 30)`.

### 5.2. Cấu hình thực tế (`/data/local/tmp/kiiaud/config.toml`) cho thiết bị `KII-Private`

Dựa trên cấu hình mẫu của hệ thống và năng lực phần cứng thực tế của DAC **C-Media Digital Hifi Audio (`0d8c:0171`)** trên TV (hỗ trợ PCM 16/24-bit tối đa `192000 Hz`, không hỗ trợ Native DSD và không hỗ trợ DoP):

```toml
[daemon]
name = "KII-Private"
socket_path = "/data/local/tmp/kiiaud/kiiaud.sock"
log_level = "info"

[output]
backend = "android"
device = "default"
bit_perfect = true
dsd_mode = "auto"
buffer_time_ms = 200
period_time_ms = 40

[security]
allowed_roots = []

[adapter.web]
enabled = true
listen = "0.0.0.0:7110"

[adapter.mpd]
enabled = false
listen = "0.0.0.0:6600"

[adapter.jellyfin]
enabled = true
server_url = "https://jellyfin.hexagram.ipv64.net"
username = "khaidanghung"
password = "Dang*K2DH-JF#"
device_id = "kiiaud-kii-private"
device_name = "kiiaud KII-Private"

[adapter.lms]
enabled = true
server_host = "192.168.50.83"
server_port = 3483
http_port = 9000
player_name = "kiiaud KII-Private (LAN)"
dsd_support = false
max_sample_rate = 192000
mac_address = [2, 75, 73, 73, 65, 87] # 02:4b:49:49:41:57

[adapter.lag]
enabled = true
gateway_url = "https://lms.kii.ipv64.net"
gateway_username = "cohancongtu"
gateway_password = "Dang*CoHan-LMS#"
gateway_device_name = "kiiaud KII-Private"
gateway_quality = "original"
player_name = "kiiaud KII-Private (LAG)"
dsd_support = false
max_sample_rate = 192000
```

### 5.3. Kết quả kiểm thử thực tế & 4 Vấn đề phát hiện trên Android TV (`v0.1.13`)

Khi chạy thực tế trên Sony Bravia VU3 (Android TV 14, không root, `armeabi-v7a`):

1. **WebUI (`:7110`) và LMS LAN (`192.168.50.83:3483`) hoạt động tốt:**
   - LMS nhận diện ngay player `kiiaud KII-Private (LAN)` (`02:4b:49:49:41:57`, `max_sample_rate=192000`, `dsd_supported=false`) và gửi luồng nhạc FLAC (`strm-s`) xuống `kiiaud`, bộ giải mã `kiiaud_decoder` mở stream `PCM 44100Hz 16-bit (S16_LE) 2ch` thành công.
2. **Lỗi 1 — `backend = "android"` (`Oboe/AAudio`) chưa phát âm thanh thật (Stub/Dummy Sink):**
   - Kiểm tra `readelf -d kiiaud` cho thấy binary chỉ liên kết `libc.so`, `libm.so`, `libdl.so` (chưa liên kết `libaaudio.so` hoặc `libOpenSLES.so`).
   - Trong `crates/kiiaud-output-android/src/lib.rs`, nhánh `AndroidOutput` hiện đang là dummy sink trả về `Ok` ngay lập tức mà không ghi xuống phần cứng hay chặn theo nhịp mẫu (sample clock), dẫn đến việc mỗi bài FLAC bị tiêu thụ hết trong ~2 giây mà không có tiếng ra DAC.
   - Nhánh `backend = "android-usb"` (`usb_output.rs`) đã có code `USBDEVFS` thật nhưng trên Android không root, `/dev/bus/usb/001/003` thuộc `root:usb` (`0660`), bắt buộc phải có **Android Companion APK** gọi `UsbManager.requestPermission()` để lấy `FileDescriptor` (`fd:N`) truyền qua JNI.
3. **Lỗi 2 — Panic `rustls-platform-verifier` khi kết nối `https://` (`LAG` & `Jellyfin`):**
   - Hai worker thread của `[adapter.lag]` (`https://lms.kii.ipv64.net`) và `[adapter.jellyfin]` (`https://jellyfin.hexagram.ipv64.net`) bị panic tại:
     `rustls-platform-verifier-0.7.1/src/android.rs:90:10: Expect rustls-platform-verifier to be initialized`
   - Nguyên nhân: Trên target `*-linux-android*`, `reqwest` / `tokio-tungstenite` khi bật `rustls-tls-native-roots` sẽ dùng `rustls-platform-verifier`, vốn đòi hỏi JNI context (`rustls_platform_verifier::android::init_hosted`) từ JVM của APK.
4. **Lỗi 3 — SELinux chặn tạo file Unix Domain Socket (`/data/local/tmp/kiiaud/kiiaud.sock`) & thiếu APK cho Google TV Launcher:**
   - Policy SELinux `u:r:shell:s0` trên Android 14 chặn `bind()` tạo `sock_file` trên filesystem trong `/data/local/tmp` (`Permission denied (os error 13)`).
   - Đồng thời, để có biểu tượng trên màn hình chính Google TV (và cấp quyền USB DAC không cần ADB), cần đóng gói **1 Universal Android Companion APK dùng chung cho cả Mobile và Android TV / Google TV**.

### 5.4. Bản chỉ dẫn kỹ thuật (Handoff Prompt) dành cho AI Agent trên AlmaLinux

*(Xem chi tiết trong tài liệu hoặc sao chép trực tiếp khối chỉ dẫn bên dưới cho AI Agent phát triển dự án `kiiaud` trên AlmaLinux).*

