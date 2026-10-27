//! Beat packet (port 50001, kind 0x28).
//!
//! A beat packet is emitted by a player on every beat boundary. It carries:
//!
//! * BPM (hundredths), so receivers can interpolate between beats
//! * The time to the next beat, the next bar, and a few beats further out,
//!   so tempo-follow clients (ShowKontrol, DJM master sync) can schedule
//!   events ahead of time
//! * The `beat_within_bar` (1..=4) so 4-beat bar structure is observable
//! * The emitting device number
//!
//! ## Layout (96 bytes)
//!
//! Matches beat-link's `Beat` parser and its own `VirtualCdj.BEAT_PAYLOAD`
//! template, and the dysentery beat-packet figure.
//!
//! ```text
//! 0x00..0x24  UpdateHeader: kind 0x28, subtype 0x00, length-remaining 0x3c
//! 0x24..0x28  ms to next beat          (u32 BE)
//! 0x28..0x2c  ms to 2nd beat           (u32 BE)
//! 0x2c..0x30  ms to next bar           (u32 BE, 1..=4 beats away)
//! 0x30..0x34  ms to 4th beat           (u32 BE)
//! 0x34..0x38  ms to 2nd bar            (u32 BE, 5..=8 beats away)
//! 0x38..0x3c  ms to 8th beat           (u32 BE)
//! 0x3c..0x54  0xff fill
//! 0x54..0x58  pitch                    (u32 BE, 0x00100000 = +0%)
//! 0x58..0x5a  0x0000
//! 0x5a..0x5c  bpm_hundredths           (u16 BE)
//! 0x5c        beat_within_bar          (1..=4)
//! 0x5d..0x5f  0x0000
//! 0x5f        device_number            (copy of 0x21)
//! ```
//!
//! Timing values assume +0% pitch, as on real players.

use crate::error::DecodeError;
use crate::header::{update_kind, DeviceName, UpdateHeader};

pub const BEAT_LEN: usize = 0x60;
pub const BEAT_SUBTYPE: u8 = 0x00;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Beat {
    pub device_name: DeviceName,
    pub device_number: u8,
    /// Track tempo (pitch not applied).
    pub bpm_hundredths: u16,
    /// Pitch, `PITCH_NEUTRAL` = +0%.
    pub pitch: u32,
    /// Position in the 4-beat bar (1..=4).
    pub beat_within_bar: u8,
}

impl Beat {
    /// Milliseconds between beats at the current BPM.
    pub fn beat_interval_ms(&self) -> u32 {
        // 60_000 / bpm, where bpm = bpm_hundredths / 100.
        // So interval_ms = 60_000 * 100 / bpm_hundredths = 6_000_000 / bpm_hundredths.
        if self.bpm_hundredths == 0 {
            0
        } else {
            6_000_000 / self.bpm_hundredths as u32
        }
    }

    /// Milliseconds until the beat `beats` beats from now. Computed from the
    /// total rather than `beats * interval` so rounding doesn't accumulate.
    /// `0xffffffff` (the "no such beat" value) when the tempo is unknown.
    fn ms_until(&self, beats: u32) -> u32 {
        if self.bpm_hundredths == 0 {
            return u32::MAX;
        }
        (u64::from(beats) * 6_000_000 / u64::from(self.bpm_hundredths)) as u32
    }

    pub fn encode(&self) -> [u8; BEAT_LEN] {
        let mut buf = [0u8; BEAT_LEN];
        UpdateHeader {
            kind: update_kind::BEAT,
            device_name: self.device_name.clone(),
            subtype: BEAT_SUBTYPE,
            device_number: self.device_number,
        }
        .encode_into(&mut buf)
        .expect("buffer is always large enough");

        let bar = self.beat_within_bar.clamp(1, 4);
        let to_next_bar = 5 - u32::from(bar);
        let beats_ahead = [1, 2, to_next_bar, 4, to_next_bar + 4, 8];
        for (slot, beats) in (0x24..0x3c).step_by(4).zip(beats_ahead) {
            buf[slot..slot + 4].copy_from_slice(&self.ms_until(beats).to_be_bytes());
        }
        buf[0x3c..0x54].fill(0xff);

        buf[0x54..0x58].copy_from_slice(&self.pitch.to_be_bytes());
        buf[0x5a..0x5c].copy_from_slice(&self.bpm_hundredths.to_be_bytes());
        buf[0x5c] = bar;
        buf[0x5f] = self.device_number;

        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        if buf.len() < BEAT_LEN {
            return Err(DecodeError::TooShort {
                need: BEAT_LEN,
                have: buf.len(),
            });
        }
        let header = UpdateHeader::decode(buf)?;
        if header.kind != update_kind::BEAT {
            return Err(DecodeError::UnknownKind(header.kind));
        }
        Ok(Self {
            device_name: header.device_name,
            device_number: header.device_number,
            bpm_hundredths: u16::from_be_bytes([buf[0x5a], buf[0x5b]]),
            pitch: u32::from_be_bytes([buf[0x54], buf[0x55], buf[0x56], buf[0x57]]),
            beat_within_bar: buf[0x5c],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::PITCH_NEUTRAL;

    fn beat(bpm_hundredths: u16, beat_within_bar: u8) -> Beat {
        Beat {
            device_name: DeviceName::new("CDJ-3000").unwrap(),
            device_number: 2,
            bpm_hundredths,
            pitch: PITCH_NEUTRAL,
            beat_within_bar,
        }
    }

    fn u32_at(buf: &[u8], off: usize) -> u32 {
        u32::from_be_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
    }

    /// Full packet, byte for byte. Layout from beat-link's
    /// `VirtualCdj.BEAT_PAYLOAD` template (beat-link 8.x, in Beat Link
    /// Trigger), with its placeholders filled for player 2 at 128.00 BPM on
    /// beat 3 of the bar.
    #[test]
    fn beat_matches_beatlink_golden_bytes() {
        #[rustfmt::skip]
        let golden: [u8; BEAT_LEN] = [
            0x51, 0x73, 0x70, 0x74, 0x31, 0x57, 0x6d, 0x4a, 0x4f, 0x4c, 0x28, 0x43, 0x44, 0x4a, 0x2d, 0x33,
            0x30, 0x30, 0x30, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
            0x00, 0x02, 0x00, 0x3c, 0x00, 0x00, 0x01, 0xd4, 0x00, 0x00, 0x03, 0xa9, 0x00, 0x00, 0x03, 0xa9,
            0x00, 0x00, 0x07, 0x53, 0x00, 0x00, 0x0a, 0xfc, 0x00, 0x00, 0x0e, 0xa6, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x32, 0x00, 0x03, 0x00, 0x00, 0x02,
        ];
        assert_eq!(beat(12800, 3).encode(), golden);
    }

    /// The offsets beat-link's `Beat` getters read (from `Beat.class`), so a
    /// layout regression names the field it broke.
    #[test]
    fn fields_land_where_beatlink_reads_them() {
        let bytes = beat(12000, 2).encode();
        assert_eq!(bytes[0x0a], 0x28, "packet kind");
        assert_eq!(bytes[0x21], 2, "DeviceUpdate.getDeviceNumber reads 0x21");
        let pitch = u32::from_be_bytes([0, bytes[0x55], bytes[0x56], bytes[0x57]]);
        assert_eq!(pitch, 0x10_0000, "Beat.getPitch reads 3 bytes at 0x55");
        assert_eq!(&bytes[0x5a..0x5c], &12000u16.to_be_bytes(), "Beat.getBpm reads 0x5a");
        assert_eq!(bytes[0x5c], 2, "Beat.getBeatWithinBar reads 0x5c");
        assert_eq!(bytes[0x5f], 2, "trailing device-number copy");
    }

    #[test]
    fn bar_timings_follow_beat_within_bar() {
        // 120 BPM: 500 ms per beat. next bar is 4, 3, 2, 1 beats away.
        for (bar, beats_to_bar) in [(1u8, 4u32), (2, 3), (3, 2), (4, 1)] {
            let bytes = beat(12000, bar).encode();
            assert_eq!(u32_at(&bytes, 0x24), 500, "next beat, bar beat {bar}");
            assert_eq!(u32_at(&bytes, 0x28), 1000, "2nd beat, bar beat {bar}");
            assert_eq!(u32_at(&bytes, 0x2c), 500 * beats_to_bar, "next bar, bar beat {bar}");
            assert_eq!(u32_at(&bytes, 0x30), 2000, "4th beat, bar beat {bar}");
            assert_eq!(u32_at(&bytes, 0x34), 500 * (beats_to_bar + 4), "2nd bar, bar beat {bar}");
            assert_eq!(u32_at(&bytes, 0x38), 4000, "8th beat, bar beat {bar}");
        }
    }

    #[test]
    fn unknown_tempo_sends_no_such_beat_timings() {
        let bytes = beat(0, 1).encode();
        for slot in (0x24..0x3c).step_by(4) {
            assert_eq!(u32_at(&bytes, slot), u32::MAX);
        }
    }

    #[test]
    fn beat_roundtrip() {
        let b = beat(12800, 3);
        let d = Beat::decode(&b.encode()).unwrap();
        assert_eq!(d, b);
    }

    #[test]
    fn decode_rejects_other_kinds() {
        let mut bytes = beat(12800, 3).encode();
        bytes[0x0a] = 0x29;
        assert!(matches!(Beat::decode(&bytes), Err(DecodeError::UnknownKind(0x29))));
    }

    #[test]
    fn beat_interval_at_120_bpm_is_500ms() {
        assert_eq!(beat(12000, 1).beat_interval_ms(), 500);
    }

    #[test]
    fn beat_within_bar_is_clamped() {
        let bytes = beat(12000, 9).encode();
        assert_eq!(bytes[0x5c], 4);
    }
}
