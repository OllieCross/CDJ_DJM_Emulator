//! Device orchestration for the CDJ emulator: virtual devices, network I/O,
//! timing.
//!
//! This crate is intentionally sans-UI. The desktop app (Tauri) will depend on
//! this crate through a thin command layer; a headless CLI (`cdjd`) does the
//! same for development and automated testing.

pub mod dbserver;
pub mod deck;
pub mod engine;
pub mod library;
pub mod feth;
pub mod grid;
pub mod link;
pub mod media;
pub mod mixer;
pub mod models;
pub mod net;
pub mod rig;
pub mod track_audio;
pub mod virtual_cdj;
pub mod virtual_djm;
