//! Status packets on port 50002.
//!
//! Two distinct packets share this port:
//!
//! * **CDJ status** (kind 0x0a) - each player broadcasts its current state
//!   (track, beat, bpm, play/pause, on-air, master status) roughly every
//!   200 ms.
//! * **DJM status** (kind 0x29) - the mixer broadcasts fader positions,
//!   crossfader, and master-tempo info.
//!
//! ## Minimal-viable fidelity (M1)
//!
//! For M1 we emit **idle-state** packets: correct magic, correct kind,
//! correct length, correct device number + on-air flags + BPM. Fields whose
//! exact meaning or byte offset is not yet verified against a real capture
//! are documented with TODO markers and currently carry zeros or the values
//! that Deep-Symmetry's beat-link source uses for idle.
//!
//! These packets are sufficient for another device on the network to
//! *discover* our virtual CDJ/DJM on :50002 and see that they are "present,
//! idle, BPM 120, not playing". Richer state (track ID, beat grid position,
//! tempo pitch) will land in M2 when we also have audio.

use crate::error::DecodeError;
use crate::header::{DeviceName, Header, PacketKind};

pub const CDJ_STATUS_LEN: usize = 0xd4; // 212 bytes, CDJ-3000 size
pub const DJM_STATUS_LEN: usize = 0x38; // 56 bytes

/// BPM is transmitted in hundredths: 120.00 -> 12000 -> 0x2EE0.
fn encode_bpm_u16(bpm_hundredths: u16) -> [u8; 2] {
    bpm_hundredths.to_be_bytes()
}

// ---------- CDJ status ----------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdjStatus {
    pub device_name: DeviceName,
    pub device_number: u8,
    pub bpm_hundredths: u16,
    /// Playing (true) vs paused/stopped (false).
    pub playing: bool,
    /// True if this device has been chosen as tempo master.
    pub master: bool,
    /// True if the mixer has this channel on-air.
    pub on_air: bool,
    /// Monotonically-increasing packet counter; wraps u32.
    pub packet_counter: u32,
    /// Position in the 4-beat bar (1..=4), used by sync-aware clients.
    pub beat_within_bar: u8,
    /// Rekordbox track ID currently loaded (0 = nothing loaded).
    /// Beat-link reads this from offset 0x2C and uses it to query the dbserver.
    pub track_id: u32,
    /// Absolute beat number since start of track (1-based). Beat-link reads this
    /// at offset 0x48 to drive the Time/Remain display. 0 = no position known.
    pub beat_number: u32,
}

impl CdjStatus {
    pub fn idle(device_name: DeviceName, device_number: u8) -> Self {
        Self {
            device_name,
            device_number,
            bpm_hundredths: 12000, // 120.00
            playing: false,
            master: false,
            on_air: false,
            packet_counter: 0,
            beat_within_bar: 1,
            track_id: 0,
            beat_number: 0,
        }
    }

    pub fn encode(&self) -> [u8; CDJ_STATUS_LEN] {
        let mut buf = [0u8; CDJ_STATUS_LEN];
        Header {
            kind: PacketKind::Announce, // CDJ status shares kind 0x0a with announce
            device_name: self.device_name.clone(),
        }
        .encode_into(&mut buf[..Header::ENCODED_LEN])
        .unwrap();

        // Header layout per beat-link DeviceUpdate.java (DEVICE_NUMBER_OFFSET = 0x21)
        // and CdjStatus.java (payloadLength at 0x22, packet length = payloadLength + 0x24):
        //   0x20: 0x01 sub-type marker
        //   0x21: device number  <- beat-link's getDeviceNumber() reads here
        //   0x22-0x23: payload length BE u16 = total - 0x24
        //   0x24+: payload
        buf[0x20] = 0x01;
        buf[0x21] = self.device_number;
        let payload_len = (CDJ_STATUS_LEN - 0x24) as u16;
        buf[0x22..0x24].copy_from_slice(&payload_len.to_be_bytes());

        // Offsets verified against beat-link CdjStatus.java (CDJ-3000, 212-byte packet).
        buf[0x24] = self.device_number; // duplicate copy in payload, observed in real packets
        // activity: 4 = USB track loaded; 0 = no track. BLT skips position
        // tracking (and dbserver queries) when this is 0.
        buf[0x25] = if self.track_id != 0 { 4 } else { 0 };

        // Track source fields. 0x28 tells BLT which player's dbserver to query;
        // 0x2C-0x2F is the rekordbox ID BLT passes in the metadata request.
        // All three must be non-zero/correct for BLT to open a dbserver connection.
        if self.track_id != 0 {
            buf[0x28] = self.device_number; // track source player (our own dbserver)
            buf[0x29] = 3; // source slot: USB_SLOT
            buf[0x2A] = 1; // track type: REKORDBOX
            buf[0x2C..0x30].copy_from_slice(&self.track_id.to_be_bytes()); // rekordbox ID
        }

        // PlayState1 (0x7B = 123): PLAYING=3, PAUSED=5. Tells clients track is loaded.
        buf[0x7B] = if self.playing { 3 } else { 5 };

        // Status flags at 0x89 (137): bit6=playing, bit5=master, bit3=on-air.
        let flags = ((self.playing as u8) << 6)
            | ((self.master as u8) << 5)
            | ((self.on_air as u8) << 3);
        buf[0x89] = flags;

        // Pitch at 0x8D (141), 3 bytes BE. 0x100000 = neutral (1× speed).
        // Effective BPM = bpm * (pitch / 0x100000). Zero pitch gives BPM = 0.
        buf[0x8D..0x90].copy_from_slice(&[0x10, 0x00, 0x00]);
        // Three redundant pitch copies; clients may read any of them.
        buf[153..156].copy_from_slice(&[0x10, 0x00, 0x00]);
        buf[193..196].copy_from_slice(&[0x10, 0x00, 0x00]);
        buf[197..200].copy_from_slice(&[0x10, 0x00, 0x00]);

        // BPM at 0x92 (146), 2 bytes BE, in hundredths (120.00 = 12000).
        buf[0x92..0x94].copy_from_slice(&encode_bpm_u16(self.bpm_hundredths));

        // Beat within bar at 0xA6 (166).
        buf[0xA6] = self.beat_within_bar.clamp(1, 4);

        // Absolute beat number since start of track at 0x48 (72), 4 bytes BE.
        // Beat-link uses this for the Time/Remain display. 0 = position unknown.
        if self.beat_number > 0 {
            buf[0x48..0x4C].copy_from_slice(&self.beat_number.to_be_bytes());
        }

        // Packet counter at 0xC8 (200), 4 bytes BE.
        buf[0xC8..0xCC].copy_from_slice(&self.packet_counter.to_be_bytes());

        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        if buf.len() < CDJ_STATUS_LEN {
            return Err(DecodeError::TooShort {
                need: CDJ_STATUS_LEN,
                have: buf.len(),
            });
        }
        let header = Header::decode(buf)?;
        let device_number = buf[0x21]; // 33 - matches beat-link DEVICE_NUMBER_OFFSET
        let flags = buf[0x89]; // 137
        let bpm_hundredths = u16::from_be_bytes([buf[0x92], buf[0x93]]); // 146-147
        let mut ctr = [0u8; 4];
        ctr.copy_from_slice(&buf[0xC8..0xCC]);
        let track_id = u32::from_be_bytes([buf[0x2C], buf[0x2D], buf[0x2E], buf[0x2F]]);
        let beat_number = u32::from_be_bytes([buf[0x48], buf[0x49], buf[0x4A], buf[0x4B]]);
        Ok(Self {
            device_name: header.device_name,
            device_number,
            bpm_hundredths,
            playing: flags & 0x40 != 0, // bit6
            on_air: flags & 0x08 != 0,  // bit3
            master: flags & 0x20 != 0,  // bit5
            packet_counter: u32::from_be_bytes(ctr),
            beat_within_bar: buf[0xA6].max(1).min(4), // 166
            track_id,
            beat_number,
        })
    }
}

// ---------- DJM status ----------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DjmStatus {
    pub device_name: DeviceName,
    pub device_number: u8,
    pub bpm_hundredths: u16,
    pub master_handoff_source: u8, // 0 = no handoff pending
    /// Bit N set if channel N+1 is currently on-air.
    pub channels_on_air: u8,
}

impl DjmStatus {
    pub fn idle(device_name: DeviceName) -> Self {
        Self {
            device_name,
            device_number: 33,
            bpm_hundredths: 12000,
            master_handoff_source: 0,
            channels_on_air: 0,
        }
    }

    pub fn encode(&self) -> [u8; DJM_STATUS_LEN] {
        let mut buf = [0u8; DJM_STATUS_LEN];
        Header {
            kind: PacketKind::Other(0x29),
            device_name: self.device_name.clone(),
        }
        .encode_into(&mut buf[..Header::ENCODED_LEN])
        .unwrap();
        let mut i = Header::ENCODED_LEN;
        buf[i] = 0x01;
        i += 1;
        buf[i] = 0x00; // TODO: verify DJM subtype
        i += 1;
        buf[i..i + 2].copy_from_slice(&(DJM_STATUS_LEN as u16).to_be_bytes());
        i += 2;
        buf[i] = self.device_number; // 36
        buf[37] = self.channels_on_air;
        buf[39] = self.master_handoff_source;
        buf[40..42].copy_from_slice(&encode_bpm_u16(self.bpm_hundredths));
        // bytes 42..56: fader/crossfader data, zeroed for idle
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        if buf.len() < DJM_STATUS_LEN {
            return Err(DecodeError::TooShort {
                need: DJM_STATUS_LEN,
                have: buf.len(),
            });
        }
        let header = Header::decode(buf)?;
        Ok(Self {
            device_name: header.device_name,
            device_number: buf[36],
            channels_on_air: buf[37],
            master_handoff_source: buf[39],
            bpm_hundredths: u16::from_be_bytes([buf[40], buf[41]]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cdj_status_roundtrip() {
        let s = CdjStatus {
            device_name: DeviceName::new("CDJ-3000").unwrap(),
            device_number: 2,
            bpm_hundredths: 12800, // 128.00
            playing: true,
            master: false,
            on_air: true,
            packet_counter: 0xdead_beef,
            beat_within_bar: 3,
            track_id: 42,
            beat_number: 256,
        };
        let bytes = s.encode();
        assert_eq!(bytes.len(), CDJ_STATUS_LEN);
        assert_eq!(bytes[10], 0x0a);
        let d = CdjStatus::decode(&bytes).unwrap();
        assert_eq!(d, s);
    }

    #[test]
    fn djm_status_roundtrip() {
        let s = DjmStatus {
            device_name: DeviceName::new("DJM-V10").unwrap(),
            device_number: 33,
            bpm_hundredths: 12450,
            master_handoff_source: 0,
            channels_on_air: 0b0000_0101, // channels 1 & 3 on-air
        };
        let bytes = s.encode();
        assert_eq!(bytes.len(), DJM_STATUS_LEN);
        assert_eq!(bytes[10], 0x29);
        let d = DjmStatus::decode(&bytes).unwrap();
        assert_eq!(d, s);
    }
}
