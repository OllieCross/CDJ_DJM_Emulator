//! Shared mutable state for a single virtual CDJ.
//!
//! Read by the status emitter and beat clock, written by the audio engine
//! (M2.2) and the UI / CLI control layer. All fields are atomic so any task
//! can observe or update them without locking.

use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};

pub struct PlayerState {
    bpm_hundredths: AtomicU16,
    playing: AtomicBool,
    master: AtomicBool,
    on_air: AtomicBool,
    beat_within_bar: AtomicU8,
    /// Ordinal of the next beat to fire; used by the beat clock to keep the
    /// bar counter and (eventually) audio playhead in sync.
    next_beat_ordinal: AtomicU32,
    /// Frames of audio output so far (monotonic, device output domain).
    /// Updated by the audio engine from its CPAL output callback.
    playhead_frames: AtomicU64,
    /// Device output sample rate in Hz (matches `playhead_frames`' domain).
    /// Zero when no track is loaded; the beat clock uses that as "no
    /// playhead - fall back to wall-clock mode".
    sample_rate: AtomicU32,
    /// Musical offset of beat 1 of bar 1 from the start of playback, in
    /// milliseconds. Typically comes from a rekordbox beat-grid; user-
    /// supplied via `--beat-offset-ms` for M2.3.
    beat_grid_offset_ms: AtomicU32,
}

impl PlayerState {
    pub fn new(bpm_hundredths: u16) -> Self {
        Self {
            bpm_hundredths: AtomicU16::new(bpm_hundredths),
            playing: AtomicBool::new(false),
            master: AtomicBool::new(false),
            on_air: AtomicBool::new(false),
            beat_within_bar: AtomicU8::new(1),
            next_beat_ordinal: AtomicU32::new(0),
            playhead_frames: AtomicU64::new(0),
            sample_rate: AtomicU32::new(0),
            beat_grid_offset_ms: AtomicU32::new(0),
        }
    }

    pub fn bpm_hundredths(&self) -> u16 {
        self.bpm_hundredths.load(Ordering::Relaxed)
    }
    pub fn set_bpm_hundredths(&self, v: u16) {
        self.bpm_hundredths.store(v, Ordering::Relaxed);
    }

    pub fn playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }
    pub fn set_playing(&self, v: bool) {
        self.playing.store(v, Ordering::Relaxed);
    }

    pub fn master(&self) -> bool {
        self.master.load(Ordering::Relaxed)
    }
    pub fn set_master(&self, v: bool) {
        self.master.store(v, Ordering::Relaxed);
    }

    pub fn on_air(&self) -> bool {
        self.on_air.load(Ordering::Relaxed)
    }
    pub fn set_on_air(&self, v: bool) {
        self.on_air.store(v, Ordering::Relaxed);
    }

    pub fn beat_within_bar(&self) -> u8 {
        self.beat_within_bar.load(Ordering::Relaxed).max(1).min(4)
    }

    /// Advance the bar position. Returns the new value (1..=4).
    pub fn advance_beat(&self) -> u8 {
        let n = self.beat_within_bar.load(Ordering::Relaxed);
        let next = if n >= 4 { 1 } else { n + 1 };
        self.beat_within_bar.store(next, Ordering::Relaxed);
        self.next_beat_ordinal.fetch_add(1, Ordering::Relaxed);
        next
    }

    pub fn reset_bar(&self) {
        self.beat_within_bar.store(1, Ordering::Relaxed);
        self.next_beat_ordinal.store(0, Ordering::Relaxed);
    }

    pub fn playhead_frames(&self) -> u64 {
        self.playhead_frames.load(Ordering::Relaxed)
    }

    /// Called by the audio engine to advance the playhead after each output
    /// buffer.
    pub fn advance_playhead(&self, frames: u64) {
        self.playhead_frames.fetch_add(frames, Ordering::Relaxed);
    }

    pub fn set_playhead_frames(&self, frames: u64) {
        self.playhead_frames.store(frames, Ordering::Relaxed);
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate.load(Ordering::Relaxed)
    }

    pub fn set_sample_rate(&self, rate: u32) {
        self.sample_rate.store(rate, Ordering::Relaxed);
    }

    pub fn beat_grid_offset_ms(&self) -> u32 {
        self.beat_grid_offset_ms.load(Ordering::Relaxed)
    }

    pub fn set_beat_grid_offset_ms(&self, ms: u32) {
        self.beat_grid_offset_ms.store(ms, Ordering::Relaxed);
    }

    /// Set the bar-position directly (used by the beat clock in phase-locked
    /// mode, where position is derived from the audio playhead rather than a
    /// local counter).
    pub fn set_beat_within_bar(&self, b: u8) {
        self.beat_within_bar.store(b.max(1).min(4), Ordering::Relaxed);
    }
}
