// SPDX-License-Identifier: MIT OR Apache-2.0

//! Lyrion Music Server (LMS / Squeezebox) SlimProto client adapter (`kiiaud-adapter-lms`, Phase 5B).
//!
//! Functions as a clean-room pure-Rust counterpart to `squeezelite` (`R-017`, `D-015`):
//! - Connects to LMS on TCP port 3483.
//! - Sends `HELO` advertising capabilities (`pcm`, `flc`, `wav`, `aif`, `wvp`, `dsf`, `dff`, `mp3`, `ogg`, `ops`) and player MAC address.
//! - Sends `SETD` player name packet so LMS displays the configured `player_name`.
//! - Parses incoming 2-byte big-endian length-prefixed SlimProto packets (`strm`, `audg`, `setd`, `aude`).
//! - Handles `strm` commands:
//!   - `'s'` (start): extracts HTTP/TCP request headers and server IP/port, constructs `AudioSource::HttpStream` or `AudioSource::TcpStream`, and dispatches `PlaybackCommand::Load` (`autostart = true`) to `kiiaud-core`, then replies with `STAT` (`STMc`, `STMh`, `STMs`).
//!   - `'p'` (pause): dispatches `PlaybackCommand::Pause` and replies `STAT` (`STMp`).
//!   - `'u'` (unpause): dispatches `PlaybackCommand::Resume` and replies `STAT` (`STMr`).
//!   - `'q'` (stop/flush): dispatches `PlaybackCommand::Stop` and replies `STAT` (`STMf`).
//!   - `'t'` (status/timer heartbeat): replies `STAT` (`STMt`) with server timestamp and live elapsed time.
//! - Subscribes to `PlaybackEvent`s to send `STAT` (`STMd` decoder ready / `STMo` output drained) on track completion so LMS seamlessly advances to the next track in its playlist.

use kiiaud_core::config::LmsAdapterConfig;
use kiiaud_core::{AudioSource, PlaybackCommand, PlaybackEvent, PlayerState, Result};
use kiiaud_playback::EngineHandle;
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Device ID 12 = `squeezelite` in SlimProto `HELO`.
const SLIMPROTO_DEVICE_ID_SQUEEZELITE: u8 = 12;
const SLIMPROTO_REVISION: u8 = 0;

/// Start the LMS SlimProto adapter loop with automatic reconnect on network failure (`NFR-007`).
pub async fn run_lms_adapter(config: LmsAdapterConfig, engine: EngineHandle) -> Result<()> {
    if !config.enabled {
        return Ok(());
    }

    let addr = format!("{}:{}", config.server_host, config.server_port);
    tracing::info!(
        server = %addr,
        player_name = %config.player_name,
        "starting LMS / Lyrion SlimProto adapter"
    );

    loop {
        match connect_and_run_session(&config, &engine).await {
            Ok(()) => {
                tracing::info!("LMS SlimProto session closed cleanly; reconnecting in 5s");
            }
            Err(e) => {
                tracing::warn!(server = %addr, "LMS SlimProto session error: {e}; retrying in 5s");
            }
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn connect_and_run_session(
    config: &LmsAdapterConfig,
    engine: &EngineHandle,
) -> std::result::Result<(), String> {
    let addr = format!("{}:{}", config.server_host, config.server_port);
    let mut stream = TcpStream::connect(&addr)
        .await
        .map_err(|e| format!("connect failed: {e}"))?;
    let _ = stream.set_nodelay(true);

    let (max_sample_rate, dsd_supported) = resolve_hardware_capabilities(config, engine);
    let start_epoch = Instant::now();

    // 1. Send HELO packet with hardware-informed capabilities
    let helo_pkt = build_helo_packet(
        &config.mac_address,
        &config.player_name,
        max_sample_rate,
        dsd_supported,
    );
    stream
        .write_all(&helo_pkt)
        .await
        .map_err(|e| format!("HELO write failed: {e}"))?;

    // 2. Send SETD (id=0: player name)
    let setd_pkt = build_setd_name_packet(&config.player_name);
    stream
        .write_all(&setd_pkt)
        .await
        .map_err(|e| format!("SETD write failed: {e}"))?;

    tracing::info!(
        server = %addr,
        player_name = %config.player_name,
        max_sample_rate,
        dsd_supported,
        "connected to LMS server and sent HELO"
    );

    let mut event_rx = engine.subscribe();
    let mut tick_1s = tokio::time::interval(Duration::from_secs(1));
    let mut len_buf = [0u8; 2];

    loop {
        tokio::select! {
            read_res = stream.read_exact(&mut len_buf) => {
                read_res.map_err(|e| format!("read packet length failed: {e}"))?;
                let pkt_len = u16::from_be_bytes(len_buf) as usize;
                if pkt_len < 4 || pkt_len > 65535 {
                    return Err(format!("invalid SlimProto packet length: {pkt_len}"));
                }
                let mut payload = vec![0u8; pkt_len];
                stream
                    .read_exact(&mut payload)
                    .await
                    .map_err(|e| format!("read packet payload failed: {e}"))?;

                handle_server_packet(&payload, config, engine, &mut stream, start_epoch).await?;
            }
            Ok(ev) = event_rx.recv() => {
                match ev {
                    PlaybackEvent::TrackStarted { .. } => {
                        // Squeezelite sends STMs only once when audio output starts
                        let stat = build_stat_packet(b"STMs", engine, start_epoch, 0);
                        let _ = stream.write_all(&stat).await;
                    }
                    PlaybackEvent::TrackEnded(_) => {
                        // Notify LMS that decoder finished reading current track data (DECODE_READY)
                        // so LMS can transition to the next track in gapless/crossfade mode.
                        let stmd = build_stat_packet(b"STMd", engine, start_epoch, 0);
                        let _ = stream.write_all(&stmd).await;
                    }
                    PlaybackEvent::StateChanged(PlayerState::Stop) => {
                        // Output underrun / stopped
                        let stmu = build_stat_packet(b"STMu", engine, start_epoch, 0);
                        let _ = stream.write_all(&stmu).await;
                    }
                    _ => {}
                }
            }
            _ = tick_1s.tick() => {
                // Squeezelite sends STMt once per second during active playback to report elapsed position.
                // When idle/stopped, client does not send periodic STMt; LMS queries with strm 't' when needed.
                if engine.status().state == PlayerState::Play {
                    let stat = build_stat_packet(b"STMt", engine, start_epoch, 0);
                    let _ = stream.write_all(&stat).await;
                }
            }
        }
    }
}

/// Resolve max sample rate and DSD support based on config override or detected DAC output capabilities.
pub fn resolve_hardware_capabilities(
    config: &LmsAdapterConfig,
    engine: &EngineHandle,
) -> (u32, bool) {
    let st = engine.status();
    let out_caps = st.output_capabilities.as_ref();

    let max_rate = config.max_sample_rate.unwrap_or_else(|| {
        out_caps
            .and_then(|c| c.sample_rates.iter().copied().max())
            .unwrap_or(192_000)
    });

    let dsd = config
        .dsd_support
        .unwrap_or_else(|| out_caps.is_some_and(|c| c.dsd_native || c.dsd_dop));

    (max_rate, dsd)
}

/// Build client-to-server `HELO` packet:
/// `[4B "HELO"][4B length BE][1B dev_id][1B rev][6B mac][16B uuid][2B wlan_ch][8B bytes_rx][2B lang][capabilities...]`
pub fn build_helo_packet(
    mac: &[u8; 6],
    player_name: &str,
    max_sample_rate: u32,
    dsd_supported: bool,
) -> Vec<u8> {
    let mut codecs = "wvp,flc,wav,aif,pcm,mp3,ogg,ops".to_string();
    if dsd_supported {
        codecs.push_str(",dsf,dff");
    }
    let caps = format!(
        "CanHTTPS=1,Model=squeezelite,AccuratePlayPoints=1,HasDigitalOut=1,HasPolarityInversion=1,ModelName={player_name},MaxSampleRate={max_sample_rate},{codecs}"
    );
    let body_len = 36 + caps.len();
    let mut pkt = Vec::with_capacity(8 + body_len);
    pkt.extend_from_slice(b"HELO");
    pkt.extend_from_slice(&(body_len as u32).to_be_bytes());
    pkt.push(SLIMPROTO_DEVICE_ID_SQUEEZELITE);
    pkt.push(SLIMPROTO_REVISION);
    pkt.extend_from_slice(mac);
    pkt.extend_from_slice(&[0u8; 16]); // UUID
    pkt.extend_from_slice(&0u16.to_be_bytes()); // wlan channel list
    pkt.extend_from_slice(&0u64.to_be_bytes()); // bytes received
    pkt.extend_from_slice(&[0u8; 2]); // language / padding
    pkt.extend_from_slice(caps.as_bytes());
    pkt
}

/// Build client-to-server `RESP` packet acknowledging HTTP response headers.
/// In SlimProto, this notifies LMS that the HTTP connection was successfully established,
/// clearing the server's `connecting` flag (`_http_response_handler`).
pub fn build_resp_packet(headers: &str) -> Vec<u8> {
    let header_bytes = headers.as_bytes();
    let body_len = header_bytes.len();
    let mut pkt = Vec::with_capacity(8 + body_len);
    pkt.extend_from_slice(b"RESP");
    pkt.extend_from_slice(&(body_len as u32).to_be_bytes());
    pkt.extend_from_slice(header_bytes);
    pkt
}

/// Build client-to-server `SETD` packet announcing the player name (`id = 0`).
pub fn build_setd_name_packet(name: &str) -> Vec<u8> {
    let name_bytes = name.as_bytes();
    let body_len = 1 + name_bytes.len();
    let mut pkt = Vec::with_capacity(8 + body_len);
    pkt.extend_from_slice(b"SETD");
    pkt.extend_from_slice(&(body_len as u32).to_be_bytes());
    pkt.push(0); // id 0 = player name
    pkt.extend_from_slice(name_bytes);
    pkt
}

/// Build client-to-server `STAT` packet (53 bytes body).
pub fn build_stat_packet(
    event_code: &[u8; 4],
    engine: &EngineHandle,
    start_epoch: Instant,
    server_timestamp: u32,
) -> Vec<u8> {
    let st = engine.status();
    let elapsed_ms = st.elapsed.as_millis() as u32;
    let elapsed_secs = st.elapsed.as_secs() as u32;
    let jiffies = start_epoch.elapsed().as_millis() as u32;

    // Report monotonic progressive bytes received (approx 176.4 KB/s for stereo audio)
    // so LMS streaming flow tracker knows data is flowing normally.
    let bytes_received = match st.state {
        PlayerState::Play => (elapsed_ms as u64).saturating_mul(176),
        _ => 0,
    };

    let (stream_buf_fullness, output_buf_fullness) = match st.state {
        PlayerState::Play => (1024 * 1024u32, 256 * 1024u32),
        PlayerState::Pause => (1024 * 1024u32, 0u32),
        PlayerState::Stop => (0u32, 0u32),
    };

    let mut body = Vec::with_capacity(53);
    body.extend_from_slice(event_code); // 0..4: event code ("STMs", "STMt", etc.)
    body.push(0); // 4: num_crlf
    body.push(0); // 5: mas_initialized
    body.push(0); // 6: mas_mode
    body.extend_from_slice(&(4 * 1024 * 1024u32).to_be_bytes()); // 7..11: stream buffer size (4 MiB)
    body.extend_from_slice(&stream_buf_fullness.to_be_bytes()); // 11..15: stream buffer fullness
    body.extend_from_slice(&bytes_received.to_be_bytes()); // 15..23: bytes received
    body.extend_from_slice(&0xffffu16.to_be_bytes()); // 23..25: signal strength (0xffff = Ethernet/wired in Squeezelite)
    body.extend_from_slice(&jiffies.to_be_bytes()); // 25..29: jiffies
    body.extend_from_slice(&(1024 * 1024u32).to_be_bytes()); // 29..33: output buffer size
    body.extend_from_slice(&output_buf_fullness.to_be_bytes()); // 33..37: output buffer fullness
    body.extend_from_slice(&elapsed_secs.to_be_bytes()); // 37..41: elapsed seconds
    body.extend_from_slice(&0u16.to_be_bytes()); // 41..43: voltage
    body.extend_from_slice(&elapsed_ms.to_be_bytes()); // 43..47: elapsed milliseconds
    body.extend_from_slice(&server_timestamp.to_be_bytes()); // 47..51: server timestamp
    body.extend_from_slice(&0u16.to_be_bytes()); // 51..53: error code

    let mut pkt = Vec::with_capacity(8 + body.len());
    pkt.extend_from_slice(b"STAT");
    pkt.extend_from_slice(&(body.len() as u32).to_be_bytes());
    pkt.extend_from_slice(&body);
    pkt
}

async fn handle_server_packet(
    payload: &[u8],
    config: &LmsAdapterConfig,
    engine: &EngineHandle,
    stream: &mut TcpStream,
    start_epoch: Instant,
) -> std::result::Result<(), String> {
    if payload.len() < 4 {
        return Ok(());
    }
    let op = &payload[0..4];
    let data = &payload[4..];

    match op {
        b"strm" => {
            if data.len() < 24 {
                return Ok(());
            }
            let command = data[0];
            let _autostart = data[1];
            let format_byte = data[2];
            let replay_gain_or_ts = u32::from_be_bytes([data[14], data[15], data[16], data[17]]);
            let server_port = u16::from_be_bytes([data[18], data[19]]);
            let server_ip_raw = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
            let http_request_bytes = &data[24..];

            match command {
                b's' => {
                    // Start streaming from LMS
                    let host = if server_ip_raw == 0 {
                        config.server_host.clone()
                    } else {
                        Ipv4Addr::from(server_ip_raw).to_string()
                    };
                    let port = if server_port == 0 {
                        config.http_port
                    } else {
                        server_port
                    };

                    let source =
                        parse_lms_strm_source(&host, port, format_byte, http_request_bytes);

                    tracing::info!(
                        %source,
                        format_byte = %(format_byte as char),
                        "LMS SlimProto strm-s start command received"
                    );

                    let _ = engine
                        .send(PlaybackCommand::Load {
                            source,
                            title: None,
                            artist: None,
                            autostart: true,
                        })
                        .await;

                    // Flush buffers, acknowledge stream connection, and acknowledge HTTP response headers
                    // without sending premature STMs (which will be sent once audio output starts).
                    let stmf = build_stat_packet(b"STMf", engine, start_epoch, 0);
                    stream
                        .write_all(&stmf)
                        .await
                        .map_err(|e| format!("STMf write failed: {e}"))?;

                    let stmc = build_stat_packet(b"STMc", engine, start_epoch, 0);
                    stream
                        .write_all(&stmc)
                        .await
                        .map_err(|e| format!("STMc write failed: {e}"))?;

                    let resp =
                        build_resp_packet("HTTP/1.0 200 OK\r\nContent-Type: audio/x-flac\r\n\r\n");
                    stream
                        .write_all(&resp)
                        .await
                        .map_err(|e| format!("RESP write failed: {e}"))?;
                }
                b'p' => {
                    let _ = engine.send(PlaybackCommand::Pause).await;
                    let pkt = build_stat_packet(b"STMp", engine, start_epoch, 0);
                    stream
                        .write_all(&pkt)
                        .await
                        .map_err(|e| format!("STAT write failed: {e}"))?;
                }
                b'u' => {
                    let _ = engine.send(PlaybackCommand::Resume).await;
                    let pkt = build_stat_packet(b"STMr", engine, start_epoch, 0);
                    stream
                        .write_all(&pkt)
                        .await
                        .map_err(|e| format!("STAT write failed: {e}"))?;
                }
                b'q' => {
                    let _ = engine.send(PlaybackCommand::Stop).await;
                    let pkt = build_stat_packet(b"STMf", engine, start_epoch, 0);
                    stream
                        .write_all(&pkt)
                        .await
                        .map_err(|e| format!("STAT write failed: {e}"))?;
                }
                b't' => {
                    let pkt = build_stat_packet(b"STMt", engine, start_epoch, replay_gain_or_ts);
                    stream
                        .write_all(&pkt)
                        .await
                        .map_err(|e| format!("STAT write failed: {e}"))?;
                }
                b'f' => {
                    let pkt = build_stat_packet(b"STMf", engine, start_epoch, 0);
                    stream
                        .write_all(&pkt)
                        .await
                        .map_err(|e| format!("STAT write failed: {e}"))?;
                }
                _ => {}
            }
        }
        b"setd" => {
            if data.first() == Some(&0) && data.len() == 1 {
                // Server querying our player name
                let pkt = build_setd_name_packet(&config.player_name);
                stream
                    .write_all(&pkt)
                    .await
                    .map_err(|e| format!("SETD reply failed: {e}"))?;
            }
        }
        b"audg" | b"aude" => {
            // In bit-perfect mode, software volume attenuation is intentionally bypassed (`D-008`).
        }
        _ => {}
    }

    Ok(())
}

/// Convert the `strm` HTTP request header bytes into an [`AudioSource`].
pub fn parse_lms_strm_source(
    host: &str,
    port: u16,
    format_byte: u8,
    http_request_bytes: &[u8],
) -> AudioSource {
    let hint = match format_byte {
        b'f' => Some("flac".to_owned()),
        b'w' => Some("wv".to_owned()),
        b'p' => Some("wav".to_owned()),
        b'a' => Some("aiff".to_owned()),
        b'd' => Some("dsf".to_owned()),
        b'm' => Some("mp3".to_owned()),
        b'o' => Some("ogg".to_owned()),
        _ => None,
    };

    let req_str = String::from_utf8_lossy(http_request_bytes);
    if let Some(first_line) = req_str.lines().next()
        && let Some(rest) = first_line.strip_prefix("GET ")
        && let Some((path_part, _)) = rest.split_once(" HTTP/")
    {
        let mut headers = HashMap::new();
        for line in req_str.lines().skip(1) {
            if let Some((k, v)) = line.split_once(':') {
                let k_trim = k.trim();
                let v_trim = v.trim();
                if !k_trim.is_empty() && !k_trim.eq_ignore_ascii_case("host") {
                    headers.insert(k_trim.to_owned(), v_trim.to_owned());
                }
            }
        }
        let url = format!("http://{host}:{port}{}", path_part.trim());
        return AudioSource::HttpStream { url, headers };
    }

    AudioSource::TcpStream {
        host: host.to_owned(),
        port,
        request: http_request_bytes.to_vec(),
        format_hint: hint,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_helo_packet_structure() {
        let mac = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];
        let pkt = build_helo_packet(&mac, "kiiaud", 192_000, false);
        assert_eq!(&pkt[0..4], b"HELO");
        let len = u32::from_be_bytes([pkt[4], pkt[5], pkt[6], pkt[7]]) as usize;
        assert_eq!(pkt.len(), 8 + len);
        assert_eq!(pkt[8], SLIMPROTO_DEVICE_ID_SQUEEZELITE);
        assert_eq!(&pkt[10..16], &mac);
        let caps = String::from_utf8_lossy(&pkt[8 + 36..]);
        assert!(caps.contains("ModelName=kiiaud"));
        assert!(caps.contains("MaxSampleRate=192000"));
        assert!(!caps.contains("dsf"));

        let pkt_dsd = build_helo_packet(&mac, "kiiaud", 768_000, true);
        let caps_dsd = String::from_utf8_lossy(&pkt_dsd[8 + 36..]);
        assert!(caps_dsd.contains("MaxSampleRate=768000"));
        assert!(caps_dsd.contains("dsf,dff"));
    }

    #[test]
    fn test_build_resp_packet() {
        let headers = "HTTP/1.0 200 OK\r\nContent-Type: audio/x-flac\r\n\r\n";
        let pkt = build_resp_packet(headers);
        assert_eq!(&pkt[0..4], b"RESP");
        let len = u32::from_be_bytes([pkt[4], pkt[5], pkt[6], pkt[7]]) as usize;
        assert_eq!(len, headers.len());
        assert_eq!(&pkt[8..], headers.as_bytes());
    }

    #[test]
    fn test_parse_lms_strm_get_request() {
        let req = b"GET /stream.mp3?player=02:11:22:33:44:55 HTTP/1.0\r\nAuthorization: Basic abc\r\n\r\n";
        let src = parse_lms_strm_source("192.168.1.100", 9000, b'f', req);
        match src {
            AudioSource::HttpStream { url, headers } => {
                assert_eq!(
                    url,
                    "http://192.168.1.100:9000/stream.mp3?player=02:11:22:33:44:55"
                );
                assert_eq!(headers.get("Authorization").unwrap(), "Basic abc");
            }
            _ => panic!("expected HttpStream"),
        }
    }

    #[test]
    fn test_build_stat_packet_structure_and_idle() {
        use kiiaud_core::config::KiiaudConfig;
        use kiiaud_playback::PlaybackEngine;

        let mut config = KiiaudConfig::default();
        config.output.backend = "mock".to_owned();
        let engine = PlaybackEngine::new(&config);
        let handle = engine.handle();

        let start_epoch = Instant::now();
        let stat_pkt = build_stat_packet(b"STMt", &handle, start_epoch, 12345);

        // Header check: 4 bytes opcode "STAT" + 4 bytes length BE
        assert_eq!(&stat_pkt[0..4], b"STAT");
        let body_len =
            u32::from_be_bytes([stat_pkt[4], stat_pkt[5], stat_pkt[6], stat_pkt[7]]) as usize;
        assert_eq!(body_len, 53);
        assert_eq!(stat_pkt.len(), 8 + 53);

        // Body: 0..4 is event code
        let body = &stat_pkt[8..];
        assert_eq!(&body[0..4], b"STMt");

        // When stopped (idle): buffer fullness is 0
        let stream_fullness = u32::from_be_bytes([body[11], body[12], body[13], body[14]]);
        let output_fullness = u32::from_be_bytes([body[33], body[34], body[35], body[36]]);
        assert_eq!(stream_fullness, 0);
        assert_eq!(output_fullness, 0);

        // Server timestamp echoed back at 47..51
        let echoed_ts = u32::from_be_bytes([body[47], body[48], body[49], body[50]]);
        assert_eq!(echoed_ts, 12345);
    }
}
