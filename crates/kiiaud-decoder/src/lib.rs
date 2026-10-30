// SPDX-License-Identifier: MIT OR Apache-2.0

//! Audio decoder layer (`kiiaud-decoder`).
//!
//! Uses the Symphonia fork with native support for:
//! - **WavPack (`.wv`)**: Both **WavPack DSD** (lossless DSD bitstream) and **WavPack PCM** (with `.wvc` hybrid correction support)
//! - **FLAC (`.flac`)**: 16/24/32-bit PCM
//! - **WAV / AIFF (`.wav`, `.aif`, `.aiff`)**: Uncompressed PCM
//! - **DSF / DFF (`.dsf`, `.dff`)**: Uncompressed native DSD64/128/256/512
//!
//! Preserves bit-perfect integer samples (`AudioBuffer::PcmI32`) and raw DSD bitstreams (`AudioBuffer::Dsd`)
//! without silent DSD-to-PCM conversion or float loss.

pub mod stream;

use kiiaud_core::{
    AudioBuffer, AudioFormat, AudioSource, BitOrder, ChannelLayout, DsdRate, DsdTransport,
    KiiaudError, PathPolicy, Result, SampleFormat,
};
use std::fs::File;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;
use symphonia::core::audio::{Audio, GenericAudioBufferRef};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::well_known::CODEC_ID_WAVPACK;
use symphonia::core::codecs::audio::{
    AudioDecoder, AudioDecoderOptions, BitOrder as SymBitOrder,
    ChannelDataLayout as SymChannelLayout,
};
use symphonia::core::errors::{Error as SymphoniaError, SeekErrorKind};
use symphonia::core::formats::probe::{Hint, Probe, ProbeOptions};
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, Timestamp};
use symphonia::default::formats::CODEC_TYPE_DSD;

/// Core decoder trait (`docs/ARCHITECTURE.md` Section 6).
pub trait Decoder: Send {
    /// Detected native format of the audio stream.
    fn format(&self) -> AudioFormat;

    /// Total duration of the track, if known.
    fn duration(&self) -> Option<Duration>;

    /// Seek to the target position in the stream.
    fn seek(&mut self, position: Duration) -> Result<()>;

    /// Decode and read the next audio chunk into `buffer`.
    /// Returns the number of frames decoded, or `0` on end-of-stream.
    fn read_chunk(&mut self) -> Result<Option<AudioBuffer>>;
}

/// Shared probe configured with a 16 MiB scan depth (matching `rmpd-player`).
static PROBE: OnceLock<Probe> = OnceLock::new();

fn shared_probe() -> &'static Probe {
    PROBE.get_or_init(|| {
        let mut probe = Probe::new_with_options(&ProbeOptions {
            max_probe_depth: 16 * 1024 * 1024,
            ..ProbeOptions::default()
        });
        symphonia::default::register_enabled_formats(&mut probe);
        probe
    })
}

/// Attach a sibling `.wvc` correction file for local `.wv` WavPack hybrid files.
fn local_file_format_options(path: &Path) -> FormatOptions {
    let opts = FormatOptions::default();
    let is_wv = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("wv"));
    if is_wv {
        symphonia::default::formats::wavpack_with_sibling_correction(path, opts)
    } else {
        opts
    }
}

/// Symphonia-backed bit-perfect decoder for PCM and DSD (including WavPack DSD & DSF/DFF).
pub struct SymphoniaAudioDecoder {
    reader: Box<dyn FormatReader>,
    decoder: Option<Box<dyn AudioDecoder>>,
    track_id: u32,
    format: AudioFormat,
    duration: Option<Duration>,
    ended: bool,
    /// True when the container is raw DSF/DFF (`CODEC_TYPE_DSD`), where packets already contain raw DSD bytes.
    /// False when the container is WavPack DSD (`CODEC_ID_WAVPACK` + `U8` sample format), where `WavPackDecoder`
    /// unpacks the compressed DSD sub-block into `GenericAudioBufferRef::U8`.
    is_raw_dsd_packet: bool,
}

impl SymphoniaAudioDecoder {
    /// Open an [`AudioSource`] after validating against the [`PathPolicy`].
    pub fn open_source(source: &AudioSource, policy: &PathPolicy) -> Result<Self> {
        let mut hint = Hint::new();
        let mut format_opts = FormatOptions::default();

        let mss = match source {
            AudioSource::LocalFile(path) => {
                let valid_path = policy.validate_local_path(path)?;
                if let Some(ext) = valid_path.extension().and_then(|e| e.to_str()) {
                    // Explicitly defer DST if `.dst` extension is encountered
                    if ext.eq_ignore_ascii_case("dst") {
                        return Err(KiiaudError::UnsupportedFormat(
                            "DST decompression is explicitly deferred in MVP".to_owned(),
                        ));
                    }
                    hint.with_extension(ext);
                }
                format_opts = local_file_format_options(&valid_path);
                let file = File::open(&valid_path).map_err(|e| {
                    KiiaudError::Source(format!(
                        "Failed to open file '{}': {e}",
                        valid_path.display()
                    ))
                })?;
                MediaSourceStream::new(Box::new(file), Default::default())
            }
            AudioSource::HttpStream { url, headers } => {
                let (media_source, ext_hint) = stream::open_http_stream(url, headers)?;
                if let Some(ext) = ext_hint.as_deref() {
                    hint.with_extension(ext);
                }
                MediaSourceStream::new(media_source, Default::default())
            }
            AudioSource::TcpStream {
                host,
                port,
                request,
                format_hint,
            } => {
                let (media_source, ext_hint) =
                    stream::open_tcp_stream(host, *port, request, format_hint.clone())?;
                if let Some(ext) = ext_hint.as_deref() {
                    hint.with_extension(ext);
                }
                MediaSourceStream::new(media_source, Default::default())
            }
        };

        Self::from_media_source_stream(mss, &hint, format_opts)
    }

    /// Open a local path directly without path policy restrictions (useful for unit tests).
    pub fn open_path(path: &Path) -> Result<Self> {
        let policy = PathPolicy::default();
        Self::open_source(&AudioSource::LocalFile(path.to_path_buf()), &policy)
    }

    pub fn from_media_source_stream(
        mss: MediaSourceStream<'static>,
        hint: &Hint,
        format_opts: FormatOptions,
    ) -> Result<Self> {
        let reader = shared_probe()
            .probe(hint, mss, format_opts, MetadataOptions::default())
            .map_err(|e| KiiaudError::Decoder(format!("Failed to probe audio format: {e}")))?;

        let track = reader
            .default_track(TrackType::Audio)
            .ok_or_else(|| KiiaudError::Decoder("No audio track found in container".to_owned()))?;

        let track_id = track.id;
        let time_base = track.time_base;

        let audio = match track.codec_params.as_ref() {
            Some(CodecParameters::Audio(audio)) => audio.clone(),
            _ => {
                return Err(KiiaudError::Decoder(
                    "Missing audio codec parameters".to_owned(),
                ));
            }
        };

        let codec_id = audio.codec;
        let sample_rate = audio
            .sample_rate
            .ok_or_else(|| KiiaudError::Decoder("Audio stream has no sample rate".to_owned()))?;
        let channels = audio
            .channels
            .as_ref()
            .map(|c| c.count() as u16)
            .unwrap_or(2);

        let duration = match (track.num_frames, time_base) {
            (Some(n_frames), Some(tb)) => tb
                .calc_time(Timestamp::new(n_frames as i64))
                .map(|t| Duration::from_secs_f64(t.as_secs_f64())),
            _ => None,
        };

        // Determine whether this is DSD (either DSF/DFF `CODEC_TYPE_DSD` or WavPack DSD `CODEC_ID_WAVPACK` + `SampleFormat::U8`)
        let is_raw_dsd_packet = codec_id == CODEC_TYPE_DSD;
        let is_wavpack_dsd = codec_id == CODEC_ID_WAVPACK
            && matches!(
                audio.sample_format,
                Some(symphonia::core::audio::sample::SampleFormat::U8)
            )
            && audio.bits_per_sample == Some(1);

        if is_raw_dsd_packet || is_wavpack_dsd {
            let dsd_rate = DsdRate::from_hz(sample_rate).ok_or_else(|| {
                KiiaudError::UnsupportedFormat(format!(
                    "Unsupported DSD sample rate: {sample_rate} Hz"
                ))
            })?;

            let bit_order = match audio.bit_order {
                Some(SymBitOrder::LsbFirst) => BitOrder::LsbFirst,
                _ => BitOrder::MsbFirst,
            };

            let layout = match audio.channel_data_layout {
                Some(SymChannelLayout::Planar) => ChannelLayout::Planar,
                _ => ChannelLayout::Interleaved,
            };

            // For WavPack DSD, we still need the WavPackDecoder to unpack the compressed DSD blocks!
            // For uncompressed DSF/DFF (`CODEC_TYPE_DSD`), packets already hold the raw DSD bytes,
            // and we must NOT instantiate the DSD-to-PCM decimator (`R-014`).
            let decoder = if is_wavpack_dsd {
                let dec = symphonia::default::get_codecs()
                    .make_audio_decoder(&audio, &AudioDecoderOptions::default().gapless(true))
                    .map_err(|e| {
                        KiiaudError::Decoder(format!("Failed to create WavPack DSD decoder: {e}"))
                    })?;
                Some(dec)
            } else {
                // Reject compressed DST inside DFF if any
                None
            };

            let format = AudioFormat::Dsd {
                rate: dsd_rate,
                channels,
                transport: DsdTransport::Native,
                bit_order,
                layout: if is_wavpack_dsd {
                    // WavPackDecoder outputs planes in GenericAudioBuffer::U8 which we interleave per byte
                    ChannelLayout::Interleaved
                } else {
                    layout
                },
            };

            tracing::info!(
                codec = ?codec_id,
                %format,
                is_wavpack_dsd,
                "opened DSD audio stream (bit-perfect passthrough, no PCM conversion)"
            );

            return Ok(Self {
                reader,
                decoder,
                track_id,
                format,
                duration,
                ended: false,
                is_raw_dsd_packet,
            });
        }

        // PCM path: create Symphonia decoder in gapless mode
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&audio, &AudioDecoderOptions::default().gapless(true))
            .map_err(|e| KiiaudError::Decoder(format!("Failed to create PCM decoder: {e}")))?;

        let actual_rate = decoder.codec_params().sample_rate.unwrap_or(sample_rate);
        let bits = audio
            .bits_per_sample
            .or(audio.bits_per_coded_sample)
            .unwrap_or(16) as u8;

        let sample_format = match (audio.sample_format, bits) {
            (Some(symphonia::core::audio::sample::SampleFormat::F32), _)
            | (Some(symphonia::core::audio::sample::SampleFormat::F64), _) => SampleFormat::F32Le,
            (_, 1..=16) => SampleFormat::S16Le,
            (_, 17..=24) => SampleFormat::S24_3Le,
            _ => SampleFormat::S32Le,
        };

        let format = AudioFormat::Pcm {
            sample_rate: actual_rate,
            channels,
            sample_format,
            bits_per_sample: bits.clamp(16, 32),
        };

        tracing::info!(codec = ?codec_id, %format, "opened PCM audio stream");

        Ok(Self {
            reader,
            decoder: Some(decoder),
            track_id,
            format,
            duration,
            ended: false,
            is_raw_dsd_packet: false,
        })
    }
}

impl Decoder for SymphoniaAudioDecoder {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    fn seek(&mut self, position: Duration) -> Result<()> {
        let secs = position.as_secs_f64();
        let time = Time::try_from_secs_f64(secs)
            .ok_or_else(|| KiiaudError::Decoder("Invalid seek timestamp".to_owned()))?;

        let at_end = self.duration.is_some_and(|d| position >= d);

        match self.reader.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time,
                track_id: Some(self.track_id),
            },
        ) {
            Ok(_) => {}
            Err(SymphoniaError::SeekError(SeekErrorKind::OutOfRange)) if at_end => {
                self.ended = true;
                return Ok(());
            }
            Err(SymphoniaError::SeekError(SeekErrorKind::Unseekable)) => {
                return Err(KiiaudError::Decoder("Stream is not seekable".to_owned()));
            }
            Err(e) => return Err(KiiaudError::Decoder(format!("Seek failed: {e}"))),
        }

        if let Some(dec) = self.decoder.as_mut() {
            dec.reset();
        }
        self.ended = at_end;
        Ok(())
    }

    fn read_chunk(&mut self) -> Result<Option<AudioBuffer>> {
        if self.ended {
            return Ok(None);
        }

        loop {
            let packet = match self.reader.next_packet() {
                Ok(Some(pkt)) => pkt,
                Ok(None) => {
                    self.ended = true;
                    return Ok(None);
                }
                Err(SymphoniaError::IoError(e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    self.ended = true;
                    return Ok(None);
                }
                Err(e) => {
                    return Err(KiiaudError::Decoder(format!("Packet read error: {e}")));
                }
            };

            if packet.track_id != self.track_id {
                continue;
            }

            // Raw DSF / DFF packet passthrough
            if self.is_raw_dsd_packet {
                if packet.data.is_empty() {
                    continue;
                }
                let AudioFormat::Dsd {
                    rate,
                    channels,
                    bit_order,
                    layout,
                    ..
                } = self.format
                else {
                    unreachable!();
                };
                return Ok(Some(AudioBuffer::Dsd {
                    data: packet.data.to_vec(),
                    rate,
                    channels,
                    bit_order,
                    layout,
                }));
            }

            let Some(dec) = self.decoder.as_mut() else {
                return Err(KiiaudError::Decoder("Decoder not initialized".to_owned()));
            };

            let decoded = match dec.decode(&packet) {
                Ok(buf) => buf,
                Err(SymphoniaError::DecodeError(msg)) => {
                    tracing::warn!("skipping corrupt audio packet: {msg}");
                    continue;
                }
                Err(e) => {
                    return Err(KiiaudError::Decoder(format!("Audio decode error: {e}")));
                }
            };

            let frames = decoded.frames();
            if frames == 0 {
                continue;
            }

            // Check if this is WavPack DSD (decoded into GenericAudioBufferRef::U8)
            if let AudioFormat::Dsd {
                rate,
                channels,
                bit_order,
                ..
            } = self.format
            {
                let GenericAudioBufferRef::U8(u8_buf) = decoded else {
                    return Err(KiiaudError::Decoder(
                        "WavPack DSD decoder returned non-U8 buffer".to_owned(),
                    ));
                };
                let interleaved_dsd: Vec<u8> = u8_buf.iter_interleaved().collect();
                return Ok(Some(AudioBuffer::Dsd {
                    data: interleaved_dsd,
                    rate,
                    channels,
                    bit_order,
                    layout: ChannelLayout::Interleaved,
                }));
            }

            // PCM path: extract exact integer samples (MSB-aligned in i32) or F32
            let AudioFormat::Pcm {
                sample_rate,
                channels,
                bits_per_sample,
                ..
            } = self.format
            else {
                unreachable!();
            };

            return Ok(Some(extract_pcm_buffer(
                decoded,
                sample_rate,
                channels,
                bits_per_sample,
            )));
        }
    }
}

/// Extract decoded PCM samples into [`AudioBuffer`] without lossy intermediate float conversions
/// for integer audio formats (FLAC, WAV, AIFF, WavPack PCM).
fn extract_pcm_buffer(
    decoded: GenericAudioBufferRef<'_>,
    sample_rate: u32,
    channels: u16,
    bits_per_sample: u8,
) -> AudioBuffer {
    let frames = decoded.frames();
    let ch = channels as usize;
    let total_samples = frames * ch;

    match decoded {
        GenericAudioBufferRef::S16(buf) => {
            // Left-align 16-bit into 32-bit MSB (`<< 16`)
            let out: Vec<i32> = buf.iter_interleaved().map(|s| (s as i32) << 16).collect();
            AudioBuffer::PcmI32 {
                samples: out,
                channels,
                sample_rate,
                bits_per_sample: 16,
            }
        }
        GenericAudioBufferRef::S24(buf) => {
            // `i24::into_i32()` returns the signed 24-bit value in `[-8388608, 8388607]`;
            // left-align into 32-bit MSB (`<< 8`)
            let out: Vec<i32> = buf.iter_interleaved().map(|s| s.inner() << 8).collect();
            AudioBuffer::PcmI32 {
                samples: out,
                channels,
                sample_rate,
                bits_per_sample: 24,
            }
        }
        GenericAudioBufferRef::S32(buf) => {
            let out: Vec<i32> = buf.iter_interleaved().collect();
            AudioBuffer::PcmI32 {
                samples: out,
                channels,
                sample_rate,
                bits_per_sample,
            }
        }
        GenericAudioBufferRef::F32(buf) => {
            let out: Vec<f32> = buf.iter_interleaved().collect();
            AudioBuffer::PcmF32 {
                samples: out,
                channels,
                sample_rate,
            }
        }
        other => {
            let mut out = Vec::with_capacity(total_samples);
            other.copy_to_vec_interleaved(&mut out);
            AudioBuffer::PcmF32 {
                samples: out,
                channels,
                sample_rate,
            }
        }
    }
}
