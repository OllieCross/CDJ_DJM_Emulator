//! Dbserver protocol (TCP 1051) message framing.
//!
//! The dbserver is Pro DJ Link's track-metadata service. Real CDJs run one on
//! a dynamically-chosen TCP port (discovered via UDP 12523) and respond to
//! metadata queries from other devices on the network. ShowKontrol uses this
//! to fetch the track title / artist / BPM of whatever is loaded on each deck.
//!
//! This module is the wire codec: [`Message`] framing and [`Field`] encoding.
//! Server behaviour (accept, handshake, synthetic track data) lives in
//! `cdj_core::dbserver`.
//!
//! Wire format ([Deep-Symmetry beat-link reference][ref]):
//!
//! ```text
//! [magic 0x872349ae] [txid u32 BE] [type u16 BE] [argc u8] [type-tags 12B] [payload...]
//! ```
//!
//! `type-tags` is a fixed 12-byte blob: one byte per possible argument slot
//! (0-indexed), where `0x06` = NumberField, `0x02` = StringField,
//! `0x03` = BinaryField, `0x00` = unused slot.
//!
//! Each field in the payload carries its own 1-byte type marker (`0x0f`,
//! `0x10`, `0x11` for 1/2/4-byte numbers; `0x26` for UTF-16BE strings;
//! `0x14` for binary blobs).
//!
//! [ref]: https://djl-analysis.deepsymmetry.org/djl-analysis/dbserver.html

use crate::error::DecodeError;

/// Magic at the start of every dbserver message.
pub const MAGIC: [u8; 4] = [0x87, 0x23, 0x49, 0xae];

/// Special transaction-id used for setup / teardown messages.
pub const SETUP_TXID: u32 = 0xffff_fffe;

/// The 5-byte connection greeting both sides exchange immediately after the
/// TCP connection opens. It is a bare 4-byte NumberField (no message header).
pub const GREETING: [u8; 5] = [0x11, 0x00, 0x00, 0x00, 0x01];

// Message types (opcodes).
pub const MSG_SETUP_REQ: u16 = 0x0000;
pub const MSG_TEARDOWN_REQ: u16 = 0x0100;
pub const MSG_ROOT_MENU_REQ: u16 = 0x1000;
pub const MSG_TRACKS_MENU_REQ: u16 = 0x1100;
pub const MSG_REKORDBOX_METADATA_REQ: u16 = 0x2002;
pub const MSG_ARTWORK_REQ: u16 = 0x2003;
pub const MSG_WAVEFORM_PREVIEW_REQ: u16 = 0x2004;
pub const MSG_BEAT_GRID_REQ: u16 = 0x2204;
pub const MSG_RENDER_MENU_REQ: u16 = 0x3000;
pub const MSG_MENU_AVAILABLE: u16 = 0x4000;
pub const MSG_MENU_HEADER: u16 = 0x4001;
pub const MSG_MENU_ITEM: u16 = 0x4101;
pub const MSG_MENU_FOOTER: u16 = 0x4201;

// Menu-item type bytes (argument 7 in a MENU_ITEM message).
pub const ITEM_ALBUM: u8 = 0x02;
pub const ITEM_TITLE: u8 = 0x04;
pub const ITEM_GENRE: u8 = 0x06;
pub const ITEM_ARTIST: u8 = 0x07;
pub const ITEM_RATING: u8 = 0x0a;
pub const ITEM_DURATION: u8 = 0x0b;
pub const ITEM_TEMPO: u8 = 0x0d;
pub const ITEM_KEY: u8 = 0x0f;
pub const ITEM_COMMENT: u8 = 0x23;
pub const ITEM_DATE_ADDED: u8 = 0x2e;

// Field type markers (first byte of each field payload).
const TAG_NUM1: u8 = 0x0f;
const TAG_NUM2: u8 = 0x10;
const TAG_NUM4: u8 = 0x11;
const TAG_BINARY: u8 = 0x14;
const TAG_STRING: u8 = 0x26;

// Message-level argument type bytes (indices 11..23 in the header).
const ARG_TAG_NUMBER: u8 = 0x06;
const ARG_TAG_STRING: u8 = 0x02;
const ARG_TAG_BINARY: u8 = 0x03;

/// Size of the fixed-length argument type-tag blob in the message header.
pub const TYPE_TAG_BLOB_LEN: usize = 12;

/// Bytes from `magic` through the end of the type-tag blob.
pub const HEADER_LEN: usize = 4 + 4 + 2 + 1 + TYPE_TAG_BLOB_LEN;

/// A single typed value carried as a dbserver message argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Field {
    Number1(u8),
    Number2(u16),
    Number4(u32),
    /// Stored as a Rust `String`; emitted as UTF-16BE with a NUL terminator.
    String(String),
    Binary(Vec<u8>),
}

impl Field {
    /// The byte that identifies this field's kind in the message-level
    /// 12-byte type-tag blob.
    fn arg_tag(&self) -> u8 {
        match self {
            Field::Number1(_) | Field::Number2(_) | Field::Number4(_) => ARG_TAG_NUMBER,
            Field::String(_) => ARG_TAG_STRING,
            Field::Binary(_) => ARG_TAG_BINARY,
        }
    }

    fn encode_into(&self, buf: &mut Vec<u8>) {
        match self {
            Field::Number1(v) => {
                buf.push(TAG_NUM1);
                buf.push(*v);
            }
            Field::Number2(v) => {
                buf.push(TAG_NUM2);
                buf.extend_from_slice(&v.to_be_bytes());
            }
            Field::Number4(v) => {
                buf.push(TAG_NUM4);
                buf.extend_from_slice(&v.to_be_bytes());
            }
            Field::String(s) => {
                buf.push(TAG_STRING);
                let units: Vec<u16> = s.encode_utf16().collect();
                // Character count is UTF-16 code units including the NUL.
                let count = (units.len() + 1) as u32;
                buf.extend_from_slice(&count.to_be_bytes());
                for u in units {
                    buf.extend_from_slice(&u.to_be_bytes());
                }
                buf.extend_from_slice(&[0, 0]); // NUL terminator
            }
            Field::Binary(data) => {
                buf.push(TAG_BINARY);
                buf.extend_from_slice(&(data.len() as u32).to_be_bytes());
                buf.extend_from_slice(data);
            }
        }
    }

    /// Decode a single field starting at `buf[offset]`. Returns the field
    /// and the number of bytes consumed.
    fn decode(buf: &[u8], offset: usize) -> Result<(Self, usize), DecodeError> {
        if offset >= buf.len() {
            return Err(DecodeError::TooShort {
                need: 1,
                have: 0,
            });
        }
        let tag = buf[offset];
        match tag {
            TAG_NUM1 => {
                if buf.len() < offset + 2 {
                    return Err(DecodeError::TooShort {
                        need: 2,
                        have: buf.len() - offset,
                    });
                }
                Ok((Field::Number1(buf[offset + 1]), 2))
            }
            TAG_NUM2 => {
                if buf.len() < offset + 3 {
                    return Err(DecodeError::TooShort {
                        need: 3,
                        have: buf.len() - offset,
                    });
                }
                let v = u16::from_be_bytes([buf[offset + 1], buf[offset + 2]]);
                Ok((Field::Number2(v), 3))
            }
            TAG_NUM4 => {
                if buf.len() < offset + 5 {
                    return Err(DecodeError::TooShort {
                        need: 5,
                        have: buf.len() - offset,
                    });
                }
                let mut b = [0u8; 4];
                b.copy_from_slice(&buf[offset + 1..offset + 5]);
                Ok((Field::Number4(u32::from_be_bytes(b)), 5))
            }
            TAG_STRING => {
                if buf.len() < offset + 5 {
                    return Err(DecodeError::TooShort {
                        need: 5,
                        have: buf.len() - offset,
                    });
                }
                let mut b = [0u8; 4];
                b.copy_from_slice(&buf[offset + 1..offset + 5]);
                let count = u32::from_be_bytes(b) as usize;
                let byte_len = count * 2;
                if buf.len() < offset + 5 + byte_len {
                    return Err(DecodeError::TooShort {
                        need: 5 + byte_len,
                        have: buf.len() - offset,
                    });
                }
                let mut units = Vec::with_capacity(count);
                for i in 0..count {
                    let hi = buf[offset + 5 + i * 2];
                    let lo = buf[offset + 5 + i * 2 + 1];
                    units.push(u16::from_be_bytes([hi, lo]));
                }
                // Drop trailing NUL, if present.
                if units.last() == Some(&0) {
                    units.pop();
                }
                let s = String::from_utf16_lossy(&units);
                Ok((Field::String(s), 5 + byte_len))
            }
            TAG_BINARY => {
                if buf.len() < offset + 5 {
                    return Err(DecodeError::TooShort {
                        need: 5,
                        have: buf.len() - offset,
                    });
                }
                let mut b = [0u8; 4];
                b.copy_from_slice(&buf[offset + 1..offset + 5]);
                let len = u32::from_be_bytes(b) as usize;
                if buf.len() < offset + 5 + len {
                    return Err(DecodeError::TooShort {
                        need: 5 + len,
                        have: buf.len() - offset,
                    });
                }
                let data = buf[offset + 5..offset + 5 + len].to_vec();
                Ok((Field::Binary(data), 5 + len))
            }
            other => Err(DecodeError::UnknownKind(other)),
        }
    }
}

/// A full dbserver message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub transaction_id: u32,
    pub message_type: u16,
    pub arguments: Vec<Field>,
}

impl Message {
    pub fn new(transaction_id: u32, message_type: u16, arguments: Vec<Field>) -> Self {
        Self {
            transaction_id,
            message_type,
            arguments,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        assert!(
            self.arguments.len() <= TYPE_TAG_BLOB_LEN,
            "at most 12 arguments per message"
        );
        let mut buf = Vec::with_capacity(64);
        buf.extend_from_slice(&MAGIC);
        buf.extend_from_slice(&self.transaction_id.to_be_bytes());
        buf.extend_from_slice(&self.message_type.to_be_bytes());
        buf.push(self.arguments.len() as u8);

        let mut tags = [0u8; TYPE_TAG_BLOB_LEN];
        for (i, arg) in self.arguments.iter().enumerate() {
            tags[i] = arg.arg_tag();
        }
        buf.extend_from_slice(&tags);

        for arg in &self.arguments {
            arg.encode_into(&mut buf);
        }
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<(Self, usize), DecodeError> {
        if buf.len() < HEADER_LEN {
            return Err(DecodeError::TooShort {
                need: HEADER_LEN,
                have: buf.len(),
            });
        }
        if buf[0..4] != MAGIC {
            let mut got = [0u8; 10];
            got[..4].copy_from_slice(&buf[0..4]);
            return Err(DecodeError::BadMagic { got });
        }
        let transaction_id = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let message_type = u16::from_be_bytes([buf[8], buf[9]]);
        let argc = buf[10] as usize;
        // buf[11..23] is the type-tag blob; we ignore it for decoding because
        // each field carries its own inline type tag.

        let mut offset = HEADER_LEN;
        let mut arguments = Vec::with_capacity(argc);
        for _ in 0..argc {
            let (field, consumed) = Field::decode(buf, offset)?;
            arguments.push(field);
            offset += consumed;
        }
        Ok((
            Self {
                transaction_id,
                message_type,
                arguments,
            },
            offset,
        ))
    }
}

/// The UDP port 12523 discovery exchange.
///
/// Clients send a 19-byte packet: 4-byte BE length (0x0000000f) + ASCII
/// "RemoteDBServer" + NUL. The server replies with a 2-byte BE port number.
pub mod port_discovery {
    /// Packet the client sends to UDP 12523. Constant on the wire.
    pub const QUERY: &[u8] = b"\x00\x00\x00\x0fRemoteDBServer\0";

    /// Check if a UDP payload matches the `RemoteDBServer` discovery query.
    pub fn is_query(buf: &[u8]) -> bool {
        buf == QUERY
    }

    /// Encode a 2-byte big-endian reply carrying the TCP port.
    pub fn reply(port: u16) -> [u8; 2] {
        port.to_be_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greeting_is_number_field_one() {
        // 5-byte payload: 0x11 + 4-byte BE 1.
        assert_eq!(GREETING, [0x11, 0x00, 0x00, 0x00, 0x01]);
    }

    #[test]
    fn magic_bytes_match_spec() {
        assert_eq!(MAGIC, [0x87, 0x23, 0x49, 0xae]);
    }

    #[test]
    fn number4_field_roundtrip() {
        let f = Field::Number4(12_000);
        let mut buf = Vec::new();
        f.encode_into(&mut buf);
        assert_eq!(buf, [TAG_NUM4, 0x00, 0x00, 0x2e, 0xe0]);
        let (decoded, n) = Field::decode(&buf, 0).unwrap();
        assert_eq!(decoded, f);
        assert_eq!(n, buf.len());
    }

    #[test]
    fn number1_and_number2_roundtrip() {
        for f in [Field::Number1(0x04), Field::Number2(0x0102)] {
            let mut buf = Vec::new();
            f.encode_into(&mut buf);
            let (d, n) = Field::decode(&buf, 0).unwrap();
            assert_eq!(d, f);
            assert_eq!(n, buf.len());
        }
    }

    #[test]
    fn string_field_matches_spec_example() {
        // "Foo" in UTF-16BE with NUL, char count 4.
        let f = Field::String("Foo".to_string());
        let mut buf = Vec::new();
        f.encode_into(&mut buf);
        assert_eq!(
            buf,
            [
                TAG_STRING, 0x00, 0x00, 0x00, 0x04, // tag + count=4
                0x00, 0x46, 0x00, 0x6f, 0x00, 0x6f, 0x00, 0x00, // F o o NUL
            ]
        );
        let (d, _) = Field::decode(&buf, 0).unwrap();
        assert_eq!(d, f);
    }

    #[test]
    fn empty_string_roundtrip() {
        let f = Field::String(String::new());
        let mut buf = Vec::new();
        f.encode_into(&mut buf);
        // Count = 1 (just the NUL).
        assert_eq!(buf, [TAG_STRING, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00]);
        let (d, _) = Field::decode(&buf, 0).unwrap();
        assert_eq!(d, f);
    }

    #[test]
    fn binary_field_roundtrip() {
        let f = Field::Binary(vec![0xaa, 0xbb, 0xcc, 0xdd]);
        let mut buf = Vec::new();
        f.encode_into(&mut buf);
        assert_eq!(
            buf,
            [TAG_BINARY, 0x00, 0x00, 0x00, 0x04, 0xaa, 0xbb, 0xcc, 0xdd]
        );
        let (d, _) = Field::decode(&buf, 0).unwrap();
        assert_eq!(d, f);
    }

    #[test]
    fn message_header_layout_matches_spec() {
        let m = Message::new(1, MSG_SETUP_REQ, vec![Field::Number4(1)]);
        let bytes = m.encode();
        assert_eq!(&bytes[0..4], &MAGIC);
        assert_eq!(&bytes[4..8], &1u32.to_be_bytes());
        assert_eq!(&bytes[8..10], &MSG_SETUP_REQ.to_be_bytes());
        assert_eq!(bytes[10], 1); // argc
        // First type-tag slot = 0x06 (number), rest zero.
        assert_eq!(bytes[11], ARG_TAG_NUMBER);
        assert_eq!(&bytes[12..23], &[0u8; 11]);
        // Payload: a 4-byte number field = 5 bytes.
        assert_eq!(&bytes[23..28], &[TAG_NUM4, 0x00, 0x00, 0x00, 0x01]);
        assert_eq!(bytes.len(), HEADER_LEN + 5);
    }

    #[test]
    fn message_roundtrip_mixed_args() {
        let m = Message::new(
            42,
            MSG_MENU_ITEM,
            vec![
                Field::Number4(0x00000045),
                Field::String("Hi".to_string()),
                Field::Number1(ITEM_TITLE),
                Field::Binary(vec![0x01, 0x02]),
            ],
        );
        let bytes = m.encode();
        let (decoded, consumed) = Message::decode(&bytes).unwrap();
        assert_eq!(decoded, m);
        assert_eq!(consumed, bytes.len());
        // Check tag-blob layout: number, string, number, binary.
        assert_eq!(
            &bytes[11..15],
            &[ARG_TAG_NUMBER, ARG_TAG_STRING, ARG_TAG_NUMBER, ARG_TAG_BINARY]
        );
    }

    #[test]
    fn port_discovery_query_constant() {
        assert_eq!(port_discovery::QUERY.len(), 19);
        assert_eq!(&port_discovery::QUERY[0..4], &[0x00, 0x00, 0x00, 0x0f]);
        assert_eq!(&port_discovery::QUERY[4..18], b"RemoteDBServer");
        assert_eq!(port_discovery::QUERY[18], 0x00);
    }

    #[test]
    fn port_discovery_reply_is_two_bytes_be() {
        assert_eq!(port_discovery::reply(1051), [0x04, 0x1b]);
    }
}
