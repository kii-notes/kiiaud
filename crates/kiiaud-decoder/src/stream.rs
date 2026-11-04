// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP/HTTPS and raw TCP stream `MediaSource` implementations for Symphonia.
//!
//! Includes an asynchronous background pre-buffering reader (`PrebufferedStreamReader`)
//! with a dedicated 4 MiB RAM ring-buffer to completely decouple real-time audio decoding
//! and WASAPI/ALSA output from network RTT latency and Wi-Fi/LAN jitter.

use kiiaud_core::{KiiaudError, Result, strip_uri_auth};
use parking_lot::{Condvar, Mutex};
use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;
use symphonia::core::io::MediaSource;

/// Capacity of the network pre-buffering RAM ring-buffer (4 MiB $\approx$ 20-40 seconds of 16/44.1 FLAC).
pub const STREAM_BUFFER_CAPACITY: usize = 4 * 1024 * 1024;

/// Initial watermark bytes before playback starts to ensure glitch-free initial playback (128 KiB).
pub const INITIAL_PREBUFFER_BYTES: usize = 128 * 1024;

/// Real-time telemetry snapshot of the active streaming buffer.
#[derive(Debug, Clone, Copy, Default)]
pub struct StreamTelemetry {
    pub total_bytes_read: u64,
    pub buffer_occupancy: u32,
    pub buffer_capacity: u32,
}

static STREAM_TELEMETRY: OnceLock<Mutex<StreamTelemetry>> = OnceLock::new();

/// Retrieve current real-time streaming buffer telemetry (used by `kiiaud-adapter-lms` for STAT packets).
pub fn get_stream_telemetry() -> StreamTelemetry {
    STREAM_TELEMETRY
        .get_or_init(|| Mutex::new(StreamTelemetry::default()))
        .lock()
        .clone()
}

/// Reset stream telemetry (called when starting a new stream or receiving `STMf` flush).
pub fn reset_stream_telemetry() {
    let cell = STREAM_TELEMETRY.get_or_init(|| Mutex::new(StreamTelemetry::default()));
    *cell.lock() = StreamTelemetry::default();
}

fn update_global_telemetry(total: u64, occupancy: u32, capacity: u32) {
    let cell = STREAM_TELEMETRY.get_or_init(|| Mutex::new(StreamTelemetry::default()));
    let mut lock = cell.lock();
    lock.total_bytes_read = total;
    lock.buffer_occupancy = occupancy;
    lock.buffer_capacity = capacity;
}

/// Internal state shared between background producer thread and consumer `Read` calls.
struct PrebufferState {
    buffer: VecDeque<u8>,
    total_bytes_read: u64,
    eof: bool,
    error: Option<String>,
    initial_prebuffered: bool,
}

/// Dedicated asynchronous stream pre-buffer reader.
/// Continuously fetches stream bytes in a background worker thread and buffers them in RAM.
pub struct PrebufferedStreamReader {
    state: Arc<Mutex<PrebufferState>>,
    condvar: Arc<Condvar>,
    stop_flag: Arc<AtomicBool>,
}

impl PrebufferedStreamReader {
    pub fn new(mut reader: Box<dyn Read + Send>) -> Self {
        reset_stream_telemetry();

        let state = Arc::new(Mutex::new(PrebufferState {
            buffer: VecDeque::with_capacity(INITIAL_PREBUFFER_BYTES * 2),
            total_bytes_read: 0,
            eof: false,
            error: None,
            initial_prebuffered: false,
        }));
        let condvar = Arc::new(Condvar::new());
        let stop_flag = Arc::new(AtomicBool::new(false));

        let state_clone = Arc::clone(&state);
        let condvar_clone = Arc::clone(&condvar);
        let stop_clone = Arc::clone(&stop_flag);

        let _ = thread::Builder::new()
            .name("kiiaud-stream-buf".to_owned())
            .spawn(move || {
                let mut chunk = vec![0u8; 64 * 1024]; // 64 KiB I/O buffer
                loop {
                    if stop_clone.load(Ordering::Relaxed) {
                        break;
                    }

                    match reader.read(&mut chunk) {
                        Ok(0) => {
                            let mut st = state_clone.lock();
                            st.eof = true;
                            st.initial_prebuffered = true;
                            update_global_telemetry(
                                st.total_bytes_read,
                                st.buffer.len() as u32,
                                STREAM_BUFFER_CAPACITY as u32,
                            );
                            condvar_clone.notify_all();
                            break;
                        }
                        Ok(n) => {
                            let mut st = state_clone.lock();
                            while st.buffer.len() + n > STREAM_BUFFER_CAPACITY
                                && !stop_clone.load(Ordering::Relaxed)
                            {
                                condvar_clone.wait_for(&mut st, Duration::from_millis(50));
                            }

                            if stop_clone.load(Ordering::Relaxed) {
                                break;
                            }

                            st.buffer.extend(&chunk[..n]);
                            st.total_bytes_read += n as u64;
                            if st.buffer.len() >= INITIAL_PREBUFFER_BYTES {
                                st.initial_prebuffered = true;
                            }
                            update_global_telemetry(
                                st.total_bytes_read,
                                st.buffer.len() as u32,
                                STREAM_BUFFER_CAPACITY as u32,
                            );
                            condvar_clone.notify_all();
                        }
                        Err(e) => {
                            if e.kind() == io::ErrorKind::Interrupted {
                                continue;
                            }
                            let mut st = state_clone.lock();
                            st.error = Some(e.to_string());
                            st.eof = true;
                            st.initial_prebuffered = true;
                            condvar_clone.notify_all();
                            break;
                        }
                    }
                }
            });

        Self {
            state,
            condvar,
            stop_flag,
        }
    }
}

impl Read for PrebufferedStreamReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let mut st = self.state.lock();

        // Wait for initial pre-buffer watermark before yielding the first batch of frames
        while !st.initial_prebuffered
            && !st.eof
            && st.error.is_none()
            && !self.stop_flag.load(Ordering::Relaxed)
        {
            self.condvar.wait_for(&mut st, Duration::from_millis(50));
        }

        // Wait if buffer is empty but stream is still running
        while st.buffer.is_empty()
            && !st.eof
            && st.error.is_none()
            && !self.stop_flag.load(Ordering::Relaxed)
        {
            self.condvar.wait(&mut st);
        }

        if !st.buffer.is_empty() {
            let to_read = buf.len().min(st.buffer.len());
            let (s1, s2) = st.buffer.as_slices();
            let s1_len = s1.len().min(to_read);
            buf[..s1_len].copy_from_slice(&s1[..s1_len]);
            let rem = to_read - s1_len;
            if rem > 0 {
                buf[s1_len..to_read].copy_from_slice(&s2[..rem]);
            }
            st.buffer.drain(0..to_read);
            update_global_telemetry(
                st.total_bytes_read,
                st.buffer.len() as u32,
                STREAM_BUFFER_CAPACITY as u32,
            );
            self.condvar.notify_all();
            return Ok(to_read);
        }

        if let Some(ref err) = st.error {
            return Err(io::Error::new(io::ErrorKind::Other, err.clone()));
        }

        if st.eof {
            return Ok(0);
        }

        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Stream prebuffer interrupted",
        ))
    }
}

impl Drop for PrebufferedStreamReader {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        self.condvar.notify_all();
    }
}

/// Internal header key injected by `kiiaud-adapter-lms` when streaming via Lyrion Anywhere Gateway.
/// Carries `<session_id>:<stream_seq>` so `open_http_stream` can emit periodic byte acknowledgements.
pub const ANYWHERE_STREAM_META_HEADER: &str = "X-Kiiaud-Anywhere-Stream";
/// Internal header key injected by `kiiaud-adapter-lms` carrying a human-readable format hint (`flac`, `wv`, `mp3`, `opus`, etc.).
pub const ANYWHERE_FORMAT_HINT_HEADER: &str = "X-Kiiaud-Format-Hint";

/// Acknowledge stream bytes to Lyrion Anywhere Gateway at least every 256 KiB.
const ANYWHERE_ACK_EVERY_BYTES: u64 = 256 * 1024;

type AckCallback = Arc<dyn Fn(&str, u64, u64) + Send + Sync>;

static ANYWHERE_ACK_HANDLER: OnceLock<Mutex<Option<AckCallback>>> = OnceLock::new();

/// Register a callback invoked by `open_http_stream` whenever a Lyrion Anywhere Gateway stream
/// advances by `ANYWHERE_ACK_EVERY_BYTES` (or finishes).
pub fn set_anywhere_ack_callback<F>(cb: F)
where
    F: Fn(&str, u64, u64) + Send + Sync + 'static,
{
    let cell = ANYWHERE_ACK_HANDLER.get_or_init(|| Mutex::new(None));
    *cell.lock() = Some(Arc::new(cb));
}

fn notify_anywhere_ack(session: &str, stream_seq: u64, total_bytes: u64) {
    if let Some(cell) = ANYWHERE_ACK_HANDLER.get()
        && let Some(cb) = cell.lock().clone()
    {
        cb(session, stream_seq, total_bytes);
    }
}

/// Reader wrapper that tracks cumulative bytes read and fires Lyrion Anywhere Gateway `{"ack":...}` notifications.
struct AnywhereAckReader<R> {
    inner: R,
    session: String,
    stream_seq: u64,
    total_read: u64,
    last_acked: u64,
}

impl<R: Read> Read for AnywhereAckReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.total_read += n as u64;
            if self.total_read - self.last_acked >= ANYWHERE_ACK_EVERY_BYTES {
                self.last_acked = self.total_read;
                notify_anywhere_ack(&self.session, self.stream_seq, self.total_read);
            }
        }
        Ok(n)
    }
}

/// A streaming non-seekable reader wrapped in a `Mutex` so it satisfies `MediaSource` (`Read + Seek + Send + Sync`).
pub struct StreamMediaSource {
    inner: Mutex<Box<dyn Read + Send>>,
    byte_len: Option<u64>,
}

impl StreamMediaSource {
    pub fn new(reader: Box<dyn Read + Send>, byte_len: Option<u64>) -> Self {
        Self {
            inner: Mutex::new(reader),
            byte_len,
        }
    }
}

impl Read for StreamMediaSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.lock().read(buf)
    }
}

impl Seek for StreamMediaSource {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        if matches!(pos, SeekFrom::Current(0)) {
            return Ok(0);
        }
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Network stream is not directly seekable",
        ))
    }
}

impl MediaSource for StreamMediaSource {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        self.byte_len
    }
}

/// Open an HTTP/HTTPS stream URL and return the `MediaSource` plus an optional extension hint.
pub fn open_http_stream(
    url: &str,
    headers: &HashMap<String, String>,
) -> Result<(Box<dyn MediaSource>, Option<String>)> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .user_agent("kiiaud/0.1")
        .build()
        .map_err(|e| {
            KiiaudError::Source(format!("Failed to build HTTP client: {}", e.without_url()))
        })?;

    let mut anywhere_meta: Option<(String, u64)> = None;
    let mut custom_format_hint: Option<String> = None;

    let mut req = client.get(url);
    for (k, v) in headers {
        if k.eq_ignore_ascii_case(ANYWHERE_STREAM_META_HEADER) {
            if let Some((sess, seq_str)) = v.rsplit_once(':')
                && let Ok(seq) = seq_str.parse::<u64>()
            {
                anywhere_meta = Some((sess.to_owned(), seq));
            }
            continue;
        }
        if k.eq_ignore_ascii_case(ANYWHERE_FORMAT_HINT_HEADER) {
            if !v.trim().is_empty() {
                custom_format_hint = Some(v.trim().to_owned());
            }
            continue;
        }
        req = req.header(k.as_str(), v.as_str());
    }

    let resp = req.send().and_then(|r| r.error_for_status()).map_err(|e| {
        KiiaudError::Source(format!(
            "HTTP stream error for '{}': {}",
            strip_uri_auth(url),
            e.without_url()
        ))
    })?;

    let content_len = resp.content_length();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    let ext_hint = url_extension_hint(url)
        .or_else(|| content_type.as_deref().and_then(mime_to_ext))
        .or(custom_format_hint);

    tracing::info!(
        url = %strip_uri_auth(url),
        ?content_type,
        ?ext_hint,
        anywhere_stream = anywhere_meta.is_some(),
        "opened HTTP audio stream (with 4 MiB asynchronous RAM pre-buffering)"
    );

    let base_reader: Box<dyn Read + Send> = if let Some((session, stream_seq)) = anywhere_meta {
        Box::new(AnywhereAckReader {
            inner: resp,
            session,
            stream_seq,
            total_read: 0,
            last_acked: 0,
        })
    } else {
        Box::new(resp)
    };

    let prebuffered_reader: Box<dyn Read + Send> =
        Box::new(PrebufferedStreamReader::new(base_reader));

    Ok((
        Box::new(StreamMediaSource::new(prebuffered_reader, content_len)),
        ext_hint,
    ))
}

/// Open a raw TCP audio stream (e.g. LMS SlimProto raw HTTP/TCP request).
pub fn open_tcp_stream(
    host: &str,
    port: u16,
    request: &[u8],
    format_hint: Option<String>,
) -> Result<(Box<dyn MediaSource>, Option<String>)> {
    let addr = format!("{host}:{port}");
    let mut stream = TcpStream::connect(&addr)
        .map_err(|e| KiiaudError::Source(format!("TCP connect to '{addr}' failed: {e}")))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));

    if !request.is_empty() {
        stream
            .write_all(request)
            .map_err(|e| KiiaudError::Source(format!("TCP write to '{addr}' failed: {e}")))?;
        // If this is an HTTP-over-TCP request from LMS (`GET /stream.mp3?... HTTP/1.0\r\n\r\n`),
        // strip the HTTP response header before handing audio bytes to Symphonia.
        if request.starts_with(b"GET ") || request.starts_with(b"HEAD ") {
            strip_http_response_headers(&mut stream)?;
        }
    }

    let prebuffered_reader: Box<dyn Read + Send> =
        Box::new(PrebufferedStreamReader::new(Box::new(stream)));

    Ok((
        Box::new(StreamMediaSource::new(prebuffered_reader, None)),
        format_hint,
    ))
}

/// Strip HTTP response headers up to `\r\n\r\n` from a raw TCP stream.
fn strip_http_response_headers(stream: &mut TcpStream) -> Result<()> {
    let mut window = [0u8; 4];
    let mut byte = [0u8; 1];
    let mut total = 0usize;
    while total < 16384 {
        let n = stream
            .read(&mut byte)
            .map_err(|e| KiiaudError::Source(format!("Error reading HTTP header: {e}")))?;
        if n == 0 {
            break;
        }
        window[0] = window[1];
        window[1] = window[2];
        window[2] = window[3];
        window[3] = byte[0];
        total += 1;
        if &window == b"\r\n\r\n" {
            return Ok(());
        }
    }
    Ok(())
}

fn url_extension_hint(url: &str) -> Option<String> {
    let without_query = url.split(['?', '#']).next().unwrap_or(url);
    let last_segment = without_query.rsplit('/').next()?;
    let (_, ext) = last_segment.rsplit_once('.')?;
    let ext = ext.to_ascii_lowercase();
    if matches!(
        ext.as_str(),
        "wv" | "flac"
            | "wav"
            | "wave"
            | "aif"
            | "aiff"
            | "dsf"
            | "dff"
            | "mp3"
            | "ogg"
            | "opus"
            | "m4a"
            | "aac"
    ) {
        Some(ext)
    } else {
        None
    }
}

fn mime_to_ext(content_type: &str) -> Option<String> {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let ext = match mime.as_str() {
        "audio/x-wavpack" | "audio/wavpack" => "wv",
        "audio/flac" | "audio/x-flac" => "flac",
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "audio/aiff" | "audio/x-aiff" => "aiff",
        "audio/dsd" | "audio/x-dsd" | "audio/dsf" | "audio/x-dsf" => "dsf",
        "audio/dff" | "audio/x-dff" => "dff",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/ogg" | "application/ogg" => "ogg",
        "audio/opus" => "opus",
        "audio/aac" | "audio/x-aac" => "aac",
        "audio/mp4" | "audio/x-m4a" => "m4a",
        _ => return None,
    };
    Some(ext.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_prebuffered_stream_reader_reads_exact_bytes() {
        let sample_data = vec![0xABu8; 300 * 1024]; // 300 KiB (> 128 KiB initial watermark)
        let cursor = Cursor::new(sample_data.clone());
        let mut reader = PrebufferedStreamReader::new(Box::new(cursor));

        let mut read_buf = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        loop {
            let n = reader.read(&mut chunk).expect("read should succeed");
            if n == 0 {
                break;
            }
            read_buf.extend_from_slice(&chunk[..n]);
        }

        assert_eq!(read_buf.len(), sample_data.len());
        assert_eq!(read_buf, sample_data);

        let telemetry = get_stream_telemetry();
        assert_eq!(telemetry.total_bytes_read, sample_data.len() as u64);
        assert_eq!(telemetry.buffer_capacity, STREAM_BUFFER_CAPACITY as u32);
    }

    #[test]
    fn test_reset_stream_telemetry() {
        reset_stream_telemetry();
        let telemetry = get_stream_telemetry();
        assert_eq!(telemetry.total_bytes_read, 0);
        assert_eq!(telemetry.buffer_occupancy, 0);
    }
}
