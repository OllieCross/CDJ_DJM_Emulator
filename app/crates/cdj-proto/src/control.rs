//! Short control packets on port 50001: mixer on-air and fader-start
//! commands, tempo-master handoff and sync control.
//!
//! Layouts follow the templates beat-link's `VirtualCdj` sends to real
//! players and how its `BeatFinder` parses the ones it receives. All share
//! the [`UpdateHeader`]; byte 0x21 is the sender's device number.
//!
//! ```text
//! kind  packet                 len   payload
//! 0x02  fader start            0x28  0x24..0x28 per channel 1-4: 0 start, 1 stop, 2 no change
//! 0x03  channels on air        0x2d  0x24..0x28 per channel 1-4: 1 on-air
//!       (6-channel, subtype 3) 0x35  ... plus channels 5-6 at 0x2d..0x2f
//! 0x26  master handoff request 0x28  0x27 requester
//! 0x27  master handoff reply   0x2c  0x27 responder, 0x2b 1 = yielding
//! 0x2a  sync control           0x2c  0x27 sender, 0x2b command
//! ```

use crate::error::DecodeError;
use crate::header::{update_kind, DeviceName, UpdateHeader};

fn header(kind: u8, name: &DeviceName, subtype: u8, device_number: u8) -> UpdateHeader {
    UpdateHeader {
        kind,
        device_name: name.clone(),
        subtype,
        device_number,
    }
}

fn decode_header(buf: &[u8], kind: u8, len: usize) -> Result<UpdateHeader, DecodeError> {
    if buf.len() < len {
        return Err(DecodeError::TooShort {
            need: len,
            have: buf.len(),
        });
    }
    let header = UpdateHeader::decode(buf)?;
    if header.kind != kind {
        return Err(DecodeError::UnknownKind(header.kind));
    }
    Ok(header)
}

// ---------- channels on air ----------

pub const ON_AIR_LEN: usize = 0x2d;
pub const ON_AIR_EXTENDED_LEN: usize = 0x35;
const ON_AIR_EXTENDED_SUBTYPE: u8 = 0x03;

/// Sent by the mixer (broadcast) so players can light their on-air flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelsOnAir {
    pub device_name: DeviceName,
    pub device_number: u8,
    /// One entry per mixer channel: 4, or 6 for six-channel mixers such as
    /// the DJM-V10 (sent as the extended packet).
    pub channels: Vec<bool>,
}

impl ChannelsOnAir {
    pub fn encode(&self) -> Vec<u8> {
        let extended = self.channels.len() > 4;
        let (len, subtype) = if extended {
            (ON_AIR_EXTENDED_LEN, ON_AIR_EXTENDED_SUBTYPE)
        } else {
            (ON_AIR_LEN, 0x00)
        };
        let mut buf = vec![0u8; len];
        header(update_kind::CHANNELS_ON_AIR, &self.device_name, subtype, self.device_number)
            .encode_into(&mut buf)
            .expect("buffer is always large enough");
        for (i, &on) in self.channels.iter().take(6).enumerate() {
            let off = if i < 4 { 0x24 + i } else { 0x2d + (i - 4) };
            buf[off] = on as u8;
        }
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        let h = decode_header(buf, update_kind::CHANNELS_ON_AIR, ON_AIR_LEN)?;
        let mut channels: Vec<bool> = buf[0x24..0x28].iter().map(|&b| b != 0).collect();
        if buf.len() >= ON_AIR_EXTENDED_LEN {
            channels.extend(buf[0x2d..0x2f].iter().map(|&b| b != 0));
        }
        Ok(Self {
            device_name: h.device_name,
            device_number: h.device_number,
            channels,
        })
    }
}

// ---------- fader start ----------

pub const FADER_START_LEN: usize = 0x28;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaderStartAction {
    Start,
    Stop,
    NoChange,
}

impl FaderStartAction {
    fn to_byte(self) -> u8 {
        match self {
            Self::Start => 0,
            Self::Stop => 1,
            Self::NoChange => 2,
        }
    }

    fn from_byte(b: u8) -> Self {
        match b {
            0 => Self::Start,
            1 => Self::Stop,
            _ => Self::NoChange,
        }
    }
}

/// Mixer telling players 1-4 to start or stop (fader start / cross-fader start).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaderStart {
    pub device_name: DeviceName,
    pub device_number: u8,
    pub players: [FaderStartAction; 4],
}

impl FaderStart {
    pub fn encode(&self) -> [u8; FADER_START_LEN] {
        let mut buf = [0u8; FADER_START_LEN];
        header(update_kind::FADER_START, &self.device_name, 0x00, self.device_number)
            .encode_into(&mut buf)
            .expect("buffer is always large enough");
        for (i, action) in self.players.iter().enumerate() {
            buf[0x24 + i] = action.to_byte();
        }
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        let h = decode_header(buf, update_kind::FADER_START, FADER_START_LEN)?;
        Ok(Self {
            device_name: h.device_name,
            device_number: h.device_number,
            players: std::array::from_fn(|i| FaderStartAction::from_byte(buf[0x24 + i])),
        })
    }
}

// ---------- tempo master handoff ----------

pub const MASTER_HANDOFF_REQUEST_LEN: usize = 0x28;
pub const MASTER_HANDOFF_RESPONSE_LEN: usize = 0x2c;

/// Sent to the current tempo master by a device that wants the role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MasterHandoffRequest {
    pub device_name: DeviceName,
    pub device_number: u8,
}

impl MasterHandoffRequest {
    pub fn encode(&self) -> [u8; MASTER_HANDOFF_REQUEST_LEN] {
        let mut buf = [0u8; MASTER_HANDOFF_REQUEST_LEN];
        header(update_kind::MASTER_HANDOFF_REQUEST, &self.device_name, 0x00, self.device_number)
            .encode_into(&mut buf)
            .expect("buffer is always large enough");
        buf[0x27] = self.device_number;
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        let h = decode_header(buf, update_kind::MASTER_HANDOFF_REQUEST, MASTER_HANDOFF_REQUEST_LEN)?;
        Ok(Self {
            device_name: h.device_name,
            device_number: h.device_number,
        })
    }
}

/// The current master's answer to a [`MasterHandoffRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MasterHandoffResponse {
    pub device_name: DeviceName,
    pub device_number: u8,
    pub yielding: bool,
}

impl MasterHandoffResponse {
    pub fn encode(&self) -> [u8; MASTER_HANDOFF_RESPONSE_LEN] {
        let mut buf = [0u8; MASTER_HANDOFF_RESPONSE_LEN];
        header(update_kind::MASTER_HANDOFF_RESPONSE, &self.device_name, 0x00, self.device_number)
            .encode_into(&mut buf)
            .expect("buffer is always large enough");
        buf[0x27] = self.device_number;
        buf[0x2b] = self.yielding as u8;
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        let h = decode_header(buf, update_kind::MASTER_HANDOFF_RESPONSE, MASTER_HANDOFF_RESPONSE_LEN)?;
        Ok(Self {
            device_name: h.device_name,
            device_number: h.device_number,
            yielding: buf[0x2b] == 1,
        })
    }
}

// ---------- sync control ----------

pub const SYNC_CONTROL_LEN: usize = 0x2c;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncCommand {
    BecomeMaster,
    SyncOn,
    SyncOff,
    Unknown(u8),
}

impl SyncCommand {
    fn to_byte(self) -> u8 {
        match self {
            Self::BecomeMaster => 0x01,
            Self::SyncOn => 0x10,
            Self::SyncOff => 0x20,
            Self::Unknown(b) => b,
        }
    }

    fn from_byte(b: u8) -> Self {
        match b {
            0x01 => Self::BecomeMaster,
            0x10 => Self::SyncOn,
            0x20 => Self::SyncOff,
            other => Self::Unknown(other),
        }
    }
}

/// Tells the receiving player to turn sync on/off or become tempo master.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncControl {
    pub device_name: DeviceName,
    pub device_number: u8,
    pub command: SyncCommand,
}

impl SyncControl {
    pub fn encode(&self) -> [u8; SYNC_CONTROL_LEN] {
        let mut buf = [0u8; SYNC_CONTROL_LEN];
        header(update_kind::SYNC_CONTROL, &self.device_name, 0x00, self.device_number)
            .encode_into(&mut buf)
            .expect("buffer is always large enough");
        buf[0x27] = self.device_number;
        buf[0x2b] = self.command.to_byte();
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        let h = decode_header(buf, update_kind::SYNC_CONTROL, SYNC_CONTROL_LEN)?;
        Ok(Self {
            device_name: h.device_name,
            device_number: h.device_number,
            command: SyncCommand::from_byte(buf[0x2b]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name() -> DeviceName {
        DeviceName::new("DJM-V10").unwrap()
    }

    /// Non-zero bytes after the name match beat-link's templates
    /// (`VirtualCdj.*_PAYLOAD`), with the sender's number filled in.
    fn assert_template(buf: &[u8], len: usize, nonzero: &[(usize, u8)]) {
        assert_eq!(buf.len(), len);
        for (off, &b) in buf.iter().enumerate().skip(0x1f) {
            let want = nonzero.iter().find(|(o, _)| *o == off).map_or(0, |(_, v)| *v);
            assert_eq!(b, want, "byte {off:#04x}");
        }
    }

    #[test]
    fn on_air_four_channels_matches_beatlink_template() {
        let p = ChannelsOnAir {
            device_name: name(),
            device_number: 33,
            channels: vec![true, false, true, false],
        };
        let b = p.encode();
        assert_eq!(b[0x0a], 0x03);
        assert_template(&b, 0x2d, &[(0x1f, 0x01), (0x21, 33), (0x23, 0x09), (0x24, 1), (0x26, 1)]);
        assert_eq!(ChannelsOnAir::decode(&b).unwrap(), p);
    }

    #[test]
    fn on_air_six_channels_uses_extended_packet() {
        let p = ChannelsOnAir {
            device_name: name(),
            device_number: 33,
            channels: vec![false, true, false, false, false, true],
        };
        let b = p.encode();
        // beat-link BeatFinder.getAudibleChannels: channels 5-6 at 0x2d..0x2f.
        assert_template(
            &b,
            0x35,
            &[(0x1f, 0x01), (0x20, 0x03), (0x21, 33), (0x23, 0x11), (0x25, 1), (0x2e, 1)],
        );
        assert_eq!(ChannelsOnAir::decode(&b).unwrap(), p);
    }

    #[test]
    fn fader_start_matches_beatlink_template() {
        use FaderStartAction::*;
        let p = FaderStart {
            device_name: name(),
            device_number: 33,
            players: [Start, Stop, NoChange, NoChange],
        };
        let b = p.encode();
        assert_eq!(b[0x0a], 0x02);
        assert_template(&b, 0x28, &[(0x1f, 0x01), (0x21, 33), (0x23, 0x04), (0x25, 1), (0x26, 2), (0x27, 2)]);
        assert_eq!(FaderStart::decode(&b).unwrap(), p);
    }

    #[test]
    fn master_handoff_packets_match_beatlink_templates() {
        let name = DeviceName::new("CDJ-3000").unwrap();
        let req = MasterHandoffRequest {
            device_name: name.clone(),
            device_number: 2,
        };
        let b = req.encode();
        assert_eq!(b[0x0a], 0x26);
        assert_template(&b, 0x28, &[(0x1f, 0x01), (0x21, 2), (0x23, 0x04), (0x27, 2)]);
        assert_eq!(MasterHandoffRequest::decode(&b).unwrap(), req);

        let resp = MasterHandoffResponse {
            device_name: name,
            device_number: 1,
            yielding: true,
        };
        let b = resp.encode();
        assert_eq!(b[0x0a], 0x27);
        assert_template(&b, 0x2c, &[(0x1f, 0x01), (0x21, 1), (0x23, 0x08), (0x27, 1), (0x2b, 1)]);
        assert_eq!(MasterHandoffResponse::decode(&b).unwrap(), resp);
    }

    #[test]
    fn sync_control_matches_beatlink_template() {
        let p = SyncControl {
            device_name: DeviceName::new("CDJ-3000").unwrap(),
            device_number: 4,
            command: SyncCommand::SyncOn,
        };
        let b = p.encode();
        assert_eq!(b[0x0a], 0x2a);
        assert_template(&b, 0x2c, &[(0x1f, 0x01), (0x21, 4), (0x23, 0x08), (0x27, 4), (0x2b, 0x10)]);
        assert_eq!(SyncControl::decode(&b).unwrap(), p);
    }
}
