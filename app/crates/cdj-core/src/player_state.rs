//! Shared mutable state for a single virtual CDJ.
//!
//! Read by the status emitter and beat clock, written by the audio engine
//! and the UI / CLI control layer. Atomic fields for lock-free hot paths;
//! `loaded_track` uses an RwLock because it holds an Arc and is only written
//! when the user explicitly loads or unloads a track.

use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, RwLock};

use crate::library::Library;
use crate::media::Slot;

/// The track on a player: which export it came from and where that export
/// is inserted (the player clients query for metadata).
#[derive(Debug, Clone)]
pub struct LoadedTrack {
    pub library: Arc<Library>,
    pub track_id: u32,
    pub source_player: u8,
    pub source_slot: Slot,
}

pub struct PlayerState {
    pub bpm_hundredths: AtomicU16,
    playing: AtomicBool,
    master: AtomicBool,
    synced: AtomicBool,
    on_air: AtomicBool,
    beat_within_bar: AtomicU8,
    next_beat_ordinal: AtomicU32,
    playhead_frames: AtomicU64,
    sample_rate: AtomicU32,
    beat_grid_offset_ms: AtomicU32,
    /// Currently loaded track. Written on load/unload.
    loaded_track: RwLock<Option<LoadedTrack>>,
}

impl PlayerState {
    pub fn new(bpm_hundredths: u16) -> Self {
        Self {
            bpm_hundredths: AtomicU16::new(bpm_hundredths),
            playing: AtomicBool::new(false),
            master: AtomicBool::new(false),
            synced: AtomicBool::new(false),
            on_air: AtomicBool::new(false),
            beat_within_bar: AtomicU8::new(1),
            next_beat_ordinal: AtomicU32::new(0),
            playhead_frames: AtomicU64::new(0),
            sample_rate: AtomicU32::new(0),
            beat_grid_offset_ms: AtomicU32::new(0),
            loaded_track: RwLock::new(None),
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

    pub fn synced(&self) -> bool {
        self.synced.load(Ordering::Relaxed)
    }
    pub fn set_synced(&self, v: bool) {
        self.synced.store(v, Ordering::Relaxed);
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

    /// Absolute beat number since the player started (1-based).
    /// Used by the CDJ status packet so BLT can show Time/Remain.
    pub fn beat_number(&self) -> u32 {
        self.next_beat_ordinal.load(Ordering::Relaxed) + 1
    }

    pub fn playhead_frames(&self) -> u64 {
        self.playhead_frames.load(Ordering::Relaxed)
    }

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

    pub fn set_beat_within_bar(&self, b: u8) {
        self.beat_within_bar.store(b.max(1).min(4), Ordering::Relaxed);
    }

    /// Set the absolute beat ordinal explicitly. Used by the phase-locked
    /// `BeatClock` after firing a beat so the next CDJ status packet reports
    /// the correct beat number.
    pub fn set_beat_ordinal(&self, ord: u32) {
        self.next_beat_ordinal.store(ord, Ordering::Relaxed);
    }

    // --- Track loading ---

    /// Load a track onto this player. Updates BPM from the track's stored
    /// tempo.
    pub fn load_track(&self, track: LoadedTrack) {
        if let Some(info) = track.library.track_by_id(track.track_id) {
            self.bpm_hundredths.store(info.bpm_hundredths, Ordering::Relaxed);
        }
        // Reset beat/playhead so Time/Remain starts from the track beginning.
        self.next_beat_ordinal.store(0, Ordering::Relaxed);
        self.beat_within_bar.store(1, Ordering::Relaxed);
        self.playhead_frames.store(0, Ordering::Relaxed);
        *self.loaded_track.write().unwrap() = Some(track);
    }

    pub fn unload_track(&self) {
        *self.loaded_track.write().unwrap() = None;
    }

    /// The loaded track. Cheap: clones the library `Arc`, not its data.
    pub fn loaded_track(&self) -> Option<LoadedTrack> {
        self.loaded_track.read().unwrap().clone()
    }
}
