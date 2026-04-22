//! Device-number claim handshake (port 50000).
//!
//! When a Pro DJ Link device comes online it performs a 3-stage claim on
//! UDP :50000 to settle which channel (1..=4 for players, 33 for a DJM) it
//! will occupy. The community analysis (dysentery, beat-link) covers the
//! field-level structure of each stage:
//!
//! | Stage | Kind | Meaning                                             |
//! |-------|------|-----------------------------------------------------|
//! | 1     | 0x00 | Pre-claim / discovery ("anyone else already here?") |
//! | 2     | 0x02 | Claiming a specific device number                   |
//! | 3     | 0x04 | Final confirmation                                  |
//!
//! Each stage is broadcast three times at ~300 ms spacing. If another device
//! objects (replies with a conflict announce) the claimant picks a new number
//! and restarts.
//!
//! ## Fidelity caveat
//!
//! We emit these stages so our virtual devices look authentic on the wire and
//! so we can later interoperate with a real CDJ/DJM that performs the dance
//! against us. The exact byte layout in later firmwares has minor deltas that
//! need pcap validation (see discovery.md §9 risk #1). The field contents we
//! emit are correct; trailing "opaque" bytes come from dysentery's published
//! captures.
//!
//! Because in the emulator all device numbers are assigned by configuration
//! (we control all 5 devices in one process), the in-process orchestrator
//! skips the retry/backoff logic - it only uses the claim stages as a polite
//! announcement ritual. Collision handling will be implemented properly when
//! we support interop with external real devices.

use crate::error::DecodeError;
use crate::header::{DeviceName, Header, PacketKind};

/// Spacing between consecutive claim packets of the same stage (~300 ms).
pub const CLAIM_PACKET_SPACING_MS: u64 = 300;
/// Number of times each stage is repeated.
pub const CLAIM_REPEATS: u8 = 3;

pub const STAGE1_LEN: usize = 44;
pub const STAGE2_LEN: usize = 50;
pub const STAGE3_LEN: usize = 38;

// ---------- Stage 1: pre-claim ----------

/// Stage 1 - pre-claim. Announces presence and iteration step (1..=3) without
/// yet committing to a device number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimStage1 {
    pub device_name: DeviceName,
    pub step: u8,
    pub mac: [u8; 6],
}

impl ClaimStage1 {
    pub fn encode(&self) -> [u8; STAGE1_LEN] {
        let mut buf = [0u8; STAGE1_LEN];
        Header {
            kind: PacketKind::DeviceNumClaim1,
            device_name: self.device_name.clone(),
        }
        .encode_into(&mut buf[..Header::ENCODED_LEN])
        .unwrap();
        let mut i = Header::ENCODED_LEN;
        buf[i] = 0x01; // sub-type marker
        i += 1;
        buf[i] = 0x02; // sub-type byte (claim / device announce family)
        i += 1;
        buf[i..i + 2].copy_from_slice(&(STAGE1_LEN as u16).to_be_bytes());
        i += 2;
        buf[i] = self.step;
        i += 1;
        buf[i] = 0x01; // constant
        i += 1;
        buf[i..i + 6].copy_from_slice(&self.mac);
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        if buf.len() < STAGE1_LEN {
            return Err(DecodeError::TooShort {
                need: STAGE1_LEN,
                have: buf.len(),
            });
        }
        let header = Header::decode(buf)?;
        if !matches!(header.kind, PacketKind::DeviceNumClaim1) {
            return Err(DecodeError::UnknownKind(header.kind.to_byte()));
        }
        let mut i = Header::ENCODED_LEN + 4; // skip marker, subtype, length
        let step = buf[i];
        i += 2; // skip 0x01 constant
        let mut mac = [0u8; 6];
        mac.copy_from_slice(&buf[i..i + 6]);
        Ok(Self {
            device_name: header.device_name,
            step,
            mac,
        })
    }
}

// ---------- Stage 2: claiming a device number ----------

/// Stage 2 - claim a specific device number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimStage2 {
    pub device_name: DeviceName,
    pub step: u8,
    pub mac: [u8; 6],
    pub ip: [u8; 4],
    pub device_number: u8,
    /// `true` if the user configured the number (vs auto-negotiated).
    pub user_assigned: bool,
}

impl ClaimStage2 {
    pub fn encode(&self) -> [u8; STAGE2_LEN] {
        let mut buf = [0u8; STAGE2_LEN];
        Header {
            kind: PacketKind::DeviceNumClaim2,
            device_name: self.device_name.clone(),
        }
        .encode_into(&mut buf[..Header::ENCODED_LEN])
        .unwrap();
        let mut i = Header::ENCODED_LEN;
        buf[i] = 0x01;
        i += 1;
        buf[i] = 0x02;
        i += 1;
        buf[i..i + 2].copy_from_slice(&(STAGE2_LEN as u16).to_be_bytes());
        i += 2;
        buf[i..i + 4].copy_from_slice(&self.ip);
        i += 4;
        buf[i..i + 6].copy_from_slice(&self.mac);
        i += 6;
        buf[i] = self.device_number;
        i += 1;
        buf[i] = self.step;
        i += 1;
        buf[i] = 0x01; // constant
        i += 1;
        buf[i] = if self.user_assigned { 0x01 } else { 0x02 };
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        if buf.len() < STAGE2_LEN {
            return Err(DecodeError::TooShort {
                need: STAGE2_LEN,
                have: buf.len(),
            });
        }
        let header = Header::decode(buf)?;
        if !matches!(header.kind, PacketKind::DeviceNumClaim2) {
            return Err(DecodeError::UnknownKind(header.kind.to_byte()));
        }
        let mut i = Header::ENCODED_LEN + 4;
        let mut ip = [0u8; 4];
        ip.copy_from_slice(&buf[i..i + 4]);
        i += 4;
        let mut mac = [0u8; 6];
        mac.copy_from_slice(&buf[i..i + 6]);
        i += 6;
        let device_number = buf[i];
        i += 1;
        let step = buf[i];
        i += 2; // skip 0x01 constant
        let user_assigned = buf[i] == 0x01;
        Ok(Self {
            device_name: header.device_name,
            step,
            mac,
            ip,
            device_number,
            user_assigned,
        })
    }
}

// ---------- Stage 3: final confirmation ----------

/// Stage 3 - final "I am device N" confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimStage3 {
    pub device_name: DeviceName,
    pub device_number: u8,
    pub step: u8,
}

impl ClaimStage3 {
    pub fn encode(&self) -> [u8; STAGE3_LEN] {
        let mut buf = [0u8; STAGE3_LEN];
        Header {
            kind: PacketKind::DeviceNumClaim3,
            device_name: self.device_name.clone(),
        }
        .encode_into(&mut buf[..Header::ENCODED_LEN])
        .unwrap();
        let mut i = Header::ENCODED_LEN;
        buf[i] = 0x01;
        i += 1;
        buf[i] = 0x02;
        i += 1;
        buf[i..i + 2].copy_from_slice(&(STAGE3_LEN as u16).to_be_bytes());
        i += 2;
        buf[i] = self.device_number;
        i += 1;
        buf[i] = self.step;
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        if buf.len() < STAGE3_LEN {
            return Err(DecodeError::TooShort {
                need: STAGE3_LEN,
                have: buf.len(),
            });
        }
        let header = Header::decode(buf)?;
        if !matches!(header.kind, PacketKind::DeviceNumClaim3) {
            return Err(DecodeError::UnknownKind(header.kind.to_byte()));
        }
        let i = Header::ENCODED_LEN + 4;
        let device_number = buf[i];
        let step = buf[i + 1];
        Ok(Self {
            device_name: header.device_name,
            device_number,
            step,
        })
    }
}

// Compile-time layout sanity.
#[allow(dead_code)]
const _: () = {
    assert!(STAGE1_LEN == 44);
    assert!(STAGE2_LEN == 50);
    assert!(STAGE3_LEN == 38);
};

#[cfg(test)]
mod tests {
    use super::*;

    fn name() -> DeviceName {
        DeviceName::new("CDJ-3000").unwrap()
    }

    #[test]
    fn stage1_roundtrip() {
        let p = ClaimStage1 {
            device_name: name(),
            step: 2,
            mac: [1, 2, 3, 4, 5, 6],
        };
        let bytes = p.encode();
        assert_eq!(bytes.len(), STAGE1_LEN);
        assert_eq!(bytes[10], 0x00);
        assert_eq!(ClaimStage1::decode(&bytes).unwrap(), p);
    }

    #[test]
    fn stage2_roundtrip() {
        let p = ClaimStage2 {
            device_name: name(),
            step: 3,
            mac: [1, 2, 3, 4, 5, 6],
            ip: [169, 254, 1, 10],
            device_number: 2,
            user_assigned: true,
        };
        let bytes = p.encode();
        assert_eq!(bytes.len(), STAGE2_LEN);
        assert_eq!(bytes[10], 0x02);
        assert_eq!(ClaimStage2::decode(&bytes).unwrap(), p);
    }

    #[test]
    fn stage3_roundtrip() {
        let p = ClaimStage3 {
            device_name: name(),
            device_number: 4,
            step: 1,
        };
        let bytes = p.encode();
        assert_eq!(bytes.len(), STAGE3_LEN);
        assert_eq!(bytes[10], 0x04);
        assert_eq!(ClaimStage3::decode(&bytes).unwrap(), p);
    }
}
