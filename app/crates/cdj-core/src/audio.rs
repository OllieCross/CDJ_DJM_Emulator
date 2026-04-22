//! Audio decode + output per virtual CDJ.
//!
//! ## M2.2 scope
//!
//! * Decode a single local file via Symphonia (mp3, aac/m4a, flac, wav, alac).
//! * Play it out the default CPAL output device.
//! * Advance [`PlayerState::playhead_frames`] from inside the output callback
//!   so a future beat clock can phase-lock to real playback.
//!
//! ## Why a dedicated std::thread?
//!
//! `cpal::Stream` is `!Send` on macOS, so we cannot move it out of the thread
//! that built it. The engine pins itself to a thread which owns the stream
//! for its lifetime and parks on a `crossbeam-channel` receiver until
//! `stop` is requested. Dropping the returned [`AudioHandle`] fires that
//! stop signal and joins the thread.
//!
//! ## Deliberate simplifications (follow-up work)
//!
//! * **Preload, don't stream.** The whole file is decoded into memory up
//!   front. Fine for dev tracks under ~10 minutes.
//! * **Per-player CPAL stream.** Each virtual CDJ opens its own output
//!   stream on the default device; CoreAudio mixes them. The proper
//!   virtual-DJM mix bus lands in M2.4.
//! * **Linear resample when device rate != file rate.** Cheap and audible;
//!   rubato drop-in planned for M3.
//! * **Loops the file.** There is no "end of track" behaviour; playback
//!   wraps to the beginning. Cue / stop / seek arrive with the UI.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use anyhow::{anyhow, Context};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, StreamConfig};
use crossbeam_channel::{bounded, Receiver, Sender};
use symphonia::core::audio::{AudioBufferRef, Signal};
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use tracing::{info, warn};

use crate::player_state::PlayerState;

/// A decoded track: interleaved f32 stereo at `source_rate`.
struct DecodedTrack {
    samples: Vec<f32>,
    source_rate: u32,
    channels: u16,
}

/// Handle to an audio engine thread. Drop to stop playback cleanly.
pub struct AudioHandle {
    stop_tx: Sender<()>,
    join: Option<thread::JoinHandle<()>>,
}

impl AudioHandle {
    pub fn spawn(path: PathBuf, state: Arc<PlayerState>) -> anyhow::Result<Self> {
        let (ready_tx, ready_rx) = bounded::<anyhow::Result<()>>(1);
        let (stop_tx, stop_rx) = bounded::<()>(1);

        let join = thread::Builder::new()
            .name("cdj-audio".into())
            .spawn(move || run_engine(path, state, stop_rx, ready_tx))
            .context("spawning audio thread")?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                stop_tx,
                join: Some(join),
            }),
            Ok(Err(e)) => {
                let _ = join.join();
                Err(e)
            }
            Err(_) => Err(anyhow!("audio thread died before signalling readiness")),
        }
    }
}

impl Drop for AudioHandle {
    fn drop(&mut self) {
        let _ = self.stop_tx.send(());
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn run_engine(
    path: PathBuf,
    state: Arc<PlayerState>,
    stop_rx: Receiver<()>,
    ready_tx: Sender<anyhow::Result<()>>,
) {
    let track = match decode_file(&path) {
        Ok(t) => t,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };
    info!(
        path = %path.display(),
        source_rate = track.source_rate,
        channels = track.channels,
        frames = track.samples.len() / track.channels as usize,
        "track decoded"
    );
    state.set_sample_rate(track.source_rate);
    state.set_playhead_frames(0);

    let host = cpal::default_host();
    let device = match host.default_output_device() {
        Some(d) => d,
        None => {
            let _ = ready_tx.send(Err(anyhow!("no default audio output device")));
            return;
        }
    };
    let supported = match device.default_output_config() {
        Ok(c) => c,
        Err(e) => {
            let _ = ready_tx.send(Err(anyhow::Error::from(e).context("default output config")));
            return;
        }
    };
    let device_rate = supported.sample_rate().0;
    let device_channels = supported.channels();
    let sample_format = supported.sample_format();

    let config = StreamConfig {
        channels: device_channels,
        sample_rate: supported.sample_rate(),
        buffer_size: cpal::BufferSize::Default,
    };

    info!(
        device_rate,
        device_channels,
        sample_format = ?sample_format,
        "opening CPAL output stream"
    );

    let stream = match build_stream(&device, &config, sample_format, track, state) {
        Ok(s) => s,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };
    if let Err(e) = stream.play() {
        let _ = ready_tx.send(Err(anyhow::Error::from(e).context("starting CPAL stream")));
        return;
    }
    let _ = ready_tx.send(Ok(()));

    // Park until the handle is dropped.
    let _ = stop_rx.recv();
    drop(stream);
}

fn decode_file(path: &Path) -> anyhow::Result<DecodedTrack> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("opening track: {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .context("probing file format")?;
    let mut format = probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| anyhow!("no playable audio track in file"))?;
    let track_id = track.id;
    let source_rate = track
        .codec_params
        .sample_rate
        .ok_or_else(|| anyhow!("track missing sample rate"))?;
    let channels = track
        .codec_params
        .channels
        .ok_or_else(|| anyhow!("track missing channel layout"))?
        .count() as u16;

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .context("making decoder")?;

    let mut samples: Vec<f32> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(e) => return Err(e.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => append_interleaved_f32(&decoded, &mut samples, channels as usize),
            Err(symphonia::core::errors::Error::DecodeError(e)) => {
                warn!("decode error (skipping packet): {e}");
            }
            Err(e) => return Err(e.into()),
        }
    }

    Ok(DecodedTrack {
        samples,
        source_rate,
        channels,
    })
}

fn append_interleaved_f32(decoded: &AudioBufferRef<'_>, out: &mut Vec<f32>, channels: usize) {
    use symphonia::core::audio::AudioBuffer;

    let mut buf = AudioBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
    decoded.convert(&mut buf);
    let frames = buf.frames();
    for f in 0..frames {
        match channels {
            1 => {
                let s = buf.chan(0)[f];
                out.push(s);
                out.push(s);
            }
            2 => {
                out.push(buf.chan(0)[f]);
                out.push(buf.chan(1)[f]);
            }
            n => {
                let mut l = 0.0f32;
                let mut r = 0.0f32;
                for c in 0..n {
                    let s = buf.chan(c)[f];
                    if c % 2 == 0 {
                        l += s;
                    } else {
                        r += s;
                    }
                }
                let half_n = (n as f32 / 2.0).max(1.0);
                out.push(l / half_n);
                out.push(r / half_n);
            }
        }
    }
}

fn build_stream(
    device: &Device,
    config: &StreamConfig,
    fmt: SampleFormat,
    track: DecodedTrack,
    state: Arc<PlayerState>,
) -> anyhow::Result<cpal::Stream> {
    match fmt {
        SampleFormat::F32 => stream_with::<f32>(device, config, track, state),
        SampleFormat::I16 => stream_with::<i16>(device, config, track, state),
        SampleFormat::U16 => stream_with::<u16>(device, config, track, state),
        other => Err(anyhow!("unsupported CPAL sample format: {other:?}")),
    }
}

fn stream_with<T>(
    device: &Device,
    config: &StreamConfig,
    track: DecodedTrack,
    state: Arc<PlayerState>,
) -> anyhow::Result<cpal::Stream>
where
    T: cpal::SizedSample + FromF32 + Send + 'static,
{
    let source_rate = track.source_rate;
    let device_rate = config.sample_rate.0;
    let channels_out = config.channels as usize;
    let samples = track.samples;
    let ratio = source_rate as f64 / device_rate as f64;
    let mut cursor_f64: f64 = 0.0;
    let total_source_frames = (samples.len() / 2) as f64;

    let err_fn = |e| tracing::warn!("CPAL stream error: {e}");
    let state_cb = state.clone();

    let stream = device.build_output_stream(
        config,
        move |out: &mut [T], _info: &cpal::OutputCallbackInfo| {
            let frames = out.len() / channels_out;
            for frame_idx in 0..frames {
                let src_pos = cursor_f64;
                let i0 = src_pos.floor() as usize;
                let frac = (src_pos - i0 as f64) as f32;

                let (l, r) = if (i0 + 1) * 2 + 1 < samples.len() {
                    let l0 = samples[i0 * 2];
                    let r0 = samples[i0 * 2 + 1];
                    let l1 = samples[(i0 + 1) * 2];
                    let r1 = samples[(i0 + 1) * 2 + 1];
                    (l0 + (l1 - l0) * frac, r0 + (r1 - r0) * frac)
                } else {
                    (0.0f32, 0.0f32)
                };

                for ch in 0..channels_out {
                    let v = if ch == 0 {
                        l
                    } else if ch == 1 {
                        r
                    } else {
                        0.0
                    };
                    out[frame_idx * channels_out + ch] = T::from_f32(v);
                }

                cursor_f64 += ratio;
                if cursor_f64 >= total_source_frames {
                    cursor_f64 = 0.0;
                }
            }
            state_cb.advance_playhead(frames as u64);
        },
        err_fn,
        None,
    )?;
    Ok(stream)
}

trait FromF32 {
    fn from_f32(v: f32) -> Self;
}

impl FromF32 for f32 {
    fn from_f32(v: f32) -> Self {
        v
    }
}
impl FromF32 for i16 {
    fn from_f32(v: f32) -> Self {
        (v.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
    }
}
impl FromF32 for u16 {
    fn from_f32(v: f32) -> Self {
        let s = (v.clamp(-1.0, 1.0) * 0.5 + 0.5) * u16::MAX as f32;
        s as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonexistent_file_errors_cleanly() {
        let state = Arc::new(PlayerState::new(12000));
        let err = AudioHandle::spawn(PathBuf::from("/nonexistent/path.flac"), state).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("opening track"), "got: {msg}");
    }
}
