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
| **Local Dev** | `./kiiaud.toml` | `.\kiiaud.toml` | Tiện lợi khi phát triển hoặc chạy portable trực tiếp từ repo |

### 1.2. Thứ tự ưu tiên & Cơ chế nạp cấu hình

**Quy tắc ưu tiên từ cao xuống thấp:**
```
1. [ƯU TIÊN TUYỆT ĐỐI] Các tham số Dòng lệnh (CLI Flags: --device, --dsd-mode, --bit-perfect, ...)
2. File cấu hình chỉ định tường minh (--config <FILE>)
3. Cấu hình Per-User (~/.config/kiiaud/config.toml)
4. Cấu hình System-wide (/etc/kiiaud/config.toml)
5. File cục bộ thư mục hiện tại (./kiiaud.toml)
6. Cấu hình mặc định trong mã nguồn (Built-in Defaults)
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
(Có cờ --system hoặc root)       (Chạy bình thường dưới quyền User)
                        /                       \
                       ▼                         ▼
┌───────────────────────────┐   ┌──────────────────────────────────────────────┐
│  SYSTEM-WIDE MODE         │   │  PER-USER MODE (Hierarchy Deep Merge)        │
│  Chỉ đọc:                 │   │  1. Đọc /etc/kiiaud/config.toml (làm base)   │
│  /etc/kiiaud/config.toml  │   │  2. Đọc ~/.config/kiiaud/config.toml (user)  │
└───────────────────────────┘   │     và ghi đè (merge) lên cấu hình hệ thống   │
                                │  3. Nếu không có cả hai: thử ./kiiaud.toml   │
                                │  4. Môi trường mới: tự tạo file user mẫu     │
                                └──────────────────────────────────────────────┘
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

---

### 2.2. Section `[output]`
Cấu hình phần cứng âm thanh và cơ chế phát nhạc Bit-Perfect.

| Tham số | Kiểu | Mặc định | Mô tả |
| :--- | :--- | :--- | :--- |
| `backend` | String | `"alsa"` (Linux)<br>`"wasapi"` (Win)<br>`"oboe"` (Android) | Backend xuất âm thanh. Các giá trị hợp lệ:<br>- `"alsa"`: Giao tiếp ALSA hardware-direct trên Linux (bypass hoàn toàn PulseAudio/PipeWire mixer).<br>- `"wasapi"`: Windows Audio Session API (chạy Exclusive Mode).<br>- `"android"` hoặc `"oboe"`: AAudio/AudioTrack direct trên Android.<br>- `"mock"` hoặc `"null"`: Dummy backend phục vụ test/CI không cần DAC. |
| `device` | String | `"hw:CARD=Audio,DEV=0"` | Định danh thiết bị xuất phần cứng.<br>- Trên Linux ALSA: Dùng cú pháp `hw:CARD=<TênCard>,DEV=0` hoặc `hw:<IndexCard>,<IndexDev>`. Xem danh sách card qua lệnh `aplay -l`.<br>- Trên Windows: Tên chuỗi con của DAC (vd `"DAC"`) hoặc `"default"`. |
| `bit_perfect` | Boolean | `true` | **Cực kỳ quan trọng**: Khi bật (`true`), kiiaud vô hiệu hóa hoàn toàn mọi thuật toán DSP phần mềm (không resample, không đổi bit depth, không chỉnh volume số làm tròn bit). DAC luôn được cấu hình đúng tần số lấy mẫu gốc của file âm thanh (từ 44.1kHz đến 768kHz, DSD64-DSD512). |
| `dsd_mode` | String | `"auto"` | Cơ chế truyền tải tín hiệu DSD (Direct Stream Digital):<br>- `"auto"`: Thử Native DSD trước (`DSD_U32_BE`/`LE`). Nếu DAC phần cứng không hỗ trợ Native DSD, tự động fallback sang DoP (DSD over PCM). Không bao giờ tự ý convert DSD sang PCM.<br>- `"native"`: Bắt buộc dùng Native DSD. Nếu DAC không hỗ trợ sẽ báo lỗi.<br>- `"dop"`: Đóng gói các bit DSD vào khung PCM 24/32-bit theo chuẩn DoP v1.1. |
| `buffer_time_ms` | Integer (u32) | `200` | Kích thước ring-buffer của phần cứng (đơn vị: mili-giây). Giúp tránh hiện tượng giật tiếng (buffer underrun) khi CPU bận. |
| `period_time_ms` | Integer (u32) | `40` | Chu kỳ ngắt phần cứng ALSA (đơn vị: mili-giây). |
| `hardware_mixer` | Option\<String\> | `null` (commented) | Tên bộ điều khiển volume analog bằng phần cứng của DAC (ví dụ: `"PCM"`, `"Master"`). Nếu để trống, âm lượng phần cứng được giữ nguyên 100% để đảm bảo tín hiệu bit-perfect. |

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
Client kết nối tới Logitech Media Server / Lyrion Music Server (giao thức SlimProto).

| Tham số | Kiểu | Mặc định | Mô tả |
| :--- | :--- | :--- | :--- |
| `enabled` | Boolean | `false` | Bật/tắt client LMS/Lyrion. Giả lập một Squeezebox player hoàn chỉnh. |
| `server_host` | String | `"192.168.1.100"` | Địa chỉ IP hoặc hostname của LMS server. |
| `server_port` | Integer (u16) | `3483` | Cổng SlimProto TCP của LMS server. |
| `http_port` | Integer (u16) | `9000` | Cổng HTTP Web của LMS để stream âm thanh. |
| `player_name` | String | `"kiiaud Squeezebox"` | Tên player hiển thị trong giao diện LMS / Material Skin. |
| `mac_address` | Array of 6 bytes | `[2, 75, 73, 73, 65, 85]` | Địa chỉ MAC ảo để LMS nhận diện và phân biệt player (`02:4B:49:49:41:55`). |

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
| **Audio Output** | `-b, --backend <BACKEND>` | `"alsa"`, `"wasapi"`, `"mock"` | `[output].backend` |
| | `-d, --device <DEVICE>` | `"hw:CARD=Audio,DEV=0"` | `[output].device` |
| | `--bit-perfect [<BOOL>]` | `true` hoặc `false` | `[output].bit_perfect` |
| | `--dsd-mode <MODE>` | `"auto"`, `"native"`, `"dop"` | `[output].dsd_mode` |
| | `--buffer-time-ms <MS>` | Integer (`200`) | `[output].buffer_time_ms` |
| | `--period-time-ms <MS>` | Integer (`40`) | `[output].period_time_ms` |
| | `--hardware-mixer <NAME>` | `"PCM"`, `"Master"`, `"none"` | `[output].hardware_mixer` |
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
| **LMS Adapter** | `--lms [<BOOL>]` | `true` hoặc `false` | `[adapter.lms].enabled` |
| | `--lms-host <HOST>` | `"192.168.1.50"` | `[adapter.lms].server_host` |
| | `--lms-port <PORT>` | Integer (`3483`) | `[adapter.lms].server_port` |
| | `--lms-http-port <PORT>` | Integer (`9000`) | `[adapter.lms].http_port` |
| | `--lms-player-name <NAME>` | String | `[adapter.lms].player_name` |
| | `--lms-mac <MAC>` | `"02:4B:49:49:41:55"` | `[adapter.lms].mac_address` |

### 3.2. Ví dụ thực tế kết hợp Dòng lệnh

```bash
# 1. Chạy với file config có sẵn nhưng ghi đè thiết bị DAC và chế độ DSD DoP:
kiiaud -d "hw:1,0" --dsd-mode dop

# 2. Tắt adapter WebUI và MPD trực tiếp từ dòng lệnh:
kiiaud --web false --mpd false

# 3. Kích hoạt và kết nối nhanh tới Jellyfin Server mà không cần mở file cấu hình:
kiiaud --jellyfin --jellyfin-url "http://192.168.1.10:8096" --jellyfin-api-key "secret-key-123"

# 4. Kiểm tra cấu hình kết hợp các cờ ghi đè (không khởi động server):
kiiaud --check-config -d "hw:0,0" --bit-perfect false

# 5. Xuất ra file TOML mới từ cấu hình hiện tại kết hợp ghi đè dòng lệnh:
kiiaud --print-config -d "hw:CARD=Audio,DEV=0" --lms --lms-host "192.168.1.80" > /tmp/resolved.toml
```

---

## 4. Bảo mật File Cấu hình (Security & File Permissions)

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

