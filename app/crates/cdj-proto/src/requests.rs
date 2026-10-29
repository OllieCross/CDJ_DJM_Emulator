//! Requests other devices send to a player's port 50002: what media is in a
//! slot, and "load this track".
//!
//! Layouts follow the templates beat-link's `VirtualCdj` sends to real
//! players (`MEDIA_QUERY_PAYLOAD`, `LOAD_TRACK_PAYLOAD`) and how its
//! `MediaDetails` parses a player's answer. All share the [`UpdateHeader`];
//! byte 0x21 is the sender's device number.
//!
//! ```text
//! kind  packet            len   payload
//! 0x05  media query       0x30  0x24..0x28 requester IP, 0x2b player, 0x2f slot
//! 0x06  media response    0xc0  0x27 player, 0x2b slot, 0x2c name (UTF-16BE, 0x40 B),
//!                               0x6c creation date (UTF-16BE, 0x18 B), 0xa6 track count,
//!                               0xa8 colour, 0xaa media type, 0xab "My Settings" saved,
//!                               0xae playlist count, 0xb0 total bytes, 0xb8 free bytes
//! 0x19  load track        0x58  0x24 sender, 0x28 source player, 0x29 slot, 0x2a track
//!                               type, 0x2c..0x30 rekordbox ID, 0x33 0x32, 0x40 target - 1
//! 0x1a  load track ack    0x28  0x27 sender (layout not documented; beat-link only
//!                               reads 0x21)
//! ```
//!
//! A player answers a media query by unicasting the response to port 50002
//! of the address in the query.

use std::net::Ipv4Addr;

use crate::control::{decode_header, header};
use crate::error::DecodeError;
use crate::header::{update_kind, DeviceName};
use crate::status::{TrackSourceSlot, TrackType};

// ---------- media query ----------

pub const MEDIA_QUERY_LEN: usize = 0x30;

/// "What is in player `player`'s `slot`?"
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaQuery {
    pub device_name: DeviceName,
    pub device_number: u8,
    /// Where the answer goes.
    pub requester: Ipv4Addr,
    pub player: u8,
    pub slot: TrackSourceSlot,
}

impl MediaQuery {
    pub fn encode(&self) -> [u8; MEDIA_QUERY_LEN] {
        let mut buf = [0u8; MEDIA_QUERY_LEN];
        header(update_kind::MEDIA_QUERY, &self.device_name, 0x00, self.device_number)
            .encode_into(&mut buf)
            .expect("buffer is always large enough");
        buf[0x24..0x28].copy_from_slice(&self.requester.octets());
        buf[0x2b] = self.player;
        buf[0x2f] = self.slot.to_byte();
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        let h = decode_header(buf, update_kind::MEDIA_QUERY, MEDIA_QUERY_LEN)?;
        Ok(Self {
            device_name: h.device_name,
            device_number: h.device_number,
            requester: Ipv4Addr::new(buf[0x24], buf[0x25], buf[0x26], buf[0x27]),
            player: buf[0x2b],
            slot: TrackSourceSlot::from_byte(buf[0x2f]),
        })
    }
}

// ---------- media response ----------

pub const MEDIA_RESPONSE_LEN: usize = 0xc0;
const MEDIA_NAME_BYTES: usize = 0x40;
const MEDIA_DATE_BYTES: usize = 0x18;

/// A player's answer to a [`MediaQuery`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaResponse {
    pub device_name: DeviceName,
    pub device_number: u8,
    pub player: u8,
    pub slot: TrackSourceSlot,
    /// Truncated to 31 characters.
    pub name: String,
    /// As rekordbox writes it, e.g. `2024-05-01`. Truncated to 11 characters.
    pub creation_date: String,
    pub track_count: u16,
    /// rekordbox media colour, 0 = none.
    pub color: u8,
    /// [`TrackType::Rekordbox`] when the media holds a rekordbox export.
    pub media_type: TrackType,
    pub has_my_settings: bool,
    pub playlist_count: u16,
    pub total_bytes: u64,
    pub free_bytes: u64,
}

/// UTF-16BE into `field`, always leaving room for a NUL terminator.
fn put_utf16(field: &mut [u8], s: &str) {
    let units = s.encode_utf16().take(field.len() / 2 - 1);
    for (i, u) in units.enumerate() {
        field[i * 2..i * 2 + 2].copy_from_slice(&u.to_be_bytes());
    }
}

fn get_utf16(field: &[u8]) -> String {
    let units: Vec<u16> = field
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

impl MediaResponse {
    pub fn encode(&self) -> [u8; MEDIA_RESPONSE_LEN] {
        let mut buf = [0u8; MEDIA_RESPONSE_LEN];
        header(update_kind::MEDIA_RESPONSE, &self.device_name, 0x00, self.device_number)
            .encode_into(&mut buf)
            .expect("buffer is always large enough");
        buf[0x27] = self.player;
        buf[0x2b] = self.slot.to_byte();
        put_utf16(&mut buf[0x2c..0x2c + MEDIA_NAME_BYTES], &self.name);
        put_utf16(&mut buf[0x6c..0x6c + MEDIA_DATE_BYTES], &self.creation_date);
        buf[0xa6..0xa8].copy_from_slice(&self.track_count.to_be_bytes());
        buf[0xa8] = self.color;
        buf[0xaa] = self.media_type.to_byte();
        buf[0xab] = self.has_my_settings as u8;
        buf[0xae..0xb0].copy_from_slice(&self.playlist_count.to_be_bytes());
        buf[0xb0..0xb8].copy_from_slice(&self.total_bytes.to_be_bytes());
        buf[0xb8..0xc0].copy_from_slice(&self.free_bytes.to_be_bytes());
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        let h = decode_header(buf, update_kind::MEDIA_RESPONSE, MEDIA_RESPONSE_LEN)?;
        let u16_at = |o: usize| u16::from_be_bytes([buf[o], buf[o + 1]]);
        let u64_at = |o: usize| u64::from_be_bytes(buf[o..o + 8].try_into().unwrap());
        Ok(Self {
            device_name: h.device_name,
            device_number: h.device_number,
            player: buf[0x27],
            slot: TrackSourceSlot::from_byte(buf[0x2b]),
            name: get_utf16(&buf[0x2c..0x2c + MEDIA_NAME_BYTES]),
            creation_date: get_utf16(&buf[0x6c..0x6c + MEDIA_DATE_BYTES]),
            track_count: u16_at(0xa6),
            color: buf[0xa8],
            media_type: TrackType::from_byte(buf[0xaa]),
            has_my_settings: buf[0xab] != 0,
            playlist_count: u16_at(0xae),
            total_bytes: u64_at(0xb0),
            free_bytes: u64_at(0xb8),
        })
    }
}

// ---------- load track ----------

pub const LOAD_TRACK_LEN: usize = 0x58;

/// "Load rekordbox track `rekordbox_id` from `source_player`'s `source_slot`."
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadTrack {
    pub device_name: DeviceName,
    pub device_number: u8,
    pub source_player: u8,
    pub source_slot: TrackSourceSlot,
    pub track_type: TrackType,
    pub rekordbox_id: u32,
    /// The player that should load it (sent zero-based).
    pub target_player: u8,
}

impl LoadTrack {
    pub fn encode(&self) -> [u8; LOAD_TRACK_LEN] {
        let mut buf = [0u8; LOAD_TRACK_LEN];
        header(update_kind::LOAD_TRACK, &self.device_name, 0x00, self.device_number)
            .encode_into(&mut buf)
            .expect("buffer is always large enough");
        buf[0x24] = self.device_number;
        buf[0x28] = self.source_player;
        buf[0x29] = self.source_slot.to_byte();
        buf[0x2a] = self.track_type.to_byte();
        buf[0x2c..0x30].copy_from_slice(&self.rekordbox_id.to_be_bytes());
        buf[0x33] = 0x32;
        buf[0x40] = self.target_player.wrapping_sub(1);
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        let h = decode_header(buf, update_kind::LOAD_TRACK, LOAD_TRACK_LEN)?;
        Ok(Self {
            device_name: h.device_name,
            device_number: h.device_number,
            source_player: buf[0x28],
            source_slot: TrackSourceSlot::from_byte(buf[0x29]),
            track_type: TrackType::from_byte(buf[0x2a]),
            rekordbox_id: u32::from_be_bytes(buf[0x2c..0x30].try_into().unwrap()),
            target_player: buf[0x40].wrapping_add(1),
        })
    }
}

pub const LOAD_TRACK_ACK_LEN: usize = 0x28;

/// A player confirming a [`LoadTrack`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadTrackAck {
    pub device_name: DeviceName,
    pub device_number: u8,
}

impl LoadTrackAck {
    pub fn encode(&self) -> [u8; LOAD_TRACK_ACK_LEN] {
        let mut buf = [0u8; LOAD_TRACK_ACK_LEN];
        header(update_kind::LOAD_TRACK_ACK, &self.device_name, 0x00, self.device_number)
            .encode_into(&mut buf)
            .expect("buffer is always large enough");
        buf[0x27] = self.device_number;
        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blt() -> DeviceName {
        DeviceName::new("beat-link").unwrap()
    }

    /// beat-link's template (`VirtualCdj.*_PAYLOAD`, from byte 0x1f) with the
    /// fields its send method fills in.
    fn assert_payload(buf: &[u8], template: &[u8], filled: &[(usize, u8)]) {
        assert_eq!(buf.len(), 0x1f + template.len());
        for (i, &t) in template.iter().enumerate() {
            let off = 0x1f + i;
            let want = filled.iter().find(|(o, _)| *o == off).map_or(t, |(_, v)| *v);
            assert_eq!(buf[off], want, "byte {off:#04x}");
        }
    }

    #[test]
    fn media_query_matches_beatlink_send_media_query() {
        let q = MediaQuery {
            device_name: blt(),
            device_number: 5,
            requester: Ipv4Addr::new(10, 77, 77, 200),
            player: 2,
            slot: TrackSourceSlot::Usb,
        };
        let b = q.encode();
        assert_eq!(b[0x0a], 0x05);
        // MEDIA_QUERY_PAYLOAD; sendMediaQuery fills [2] device, [5..9] our
        // IP (from keepAliveBytes 44..48), [12] player, [16] slot.
        let template = [1, 0, 0x0d, 0, 0x0c, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_payload(
            &b,
            &template,
            &[(0x21, 5), (0x24, 10), (0x25, 77), (0x26, 77), (0x27, 200), (0x2b, 2), (0x2f, 3)],
        );
        assert_eq!(MediaQuery::decode(&b).unwrap(), q);
    }

    #[test]
    fn media_response_round_trips_at_beatlink_offsets() {
        let r = MediaResponse {
            device_name: DeviceName::new("CDJ-3000").unwrap(),
            device_number: 2,
            player: 2,
            slot: TrackSourceSlot::Sd,
            name: "GIG USB".into(),
            creation_date: "2024-05-01".into(),
            track_count: 312,
            color: 3,
            media_type: TrackType::Rekordbox,
            has_my_settings: false,
            playlist_count: 7,
            total_bytes: 64_000_000_000,
            free_bytes: 12_345_678,
        };
        let b = r.encode();
        assert_eq!(b.len(), 192, "MediaDetails.MINIMUM_PACKET_SIZE");
        assert_eq!(&b[0x22..0x24], &[0x00, 0x9c], "lenr");
        assert_eq!((b[0x27], b[0x2b], b[0xaa]), (2, 2, 1));
        assert_eq!(&b[0x2c..0x30], &[0, b'G', 0, b'I'], "UTF-16BE name");
        assert_eq!(&b[0xa6..0xa8], &312u16.to_be_bytes());
        assert_eq!(MediaResponse::decode(&b).unwrap(), r);
    }

    #[test]
    fn media_strings_are_truncated_with_a_terminator() {
        let r = MediaResponse {
            device_name: DeviceName::new("CDJ-3000").unwrap(),
            device_number: 1,
            player: 1,
            slot: TrackSourceSlot::Usb,
            name: "N".repeat(40),
            creation_date: "2024-05-01T12:00".into(),
            track_count: 0,
            color: 0,
            media_type: TrackType::Rekordbox,
            has_my_settings: false,
            playlist_count: 0,
            total_bytes: 0,
            free_bytes: 0,
        };
        let d = MediaResponse::decode(&r.encode()).unwrap();
        assert_eq!(d.name.len(), 31);
        assert_eq!(d.creation_date, "2024-05-01T");
    }

    #[test]
    fn load_track_matches_beatlink_send_load_track_command() {
        let l = LoadTrack {
            device_name: blt(),
            device_number: 5,
            source_player: 1,
            source_slot: TrackSourceSlot::Usb,
            track_type: TrackType::Rekordbox,
            rekordbox_id: 0x0102_0304,
            target_player: 3,
        };
        let b = l.encode();
        assert_eq!(b[0x0a], 0x19);
        // LOAD_TRACK_PAYLOAD; sendLoadTrackCommand fills [2] and [5] our
        // number, [9] source player, [10] slot, [11] type, [13..17] the ID,
        // [33] target - 1.
        let mut template = [0u8; 57];
        template[..6].copy_from_slice(&[1, 0, 0x0d, 0, 0x34, 0x0d]);
        template[20] = 0x32;
        assert_payload(
            &b,
            &template,
            &[
                (0x21, 5),
                (0x24, 5),
                (0x28, 1),
                (0x29, 3),
                (0x2a, 1),
                (0x2c, 1),
                (0x2d, 2),
                (0x2e, 3),
                (0x2f, 4),
                (0x40, 2),
            ],
        );
        assert_eq!(LoadTrack::decode(&b).unwrap(), l);
    }

    #[test]
    fn load_track_ack_names_the_player() {
        let b = LoadTrackAck {
            device_name: DeviceName::new("CDJ-3000").unwrap(),
            device_number: 3,
        }
        .encode();
        assert_eq!((b[0x0a], b[0x21], b.len()), (0x1a, 3, 0x28));
    }
}
