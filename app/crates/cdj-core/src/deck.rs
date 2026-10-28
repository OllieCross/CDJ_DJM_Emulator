//! A player's transport: what a CDJ does when you press its buttons.
//!
//! The audio thread owns the playhead ([`crate::engine`]); a `Deck` turns
//! button presses into voice commands (play, cue, hot cues, loops, pitch)
//! and keeps the state clients see: play state, cue points, the active
//! loop, tempo-master, sync and on-air flags.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use cdj_proto::PlayState1;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

use crate::engine::{AudioEngine, BeatEvent, VoiceCommand, VoiceFeedback};
use crate::grid::BeatGrid;
use crate::library::{Cue, Library};
use crate::media::Slot;
use crate::track_audio::TrackAudio;

pub const HOT_CUES: usize = 8;
/// Positions closer than this count as "at" a cue.
const AT_CUE_MS: f64 = 5.0;
/// Beats ahead the memory-cue countdown reaches (64 bars).
const CUE_COUNTDOWN_BEATS: i64 = 256;

/// The track on a player: which export it came from and where that export
/// is inserted (the player clients query for metadata).
#[derive(Debug, Clone)]
pub struct LoadedTrack {
    pub library: Arc<Library>,
    pub track_id: u32,
    pub source_player: u8,
    pub source_slot: Slot,
}

/// Pitch fader range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TempoRange {
    Six,
    Ten,
    Sixteen,
    Wide,
}

impl TempoRange {
    pub fn fraction(self) -> f64 {
        match self {
            Self::Six => 0.06,
            Self::Ten => 0.10,
            Self::Sixteen => 0.16,
            Self::Wide => 1.0,
        }
    }
}

/// A hot cue: a point, or a loop when `loop_end_ms` is set.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct HotCue {
    pub start_ms: f64,
    pub loop_end_ms: Option<f64>,
    /// RGB colour from rekordbox, if any.
    pub color: Option<[u8; 3]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct LoopRegion {
    pub start_ms: f64,
    pub end_ms: f64,
    /// Length in beats for beat loops.
    pub beats: Option<f64>,
    pub active: bool,
}

/// Everything about a deck at one moment.
#[derive(Debug, Clone)]
pub struct DeckView {
    pub loaded: Option<LoadedTrack>,
    pub play_state: PlayState1,
    /// The playhead is moving.
    pub moving: bool,
    pub position_ms: f64,
    pub length_ms: Option<f64>,
    /// Tempo of the track at the playhead, BPM x 100.
    pub track_bpm_hundredths: u16,
    /// Pitch as a fraction (+0.05 = +5%).
    pub pitch: f64,
    pub pitch_fader: f64,
    pub tempo_range: TempoRange,
    pub master_tempo: bool,
    pub reverse: bool,
    pub quantize: bool,
    /// Grid index (0-based) of the beat at the playhead, -1 before the first.
    pub beat_index: i64,
    pub bar_beat: u8,
    pub cue_ms: f64,
    pub hot_cues: [Option<HotCue>; HOT_CUES],
    pub memory_cues: Vec<f64>,
    /// Beats to the next memory cue, if one is within 64 bars.
    pub cue_countdown: Option<u16>,
    pub active_loop: Option<LoopRegion>,
}

impl DeckView {
    /// On-screen BPM: track tempo with pitch applied.
    pub fn effective_bpm(&self) -> f64 {
        f64::from(self.track_bpm_hundredths) / 100.0 * (1.0 + self.pitch)
    }

    /// Beat number clients expect (1-based; 1 before the first beat).
    pub fn beat_number(&self) -> u32 {
        (self.beat_index + 1).max(1) as u32
    }
}

struct DeckInner {
    loaded: Option<LoadedTrack>,
    grid: Arc<BeatGrid>,
    /// Tempo for an empty deck (timecode without a track).
    idle_bpm_hundredths: u16,
    length_ms: Option<f64>,
    cue_ms: f64,
    hot_cues: [Option<HotCue>; HOT_CUES],
    memory_cues: Vec<f64>,
    loop_region: Option<LoopRegion>,
    /// Loop-in pressed, waiting for loop-out.
    loop_in_ms: Option<f64>,
    playing: bool,
    /// CUE held while paused at the cue point.
    previewing: bool,
    pitch_fader: f64,
    tempo_range: TempoRange,
    /// Set by sync; overrides the fader.
    pitch_override: Option<f64>,
    master_tempo: bool,
    reverse: bool,
    quantize: bool,
    /// Temporary speed change while the jog wheel is turned during play.
    bend: f64,
}

/// One player's transport and the flags it reports.
pub struct Deck {
    pub number: u8,
    /// Hot cues this player model has (3 on older players, else 8).
    pub hot_cue_slots: usize,
    engine: Arc<AudioEngine>,
    feedback: Arc<VoiceFeedback>,
    inner: Mutex<DeckInner>,
    master: AtomicBool,
    synced: AtomicBool,
    on_air: AtomicBool,
}

impl Deck {
    /// Player `number` (1-based), playing through voice `number - 1`, with
    /// `hot_cue_slots` hot cues (at most [`HOT_CUES`]).
    pub fn new(number: u8, engine: Arc<AudioEngine>, idle_bpm_hundredths: u16, hot_cue_slots: usize) -> Self {
        let voice = usize::from(number - 1);
        let deck = Self {
            number,
            hot_cue_slots: hot_cue_slots.min(HOT_CUES),
            feedback: engine.feedback(voice),
            engine,
            inner: Mutex::new(DeckInner {
                loaded: None,
                grid: Arc::new(BeatGrid::constant(idle_bpm_hundredths, 0.0)),
                idle_bpm_hundredths,
                length_ms: None,
                cue_ms: 0.0,
                hot_cues: [None; HOT_CUES],
                memory_cues: Vec::new(),
                loop_region: None,
                loop_in_ms: None,
                playing: false,
                previewing: false,
                pitch_fader: 0.0,
                tempo_range: TempoRange::Ten,
                pitch_override: None,
                master_tempo: false,
                reverse: false,
                quantize: true,
                bend: 0.0,
            }),
            master: AtomicBool::new(false),
            synced: AtomicBool::new(false),
            on_air: AtomicBool::new(false),
        };
        deck.unload();
        deck
    }

    fn send(&self, cmd: VoiceCommand) {
        self.engine.send(usize::from(self.number - 1), cmd);
    }

    /// Beats this deck plays from now on, stamped with when they're heard.
    pub fn beat_events(&self) -> UnboundedReceiver<BeatEvent> {
        let (tx, rx) = unbounded_channel();
        self.send(VoiceCommand::SetBeatSink(Some(tx)));
        rx
    }

    pub fn position_ms(&self) -> f64 {
        self.feedback.position_ms()
    }

    // ---------- flags ----------

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
        if !v {
            self.inner.lock().unwrap().pitch_override = None;
            self.apply_speed();
        }
    }
    pub fn on_air(&self) -> bool {
        self.on_air.load(Ordering::Relaxed)
    }
    pub fn set_on_air(&self, v: bool) {
        self.on_air.store(v, Ordering::Relaxed);
    }

    // ---------- loading ----------

    /// Put a track on the deck, cued to its first memory cue (or the
    /// start). `audio` is `None` when only metadata is available; the deck
    /// still plays (silently) so beats and timecode work.
    pub fn load(
        &self,
        track: LoadedTrack,
        audio: Option<Arc<TrackAudio>>,
        grid: BeatGrid,
        cues: &[Cue],
        length_ms: Option<f64>,
    ) {
        let mut hot_cues = [None; HOT_CUES];
        let mut memory_cues = Vec::new();
        for c in cues {
            let start = f64::from(c.time_ms);
            match c.hot_cue as usize {
                0 => memory_cues.push(start),
                // Hot cues past what this model has are ignored, as on the player.
                n @ 1..=HOT_CUES if n <= self.hot_cue_slots => {
                    hot_cues[n - 1] = Some(HotCue {
                        start_ms: start,
                        loop_end_ms: c.loop_end_ms.map(f64::from),
                        color: c.hot_cue_color.map(|(_, rgb)| rgb),
                    })
                }
                _ => {}
            }
        }
        memory_cues.sort_by(f64::total_cmp);
        let cue_ms = memory_cues.first().copied().unwrap_or(0.0);
        let grid = Arc::new(grid);

        let mut d = self.inner.lock().unwrap();
        d.loaded = Some(track);
        d.grid = grid.clone();
        d.length_ms = length_ms;
        d.cue_ms = cue_ms;
        d.hot_cues = hot_cues;
        d.memory_cues = memory_cues;
        d.loop_region = None;
        d.loop_in_ms = None;
        d.playing = false;
        d.previewing = false;
        drop(d);
        self.send(VoiceCommand::Load {
            audio,
            grid,
            length_ms,
            position_ms: cue_ms,
        });
        self.apply_speed();
    }

    /// Play audio that isn't on any media (no metadata, so clients see no
    /// track), on `grid`.
    pub fn load_unlisted(&self, audio: Arc<TrackAudio>, grid: BeatGrid) {
        self.unload();
        let grid = Arc::new(grid);
        self.inner.lock().unwrap().grid = grid.clone();
        self.send(VoiceCommand::Load {
            audio: Some(audio),
            grid,
            length_ms: None,
            position_ms: 0.0,
        });
    }

    /// Empty the deck. It keeps time at its idle tempo, silently.
    pub fn unload(&self) {
        let mut d = self.inner.lock().unwrap();
        d.loaded = None;
        d.grid = Arc::new(BeatGrid::constant(d.idle_bpm_hundredths, 0.0));
        d.length_ms = None;
        d.cue_ms = 0.0;
        d.hot_cues = [None; HOT_CUES];
        d.memory_cues.clear();
        d.loop_region = None;
        d.loop_in_ms = None;
        d.playing = false;
        d.previewing = false;
        let grid = d.grid.clone();
        drop(d);
        self.send(VoiceCommand::Load {
            audio: None,
            grid,
            length_ms: None,
            position_ms: 0.0,
        });
        self.apply_speed();
    }

    pub fn loaded(&self) -> Option<LoadedTrack> {
        self.inner.lock().unwrap().loaded.clone()
    }

    pub fn grid(&self) -> Arc<BeatGrid> {
        self.inner.lock().unwrap().grid.clone()
    }

    // ---------- transport ----------

    pub fn play(&self) {
        let mut d = self.inner.lock().unwrap();
        d.playing = true;
        d.previewing = false;
        drop(d);
        self.send(VoiceCommand::SetPlaying(true));
    }

    pub fn pause(&self) {
        let mut d = self.inner.lock().unwrap();
        d.playing = false;
        d.previewing = false;
        drop(d);
        self.send(VoiceCommand::SetPlaying(false));
    }

    /// The deck is playing (or meant to be: it may have just been told).
    pub fn is_playing(&self) -> bool {
        let d = self.inner.lock().unwrap();
        d.playing && !self.feedback.ended() || d.previewing
    }

    pub fn play_pause(&self) {
        if self.is_playing() {
            self.pause();
        } else {
            self.play();
        }
    }

    /// Snap to the nearest beat when quantize is on.
    fn quantized(d: &DeckInner, ms: f64) -> f64 {
        if d.quantize && d.loaded.is_some() {
            d.grid.time_of(d.grid.nearest(ms)).max(0.0)
        } else {
            ms
        }
    }

    /// CUE pressed. Playing: back to the cue point and pause. Paused at the
    /// cue point: play while held. Paused elsewhere: set the cue point here.
    pub fn cue_press(&self) {
        let pos = self.position_ms();
        let mut d = self.inner.lock().unwrap();
        // Also covers a track that played to its end.
        if d.playing {
            d.playing = false;
            let cue = d.cue_ms;
            drop(d);
            self.send(VoiceCommand::SetPlaying(false));
            self.send(VoiceCommand::Seek(cue));
        } else if (pos - d.cue_ms).abs() < AT_CUE_MS {
            d.previewing = true;
            drop(d);
            self.send(VoiceCommand::SetPlaying(true));
        } else {
            d.cue_ms = Self::quantized(&d, pos);
            let cue = d.cue_ms;
            drop(d);
            self.send(VoiceCommand::Seek(cue));
        }
    }

    /// CUE released: a preview returns to the cue point.
    pub fn cue_release(&self) {
        let mut d = self.inner.lock().unwrap();
        if d.previewing {
            d.previewing = false;
            let cue = d.cue_ms;
            drop(d);
            self.send(VoiceCommand::SetPlaying(false));
            self.send(VoiceCommand::Seek(cue));
        }
    }

    /// Needle search: jump to `ms`, keeping play/pause.
    pub fn seek(&self, ms: f64) {
        let d = self.inner.lock().unwrap();
        let target = match d.length_ms {
            Some(len) => ms.clamp(0.0, len),
            None => ms.max(0.0),
        };
        let playing = d.playing;
        drop(d);
        self.send(VoiceCommand::Seek(target));
        if playing {
            // Restart after the end of the track.
            self.send(VoiceCommand::SetPlaying(true));
        }
    }

    /// Hot cue `n` (0-based) pressed: jump to it and play (a loop hot cue
    /// starts its loop); an empty one stores the current position.
    pub fn hot_cue(&self, n: usize) {
        if n >= self.hot_cue_slots {
            return;
        }
        let pos = self.position_ms();
        let mut d = self.inner.lock().unwrap();
        match d.hot_cues[n] {
            Some(cue) => {
                d.playing = true;
                d.previewing = false;
                d.loop_region = cue.loop_end_ms.map(|end| LoopRegion {
                    start_ms: cue.start_ms,
                    end_ms: end,
                    beats: None,
                    active: true,
                });
                let loop_ms = cue.loop_end_ms.map(|end| (cue.start_ms, end));
                drop(d);
                self.send(VoiceCommand::SetLoop(loop_ms));
                self.send(VoiceCommand::Seek(cue.start_ms));
                self.send(VoiceCommand::SetPlaying(true));
            }
            None => {
                let start = Self::quantized(&d, pos);
                d.hot_cues[n] = Some(HotCue {
                    start_ms: start,
                    loop_end_ms: None,
                    color: None,
                });
            }
        }
    }

    pub fn clear_hot_cue(&self, n: usize) {
        if let Some(slot) = self.inner.lock().unwrap().hot_cues.get_mut(n) {
            *slot = None;
        }
    }

    /// Jump to the next (or previous) memory cue and make it the cue point.
    pub fn memory_cue(&self, forward: bool) {
        let pos = self.position_ms();
        let mut d = self.inner.lock().unwrap();
        let target = if forward {
            d.memory_cues.iter().copied().find(|&c| c > pos + AT_CUE_MS)
        } else {
            d.memory_cues.iter().rev().copied().find(|&c| c < pos - AT_CUE_MS)
        };
        if let Some(t) = target {
            d.cue_ms = t;
            drop(d);
            self.send(VoiceCommand::Seek(t));
        }
    }

    // ---------- loops ----------

    fn set_loop(&self, d: &mut DeckInner, region: Option<LoopRegion>) {
        d.loop_region = region;
        let active = region.filter(|r| r.active).map(|r| (r.start_ms, r.end_ms));
        self.send(VoiceCommand::SetLoop(active));
    }

    /// Loop `beats` beats from the current beat (auto beat loop).
    pub fn beat_loop(&self, beats: f64) {
        let pos = self.position_ms();
        let mut d = self.inner.lock().unwrap();
        let start = if d.quantize {
            d.grid.time_of(d.grid.index_at(pos)).max(0.0)
        } else {
            pos
        };
        let end = d.grid.offset_by_beats(start, beats);
        self.set_loop(
            &mut d,
            Some(LoopRegion {
                start_ms: start,
                end_ms: end,
                beats: Some(beats),
                active: true,
            }),
        );
    }

    pub fn loop_in(&self) {
        let pos = self.position_ms();
        let mut d = self.inner.lock().unwrap();
        d.loop_in_ms = Some(Self::quantized(&d, pos));
    }

    /// End the loop started with [`loop_in`](Self::loop_in) here and start
    /// looping.
    pub fn loop_out(&self) {
        let pos = self.position_ms();
        let mut d = self.inner.lock().unwrap();
        let Some(start) = d.loop_in_ms.take() else {
            return;
        };
        let end = Self::quantized(&d, pos);
        if end > start {
            self.set_loop(
                &mut d,
                Some(LoopRegion {
                    start_ms: start,
                    end_ms: end,
                    beats: None,
                    active: true,
                }),
            );
        }
    }

    /// Stop looping; playback carries on past the loop end.
    pub fn exit_loop(&self) {
        let mut d = self.inner.lock().unwrap();
        let region = d.loop_region.map(|r| LoopRegion { active: false, ..r });
        self.set_loop(&mut d, region);
    }

    /// Turn the last loop back on and jump to its start.
    pub fn reloop(&self) {
        let mut d = self.inner.lock().unwrap();
        if let Some(r) = d.loop_region {
            self.set_loop(&mut d, Some(LoopRegion { active: true, ..r }));
            drop(d);
            self.send(VoiceCommand::Seek(r.start_ms));
        }
    }

    /// Halve (`factor` 0.5) or double (2.0) the active loop.
    pub fn resize_loop(&self, factor: f64) {
        let mut d = self.inner.lock().unwrap();
        if let Some(r) = d.loop_region {
            let (end, beats) = match r.beats {
                Some(b) => (d.grid.offset_by_beats(r.start_ms, b * factor), Some(b * factor)),
                None => (r.start_ms + (r.end_ms - r.start_ms) * factor, None),
            };
            if end > r.start_ms + 1.0 {
                self.set_loop(
                    &mut d,
                    Some(LoopRegion {
                        end_ms: end,
                        beats,
                        ..r
                    }),
                );
            }
        }
    }

    // ---------- tempo ----------

    /// Current pitch as a fraction: sync's override, else the fader.
    fn pitch(d: &DeckInner) -> f64 {
        d.pitch_override
            .unwrap_or(d.pitch_fader * d.tempo_range.fraction())
    }

    fn apply_speed(&self) {
        let d = self.inner.lock().unwrap();
        let speed = (1.0 + Self::pitch(&d)) * (1.0 + d.bend) * if d.reverse { -1.0 } else { 1.0 };
        drop(d);
        self.send(VoiceCommand::SetSpeed(speed));
    }

    /// Tempo fader, -1.0 (fastest down) ..= 1.0.
    pub fn set_pitch_fader(&self, position: f64) {
        self.inner.lock().unwrap().pitch_fader = position.clamp(-1.0, 1.0);
        self.apply_speed();
    }

    pub fn set_tempo_range(&self, range: TempoRange) {
        self.inner.lock().unwrap().tempo_range = range;
        self.apply_speed();
    }

    /// Master tempo (key lock): tempo changes keep the track's pitch.
    pub fn set_master_tempo(&self, on: bool) {
        self.inner.lock().unwrap().master_tempo = on;
        self.send(VoiceCommand::SetMasterTempo(on));
    }

    /// Jog wheel turned while playing: nudge the speed by `bend` (a
    /// fraction, e.g. +0.04) until it is released (`0.0`).
    pub fn set_bend(&self, bend: f64) {
        self.inner.lock().unwrap().bend = bend.clamp(-0.5, 0.5);
        self.apply_speed();
    }

    pub fn set_reverse(&self, on: bool) {
        self.inner.lock().unwrap().reverse = on;
        self.apply_speed();
    }

    pub fn set_quantize(&self, on: bool) {
        self.inner.lock().unwrap().quantize = on;
    }

    /// Play at `bpm` (sync), whatever the fader says.
    pub fn follow_bpm(&self, bpm: f64) {
        let view = self.view();
        let track_bpm = f64::from(view.track_bpm_hundredths) / 100.0;
        if track_bpm > 0.0 {
            self.inner.lock().unwrap().pitch_override = Some(bpm / track_bpm - 1.0);
            self.apply_speed();
        }
    }

    /// Nudge the playhead so its position within the beat matches
    /// `phase` (0.0 ..< 1.0), by the smallest move.
    pub fn align_phase(&self, phase: f64) {
        let pos = self.position_ms();
        let grid = self.grid();
        let i = grid.index_at(pos);
        let len = grid.beat_len(i);
        let current = (pos - grid.time_of(i)) / len;
        let mut delta = phase - current;
        if delta > 0.5 {
            delta -= 1.0;
        } else if delta < -0.5 {
            delta += 1.0;
        }
        self.send(VoiceCommand::Seek((pos + delta * len).max(0.0)));
    }

    /// Where the playhead sits within its beat, 0.0 ..< 1.0.
    pub fn beat_phase(&self) -> f64 {
        let pos = self.position_ms();
        let grid = self.grid();
        let i = grid.index_at(pos);
        (pos - grid.time_of(i)) / grid.beat_len(i)
    }

    // ---------- reporting ----------

    pub fn view(&self) -> DeckView {
        let pos = self.position_ms();
        let moving = self.feedback.playing();
        let ended = self.feedback.ended();
        let d = self.inner.lock().unwrap();
        let grid = &d.grid;
        let beat_index = grid.index_at(pos);
        let bpm_hundredths = match d.loaded {
            Some(_) => (grid.bpm_at(pos) * 100.0).round() as u16,
            None => d.idle_bpm_hundredths,
        };
        let looping = d.loop_region.is_some_and(|r| r.active);
        let play_state = match (&d.loaded, moving) {
            (_, true) if d.previewing => PlayState1::CuePlaying,
            (_, true) if looping => PlayState1::Looping,
            (_, true) => PlayState1::Playing,
            (None, false) => PlayState1::NoTrack,
            (Some(_), false) if ended => PlayState1::Ended,
            (Some(_), false) if (pos - d.cue_ms).abs() < AT_CUE_MS => PlayState1::Cued,
            (Some(_), false) => PlayState1::Paused,
        };
        let cue_countdown = d
            .memory_cues
            .iter()
            .find(|&&c| c > pos + AT_CUE_MS)
            .map(|&c| grid.index_at(c) - beat_index)
            .filter(|&beats| (0..=CUE_COUNTDOWN_BEATS).contains(&beats))
            .map(|beats| beats as u16);
        DeckView {
            loaded: d.loaded.clone(),
            play_state,
            moving,
            position_ms: pos,
            length_ms: d.length_ms,
            track_bpm_hundredths: bpm_hundredths,
            pitch: Self::pitch(&d),
            pitch_fader: d.pitch_fader,
            tempo_range: d.tempo_range,
            master_tempo: d.master_tempo,
            reverse: d.reverse,
            quantize: d.quantize,
            beat_index,
            bar_beat: grid.bar_beat(beat_index),
            cue_ms: d.cue_ms,
            hot_cues: d.hot_cues,
            memory_cues: d.memory_cues.clone(),
            cue_countdown,
            active_loop: d.loop_region.filter(|r| r.active),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::library::TrackInfo;
    use std::time::Duration;

    pub(crate) fn test_track() -> LoadedTrack {
        let info = TrackInfo {
            id: 7,
            title: "Test".into(),
            bpm_hundredths: 12000,
            duration_s: 60,
            ..Default::default()
        };
        LoadedTrack {
            library: Arc::new(Library::from_tracks(vec![info])),
            track_id: 7,
            source_player: 1,
            source_slot: Slot::Usb,
        }
    }

    /// A deck on a fresh engine that renders in real time, loaded with a
    /// silent 60 s track on a 120 BPM grid starting at 0.
    fn deck_with_track(cues: &[Cue]) -> (Arc<AudioEngine>, Deck) {
        let engine = Arc::new(AudioEngine::new());
        engine.start_silent().unwrap();
        let deck = Deck::new(1, engine.clone(), 12000, HOT_CUES);
        deck.load(test_track(), None, BeatGrid::constant(12000, 0.0), cues, Some(60_000.0));
        settle();
        (engine, deck)
    }

    fn settle() {
        std::thread::sleep(Duration::from_millis(40));
    }

    fn cue(hot: u8, ms: u32, loop_end: Option<u32>) -> Cue {
        Cue {
            hot_cue: hot,
            time_ms: ms,
            loop_end_ms: loop_end,
            color_id: 0,
            hot_cue_color: None,
            comment: String::new(),
        }
    }

    #[test]
    fn empty_deck_reports_no_track_and_idle_tempo() {
        let engine = Arc::new(AudioEngine::new());
        let deck = Deck::new(2, engine, 12800, HOT_CUES);
        let v = deck.view();
        assert_eq!(v.play_state, PlayState1::NoTrack);
        assert_eq!(v.track_bpm_hundredths, 12800);
        assert!(v.loaded.is_none());
    }

    #[test]
    fn loading_cues_to_first_memory_cue_and_reads_hot_cues() {
        let (_e, deck) = deck_with_track(&[cue(0, 8000, None), cue(0, 4000, None), cue(2, 16_000, Some(18_000))]);
        let v = deck.view();
        assert_eq!(v.play_state, PlayState1::Cued);
        assert_eq!(v.cue_ms, 4000.0);
        assert!((v.position_ms - 4000.0).abs() < 1.0);
        assert_eq!(v.memory_cues, [4000.0, 8000.0]);
        assert_eq!(v.hot_cues[1].unwrap().loop_end_ms, Some(18_000.0));
        assert_eq!(v.cue_countdown, Some(8), "next memory cue 8 beats away at 120 BPM");
    }

    #[test]
    fn cue_button_behaves_like_a_cdj() {
        let (_e, deck) = deck_with_track(&[]);
        deck.play();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(deck.view().play_state, PlayState1::Playing);
        assert!(deck.position_ms() > 100.0);

        // Playing: CUE goes back to the cue point and stops.
        deck.cue_press();
        settle();
        let v = deck.view();
        assert_eq!(v.play_state, PlayState1::Cued);
        assert!(v.position_ms.abs() < 1.0);

        // Paused away from the cue: CUE sets a new (quantized) cue point.
        deck.seek(1234.0);
        settle();
        deck.cue_press();
        settle();
        assert_eq!(deck.view().cue_ms, 1000.0, "snapped to the nearest beat");

        // Paused on the cue: CUE previews while held.
        deck.cue_press();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(deck.view().play_state, PlayState1::CuePlaying);
        deck.cue_release();
        settle();
        assert_eq!(deck.view().play_state, PlayState1::Cued);
    }

    #[test]
    fn older_models_only_have_three_hot_cues() {
        let engine = Arc::new(AudioEngine::new());
        let deck = Deck::new(1, engine, 12000, 3);
        deck.load(test_track(), None, BeatGrid::constant(12000, 0.0), &[cue(2, 1000, None), cue(5, 2000, None)], None);
        let v = deck.view();
        assert!(v.hot_cues[1].is_some(), "B is kept");
        assert!(v.hot_cues[4].is_none(), "E is beyond a 3-cue player");
        deck.hot_cue(4);
        assert!(deck.view().hot_cues[4].is_none(), "pad E doesn't exist");
    }

    #[test]
    fn hot_cues_jump_play_and_store() {
        let (_e, deck) = deck_with_track(&[cue(1, 10_000, None), cue(2, 20_000, Some(21_000))]);
        deck.hot_cue(0);
        settle();
        let v = deck.view();
        assert_eq!(v.play_state, PlayState1::Playing);
        assert!(v.position_ms >= 10_000.0 && v.position_ms < 10_200.0);

        deck.hot_cue(1);
        std::thread::sleep(Duration::from_millis(1300));
        let v = deck.view();
        assert_eq!(v.play_state, PlayState1::Looping);
        assert!(v.position_ms >= 20_000.0 && v.position_ms < 21_000.0, "inside the loop: {}", v.position_ms);

        deck.exit_loop();
        deck.hot_cue(5);
        assert!(deck.view().hot_cues[5].is_some(), "empty pad stores the position");
    }

    #[test]
    fn beat_loops_resize_and_exit() {
        let (_e, deck) = deck_with_track(&[]);
        deck.seek(2100.0);
        settle();
        deck.beat_loop(4.0);
        let r = deck.view().active_loop.unwrap();
        assert_eq!((r.start_ms, r.end_ms), (2000.0, 4000.0), "starts on the current beat");
        deck.resize_loop(0.5);
        assert_eq!(deck.view().active_loop.unwrap().end_ms, 3000.0);
        deck.resize_loop(2.0);
        assert_eq!(deck.view().active_loop.unwrap().end_ms, 4000.0);
        deck.exit_loop();
        assert!(deck.view().active_loop.is_none());
        deck.reloop();
        assert!(deck.view().active_loop.is_some());
    }

    #[test]
    fn pitch_follows_fader_range_and_sync() {
        let (_e, deck) = deck_with_track(&[]);
        deck.set_tempo_range(TempoRange::Six);
        deck.set_pitch_fader(0.5);
        let v = deck.view();
        assert!((v.pitch - 0.03).abs() < 1e-9);
        assert!((v.effective_bpm() - 123.6).abs() < 1e-9);
        deck.follow_bpm(126.0);
        assert!((deck.view().pitch - 0.05).abs() < 1e-9, "sync overrides the fader");
        deck.set_synced(false);
        assert!((deck.view().pitch - 0.03).abs() < 1e-9);
    }

    #[test]
    fn plays_to_the_end_and_stops() {
        let (_e, deck) = deck_with_track(&[]);
        deck.seek(59_900.0);
        deck.play();
        std::thread::sleep(Duration::from_millis(300));
        let v = deck.view();
        assert_eq!(v.play_state, PlayState1::Ended);
        assert_eq!(v.position_ms, 60_000.0);
        assert!(!deck.is_playing());
    }
}
