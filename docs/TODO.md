# kiiaud — Danh sách công việc & Tiến độ triển khai

Tổng hợp từ báo cáo vận hành thực tế trên ARM SBC (`rpiser` – RPi5 & `tx3-mini-1` – Amlogic S905W, LAN 100Mb/s, DAC SMSL DL100) và các yêu cầu bổ sung cho Windows & Android.

## Quyết định phương án đã chốt
- **Mục 1 (HTTP pre-buffer):** **A2** — Thêm `stream_buffer_mb` (mặc định `0` = auto-scale ~8 giây theo bitrate, kẹp trong khoảng `8 MiB .. 64 MiB`; nếu `> 0` dùng giá trị cố định).
- **Mục 2 (Log xrun `errno=32`):** Log kèm trạng thái đệm (`stream_buffer_bytes`, `stream_buffer_capacity`, `%`) khi xảy ra `EPIPE`.
- **Mục 3 (Mặc định SBC & `kiiaud.service`):** **C2** — Giữ `200 / 40` trên desktop, mặc định `600 / 100` trên ARM (`aarch64` / `arm`); cập nhật `kiiaud.service` với `CPUSchedulingPolicy=fifo`, `CPUSchedulingPriority=50`, `Nice=-15`.
- **Mục 4 (`max_dsd_rate`):** **Đã bỏ qua** — SlimProto/LMS và LA Gateway không hỗ trợ khóa `MaxDSDRate` trong `HELO`, đồng thời `HELO` giới hạn số lượng `key=value`.
- **Mục 5 (Tự phục hồi ALSA `ENODEV`):** **E2** — Thử re-open 1–3 lần có backoff khi gặp `No such device (19)`; nếu vẫn mất thiết bị thì chờ lệnh phát/resume tiếp theo để re-open lại mà không cần restart service.
- **Mục 6 (Windows ẩn terminal):** **W1** — Dùng `windows_subsystem = "windows"` + `AttachConsole(ATTACH_PARENT_PROCESS)` cho CLI + `AllocConsole()`/`FreeConsole()` cho tính năng Show/Hide Console trên Tray.
- **Mục 7 (Android 32-bit):** Thêm target `armv7-linux-androideabi` (`armeabi-v7a`) vào code kiểm tra 32-bit & GitHub Actions `.github/workflows/release.yml`.

---

## Tiến độ thực hiện (Checklist)
- [x] **Bước 1 — Cấu hình & Telemetry Buffer (`kiiaud-core`, `kiiaud-decoder`, `kiiaud-playback`, `kiiaud-adapter-web`):**
  - Thêm `stream_buffer_mb` (mặc định `0` = auto) vào `config.rs` + WebUI.
  - Cài đặt **C2**: mặc định `buffer_time_ms = 600`, `period_time_ms = 100` trên `arm`/`aarch64`, giữ `200 / 40` trên x86_64/desktop.
  - Cập nhật `kiiaud-decoder/src/stream.rs` hỗ trợ dung lượng buffer động theo `stream_buffer_mb` / auto-scale theo bitrate + expose telemetry `StreamTelemetry` trong `kiiaud-core::telemetry`.
- [x] **Bước 2 — ALSA Diagnostics & Recovery (`kiiaud-output-alsa`, `kiiaud-playback`, `kiiaud/src/service.rs`, `contrib/systemd/`):**
  - Truyền telemetry buffer xuống ALSA output để log `stream_buffer_bytes`, `stream_buffer_capacity`, `stream_buffer_pct` khi xảy ra `EPIPE` (`errno=32`).
  - Cài đặt **E2**: retry re-open `1..=3` lần có backoff khi gặp `ENODEV (19)`, và xóa handle cũ để lệnh phát mới re-open sạch.
  - Cập nhật template `kiiaud.service` (`CPUSchedulingPolicy=fifo`, `CPUSchedulingPriority=50`, `Nice=-15`) và ghi chú cho user service (`kiiaud-user.service`, `service.rs`).
- [~] **Bước 3 — LMS/LAG `max_dsd_rate` (`kiiaud-adapter-lms`):**
  - Đã hủy/gỡ bỏ `MaxDSDRate` khỏi `HELO` và `config.rs` do LMS và LA Gateway không xử lý khóa này và `HELO` có giới hạn số lượng `key=value`.
- [x] **Bước 4 — Windows Subsystem & Console (`kiiaud/src/main.rs`, `tray.rs`):**
  - Cài đặt **W1**: `windows_subsystem = "windows"`, `AttachConsole(ATTACH_PARENT_PROCESS)` khi chạy từ terminal, `AllocConsole`/`ShowWindow` cho Tray Show/Hide Console. *(Lưu ý: cần CI Windows MSVC xác minh vì host Linux không có `x86_64-w64-mingw32-gcc`).*
- [x] **Bước 5 — Android 32-bit & GitHub Actions (`kiiaud-output-android`, `.github/workflows/release.yml`):**
  - Thêm `#[cfg(target_pointer_width = "64")]` cho test kích thước ioctl 64-bit trong `kiiaud-output-android`.
  - Cập nhật `.github/workflows/release.yml` để build & phát hành bản Android 32-bit (`armv7-linux-androideabi` / `armeabi-v7a`).
- [x] **Bước 6 — Kiểm thử & Xác minh:**
  - Chạy `cargo test --workspace` sau chỉnh sửa cuối cùng.
