//! Beat grids: where each beat of a track falls and where it sits in the bar.
//!
//! Tracks from a rekordbox export use the analysed grid (every beat's time
//! and bar position, so tempo changes are followed exactly). Files without
//! analysis and empty decks get a constant grid from a BPM.

/// One beat: its time in the track, its position in the bar (1..=4) and
/// the tempo rekordbox stored for it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridBeat {
    pub time_ms: f64,
    pub bar_beat: u8,
    pub bpm_hundredths: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BeatGrid {
    /// Beats as analysed by rekordbox, sorted by time.
    Analysed(Vec<GridBeat>),
    /// Beat 0 at `first_ms` on beat 1 of a bar, then one every `period_ms`,
    /// forever.
    Constant { first_ms: f64, period_ms: f64 },
}

impl BeatGrid {
    /// From `(time_ms, bar_beat, bpm_hundredths)` as `Library::beats_for`
    /// returns. Falls back to a 120 BPM constant grid if there are none.
    pub fn from_analysis(beats: &[(u32, u8, u16)]) -> Self {
        if beats.is_empty() {
            return Self::constant(12000, 0.0);
        }
        Self::Analysed(
            beats
                .iter()
                .map(|&(t, bar, bpm)| GridBeat {
                    time_ms: f64::from(t),
                    bar_beat: bar.clamp(1, 4),
                    bpm_hundredths: bpm,
                })
                .collect(),
        )
    }

    pub fn constant(bpm_hundredths: u16, first_ms: f64) -> Self {
        Self::Constant {
            first_ms,
            period_ms: 6_000_000.0 / f64::from(bpm_hundredths.max(1)),
        }
    }

    /// Time of beat `i` (0-based). Analysed grids continue past their last
    /// beat at the last beat's tempo.
    pub fn time_of(&self, i: i64) -> f64 {
        match self {
            Self::Constant { first_ms, period_ms } => first_ms + i as f64 * period_ms,
            Self::Analysed(beats) => {
                let last = beats.len() as i64 - 1;
                if i < 0 {
                    beats[0].time_ms + i as f64 * self.period_near(0)
                } else if i > last {
                    beats[last as usize].time_ms + (i - last) as f64 * self.period_near(last as usize)
                } else {
                    beats[i as usize].time_ms
                }
            }
        }
    }

    /// Bar position (1..=4) of beat `i`.
    pub fn bar_beat(&self, i: i64) -> u8 {
        match self {
            Self::Constant { .. } => (i.rem_euclid(4) + 1) as u8,
            Self::Analysed(beats) => {
                let last = beats.len() as i64 - 1;
                let (anchor, bar) = if i < 0 {
                    (0, beats[0].bar_beat)
                } else if i > last {
                    (last, beats[last as usize].bar_beat)
                } else {
                    return beats[i as usize].bar_beat;
                };
                ((i64::from(bar) - 1 + (i - anchor)).rem_euclid(4) + 1) as u8
            }
        }
    }

    /// Index of the last beat at or before `ms`; -1 before the first beat.
    pub fn index_at(&self, ms: f64) -> i64 {
        match self {
            Self::Constant { first_ms, period_ms } => ((ms - first_ms) / period_ms).floor() as i64,
            Self::Analysed(beats) => {
                let n = beats.partition_point(|b| b.time_ms <= ms) as i64;
                if n == 0 {
                    // Before the first beat: extrapolate backwards.
                    let p = self.period_near(0);
                    ((ms - beats[0].time_ms) / p).floor() as i64
                } else if n == beats.len() as i64 {
                    let last = n - 1;
                    let p = self.period_near(last as usize);
                    last + ((ms - beats[last as usize].time_ms) / p).floor() as i64
                } else {
                    n - 1
                }
            }
        }
    }

    /// The beat closest to `ms`.
    pub fn nearest(&self, ms: f64) -> i64 {
        let i = self.index_at(ms);
        if (ms - self.time_of(i)) <= (self.time_of(i + 1) - ms) {
            i
        } else {
            i + 1
        }
    }

    /// Length of beat `i` (until the next one), in ms.
    pub fn beat_len(&self, i: i64) -> f64 {
        self.time_of(i + 1) - self.time_of(i)
    }

    /// Tempo at `ms`, in BPM.
    /// Analysed grids give the tempo rekordbox stored for the beat (what
    /// players report); beat spacing is rounded to whole ms, so deriving the
    /// tempo from it is slightly off.
    pub fn bpm_at(&self, ms: f64) -> f64 {
        let i = self.index_at(ms);
        if let Self::Analysed(beats) = self {
            let stored = beats[i.clamp(0, beats.len() as i64 - 1) as usize].bpm_hundredths;
            if stored > 0 {
                return f64::from(stored) / 100.0;
            }
        }
        60_000.0 / self.beat_len(i)
    }

    /// Position `beats` beats away from `ms` (fractional and negative
    /// allowed), following the grid's tempo.
    pub fn offset_by_beats(&self, ms: f64, beats: f64) -> f64 {
        let i = self.index_at(ms);
        let phase = (ms - self.time_of(i)) / self.beat_len(i);
        let target = i as f64 + phase + beats;
        let j = target.floor() as i64;
        self.time_of(j) + (target - j as f64) * self.beat_len(j)
    }

    /// Beat period near analysed beat `i`.
    fn period_near(&self, i: usize) -> f64 {
        match self {
            Self::Constant { period_ms, .. } => *period_ms,
            Self::Analysed(beats) if beats.len() >= 2 => {
                let j = i.min(beats.len() - 2);
                (beats[j + 1].time_ms - beats[j].time_ms).max(1.0)
            }
            Self::Analysed(_) => 500.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analysed() -> BeatGrid {
        // 120 BPM from 100 ms, starting on beat 3 of the bar.
        BeatGrid::from_analysis(&[(100, 3, 12000), (600, 4, 12000), (1100, 1, 12000), (1600, 2, 12000)])
    }

    #[test]
    fn constant_grid_counts_beats_and_bars() {
        let g = BeatGrid::constant(12000, 250.0);
        assert_eq!(g.index_at(249.0), -1);
        assert_eq!(g.index_at(250.0), 0);
        assert_eq!(g.index_at(1249.0), 1);
        assert_eq!(g.time_of(4), 2250.0);
        assert_eq!((g.bar_beat(0), g.bar_beat(3), g.bar_beat(4), g.bar_beat(-1)), (1, 4, 1, 4));
        assert!((g.bpm_at(10_000.0) - 120.0).abs() < 1e-9);
    }

    #[test]
    fn analysed_grid_uses_real_times_and_bar_positions() {
        let g = analysed();
        assert_eq!(g.index_at(99.0), -1);
        assert_eq!(g.index_at(600.0), 1);
        assert_eq!(g.bar_beat(2), 1);
        // Past the end: continues at the last tempo, bar positions roll on.
        assert_eq!(g.time_of(5), 2600.0);
        assert_eq!(g.bar_beat(5), 4);
        assert_eq!(g.index_at(2700.0), 5);
        // Before the start too.
        assert_eq!(g.time_of(-1), -400.0);
        assert_eq!(g.bar_beat(-1), 2);
    }

    #[test]
    fn analysed_tempo_is_the_stored_one() {
        // 468 ms spacing is 128.205 BPM; rekordbox stored 128.00.
        let g = BeatGrid::from_analysis(&[(0, 1, 12800), (468, 2, 12800), (937, 3, 12800)]);
        assert_eq!(g.bpm_at(500.0), 128.0);
        assert_eq!(g.bpm_at(-100.0), 128.0, "before the first beat");
        assert_eq!(g.bpm_at(5000.0), 128.0, "after the last beat");
    }

    #[test]
    fn nearest_and_offsets_follow_the_grid() {
        let g = analysed();
        assert_eq!(g.nearest(340.0), 0);
        assert_eq!(g.nearest(360.0), 1);
        assert_eq!(g.offset_by_beats(100.0, 4.0), 2100.0);
        assert_eq!(g.offset_by_beats(350.0, 1.0), 850.0, "keeps the phase");
        assert_eq!(g.offset_by_beats(1100.0, -2.0), 100.0);
    }
}
