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
        }
    }

    pub fn encode(&self) -> [u8; CDJ_STATUS_LEN] {
        let mut buf = [0u8; CDJ_STATUS_LEN];
        // Header: magic, kind=0x0a, 0x00, name
        Header {
            kind: PacketKind::Announce, // same kind byte 0x0a
            device_name: self.device_name.clone(),
        }
        .encode_into(&mut buf[..Header::ENCODED_LEN])
        .unwrap();

        let mut i = Header::ENCODED_LEN;
        buf[i] = 0x01; // sub-type marker
        i += 1;
        buf[i] = 0x03; // TODO: verify CDJ-3000 subtype against pcap
        i += 1;
        buf[i..i + 2].copy_from_slice(&(CDJ_STATUS_LEN as u16).to_be_bytes());
        i += 2;
        buf[i] = self.device_number; // 36
        // Payload follows with many fields we do not yet decode. Key offsets
        // below are drawn from the beat-link Java parser; all others zero.
        //
        // offsets within payload (from byte 37..end):
        //   37    activity (0=idle, 1=playing)  -- used as a heartbeat flag
        //   89    flags byte: bit0=playing, bit5=master, bit3=on_air
        //   90    bpm (u16 BE, hundredths)
        //   0xc8  packet counter (u32 BE)
        //
        // Everything else we leave as zero for now; beat-link tolerates that.
        buf[37] = if self.playing { 1 } else { 0 };
        let flags = (self.playing as u8) | ((self.on_air as u8) << 3) | ((self.master as u8) << 5);
        buf[89] = flags;
        let bpm = encode_bpm_u16(self.bpm_hundredths);
        buf[90..92].copy_from_slice(&bpm);
        buf[0xc8..0xc8 + 4].copy_from_slice(&self.packet_counter.to_be_bytes());

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
        let device_number = buf[36];
        let flags = buf[89];
        let bpm_hundredths = u16::from_be_bytes([buf[90], buf[91]]);
        let mut ctr = [0u8; 4];
        ctr.copy_from_slice(&buf[0xc8..0xc8 + 4]);
        Ok(Self {
            device_name: header.device_name,
            device_number,
            bpm_hundredths,
            playing: flags & 0x01 != 0,
            on_air: flags & 0x08 != 0,
            master: flags & 0x20 != 0,
            packet_counter: u32::from_be_bytes(ctr),
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
