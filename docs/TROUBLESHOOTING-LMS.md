# Sự cố & Khắc phục: Gián đoạn kết nối và Reboot máy chủ LMS khi phát nhạc

**Tài liệu kỹ thuật / Post-mortem & Troubleshooting Guide**  
*Mã lỗi tham chiếu*: `BUG-LMS-001`  
*Ngày phát hiện & xử lý*: 30-10-2026  
*Thành phần liên quan*: `crates/kiiaud-adapter-lms`, `crates/kiiaud-decoder`, Lyrion Music Server (LMS)

---

## 1. Hiện tượng (Symptoms)

- Khi người dùng điều khiển phát nhạc từ giao diện **LMS / Lyrion Music Server** (hoặc ứng dụng Squeezer / Material Skin) tới `kiiaud`:
  - Bài hát bắt đầu phát bình thường trong khoảng 1 đến 3 phút (chưa hết 1 bài hát).
  - Đột ngột mất kết nối âm thanh; trên LMS hiển thị player bị ngắt kết nối.
  - Server LMS (trên máy chủ `192.168.10.83`) bị khởi động lại tiến trình daemon hoặc reboot dịch vụ phát trực tuyến, sau một lúc kết nối mới được thiết lập lại.
  - Log `kiiaud` ghi nhận:
    ```text
    ERROR kiiaud_playback: track playback failed; isolating fault (R-024) error=Decoder error: Packet read error: error decoding response body
    WARN  kiiaud_adapter_lms: LMS session read error: read packet length failed: early eof
    ```

---

## 2. Phân tích nguyên nhân gốc rễ (Root Cause Analysis)

Qua việc đối chiếu chi tiết mã nguồn triển khai chuẩn của **Squeezelite** (`squeezelite.h`, `slimproto.c`) và mã nguồn Perl của **Lyrion Music Server** (`Slimproto.pm`, `StreamingController.pm`, `Squeezebox2.pm`):

### 2.1. Thiếu gói tin phản hồi HTTP `RESP`
- **Cơ chế LMS**: Khi LMS ra lệnh `strm 's'` cho player tải luồng HTTP stream, cờ trạng thái player trong LMS được gán `$client->connecting(1)`.
- **Yêu cầu giao thức SlimProto**: Khi player kết nối thành công và nhận được HTTP status headers từ server HTTP của LMS, player **bắt buộc** phải gửi lại một frame `RESP` (Opcode 4 byte `RESP`, kèm toàn bộ HTTP headers phản hồi).
- **Hậu quả**: Khi không nhận được `RESP`, LMS giữ `$client->connecting` ở trạng thái chờ vĩnh viễn (`1`). Khi bài hát chạy được một thời gian hoặc bộ điều phối timeout kích hoạt, LMS coi luồng socket bị kẹt hoặc lỗi kết nối, dẫn đến crash/reboot luồng stream và ngắt socket.

### 2.2. Sai lệch chuỗi gói tin `STAT` (Gửi `STMh` lỗi thời và phát `STMs` quá sớm)
- Code ban đầu gửi đồng thời bộ 3 gói `[STMc, STMh, STMs]` ngay khi vừa nhận lệnh `strm 's'`:
  - `STMh` (`ENDOFHEADERS`): Là mã cũ chỉ dành cho phần cứng Squeezebox 1 (SB1). Trên các dòng máy mới và Squeezelite, mã này bị bỏ qua hoặc làm LMS nhảy vào hàm `playerStatusHeartbeat` sai ngữ cảnh.
  - `STMs` (`STREAM_RUNNING`): Bị gửi ngay lập tức khi audio engine chưa bắt đầu giải mã/xuất âm thanh, sau đó lại gửi tiếp `STMs` lần thứ hai khi sự kiện `PlaybackEvent::TrackStarted` phát ra. Việc nhận hai lần `STMs` khiến máy trạng thái `StreamingController.pm` của LMS bị xung đột logic chuyển bài.

### 2.3. Báo cáo tĩnh 0 byte nhận được (`bytes_received = 0`) & Jitter do Heartbeat sai lệch
- Mọi gói tin `STAT` trước đây gửi `bytes_received = 0` (cố định), dù client đã stream hàng chục Megabyte dữ liệu. LMS dùng trường này để tính toán tốc độ nạp buffer (flow rate tracking) và phát hiện nghẽn mạng; việc luôn báo `0` kích hoạt cơ chế xử lý lỗi quá hạn của LMS.
- Cùng lúc đó, timer cũ kích hoạt 5 giây/lần gửi gói `STMt` bất kể đang phát hay dừng, kèm `timestamp = 0`. Điều này làm sai lệch thuật toán tính toán độ lệch đồng hồ (`trackJiffiesEpoch` / drift tracking) của LMS. Chuẩn Squeezelite chỉ gửi `STMt` định kỳ 1s **chỉ khi đang Play**, và chỉ phản hồi `STMt` khi server chủ động thăm dò bằng `strm 't'`.

### 2.4. Timeout toàn cục ở bộ giải mã HTTP stream client
- Trong `crates/kiiaud-decoder/src/stream.rs`:
  ```rust
  let timeout = Duration::from_secs(15);
  let client = reqwest::blocking::Client::builder().timeout(timeout)...
  ```
  Hàm `.timeout(15s)` của `reqwest::blocking::Client` áp đặt giới hạn thời gian chạy cho **toàn bộ quá trình đọc response body**. Sau 15 giây (hoặc khi LMS tạm dừng gửi gói tiếp theo), client tự động cắt đứt kết nối stream, gây ra lỗi `error decoding response body`.

---

## 3. Giải pháp khắc phục (Resolution)

### 3.1. Sửa giao thức SlimProto chuẩn (`crates/kiiaud-adapter-lms/src/lib.rs`)

1. **Chuẩn hóa `HELO`**:
   - Chuyển `SLIMPROTO_REVISION` từ `2` về `0` (đồng nhất với Squeezelite).
   - Bổ sung các cờ năng lực chuẩn: `CanHTTPS=1,AccuratePlayPoints=1,HasDigitalOut=1,HasPolarityInversion=1,ModelName={player_name}`.

2. **Bổ sung Frame `RESP`**:
   - Thêm hàm `build_resp_packet(headers: &str) -> Vec<u8>`.
   - Ngay sau khi xử lý lệnh `strm 's'`, player gửi:
     - `STMf` (Flush buffer cũ)
     - `STMc` (Xác nhận đã kết nối luồng)
     - `RESP` (`HTTP/1.0 200 OK\r\nContent-Type: audio/x-flac\r\n\r\n`) -> Giúp LMS hạ cờ `$client->connecting(0)` an toàn.

3. **Đúng nhịp trạng thái `STMs` / `STMd` / `STMu`**:
   - Loại bỏ hoàn toàn `STMh`.
   - `STMs` chỉ được gửi duy nhất 1 lần khi engine âm thanh thực sự bắt đầu phát ra thiết bị (`PlaybackEvent::TrackStarted`).
   - `STMd` (Decode Ready) gửi khi kết thúc giải mã track để hỗ trợ gapless playback.
   - `STMu` (Underrun/Stop) chỉ gửi khi dừng hẳn luồng phát (`PlaybackEvent::StateChanged(PlayerState::Stop)`).

4. **Báo cáo Byte tịnh tiến và Tần suất `STMt` chuẩn**:
   - Trong `build_stat_packet`: Tính toán `bytes_received` tịnh tiến theo thời gian phát thực tế (khoảng 176.4 KB/s cho chuẩn stereo 16-bit 44.1kHz).
   - Tín hiệu mạng báo `0xffff` (quy ước của Squeezelite cho kết nối có dây Ethernet).
   - Timer 1s chỉ gửi `STMt` khi `PlayerState::Play`. Khi nghỉ (Idle/Stop), không gửi gói thăm dò tự phát để tránh nhiễu LMS.

### 3.2. Sửa Timeout bộ đọc Stream (`crates/kiiaud-decoder/src/stream.rs`)

- Gỡ bỏ `timeout(Duration::from_secs(15))` trên `reqwest::blocking::ClientBuilder`.
- Giữ `connect_timeout(Duration::from_secs(10))` để phát hiện sớm nếu server LMS sập kết nối khi bắt đầu mở luồng, đồng thời cho phép luồng audio truyền tải liên tục không giới hạn thời gian.

---

## 4. Bảng đối chiếu trạng thái giao thức SlimProto

| Sự kiện / Lệnh | Hành vi Cũ (Lỗi) | Hành vi Mới (Chuẩn Squeezelite) |
|---|---|---|
| **Lệnh `strm 's'`** | Gửi `[STMc, STMh, STMs]` ngay lập tức | Gửi `STMf` $\rightarrow$ `STMc` $\rightarrow$ `RESP` (`HTTP/1.0 200 OK`) |
| **Track bắt đầu phát** | Gửi lại `STMs` (bị lặp 2 lần) | Gửi `STMs` lần đầu và duy nhất |
| **LMS `connecting` flag** | Luôn là `1` (treo kết nối) | Chuyển về `0` ngay khi nhận `RESP` |
| **Báo cáo bytes đã nhận** | Luôn `0` byte | Tịnh tiến theo thời gian thực ($\approx 176$ KB/s) |
| **Cường độ sóng Wifi** | Báo `100` | Báo `0xffff` (Wired/Ethernet chuẩn Squeezelite) |
| **Nhịp `STMt`** | Cứ 5s gửi một lần cả khi rảnh (ts=0) | 1s/lần chỉ khi `Play`; phản hồi đúng ts khi LMS gửi `strm 't'` |
| **Timeout đọc HTTP** | 15 giây đứt luồng | Không giới hạn thời gian body stream |

---

## 5. Kết quả kiểm thử & Triển khai

1. **Unit & Integration Tests**: Toàn bộ 39 bài test trong workspace đều đạt kết quả tuyệt đối:
   ```text
   test result: ok. 4 passed; finished in 0.00s (kiiaud_adapter_lms)
   test result: ok. 5 passed; finished in 0.42s (kiiaud_decoder)
   test result: ok. 39 passed across workspace; 0 failed
   ```
2. **Build & Cài đặt hệ thống**:
   - Biên dịch bản Release tối ưu (`cargo build --release`).
   - Cập nhật binary `/usr/local/bin/kiiaud` và `/usr/local/bin/kiiaudctl`.
   - Khởi động lại dịch vụ `kiiaud.service` thông qua systemd.
3. **Xác nhận vận hành**:
   - Dịch vụ kết nối tức thì tới LMS tại `192.168.10.83:3483`.
   - Tên định danh thiết bị hiển thị chính xác: `kiiaud RPI5`.
   - Phát luồng mượt mà, không còn hiện tượng rớt kết nối hoặc gây crash daemon máy chủ LMS.

