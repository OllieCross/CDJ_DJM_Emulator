//! The player and mixer models the emulator can present as.
//!
//! What differs between models on the wire, as far as public analysis
//! (beat-link, dysentery) shows:
//!
//! * the device name in every packet; clients read the model from it, and
//!   beat-link treats names specially (a "CDJ-...900"/"CDJ-...2000" name is a
//!   pre-nexus player; only "CDJ-3000" is trusted to serve track data to any
//!   player number);
//! * the CDJ status packet length: 0xd0 (pre-nexus), 0xd4 (nexus), 0x11c
//!   (nexus 2 generation), 0x200 (CDJ-3000). Shorter packets simply end
//!   earlier; fields keep their offsets;
//! * absolute-position packets, sent by CDJ-3000s only;
//! * hot cues: 3 on the CDJ-2000 and nexus, 8 on later players;
//! * mixer channels: 6 on the DJM-V10, 4 on the others.
//!
//! Entries marked unverified are educated guesses until compared with
//! captures of the real units.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PlayerModel {
    Cdj3000,
    Cdj3000X,
    Cdj2000Nxs2,
    Cdj2000Nxs,
    Cdj2000,
    Xdj1000Mk2,
}

impl PlayerModel {
    pub const ALL: [Self; 6] = [
        Self::Cdj3000,
        Self::Cdj3000X,
        Self::Cdj2000Nxs2,
        Self::Cdj2000Nxs,
        Self::Cdj2000,
        Self::Xdj1000Mk2,
    ];

    /// Name shown to people.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Cdj3000 => "CDJ-3000",
            Self::Cdj3000X => "CDJ-3000X",
            Self::Cdj2000Nxs2 => "CDJ-2000NXS2",
            Self::Cdj2000Nxs => "CDJ-2000NXS",
            Self::Cdj2000 => "CDJ-2000",
            Self::Xdj1000Mk2 => "XDJ-1000MK2",
        }
    }

    /// Name the player puts in its packets.
    pub fn device_name(self) -> &'static str {
        match self {
            Self::Cdj3000 => "CDJ-3000",
            // Unverified: may announce as "CDJ-3000".
            Self::Cdj3000X => "CDJ-3000X",
            Self::Cdj2000Nxs2 => "CDJ-2000NXS2",
            Self::Cdj2000Nxs => "CDJ-2000nexus",
            Self::Cdj2000 => "CDJ-2000",
            Self::Xdj1000Mk2 => "XDJ-1000MK2",
        }
    }

    /// Length of its CDJ status packets.
    pub fn status_len(self) -> usize {
        match self {
            Self::Cdj3000 | Self::Cdj3000X => 0x200,
            Self::Cdj2000Nxs2 => 0x11c,
            // Unverified: same generation as the NXS2.
            Self::Xdj1000Mk2 => 0x11c,
            Self::Cdj2000Nxs => 0xd4,
            Self::Cdj2000 => 0xd0,
        }
    }

    /// Firmware version in its status packets (unverified, plausible).
    pub fn firmware(self) -> [u8; 4] {
        match self {
            Self::Cdj3000 => *b"3.22",
            Self::Cdj3000X => *b"1.10",
            Self::Cdj2000Nxs2 => *b"1.85",
            Self::Cdj2000Nxs => *b"1.86",
            Self::Cdj2000 => *b"4.32",
            Self::Xdj1000Mk2 => *b"1.11",
        }
    }

    /// Sends absolute-position packets (CDJ-3000 generation).
    pub fn sends_position(self) -> bool {
        matches!(self, Self::Cdj3000 | Self::Cdj3000X)
    }

    /// Number of hot cues it can store and play (A, B, C, ...).
    pub fn hot_cues(self) -> usize {
        match self {
            Self::Cdj2000 | Self::Cdj2000Nxs => 3,
            _ => 8,
        }
    }

    /// Things about this model that are guesses, for people to see.
    pub fn notes(self) -> &'static str {
        match self {
            Self::Cdj3000 => "",
            Self::Cdj3000X => "Treated like a CDJ-3000; its name on the network is unverified.",
            Self::Cdj2000Nxs2 => "No absolute-position packets.",
            Self::Cdj2000Nxs => "3 hot cues, no absolute-position packets.",
            Self::Cdj2000 => "Pre-nexus: 3 hot cues, no absolute-position packets; clients ignore its bar position.",
            Self::Xdj1000Mk2 => "Status packet length unverified.",
        }
    }

    /// From a display or device name, ignoring case.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| {
            m.display_name().eq_ignore_ascii_case(name) || m.device_name().eq_ignore_ascii_case(name)
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MixerModel {
    DjmV10,
    DjmA9,
    Djm900Nxs2,
    Djm750Mk2,
}

impl MixerModel {
    pub const ALL: [Self; 4] = [Self::DjmV10, Self::DjmA9, Self::Djm900Nxs2, Self::Djm750Mk2];

    /// Name shown to people and put in its packets.
    pub fn device_name(self) -> &'static str {
        match self {
            Self::DjmV10 => "DJM-V10",
            Self::DjmA9 => "DJM-A9",
            Self::Djm900Nxs2 => "DJM-900NXS2",
            Self::Djm750Mk2 => "DJM-750MK2",
        }
    }

    /// Input channels, as reported in its channels-on-air packets.
    pub fn channels(self) -> u8 {
        match self {
            Self::DjmV10 => 6,
            _ => 4,
        }
    }

    pub fn notes(self) -> &'static str {
        match self {
            Self::DjmV10 => "6 channels; the emulator's EQ is 3-band (the V10 has 4).",
            _ => "",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.device_name().eq_ignore_ascii_case(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cdj_proto::DeviceName;

    #[test]
    fn every_name_fits_the_packet_field() {
        for m in PlayerModel::ALL {
            DeviceName::new(m.device_name()).unwrap();
        }
        for m in MixerModel::ALL {
            DeviceName::new(m.device_name()).unwrap();
        }
    }

    #[test]
    fn names_round_trip() {
        for m in PlayerModel::ALL {
            assert_eq!(PlayerModel::from_name(m.display_name()), Some(m));
            assert_eq!(PlayerModel::from_name(m.device_name()), Some(m));
        }
        assert_eq!(PlayerModel::from_name("cdj-2000nxs2"), Some(PlayerModel::Cdj2000Nxs2));
        assert_eq!(MixerModel::from_name("djm-a9"), Some(MixerModel::DjmA9));
        assert_eq!(PlayerModel::from_name("CDJ-1000"), None);
    }

    #[test]
    fn beatlink_reads_pre_nexus_from_the_name() {
        // beat-link: a CDJ whose name ends in "900" or "2000" is pre-nexus.
        let pre_nexus = |m: PlayerModel| {
            let n = m.device_name();
            n.starts_with("CDJ") && (n.ends_with("900") || n.ends_with("2000"))
        };
        assert!(pre_nexus(PlayerModel::Cdj2000));
        assert!(PlayerModel::ALL.iter().filter(|&&m| m != PlayerModel::Cdj2000).all(|&m| !pre_nexus(m)));
    }
}
