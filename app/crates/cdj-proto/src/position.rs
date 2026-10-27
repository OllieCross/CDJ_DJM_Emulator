//! Absolute position packet (port 50001, kind 0x0b), sent by CDJ-3000s about
//! every 30 ms while a track is loaded, playing or not. Lets clients follow
//! the playhead exactly instead of interpolating between beats.
//!
//! ## Layout (60 bytes)
//!
//! Matches beat-link's `PrecisePosition` parser and the dysentery figure.
//!
//! ```text
//! 0x00..0x24  UpdateHeader: kind 0x0b, subtype 0x02, length-remaining 0x18
//! 0x24..0x28  track length, whole seconds (u32 BE)
//! 0x28..0x2c  playhead, ms (u32 BE)
//! 0x2c..0x30  pitch fader, percent x 100 (i32 BE; 326 = +3.26%)
//! 0x30..0x38  0
//! 0x38..0x3c  effective BPM x 10 (u32 BE; 0xffffffff = unknown)
//! ```

use crate::error::DecodeError;
use crate::header::{update_kind, DeviceName, UpdateHeader};

pub const POSITION_LEN: usize = 0x3c;
/// beat-link does not check this byte; value taken from the dysentery figure.
pub const POSITION_SUBTYPE: u8 = 0x02;
const NO_BPM: u32 = 0xffff_ffff;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrecisePosition {
    pub device_name: DeviceName,
    pub device_number: u8,
    pub track_length_s: u32,
    pub playhead_ms: u32,
    /// Pitch fader in hundredths of a percent (+3.26% = 326).
    pub pitch_hundredths_percent: i32,
    /// On-screen BPM (pitch applied) in tenths; `None` when unknown.
    pub effective_bpm_tenths: Option<u32>,
}

impl PrecisePosition {
    pub fn encode(&self) -> [u8; POSITION_LEN] {
        let mut buf = [0u8; POSITION_LEN];
        UpdateHeader {
            kind: update_kind::PRECISE_POSITION,
            device_name: self.device_name.clone(),
            subtype: POSITION_SUBTYPE,
            device_number: self.device_number,
        }
        .encode_into(&mut buf)
        .expect("buffer is always large enough");
        buf[0x24..0x28].copy_from_slice(&self.track_length_s.to_be_bytes());
        buf[0x28..0x2c].copy_from_slice(&self.playhead_ms.to_be_bytes());
        buf[0x2c..0x30].copy_from_slice(&self.pitch_hundredths_percent.to_be_bytes());
        buf[0x38..0x3c].copy_from_slice(&self.effective_bpm_tenths.unwrap_or(NO_BPM).to_be_bytes());
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        if buf.len() < POSITION_LEN {
            return Err(DecodeError::TooShort {
                need: POSITION_LEN,
                have: buf.len(),
            });
        }
        let header = UpdateHeader::decode(buf)?;
        if header.kind != update_kind::PRECISE_POSITION {
            return Err(DecodeError::UnknownKind(header.kind));
        }
        let word = |off: usize| [buf[off], buf[off + 1], buf[off + 2], buf[off + 3]];
        let bpm = u32::from_be_bytes(word(0x38));
        Ok(Self {
            device_name: header.device_name,
            device_number: header.device_number,
            track_length_s: u32::from_be_bytes(word(0x24)),
            playhead_ms: u32::from_be_bytes(word(0x28)),
            pitch_hundredths_percent: i32::from_be_bytes(word(0x2c)),
            effective_bpm_tenths: (bpm != NO_BPM).then_some(bpm),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PrecisePosition {
        PrecisePosition {
            device_name: DeviceName::new("CDJ-3000").unwrap(),
            device_number: 3,
            track_length_s: 245,
            playhead_ms: 61_234,
            pitch_hundredths_percent: -326,
            effective_bpm_tenths: Some(1238),
        }
    }

    #[test]
    fn fields_land_where_beatlink_reads_them() {
        let b = sample().encode();
        assert_eq!(b[0x0a], 0x0b, "packet kind");
        assert_eq!(b[0x21], 3, "getDeviceNumber reads 0x21");
        assert_eq!(&b[0x22..0x24], &[0x00, 0x18], "length - 0x24");
        assert_eq!(&b[0x24..0x28], &245u32.to_be_bytes(), "getTrackLength reads 0x24");
        assert_eq!(&b[0x28..0x2c], &61_234u32.to_be_bytes(), "getPlaybackPosition reads 0x28");
        assert_eq!(&b[0x2c..0x30], &(-326i32).to_be_bytes(), "pitch is signed at 0x2c");
        assert_eq!(&b[0x38..0x3c], &1238u32.to_be_bytes(), "BPM x 10 at 0x38");
    }

    #[test]
    fn unknown_bpm_is_all_ones() {
        let b = PrecisePosition {
            effective_bpm_tenths: None,
            ..sample()
        }
        .encode();
        assert_eq!(&b[0x38..0x3c], &[0xff; 4]);
    }

    #[test]
    fn roundtrip() {
        let p = sample();
        assert_eq!(PrecisePosition::decode(&p.encode()).unwrap(), p);
    }
}
