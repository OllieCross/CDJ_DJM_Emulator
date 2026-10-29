//! Pro DJ Link packet codec.
//!
//! Clean-room implementation based on the public packet analysis published by
//! the Deep-Symmetry project (dysentery `packets.pdf`). No Pioneer / AlphaTheta
//! source is used or referenced here.
//!
//! Packets travel over UDP on three ports:
//! * 50000 - device announce / keep-alive / number claim
//! * 50001 - beat broadcast
//! * 50002 - CDJ / DJM status and control
//!
//! Every packet begins with the 10-byte magic string `Qspt1WmJOL`
//! (`51 73 70 74 31 57 6d 4a 4f 4c`), followed by a 1-byte "kind"
//! discriminator and a 20-byte NUL-padded device name. The two header shapes
//! differ in where that name sits:
//!
//! * port 50000 ([`Header`]): a constant `00`, the name at `0x0c`, then
//!   `01`, the sub-type and a length field.
//! * ports 50001 / 50002 ([`UpdateHeader`]): the name at `0x0b`, then `01`,
//!   the sub-type, the device number at `0x21` and a length field.
//!
//! See submodules for per-packet structures.

pub mod announce;
pub mod beat;
pub mod claim;
pub mod control;
pub mod dbserver;
pub mod error;
pub mod header;
pub mod position;
pub mod requests;
pub mod status;

pub use announce::{KeepAlive, MIXER_NUM, PLAYER_NUM_MAX, PLAYER_NUM_MIN};
pub use beat::Beat;
pub use claim::{ClaimStage1, ClaimStage2, ClaimStage3, CLAIM_PACKET_SPACING_MS, CLAIM_REPEATS};
pub use error::{DecodeError, EncodeError};
pub use control::{
    ChannelsOnAir, FaderStart, FaderStartAction, MasterHandoffRequest, MasterHandoffResponse,
    SyncCommand, SyncControl,
};
pub use header::{
    update_kind, DeviceName, Header, Magic, PacketKind, UpdateHeader, MAGIC, MAGIC_LEN,
    PITCH_NEUTRAL,
};
pub use position::PrecisePosition;
pub use requests::{LoadTrack, LoadTrackAck, MediaQuery, MediaResponse};
pub use status::{
    ActiveLoop, CdjStatus, DjmStatus, PlayState1, PlayState3, TrackSourceSlot, TrackType,
};
