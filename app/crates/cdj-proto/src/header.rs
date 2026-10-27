use crate::error::{DecodeError, EncodeError};

pub const MAGIC_LEN: usize = 10;
pub const MAGIC: [u8; MAGIC_LEN] = *b"Qspt1WmJOL";

pub const DEVICE_NAME_LEN: usize = 20;

/// Bytes `0x00` then `0x01` that sit between the device-name field and the
/// sub-type on most packet kinds. The dysentery analysis labels them as
/// "constant padding". Kept as a named constant so future readers don't have
/// to guess.
pub const HEADER_PAD_BEFORE_NAME: u8 = 0x00;
pub const HEADER_PAD_AFTER_NAME: u8 = 0x01;

/// Neutral pitch (+0%, 1x speed) as carried in beat and status packets.
/// `0x00000000` is -100%, `0x00200000` is +100%.
pub const PITCH_NEUTRAL: u32 = 0x0010_0000;

pub struct Magic;

impl Magic {
    pub fn validate(buf: &[u8]) -> Result<(), DecodeError> {
        if buf.len() < MAGIC_LEN {
            return Err(DecodeError::TooShort {
                need: MAGIC_LEN,
                have: buf.len(),
            });
        }
        if buf[..MAGIC_LEN] != MAGIC {
            let mut got = [0u8; MAGIC_LEN];
            got.copy_from_slice(&buf[..MAGIC_LEN]);
            return Err(DecodeError::BadMagic { got });
        }
        Ok(())
    }
}

/// Packet kind byte - the first byte after the magic.
///
/// Values are drawn from the dysentery analysis. We only list the kinds the
/// emulator currently handles; unknown kinds decode into `PacketKind::Other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PacketKind {
    /// Stage-1 device-number claim (port 50000).
    DeviceNumClaim1 = 0x00,
    /// Stage-2 device-number claim (port 50000).
    DeviceNumClaim2 = 0x02,
    /// Stage-3 device-number claim (port 50000).
    DeviceNumClaim3 = 0x04,
    /// Periodic keep-alive / device-announce (port 50000, 1.5 s cadence).
    KeepAlive = 0x06,
    /// Initial announce packet sent at startup (port 50000).
    /// CDJ player status on :50002 shares the 0x0a kind byte but is
    /// distinguished by destination port + sub-type; handled in the status
    /// module rather than as a separate variant here.
    Announce = 0x0a,
    Other(u8),
}

impl PacketKind {
    pub fn from_byte(b: u8) -> Self {
        match b {
            0x00 => Self::DeviceNumClaim1,
            0x02 => Self::DeviceNumClaim2,
            0x04 => Self::DeviceNumClaim3,
            0x06 => Self::KeepAlive,
            0x0a => Self::Announce,
            other => Self::Other(other),
        }
    }

    pub fn to_byte(self) -> u8 {
        match self {
            Self::DeviceNumClaim1 => 0x00,
            Self::DeviceNumClaim2 => 0x02,
            Self::DeviceNumClaim3 => 0x04,
            Self::KeepAlive => 0x06,
            Self::Announce => 0x0a,
            Self::Other(b) => b,
        }
    }
}

/// 20-byte device name field, space-padded, NUL-terminated if shorter.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DeviceName([u8; DEVICE_NAME_LEN]);

impl DeviceName {
    pub fn new(s: &str) -> Result<Self, EncodeError> {
        let bytes = s.as_bytes();
        if bytes.len() > DEVICE_NAME_LEN {
            return Err(EncodeError::DeviceNameTooLong(bytes.len()));
        }
        let mut out = [0u8; DEVICE_NAME_LEN];
        out[..bytes.len()].copy_from_slice(bytes);
        Ok(Self(out))
    }

    pub fn as_bytes(&self) -> &[u8; DEVICE_NAME_LEN] {
        &self.0
    }

    pub fn as_str_lossy(&self) -> std::borrow::Cow<'_, str> {
        let end = self.0.iter().position(|&b| b == 0).unwrap_or(DEVICE_NAME_LEN);
        String::from_utf8_lossy(&self.0[..end])
    }

    pub fn from_bytes(buf: &[u8; DEVICE_NAME_LEN]) -> Self {
        Self(*buf)
    }
}

/// Common 32-byte header prefix shared by the `0x06` and `0x0a` packet families:
///
/// ```text
/// 0..10   magic ("Qspt1WmJOL")
/// 10      packet kind
/// 11      0x00
/// 12..32  device name (20 bytes)
/// ```
///
/// Packets on port 50002 add a `0x01` byte + sub-type byte after this; the
/// decoders for those packets handle it.
#[derive(Debug, Clone)]
pub struct Header {
    pub kind: PacketKind,
    pub device_name: DeviceName,
}

impl Header {
    pub const ENCODED_LEN: usize = MAGIC_LEN + 1 + 1 + DEVICE_NAME_LEN; // 32

    pub fn encode_into(&self, buf: &mut [u8]) -> Result<(), EncodeError> {
        if buf.len() < Self::ENCODED_LEN {
            return Err(EncodeError::BufferTooSmall);
        }
        buf[..MAGIC_LEN].copy_from_slice(&MAGIC);
        buf[MAGIC_LEN] = self.kind.to_byte();
        buf[MAGIC_LEN + 1] = HEADER_PAD_BEFORE_NAME;
        buf[MAGIC_LEN + 2..MAGIC_LEN + 2 + DEVICE_NAME_LEN]
            .copy_from_slice(self.device_name.as_bytes());
        Ok(())
    }

    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        if buf.len() < Self::ENCODED_LEN {
            return Err(DecodeError::TooShort {
                need: Self::ENCODED_LEN,
                have: buf.len(),
            });
        }
        Magic::validate(buf)?;
        let kind = PacketKind::from_byte(buf[MAGIC_LEN]);
        let mut name = [0u8; DEVICE_NAME_LEN];
        name.copy_from_slice(&buf[MAGIC_LEN + 2..MAGIC_LEN + 2 + DEVICE_NAME_LEN]);
        Ok(Self {
            kind,
            device_name: DeviceName::from_bytes(&name),
        })
    }
}

/// Kind bytes of packets on ports 50001 and 50002. Several values mean
/// something else on port 50000 (0x02 is also claim stage 2), so these are
/// raw bytes rather than [`PacketKind`] variants.
pub mod update_kind {
    pub const FADER_START: u8 = 0x02;
    pub const CHANNELS_ON_AIR: u8 = 0x03;
    pub const MEDIA_QUERY: u8 = 0x05;
    pub const MEDIA_RESPONSE: u8 = 0x06;
    pub const CDJ_STATUS: u8 = 0x0a;
    pub const PRECISE_POSITION: u8 = 0x0b;
    pub const LOAD_TRACK: u8 = 0x19;
    pub const LOAD_TRACK_ACK: u8 = 0x1a;
    pub const MASTER_HANDOFF_REQUEST: u8 = 0x26;
    pub const MASTER_HANDOFF_RESPONSE: u8 = 0x27;
    pub const BEAT: u8 = 0x28;
    pub const MIXER_STATUS: u8 = 0x29;
    pub const SYNC_CONTROL: u8 = 0x2a;
}

/// Header shared by the packets on ports 50001 and 50002 (beat, CDJ status,
/// mixer status), matching beat-link's `DeviceUpdate` parser:
///
/// ```text
/// 0x00..0x0a  magic ("Qspt1WmJOL")
/// 0x0a        packet kind
/// 0x0b..0x1f  device name (20 bytes, NUL-padded)
/// 0x1f        0x01
/// 0x20        subtype
/// 0x21        device number  <- beat-link reads the sender's number here
/// 0x22..0x24  bytes remaining after this field (total - 0x24), u16 BE
/// ```
///
/// Unlike the port-50000 [`Header`], there is no `0x00` pad before the
/// device name, so the name starts one byte earlier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateHeader {
    /// One of the [`update_kind`] values.
    pub kind: u8,
    pub device_name: DeviceName,
    pub subtype: u8,
    pub device_number: u8,
}

impl UpdateHeader {
    pub const ENCODED_LEN: usize = 0x24;

    /// Write the header into the start of `packet`. `packet` must be the whole
    /// packet: the length field is derived from `packet.len()`.
    pub fn encode_into(&self, packet: &mut [u8]) -> Result<(), EncodeError> {
        if packet.len() < Self::ENCODED_LEN {
            return Err(EncodeError::BufferTooSmall);
        }
        let remaining = u16::try_from(packet.len() - Self::ENCODED_LEN)
            .map_err(|_| EncodeError::BufferTooSmall)?;
        packet[..MAGIC_LEN].copy_from_slice(&MAGIC);
        packet[0x0a] = self.kind;
        packet[0x0b..0x1f].copy_from_slice(self.device_name.as_bytes());
        packet[0x1f] = HEADER_PAD_AFTER_NAME;
        packet[0x20] = self.subtype;
        packet[0x21] = self.device_number;
        packet[0x22..0x24].copy_from_slice(&remaining.to_be_bytes());
        Ok(())
    }

    pub fn decode(packet: &[u8]) -> Result<Self, DecodeError> {
        if packet.len() < Self::ENCODED_LEN {
            return Err(DecodeError::TooShort {
                need: Self::ENCODED_LEN,
                have: packet.len(),
            });
        }
        Magic::validate(packet)?;
        let mut name = [0u8; DEVICE_NAME_LEN];
        name.copy_from_slice(&packet[0x0b..0x1f]);
        Ok(Self {
            kind: packet[0x0a],
            device_name: DeviceName::from_bytes(&name),
            subtype: packet[0x20],
            device_number: packet[0x21],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_matches_dysentery_spec() {
        assert_eq!(&MAGIC, b"Qspt1WmJOL");
        assert_eq!(
            MAGIC,
            [0x51, 0x73, 0x70, 0x74, 0x31, 0x57, 0x6d, 0x4a, 0x4f, 0x4c]
        );
    }

    #[test]
    fn header_roundtrip() {
        let h = Header {
            kind: PacketKind::KeepAlive,
            device_name: DeviceName::new("CDJ-3000").unwrap(),
        };
        let mut buf = [0u8; Header::ENCODED_LEN];
        h.encode_into(&mut buf).unwrap();
        let d = Header::decode(&buf).unwrap();
        assert_eq!(d.kind, PacketKind::KeepAlive);
        assert_eq!(d.device_name.as_str_lossy(), "CDJ-3000");
    }

    #[test]
    fn bad_magic_rejected() {
        let mut buf = [0u8; Header::ENCODED_LEN];
        buf[..10].copy_from_slice(b"NotProDJLk");
        assert!(matches!(Header::decode(&buf), Err(DecodeError::BadMagic { .. })));
    }

    #[test]
    fn update_header_matches_beatlink_device_update_layout() {
        let h = UpdateHeader {
            kind: update_kind::BEAT,
            device_name: DeviceName::new("CDJ-3000").unwrap(),
            subtype: 0x00,
            device_number: 3,
        };
        let mut buf = [0u8; 0x60];
        h.encode_into(&mut buf).unwrap();
        // beat-link DeviceUpdate: name = String(bytes, 0x0b, 20).trim(),
        // deviceNumber = bytes[0x21].
        assert_eq!(&buf[0x0b..0x13], b"CDJ-3000");
        assert_eq!(buf[0x1f], 0x01);
        assert_eq!(buf[0x21], 3);
        assert_eq!(&buf[0x22..0x24], &[0x00, 0x3c]);
        assert_eq!(UpdateHeader::decode(&buf).unwrap(), h);
    }

    #[test]
    fn device_name_too_long_rejected() {
        assert!(DeviceName::new("012345678901234567890").is_err());
    }
}
