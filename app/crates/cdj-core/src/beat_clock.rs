//! Beat clock - emits beat packets at the BPM carried in [`PlayerState`] and
//! advances the bar counter.
//!
//! ## Two modes
//!
//! 1. **Phase-locked to audio playhead** (when a track is loaded and
//!    `PlayerState::sample_rate() > 0`). The clock computes the musical time
//!    from `playhead_frames / sample_rate`, finds the next beat on the grid
//!    defined by `bpm` + `beat_grid_offset_ms`, and sleeps until that
//!    moment. Beats stay aligned with the audio even if the system clock
//!    drifts or the audio device reports glitches.
//!
//! 2. **Wall-clock fallback** (no track loaded). Absolute-deadline timer at
//!    `60/bpm` seconds; beat-within-bar is advanced by an internal counter.
//!    Kept so `--autoplay` (timecode-only mode) still works.
//!
//! The mode is evaluated on every iteration so loading / unloading a track
//! switches cleanly at the next beat.

use std::sync::Arc;
use std::time::{Duration, Instant};

use cdj_proto::{Beat, DeviceName};
use tokio::net::UdpSocket;
use tokio::time;
use tracing::{debug, trace};

use crate::net::{broadcast_addr, Interface, PORT_BEAT};
use crate::player_state::PlayerState;

pub struct BeatClock {
    pub device_name: DeviceName,
    pub device_number: u8,
    pub state: Arc<PlayerState>,
    pub iface: Interface,
    pub socket: Arc<UdpSocket>,
}

impl BeatClock {
    pub async fn run(self) -> anyhow::Result<()> {
        let dest = broadcast_addr(&self.iface, PORT_BEAT);
        let mut wallclock_next = Instant::now();
        // In phase-locked mode, track the last ordinal we fired. The playhead
        // is updated by the CPAL callback at buffer granularity (~10 ms on
        // macOS), so after we fire ordinal N there can be several iterations
        // where the playhead still reads the same value and the math would
        // re-select N. We bump `last_fired_ord` here and force-forward the
        // target ordinal to N+1 until the playhead catches up.
        let mut last_fired_ord: Option<u64> = None;

        loop {
            if !self.state.playing() {
                time::sleep(Duration::from_millis(50)).await;
                wallclock_next = Instant::now();
                last_fired_ord = None;
                continue;
            }

            let bpm = self.state.bpm_hundredths();
            let sr = self.state.sample_rate();

            if sr > 0 {
                let playhead = self.state.playhead_frames();
                let offset_ms = self.state.beat_grid_offset_ms();
                let mut next = next_beat_from_playhead(playhead, sr, bpm, offset_ms);
                if let Some(last) = last_fired_ord {
                    if next.ordinal <= last {
                        // Force to the next beat beyond what we already fired.
                        next = beat_at_ordinal(last + 1, sr, playhead, bpm, offset_ms);
                    }
                }
                self.state.set_beat_within_bar(next.beat_within_bar);
                time::sleep(Duration::from_micros((next.wait_ms * 1000.0) as u64)).await;

                let pkt = Beat {
                    device_name: self.device_name.clone(),
                    device_number: self.device_number,
                    bpm_hundredths: bpm,
                    beat_within_bar: next.beat_within_bar,
                }
                .encode();
                if let Err(e) = self.socket.send_to(&pkt, dest).await {
                    tracing::warn!("beat send failed: {e}");
                } else {
                    trace!(
                        num = self.device_number,
                        beat = next.beat_within_bar,
                        ord = next.ordinal,
                        mode = "phase-locked",
                        "beat sent"
                    );
                }
                last_fired_ord = Some(next.ordinal);
                if next.beat_within_bar == 1 {
                    debug!(num = self.device_number, "bar rollover");
                }
            } else {
                // Wall-clock fallback (no audio loaded).
                time::sleep_until(wallclock_next.into()).await;
                let beat_within_bar = self.state.beat_within_bar();
                wallclock_next += beat_interval(bpm);
                if wallclock_next < Instant::now() {
                    wallclock_next = Instant::now() + beat_interval(bpm);
                }
                let pkt = Beat {
                    device_name: self.device_name.clone(),
                    device_number: self.device_number,
                    bpm_hundredths: bpm,
                    beat_within_bar,
                }
                .encode();
                if let Err(e) = self.socket.send_to(&pkt, dest).await {
                    tracing::warn!("beat send failed: {e}");
                } else {
                    trace!(
                        num = self.device_number,
                        beat = beat_within_bar,
                        mode = "wall-clock",
                        "beat sent"
                    );
                }
                let advanced = self.state.advance_beat();
                if advanced == 1 {
                    debug!(num = self.device_number, "bar rollover");
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct NextBeat {
    ordinal: u64,
    beat_within_bar: u8,
    wait_ms: f64,
}

fn beat_at_ordinal(
    ordinal: u64,
    sample_rate: u32,
    playhead_frames: u64,
    bpm_hundredths: u16,
    beat_grid_offset_ms: u32,
) -> NextBeat {
    let beat_period_ms = 6_000_000.0 / bpm_hundredths.max(1) as f64;
    let beat_ms = beat_grid_offset_ms as f64 + ordinal as f64 * beat_period_ms;
    let played_ms = playhead_frames as f64 * 1000.0 / sample_rate as f64;
    NextBeat {
        ordinal,
        beat_within_bar: ((ordinal % 4) + 1) as u8,
        wait_ms: (beat_ms - played_ms).max(0.0),
    }
}

/// Compute how long to wait until the next beat, and which beat-within-bar
/// that will be. Returns `(wait_ms, beat_within_bar)`.
///
/// Beats are indexed from zero at the grid origin (`beat_grid_offset_ms`).
/// Beat ordinal N is at `offset_ms + N * (60_000 * 100) / bpm_hundredths` ms.
/// The function returns the smallest N such that the beat time is strictly
/// in the future relative to the current playhead.
fn next_beat_from_playhead(
    playhead_frames: u64,
    sample_rate: u32,
    bpm_hundredths: u16,
    beat_grid_offset_ms: u32,
) -> NextBeat {
    let played_ms = playhead_frames as f64 * 1000.0 / sample_rate as f64;
    let grid_ms = played_ms - beat_grid_offset_ms as f64;
    let beat_period_ms = 6_000_000.0 / bpm_hundredths.max(1) as f64;
    let beats_so_far = if grid_ms < 0.0 {
        -1.0
    } else {
        grid_ms / beat_period_ms
    };
    let next_ord = (beats_so_far.floor() as i64 + 1).max(0) as u64;
    let next_ms = beat_grid_offset_ms as f64 + next_ord as f64 * beat_period_ms;
    NextBeat {
        ordinal: next_ord,
        beat_within_bar: ((next_ord % 4) + 1) as u8,
        wait_ms: (next_ms - played_ms).max(0.0),
    }
}

fn beat_interval(bpm_hundredths: u16) -> Duration {
    if bpm_hundredths == 0 {
        return Duration::from_millis(500);
    }
    let micros = 6_000_000_000u64 / bpm_hundredths as u64;
    Duration::from_micros(micros)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_at_120_bpm_is_500ms() {
        assert_eq!(beat_interval(12000), Duration::from_millis(500));
    }

    #[test]
    fn interval_at_128_bpm_is_468_750us() {
        assert_eq!(beat_interval(12800), Duration::from_micros(468_750));
    }

    #[test]
    fn interval_at_zero_bpm_is_safe() {
        assert_eq!(beat_interval(0), Duration::from_millis(500));
    }

    #[test]
    fn at_playhead_zero_with_zero_offset_next_beat_is_ordinal_1() {
        let nb = next_beat_from_playhead(0, 48_000, 12000, 0);
        assert!((nb.wait_ms - 500.0).abs() < 1e-6);
        assert_eq!(nb.ordinal, 1);
        assert_eq!(nb.beat_within_bar, 2);
    }

    #[test]
    fn with_positive_offset_first_beat_is_ordinal_0() {
        let nb = next_beat_from_playhead(0, 48_000, 12000, 250);
        assert!((nb.wait_ms - 250.0).abs() < 1e-6);
        assert_eq!(nb.ordinal, 0);
        assert_eq!(nb.beat_within_bar, 1);
    }

    #[test]
    fn playhead_past_offset_lands_on_next_beat() {
        // 120 BPM, 600 ms played (1.2 beats). Next beat is ordinal 2.
        let frames = (48_000f64 * 0.6) as u64;
        let nb = next_beat_from_playhead(frames, 48_000, 12000, 0);
        assert!((nb.wait_ms - 400.0).abs() < 1e-3);
        assert_eq!(nb.ordinal, 2);
        assert_eq!(nb.beat_within_bar, 3);
    }

    #[test]
    fn bar_position_wraps_1_to_4_across_bars() {
        for n in 0u32..8 {
            let frames = (48_000f64 * (n as f64 * 500.0) / 1000.0) as u64;
            let nb = next_beat_from_playhead(frames, 48_000, 12000, 0);
            let expected = (((n + 1) % 4) + 1) as u8;
            assert_eq!(nb.beat_within_bar, expected, "playhead ord {n}");
        }
    }

    #[test]
    fn beat_at_ordinal_gives_expected_wait() {
        // At playhead 600 ms, ordinal 2 (beat at 1000 ms): wait = 400 ms.
        let frames = (48_000f64 * 0.6) as u64;
        let nb = beat_at_ordinal(2, 48_000, frames, 12000, 0);
        assert!((nb.wait_ms - 400.0).abs() < 1e-3);
        assert_eq!(nb.beat_within_bar, 3);
    }
}
