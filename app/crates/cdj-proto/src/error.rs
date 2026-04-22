use thiserror::Error;

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("buffer too short: need {need} bytes, have {have}")]
    TooShort { need: usize, have: usize },

    #[error("bad magic: expected Qspt1WmJOL, got {got:02x?}")]
    BadMagic { got: [u8; 10] },

    #[error("unknown packet kind 0x{0:02x}")]
    UnknownKind(u8),

    #[error("invalid device name (non-UTF8 or too long)")]
    InvalidDeviceName,

    #[error("unexpected sub-type {got:#04x} for packet kind {kind:#04x}")]
    UnexpectedSubtype { kind: u8, got: u8 },

    #[error("trailing bytes after packet: {0} byte(s) remaining")]
    TrailingBytes(usize),
}

#[derive(Debug, Error)]
pub enum EncodeError {
    #[error("device name too long (max 20 bytes UTF-8), got {0}")]
    DeviceNameTooLong(usize),

    #[error("output buffer too small")]
    BufferTooSmall,
}
