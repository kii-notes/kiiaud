// SPDX-License-Identifier: MIT OR Apache-2.0

use kiiaud_adapter_cli::{IpcRequest, IpcResponse, run_ipc_server, send_ipc_request};
use kiiaud_adapter_mpd::{CmdOutcome, execute_mpd_line};
use kiiaud_core::{AudioSource, PathPolicy, PlaybackCommand, PlaybackEvent, PlayerState};
use kiiaud_output::MockAudioOutput;
use kiiaud_playback::PlaybackEngine;
use std::fs;
use std::time::Duration;

fn build_wav_s16le(sample_rate: u32, frames: usize) -> Vec<u8> {
    let channels = 2u16;
    let bits_per_sample = 16u16;
    let block_align = channels * (bits_per_sample / 8);
    let byte_rate = sample_rate * u32::from(block_align);
    let data_len = (frames * usize::from(block_align)) as u32;
    let riff_len = 36 + data_len;

    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_len.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits_per_sample.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..frames {
        let s = (i as i16).wrapping_mul(17);
        out.extend_from_slice(&s.to_le_bytes());
        out.extend_from_slice(&(-s).to_le_bytes());
    }
    out
}

#[tokio::test]
async fn test_engine_gapless_transition_and_mock_output() {
    let dir = tempfile::tempdir().unwrap();
    let track1_path = dir.path().join("track1.wav");
    let track2_path = dir.path().join("track2.wav");
    fs::write(&track1_path, build_wav_s16le(44_100, 500)).unwrap();
    fs::write(&track2_path, build_wav_s16le(44_100, 500)).unwrap();

    let mock_out = MockAudioOutput::new();
    let mock_inspector = mock_out.clone();
    let policy = PathPolicy::new(vec![dir.path().to_path_buf()]);

    let engine = PlaybackEngine::with_output(Box::new(mock_out), policy);
    let handle = engine.handle();
    let mut events = handle.subscribe();

    let engine_task = tokio::spawn(async move {
        engine.run().await;
    });

    // Enqueue two consecutive tracks
    handle
        .send(PlaybackCommand::Enqueue {
            source: AudioSource::LocalFile(track1_path),
            title: Some("Track One".to_owned()),
            artist: Some("Artist".to_owned()),
        })
        .await
        .unwrap();
    handle
        .send(PlaybackCommand::Enqueue {
            source: AudioSource::LocalFile(track2_path),
            title: Some("Track Two".to_owned()),
            artist: Some("Artist".to_owned()),
        })
        .await
        .unwrap();

    handle.send(PlaybackCommand::Play(None)).await.unwrap();

    let mut started_ids = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if let Ok(Ok(ev)) = tokio::time::timeout(Duration::from_millis(500), events.recv()).await {
            match ev {
                PlaybackEvent::TrackStarted { item, .. } => {
                    started_ids.push(item.id);
                }
                PlaybackEvent::StateChanged(PlayerState::Stop) if started_ids.len() >= 2 => {
                    break;
                }
                _ => {}
            }
        }
    }

    assert_eq!(
        started_ids,
        vec![1, 2],
        "Both tracks should play sequentially via gapless transition"
    );
    assert_eq!(
        mock_inspector.state().frames_written,
        1000,
        "MockAudioOutput must receive all 1000 frames across both tracks"
    );

    engine_task.abort();
}

#[tokio::test]
async fn test_engine_fault_tolerance_skips_broken_track() {
    let dir = tempfile::tempdir().unwrap();
    let broken_path = dir.path().join("corrupt.wav");
    let valid_path = dir.path().join("valid.wav");
    fs::write(&broken_path, b"NOT_A_VALID_AUDIO_FILE").unwrap();
    fs::write(&valid_path, build_wav_s16le(48_000, 300)).unwrap();

    let mock_out = MockAudioOutput::new();
    let mock_inspector = mock_out.clone();
    let policy = PathPolicy::new(vec![dir.path().to_path_buf()]);

    let engine = PlaybackEngine::with_output(Box::new(mock_out), policy);
    let handle = engine.handle();
    let mut events = handle.subscribe();

    let engine_task = tokio::spawn(async move {
        engine.run().await;
    });

    // Enqueue corrupt track #1 followed by valid track #2
    handle
        .send(PlaybackCommand::Enqueue {
            source: AudioSource::LocalFile(broken_path),
            title: Some("Broken".to_owned()),
            artist: None,
        })
        .await
        .unwrap();
    handle
        .send(PlaybackCommand::Enqueue {
            source: AudioSource::LocalFile(valid_path),
            title: Some("Valid".to_owned()),
            artist: None,
        })
        .await
        .unwrap();

    handle.send(PlaybackCommand::Play(None)).await.unwrap();

    let mut saw_error = false;
    let mut started_valid = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if let Ok(Ok(ev)) = tokio::time::timeout(Duration::from_millis(500), events.recv()).await {
            match ev {
                PlaybackEvent::Error(_) => {
                    saw_error = true;
                }
                PlaybackEvent::TrackStarted { item, .. } if item.id == 2 => {
                    started_valid = true;
                }
                PlaybackEvent::StateChanged(PlayerState::Stop) if started_valid => {
                    break;
                }
                _ => {}
            }
        }
    }

    assert!(saw_error, "Broken track must emit PlaybackEvent::Error");
    assert!(
        started_valid,
        "Engine must survive corrupt track and advance to valid track #2"
    );
    assert_eq!(mock_inspector.state().frames_written, 300);

    engine_task.abort();
}

#[tokio::test]
async fn test_cli_ipc_and_mpd_protocol_adapters() {
    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("kiiaud-test.sock");
    let wav_path = dir.path().join("song.wav");
    fs::write(&wav_path, build_wav_s16le(44_100, 200)).unwrap();

    let mock_out = MockAudioOutput::new();
    let policy = PathPolicy::new(vec![dir.path().to_path_buf()]);
    let engine = PlaybackEngine::with_output(Box::new(mock_out), policy);
    let handle = engine.handle();

    let engine_task = tokio::spawn(async move {
        engine.run().await;
    });

    let ipc_sock = sock_path.clone();
    let ipc_handle = handle.clone();
    let ipc_task = tokio::spawn(async move {
        let _ = run_ipc_server(ipc_sock, ipc_handle).await;
    });

    // Wait briefly for Unix socket listener to bind
    for _ in 0..20 {
        if sock_path.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // 1. Test CLI IPC enqueue & queue listing
    let resp = send_ipc_request(
        &sock_path,
        &IpcRequest::Enqueue {
            target: wav_path.to_string_lossy().to_string(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(resp, IpcResponse::Ok { .. }));

    // Give coordinator a moment to process the enqueue command
    tokio::time::sleep(Duration::from_millis(30)).await;

    let q_resp = send_ipc_request(&sock_path, &IpcRequest::Queue)
        .await
        .unwrap();
    match q_resp {
        IpcResponse::Queue { items } => {
            assert_eq!(items.len(), 1);
            assert!(items[0].source.display_uri().ends_with("song.wav"));
        }
        other => panic!("Expected Queue response, got {other:?}"),
    }

    // 2. Test MPD command execution against the same shared EngineHandle
    let mpd_status = execute_mpd_line("status", &handle, 0).await.unwrap();
    match mpd_status {
        CmdOutcome::Reply(text) => {
            assert!(text.contains("state: stop"));
            assert!(text.contains("playlistlength: 1"));
            assert!(text.contains("volume: 100"));
        }
        _ => panic!("Expected MPD Reply"),
    }

    // Bit-perfect volume policy (`D-009`): `setvol` succeeds as a no-op
    let mpd_vol = execute_mpd_line("setvol 50", &handle, 0).await.unwrap();
    assert!(matches!(mpd_vol, CmdOutcome::Reply(ref s) if s.is_empty()));

    // `decoders` advertises WavPack (.wv), FLAC, WAV, AIFF, DSF, DFF
    let mpd_dec = execute_mpd_line("decoders", &handle, 0).await.unwrap();
    match mpd_dec {
        CmdOutcome::Reply(text) => {
            assert!(text.contains("plugin: symphonia"));
            assert!(text.contains("suffix: wv"));
            assert!(text.contains("suffix: dsf"));
        }
        _ => panic!("Expected MPD decoders reply"),
    }

    // Database-oriented commands return an explicit, non-fatal ACK (`docs/ADAPTERS.md` Section 3)
    let mpd_db = execute_mpd_line("update", &handle, 0).await;
    assert!(mpd_db.is_err());
    assert!(mpd_db.unwrap_err().contains("database-free"));

    ipc_task.abort();
    engine_task.abort();
}
