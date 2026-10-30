// SPDX-License-Identifier: MIT OR Apache-2.0

use kiiaud_core::{
    AudioBuffer, AudioFormat, AudioSource, BitOrder, ChannelLayout, DsdRate, KiiaudError,
    PathPolicy, SampleFormat,
};
use kiiaud_decoder::{Decoder, SymphoniaAudioDecoder};
use kiiaud_output::dop::DopEncoder;
use std::fs;
use std::path::Path;
use std::process::Command;

/// Build a valid 16-bit stereo WAV file in memory with exact known samples.
fn build_wav_s16le(sample_rate: u32, samples_lr: &[(i16, i16)]) -> Vec<u8> {
    let channels = 2u16;
    let bits_per_sample = 16u16;
    let block_align = channels * (bits_per_sample / 8);
    let byte_rate = sample_rate * u32::from(block_align);
    let data_len = (samples_lr.len() * usize::from(block_align)) as u32;
    let riff_len = 36 + data_len;

    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_len.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits_per_sample.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for &(l, r) in samples_lr {
        out.extend_from_slice(&l.to_le_bytes());
        out.extend_from_slice(&r.to_le_bytes());
    }
    out
}

/// Build a valid 24-bit packed (`S24_3LE`) stereo WAV file in memory with exact 24-bit signed values.
fn build_wav_s24le(sample_rate: u32, samples_lr: &[(i32, i32)]) -> Vec<u8> {
    let channels = 2u16;
    let bits_per_sample = 24u16;
    let block_align = channels * (bits_per_sample / 8);
    let byte_rate = sample_rate * u32::from(block_align);
    let data_len = (samples_lr.len() * usize::from(block_align)) as u32;
    let riff_len = 36 + data_len;

    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_len.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits_per_sample.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for &(l, r) in samples_lr {
        let lb = l.to_le_bytes();
        let rb = r.to_le_bytes();
        out.extend_from_slice(&[lb[0], lb[1], lb[2]]);
        out.extend_from_slice(&[rb[0], rb[1], rb[2]]);
    }
    out
}

/// Build a valid Sony DSF (`.dsf`) DSD64 stereo file with known planar DSD bytes.
fn build_dsf_dsd64(
    ch0_block: &[u8; 4096],
    ch1_block: &[u8; 4096],
    valid_bytes_per_ch: u64,
) -> Vec<u8> {
    let data_chunk_size = 12u64 + 4096 * 2;
    let file_size = 28u64 + 52u64 + data_chunk_size;
    let sample_count = valid_bytes_per_ch * 8; // 1-bit samples per channel

    let mut out = Vec::with_capacity(file_size as usize);
    // "DSD " chunk (28 bytes)
    out.extend_from_slice(b"DSD ");
    out.extend_from_slice(&28u64.to_le_bytes());
    out.extend_from_slice(&file_size.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes()); // no ID3 metadata

    // "fmt " chunk (52 bytes)
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&52u64.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes()); // format_version = 1
    out.extend_from_slice(&0u32.to_le_bytes()); // format_id = 0 (DSD raw)
    out.extend_from_slice(&2u32.to_le_bytes()); // channel_type = 2 (stereo)
    out.extend_from_slice(&2u32.to_le_bytes()); // channel_count = 2
    out.extend_from_slice(&2_822_400u32.to_le_bytes()); // DSD64 sample rate
    out.extend_from_slice(&1u32.to_le_bytes()); // bits_per_sample = 1 (LSB-first per DSF spec)
    out.extend_from_slice(&sample_count.to_le_bytes());
    out.extend_from_slice(&4096u32.to_le_bytes()); // block_size_per_channel = 4096
    out.extend_from_slice(&0u32.to_le_bytes()); // reserved

    // "data" chunk (12 + 8192 bytes)
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_chunk_size.to_le_bytes());
    out.extend_from_slice(ch0_block);
    out.extend_from_slice(ch1_block);
    out
}

/// Build a valid WavPack v5 DSD64 (`.wv`) stereo file in pure Rust using uncompressed DSD mode (`mode = 0`).
///
/// In WavPack DSD:
/// - `flags` has `DSD_FLAG` (`0x8000_0000`), `INITIAL_BLOCK` (`0x800`), `FINAL_BLOCK` (`0x1000`),
///   and `SRATE_LSB` (bit 23) = index 9 (`44100` Hz base byte-rate before shift).
/// - `ID_DSD_BLOCK` (`0x0e`) payload starts with `[rate_shift = 3, mode = 0]`, followed by
///   `block_samples * 2` interleaved DSD bytes (`[L0, R0, L1, R1, ...]`).
///   Because `(44100 * 8) << 3 == 2_822_400` Hz, the stream is recognized as stereo DSD64!
fn build_wavpack_dsd64(interleaved_lr_bytes: &[(u8, u8)]) -> Vec<u8> {
    let block_samples = interleaved_lr_bytes.len() as u32;
    let mut dsd_subblock_data = Vec::with_capacity(2 + interleaved_lr_bytes.len() * 2);
    dsd_subblock_data.push(3u8); // shift = 3 -> (44100 * 8) << 3 = 2_822_400 Hz (DSD64)
    dsd_subblock_data.push(0u8); // mode = 0 (uncompressed raw DSD bytes in WavPack DSD sub-block)
    for &(l, r) in interleaved_lr_bytes {
        dsd_subblock_data.push(l);
        dsd_subblock_data.push(r);
    }

    // Compute WavPack DSD CRC over all interleaved DSD bytes
    let crc = interleaved_lr_bytes
        .iter()
        .flat_map(|&(l, r)| [u32::from(l), u32::from(r)])
        .fold(u32::MAX, |acc, b| acc.wrapping_mul(3).wrapping_add(b));

    // Sub-block header for ID_DSD_BLOCK (0x0e):
    // Since dsd_subblock_data.len() is even (2 + 2*N), no ID_ODD_SIZE flag is needed.
    assert!(dsd_subblock_data.len() % 2 == 0);
    let word_len = dsd_subblock_data.len() / 2;
    assert!(word_len < 256);

    let mut body = Vec::with_capacity(2 + dsd_subblock_data.len());
    body.push(0x0e); // ID_DSD_BLOCK
    body.push(word_len as u8);
    body.extend_from_slice(&dsd_subblock_data);

    // 32-byte WavPack block header ("wvpk")
    // ck_size is total block bytes minus 8
    let ck_size = (32 - 8 + body.len()) as u32;
    let version: u16 = 0x0410;
    // Flags:
    // - BYTES_STORED = 0 (1 byte per sample)
    // - INITIAL_BLOCK = 0x0000_0800
    // - FINAL_BLOCK   = 0x0000_1000
    // - SRATE_LSB (9 << 23) for 44100 Hz base rate
    // - DSD_FLAG      = 0x8000_0000
    let flags: u32 = 0x8000_0000 | 0x0000_0800 | 0x0000_1000 | (9u32 << 23);

    let mut out = Vec::with_capacity(32 + body.len());
    out.extend_from_slice(b"wvpk");
    out.extend_from_slice(&ck_size.to_le_bytes());
    out.extend_from_slice(&version.to_le_bytes());
    out.push(0); // block_index_u8
    out.push(0); // total_samples_u8
    out.extend_from_slice(&block_samples.to_le_bytes()); // total_samples
    out.extend_from_slice(&0u32.to_le_bytes()); // block_index
    out.extend_from_slice(&block_samples.to_le_bytes()); // block_samples
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&body);
    out
}

#[test]
fn test_wav_16bit_and_24bit_bit_perfect_decoding() {
    let dir = tempfile::tempdir().unwrap();

    // 1. 16-bit WAV test
    let wav16_path = dir.path().join("test16.wav");
    let expected_16: Vec<(i16, i16)> = vec![
        (0, 0),
        (1, -1),
        (32767, -32768),
        (-12345, 23456),
        (16384, -16384),
    ];
    fs::write(&wav16_path, build_wav_s16le(44_100, &expected_16)).unwrap();

    let mut dec16 = SymphoniaAudioDecoder::open_path(&wav16_path).unwrap();
    assert_eq!(
        dec16.format(),
        AudioFormat::Pcm {
            sample_rate: 44_100,
            channels: 2,
            sample_format: SampleFormat::S16Le,
            bits_per_sample: 16,
        }
    );

    let mut decoded_16 = Vec::new();
    while let Some(chunk) = dec16.read_chunk().unwrap() {
        match chunk {
            AudioBuffer::PcmI32 {
                samples,
                channels,
                sample_rate,
                bits_per_sample,
            } => {
                assert_eq!(channels, 2);
                assert_eq!(sample_rate, 44_100);
                assert_eq!(bits_per_sample, 16);
                for pair in samples.chunks_exact(2) {
                    decoded_16.push(((pair[0] >> 16) as i16, (pair[1] >> 16) as i16));
                }
            }
            other => panic!("Expected PcmI32 for 16-bit WAV, got {other:?}"),
        }
    }
    assert_eq!(decoded_16, expected_16);

    // 2. 24-bit WAV test
    let wav24_path = dir.path().join("test24.wav");
    let expected_24: Vec<(i32, i32)> = vec![
        (0, 0),
        (1, -1),
        (8_388_607, -8_388_608),
        (0x123456, -0x654321),
    ];
    fs::write(&wav24_path, build_wav_s24le(96_000, &expected_24)).unwrap();

    let mut dec24 = SymphoniaAudioDecoder::open_path(&wav24_path).unwrap();
    assert_eq!(
        dec24.format(),
        AudioFormat::Pcm {
            sample_rate: 96_000,
            channels: 2,
            sample_format: SampleFormat::S24_3Le,
            bits_per_sample: 24,
        }
    );

    let mut decoded_24 = Vec::new();
    while let Some(chunk) = dec24.read_chunk().unwrap() {
        match chunk {
            AudioBuffer::PcmI32 {
                samples,
                channels,
                sample_rate,
                bits_per_sample,
            } => {
                assert_eq!(channels, 2);
                assert_eq!(sample_rate, 96_000);
                assert_eq!(bits_per_sample, 24);
                for pair in samples.chunks_exact(2) {
                    decoded_24.push((pair[0] >> 8, pair[1] >> 8));
                }
            }
            other => panic!("Expected PcmI32 for 24-bit WAV, got {other:?}"),
        }
    }
    assert_eq!(decoded_24, expected_24);
}

#[test]
fn test_flac_and_wavpack_pcm_bit_perfect_decoding() {
    let dir = tempfile::tempdir().unwrap();
    let wav24_path = dir.path().join("source24.wav");
    let flac_path = dir.path().join("encoded24.flac");
    let wv_path = dir.path().join("encoded24.wv");

    let expected_24: Vec<(i32, i32)> = (0..256)
        .map(|i| {
            let l = ((i * 32719) % 8_388_607) - 4_000_000;
            let r = -l;
            (l, r)
        })
        .collect();
    fs::write(&wav24_path, build_wav_s24le(88_200, &expected_24)).unwrap();

    // Use ffmpeg (installed on RPi5) to encode the 24-bit WAV into lossless FLAC and lossless WavPack (.wv)
    let flac_status = Command::new("ffmpeg")
        .args([
            "-y",
            "-loglevel",
            "error",
            "-i",
            wav24_path.to_str().unwrap(),
            "-c:a",
            "flac",
            flac_path.to_str().unwrap(),
        ])
        .status()
        .expect("ffmpeg should be runnable");
    assert!(flac_status.success(), "ffmpeg FLAC encode failed");

    let wv_status = Command::new("ffmpeg")
        .args([
            "-y",
            "-loglevel",
            "error",
            "-i",
            wav24_path.to_str().unwrap(),
            "-c:a",
            "wavpack",
            "-bits_per_raw_sample",
            "24",
            wv_path.to_str().unwrap(),
        ])
        .status()
        .expect("ffmpeg should be runnable");
    assert!(wv_status.success(), "ffmpeg WavPack encode failed");

    // Verify FLAC bit-exactness against original 24-bit integer samples
    let mut flac_dec = SymphoniaAudioDecoder::open_path(&flac_path).unwrap();
    assert_eq!(
        flac_dec.format(),
        AudioFormat::Pcm {
            sample_rate: 88_200,
            channels: 2,
            sample_format: SampleFormat::S24_3Le,
            bits_per_sample: 24,
        }
    );
    let mut flac_samples = Vec::new();
    while let Some(chunk) = flac_dec.read_chunk().unwrap() {
        let AudioBuffer::PcmI32 { samples, .. } = chunk else {
            panic!("Expected PcmI32 from FLAC");
        };
        for pair in samples.chunks_exact(2) {
            flac_samples.push((pair[0] >> 8, pair[1] >> 8));
        }
    }
    assert_eq!(
        flac_samples, expected_24,
        "FLAC 24-bit decode must be bit-exact"
    );

    // Verify WavPack PCM bit-exactness against original 24-bit integer samples
    let mut wv_dec = SymphoniaAudioDecoder::open_path(&wv_path).unwrap();
    assert_eq!(
        wv_dec.format(),
        AudioFormat::Pcm {
            sample_rate: 88_200,
            channels: 2,
            sample_format: SampleFormat::S24_3Le,
            bits_per_sample: 24,
        }
    );
    let mut wv_samples = Vec::new();
    while let Some(chunk) = wv_dec.read_chunk().unwrap() {
        let AudioBuffer::PcmI32 { samples, .. } = chunk else {
            panic!("Expected PcmI32 from WavPack PCM");
        };
        for pair in samples.chunks_exact(2) {
            wv_samples.push((pair[0] >> 8, pair[1] >> 8));
        }
    }
    assert_eq!(
        wv_samples, expected_24,
        "WavPack 24-bit PCM decode must be bit-exact"
    );
}

#[test]
fn test_wavpack_dsd64_bit_perfect_passthrough() {
    let dir = tempfile::tempdir().unwrap();
    let wv_dsd_path = dir.path().join("dsd64.wv");

    let expected_lr: Vec<(u8, u8)> = (0..128)
        .map(|i| {
            (
                (i as u8).wrapping_mul(0x55) ^ 0xA5,
                (i as u8).wrapping_mul(0x33) ^ 0x69,
            )
        })
        .collect();
    let wv_bytes = build_wavpack_dsd64(&expected_lr);
    fs::write(&wv_dsd_path, &wv_bytes).unwrap();

    let mut dec = SymphoniaAudioDecoder::open_path(&wv_dsd_path).unwrap();
    assert_eq!(
        dec.format(),
        AudioFormat::Dsd {
            rate: DsdRate::Dsd64,
            channels: 2,
            transport: kiiaud_core::DsdTransport::Native,
            bit_order: BitOrder::MsbFirst,
            layout: ChannelLayout::Interleaved,
        }
    );

    let chunk = dec
        .read_chunk()
        .unwrap()
        .expect("should read WavPack DSD chunk");
    match chunk {
        AudioBuffer::Dsd {
            data,
            rate,
            channels,
            bit_order,
            layout,
        } => {
            assert_eq!(rate, DsdRate::Dsd64);
            assert_eq!(channels, 2);
            assert_eq!(bit_order, BitOrder::MsbFirst);
            assert_eq!(layout, ChannelLayout::Interleaved);
            let pairs: Vec<(u8, u8)> = data.chunks_exact(2).map(|c| (c[0], c[1])).collect();
            assert_eq!(
                pairs, expected_lr,
                "WavPack DSD bitstream must be bit-exact"
            );

            // Also verify DoP packing of the decoded WavPack DSD chunk
            let mut dop = DopEncoder::new(rate, 2, layout, bit_order).unwrap();
            let mut dop_out = Vec::new();
            dop.encode(&data, &mut dop_out);
            assert_eq!(dop_out.len() / 2, expected_lr.len() / 2);
            // First DoP frame has marker 0x05, second has 0xFA
            assert_eq!((dop_out[0] >> 24) as u8, 0x05);
            assert_eq!((dop_out[2] >> 24) as u8, 0xFA);
        }
        other => panic!("WavPack DSD must NEVER be converted to PCM, got {other:?}"),
    }
    assert!(dec.read_chunk().unwrap().is_none());
}

#[test]
fn test_dsf_dsd64_bit_perfect_passthrough() {
    let dir = tempfile::tempdir().unwrap();
    let dsf_path = dir.path().join("track.dsf");

    let mut ch0 = [0x69u8; 4096];
    let mut ch1 = [0x96u8; 4096];
    for i in 0..64 {
        ch0[i] = i as u8;
        ch1[i] = (255 - i) as u8;
    }

    let dsf_bytes = build_dsf_dsd64(&ch0, &ch1, 64);
    fs::write(&dsf_path, &dsf_bytes).unwrap();

    let mut dec = SymphoniaAudioDecoder::open_path(&dsf_path).unwrap();
    assert_eq!(
        dec.format(),
        AudioFormat::Dsd {
            rate: DsdRate::Dsd64,
            channels: 2,
            transport: kiiaud_core::DsdTransport::Native,
            bit_order: BitOrder::LsbFirst,
            layout: ChannelLayout::Planar,
        }
    );

    let chunk = dec.read_chunk().unwrap().expect("should read DSF chunk");
    match chunk {
        AudioBuffer::Dsd {
            data,
            rate,
            channels,
            bit_order,
            layout,
        } => {
            assert_eq!(rate, DsdRate::Dsd64);
            assert_eq!(channels, 2);
            assert_eq!(bit_order, BitOrder::LsbFirst);
            assert_eq!(layout, ChannelLayout::Planar);
            // trimmed planar layout: 64 bytes of ch0 followed by 64 bytes of ch1
            assert_eq!(data.len(), 128);
            assert_eq!(&data[..64], &ch0[..64]);
            assert_eq!(&data[64..128], &ch1[..64]);
        }
        other => panic!("DSF must NEVER be converted to PCM, got {other:?}"),
    }
}

#[test]
fn test_dst_explicitly_deferred_and_path_policy_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let dst_path = dir.path().join("compressed.dst");
    fs::write(&dst_path, b"DST ").unwrap();

    let err = SymphoniaAudioDecoder::open_path(&dst_path).err().unwrap();
    assert!(matches!(err, KiiaudError::UnsupportedFormat(_)));

    // Path traversal outside allowed_roots must fail with SecurityViolation
    let policy = PathPolicy::new(vec![dir.path().to_path_buf()]);
    let outside = Path::new("/etc/hosts").to_path_buf();
    let err2 = SymphoniaAudioDecoder::open_source(&AudioSource::LocalFile(outside), &policy)
        .err()
        .unwrap();
    assert!(matches!(err2, KiiaudError::SecurityViolation(_)));
}
