// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP/HTTPS and raw TCP stream `MediaSource` implementations for Symphonia.

use kiiaud_core::{KiiaudError, Result, strip_uri_auth};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;
use symphonia::core::io::MediaSource;

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
        "opened HTTP audio stream"
    );

    let reader: Box<dyn Read + Send> = if let Some((session, stream_seq)) = anywhere_meta {
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

    Ok((
        Box::new(StreamMediaSource::new(reader, content_len)),
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

    Ok((
        Box::new(StreamMediaSource::new(Box::new(stream), None)),
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
