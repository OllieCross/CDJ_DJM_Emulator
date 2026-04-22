//! Port 50000 announce + keep-alive + device-number-claim packets.
//!
//! ## Status of these structures
//!
//! The layouts below are drawn from the Deep-Symmetry dysentery analysis and
//! cross-referenced against the Java beat-link source. They are well-understood
//! at the *field* level, but several inter-field "constant" bytes are still
//! treated as opaque padding by the community - we copy them verbatim so a real
//! CDJ/DJM/rekordbox on the same LAN accepts our packets as authentic.
//!
//! These fixtures **must** be re-validated against a real pcap capture once
//! hardware is available (see discovery.md §9 risk #1). Byte-level tests in
//! this module currently verify round-trip encode-decode only; golden-byte
//! fixtures will be added when we have an authoritative capture.
//!
//! ## Layout - Keep-Alive (kind 0x06, sub 0x02), total 54 bytes
//!
//! ```text
//! offset  len  field
//! ------  ---  -------------------------------------------------------------
//! 0       10   magic "Qspt1WmJOL"
//! 10      1    kind = 0x06
//! 11      1    0x00
//! 12      20   device_name (UTF-8, NUL-padded)
//! 32      1    0x01     sub-type marker
//! 33      1    0x02     sub-type value (keep-alive)
//! 34      2    length   big-endian u16 of total packet length (0x0036 = 54)
//! 36      1    device_number  (1..=4 for players, 33 for DJM)
//! 37      1    0x01     constant (believed: "device number assigned")
//! 38      6    mac_address
//! 44      4    ip_address (v4)
//! 48      6    trailer  0x02 0x00 0x00 0x00 0x01 0x00
//! ```

use crate::error::DecodeError;
use crate::header::{DeviceName, Header, PacketKind, DEVICE_NAME_LEN, MAGIC_LEN};

pub const KEEPALIVE_LEN: usize = 54;
pub const KEEPALIVE_SUBTYPE_MARKER: u8 = 0x01;
pub const KEEPALIVE_SUBTYPE: u8 = 0x02;
pub const KEEPALIVE_CONSTANT_37: u8 = 0x01;
pub const KEEPALIVE_TRAILER: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x01, 0x00];

/// Device-number ranges from the Pro DJ Link analysis.
pub const PLAYER_NUM_MIN: u8 = 1;
pub const PLAYER_NUM_MAX: u8 = 4;
pub const MIXER_NUM: u8 = 33;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeepAlive {
    pub device_name: DeviceName,
    pub device_number: u8,
    pub mac: [u8; 6],
    pub ip: [u8; 4],
}

impl KeepAlive {
    pub fn encode(&self) -> [u8; KEEPALIVE_LEN] {
        let mut buf = [0u8; KEEPALIVE_LEN];
        let header = Header {
            kind: PacketKind::KeepAlive,
            device_name: self.device_name.clone(),
        };
        header
            .encode_into(&mut buf[..Header::ENCODED_LEN])
            .expect("buffer is always large enough");

        let mut i = Header::ENCODED_LEN;
        buf[i] = KEEPALIVE_SUBTYPE_MARKER;
        i += 1;
        buf[i] = KEEPALIVE_SUBTYPE;
        i += 1;
        buf[i..i + 2].copy_from_slice(&(KEEPALIVE_LEN as u16).to_be_bytes());
        i += 2;
        buf[i] = self.device_number;
        i += 1;
        buf[i] = KEEPALIVE_CONSTANT_37;
        i += 1;
        buf[i..i + 6].copy_from_slice(&self.mac);
        i += 6;
        buf[i..i + 4].copy_from_slice(&self.ip);
        i += 4;
        buf[i..i + 6].copy_from_slice(&KEEPALIVE_TRAILER);

        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        if buf.len() < KEEPALIVE_LEN {
            return Err(DecodeError::TooShort {
                need: KEEPALIVE_LEN,
                have: buf.len(),
            });
        }
        let header = Header::decode(buf)?;
        if !matches!(header.kind, PacketKind::KeepAlive) {
            return Err(DecodeError::UnknownKind(header.kind.to_byte()));
        }
        let mut i = Header::ENCODED_LEN;
        let _marker = buf[i];
        i += 1;
        let subtype = buf[i];
        i += 1;
        if subtype != KEEPALIVE_SUBTYPE {
            return Err(DecodeError::UnexpectedSubtype {
                kind: PacketKind::KeepAlive.to_byte(),
                got: subtype,
            });
        }
        // skip length
        i += 2;
        let device_number = buf[i];
        i += 1;
        // skip 0x01 constant
        i += 1;
        let mut mac = [0u8; 6];
        mac.copy_from_slice(&buf[i..i + 6]);
        i += 6;
        let mut ip = [0u8; 4];
        ip.copy_from_slice(&buf[i..i + 4]);

        Ok(Self {
            device_name: header.device_name,
            device_number,
            mac,
            ip,
        })
    }
}

/// Helper asserting we can map raw indices as documented above. Catches any
/// future drift between the layout comment and the constants.
#[allow(dead_code)]
const _LAYOUT_CHECK: () = {
    assert!(MAGIC_LEN == 10);
    assert!(DEVICE_NAME_LEN == 20);
    assert!(Header::ENCODED_LEN == 32);
    assert!(KEEPALIVE_LEN == 54);
};

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> KeepAlive {
        KeepAlive {
            device_name: DeviceName::new("CDJ-3000").unwrap(),
            device_number: 1,
            mac: [0x02, 0x00, 0x00, 0x00, 0x00, 0x01],
            ip: [169, 254, 1, 10],
        }
    }

    #[test]
    fn keepalive_roundtrip() {
        let k = sample();
        let bytes = k.encode();
        let d = KeepAlive::decode(&bytes).unwrap();
        assert_eq!(k, d);
    }

    #[test]
    fn keepalive_has_correct_magic_and_kind() {
        let bytes = sample().encode();
        assert_eq!(&bytes[..10], b"Qspt1WmJOL");
        assert_eq!(bytes[10], 0x06);
        assert_eq!(bytes[11], 0x00);
    }

    #[test]
    fn keepalive_has_correct_length_field() {
        let bytes = sample().encode();
        let len = u16::from_be_bytes([bytes[34], bytes[35]]);
        assert_eq!(len as usize, KEEPALIVE_LEN);
    }

    #[test]
    fn keepalive_trailer_matches_fixture() {
        let bytes = sample().encode();
        assert_eq!(&bytes[48..54], &KEEPALIVE_TRAILER);
    }

    #[test]
    fn keepalive_carries_device_num_mac_ip() {
        let bytes = sample().encode();
        assert_eq!(bytes[36], 1);
        assert_eq!(&bytes[38..44], &[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
        assert_eq!(&bytes[44..48], &[169, 254, 1, 10]);
    }

    #[test]
    fn keepalive_rejects_short_buffer() {
        let err = KeepAlive::decode(&[0u8; 10]).unwrap_err();
        assert!(matches!(err, DecodeError::TooShort { .. }));
    }
}
