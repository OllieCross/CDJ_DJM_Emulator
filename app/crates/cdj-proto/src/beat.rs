//! Beat packet (port 50001, kind 0x28).
//!
//! A beat packet is emitted by a player on every beat boundary. It carries:
//!
//! * BPM (hundredths), so receivers can interpolate between beats
//! * The time-to-the-next beat and the next 2nd/4th/8th/16th/32nd/64th beat,
//!   so tempo-follow clients (ShowKontrol, DJM master sync) can schedule
//!   events ahead of time
//! * The `beat_within_bar` (1..=4) so 4-beat bar structure is observable
//! * The emitting device number
//!
//! ## Layout (96 bytes)
//!
//! ```text
//! 0..10    magic
//! 10       kind = 0x28
//! 11       0x00
//! 12..32   device_name
//! 32       0x01 sub-type marker
//! 33       0x00 sub-type byte (TODO: verify against pcap)
//! 34..36   length = 0x0060 (96)
//! 36..40   ms-to-next-beat        (u32 BE)
//! 40..44   ms-to-2nd-beat         (u32 BE)
//! 44..48   ms-to-4th-beat         (u32 BE)
//! 48..52   ms-to-8th-beat         (u32 BE)
//! 52..56   ms-to-16th-beat        (u32 BE)
//! 56..60   ms-to-32nd-beat        (u32 BE)
//! 60..64   ms-to-64th-beat        (u32 BE)
//! 64..84   24 bytes reserved / pitch info (zeroed in idle)
//! 85       beat_within_bar (1..=4)
//! 86..88   reserved
//! 88..90   bpm_hundredths (u16 BE)
//! 90..95   reserved
//! 95       device_number
//! ```
//!
//! The offsets at 84..95 match beat-link's BeatPacket parser; trailing reserved
//! fields are zeroed here and flagged for pcap validation in a later pass.

use crate::error::DecodeError;
use crate::header::{DeviceName, Header, PacketKind};

pub const BEAT_LEN: usize = 96;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Beat {
    pub device_name: DeviceName,
    pub device_number: u8,
    pub bpm_hundredths: u16,
    /// Position in the 4-beat bar (1..=4).
    pub beat_within_bar: u8,
}

impl Beat {
    /// Milliseconds between beats at the current BPM. `ms-to-next-beat` etc.
    /// are computed from this assuming no tempo drift inside a bar.
    pub fn beat_interval_ms(&self) -> u32 {
        // 60_000 / bpm, where bpm = bpm_hundredths / 100.
        // So interval_ms = 60_000 * 100 / bpm_hundredths = 6_000_000 / bpm_hundredths.
        if self.bpm_hundredths == 0 {
            0
        } else {
            6_000_000 / self.bpm_hundredths as u32
        }
    }

    pub fn encode(&self) -> [u8; BEAT_LEN] {
        let mut buf = [0u8; BEAT_LEN];
        Header {
            kind: PacketKind::Other(0x28),
            device_name: self.device_name.clone(),
        }
        .encode_into(&mut buf[..Header::ENCODED_LEN])
        .unwrap();

        buf[32] = 0x01;
        buf[33] = 0x00;
        buf[34..36].copy_from_slice(&(BEAT_LEN as u16).to_be_bytes());

        let interval = self.beat_interval_ms();
        for (slot, mult) in (36..64).step_by(4).zip([1u32, 2, 4, 8, 16, 32, 64]) {
            let ms = interval.saturating_mul(mult);
            buf[slot..slot + 4].copy_from_slice(&ms.to_be_bytes());
        }

        buf[85] = self.beat_within_bar.clamp(1, 4);
        buf[88..90].copy_from_slice(&self.bpm_hundredths.to_be_bytes());
        buf[95] = self.device_number;

        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        if buf.len() < BEAT_LEN {
            return Err(DecodeError::TooShort {
                need: BEAT_LEN,
                have: buf.len(),
            });
        }
        let header = Header::decode(buf)?;
        if header.kind.to_byte() != 0x28 {
            return Err(DecodeError::UnknownKind(header.kind.to_byte()));
        }
        Ok(Self {
            device_name: header.device_name,
            beat_within_bar: buf[85],
            bpm_hundredths: u16::from_be_bytes([buf[88], buf[89]]),
            device_number: buf[95],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beat_roundtrip() {
        let b = Beat {
            device_name: DeviceName::new("CDJ-3000").unwrap(),
            device_number: 2,
            bpm_hundredths: 12800,
            beat_within_bar: 3,
        };
        let bytes = b.encode();
        assert_eq!(bytes.len(), BEAT_LEN);
        assert_eq!(bytes[10], 0x28);
        let d = Beat::decode(&bytes).unwrap();
        assert_eq!(d, b);
    }

    #[test]
    fn beat_length_field_matches() {
        let b = Beat {
            device_name: DeviceName::new("X").unwrap(),
            device_number: 1,
            bpm_hundredths: 12000,
            beat_within_bar: 1,
        };
        let bytes = b.encode();
        let len = u16::from_be_bytes([bytes[34], bytes[35]]);
        assert_eq!(len as usize, BEAT_LEN);
    }

    #[test]
    fn beat_interval_at_120_bpm_is_500ms() {
        let b = Beat {
            device_name: DeviceName::new("X").unwrap(),
            device_number: 1,
            bpm_hundredths: 12000,
            beat_within_bar: 1,
        };
        assert_eq!(b.beat_interval_ms(), 500);
    }

    #[test]
    fn next_beat_predictions_are_multiples_of_interval() {
        let b = Beat {
            device_name: DeviceName::new("X").unwrap(),
            device_number: 1,
            bpm_hundredths: 12000, // 500 ms per beat
            beat_within_bar: 1,
        };
        let bytes = b.encode();
        let mut predictions = [0u32; 7];
        for (i, slot) in (36..64).step_by(4).enumerate() {
            predictions[i] = u32::from_be_bytes([
                bytes[slot],
                bytes[slot + 1],
                bytes[slot + 2],
                bytes[slot + 3],
            ]);
        }
        assert_eq!(predictions, [500, 1000, 2000, 4000, 8000, 16000, 32000]);
    }

    #[test]
    fn beat_within_bar_is_clamped() {
        let b = Beat {
            device_name: DeviceName::new("X").unwrap(),
            device_number: 1,
            bpm_hundredths: 12000,
            beat_within_bar: 9,
        };
        let bytes = b.encode();
        assert_eq!(bytes[85], 4);
    }
}
