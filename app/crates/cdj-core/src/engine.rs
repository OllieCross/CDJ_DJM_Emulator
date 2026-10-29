//! The audio engine: one output stream that plays the four decks through
//! the mixer.
//!
//! Each deck is a voice owned by the audio thread, which therefore owns the
//! playhead: loops wrap, cues jump and tracks end exactly on the sample.
//! Deck controllers send [`VoiceCommand`]s; the audio thread publishes each
//! voice's position in [`VoiceFeedback`] and reports every beat it crosses as
//! a [`BeatEvent`] stamped with the moment that beat will be heard (output
//! latency included), which is when the network side sends the beat packet.
//!
//! Without an output device the same rendering runs on a timer, so beats,
//! positions and timecode keep working silently.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{bounded, Receiver, Sender};
use signalsmith_stretch::Stretch;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{info, warn};

use crate::grid::BeatGrid;
use crate::track_audio::TrackAudio;

pub const DECKS: usize = 4;
pub const MIX_CHANNELS: usize = 6;
/// Rate the timer-driven (no device) output renders at.
const HEADLESS_RATE: u32 = 48_000;
const HEADLESS_BLOCK: usize = 480;

// ---------------------------------------------------------------------------
// Shared values
// ---------------------------------------------------------------------------

/// An `f32` that can be shared with the audio thread.
#[derive(Debug, Default)]
pub struct AtomicF32(AtomicU32);

impl AtomicF32 {
    pub fn new(v: f32) -> Self {
        Self(AtomicU32::new(v.to_bits()))
    }
    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
    pub fn set(&self, v: f32) {
        self.0.store(v.to_bits(), Ordering::Relaxed);
    }
}

/// Linear gains the mixer applies per channel, set by the mixer controls.
#[derive(Debug)]
pub struct ChannelGains {
    /// Trim x channel fader x crossfader.
    pub level: AtomicF32,
    /// Low, mid, high isolator gains.
    pub eq: [AtomicF32; 3],
}

impl Default for ChannelGains {
    fn default() -> Self {
        Self {
            level: AtomicF32::new(1.0),
            eq: [AtomicF32::new(1.0), AtomicF32::new(1.0), AtomicF32::new(1.0)],
        }
    }
}

#[derive(Debug)]
pub struct MixGains {
    pub channels: [ChannelGains; MIX_CHANNELS],
    pub master: AtomicF32,
}

impl Default for MixGains {
    fn default() -> Self {
        Self {
            channels: Default::default(),
            master: AtomicF32::new(1.0),
        }
    }
}

/// A beat a deck reached, and when it will be heard.
#[derive(Debug, Clone, Copy)]
pub struct BeatEvent {
    /// Grid index (0-based) of the beat.
    pub index: i64,
    pub bar_beat: u8,
    pub at: Instant,
}

/// Sent from a deck controller to its voice on the audio thread.
pub enum VoiceCommand {
    /// Put a track on the deck. `audio` is `None` for a deck that plays
    /// silently (metadata-only track, or tempo without a track).
    Load {
        audio: Option<Arc<TrackAudio>>,
        grid: Arc<BeatGrid>,
        /// Known length; playback stops there. `None` plays forever.
        length_ms: Option<f64>,
        position_ms: f64,
    },
    SetPlaying(bool),
    Seek(f64),
    /// 1.0 = normal speed; negative plays in reverse.
    SetSpeed(f64),
    /// Active loop, in track ms.
    SetLoop(Option<(f64, f64)>),
    SetBeatSink(Option<UnboundedSender<BeatEvent>>),
    /// Master tempo (key lock): change tempo without changing pitch.
    SetMasterTempo(bool),
}

/// What the audio thread reports about one deck.
#[derive(Debug)]
pub struct VoiceFeedback {
    epoch: Instant,
    /// Position (ms, f64 bits) that is audible at `stamp_ns`.
    position_ms: AtomicU64,
    stamp_ns: AtomicU64,
    speed: AtomicU64,
    /// Speed the playhead is heard moving at towards `position_ms` until
    /// `stamp_ns` (0 after a jump or from a standstill), and where that
    /// movement began.
    approach: AtomicU64,
    approach_from_ms: AtomicU64,
    playing: AtomicBool,
    ended: AtomicBool,
}

impl VoiceFeedback {
    fn new(epoch: Instant) -> Self {
        Self {
            epoch,
            position_ms: AtomicU64::new(0f64.to_bits()),
            stamp_ns: AtomicU64::new(0),
            speed: AtomicU64::new(1f64.to_bits()),
            approach: AtomicU64::new(0f64.to_bits()),
            approach_from_ms: AtomicU64::new(0f64.to_bits()),
            playing: AtomicBool::new(false),
            ended: AtomicBool::new(false),
        }
    }

    fn publish(&self, position_ms: f64, audible_at: Instant, speed: f64, approach: (f64, f64), playing: bool, ended: bool) {
        let stamp = audible_at.saturating_duration_since(self.epoch).as_nanos() as u64;
        self.position_ms.store(position_ms.to_bits(), Ordering::Relaxed);
        self.speed.store(speed.to_bits(), Ordering::Relaxed);
        self.approach.store(approach.0.to_bits(), Ordering::Relaxed);
        self.approach_from_ms.store(approach.1.to_bits(), Ordering::Relaxed);
        self.playing.store(playing, Ordering::Relaxed);
        self.ended.store(ended, Ordering::Relaxed);
        self.stamp_ns.store(stamp, Ordering::Release);
    }

    /// The position being heard right now, in track ms. The published
    /// position is heard at its stamp, a little in the future: until then
    /// what's heard is the audio before it, still moving towards it (so the
    /// position runs smoothly across blocks, and glides into a pause).
    pub fn position_ms(&self) -> f64 {
        let stamp = self.stamp_ns.load(Ordering::Acquire);
        let pos = f64::from_bits(self.position_ms.load(Ordering::Relaxed));
        let now = Instant::now().saturating_duration_since(self.epoch).as_nanos() as f64;
        let since_ms = (now - stamp as f64) / 1e6;
        if since_ms < 0.0 {
            let approach = f64::from_bits(self.approach.load(Ordering::Relaxed));
            let from = f64::from_bits(self.approach_from_ms.load(Ordering::Relaxed));
            let heard = pos + since_ms * approach;
            if approach >= 0.0 {
                heard.max(from)
            } else {
                heard.min(from)
            }
        } else if self.playing() {
            pos + since_ms * f64::from_bits(self.speed.load(Ordering::Relaxed))
        } else {
            pos
        }
    }

    /// The playhead is moving.
    pub fn playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    /// Playback stopped at the end of the track.
    pub fn ended(&self) -> bool {
        self.ended.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// Audio thread
// ---------------------------------------------------------------------------

struct Voice {
    audio: Option<Arc<TrackAudio>>,
    grid: Arc<BeatGrid>,
    length_ms: Option<f64>,
    pos_ms: f64,
    playing: bool,
    ended: bool,
    speed: f64,
    loop_ms: Option<(f64, f64)>,
    beat_sink: Option<UnboundedSender<BeatEvent>>,
    /// Grid index of the last beat at or before `pos_ms`, and when the next
    /// one is due.
    last_beat: i64,
    next_beat_ms: f64,
    master_tempo: bool,
    /// Time-stretcher for master tempo, whether it ran last block, and at
    /// what speed.
    stretch: Stretch,
    stretching: bool,
    stretch_speed: f64,
    /// After a jump the stretcher holds stale audio; refill it first.
    needs_prime: bool,
    /// Fraction of an input frame carried between blocks.
    carry: f64,
    stretch_in: Vec<f32>,
    /// The path that sounded before the stretcher started or stopped,
    /// still playing under the new one.
    handover: Option<Handover>,
    handover_out: Vec<f32>,
    /// Speed the playhead moved at through the last block without jumping
    /// (0 if it stood still or jumped since: seek, loop), and the heard
    /// position that movement started from.
    motion: f64,
    motion_from_ms: f64,
    /// The playhead wrapped round a loop during this block.
    wrapped: bool,
}

/// Switching between direct playback and the stretcher (master tempo on or
/// off, or the stretcher restarting after a jump) without a gap or a jump in
/// what's heard: the outgoing path plays on alone for `hold` frames, while a
/// freshly started stretcher only fades in, then fades out over `fade`.
#[derive(Debug, Clone, Copy)]
struct Handover {
    /// The outgoing path is the stretcher (else direct playback).
    stretched: bool,
    /// Where the outgoing path reads: the heard position for direct
    /// playback, the stretcher's input position for the stretcher.
    pos_ms: f64,
    hold: usize,
    fade_left: usize,
    fade: usize,
}

/// Length of the crossfade between the two paths.
const HANDOVER_FADE_MS: f64 = 10.0;

impl Voice {
    fn new(rate: u32) -> Self {
        let grid = Arc::new(BeatGrid::constant(12000, 0.0));
        let mut v = Self {
            audio: None,
            grid,
            length_ms: Some(0.0),
            pos_ms: 0.0,
            playing: false,
            ended: false,
            speed: 1.0,
            loop_ms: None,
            beat_sink: None,
            last_beat: 0,
            next_beat_ms: 0.0,
            master_tempo: false,
            stretch: Stretch::preset_default(2, rate),
            stretching: false,
            stretch_speed: 1.0,
            needs_prime: true,
            carry: 0.0,
            stretch_in: Vec::with_capacity(32_768),
            handover: None,
            handover_out: Vec::with_capacity(8192),
            motion: 0.0,
            motion_from_ms: 0.0,
            wrapped: false,
        };
        v.resync_beat();
        v
    }

    /// Recompute the beat bookkeeping after a jump (no beat is reported).
    fn resync_beat(&mut self) {
        self.last_beat = self.grid.index_at(self.pos_ms);
        self.next_beat_ms = self.grid.time_of(self.last_beat + 1);
    }

    fn apply(&mut self, cmd: VoiceCommand, trash: &Sender<Arc<TrackAudio>>) {
        match cmd {
            VoiceCommand::Load {
                audio,
                grid,
                length_ms,
                position_ms,
            } => {
                if let Some(old) = std::mem::replace(&mut self.audio, audio) {
                    // Freeing a decoded track is too slow for this thread.
                    let _ = trash.try_send(old);
                }
                self.grid = grid;
                self.length_ms = length_ms;
                self.pos_ms = position_ms;
                self.playing = false;
                self.ended = false;
                self.loop_ms = None;
                self.needs_prime = true;
                self.motion = 0.0;
                self.resync_beat();
            }
            VoiceCommand::SetPlaying(p) => {
                if p && !self.playing {
                    self.needs_prime = true;
                }
                self.playing = p;
                if p {
                    self.ended = false;
                }
            }
            VoiceCommand::Seek(ms) => {
                self.pos_ms = ms.max(0.0);
                self.ended = false;
                self.needs_prime = true;
                self.motion = 0.0;
                self.resync_beat();
            }
            VoiceCommand::SetSpeed(s) => self.speed = s,
            VoiceCommand::SetLoop(l) => {
                self.loop_ms = l;
                // A loop that now ends behind the playhead (halved, or OUT
                // snapped back to a beat) takes it back in, keeping its
                // place in the loop.
                if let Some((start, end)) = l {
                    let len = end - start;
                    if len > 0.0 && self.pos_ms >= end {
                        self.pos_ms = start + (self.pos_ms - start) % len;
                        self.motion = 0.0;
                        self.resync_beat();
                    }
                }
            }
            VoiceCommand::SetBeatSink(s) => self.beat_sink = s,
            VoiceCommand::SetMasterTempo(on) => self.master_tempo = on,
        }
    }

    /// Master tempo applies: playing forwards with audio. (Reverse play is
    /// always varispeed.)
    fn stretch_active(&self) -> bool {
        self.master_tempo && self.playing && self.speed > 0.0 && self.audio.is_some()
    }

    /// Delay the stretcher adds between reading audio and it being heard,
    /// in output frames, at `speed`.
    fn stretch_delay_frames(&self, speed: f64) -> f64 {
        self.stretch.input_latency() as f64 / speed + self.stretch.output_latency() as f64
    }

    /// The position being heard at the start of the next block: while the
    /// stretcher runs, behind where it reads by its delay.
    fn heard_ms(&self, ms_per_frame: f64) -> f64 {
        if self.stretching && !self.needs_prime {
            let behind = self.stretch_delay_frames(self.stretch_speed) * self.stretch_speed * ms_per_frame;
            match self.loop_ms {
                // Read position wrapped round the loop; what's heard hasn't.
                Some((start, end)) if end > start && self.pos_ms >= start && self.pos_ms - behind < start => {
                    end - (start - (self.pos_ms - behind)) % (end - start)
                }
                _ => self.pos_ms - behind,
            }
        } else {
            self.pos_ms
        }
    }

    /// `ms` moved on by `step_ms` through the active loop, without the side
    /// effects of [`next_frame`](Self::next_frame).
    fn advance(&self, ms: f64, step_ms: f64) -> f64 {
        let next = ms + step_ms;
        match self.loop_ms {
            Some((start, end)) if end > start && step_ms > 0.0 && ms < end && next >= end => start + (next - end) % (end - start),
            _ => next,
        }
    }

    /// Produce `frames` stereo frames into `out` (interleaved). The first
    /// is heard at `audible_at`.
    fn render(&mut self, frames: usize, ms_per_frame: f64, frame_time: Duration, audible_at: Instant, out: &mut Vec<f32>) {
        out.clear();
        let moving = self.playing;
        let heard = self.heard_ms(ms_per_frame);
        self.wrapped = false;
        if self.stretch_active() {
            self.render_stretched(frames, ms_per_frame, frame_time, audible_at, out);
        } else {
            if self.stretching {
                self.leave_stretch(ms_per_frame);
            }
            let mut at = audible_at;
            for _ in 0..frames {
                let s = self.next_frame(self.speed * ms_per_frame, at);
                out.extend_from_slice(&s);
                at += frame_time;
            }
        }
        self.mix_handover(frames, ms_per_frame, out);
        if moving && !self.wrapped {
            if self.motion == 0.0 {
                self.motion_from_ms = heard;
            }
            self.motion = self.speed;
        } else {
            self.motion = 0.0;
        }
    }

    /// The stretcher stops (master tempo off, pause, reverse): carry on from
    /// what's being heard, not from where the stretcher read up to. Beats up
    /// to there were already reported, so while playing on the beat
    /// bookkeeping stays.
    fn leave_stretch(&mut self, ms_per_frame: f64) {
        let read = self.pos_ms;
        let heard = self.heard_ms(ms_per_frame);
        self.stretching = false;
        self.handover = None;
        if self.needs_prime {
            // A jump replaced the position anyway.
            return;
        }
        self.pos_ms = heard;
        if self.playing && self.audio.is_some() {
            let fade = (HANDOVER_FADE_MS / ms_per_frame) as usize;
            self.handover = Some(Handover {
                stretched: true,
                pos_ms: read,
                hold: 0,
                fade_left: fade,
                fade,
            });
        } else {
            self.resync_beat();
        }
    }

    /// Master tempo: read the track at its own pitch (1x) and let the
    /// stretcher fit `frames * speed` frames of it into `frames`.
    fn render_stretched(&mut self, frames: usize, ms_per_frame: f64, frame_time: Duration, audible_at: Instant, out: &mut Vec<f32>) {
        if !self.stretching || self.needs_prime {
            // The stretcher's first output only fades in: play the track
            // directly from here until it's ready.
            let heard = self.pos_ms;
            self.prime(ms_per_frame, frame_time, audible_at);
            let fade = (HANDOVER_FADE_MS / ms_per_frame) as usize;
            self.handover = Some(Handover {
                stretched: false,
                pos_ms: heard,
                hold: self.stretch.output_latency(),
                fade_left: fade,
                fade,
            });
        }
        self.stretching = true;
        self.needs_prime = false;
        self.stretch_speed = self.speed;

        let speed = self.speed;
        let wanted = frames as f64 * speed + self.carry;
        let inputs = wanted.floor() as usize;
        self.carry = wanted - inputs as f64;
        // Beats read now are heard after the stretcher's delay.
        let delay = frame_time.mul_f64(self.stretch_delay_frames(speed));
        self.stretch_in.clear();
        for k in 0..inputs {
            let at = audible_at + frame_time.mul_f64(k as f64 / speed) + delay;
            let s = self.next_frame(ms_per_frame, at);
            self.stretch_in.extend_from_slice(&s);
        }
        out.resize(frames * 2, 0.0);
        self.stretch.process(&self.stretch_in, &mut out[..]);
    }

    /// Restart the stretcher so that what it plays is heard from the current
    /// position on. Audio it reads is heard after its delay, so it reads
    /// ahead by that much, through the loop and reporting the beats passed
    /// at the moment they'll be heard; that audio pre-rolls it.
    fn prime(&mut self, ms_per_frame: f64, frame_time: Duration, audible_at: Instant) {
        self.stretch.reset();
        self.carry = 0.0;
        let speed = self.speed;
        let ahead = (self.stretch_delay_frames(speed) * speed).round() as usize;
        self.stretch_in.clear();
        for k in 0..ahead {
            let s = self.next_frame(ms_per_frame, audible_at + frame_time.mul_f64(k as f64 / speed));
            self.stretch_in.extend_from_slice(&s);
        }
        self.stretch.seek(&self.stretch_in, speed);
    }

    /// Play the outgoing path of a [`Handover`] under `out` and fade it out.
    fn mix_handover(&mut self, frames: usize, ms_per_frame: f64, out: &mut [f32]) {
        let Some(mut h) = self.handover.take() else {
            return;
        };
        if !self.playing {
            return;
        }
        self.handover_out.clear();
        if h.stretched {
            let inputs = (frames as f64 * self.stretch_speed).round() as usize;
            self.stretch_in.clear();
            for _ in 0..inputs {
                let s = self.sample_at(h.pos_ms);
                self.stretch_in.extend_from_slice(&s);
                h.pos_ms = self.advance(h.pos_ms, ms_per_frame);
            }
            self.handover_out.resize(frames * 2, 0.0);
            self.stretch.process(&self.stretch_in, &mut self.handover_out[..]);
        } else {
            let step = self.speed * ms_per_frame;
            for _ in 0..frames {
                let s = self.sample_at(h.pos_ms);
                self.handover_out.extend_from_slice(&s);
                h.pos_ms = self.advance(h.pos_ms, step);
            }
        }
        for (frame, old) in out.chunks_mut(2).zip(self.handover_out.chunks(2)) {
            let g = if h.hold > 0 {
                h.hold -= 1;
                1.0
            } else if h.fade_left > 0 {
                h.fade_left -= 1;
                h.fade_left as f32 / h.fade.max(1) as f32
            } else {
                0.0
            };
            frame[0] = frame[0] * (1.0 - g) + old[0] * g;
            frame[1] = frame[1] * (1.0 - g) + old[1] * g;
        }
        if h.hold > 0 || h.fade_left > 0 {
            self.handover = Some(h);
        }
    }

    /// The end of the track, if known: the decoded length once decoding has
    /// finished, else what the library says.
    fn end_ms(&self) -> Option<f64> {
        match &self.audio {
            Some(a) => match a.total_frames() {
                Some(frames) => Some(frames as f64 * 1000.0 / f64::from(a.sample_rate)),
                None => self.length_ms,
            },
            None => self.length_ms,
        }
    }

    fn sample_at(&self, ms: f64) -> [f32; 2] {
        let Some(audio) = &self.audio else {
            return [0.0; 2];
        };
        let pos = ms * f64::from(audio.sample_rate) / 1000.0;
        if pos < 0.0 {
            return [0.0; 2];
        }
        let i = pos.floor() as u64;
        let frac = (pos - i as f64) as f32;
        let Some(a) = audio.frame(i) else {
            return [0.0; 2];
        };
        let b = audio.frame(i + 1).unwrap_or(a);
        [a[0] + (b[0] - a[0]) * frac, a[1] + (b[1] - a[1]) * frac]
    }

    /// Produce one frame and advance the playhead by `step_ms` (negative in
    /// reverse). `at` is when this frame is heard.
    fn next_frame(&mut self, step_ms: f64, at: Instant) -> [f32; 2] {
        if !self.playing {
            return [0.0; 2];
        }
        let out = self.sample_at(self.pos_ms);
        let mut next = self.pos_ms + step_ms;

        let mut jumped = false;
        if let Some((start, end)) = self.loop_ms {
            let len = end - start;
            if len > 0.0 {
                if step_ms > 0.0 && self.pos_ms < end && next >= end {
                    next = start + (next - end) % len;
                    jumped = true;
                } else if step_ms < 0.0 && self.pos_ms >= start && next < start {
                    next = end - (start - next) % len;
                    jumped = true;
                }
            }
        }

        if let Some(end) = self.end_ms() {
            if step_ms > 0.0 && next >= end {
                next = end;
                self.playing = false;
                self.ended = true;
            }
        }
        if next < 0.0 {
            next = 0.0;
            self.playing = false;
        }

        if jumped {
            self.wrapped = true;
            // A loop restarting on a beat plays that beat again.
            self.pos_ms = next;
            let i = self.grid.index_at(next);
            if next - self.grid.time_of(i) < step_ms.abs() {
                self.report_beat(i, at);
            }
            self.resync_beat();
        } else if step_ms > 0.0 && next >= self.next_beat_ms {
            self.pos_ms = next;
            self.resync_beat();
            self.report_beat(self.last_beat, at);
        } else {
            self.pos_ms = next;
            if step_ms < 0.0 {
                self.resync_beat();
            }
        }
        out
    }

    fn report_beat(&self, index: i64, at: Instant) {
        if let Some(sink) = &self.beat_sink {
            let _ = sink.send(BeatEvent {
                index,
                bar_beat: self.grid.bar_beat(index),
                at,
            });
        }
    }
}

/// Second-order section (RBJ cookbook), direct form I.
#[derive(Debug, Clone, Copy, Default)]
struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    x: [[f32; 2]; 2],
    y: [[f32; 2]; 2],
}

impl Biquad {
    fn butterworth(rate: f32, freq: f32, high_pass: bool) -> Self {
        let w = 2.0 * std::f32::consts::PI * freq / rate;
        let alpha = w.sin() / (2.0 * std::f32::consts::FRAC_1_SQRT_2);
        let cos = w.cos();
        let a0 = 1.0 + alpha;
        let b = if high_pass {
            [(1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0]
        } else {
            [(1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0]
        };
        Self {
            b: b.map(|v| v / a0),
            a: [-2.0 * cos / a0, (1.0 - alpha) / a0],
            ..Default::default()
        }
    }

    fn process(&mut self, x: [f32; 2]) -> [f32; 2] {
        let mut y = [0.0; 2];
        for c in 0..2 {
            y[c] = self.b[0] * x[c] + self.b[1] * self.x[0][c] + self.b[2] * self.x[1][c]
                - self.a[0] * self.y[0][c]
                - self.a[1] * self.y[1][c];
            self.x[1][c] = self.x[0][c];
            self.x[0][c] = x[c];
            self.y[1][c] = self.y[0][c];
            self.y[0][c] = y[c];
        }
        y
    }
}

/// Fourth-order Linkwitz-Riley filter: two Butterworth sections.
#[derive(Debug, Clone, Copy, Default)]
struct Lr4([Biquad; 2]);

impl Lr4 {
    fn new(rate: f32, freq: f32, high_pass: bool) -> Self {
        Self([Biquad::butterworth(rate, freq, high_pass); 2])
    }

    fn process(&mut self, x: [f32; 2]) -> [f32; 2] {
        let y = self.0[0].process(x);
        self.0[1].process(y)
    }
}

fn add(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] + b[0], a[1] + b[1]]
}

/// DJM-style 3-band isolator: Linkwitz-Riley crossovers at 250 Hz and
/// 2.5 kHz. The low band goes through the 2.5 kHz crossover's allpass so the
/// bands stay in phase and sum flat; a band at gain 0 is gone.
#[derive(Debug, Clone, Copy, Default)]
struct Isolator {
    low: Lr4,
    rest: Lr4,
    mid: Lr4,
    high: Lr4,
    low_allpass: [Lr4; 2],
}

const LOW_CROSSOVER_HZ: f32 = 250.0;
const HIGH_CROSSOVER_HZ: f32 = 2500.0;

impl Isolator {
    fn new(rate: u32) -> Self {
        let r = rate as f32;
        Self {
            low: Lr4::new(r, LOW_CROSSOVER_HZ, false),
            rest: Lr4::new(r, LOW_CROSSOVER_HZ, true),
            mid: Lr4::new(r, HIGH_CROSSOVER_HZ, false),
            high: Lr4::new(r, HIGH_CROSSOVER_HZ, true),
            low_allpass: [Lr4::new(r, HIGH_CROSSOVER_HZ, false), Lr4::new(r, HIGH_CROSSOVER_HZ, true)],
        }
    }

    fn process(&mut self, x: [f32; 2], gains: [f32; 3]) -> [f32; 2] {
        let low = self.low.process(x);
        let low = add(self.low_allpass[0].process(low), self.low_allpass[1].process(low));
        let rest = self.rest.process(x);
        let mid = self.mid.process(rest);
        let high = self.high.process(rest);
        [
            low[0] * gains[0] + mid[0] * gains[1] + high[0] * gains[2],
            low[1] * gains[0] + mid[1] * gains[1] + high[1] * gains[2],
        ]
    }
}

/// Renders all decks through the mixer. Lives on the audio thread.
pub(crate) struct Renderer {
    rate: u32,
    voices: Vec<Voice>,
    commands: Vec<Receiver<VoiceCommand>>,
    feedback: Vec<Arc<VoiceFeedback>>,
    gains: Arc<MixGains>,
    eq: Vec<Isolator>,
    /// Smoothed per-channel levels and master, to avoid zipper noise.
    level: Vec<f32>,
    master: f32,
    trash: Sender<Arc<TrackAudio>>,
    /// One voice's block, then the mix.
    voice_buf: Vec<f32>,
    scratch: Vec<f32>,
}

impl Renderer {
    /// Called before a stream starts (never from the audio callback): the
    /// stretchers are rebuilt for the new rate.
    fn set_rate(&mut self, rate: u32) {
        self.rate = rate;
        self.eq = vec![Isolator::new(rate); DECKS];
        for voice in &mut self.voices {
            voice.stretch = Stretch::preset_default(2, rate);
            voice.needs_prime = true;
        }
    }

    /// Mix `frames` stereo frames into `self.scratch`. The first frame is
    /// heard at `audible_at`.
    fn render(&mut self, frames: usize, audible_at: Instant) {
        for (voice, rx) in self.voices.iter_mut().zip(&self.commands) {
            while let Ok(cmd) = rx.try_recv() {
                voice.apply(cmd, &self.trash);
            }
        }
        self.scratch.clear();
        self.scratch.resize(frames * 2, 0.0);
        let ms_per_frame = 1000.0 / f64::from(self.rate);
        let frame_time = Duration::from_secs_f64(1.0 / f64::from(self.rate));
        // Approach targets over ~5 ms.
        let smoothing = 1.0 - (-1.0 / (0.005 * self.rate as f32)).exp();

        for (d, voice) in self.voices.iter_mut().enumerate() {
            self.feedback[d].publish(
                voice.heard_ms(ms_per_frame),
                audible_at,
                voice.speed,
                (voice.motion, voice.motion_from_ms),
                voice.playing,
                voice.ended,
            );
            voice.render(frames, ms_per_frame, frame_time, audible_at, &mut self.voice_buf);
            let ch = &self.gains.channels[d];
            let target = ch.level.get();
            let eq = [ch.eq[0].get(), ch.eq[1].get(), ch.eq[2].get()];
            for (f, s) in self.voice_buf.chunks(2).enumerate() {
                self.level[d] += (target - self.level[d]) * smoothing;
                let s = self.eq[d].process([s[0], s[1]], eq);
                self.scratch[f * 2] += s[0] * self.level[d];
                self.scratch[f * 2 + 1] += s[1] * self.level[d];
            }
        }
        let target = self.gains.master.get();
        for frame in self.scratch.chunks_mut(2) {
            self.master += (target - self.master) * smoothing;
            frame[0] = (frame[0] * self.master).clamp(-1.0, 1.0);
            frame[1] = (frame[1] * self.master).clamp(-1.0, 1.0);
        }
    }
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// A running output (device stream or timer thread); dropping it stops it.
struct Output {
    stop: Sender<()>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Drop for Output {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
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
        (v * i16::MAX as f32) as i16
    }
}
impl FromF32 for u16 {
    fn from_f32(v: f32) -> Self {
        ((v * 0.5 + 0.5) * u16::MAX as f32) as u16
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    renderer: Arc<Mutex<Renderer>>,
) -> anyhow::Result<cpal::Stream>
where
    T: cpal::SizedSample + FromF32 + Send + 'static,
{
    let channels = config.channels as usize;
    let stream = device.build_output_stream(
        config,
        move |out: &mut [T], info: &cpal::OutputCallbackInfo| {
            let ts = info.timestamp();
            let latency = ts.playback.duration_since(&ts.callback).unwrap_or_default();
            let audible_at = Instant::now() + latency;
            let frames = out.len() / channels;
            let mut r = renderer.lock().unwrap();
            r.render(frames, audible_at);
            for (f, frame) in out.chunks_mut(channels).enumerate() {
                for (c, sample) in frame.iter_mut().enumerate() {
                    let v = if c < 2 { r.scratch[f * 2 + c] } else { 0.0 };
                    *sample = T::from_f32(v);
                }
            }
        },
        |e| warn!("audio output error: {e}"),
        None,
    )?;
    Ok(stream)
}

/// Run the output stream for `device_name` (default device if `None`) on its
/// own thread, which owns the (non-`Send`) CPAL stream.
fn start_device(renderer: Arc<Mutex<Renderer>>, device_name: Option<String>) -> anyhow::Result<(Output, String)> {
    let (ready_tx, ready_rx) = bounded::<anyhow::Result<String>>(1);
    let (stop_tx, stop_rx) = bounded::<()>(1);
    let thread = thread::Builder::new()
        .name("cdj-audio-out".into())
        .spawn(move || {
            let result = (|| {
                let host = cpal::default_host();
                let device = match &device_name {
                    Some(name) => host
                        .output_devices()?
                        .find(|d| d.name().is_ok_and(|n| &n == name))
                        .ok_or_else(|| anyhow!("output device {name:?} not found"))?,
                    None => host.default_output_device().context("no default audio output device")?,
                };
                let name = device.name().unwrap_or_else(|_| "unknown".into());
                let supported = device.default_output_config().context("default output config")?;
                let config = cpal::StreamConfig {
                    channels: supported.channels(),
                    sample_rate: supported.sample_rate(),
                    buffer_size: cpal::BufferSize::Default,
                };
                renderer.lock().unwrap().set_rate(config.sample_rate.0);
                let stream = match supported.sample_format() {
                    cpal::SampleFormat::F32 => build_stream::<f32>(&device, &config, renderer.clone())?,
                    cpal::SampleFormat::I16 => build_stream::<i16>(&device, &config, renderer.clone())?,
                    cpal::SampleFormat::U16 => build_stream::<u16>(&device, &config, renderer.clone())?,
                    other => anyhow::bail!("unsupported sample format {other:?}"),
                };
                stream.play().context("starting output stream")?;
                info!(device = %name, rate = config.sample_rate.0, "audio output started");
                Ok((stream, name))
            })();
            match result {
                Ok((stream, name)) => {
                    let _ = ready_tx.send(Ok(name));
                    let _ = stop_rx.recv();
                    drop(stream);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
        })
        .context("spawning audio output thread")?;
    let name = ready_rx.recv().context("audio output thread died")??;
    Ok((
        Output {
            stop: stop_tx,
            thread: Some(thread),
        },
        name,
    ))
}

/// Render on a timer with nowhere to play to.
fn start_timer(renderer: Arc<Mutex<Renderer>>) -> anyhow::Result<Output> {
    let (stop_tx, stop_rx) = bounded::<()>(1);
    renderer.lock().unwrap().set_rate(HEADLESS_RATE);
    let block = Duration::from_secs_f64(HEADLESS_BLOCK as f64 / f64::from(HEADLESS_RATE));
    let thread = thread::Builder::new()
        .name("cdj-audio-timer".into())
        .spawn(move || {
            let mut next = Instant::now();
            loop {
                renderer.lock().unwrap().render(HEADLESS_BLOCK, next);
                next += block;
                match stop_rx.recv_timeout(next.saturating_duration_since(Instant::now())) {
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                    _ => return,
                }
            }
        })
        .context("spawning audio timer thread")?;
    Ok(Output {
        stop: stop_tx,
        thread: Some(thread),
    })
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

pub struct AudioEngine {
    commands: Vec<Sender<VoiceCommand>>,
    feedback: Vec<Arc<VoiceFeedback>>,
    gains: Arc<MixGains>,
    renderer: Arc<Mutex<Renderer>>,
    output: Mutex<Option<Output>>,
    device: Mutex<Option<String>>,
}

impl AudioEngine {
    /// An engine with no output yet; call [`start_output`](Self::start_output)
    /// or [`start_silent`](Self::start_silent).
    pub fn new() -> Self {
        let epoch = Instant::now();
        let (commands, receivers): (Vec<_>, Vec<_>) = (0..DECKS).map(|_| bounded(256)).unzip();
        let feedback: Vec<_> = (0..DECKS).map(|_| Arc::new(VoiceFeedback::new(epoch))).collect();
        let gains = Arc::new(MixGains::default());
        let (trash_tx, trash_rx) = bounded::<Arc<TrackAudio>>(16);
        let _ = thread::Builder::new()
            .name("cdj-audio-trash".into())
            .spawn(move || for audio in trash_rx { drop(audio) });
        let renderer = Renderer {
            rate: HEADLESS_RATE,
            voices: (0..DECKS).map(|_| Voice::new(HEADLESS_RATE)).collect(),
            commands: receivers,
            feedback: feedback.clone(),
            gains: gains.clone(),
            eq: vec![Isolator::new(HEADLESS_RATE); DECKS],
            level: vec![1.0; DECKS],
            master: 1.0,
            trash: trash_tx,
            voice_buf: Vec::with_capacity(8192),
            scratch: Vec::with_capacity(8192),
        };
        Self {
            commands,
            feedback,
            gains,
            renderer: Arc::new(Mutex::new(renderer)),
            output: Mutex::new(None),
            device: Mutex::new(None),
        }
    }

    /// Play through `device` (`None`: the system default). If that fails,
    /// keep time silently and return the error.
    pub fn start_output(&self, device: Option<&str>) -> anyhow::Result<String> {
        let mut output = self.output.lock().unwrap();
        *output = None;
        match start_device(self.renderer.clone(), device.map(str::to_owned)) {
            Ok((out, name)) => {
                *output = Some(out);
                *self.device.lock().unwrap() = Some(name.clone());
                Ok(name)
            }
            Err(e) => {
                *output = Some(start_timer(self.renderer.clone())?);
                *self.device.lock().unwrap() = None;
                Err(e)
            }
        }
    }

    /// Keep time without playing audio.
    pub fn start_silent(&self) -> anyhow::Result<()> {
        let mut output = self.output.lock().unwrap();
        *output = None;
        *output = Some(start_timer(self.renderer.clone())?);
        *self.device.lock().unwrap() = None;
        Ok(())
    }

    /// The output device in use, if any.
    pub fn device(&self) -> Option<String> {
        self.device.lock().unwrap().clone()
    }

    pub fn output_devices() -> Vec<String> {
        cpal::default_host()
            .output_devices()
            .map(|devs| devs.filter_map(|d| d.name().ok()).collect())
            .unwrap_or_default()
    }

    /// Send a command to deck `deck` (0-based).
    pub fn send(&self, deck: usize, cmd: VoiceCommand) {
        if let Some(tx) = self.commands.get(deck) {
            if tx.try_send(cmd).is_err() {
                warn!(deck, "audio command queue full; command dropped");
            }
        }
    }

    pub fn feedback(&self, deck: usize) -> Arc<VoiceFeedback> {
        self.feedback[deck].clone()
    }

    pub fn gains(&self) -> &MixGains {
        &self.gains
    }

    /// Render `frames` frames synchronously (tests drive time this way).
    #[cfg(test)]
    pub(crate) fn render_now(&self, frames: usize) -> Vec<f32> {
        let mut r = self.renderer.lock().unwrap();
        r.render(frames, Instant::now());
        r.scratch.clone()
    }
}

impl Default for AudioEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: usize = HEADLESS_RATE as usize;

    /// A constant-level stereo track `ms` long at 48 kHz.
    fn track(ms: usize, level: i16) -> Arc<TrackAudio> {
        TrackAudio::from_frames(48_000, &vec![[level, level]; ms * 48])
    }

    fn load(engine: &AudioEngine, deck: usize, audio: Option<Arc<TrackAudio>>, grid: BeatGrid, length_ms: Option<f64>) {
        engine.send(
            deck,
            VoiceCommand::Load {
                audio,
                grid: Arc::new(grid),
                length_ms,
                position_ms: 0.0,
            },
        );
    }

    fn position(engine: &AudioEngine, deck: usize) -> f64 {
        // Feedback is published at the start of each render.
        engine.render_now(0);
        f64::from_bits(engine.feedback[deck].position_ms.load(Ordering::Relaxed))
    }

    #[test]
    fn plays_at_speed_and_mixes_through_gains() {
        let engine = AudioEngine::new();
        load(&engine, 0, Some(track(2000, 16384)), BeatGrid::constant(12000, 0.0), None);
        engine.send(0, VoiceCommand::SetPlaying(true));
        let out = engine.render_now(RATE / 2);
        let settled = out[out.len() - 2];
        assert!((settled - 0.5).abs() < 1e-3, "track level passes through: {settled}");
        assert!((position(&engine, 0) - 500.0).abs() < 1e-6);

        engine.send(0, VoiceCommand::SetSpeed(1.5));
        engine.render_now(RATE / 2);
        assert!((position(&engine, 0) - 1250.0).abs() < 1e-6);

        engine.gains().channels[0].level.set(0.0);
        let out = engine.render_now(RATE / 10);
        assert!(out[out.len() - 1].abs() < 1e-3, "fader closed, level smoothed to 0");
    }

    #[test]
    fn stops_at_the_end_and_wraps_loops() {
        let engine = AudioEngine::new();
        load(&engine, 1, Some(track(1000, 1000)), BeatGrid::constant(12000, 0.0), None);
        engine.send(1, VoiceCommand::SetPlaying(true));
        engine.render_now(RATE * 2);
        assert_eq!(position(&engine, 1), 1000.0);
        assert!(engine.feedback(1).ended() && !engine.feedback(1).playing());

        engine.send(1, VoiceCommand::Seek(100.0));
        engine.send(1, VoiceCommand::SetLoop(Some((100.0, 600.0))));
        engine.send(1, VoiceCommand::SetPlaying(true));
        engine.render_now(RATE * 7 / 10); // 700 ms of a 500 ms loop
        assert!((position(&engine, 1) - 300.0).abs() < 1e-6);
    }

    #[test]
    fn reports_each_beat_crossed_with_its_bar_position() {
        let engine = AudioEngine::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        // Silent deck (no audio), 120 BPM grid from 250 ms, plays forever.
        load(&engine, 2, None, BeatGrid::constant(12000, 250.0), None);
        engine.send(2, VoiceCommand::SetBeatSink(Some(tx)));
        engine.send(2, VoiceCommand::SetPlaying(true));
        let start = Instant::now();
        engine.render_now(RATE * 2);
        let beats: Vec<BeatEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(beats.iter().map(|b| b.index).collect::<Vec<_>>(), [0, 1, 2, 3]);
        assert_eq!(beats.iter().map(|b| b.bar_beat).collect::<Vec<_>>(), [1, 2, 3, 4]);
        let gap = beats[1].at - beats[0].at;
        assert!((gap.as_secs_f64() - 0.5).abs() < 1e-3, "beats 500 ms apart: {gap:?}");
        assert!(beats[0].at >= start + Duration::from_millis(249));
    }

    /// A sine track at `freq` Hz, `ms` long, at 48 kHz.
    fn sine(freq: f64, ms: usize) -> Arc<TrackAudio> {
        let frames: Vec<[i16; 2]> = (0..ms * 48)
            .map(|i| {
                let v = ((2.0 * std::f64::consts::PI * freq * i as f64 / 48_000.0).sin() * 16_000.0) as i16;
                [v, v]
            })
            .collect();
        TrackAudio::from_frames(48_000, &frames)
    }

    /// Frequency of the left channel of the last half of `out`, from its
    /// rising zero crossings.
    fn frequency(out: &[f32]) -> f64 {
        let left: Vec<f32> = out.iter().step_by(2).copied().collect();
        let tail = &left[left.len() / 2..];
        let crossings = tail.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        crossings as f64 / (tail.len() as f64 / 48_000.0)
    }

    fn play_sine_at(speed: f64, master_tempo: bool) -> (AudioEngine, Vec<f32>) {
        let engine = AudioEngine::new();
        load(&engine, 0, Some(sine(440.0, 10_000)), BeatGrid::constant(12000, 0.0), None);
        engine.send(0, VoiceCommand::SetSpeed(speed));
        engine.send(0, VoiceCommand::SetMasterTempo(master_tempo));
        engine.send(0, VoiceCommand::SetPlaying(true));
        // Render in device-sized blocks, as the output does.
        let mut out = Vec::new();
        for _ in 0..(RATE * 2 / 512) {
            out.extend(engine.render_now(512));
        }
        (engine, out)
    }

    #[test]
    fn master_tempo_keeps_pitch_while_tempo_changes() {
        let (_, varispeed) = play_sine_at(1.1, false);
        let f = frequency(&varispeed);
        assert!((f - 484.0).abs() < 5.0, "varispeed raises the pitch: {f} Hz");

        let (engine, locked) = play_sine_at(1.1, true);
        let f = frequency(&locked);
        assert!((f - 440.0).abs() < 5.0, "key lock keeps 440 Hz: {f} Hz");
        // The stretcher reads ahead by its delay, so what's heard is where
        // varispeed would be: the time rendered at 1.1x.
        let rendered_ms = (RATE * 2 / 512 * 512) as f64 * 1000.0 / RATE as f64;
        let heard = position(&engine, 0);
        assert!((heard - rendered_ms * 1.1).abs() < 1.0, "heard position {heard} ms");
    }

    #[test]
    fn heard_position_runs_smoothly_and_glides_into_a_pause() {
        let engine = AudioEngine::new();
        load(&engine, 0, Some(track(5000, 1000)), BeatGrid::constant(12000, 0.0), None);
        engine.send(0, VoiceCommand::SetPlaying(true));
        let fb = engine.feedback(0);
        let latency = Duration::from_millis(50);
        let mut r = engine.renderer.lock().unwrap();
        // 10 ms blocks, each heard 50 ms after it's rendered: the published
        // position runs 50 ms ahead of what's heard.
        r.render(480, Instant::now() + latency);
        r.render(480, Instant::now() + latency);
        assert_eq!(fb.position_ms(), 0.0, "nothing heard before playback started");
        for _ in 0..20 {
            r.render(480, Instant::now() + latency);
        }
        // 220 ms rendered; the last block, from 210 ms, is heard in 50 ms.
        let heard = fb.position_ms();
        assert!((heard - 160.0).abs() < 5.0, "heard {heard} ms, not the 210 ms read");
        engine.send(0, VoiceCommand::SetPlaying(false));
        r.render(480, Instant::now() + latency);
        let gliding = fb.position_ms();
        assert!(gliding < 220.0, "still hearing the audio before the pause: {gliding}");
        std::thread::sleep(latency + Duration::from_millis(5));
        let stopped = fb.position_ms();
        assert!((stopped - 220.0).abs() < 1e-6, "stops where playback stopped: {stopped}");
    }

    #[test]
    fn a_loop_shortened_behind_the_playhead_keeps_it_in() {
        let engine = AudioEngine::new();
        load(&engine, 0, Some(track(10_000, 1000)), BeatGrid::constant(12000, 0.0), None);
        engine.send(0, VoiceCommand::SetLoop(Some((1000.0, 3000.0))));
        engine.send(0, VoiceCommand::Seek(1000.0));
        engine.send(0, VoiceCommand::SetPlaying(true));
        engine.render_now(RATE * 3 / 2); // 1.5 s into the 2 s loop
        engine.send(0, VoiceCommand::SetLoop(Some((1000.0, 2000.0))));
        assert!((position(&engine, 0) - 1500.0).abs() < 1e-3, "halved: same place in the loop");
        engine.send(0, VoiceCommand::SetLoop(Some((1000.0, 1200.0))));
        assert!((position(&engine, 0) - 1100.0).abs() < 1e-3, "shorter still: wrapped, 500 ms in is 100 ms in");
        engine.render_now(RATE * 2);
        let pos = position(&engine, 0);
        assert!((1000.0..1200.0).contains(&pos), "still looping: {pos}");
    }

    /// Silence with a 1 kHz burst from 500 ms to 600 ms, at 48 kHz.
    fn burst() -> Arc<TrackAudio> {
        let frames: Vec<[i16; 2]> = (0..2000 * 48)
            .map(|i| {
                let t = i as f64 / 48_000.0;
                let v = if (0.5..0.6).contains(&t) {
                    ((2.0 * std::f64::consts::PI * 1000.0 * t).sin() * 16_000.0) as i16
                } else {
                    0
                };
                [v, v]
            })
            .collect();
        TrackAudio::from_frames(48_000, &frames)
    }

    /// Plays [`burst`] from the start in 512-frame blocks, sending
    /// `switch(block)` before each block, and returns when the burst is
    /// first heard at half its level, in ms.
    fn burst_heard_at(mut switch: impl FnMut(&AudioEngine, usize)) -> f64 {
        let engine = AudioEngine::new();
        load(&engine, 0, Some(burst()), BeatGrid::constant(12000, 0.0), None);
        engine.send(0, VoiceCommand::SetPlaying(true));
        let mut left = Vec::new();
        for block in 0..(RATE / 512) {
            switch(&engine, block);
            left.extend(engine.render_now(512).iter().step_by(2).copied());
        }
        let onset = left.iter().position(|s| s.abs() > 0.25).expect("burst heard");
        onset as f64 * 1000.0 / RATE as f64
    }

    #[test]
    fn master_tempo_switches_without_a_jump() {
        let varispeed = burst_heard_at(|_, _| {});
        assert!((varispeed - 500.0).abs() < 1.0, "direct: {varispeed} ms");

        let from_start = burst_heard_at(|e, b| {
            if b == 0 {
                e.send(0, VoiceCommand::SetMasterTempo(true));
            }
        });
        let on_while_playing = burst_heard_at(|e, b| {
            if b == 20 {
                e.send(0, VoiceCommand::SetMasterTempo(true));
            }
        });
        let off_while_playing = burst_heard_at(|e, b| match b {
            0 => e.send(0, VoiceCommand::SetMasterTempo(true)),
            25 => e.send(0, VoiceCommand::SetMasterTempo(false)),
            _ => {}
        });
        for (what, at) in [("on from the start", from_start), ("on while playing", on_while_playing), ("off while playing", off_while_playing)] {
            assert!((at - 500.0).abs() < 5.0, "master tempo {what}: burst heard at {at} ms");
        }
    }

    #[test]
    fn master_tempo_beats_keep_the_tempo() {
        let engine = AudioEngine::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        load(&engine, 0, Some(sine(440.0, 10_000)), BeatGrid::constant(12000, 0.0), None);
        engine.send(0, VoiceCommand::SetBeatSink(Some(tx)));
        engine.send(0, VoiceCommand::SetSpeed(1.25));
        engine.send(0, VoiceCommand::SetMasterTempo(true));
        engine.send(0, VoiceCommand::SetPlaying(true));
        let start = Instant::now();
        let mut r = engine.renderer.lock().unwrap();
        for block in 0..(RATE * 3 / 512) {
            // Pretend each block is heard 512 frames after the previous one.
            r.render(512, start + Duration::from_secs_f64(block as f64 * 512.0 / 48_000.0));
        }
        drop(r);
        let beats: Vec<BeatEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(beats.len() >= 6, "{} beats", beats.len());
        for pair in beats.windows(2) {
            let gap = (pair[1].at - pair[0].at).as_secs_f64();
            assert!((gap - 0.4).abs() < 0.001, "500 ms beats at 1.25x are 400 ms apart, got {gap}");
        }
    }

    /// Runs the real default output device for half a second, playing
    /// silence (no track loaded). Manual: `cargo test -- --ignored`.
    #[test]
    #[ignore = "uses the audio hardware"]
    fn device_output_keeps_time() {
        let engine = AudioEngine::new();
        let device = engine.start_output(None).expect("default output device");
        load(&engine, 0, None, BeatGrid::constant(12000, 0.0), None);
        engine.send(0, VoiceCommand::SetPlaying(true));
        std::thread::sleep(Duration::from_millis(600));
        let pos = engine.feedback(0).position_ms();
        assert!((300.0..700.0).contains(&pos), "{device}: deck clock ran {pos} ms in 600 ms");
    }

    /// Steady-state gain (output RMS / input RMS) of a sine at `freq`
    /// through the isolator.
    fn isolator_gain(freq: f32, gains: [f32; 3]) -> f32 {
        let mut iso = Isolator::new(48_000);
        let (mut sum_in, mut sum_out) = (0.0f32, 0.0f32);
        for i in 0..48_000 {
            let x = (2.0 * std::f32::consts::PI * freq * i as f32 / 48_000.0).sin();
            let y = iso.process([x, x], gains)[0];
            if i > 24_000 {
                sum_in += x * x;
                sum_out += y * y;
            }
        }
        (sum_out / sum_in).sqrt()
    }

    #[test]
    fn isolator_sums_flat_and_kills_bands() {
        for freq in [50.0, 250.0, 1000.0, 2500.0, 8000.0] {
            let flat = isolator_gain(freq, [1.0; 3]);
            assert!((flat - 1.0).abs() < 0.01, "{freq} Hz flat: {flat}");
        }
        assert!(isolator_gain(50.0, [0.0, 1.0, 1.0]) < 0.05, "lows killed");
        assert!(isolator_gain(800.0, [1.0, 0.0, 1.0]) < 0.3, "mids killed");
        assert!(isolator_gain(10_000.0, [1.0, 1.0, 0.0]) < 0.05, "highs killed");
    }
}
