//! Mixer controls that decide which channels are on air.
//!
//! Channel N carries player N. A channel is on air when its fader is up and
//! the crossfader doesn't cut it, the same rule DJMs use to light their
//! on-air indicators and tell players via the channels-on-air packet.

use serde::{Deserialize, Serialize};

pub const MAX_CHANNELS: usize = 6;
/// EQ knobs run from this (a kill: the band is gone) up to +6 dB.
pub const EQ_KILL_DB: f32 = -26.0;
pub const EQ_MAX_DB: f32 = 6.0;
pub const TRIM_MIN_DB: f32 = -26.0;
pub const TRIM_MAX_DB: f32 = 9.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CrossfaderAssign {
    A,
    Thru,
    B,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MixerControls {
    /// Channel fader positions, 0.0 (closed) ..= 1.0 (fully up).
    pub channel_faders: [f32; MAX_CHANNELS],
    /// 0.0 = fully A side, 0.5 = centre, 1.0 = fully B side.
    pub crossfader: f32,
    pub assign: [CrossfaderAssign; MAX_CHANNELS],
    /// Channel trim, dB.
    pub trim_db: [f32; MAX_CHANNELS],
    /// Low, mid, high EQ per channel, dB; `EQ_KILL_DB` is a kill.
    pub eq_db: [[f32; 3]; MAX_CHANNELS],
    /// Master level, 0.0 ..= 1.0.
    pub master: f32,
}

fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

impl Default for MixerControls {
    /// Faders up, crossfader centred, every channel THRU: every player is
    /// audible, so every player is on air.
    fn default() -> Self {
        Self {
            channel_faders: [1.0; MAX_CHANNELS],
            crossfader: 0.5,
            assign: [CrossfaderAssign::Thru; MAX_CHANNELS],
            trim_db: [0.0; MAX_CHANNELS],
            eq_db: [[0.0; 3]; MAX_CHANNELS],
            master: 1.0,
        }
    }
}

impl MixerControls {
    /// Whether channel `channel` (1-based) is on air.
    pub fn on_air(&self, channel: u8) -> bool {
        let Some(i) = (channel as usize).checked_sub(1).filter(|&i| i < MAX_CHANNELS) else {
            return false;
        };
        let crossfader_open = match self.assign[i] {
            CrossfaderAssign::Thru => true,
            CrossfaderAssign::A => self.crossfader < 1.0,
            CrossfaderAssign::B => self.crossfader > 0.0,
        };
        self.channel_faders[i] > 0.0 && crossfader_open
    }

    /// Crossfader gain for channel `i` (0-based): full on its own side and
    /// at the centre, fading out towards the far end.
    fn crossfader_gain(&self, i: usize) -> f32 {
        match self.assign[i] {
            CrossfaderAssign::Thru => 1.0,
            CrossfaderAssign::A => (2.0 * (1.0 - self.crossfader)).min(1.0),
            CrossfaderAssign::B => (2.0 * self.crossfader).min(1.0),
        }
    }

    /// Linear gain for channel `i` (0-based): trim, fader (squared, so the
    /// top of the fader travel does the most) and crossfader.
    pub fn channel_gain(&self, i: usize) -> f32 {
        db_to_gain(self.trim_db[i]) * self.channel_faders[i].powi(2) * self.crossfader_gain(i)
    }

    /// Linear low/mid/high gains for channel `i` (0-based).
    pub fn eq_gains(&self, i: usize) -> [f32; 3] {
        self.eq_db[i].map(|db| if db <= EQ_KILL_DB { 0.0 } else { db_to_gain(db) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_mixer_has_every_channel_on_air() {
        let m = MixerControls::default();
        assert!((1..=6).all(|ch| m.on_air(ch)));
        assert!(!m.on_air(0) && !m.on_air(7));
    }

    #[test]
    fn closed_fader_takes_channel_off_air() {
        let mut m = MixerControls::default();
        m.channel_faders[1] = 0.0;
        assert!(m.on_air(1));
        assert!(!m.on_air(2));
    }

    #[test]
    fn gains_follow_faders_trim_and_eq() {
        let mut m = MixerControls::default();
        assert_eq!(m.channel_gain(0), 1.0);
        assert_eq!(m.eq_gains(0), [1.0; 3]);
        m.channel_faders[0] = 0.5;
        assert_eq!(m.channel_gain(0), 0.25);
        m.trim_db[0] = 6.0;
        assert!((m.channel_gain(0) - 0.25 * 1.995).abs() < 1e-3);
        m.eq_db[1] = [EQ_KILL_DB, -6.0, 6.0];
        let eq = m.eq_gains(1);
        assert_eq!(eq[0], 0.0, "kill");
        assert!((eq[1] - 0.501).abs() < 1e-3 && (eq[2] - 1.995).abs() < 1e-3);
        m.assign[2] = CrossfaderAssign::A;
        m.crossfader = 0.75;
        assert_eq!(m.channel_gain(2), 0.5);
    }

    #[test]
    fn crossfader_cuts_the_far_side_only_at_the_end_stop() {
        let mut m = MixerControls::default();
        m.assign[0] = CrossfaderAssign::A;
        m.assign[1] = CrossfaderAssign::B;
        m.crossfader = 0.0;
        assert!(m.on_air(1) && !m.on_air(2));
        m.crossfader = 0.9;
        assert!(m.on_air(1) && m.on_air(2));
        m.crossfader = 1.0;
        assert!(!m.on_air(1) && m.on_air(2));
        assert!(m.on_air(3), "THRU channels ignore the crossfader");
    }
}
