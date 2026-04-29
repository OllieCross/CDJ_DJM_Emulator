#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::{Arc, Mutex};

use anyhow::Context;
use cdj_core::library::Library;
use cdj_core::net::Interface;
use cdj_core::orchestrator::{Fleet, FleetConfig, FleetHandle};
use clap::Parser;
use serde::Serialize;
use tauri::State;

// ---------------------------------------------------------------------------
// CLI args
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(name = "cdjapp", about = "CDJ Emulator desktop app")]
struct Args {
    /// Network interface to bind (overrides CDJ_IFACE env var).
    #[arg(short, long, env = "CDJ_IFACE")]
    iface: String,
    #[arg(long, default_value_t = 4)]
    players: u8,
    /// Rekordbox USB export root (overrides CDJ_LIBRARY env var).
    #[arg(long, env = "CDJ_LIBRARY")]
    library: Option<std::path::PathBuf>,
    #[arg(long)]
    no_mixer: bool,
}

// ---------------------------------------------------------------------------
// Shared Tauri state
// ---------------------------------------------------------------------------

struct AppState {
    fleet: Mutex<Option<FleetHandle>>,
    library: Mutex<Option<Arc<Library>>>,
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
struct TrackSummary {
    id: u32,
    title: String,
    artist: String,
    album: String,
    bpm: f32,
    duration_s: u32,
}

#[derive(Serialize, Clone)]
struct PlayerStatus {
    player: u8,
    playing: bool,
    track: Option<TrackSummary>,
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

#[tauri::command]
fn list_tracks(state: State<AppState>) -> Vec<TrackSummary> {
    let lib = state.library.lock().unwrap();
    let Some(lib) = lib.as_ref() else {
        return Vec::new();
    };
    lib.tracks
        .iter()
        .map(|t| TrackSummary {
            id: t.id,
            title: t.title.clone(),
            artist: t.artist.clone(),
            album: t.album.clone(),
            bpm: t.bpm_hundredths as f32 / 100.0,
            duration_s: t.duration_s,
        })
        .collect()
}

#[tauri::command]
fn get_players(state: State<AppState>) -> Vec<PlayerStatus> {
    let fleet = state.fleet.lock().unwrap();
    let Some(fleet) = fleet.as_ref() else {
        return Vec::new();
    };
    fleet
        .players
        .iter()
        .map(|h| {
            let track = h.state.loaded_track().and_then(|(lib, id)| {
                lib.track_by_id(id).map(|t| TrackSummary {
                    id: t.id,
                    title: t.title.clone(),
                    artist: t.artist.clone(),
                    album: t.album.clone(),
                    bpm: t.bpm_hundredths as f32 / 100.0,
                    duration_s: t.duration_s,
                })
            });
            PlayerStatus {
                player: h.player_number,
                playing: h.state.playing(),
                track,
            }
        })
        .collect()
}

#[tauri::command]
fn load_track(player: u8, track_id: u32, state: State<AppState>) -> Result<(), String> {
    let fleet = state.fleet.lock().unwrap();
    let fleet = fleet.as_ref().ok_or("fleet not running")?;
    fleet.load_track(player, track_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn play(player: u8, state: State<AppState>) {
    if let Some(fleet) = state.fleet.lock().unwrap().as_ref() {
        fleet.play(player);
    }
}

#[tauri::command]
fn pause(player: u8, state: State<AppState>) {
    if let Some(fleet) = state.fleet.lock().unwrap().as_ref() {
        fleet.pause(player);
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,cdj_core=debug")),
        )
        .init();

    let args = Args::parse();

    let iface = Interface::by_name(&args.iface)
        .with_context(|| format!("resolving interface {}", args.iface))?;

    let library = if let Some(root) = args.library {
        let lib = tokio::task::spawn_blocking(move || Library::open(&root))
            .await
            .context("library loader panicked")?
            .context("loading rekordbox library")?;
        Some(lib)
    } else {
        None
    };

    let cfg = FleetConfig {
        iface,
        num_players: args.players,
        include_mixer: !args.no_mixer,
        player_model: "CDJ-3000".to_string(),
        mixer_model: "DJM-V10".to_string(),
        initial_bpm_hundredths: 12000,
        autoplay: false,
        tracks: Vec::new(),
        beat_grid_offset_ms: 0,
        library: library.clone(),
    };

    let fleet = Fleet::new(cfg).start().await?;

    let state = AppState {
        fleet: Mutex::new(Some(fleet)),
        library: Mutex::new(library),
    };

    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            list_tracks,
            get_players,
            load_track,
            play,
            pause,
        ])
        .run(tauri::generate_context!())
        .expect("error running tauri application");

    Ok(())
}
