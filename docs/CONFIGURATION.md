# kiiaud Configuration Guide & Reference

Tài liệu này hướng dẫn chi tiết về cấu trúc, vị trí, thứ tự ưu tiên và toàn bộ các tham số (options) cấu hình của **kiiaud** — Bit-Perfect Network Audio Playback Daemon.

---

## 1. Vị trí và Thứ tự Ưu tiên File Cấu hình (Configuration Hierarchy)

kiiaud hỗ trợ cả hai mô hình vận hành: **System-wide Daemon** (dành cho headless music server / Raspberry Pi chuyên dụng) và **Per-user Daemon** (dành cho máy tính cá nhân đa người dùng).

### 1.1. Các đường dẫn chuẩn

| Phạm vi (Scope) | Đường dẫn mặc định (Linux / Unix) | Đường dẫn mặc định (Windows) | Mục đích sử dụng |
| :--- | :--- | :--- | :--- |
| **System-wide** | `/etc/kiiaud/config.toml` | `%ProgramData%\kiiaud\config.toml` | Cấu hình cấp hệ điều hành (phần cứng DAC, security roots) |
| **Per-user** | `~/.config/kiiaud/config.toml`<br>*(hoặc `$XDG_CONFIG_HOME/kiiaud/config.toml`)* | `%APPDATA%\kiiaud\config.toml` | Cấu hình riêng cho từng user (Jellyfin account, WebUI port...) |
| **Portable / Local** | `./kiiaud.toml`<br>hoặc `<exe_dir>/kiiaud.toml` | `.\kiiaud.toml`<br>hoặc `<exe_dir>\kiiaud.toml` | Chạy độc lập (Portable), đóng gói kèm binary hoặc dev trực tiếp từ repo |

### 1.2. Thứ tự ưu tiên & Cơ chế nạp cấu hình

**Quy tắc ưu tiên từ cao xuống thấp:**
```
1. [ƯU TIÊN TUYỆT ĐỐI] Các tham số Dòng lệnh (CLI Flags: --device, --dsd-mode, --bit-perfect, ...)
2. File cấu hình chỉ định tường minh (--config <FILE>)
3. Cấu hình Cục bộ / Thư mục ứng dụng (Portable: ./kiiaud.toml, <exe_dir>/kiiaud.toml)
4. Cấu hình Per-User (~/.config/kiiaud/config.toml) merge với System-wide
5. Cấu hình System-wide (/etc/kiiaud/config.toml)
6. Môi trường mới (không có cấu hình ở bất kỳ đâu): Tự động tạo file mẫu ~/.config/kiiaud/config.toml
7. Cấu hình mặc định trong mã nguồn (Built-in Defaults)
```

```
┌────────────────────────────────────────────────────────────────────────┐
│  CLI FLAGS (--name, -d/--device, --bit-perfect, --web, --jellyfin...)   │ ◄── [ƯU TIÊN CAO NHẤT]
└───────────────────────────────────┬────────────────────────────────────┘
                                    │ Ghi đè lên mọi giá trị đọc từ file
                                    ▼
┌────────────────────────────────────────────────────────────────────────┐
│  kiiaud --config <FILE> (Chỉ định rõ file cấu hình)                   │
└───────────────────────────────────┬────────────────────────────────────┘
                                    │ (Nếu không truyền cờ --config)
                                    ▼
                        ┌───────────────────────┐
                        │  Chế độ chạy là gì?   │
                        └───────────────────────┘
                          /                   \
(Có cờ --system)         /                     \ (Chạy bình thường dưới quyền User)
                        /                       ▼
                       /       ┌────────────────────────────────────────────────┐
                      /        │ 1. Thư mục hiện tại (./kiiaud.toml, config.toml)│
                     /         │    hoặc Thư mục ứng dụng (<exe>/kiiaud.toml)   │
                    /          └───────────────────────┬────────────────────────┘
                   /                                   │ Có file portable?
                  /                        Có ─────────┴───────── Không
                 /                         │                       │
                /                          ▼                       ▼
               /               ┌───────────────────────┐ ┌──────────────────────────────┐
              /                │ Nạp cấu hình Portable │ │ 2. Hierarchy Deep Merge:     │
             /                 │ (Không ghi đè vào     │ │    Đọc /etc/kiiaud/config    │
            /                  │  ~/.config)           │ │    Merge ~/.config/kiiaud/   │
           /                   └───────────────────────┘ └──────────────┬───────────────┘
          ▼                                                             │ Có ít nhất 1 file?
┌───────────────────────────┐                                Có ────────┴──────── Không
│  SYSTEM-WIDE MODE         │                                │                     │
│  Chỉ đọc:                 │                                ▼                     ▼
│  /etc/kiiaud/config.toml  │                    ┌───────────────────────┐ ┌────────────────────┐
└───────────────────────────┘                    │ Nạp cấu hình đã merge │ │ 3. Lần đầu chạy:   │
                                                 └───────────────────────┘ │    Tự tạo file mẫu │
                                                                           │    tại ~/.config/  │
                                                                           └────────────────────┘
```

#### Quy ước Deep Merge (Table-level merge):
- Nếu key là một **Table** (như `[output]`, `[adapter.jellyfin]`): kiiaud duyệt đệ quy từng trường. User có thể chỉ cần ghi đè `device = "hw:1,0"` trong khi vẫn kế thừa toàn bộ các thiết lập `bit_perfect = true` và `buffer_time_ms = 200` từ `/etc/kiiaud/config.toml`.
- Nếu key là **Scalar value** (string, bool, int): giá trị ở file user ghi đè hoàn toàn giá trị của hệ thống.
- **Bảo toàn**: Các thiết lập không được nhắc đến trong file user sẽ giữ nguyên giá trị từ file hệ thống (hoặc fallback default).

---

## 2. Tra cứu chi tiết từng Option (Full Reference)

### 2.1. Section `[daemon]`
Cấu hình nhận diện tiến trình daemon và logging.

| Tham số | Kiểu | Mặc định | Mô tả |
| :--- | :--- | :--- | :--- |
| `name` | String | `"kiiaud-endpoint"` | Tên endpoint nhận diện hiển thị trong log, service discovery và WebUI. |
| `socket_path` | String (Path) | `"/tmp/kiiaud.sock"` | Đường dẫn Unix Domain Socket phục vụ IPC cho công cụ điều khiển dòng lệnh `kiiaudctl`.<br>*(Khuyến nghị: với user session, có thể đặt vào `/tmp/kiiaud.sock` hoặc `$XDG_RUNTIME_DIR/kiiaud.sock`; với system service đặt vào `/run/kiiaud/kiiaud.sock`)*. |
| `log_level` | String | `"info"` | Mức độ chi tiết của nhật ký hệ thống. Các giá trị hợp lệ: `"trace"`, `"debug"`, `"info"`, `"warn"`, `"error"`. Biến môi trường `RUST_LOG` có độ ưu tiên cao hơn giá trị này nếu được thiết lập. |

> [!NOTE]
> **Single-Instance Guard**: `kiiaud` tự động ngăn chặn việc chạy nhiều tiến trình daemon cùng lúc gây tranh chấp card âm thanh (trên Windows dùng Named Mutex `Local\kiiaud_single_instance_{name}`, trên Linux dùng Unix file lock `flock`). Nếu muốn chạy đồng thời nhiều tiến trình thử nghiệm, sử dụng cờ `--allow-multiple-instances`.

---

### 2.2. Section `[output]`
Cấu hình phần cứng âm thanh và cơ chế phát nhạc Bit-Perfect.

| Tham số | Kiểu | Mặc định | Mô tả |
| :--- | :--- | :--- | :--- |
| `backend` | String | `"alsa"` (Linux)<br>`"wasapi"` (Win)<br>`"oboe"` (Android) | Backend xuất âm thanh. Các giá trị hợp lệ:<br>- `"alsa"`: Giao tiếp ALSA hardware-direct trên Linux (bypass hoàn toàn PulseAudio/PipeWire mixer).<br>- `"wasapi"`: Windows Audio Session API (chạy Exclusive Mode, MMCSS Pro Audio, hỗ trợ PCM & DoP).<br>- `"asio"`: Windows ASIO driver transport (giao tiếp trực tiếp phần cứng, hỗ trợ **Native 1-bit DSD** từ DSD64 đến DSD1024 và Bit-perfect PCM).<br>- `"android-usb"`: Driver USB UAC1/UAC2 thuần Rust chạy trực tiếp trên Android qua Linux `usbdevfs` ioctl, bypass hoàn toàn AudioFlinger, AAudio và kernel ALSA driver.<br>- `"android"` hoặc `"oboe"`: AAudio/AudioTrack direct trên Android.<br>- `"mock"` hoặc `"null"`: Dummy backend phục vụ test/CI không cần DAC. |
| `device` | String | `"hw:CARD=Audio,DEV=0"` | Định danh thiết bị xuất phần cứng.<br>- Trên Linux ALSA: Dùng cú pháp `hw:CARD=<TênCard>,DEV=0` hoặc `hw:<IndexCard>,<IndexDev>`. Xem danh sách card qua lệnh `aplay -l`.<br>- Trên Windows WASAPI: Tên chuỗi con của DAC (vd `"DAC"`) hoặc `"default"`.<br>- Trên Windows ASIO: Tên hoặc chuỗi con của ASIO driver (không phân biệt hoa thường, ví dụ: `"SXW UAD ASIO"`, `"Bravo-HD"`, `"Focusrite"`, `"UAD"`) hoặc `"default"` để tự động chọn driver ASIO đầu tiên tìm thấy trong Windows Registry (`HKLM\SOFTWARE\ASIO`). |
| `bit_perfect` | Boolean | `true` | **Cực kỳ quan trọng**: Khi bật (`true`), kiiaud vô hiệu hóa hoàn toàn mọi thuật toán DSP phần mềm (không resample, không đổi bit depth, không chỉnh volume số làm tròn bit). DAC luôn được cấu hình đúng tần số lấy mẫu gốc của file âm thanh (từ 44.1kHz đến 768kHz, DSD64-DSD1024). Với backend ASIO, chế độ luôn là direct hardware stream. |
| `dsd_mode` | String | `"auto"` | Cơ chế truyền tải tín hiệu DSD (Direct Stream Digital):<br>- `"auto"`: Tự động tối ưu. Trên Linux ALSA: Thử Native DSD trước (`DSD_U32_BE`/`LE`). Trên Windows ASIO: Thử Steinberg ASIO 2.0+ Native DSD (`kAsioSetIoFormat`, `ASIOSTDSDInt8LSB1`/`MSB1`). Nếu driver/DAC không hỗ trợ Native DSD, tự động fallback sang DoP (DSD over PCM). Tuyệt đối không convert DSD sang PCM.<br>- `"native"`: Bắt buộc dùng Native 1-bit DSD (hỗ trợ tới DSD1024 trên ASIO/ALSA). Nếu driver/DAC không hỗ trợ sẽ báo lỗi.<br>- `"dop"`: Đóng gói các bit DSD vào khung PCM 24/32-bit theo chuẩn DoP v1.1. |
| `buffer_time_ms` | Integer (u32) | `200` | Kích thước ring-buffer của phần cứng (đơn vị: mili-giây). Giúp tránh hiện tượng giật tiếng (buffer underrun) khi CPU bận. (Trên ASIO, kích thước buffer phần cứng được cấu hình qua ASIO Driver Control Panel và tự động đồng bộ). |
| `period_time_ms` | Integer (u32) | `40` | Chu kỳ ngắt phần cứng ALSA (đơn vị: mili-giây). |
| `hardware_mixer` | Option\<String\> | `null` (commented) | Tên bộ điều khiển volume analog bằng phần cứng của DAC (ví dụ: `"PCM"`, `"Master"`). Nếu để trống, âm lượng phần cứng được giữ nguyên 100% để đảm bảo tín hiệu bit-perfect. |

---

### 2.2.1. Hướng dẫn cấu hình ASIO & Native 1-bit DSD trên Windows

`kiiaud` trang bị backend ASIO độc lập (`kiiaud-output-asio`), cho phép các audiophile trên Windows chơi nhạc với chất lượng phòng thu cao nhất:

#### 1. So sánh WASAPI Exclusive vs. ASIO trên Windows

| Tiêu chí | WASAPI Exclusive (`backend = "wasapi"`) | ASIO Direct (`backend = "asio"`) |
| :--- | :--- | :--- |
| **Bypass Windows Mixer** | Có (Exclusive Mode) | Có (Giao tiếp trực tiếp driver phần cứng) |
| **Độ trễ (Latency)** | Rất thấp (MMCSS Pro Audio) | Cực thấp (Double-buffered hardware callbacks) |
| **Định dạng PCM** | 16-bit, 24-bit, 32-bit, Float32 (tới 768kHz) | 16-bit, 24-bit, 32-bit, Int32LSB24, Float32 (tới 768kHz) |
| **Phát DSD** | **DoP (DSD over PCM)** (tối đa DSD256) | **Native 1-bit DSD** (DSD64, DSD128, DSD256, DSD512, DSD1024) và DoP |
| **Cài đặt Driver** | Plug & Play (dùng driver Windows tiêu chuẩn) | Cần driver ASIO của nhà sản xuất DAC (hoặc ASIO4ALL) |

#### 2. Cơ chế tự động tìm kiếm Driver ASIO (Registry Discovery)
- `kiiaud` tự động quét các khóa Windows Registry:
  - `HKEY_LOCAL_MACHINE\SOFTWARE\ASIO` (64-bit native drivers)
  - `HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node\ASIO` (32-bit drivers)
- Khi `device = "default"`, kiiaud tự động nạp driver đầu tiên được tìm thấy.
- Bạn có thể chỉ định tên chính xác hoặc một phần tên (case-insensitive), ví dụ:
  - `device = "SXW UAD ASIO"`
  - `device = "Bravo-HD"`
  - `device = "Focusrite"`
  - `device = "XMOS"`

#### 3. Cấu hình mẫu cho Windows ASIO (`kiiaud.toml`):
```toml
[output]
backend = "asio"
device = "SXW UAD ASIO"   # hoặc "default"
bit_perfect = true
dsd_mode = "auto"         # "auto" (Native DSD với DoP fallback), hoặc "native" (chỉ Native DSD)
buffer_time_ms = 200
```


---

### 2.3. Section `[security]`
Chính sách kiểm soát truy cập tệp tin nội bộ.

| Tham số | Kiểu | Mặc định | Mô tả |
| :--- | :--- | :--- | :--- |
| `allowed_roots` | Array of Paths | `[]` | Danh sách các thư mục gốc được phép truy xuất để phát tệp cục bộ (Local File Playback).<br>Ví dụ: `["/media/music", "/mnt/nas/flac"]`.<br>Nếu để mảng rỗng `[]`, kiiaud cho phép phát mọi tệp thông thường (regular file) hợp lệ của người dùng nhưng vẫn chặn các đường dẫn nhạy cảm của hệ thống (`/etc`, `/proc`, `/sys`, `/dev`). |

---

### 2.4. Section `[adapter.web]`
Giao diện người dùng WebUI tích hợp và REST/SSE API.

| Tham số | Kiểu | Mặc định | Mô tả |
| :--- | :--- | :--- | :--- |
| `enabled` | Boolean | `true` | Bật/tắt WebUI tích hợp. |
| `listen` | String | `"0.0.0.0:7110"` | Địa chỉ IP và cổng TCP lắng nghe. Mở trình duyệt tại `http://<IP-thiết-bị>:7110` để xem trạng thái DAC, thời gian thực, queue và tải bài nhạc. |

---

### 2.5. Section `[adapter.mpd]`
Trình điều khiển MPD (Music Player Daemon) - Universal Playlist & Transport Controller.

| Tham số | Kiểu | Mặc định | Mô tả |
| :--- | :--- | :--- | :--- |
| `enabled` | Boolean | `true` | Bật/tắt server MPD. |
| `listen` | String | `"0.0.0.0:6600"` | Cổng lắng nghe MPD standard. Cho phép kết nối và điều khiển kiiaud từ mọi ứng dụng MPD Client thông dụng trên máy tính và điện thoại (ncmpcpp, Cantata, MPDroid, MAFA, Rigelian...). |
| `music_directory` | Option\<String\> | `null` | Thư mục nhạc gốc cho lệnh duyệt file trực tiếp `lsinfo` và cho các lệnh nạp đường dẫn tương đối `add`. Không cần lập cơ sở dữ liệu. |

---

### 2.6. Section `[adapter.jellyfin]`
Endpoint tiếp nhận điều khiển phát nhạc từ xa ("Play To" / Cast Target) của Jellyfin.

| Tham số | Kiểu | Mặc định | Mô tả |
| :--- | :--- | :--- | :--- |
| `enabled` | Boolean | `false` | Bật/tắt adapter Jellyfin. Khi bật, kiiaud sẽ duy trì WebSocket session với máy chủ Jellyfin và xuất hiện trong menu Cast ("Play On") của app Jellyfin. |
| `server_url` | String | `"http://jellyfin.local:8096"` | Địa chỉ HTTP/HTTPS của máy chủ Jellyfin. |
| `api_key` | Option\<String\> | `null` | API Key tạo từ Jellyfin Dashboard (ưu tiên sử dụng nếu có). |
| `username` | Option\<String\> | `null` | Tên đăng nhập Jellyfin (dùng nếu không dùng `api_key`). |
| `password` | Option\<String\> | `null` | Mật khẩu tài khoản Jellyfin. |
| `device_id` | String | `"kiiaud-endpoint-01"` | Mã định danh duy nhất của thiết bị trên Jellyfin server. |
| `device_name` | String | `"kiiaud Hi-Fi Endpoint"` | Tên thiết bị hiển thị trong menu "Play On" trên Web và App Jellyfin. |

---

### 2.7. Section `[adapter.lms]`
Client kết nối tới Logitech Media Server / Lyrion Music Server trong mạng cục bộ LAN (giao thức SlimProto `DirectTcp` trên cổng 3483 và stream HTTP trên cổng 9000). Hỗ trợ **hot-reload trực tiếp lúc đang chạy** từ WebUI (`reload_lms_adapter()`) mà không cần khởi động lại daemon.

| Tham số | Kiểu | Mặc định | Mô tả |
| :--- | :--- | :--- | :--- |
| `enabled` | Boolean | `false` | Bật/tắt client LMS LAN. Giả lập một Squeezebox player hoàn chỉnh (Device ID 12). |
| `server_host` | String | `"192.168.1.100"` | Địa chỉ IP hoặc hostname của LMS server trong mạng LAN. |
| `server_port` | Integer (u16) | `3483` | Cổng SlimProto TCP của LMS server. |
| `http_port` | Integer (u16) | `9000` | Cổng HTTP Web của LMS để stream âm thanh. |
| `player_name` | String | `"kiiaud Squeezebox"` | Tên player hiển thị trong giao diện LMS / Material Skin. |
| `mac_address` | String hoặc Array 6 bytes | `[2, 75, 73, 73, 65, 85]` | Địa chỉ MAC định danh player trong mạng LAN (`DirectTcp`). Hỗ trợ dạng chuỗi (`"38:D5:47:15:E8:86"`), mảng Hex (`[0x38, ...]`), hoặc mảng số nguyên thập phân (`[56, 213, ...]`). |
| `max_sample_rate` | Option\<u32\> | `null` (auto) | Tần số lấy mẫu tối đa thông báo tới LMS trong frame `HELO` (ví dụ: `192000`, `384000`, `768000`). |
| `dsd_support` | Option\<bool\> | `null` (auto) | Thông báo LMS hỗ trợ phát DSD trực tiếp (`dsf`, `dff`) hay yêu cầu LMS tự động transcode sang PCM. |

---

### 2.8. Section `[adapter.lag]` — Lyrion Anywhere (Gateway)
Client kết nối từ xa tới [Lyrion Anywhere Gateway](https://github.com/JameZUK/lyrion-anywhere) qua Internet (đường hầm WebSocket `WSS /v1/player` và luồng HTTP stream có xác thực Bearer token). Được thiết kế hoàn toàn độc lập với adapter LMS LAN, cho phép bật/tắt riêng biệt (`enabled = true / false`) hoặc chạy song song cả hai mà không xung đột. Hỗ trợ **hot-reload trực tiếp** từ WebUI (`reload_lag_adapter()`).

| Tham số | Kiểu | Mặc định | Mô tả |
| :--- | :--- | :--- | :--- |
| `enabled` | Boolean | `false` | Bật/tắt kết nối từ xa Lyrion Anywhere Gateway. |
| `gateway_url` | Option\<String\> | `null` | Địa chỉ HTTPS/HTTP của Gateway (ví dụ: `"https://music.example.com"`). |
| `gateway_quality` | String | `"original"` | Chế độ chất lượng stream:<br>- `"original"` (**Audiophile Bit-Perfect Passthrough**): Giữ nguyên luồng FLAC / PCM / WavPack / DSD gốc từ LMS, không transcode.<br>- `"high"`: Gateway transcode sang Opus ~192 kbps.<br>- `"low"`: Gateway transcode sang Opus ~96 kbps. |
| `gateway_refresh_token` | Option\<String\> | `null` | Refresh token dài hạn (90 ngày) dùng cho phiên kết nối Gateway. Tự động được lưu và xoay vòng (rotate) khi đăng nhập hoặc ghép đôi trên WebUI / CLI. |
| `gateway_device_name` | Option\<String\> | `null` | Tên thiết bị đăng ký trên Gateway khi thực hiện ghép đôi (mặc định lấy theo `player_name` nếu để trống). |
| `gateway_pairing_code` | Option\<String\> | `null` | Mã ghép đôi dùng 1 lần (`ABCDEFGHJK`) hoặc link `lyrion-anywhere://pair?...` dùng để đổi sang `gateway_refresh_token` trong lần kết nối đầu tiên. |
| `gateway_username` | Option\<String\> | `null` | Tên đăng nhập tài khoản Gateway (tùy chọn). |
| `gateway_password` | Option\<String\> | `null` | Mật khẩu tài khoản Gateway (tùy chọn). |
| `player_name` | Option\<String\> | `null` | Tên hiển thị của player trên Gateway (mặc định dùng tên chung của daemon nếu để trống). |
| `max_sample_rate` | Option\<u32\> | `null` (auto) | Tần số lấy mẫu tối đa thông báo tới Gateway/LMS trong frame `HELO`. |
| `dsd_support` | Option\<bool\> | `null` (auto) | Bật/tắt cờ hỗ trợ DSD trực tiếp gửi tới Gateway. |

> [!NOTE]
> **Tương thích ngược (Backward Compatibility)**: Nếu file cấu hình cũ sử dụng cú pháp gộp `[adapter.lms] use_gateway = true`, `kiiaud` sẽ tự động chuyển đổi (migrate) các thiết lập Gateway sang `[adapter.lag]` ngay khi nạp cấu hình mà không làm gián đoạn vận hành.

#### 2.7.1. Hướng dẫn toàn diện: Xác định, Quy đổi và Cấu hình Địa chỉ MAC cho LMS

##### 1. Tại sao LMS / Lyrion cần địa chỉ MAC?
- Giao thức **SlimProto** của Logitech Media Server định danh và quản lý các máy nghe nhạc (Squeezebox, Squeezelite, kiiaud) duy nhất thông qua **địa chỉ MAC 6 byte**.
- LMS dùng địa chỉ MAC này làm khóa chính để lưu trạng thái phiên nghe: danh sách bài hát đang phát (playlist), mức âm lượng (volume), bộ cân bằng âm thanh (equalizer), thiết lập plugin và liên kết nhóm phát đồng bộ đa phòng (sync group).
- **Lưu ý quan trọng**: Nếu có hai thiết bị chạy kiiaud hoặc squeezelite trong cùng mạng LAN dùng chung một địa chỉ MAC, LMS sẽ coi chúng là cùng một player và gây ra xung đột điều khiển. Mỗi endpoint phải có một địa chỉ MAC riêng biệt.

##### 2. Cách xác định địa chỉ MAC của máy tính
Tùy thuộc vào hệ điều hành đang cài đặt `kiiaud`:

- **Trên Windows**:
  1. Mở PowerShell hoặc Command Prompt (`cmd`).
  2. Chạy lệnh:
     ```cmd
     getmac /v
     ```
     hoặc:
     ```cmd
     ipconfig /all
     ```
  3. Tìm đến card mạng đang kết nối Internet/LAN (ví dụ `Ethernet` hoặc `Wi-Fi`), đọc giá trị tại dòng **Physical Address** (Địa chỉ vật lý), có dạng: `38-D5-47-15-E8-86`.

- **Trên Linux** (Raspberry Pi, Ubuntu, Debian, Arch,...):
  1. Mở Terminal và chạy:
     ```bash
     ip link
     ```
     hoặc đọc trực tiếp từ sysfs:
     ```bash
     cat /sys/class/net/eth0/address   # Thay eth0 bằng tên card mạng như wlan0, enp3s0
     ```
  2. Địa chỉ MAC sẽ hiển thị dạng: `38:d5:47:15:e8:86`.

- **Trên macOS**:
  1. Mở Terminal và chạy:
     ```bash
     ifconfig en0 | grep ether
     ```
  2. Kết quả có dạng: `ether 38:d5:47:15:e8:86`.

##### 3. Các cách biểu diễn địa chỉ MAC trong file cấu hình kiiaud
`kiiaud` hỗ trợ đầy đủ và linh hoạt các định dạng biểu diễn:

- **Cách 1: Chuỗi ký tự chuẩn (Khuyên dùng - Đơn giản nhất)**
  Dán trực tiếp chuỗi MAC bạn vừa copy (hỗ trợ cả dấu hai chấm `:` hoặc gạch nối `-` của Windows):
  ```toml
  [adapter.lms]
  mac_address = "38:D5:47:15:E8:86"
  # Hoặc:
  # mac_address = "38-D5-47-15-E8-86"
  ```

- **Cách 2: Mảng Hex `0x..` (Chuẩn TOML nguyên bản)**
  Giữ nguyên giá trị hex của từng byte và thêm tiền tố `0x`:
  ```toml
  [adapter.lms]
  mac_address = [0x38, 0xD5, 0x47, 0x15, 0xE8, 0x86]
  ```

- **Cách 3: Mảng số nguyên thập phân (Decimal 0–255)**
  Chuyển đổi từng byte hex thành số nguyên thập phân (tương đương với mẫu mặc định `[2, 75, 73, 73, 65, 85]`):
  ```toml
  [adapter.lms]
  mac_address = [56, 213, 71, 21, 232, 134]
  ```

##### 4. Bảng và Công cụ quy đổi nhanh Hex sang Decimal
Mỗi byte trong địa chỉ MAC gồm 2 ký tự Hex `XY`. Công thức tính sang số thập phân:
$$\text{Giá trị Decimal} = (X \times 16) + Y$$
*(với $A=10, B=11, C=12, D=13, E=14, F=15$)*

| Byte Hex | Phép tính quy đổi | Giá trị Decimal |
| :---: | :---: | :---: |
| `38` | $3 \times 16 + 8 = 48 + 8$ | **56** |
| `D5` | $13 \times 16 + 5 = 208 + 5$ | **213** |
| `47` | $4 \times 16 + 7 = 64 + 7$ | **71** |
| `15` | $1 \times 16 + 5 = 16 + 5$ | **21** |
| `E8` | $14 \times 16 + 8 = 224 + 8$ | **232** |
| `86` | $8 \times 16 + 6 = 128 + 6$ | **134** |

**Lệnh quy đổi tự động 1 dòng:**

- **PowerShell (Windows)**:
  ```powershell
  "[" + (("38:D5:47:15:E8:86" -split '[:-]' | ForEach-Object { [Convert]::ToByte($_, 16) }) -join ", ") + "]"
  # Kết quả: [56, 213, 71, 21, 232, 134]
  ```

- **Python (Mọi nền tảng)**:
  ```bash
  python3 -c 'mac="38:D5:47:15:E8:86"; print([int(x, 16) for x in mac.replace("-", ":").split(":")])'
  # Kết quả: [56, 213, 71, 21, 232, 134]
  ```

- **Bash / Linux**:
  ```bash
  echo "38:D5:47:15:E8:86" | awk -F'[:-]' '{printf "[%d, %d, %d, %d, %d, %d]\n", "0x"$1, "0x"$2, "0x"$3, "0x"$4, "0x"$5, "0x"$6}'
  ```

##### 5. Ghi đè địa chỉ MAC qua tham số dòng lệnh (CLI)
Bạn có thể ghi đè địa chỉ MAC mà không cần sửa file `kiiaud.toml` bằng cờ `--lms-mac`:
```bash
kiiaud --lms-mac "38:D5:47:15:E8:86"
```

##### 6. Lưu ý về Địa chỉ MAC ảo (Locally-Administered MAC)
Nếu không muốn dùng địa chỉ MAC thật của card mạng phần cứng (hoặc trên môi trường Docker / máy ảo không có MAC cố định):
- Bạn hoàn toàn có thể tự sinh một địa chỉ MAC ảo. Theo tiêu chuẩn mạng IEEE 802, địa chỉ MAC quản trị nội bộ (*locally administered*) có bit thứ 2 của byte đầu tiên bằng `1`.
- Các byte đầu hợp lệ phổ biến: `02`, `06`, `0A`, `0E`.
- Ví dụ: Mặc định của `kiiaud` là `02:4B:49:49:41:55` (`[0x02, 0x4B, 0x49, 0x49, 0x41, 0x55]`), trong đó 5 byte sau chính là các ký tự mã ASCII của từ `"KIIAU"` (`[75, 73, 73, 65, 85]`).

---

#### 2.8.1. Hướng dẫn Kết nối từ xa qua Lyrion Anywhere Gateway (`adapter.lag`)

Khi thiết bị chạy `kiiaud` (ở văn phòng, nhà nghỉ, hoặc qua 4G/5G) nằm ngoài mạng LAN của máy chủ LMS ở nhà, bạn có thể kết nối thông qua **[Lyrion Anywhere Gateway](https://github.com/JameZUK/lyrion-anywhere)** chỉ bằng 1 cổng HTTPS duy nhất mà không cần mở port TCP 3483 hay cấu hình VPN.

##### 1. Hai Adapter hoạt động hoàn toàn độc lập
- **LMS Adapter LAN (`[adapter.lms]`)**: Kết nối trực tiếp tới LMS trong mạng LAN qua giao thức TCP SlimProto (port `3483`) và nhận stream HTTP trực tiếp từ LMS (port `9000`). Bật/tắt bằng `enabled = true / false`.
- **Lyrion Anywhere Gateway (`[adapter.lag]`)**: Đóng vai một thiết bị **Lyrion Anywhere Client (Protocol v2)**:
  - Bật/tắt độc lập qua `[adapter.lag] enabled = true / false`.
  - Thiết lập đường hầm WebSocket `WSS /v1/player` tới Gateway.
  - Tự động nhận và áp dụng địa chỉ MAC ảo (`02:...`) và tên thiết bị do Gateway cấp phát trong bản tin Greeting.
  - Nhận luồng âm thanh qua `GET <gateway_url>/v1/stream/<session>/<seq>` kèm `Authorization: Bearer <access_token>` và gửi gói xác nhận lưu lượng `{"ack":{"stream":<seq>,"bytes":<total>}}` mỗi **256 KiB** để trượt cửa sổ bộ đệm 4 MiB của Gateway.
  - Hiển thị đầy đủ thông tin luồng Gateway (`original` Bit-Perfect Passthrough hoặc `high`/`low` Opus) ngay trong bảng **Audiophile Signal Path** trên WebUI.

##### 2. Thao tác Đăng nhập / Ghép đôi ngay trên WebUI (`http://<ip>:7110`)
1. Mở trình duyệt truy cập **Hi-Fi WebUI** (`http://<IP-kiiaud>:7110`) -> chuyển sang tab **Config**.
2. Tìm đến card **Lyrion Anywhere (Gateway)**:
   - Đánh dấu chọn **`Enable Lyrion Anywhere (Gateway) Adapter`**.
   - Nhập **`Gateway URL`** (ví dụ: `https://music.example.com`) và chọn **`Gateway Stream Quality`** (`original — Bit-Perfect Passthrough` để giữ nguyên FLAC/PCM/WavPack/DSD gốc).
3. **Xác thực với Gateway ngay trong lúc cấu hình** (chọn 1 trong 2 cách):
   - **Cách 1 — Mã ghép đôi 1 lần (One-Time Pairing Code — Khuyên dùng)**:
     - Trên trang quản trị Lyrion Anywhere Gateway (`/admin` hoặc `/pair`), tạo mã ghép đôi 10 ký tự (ví dụ `ABCDEFGHJK`) hoặc copy đường dẫn `lyrion-anywhere://pair?gw=...&code=...`.
     - Dán vào ô **Option 1: One-Time Pairing Code or Link** và bấm **`Pair Device`**.
   - **Cách 2 — Tài khoản Username & Password (+ 2FA/TOTP)**:
     - Nhập Username, Password (và mã TOTP 6 số nếu tài khoản bật 2FA) vào ô **Option 2** rồi bấm **`Sign In & Save Token`**.
4. Ngay khi xác thực thành công:
   - `kiiaud` tự động lưu `gateway_refresh_token` (hạn 90 ngày, tự động xoay vòng mỗi 10 phút) vào mục `[adapter.lag]` trong file cấu hình.
   - Adapter Gateway được **hot-reload tức thì** (`reload_lag_adapter()`) mà không ảnh hưởng tới các adapter khác.
   - Bấm **`Save Config to Disk`** bất kỳ lúc nào để lưu các thay đổi lâu dài.

##### 3. Cấu hình mẫu `kiiaud.toml` cho Lyrion Anywhere Gateway
```toml
[adapter.lms]
enabled = false                       # Tắt LMS LAN nếu không ở cùng mạng nội bộ

[adapter.lag]
enabled = true                        # Bật kết nối Lyrion Anywhere (Gateway)
gateway_url = "https://music.example.com"
gateway_quality = "original"          # "original" (Bit-Perfect), "high" (Opus 192k), "low" (Opus 96k)
gateway_refresh_token = "eyJhbGci..." # Tự động điền sau khi Pair / Login trên WebUI hoặc CLI
player_name = "kiiaud Office DAC"
```

---

## 3. Hệ thống Tham số Dòng lệnh (CLI Flags & Overrides)

Hệ thống tham số dòng lệnh của `kiiaud` được thiết kế đầy đủ và bao quát 100% các option trong file cấu hình. Bất kỳ cờ dòng lệnh nào được truyền vào sẽ **ghi đè giá trị tương ứng của file cấu hình**, bất kể file đó là `/etc/kiiaud/config.toml`, `~/.config/kiiaud/config.toml` hay file truyền qua `--config`.

### 3.1. Bảng tra cứu tham số Dòng lệnh tương ứng từng Option

| Phân nhóm CLI | Tham số dòng lệnh | Kiểu / Giá trị mẫu | Trường tương ứng trong TOML |
| :--- | :--- | :--- | :--- |
| **Cấu hình & Vận hành** | `-c, --config <FILE>` | Path | *(Chỉ định file cấu hình tường minh)* |
| | `--system` | Flag | *(Buộc chỉ đọc `/etc/kiiaud/config.toml`)* |
| | `--generate-config [<PATH>]`| Path tùy chọn | *(Sinh file cấu hình mẫu đầy đủ chú thích)* |
| | `--force` | Flag | *(Ghi đè file nếu đã tồn tại)* |
| | `--check-config` | Flag | *(Validate cấu hình đã resolve và thoát)* |
| | `--print-config` | Flag | *(In cấu hình đã resolve ra stdout TOML)* |
| | `--no-auto-generate` | Flag | *(Không tự động tạo file config ở môi trường mới)* |
| | `-p, --play <TARGET>` | Path / URL | *(Phát ngay file hoặc link khi khởi động)* |
| **Daemon Options** | `--name <NAME>` | String (`"kiiaud-rpi5"`) | `[daemon].name` |
| | `--socket-path <PATH>` | Path (`"/tmp/kiiaud.sock"`) | `[daemon].socket_path` |
| | `-l, --log-level <LVL>` | `"trace"`, `"debug"`, `"info"`... | `[daemon].log_level` |
| | `--allow-multiple-instances` | Flag | *(Bỏ qua cơ chế khóa đơn phiên, cho phép chạy song song)* |
| | `--background` | Flag | *(Chạy nền; trên Windows tự động ẩn cửa sổ dòng lệnh console)* |
| | `--tray [<BOOL>]` | `true` hoặc `false` | *(Hiển thị icon thông báo khay hệ thống trên Windows; mặc định bật khi có `--background`)* |
| **Quản lý Service** | `--service <ACTION>` | `"install"`, `"uninstall"`, `"start"`, `"stop"`, `"status"` | *(Quản lý dịch vụ chạy tự động cùng hệ điều hành)* |
| | `--service-install` | Flag | *(Cài đặt dịch vụ nền tự khởi động và thoát)* |
| | `--service-uninstall` | Flag | *(Gỡ bỏ dịch vụ nền và thoát)* |
| | `--service-status` | Flag | *(Kiểm tra trạng thái dịch vụ nền và thoát)* |
| | `--service-start` | Flag | *(Khởi động dịch vụ nền và thoát)* |
| | `--service-stop` | Flag | *(Dừng dịch vụ nền đang chạy và thoát)* |
| **Audio Output** | `-b, --backend <BACKEND>` | `"alsa"`, `"wasapi"`, `"asio"`, `"android-usb"`, `"android"`, `"oboe"`, `"mock"` | `[output].backend` |
| | `-d, --device <DEVICE>` | `"hw:CARD=Audio,DEV=0"` (ALSA), `"SXW UAD ASIO"` / `"default"` (ASIO), `"fd:42"` (Android USB) | `[output].device` |
| | `--bit-perfect [<BOOL>]` | `true` hoặc `false` | `[output].bit_perfect` |
| | `--dsd-mode <MODE>` | `"auto"`, `"native"`, `"dop"` | `[output].dsd_mode` |
| | `--buffer-time-ms <MS>` | Integer (`200`) | `[output].buffer_time_ms` |
| | `--period-time-ms <MS>` | Integer (`40`) | `[output].period_time_ms` |
| | `--hardware-mixer <NAME>` | `"PCM"`, `"Master"`, `"none"` | `[output].hardware_mixer` |
| | `-D, --dac-info` | Flag | *(Kiểm tra thông số DAC, tần số lấy mẫu, hỗ trợ Native DSD và quét driver ASIO)* |
| **Security** | `--allowed-root <PATH>` | Path (có thể truyền nhiều lần) | `[security].allowed_roots` |
| **WebUI Adapter** | `--web [<BOOL>]` | `true` hoặc `false` | `[adapter.web].enabled` |
| | `--web-listen <IP:PORT>` | `"0.0.0.0:7110"` | `[adapter.web].listen` |
| **MPD Adapter** | `--mpd [<BOOL>]` | `true` hoặc `false` | `[adapter.mpd].enabled` |
| | `--mpd-listen <IP:PORT>` | `"0.0.0.0:6600"` | `[adapter.mpd].listen` |
| **Jellyfin Adapter** | `--jellyfin [<BOOL>]` | `true` hoặc `false` | `[adapter.jellyfin].enabled` |
| | `--jellyfin-url <URL>` | `"http://192.168.1.10:8096"` | `[adapter.jellyfin].server_url` |
| | `--jellyfin-api-key <KEY>` | String | `[adapter.jellyfin].api_key` |
| | `--jellyfin-username <USER>`| String | `[adapter.jellyfin].username` |
| | `--jellyfin-password <PASS>`| String | `[adapter.jellyfin].password` |
| | `--jellyfin-device-id <ID>` | String | `[adapter.jellyfin].device_id` |
| | `--jellyfin-device-name <NAME>`| String | `[adapter.jellyfin].device_name` |
| **LMS Adapter (LAN)** | `--lms [<BOOL>]` | `true` hoặc `false` | `[adapter.lms].enabled` |
| | `--lms-host <HOST>` | `"192.168.1.50"` | `[adapter.lms].server_host` |
| | `--lms-port <PORT>` | Integer (`3483`) | `[adapter.lms].server_port` |
| | `--lms-http-port <PORT>` | Integer (`9000`) | `[adapter.lms].http_port` |
| | `--lms-player-name <NAME>` | String | `[adapter.lms].player_name` |
| | `--lms-mac <MAC>` | `"02:4B:49:49:41:55"` | `[adapter.lms].mac_address` |
| **Lyrion Anywhere (Gateway)** | `--lag [<BOOL>]` | `true` hoặc `false` | `[adapter.lag].enabled` |
| | `--lag-url <URL>` | `"https://music.example.com"` | `[adapter.lag].gateway_url` |
| | `--lag-user <USER>` | String | `[adapter.lag].gateway_username` |
| | `--lag-password <PASS>` | String | `[adapter.lag].gateway_password` |
| | `--lag-pair-code <CODE>` | `"ABCDEFGHJK"` hoặc `lyrion-anywhere://...` | `[adapter.lag].gateway_pairing_code` |
| | `--lag-quality <QUALITY>` | `"original"`, `"high"`, `"low"` | `[adapter.lag].gateway_quality` |
| | `--lag-player-name <NAME>` | String | `[adapter.lag].player_name` |
| *(Legacy Gateway Aliases)* | `--lms-gateway`, `--lms-user`, `--lms-password`, `--lms-pair-code`, `--lms-quality` | Tương thích ngược | *(Ghi đè cấu hình `[adapter.lag]`)* |

### 3.2. Ví dụ thực tế kết hợp Dòng lệnh

```bash
# 1. Chạy với backend ASIO trên Windows (tự động nhận diện driver ASIO và Native DSD):
kiiaud -b asio

# 2. Chỉ định driver ASIO cụ thể và ép buộc chế độ Native 1-bit DSD:
kiiaud -b asio -d "SXW UAD ASIO" --dsd-mode native

# 3. Quét kiểm tra DAC và danh sách ASIO driver đăng ký trong Windows Registry:
kiiaud -b asio --dac-info

# 4. Chạy trên Linux ALSA với file config có sẵn nhưng ghi đè thiết bị DAC và chế độ DSD DoP:
kiiaud -d "hw:1,0" --dsd-mode dop

# 5. Chạy trên Android với driver USB Audio UAC2 thuần Rust trực tiếp tới DAC:
kiiaud -b android-usb -d fd:42

# 6. Khởi động chạy nền và kích hoạt icon khay hệ thống (System Tray) trên Windows:
kiiaud --background --tray

# 7. Cài đặt tự khởi động cùng Windows (User Logon Task) hoặc Linux (systemd user unit):
kiiaud --service install
kiiaud --service status
kiiaud --service start
kiiaud --service stop

# 8. Tắt adapter WebUI và MPD trực tiếp từ dòng lệnh:
kiiaud --web false --mpd false

# 9. Kích hoạt và kết nối nhanh tới Jellyfin Server mà không cần mở file cấu hình:
kiiaud --jellyfin --jellyfin-url "http://192.168.1.10:8096" --jellyfin-api-key "secret-key-123"

# 10. Kết nối từ xa tới Lyrion Anywhere Gateway bằng mã ghép đôi 1 lần (Bit-Perfect Passthrough):
kiiaud --lag --lag-url "https://music.example.com" --lag-pair-code "ABCDEFGHJK" --lag-quality original

# 11. Kiểm tra cấu hình kết hợp các cờ ghi đè (không khởi động server):
kiiaud --check-config -b asio -d "SXW UAD ASIO" --bit-perfect true

# 12. Xuất ra file TOML mới từ cấu hình hiện tại kết hợp ghi đè dòng lệnh:
kiiaud --print-config -b asio -d "SXW UAD ASIO" --lms --lms-host "192.168.1.80" > /tmp/resolved.toml
```

---

## 4. Quản lý Dịch vụ Nền & Khay Hệ thống (Background Service & System Tray)

### 4.1. Giải pháp Service trên Windows (Task Scheduler vs. Session 0)
Trong kiến trúc Windows, các service truyền thống (`services.msc` / `sc.exe`) bị cô lập hoàn toàn trong **`Session 0`**. Kể từ Windows Vista trở đi, Microsoft tước bỏ toàn bộ quyền truy cập thiết bị âm thanh, phiên WASAPI Exclusive và các driver COM ASIO đối với các tiến trình chạy trong Session 0.

Để giải quyết vấn đề này mà vẫn đảm bảo daemon chạy tự động và không bị gián đoạn:
- Lệnh `kiiaud --service install` sử dụng **Windows Task Scheduler** (`schtasks.exe`) để đăng ký task tự khởi chạy ngay khi người dùng đăng nhập (`/SC ONLOGON /RL LIMITED /DELAY 0000:05`).
- Tiến trình chạy trực tiếp trong interactive user session, có đầy đủ quyền mở driver ASIO, WASAPI Exclusive và hiển thị System Tray.

### 4.2. Quản lý Service trên Linux (systemd --user)
- Lệnh `kiiaud --service install` tự động sinh file unit tại `~/.config/systemd/user/kiiaud.service` và gọi `systemctl --user enable --now kiiaud`.
- Hoạt động trong session của user có quyền truy cập group `audio` và các cổng socket mạng mà không cần quyền `sudo`/`root`.

### 4.3. Tích hợp Khay Hệ thống (System Tray) trên Windows
Khi chạy với cờ `--background` hoặc `--tray`:
- Cửa sổ terminal console sẽ tự động ẩn đi (`SW_HIDE`).
- Biểu tượng `kiiaud` xuất hiện tại khu vực khay thông báo thanh taskbar.
- Click chuột trái / đúp chuột: Mở ngay WebUI trên trình duyệt web mặc định.
- Click chuột phải: Hiển thị menu ngữ cảnh trực quan:
  - Trạng thái phát nhạc và DAC đang kết nối (vd: `Active: Playing (192.0 kHz / 24-bit)` hoặc `Active: Playing (DSD64)`).
  - Tên thiết bị DAC và backend xuất âm thanh (`DAC: SXW UAD ASIO (asio)`).
  - Menu `Open WebUI (http://localhost:7110)`.
  - Toggle `Show Console Window` / `Hide Console Window` để soi log realtime khi cần.
  - `Exit kiiaud` để tắt an toàn và giải phóng tài nguyên phần cứng.

---

## 5. Bảo mật File Cấu hình (Security & File Permissions)

> [!WARNING]
> Nếu bạn sử dụng Jellyfin Adapter với `username`/`password` hoặc `api_key`, file cấu hình sẽ chứa thông tin xác thực dạng văn bản.

1. **Với người dùng thông thường (`per-user`)**:
   Khi bạn chạy `kiiaud --generate-config` hoặc daemon tự động tạo cấu hình, kiiaud sẽ tự động thiết lập quyền truy cập **`0600`** (`-rw-------`). Chỉ tài khoản của bạn mới có quyền đọc/ghi file này:
   ```bash
   chmod 600 ~/.config/kiiaud/config.toml
   ```

2. **Với quản trị viên hệ thống (`system-wide`)**:
   File `/etc/kiiaud/config.toml` nên được gán cho user chạy dịch vụ (`kiiaud`) và nhóm `audio`:
   ```bash
   sudo chown kiiaud:audio /etc/kiiaud/config.toml
   sudo chmod 640 /etc/kiiaud/config.toml
   ```

