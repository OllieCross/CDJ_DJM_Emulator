//! Status packets on port 50002.
//!
//! Two distinct packets share this port:
//!
//! * **CDJ status** (kind 0x0a) - each player sends its current state
//!   (track, beat, bpm, play/pause, on-air, master status) roughly every
//!   200 ms.
//! * **DJM status** (kind 0x29) - the mixer's tempo-master state, BPM,
//!   master handoff and beat within bar. Fader positions are not in this
//!   packet, and on-air state travels in a separate channels-on-air packet.

use crate::error::DecodeError;
use crate::header::{update_kind, DeviceName, UpdateHeader, PITCH_NEUTRAL};

/// CDJ-3000 status packets are 0x200 bytes (dysentery); older players send
/// 0xd0 / 0xd4 / 0x11c.
pub const CDJ_STATUS_LEN: usize = 0x200;
pub const DJM_STATUS_LEN: usize = 0x38; // 56 bytes

/// BPM is transmitted in hundredths: 120.00 -> 12000 -> 0x2EE0.
fn encode_bpm_u16(bpm_hundredths: u16) -> [u8; 2] {
    bpm_hundredths.to_be_bytes()
}

fn u16_at(buf: &[u8], off: usize) -> u16 {
    u16::from_be_bytes([buf[off], buf[off + 1]])
}

fn u32_at(buf: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

// ---------- CDJ status ----------
//
// Layout follows beat-link's `CdjStatus` parser and the status template its
// own `VirtualCdj` sends to real players (`STATUS_PAYLOAD`), extended to the
// CDJ-3000 length:
//
//   0x00..0x24  UpdateHeader: kind 0x0a, subtype 0x04, length-remaining
//   0x24        device number (copy)
//   0x27        activity: 1 while playing / busy
//   0x28        track source player     0x29 source slot   0x2a track type
//   0x2c..0x30  rekordbox ID            0x32..0x34 track number
//   0x6f        USB slot state (0 loaded, 4 empty)
//   0x73        SD slot state  (0 loaded, 4 empty)
//   0x75        1 when link media is available
//   0x7b        PlayState1
//   0x7c..0x80  firmware version, ASCII
//   0x84..0x88  sync number
//   0x89        status flags: 0x84 | playing 0x40 | master 0x20 | synced 0x10 | on-air 0x08
//   0x8b        PlayState2 (0x7a moving, 0x7e stopped)
//   0x8c..0x90  pitch (fader); copies at 0x98, 0xc0, 0xc4
//   0x92..0x94  bpm_hundredths (0xffff = no track)
//   0x9d        PlayState3
//   0x9e        1 when tempo master
//   0x9f        master handoff target (0xff = none)
//   0xa0..0xa4  beat number (0xffffffff = unknown)
//   0xa4..0xa6  beats until next memory cue (0x01ff = none)
//   0xa6        beat within bar
//   0xc8..0xcc  packet counter
//   0x1b6..0x1ba loop start, 0x1be..0x1c2 loop end (ms * 1000 / 65536)
//   0x1c8..0x1ca loop length in beats
//
// Bytes not listed carry the constants from beat-link's template; their
// meaning is not documented.

pub const CDJ_STATUS_SUBTYPE: u8 = 0x04;

/// Firmware version CDJ-3000 status packets advertise.
pub const CDJ3000_FIRMWARE: [u8; 4] = *b"3.22";

const NO_BEAT_NUMBER: u32 = 0xffff_ffff;
const NO_CUE_COUNTDOWN: u16 = 0x01ff;
const NO_BPM: u16 = 0xffff;
const NO_MASTER_HANDOFF: u8 = 0xff;
const STATUS_FLAGS_BASE: u8 = 0x84;
const SLOT_LOADED: u8 = 0x00;
const SLOT_EMPTY: u8 = 0x04;

/// Constants from beat-link's `VirtualCdj.STATUS_PAYLOAD`, as (offset, value).
const TEMPLATE_CONSTANTS: &[(usize, u8)] = &[
    (0x26, 0x01),
    (0x3a, 0xa0),
    (0x68, 0x01),
    (0x6a, 0x04),
    (0x6b, 0x04),
    (0x8a, 0xff),
    (0x90, 0x80),
    (0x94, 0x7f),
    (0x95, 0xff),
    (0x96, 0xff),
    (0x97, 0xff),
    (0xb6, 0x01),
    (0xcc, 0x0f),
    (0xcd, 0x01),
    (0xd0, 0x12),
    (0xd1, 0x34),
    (0xd2, 0x56),
    (0xd3, 0x78),
    (0xd7, 0x01),
    (0xd8, 0x01),
    (0xd9, 0x01),
    (0xda, 0x01),
    (0xdb, 0x01),
    (0x113, 0x15),
    (0x116, 0x07),
    (0x117, 0x61),
    (0x11a, 0x06),
    (0x11b, 0x2f),
];

macro_rules! byte_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident = $value:expr),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name {
            $($(#[$vmeta])* $variant),+,
            Unknown(u8),
        }

        impl $name {
            pub fn from_byte(b: u8) -> Self {
                match b {
                    $($value => Self::$variant,)+
                    other => Self::Unknown(other),
                }
            }

            pub fn to_byte(self) -> u8 {
                match self {
                    $(Self::$variant => $value,)+
                    Self::Unknown(b) => b,
                }
            }
        }
    };
}

byte_enum!(
    /// Byte 0x7b, values from beat-link `CdjStatus.PlayState1`.
    PlayState1 {
        NoTrack = 0x00,
        Loading = 0x02,
        Playing = 0x03,
        Looping = 0x04,
        Paused = 0x05,
        Cued = 0x06,
        CuePlaying = 0x07,
        CueScratching = 0x08,
        Searching = 0x09,
        Ended = 0x11,
    }
);

byte_enum!(
    /// Byte 0x9d, values from beat-link `CdjStatus.PlayState3`.
    PlayState3 {
        NoTrack = 0x00,
        PausedOrReverse = 0x01,
        ForwardVinyl = 0x09,
        ForwardCdj = 0x0d,
    }
);

byte_enum!(
    /// Byte 0x29, values from beat-link `CdjStatus.TrackSourceSlot`.
    TrackSourceSlot {
        NoTrack = 0x00,
        Cd = 0x01,
        Sd = 0x02,
        Usb = 0x03,
        Collection = 0x04,
    }
);

byte_enum!(
    /// Byte 0x2a, values from beat-link `CdjStatus.TrackType`.
    TrackType {
        NoTrack = 0x00,
        Rekordbox = 0x01,
        Unanalyzed = 0x02,
        CdDigitalAudio = 0x05,
    }
);

/// The loop a CDJ-3000 is currently playing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveLoop {
    pub start_ms: u32,
    pub end_ms: u32,
    /// Loop length in beats; 0 for a manual loop that is not beat-sized.
    pub beats: u16,
}

/// beat-link reads loop times as `raw * 65536 / 1000` milliseconds.
fn loop_ms_to_raw(ms: u32) -> u32 {
    ((u64::from(ms) * 1000 + 32_768) / 65_536) as u32
}

fn loop_raw_to_ms(raw: u32) -> u32 {
    (u64::from(raw) * 65_536 / 1000) as u32
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdjStatus {
    pub device_name: DeviceName,
    pub device_number: u8,
    /// Player is busy (playing or loading).
    pub active: bool,
    /// Player whose media the loaded track came from (clients query that
    /// player's dbserver), plus slot and track type.
    pub track_source_player: u8,
    pub track_source_slot: TrackSourceSlot,
    pub track_type: TrackType,
    /// Rekordbox ID of the loaded track; 0 when nothing is loaded.
    pub rekordbox_id: u32,
    /// Position of the track in the list it was loaded from (1-based).
    pub track_number: u16,
    pub usb_loaded: bool,
    pub sd_loaded: bool,
    pub link_media_available: bool,
    pub play_state1: PlayState1,
    /// PlayState2: true while the playhead is moving.
    pub moving: bool,
    pub play_state3: PlayState3,
    pub firmware: [u8; 4],
    pub sync_number: u32,
    pub playing: bool,
    /// True if this device has been chosen as tempo master.
    pub master: bool,
    pub synced: bool,
    /// True if the mixer has this channel on-air.
    pub on_air: bool,
    /// Pitch fader, `PITCH_NEUTRAL` = +0%.
    pub pitch: u32,
    /// Track BPM in hundredths; `None` when no track is loaded.
    pub bpm_hundredths: Option<u16>,
    /// Device this player is handing tempo master to.
    pub master_handoff_to: Option<u8>,
    /// Absolute beat number within the track (1-based); `None` when unknown.
    pub beat_number: Option<u32>,
    /// Beats until the next memory cue; `None` when there is none in range.
    pub cue_countdown: Option<u16>,
    /// Position in the 4-beat bar (1..=4), 0 when unknown.
    pub beat_within_bar: u8,
    /// Monotonically-increasing packet counter; wraps u32.
    pub packet_counter: u32,
    pub active_loop: Option<ActiveLoop>,
}

impl CdjStatus {
    /// A powered-on player with nothing loaded.
    pub fn idle(device_name: DeviceName, device_number: u8) -> Self {
        Self {
            device_name,
            device_number,
            active: false,
            track_source_player: 0,
            track_source_slot: TrackSourceSlot::NoTrack,
            track_type: TrackType::NoTrack,
            rekordbox_id: 0,
            track_number: 0,
            usb_loaded: false,
            sd_loaded: false,
            link_media_available: false,
            play_state1: PlayState1::NoTrack,
            moving: false,
            play_state3: PlayState3::NoTrack,
            firmware: CDJ3000_FIRMWARE,
            sync_number: 0,
            playing: false,
            master: false,
            synced: false,
            on_air: false,
            pitch: PITCH_NEUTRAL,
            bpm_hundredths: None,
            master_handoff_to: None,
            beat_number: None,
            cue_countdown: None,
            beat_within_bar: 0,
            packet_counter: 0,
            active_loop: None,
        }
    }

    pub fn encode(&self) -> [u8; CDJ_STATUS_LEN] {
        let mut buf = [0u8; CDJ_STATUS_LEN];
        UpdateHeader {
            kind: update_kind::CDJ_STATUS,
            device_name: self.device_name.clone(),
            subtype: CDJ_STATUS_SUBTYPE,
            device_number: self.device_number,
        }
        .encode_into(&mut buf)
        .expect("buffer is always large enough");
        for &(off, value) in TEMPLATE_CONSTANTS {
            buf[off] = value;
        }

        buf[0x24] = self.device_number;
        buf[0x27] = self.active as u8;
        buf[0x28] = self.track_source_player;
        buf[0x29] = self.track_source_slot.to_byte();
        buf[0x2a] = self.track_type.to_byte();
        buf[0x2c..0x30].copy_from_slice(&self.rekordbox_id.to_be_bytes());
        buf[0x32..0x34].copy_from_slice(&self.track_number.to_be_bytes());
        buf[0x6f] = if self.usb_loaded { SLOT_LOADED } else { SLOT_EMPTY };
        buf[0x73] = if self.sd_loaded { SLOT_LOADED } else { SLOT_EMPTY };
        buf[0x75] = self.link_media_available as u8;
        buf[0x7b] = self.play_state1.to_byte();
        buf[0x7c..0x80].copy_from_slice(&self.firmware);
        buf[0x84..0x88].copy_from_slice(&self.sync_number.to_be_bytes());
        buf[0x89] = STATUS_FLAGS_BASE
            | (self.playing as u8) << 6
            | (self.master as u8) << 5
            | (self.synced as u8) << 4
            | (self.on_air as u8) << 3;
        buf[0x8b] = if self.moving { 0x7a } else { 0x7e };
        for off in [0x8c, 0x98, 0xc0, 0xc4] {
            buf[off..off + 4].copy_from_slice(&self.pitch.to_be_bytes());
        }
        buf[0x92..0x94].copy_from_slice(&encode_bpm_u16(self.bpm_hundredths.unwrap_or(NO_BPM)));
        buf[0x9d] = self.play_state3.to_byte();
        buf[0x9e] = self.master as u8;
        buf[0x9f] = self.master_handoff_to.unwrap_or(NO_MASTER_HANDOFF);
        buf[0xa0..0xa4].copy_from_slice(&self.beat_number.unwrap_or(NO_BEAT_NUMBER).to_be_bytes());
        buf[0xa4..0xa6]
            .copy_from_slice(&self.cue_countdown.unwrap_or(NO_CUE_COUNTDOWN).to_be_bytes());
        buf[0xa6] = self.beat_within_bar.min(4);
        buf[0xc8..0xcc].copy_from_slice(&self.packet_counter.to_be_bytes());
        if let Some(l) = self.active_loop {
            buf[0x1b6..0x1ba].copy_from_slice(&loop_ms_to_raw(l.start_ms).to_be_bytes());
            buf[0x1be..0x1c2].copy_from_slice(&loop_ms_to_raw(l.end_ms).to_be_bytes());
            buf[0x1c8..0x1ca].copy_from_slice(&l.beats.to_be_bytes());
        }
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        if buf.len() < CDJ_STATUS_LEN {
            return Err(DecodeError::TooShort {
                need: CDJ_STATUS_LEN,
                have: buf.len(),
            });
        }
        let header = UpdateHeader::decode(buf)?;
        if header.kind != update_kind::CDJ_STATUS {
            return Err(DecodeError::UnknownKind(header.kind));
        }
        let flags = buf[0x89];
        let bpm = u16_at(buf, 0x92);
        let beat = u32_at(buf, 0xa0);
        let cue = u16_at(buf, 0xa4);
        let loop_start = u32_at(buf, 0x1b6);
        let loop_end = u32_at(buf, 0x1be);
        let mut firmware = [0u8; 4];
        firmware.copy_from_slice(&buf[0x7c..0x80]);
        Ok(Self {
            device_name: header.device_name,
            device_number: header.device_number,
            active: buf[0x27] != 0,
            track_source_player: buf[0x28],
            track_source_slot: TrackSourceSlot::from_byte(buf[0x29]),
            track_type: TrackType::from_byte(buf[0x2a]),
            rekordbox_id: u32_at(buf, 0x2c),
            track_number: u16_at(buf, 0x32),
            usb_loaded: buf[0x6f] == SLOT_LOADED,
            sd_loaded: buf[0x73] == SLOT_LOADED,
            link_media_available: buf[0x75] != 0,
            play_state1: PlayState1::from_byte(buf[0x7b]),
            moving: buf[0x8b] == 0x7a,
            play_state3: PlayState3::from_byte(buf[0x9d]),
            firmware,
            sync_number: u32_at(buf, 0x84),
            playing: flags & 0x40 != 0,
            master: flags & 0x20 != 0,
            synced: flags & 0x10 != 0,
            on_air: flags & 0x08 != 0,
            pitch: u32_at(buf, 0x8c),
            bpm_hundredths: (bpm != NO_BPM).then_some(bpm),
            master_handoff_to: (buf[0x9f] != NO_MASTER_HANDOFF).then_some(buf[0x9f]),
            beat_number: (beat != NO_BEAT_NUMBER).then_some(beat),
            cue_countdown: (cue != NO_CUE_COUNTDOWN).then_some(cue),
            beat_within_bar: buf[0xa6],
            packet_counter: u32_at(buf, 0xc8),
            active_loop: (loop_start != 0 || loop_end != 0).then(|| ActiveLoop {
                start_ms: loop_raw_to_ms(loop_start),
                end_ms: loop_raw_to_ms(loop_end),
                beats: u16_at(buf, 0x1c8),
            }),
        })
    }
}

// ---------- DJM status ----------
//
// Layout (56 bytes), per beat-link's `MixerStatus` parser and the dysentery
// mixer-status figure:
//
//   0x00..0x24  UpdateHeader: kind 0x29, subtype 0x00, length-remaining 0x14
//   0x24        device number (copy of 0x21)
//   0x25..0x27  0x0000
//   0x27        status flags: 0xd0 from real mixers, | 0x20 when tempo master
//   0x28..0x2c  pitch (0x00100000, mixers always report +0%)
//   0x2c..0x2e  0x8000
//   0x2e..0x30  bpm_hundredths (u16 BE)
//   0x30..0x34  second pitch copy (0x00100000)
//   0x34        0x00
//   0x35        0x09
//   0x36        master handoff: device the master role is being yielded to,
//               0xff when none (beat-link treats 0xff as "no handoff")
//   0x37        beat within bar (1..=4)

pub const DJM_STATUS_SUBTYPE: u8 = 0x00;
/// Status flags real mixers send when they are not tempo master (playing +
/// synced bits set).
const DJM_FLAGS_BASE: u8 = 0xd0;
const DJM_MASTER_FLAG: u8 = 0x20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DjmStatus {
    pub device_name: DeviceName,
    pub device_number: u8,
    pub bpm_hundredths: u16,
    /// True if the mixer is currently tempo master.
    pub master: bool,
    /// Device the mixer is handing the tempo-master role to, if any.
    pub master_handoff_to: Option<u8>,
    /// Position in the 4-beat bar (1..=4).
    pub beat_within_bar: u8,
}

impl DjmStatus {
    pub fn idle(device_name: DeviceName) -> Self {
        Self {
            device_name,
            device_number: 33,
            bpm_hundredths: 12000,
            master: false,
            master_handoff_to: None,
            beat_within_bar: 1,
        }
    }

    pub fn encode(&self) -> [u8; DJM_STATUS_LEN] {
        let mut buf = [0u8; DJM_STATUS_LEN];
        UpdateHeader {
            kind: update_kind::MIXER_STATUS,
            device_name: self.device_name.clone(),
            subtype: DJM_STATUS_SUBTYPE,
            device_number: self.device_number,
        }
        .encode_into(&mut buf)
        .expect("buffer is always large enough");

        buf[0x24] = self.device_number;
        buf[0x27] = DJM_FLAGS_BASE | if self.master { DJM_MASTER_FLAG } else { 0 };
        buf[0x28..0x2c].copy_from_slice(&PITCH_NEUTRAL.to_be_bytes());
        buf[0x2c] = 0x80;
        buf[0x2e..0x30].copy_from_slice(&encode_bpm_u16(self.bpm_hundredths));
        buf[0x30..0x34].copy_from_slice(&PITCH_NEUTRAL.to_be_bytes());
        buf[0x35] = 0x09;
        buf[0x36] = self.master_handoff_to.unwrap_or(NO_MASTER_HANDOFF);
        buf[0x37] = self.beat_within_bar.clamp(1, 4);
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        if buf.len() < DJM_STATUS_LEN {
            return Err(DecodeError::TooShort {
                need: DJM_STATUS_LEN,
                have: buf.len(),
            });
        }
        let header = UpdateHeader::decode(buf)?;
        if header.kind != update_kind::MIXER_STATUS {
            return Err(DecodeError::UnknownKind(header.kind));
        }
        Ok(Self {
            device_name: header.device_name,
            device_number: header.device_number,
            bpm_hundredths: u16::from_be_bytes([buf[0x2e], buf[0x2f]]),
            master: buf[0x27] & DJM_MASTER_FLAG != 0,
            master_handoff_to: match buf[0x36] {
                NO_MASTER_HANDOFF => None,
                n => Some(n),
            },
            beat_within_bar: buf[0x37],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded(n: u8) -> CdjStatus {
        CdjStatus {
            active: true,
            track_source_player: n,
            track_source_slot: TrackSourceSlot::Usb,
            track_type: TrackType::Rekordbox,
            rekordbox_id: 42,
            track_number: 7,
            usb_loaded: true,
            link_media_available: true,
            play_state1: PlayState1::Playing,
            moving: true,
            play_state3: PlayState3::ForwardCdj,
            sync_number: 3,
            playing: true,
            master: true,
            on_air: true,
            bpm_hundredths: Some(12800),
            beat_number: Some(256),
            beat_within_bar: 4,
            packet_counter: 0xdead_beef,
            ..CdjStatus::idle(DeviceName::new("CDJ-3000").unwrap(), n)
        }
    }

    /// The offsets beat-link's `CdjStatus` getters read (from `CdjStatus.class`),
    /// so a layout regression names the field it broke.
    #[test]
    fn cdj_status_fields_land_where_beatlink_reads_them() {
        let b = loaded(2).encode();
        assert_eq!(b.len(), 0x200, "CDJ-3000 status length");
        assert_eq!(&b[0x0b..0x13], b"CDJ-3000", "DeviceUpdate reads the name at 0x0b");
        assert_eq!(b[0x21], 2, "getDeviceNumber reads 0x21");
        assert_eq!(&b[0x22..0x24], &(0x200u16 - 0x24).to_be_bytes(), "length - 0x24");
        assert_eq!(b[0x27], 1, "isBusy reads 0x27");
        assert_eq!(b[0x28], 2, "track source player");
        assert_eq!(b[0x29], 0x03, "USB slot");
        assert_eq!(b[0x2a], 0x01, "rekordbox track type");
        assert_eq!(&b[0x2c..0x30], &42u32.to_be_bytes(), "rekordbox ID at 0x2c");
        assert_eq!(&b[0x32..0x34], &7u16.to_be_bytes(), "getTrackNumber reads 0x32");
        assert_eq!(b[0x6f], 0, "isLocalUsbLoaded: 0x6f == 0");
        assert_eq!(b[0x73], 4, "isLocalSdEmpty: 0x73 == 4");
        assert_eq!(b[0x75], 1, "isLinkMediaAvailable reads 0x75");
        assert_eq!(b[0x7b], 0x03, "PlayState1 PLAYING");
        assert_eq!(&b[0x7c..0x80], b"3.22", "firmware at 0x7c");
        assert_eq!(&b[0x84..0x88], &3u32.to_be_bytes(), "getSyncNumber reads 0x84");
        assert_eq!(b[0x89], 0x84 | 0x40 | 0x20 | 0x08, "status flags at 0x89");
        assert_eq!(b[0x8b], 0x7a, "PlayState2 MOVING");
        assert_eq!(&b[0x8d..0x90], &[0x10, 0x00, 0x00], "getPitch(1) reads 0x8d");
        assert_eq!(&b[0x92..0x94], &12800u16.to_be_bytes(), "getBpm reads 0x92");
        assert_eq!(b[0x9d], 0x0d, "PlayState3 FORWARD_CDJ");
        assert_eq!(b[0x9f], 0xff, "no master handoff");
        assert_eq!(&b[0xa0..0xa4], &256u32.to_be_bytes(), "getBeatNumber reads 0xa0");
        assert_eq!(&b[0xa4..0xa6], &[0x01, 0xff], "no memory cue in range");
        assert_eq!(b[0xa6], 4, "getBeatWithinBar reads 0xa6");
        assert_eq!(&b[0xc8..0xcc], &0xdead_beefu32.to_be_bytes(), "packet counter");
    }

    #[test]
    fn idle_player_reports_no_track() {
        let b = CdjStatus::idle(DeviceName::new("CDJ-3000").unwrap(), 1).encode();
        assert_eq!(b[0x7b], 0x00, "PlayState1 NO_TRACK, so isTrackLoaded() is false");
        assert_eq!(&b[0x92..0x94], &[0xff, 0xff], "no BPM");
        assert_eq!(&b[0xa0..0xa4], &[0xff; 4], "no beat number");
        assert_eq!(b[0x6f], 4, "USB slot empty");
        assert_eq!(b[0x89], 0x84, "no flags set");
    }

    #[test]
    fn loop_fields_use_beatlink_units() {
        let s = CdjStatus {
            play_state1: PlayState1::Looping,
            active_loop: Some(ActiveLoop {
                start_ms: 65_536,
                end_ms: 131_072,
                beats: 4,
            }),
            ..loaded(1)
        };
        let b = s.encode();
        assert_eq!(&b[0x1b6..0x1ba], &1000u32.to_be_bytes());
        assert_eq!(&b[0x1be..0x1c2], &2000u32.to_be_bytes());
        assert_eq!(&b[0x1c8..0x1ca], &4u16.to_be_bytes());
        assert_eq!(CdjStatus::decode(&b).unwrap(), s);
    }

    #[test]
    fn cdj_status_roundtrip() {
        let idle = CdjStatus::idle(DeviceName::new("CDJ-3000").unwrap(), 3);
        for s in [idle, loaded(2)] {
            assert_eq!(CdjStatus::decode(&s.encode()).unwrap(), s);
        }
    }

    fn djm(master: bool, master_handoff_to: Option<u8>, beat_within_bar: u8) -> DjmStatus {
        DjmStatus {
            device_name: DeviceName::new("DJM-V10").unwrap(),
            device_number: 33,
            bpm_hundredths: 12450,
            master,
            master_handoff_to,
            beat_within_bar,
        }
    }

    /// Full packet, byte for byte, built from the dysentery mixer-status
    /// figure: mixer 33 as tempo master at 124.50 BPM on beat 2.
    #[test]
    fn djm_status_matches_golden_bytes() {
        #[rustfmt::skip]
        let golden: [u8; DJM_STATUS_LEN] = [
            0x51, 0x73, 0x70, 0x74, 0x31, 0x57, 0x6d, 0x4a, 0x4f, 0x4c, 0x29, 0x44, 0x4a, 0x4d, 0x2d, 0x56,
            0x31, 0x30, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
            0x00, 0x21, 0x00, 0x14, 0x21, 0x00, 0x00, 0xf0, 0x00, 0x10, 0x00, 0x00, 0x80, 0x00, 0x30, 0xa2,
            0x00, 0x10, 0x00, 0x00, 0x00, 0x09, 0xff, 0x02,
        ];
        assert_eq!(djm(true, None, 2).encode(), golden);
    }

    /// The offsets beat-link's `MixerStatus` getters read (from
    /// `MixerStatus.class`), so a layout regression names the field it broke.
    #[test]
    fn djm_fields_land_where_beatlink_reads_them() {
        let bytes = djm(false, Some(3), 4).encode();
        assert_eq!(bytes[0x0a], 0x29, "packet kind");
        assert_eq!(bytes[0x20], 0x00, "subtype 0: 0x22 holds the remaining length");
        assert_eq!(bytes[0x21], 33, "DeviceUpdate.getDeviceNumber reads 0x21");
        assert_eq!(&bytes[0x22..0x24], &[0x00, 0x14], "MixerStatus checks length - 0x24");
        assert_eq!(bytes[0x27], 0xd0, "MixerStatus.isTempoMaster reads bit 0x20 at 0x27");
        assert_eq!(&bytes[0x28..0x2c], &[0x00, 0x10, 0x00, 0x00], "MixerStatus.getPitch reads 0x28");
        assert_eq!(&bytes[0x2e..0x30], &12450u16.to_be_bytes(), "MixerStatus.getBpm reads 0x2e");
        assert_eq!(bytes[0x36], 3, "getDeviceMasterIsBeingYieldedTo reads 0x36");
        assert_eq!(bytes[0x37], 4, "MixerStatus.getBeatWithinBar reads 0x37");
    }

    #[test]
    fn djm_idle_is_not_master_and_has_no_handoff() {
        let bytes = DjmStatus::idle(DeviceName::new("DJM-V10").unwrap()).encode();
        assert_eq!(bytes[0x21], 33);
        assert_eq!(bytes[0x27] & 0x20, 0);
        assert_eq!(bytes[0x36], 0xff);
    }

    #[test]
    fn djm_status_roundtrip() {
        for s in [djm(true, None, 1), djm(false, Some(2), 3)] {
            assert_eq!(DjmStatus::decode(&s.encode()).unwrap(), s);
        }
    }
}
