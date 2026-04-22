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
//! (`51 73 70 74 31 57 6d 4a 4f 4c`), followed by a 1-byte "kind" discriminator,
//! followed by a 1-byte constant `00`, followed by a 20-byte UTF-8 device-name
//! field (space-padded, NUL-terminated at byte 20), followed by a 1-byte
//! constant `01`, followed by a 1-byte sub-type, then the payload.
//!
//! See submodules for per-packet structures.

pub mod announce;
pub mod claim;
pub mod error;
pub mod header;
pub mod status;

pub use announce::{KeepAlive, MIXER_NUM, PLAYER_NUM_MAX, PLAYER_NUM_MIN};
pub use claim::{ClaimStage1, ClaimStage2, ClaimStage3, CLAIM_PACKET_SPACING_MS, CLAIM_REPEATS};
pub use error::{DecodeError, EncodeError};
pub use header::{DeviceName, Header, Magic, PacketKind, MAGIC, MAGIC_LEN};
pub use status::{CdjStatus, DjmStatus};
