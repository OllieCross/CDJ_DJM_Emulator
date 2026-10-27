//! A track's audio, decoded in the background.
//!
//! Decoding fills fixed-size chunks and publishes each one whole, so
//! playback can start (and seek within what is decoded) while the rest of
//! the file is still being read, and the audio thread never waits or locks:
//! a frame is either available or not yet.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;

use anyhow::{anyhow, Context};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use tracing::{debug, warn};

/// Frames per chunk (~0.7 s at 44.1 kHz).
pub const CHUNK_FRAMES: usize = 32_768;
/// Room reserved when the file doesn't say how long it is.
const MAX_TRACK_SECONDS: u64 = 3 * 60 * 60;

pub struct TrackAudio {
    pub sample_rate: u32,
    /// Interleaved stereo i16, `CHUNK_FRAMES` frames per chunk (the last
    /// one may be shorter).
    chunks: Box<[OnceLock<Box<[i16]>>]>,
    decoded_frames: AtomicU64,
    total_frames: OnceLock<u64>,
    error: OnceLock<String>,
}

impl std::fmt::Debug for TrackAudio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrackAudio")
            .field("sample_rate", &self.sample_rate)
            .field("decoded_frames", &self.decoded_frames())
            .field("total_frames", &self.total_frames())
            .finish()
    }
}

impl TrackAudio {
    fn with_capacity(sample_rate: u32, frames: u64) -> Self {
        let chunks = (frames as usize).div_ceil(CHUNK_FRAMES) + 1;
        Self {
            sample_rate,
            chunks: (0..chunks).map(|_| OnceLock::new()).collect(),
            decoded_frames: AtomicU64::new(0),
            total_frames: OnceLock::new(),
            error: OnceLock::new(),
        }
    }

    /// Start decoding `path`. Returns once the format is recognised; the
    /// audio arrives in the background.
    pub fn open(path: &Path) -> anyhow::Result<Arc<Self>> {
        let file = std::fs::File::open(path).with_context(|| format!("opening track: {}", path.display()))?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let probed = symphonia::default::get_probe()
            .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
            .context("probing file format")?;
        let format = probed.format;
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or_else(|| anyhow!("no playable audio track in file"))?;
        let sample_rate = track.codec_params.sample_rate.context("track missing sample rate")?;
        let frames = track
            .codec_params
            .n_frames
            .unwrap_or(MAX_TRACK_SECONDS * u64::from(sample_rate));
        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .context("making decoder")?;
        let track_id = track.id;

        let audio = Arc::new(Self::with_capacity(sample_rate, frames));
        let target = audio.clone();
        let name = path.display().to_string();
        thread::Builder::new()
            .name("cdj-decode".into())
            .spawn(move || {
                if let Err(e) = decode_into(format, decoder, track_id, &target) {
                    warn!(track = %name, "decode failed: {e:#}");
                    let _ = target.error.set(format!("{e:#}"));
                }
                // Whatever was decoded is all there is.
                let _ = target.total_frames.set(target.decoded_frames());
                debug!(track = %name, frames = target.decoded_frames(), "decode finished");
            })
            .context("spawning decoder thread")?;
        Ok(audio)
    }

    /// Already-decoded audio (tests, generated sources).
    pub fn from_frames(sample_rate: u32, frames: &[[i16; 2]]) -> Arc<Self> {
        let audio = Self::with_capacity(sample_rate, frames.len() as u64);
        for (i, chunk) in frames.chunks(CHUNK_FRAMES).enumerate() {
            let samples: Box<[i16]> = chunk.iter().flatten().copied().collect();
            let _ = audio.chunks[i].set(samples);
        }
        audio.decoded_frames.store(frames.len() as u64, Ordering::Release);
        let _ = audio.total_frames.set(frames.len() as u64);
        Arc::new(audio)
    }

    /// Stereo sample at `frame`, or `None` if it isn't decoded (yet) or is
    /// past the end.
    pub fn frame(&self, frame: u64) -> Option<[f32; 2]> {
        let chunk = self.chunks.get(frame as usize / CHUNK_FRAMES)?.get()?;
        let i = (frame as usize % CHUNK_FRAMES) * 2;
        let s = chunk.get(i..i + 2)?;
        Some([f32::from(s[0]) / 32768.0, f32::from(s[1]) / 32768.0])
    }

    pub fn decoded_frames(&self) -> u64 {
        self.decoded_frames.load(Ordering::Acquire)
    }

    /// Length in frames, once decoding has finished.
    pub fn total_frames(&self) -> Option<u64> {
        self.total_frames.get().copied()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.get().map(String::as_str)
    }
}

fn to_i16(v: f32) -> i16 {
    (v * 32768.0).round().clamp(-32768.0, 32767.0) as i16
}

fn decode_into(
    mut format: Box<dyn symphonia::core::formats::FormatReader>,
    mut decoder: Box<dyn symphonia::core::codecs::Decoder>,
    track_id: u32,
    audio: &Arc<TrackAudio>,
) -> anyhow::Result<()> {
    let mut chunk: Vec<i16> = Vec::with_capacity(CHUNK_FRAMES * 2);
    let mut next_chunk = 0usize;
    let mut buffer: Option<SampleBuffer<f32>> = None;

    let publish = |chunk: &mut Vec<i16>, next_chunk: &mut usize| -> anyhow::Result<()> {
        if chunk.is_empty() {
            return Ok(());
        }
        let slot = audio.chunks.get(*next_chunk).context("track longer than its reserved length")?;
        let frames = chunk.len() as u64 / 2;
        let _ = slot.set(std::mem::take(chunk).into_boxed_slice());
        chunk.reserve(CHUNK_FRAMES * 2);
        audio.decoded_frames.fetch_add(frames, Ordering::Release);
        *next_chunk += 1;
        Ok(())
    };

    loop {
        // Nobody wants this track any more (it was unloaded).
        if Arc::strong_count(audio) == 1 {
            return Ok(());
        }
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(symphonia::core::errors::Error::DecodeError(e)) => {
                warn!("decode error (skipping packet): {e}");
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        let spec = *decoded.spec();
        let channels = spec.channels.count().max(1);
        let buf = match &mut buffer {
            Some(b) if b.capacity() >= decoded.capacity() * channels => b,
            _ => buffer.insert(SampleBuffer::new(decoded.capacity() as u64, spec)),
        };
        buf.copy_interleaved_ref(decoded);
        for frame in buf.samples().chunks(channels) {
            // Mono to both sides; more than two channels: left = even, right = odd.
            let (l, r) = match frame {
                [m] => (*m, *m),
                [l, r] => (*l, *r),
                many => {
                    let half = many.len() as f32 / 2.0;
                    let l: f32 = many.iter().step_by(2).sum();
                    let r: f32 = many.iter().skip(1).step_by(2).sum();
                    (l / half, r / half)
                }
            };
            chunk.push(to_i16(l));
            chunk.push(to_i16(r));
            if chunk.len() == CHUNK_FRAMES * 2 {
                publish(&mut chunk, &mut next_chunk)?;
            }
        }
    }
    publish(&mut chunk, &mut next_chunk)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Write a 16-bit stereo WAV whose left channel counts frames (mod
    /// 32768) and right channel is its negation.
    pub(crate) fn write_test_wav(path: &Path, rate: u32, frames: u32) {
        let data_len = frames * 4;
        let mut out = Vec::with_capacity(44 + data_len as usize);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&2u16.to_le_bytes()); // stereo
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 4).to_le_bytes());
        out.extend_from_slice(&4u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        for i in 0..frames {
            let v = (i % 32768) as i16;
            out.extend_from_slice(&v.to_le_bytes());
            out.extend_from_slice(&(-v).to_le_bytes());
        }
        std::fs::write(path, out).unwrap();
    }

    fn wait_complete(audio: &TrackAudio) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while audio.total_frames().is_none() {
            assert!(Instant::now() < deadline, "decode didn't finish");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn decodes_a_wav_into_chunks() {
        let dir = std::env::temp_dir().join(format!("cdj-track-audio-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("count.wav");
        let frames = CHUNK_FRAMES as u32 * 2 + 1000;
        write_test_wav(&path, 44_100, frames);

        let audio = TrackAudio::open(&path).unwrap();
        assert_eq!(audio.sample_rate, 44_100);
        wait_complete(&audio);
        assert_eq!(audio.total_frames(), Some(u64::from(frames)));
        assert!(audio.error().is_none());

        let at = |f: u64| audio.frame(f).unwrap();
        assert_eq!(at(0), [0.0, 0.0]);
        let f = CHUNK_FRAMES as u64 + 5;
        let expected = ((f % 32768) as f32) / 32768.0;
        assert_eq!(at(f), [expected, -expected], "frames line up across chunk boundaries");
        assert!(audio.frame(u64::from(frames)).is_none(), "past the end");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn missing_file_errors() {
        let err = TrackAudio::open(Path::new("/nonexistent/track.flac")).unwrap_err();
        assert!(format!("{err:#}").contains("opening track"));
    }

    #[test]
    fn from_frames_is_complete_immediately() {
        let audio = TrackAudio::from_frames(48_000, &[[100, -100]; 10]);
        assert_eq!(audio.total_frames(), Some(10));
        assert!(audio.frame(9).is_some() && audio.frame(10).is_none());
    }
}
