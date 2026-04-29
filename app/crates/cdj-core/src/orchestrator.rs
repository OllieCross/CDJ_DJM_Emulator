//! Fleet orchestrator: run N virtual CDJs + 1 virtual DJM in a single process.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use cdj_proto::DeviceName;
use tokio::task::JoinSet;
use tracing::{error, info, warn};

use crate::dbserver::{DbServer, DbServerConfig};
use crate::library::Library;
use crate::net::{bind_sender, Interface};
use crate::player_state::PlayerState;
use crate::virtual_cdj::{VirtualCdj, VirtualCdjConfig};
use crate::virtual_djm::{VirtualDjm, VirtualDjmConfig};

#[derive(Debug, Clone)]
pub struct FleetConfig {
    pub iface: Interface,
    pub num_players: u8,
    pub include_mixer: bool,
    pub player_model: String,
    pub mixer_model: String,
    pub initial_bpm_hundredths: u16,
    pub autoplay: bool,
    /// Optional track files, assigned sequentially to player 1, 2, 3, 4.
    pub tracks: Vec<Option<PathBuf>>,
    pub beat_grid_offset_ms: u32,
    /// Rekordbox USB export library. Tracks are assigned in PDB order to
    /// players 1..N at startup; can be swapped at runtime via `FleetHandle`.
    pub library: Option<Arc<Library>>,
}

impl FleetConfig {
    pub fn default_four_plus_mixer(iface: Interface) -> Self {
        Self {
            iface,
            num_players: 4,
            include_mixer: true,
            player_model: "CDJ-3000".to_string(),
            mixer_model: "DJM-V10".to_string(),
            initial_bpm_hundredths: 12000,
            autoplay: false,
            tracks: Vec::new(),
            beat_grid_offset_ms: 0,
            library: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Runtime handle
// ---------------------------------------------------------------------------

/// Per-player handle for UI / CLI control.
#[derive(Clone)]
pub struct PlayerHandle {
    pub player_number: u8,
    pub state: Arc<PlayerState>,
}

/// Returned by `Fleet::start()`. Lets the UI load tracks, play, and pause
/// individual players without touching the running network tasks.
#[derive(Clone)]
pub struct FleetHandle {
    pub players: Vec<PlayerHandle>,
    pub library: Option<Arc<Library>>,
}

impl FleetHandle {
    pub fn player(&self, n: u8) -> Option<&PlayerHandle> {
        self.players.iter().find(|p| p.player_number == n)
    }

    pub fn load_track(&self, player: u8, track_id: u32) -> anyhow::Result<()> {
        let lib = self.library.as_ref().context("no library loaded")?;
        let handle = self.player(player).context("invalid player number")?;
        lib.track_by_id(track_id).context("track not found in library")?;
        handle.state.load_track(lib.clone(), track_id);
        Ok(())
    }

    pub fn play(&self, player: u8) {
        if let Some(h) = self.player(player) {
            h.state.set_playing(true);
        }
    }

    pub fn pause(&self, player: u8) {
        if let Some(h) = self.player(player) {
            h.state.set_playing(false);
        }
    }
}

// ---------------------------------------------------------------------------
// Fleet
// ---------------------------------------------------------------------------

pub struct Fleet {
    cfg: FleetConfig,
}

impl Fleet {
    pub fn new(cfg: FleetConfig) -> Self {
        Self { cfg }
    }

    /// Build all tasks and return the handle. Tasks run as detached tokio
    /// jobs; errors are logged. Use this from the Tauri app.
    pub async fn start(self) -> anyhow::Result<FleetHandle> {
        let (handle, tasks) = self.setup().await?;
        tokio::spawn(async move {
            let mut tasks = tasks;
            while let Some(result) = tasks.join_next().await {
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => error!("fleet task failed: {e}"),
                    Err(e) => error!("fleet task panicked: {e}"),
                }
            }
        });
        Ok(handle)
    }

    /// Build all tasks and drive them to completion. Use this from the CLI.
    pub async fn run(self) -> anyhow::Result<()> {
        let (_, mut tasks) = self.setup().await?;
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    warn!("device task exited: {e}");
                    return Err(e);
                }
                Err(e) => {
                    warn!("device task panicked: {e}");
                    return Err(anyhow::anyhow!("task join failed: {e}"));
                }
            }
        }
        Ok(())
    }

    async fn setup(self) -> anyhow::Result<(FleetHandle, JoinSet<anyhow::Result<()>>)> {
        assert!(
            (1..=4).contains(&self.cfg.num_players),
            "num_players must be in 1..=4"
        );
        let _ = DeviceName::new(&self.cfg.player_model)
            .map_err(|e| anyhow::anyhow!("player_model invalid: {e}"))?;
        let _ = DeviceName::new(&self.cfg.mixer_model)
            .map_err(|e| anyhow::anyhow!("mixer_model invalid: {e}"))?;

        let base = self.cfg.iface.ipv4.octets();
        let name = self.cfg.iface.name.as_str();

        info!(
            iface = %self.cfg.iface.name,
            ip = %self.cfg.iface.ipv4,
            players = self.cfg.num_players,
            mixer = self.cfg.include_mixer,
            bpm = self.cfg.initial_bpm_hundredths as f64 / 100.0,
            autoplay = self.cfg.autoplay,
            "fleet starting"
        );

        let mut tasks = JoinSet::new();
        let mut player_handles = Vec::new();

        for n in 1..=self.cfg.num_players {
            let mut mac = self.cfg.iface.mac;
            mac[5] = mac[5].wrapping_add(n);

            let mut device_ip_octets = base;
            device_ip_octets[3] = n;
            let device_ip = std::net::Ipv4Addr::from(device_ip_octets);

            let state = Arc::new(PlayerState::new(self.cfg.initial_bpm_hundredths));
            if self.cfg.autoplay {
                state.set_playing(true);
            }
            state.set_beat_grid_offset_ms(self.cfg.beat_grid_offset_ms);

            // Load track metadata from library into this player's dbserver state.
            // We do NOT set an audio path from the library; library loading is
            // metadata-only. Audio is only started when --track paths are passed
            // explicitly, keeping playback fully user-controlled.
            if let Some(lib) = &self.cfg.library {
                if let Some(track) = lib.tracks.get(n as usize - 1) {
                    state.load_track(lib.clone(), track.id);
                }
            }

            // Explicit per-player --track flag starts audio for that player.
            let track = self.cfg.tracks.get(n as usize - 1).cloned().flatten();

            let announce_sock = Arc::new(bind_sender(device_ip, name).await?);
            let beat_sock = Arc::new(bind_sender(device_ip, name).await?);
            let status_sock = Arc::new(bind_sender(device_ip, name).await?);

            let cdj_cfg = VirtualCdjConfig {
                model_name: self.cfg.player_model.clone(),
                device_number: n,
                iface: self.cfg.iface.clone(),
                mac,
                ip: device_ip_octets,
                track,
            };
            let cdj = VirtualCdj::new(cdj_cfg, announce_sock, beat_sock, status_sock, state.clone());
            tasks.spawn(async move { cdj.run().await });

            let db_cfg = DbServerConfig {
                device_number: n,
                ip: device_ip,
                player_model: self.cfg.player_model.clone(),
            };
            let db = DbServer::new(db_cfg, state.clone());
            tasks.spawn(async move { db.run().await });

            player_handles.push(PlayerHandle { player_number: n, state: state.clone() });
        }

        if self.cfg.include_mixer {
            let mut mac = self.cfg.iface.mac;
            mac[5] = mac[5].wrapping_add(33);

            let mut djm_ip_octets = base;
            djm_ip_octets[3] = 33;
            let djm_ip = std::net::Ipv4Addr::from(djm_ip_octets);

            let djm_announce_sock = Arc::new(bind_sender(djm_ip, name).await?);
            let djm_status_sock = Arc::new(bind_sender(djm_ip, name).await?);

            let djm_cfg = VirtualDjmConfig {
                model_name: self.cfg.mixer_model.clone(),
                iface: self.cfg.iface.clone(),
                mac,
                ip: djm_ip_octets,
            };
            let djm = VirtualDjm::new(djm_cfg, djm_announce_sock, djm_status_sock);
            tasks.spawn(async move { djm.run().await });
        }

        let handle = FleetHandle {
            players: player_handles,
            library: self.cfg.library.clone(),
        };
        Ok((handle, tasks))
    }
}
