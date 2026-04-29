//! Device orchestration for the CDJ emulator: virtual devices, network I/O,
//! timing.
//!
//! This crate is intentionally sans-UI. The desktop app (Tauri) will depend on
//! this crate through a thin command layer; a headless CLI (`cdjd`) does the
//! same for development and automated testing.

pub mod audio;
pub mod beat_clock;
pub mod dbserver;
pub mod library;
pub mod feth;
pub mod net;
pub mod orchestrator;
pub mod player_state;
pub mod virtual_cdj;
pub mod virtual_djm;
