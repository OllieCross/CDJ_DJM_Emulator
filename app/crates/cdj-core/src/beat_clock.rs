//! Beat clock — emits beat packets at the BPM carried in [`PlayerState`] and
//! advances the bar counter.
//!
//! The clock uses absolute wall-clock deadlines rather than "sleep for N ms"
//! so small sleep overruns don't accumulate. This keeps downstream timecode
//! drift bounded regardless of how many beats we emit.

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
        let mut next_deadline = Instant::now();

        loop {
            // Wait until the clock is playing. Re-sample BPM on every iteration
            // so live tempo changes take effect on the following beat.
            if !self.state.playing() {
                time::sleep(Duration::from_millis(50)).await;
                // On resume, realign the deadline to "now" so we don't fire a
                // backlog of beats.
                next_deadline = Instant::now();
                continue;
            }

            let bpm = self.state.bpm_hundredths();
            let interval = beat_interval(bpm);

            time::sleep_until(next_deadline.into()).await;

            let beat_within_bar = self.state.beat_within_bar();
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
                    "beat sent"
                );
            }

            let advanced = self.state.advance_beat();
            if advanced == 1 {
                debug!(num = self.device_number, "bar rollover");
            }

            next_deadline += interval;
            // If we've fallen behind by more than one beat (heavy load),
            // resync rather than burning through stale deadlines.
            if next_deadline < Instant::now() {
                next_deadline = Instant::now() + interval;
            }
        }
    }
}

fn beat_interval(bpm_hundredths: u16) -> Duration {
    if bpm_hundredths == 0 {
        return Duration::from_millis(500);
    }
    // 60_000_000 us / (bpm_hundredths / 100) = 6_000_000_000 us / bpm_hundredths
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
}
