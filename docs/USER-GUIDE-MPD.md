# Hướng Dẫn Sử Dụng: MPD Adapter như Universal Playlist & Transport Controller

## 1. Triết lý thiết kế (Zero-DB / Pure In-Memory Session Queue)

Thông thường, máy chủ MPD truyền thống đòi hỏi quét toàn bộ ổ đĩa, phân tích thẻ ID3 và lưu vào một tệp cơ sở dữ liệu (database cache). Việc này gây tốn RAM, làm chậm quá trình khởi động và dễ lỗi chỉ mục khi quản lý các thư viện nhạc DSD/Hi-Res dung lượng hàng terabyte.

Trong `kiiaud`, **MPD Adapter đóng vai trò là một "Universal Remote Controller" cho phiên nghe nhạc hiện tại**:
- **Không database**: `kiiaud` khởi động tức thì chỉ trong vài mili-giây, không tốn tài nguyên nền.
- **Hàng đợi bộ nhớ dùng chung (Shared In-Memory Queue)**: Tất cả các nguồn nhạc (quét thư mục cục bộ, Jellyfin "Play To", LMS Squeezebox, WebUI, CLI) đều hội tụ về một danh sách phát duy nhất.
- **Tận dụng toàn bộ hệ sinh thái MPD**: Bạn có thể dùng `ncmpcpp`, `mpc`, phím tắt bàn phím, app điện thoại (`MPDroid`, `MAFA`, `Rigelian`), widget màn hình hay Stream Deck để điều khiển toàn diện.

---

## 2. Cấu hình nhanh (`kiiaud.toml`)

Thêm hoặc chỉnh sửa phần `[adapter.mpd]` trong file cấu hình (thường tại `~/.config/kiiaud/config.toml` hoặc `/etc/kiiaud/config.toml`):

```toml
[adapter.mpd]
enabled = true
listen = "0.0.0.0:6600"

# (Tùy chọn) Thư mục nhạc gốc cục bộ hoặc ổ NAS đã mount
# Dùng để duyệt cây thư mục qua lsinfo và hỗ trợ lệnh add với đường dẫn tương đối
music_directory = "/mnt/music"
```

Khởi động daemon:
```bash
kiiaud
```

---

## 3. Nạp nhạc vào Playlist (Queue Ingestion)

Bạn có nhiều cách cực kỳ linh hoạt để ném nhạc vào danh sách phát:

### Cách 1: Ném nguyên cả Album / Thư mục bằng `mpc`
Nhờ tính năng quét đệ quy (recursive scanning), bạn có thể ném toàn bộ thư mục chỉ bằng một lệnh:
```bash
# Ném một thư mục tuyệt đối
mpc add "/mnt/music/Jazz/Miles Davis - Kind of Blue/"

# Nếu đã cấu hình music_directory = "/mnt/music", có thể dùng đường dẫn tương đối:
mpc add "Jazz/Miles Davis - Kind of Blue"

# Phát ngay bài đầu tiên vừa nạp
mpc play
```
*Tất cả các định dạng âm thanh chuẩn audiophile (`.flac`, `.wv` WavPack PCM & DSD, `.dsf`, `.dff`, `.wav`, `.aiff`, `.mp3`, `.opus`, `.ogg`, `.alac`) sẽ tự động được lọc, sắp xếp theo đúng thứ tự file và đưa vào hàng đợi.*

### Cách 2: Sử dụng CLI nội bộ `kiiaudctl`
Nếu không cài đặt công cụ `mpc`, bạn có thể dùng chính công cụ đi kèm của `kiiaud`:
```bash
# Thêm file đơn lẻ
kiiaudctl add /mnt/music/track1.flac

# Thêm cả thư mục album
kiiaudctl add "/mnt/music/Jazz/John Coltrane - Blue Train/"

# Xem danh sách phát hiện tại
kiiaudctl queue
```

### Cách 3: Nạp danh sách từ Jellyfin hoặc LMS
* **Jellyfin**: Mở web hoặc app Jellyfin trên điện thoại/TV -> Chọn Album hoặc Playlist yêu thích -> Bấm biểu tượng Cast ("Play On") -> Chọn **`kiiaud`**. Toàn bộ bài hát sẽ được nạp vào hàng đợi và client MPD của bạn sẽ lập tức hiển thị danh sách này!
* **Web UI (Port 7110)**: Mở trình duyệt tại `http://localhost:7110` -> Dùng trình duyệt file tích hợp kéo thả hoặc bấm Add để nạp nhạc.

---

## 4. Duyệt thư mục trực tiếp không cần DB (`lsinfo`)

Các ứng dụng MPD như `ncmpcpp`, `Cantata`, `MPDroid` có tab duyệt file (File Browser). `kiiaud` hỗ trợ lệnh `lsinfo` duyệt trực tiếp cây thư mục ổ đĩa theo cấu hình `music_directory`:
1. Mở `ncmpcpp`.
2. Bấm phím `2` hoặc `3` để vào tab **Browse Files**.
3. Bạn sẽ thấy danh sách các thư mục con và file nhạc thực tế trên đĩa cứng.
4. Bấm phím **Space** hoặc phím **A** để thêm nhanh folder vào Playlist của phiên nghe nhạc.

---

## 5. Thao tác và Sắp xếp Playlist

Hệ thống hỗ trợ đầy đủ các lệnh thay đổi trật tự bài hát:

| Thao tác | Lệnh `mpc` | Lệnh `kiiaudctl` | Phím tắt trong `ncmpcpp` |
| :--- | :--- | :--- | :--- |
| **Đổi vị trí bài** | `mpc move 5 1` *(chuyển bài ở vị trí 5 lên vị trí 1)* | `kiiaudctl move 5 1` | `Ctrl + V` hoặc `M` |
| **Xáo trộn danh sách** | `mpc shuffle` | `kiiaudctl shuffle` | `z` hoặc `Ctrl + S` |
| **Xoá bài khỏi queue**| `mpc del 3` *(xoá bài thứ 3)* | - | `d` |
| **Xoá trắng danh sách**| `mpc clear` | `kiiaudctl clear` | `c` |
| **Chế độ phát** | `mpc repeat on`, `mpc random on` | - | `r`, `s` |

> [!NOTE]
> Khi thực hiện `shuffle` hoặc di chuyển bài hát (`move`, `swap`), vị trí con trỏ của bài hát đang phát sẽ được bảo toàn chính xác, bài nhạc đang phát không bị gián đoạn hay ngắt quãng.

---

## 6. Gợi ý thiết lập cho Audiophile (Tips & Workflows)

### Phím tắt toàn hệ thống (i3 / Sway / Hyprland)
Thêm vào file cấu hình Window Manager (ví dụ `~/.config/hypr/hyprland.conf` hoặc `~/.config/i3/config`):
```text
bindsym XF86AudioPlay exec mpc toggle
bindsym XF86AudioNext exec mpc next
bindsym XF86AudioPrev exec mpc prev
bindsym XF86AudioStop exec mpc stop
```

### Sử dụng ncmpcpp giao diện tối giản
Tạo file `~/.config/ncmpcpp/config`:
```text
mpd_host = "localhost"
mpd_port = "6600"
user_interface = "classic"
playlist_display_mode = "columns"
browser_display_mode = "columns"
autocenter_mode = "yes"
```
Mỗi khi khởi động máy, bạn chỉ cần gõ `ncmpcpp` trong terminal là có ngay một màn hình điều khiển phát nhạc bit-perfect Hi-Res cực kỳ chuyên nghiệp và mượt mà.

