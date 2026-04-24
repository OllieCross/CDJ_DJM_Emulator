//! Fleet orchestrator: run N virtual CDJs + 1 virtual DJM in a single process.
//!
//! Each device gets its own set of UDP sockets bound to its own IP address
//! (last octet = device number). Real CDJs have unique IPs; Pro DJ Link clients
//! like ShowKontrol use source IP to identify devices, so sharing an IP causes
//! all devices to collapse into one row.

use std::path::PathBuf;
use std::sync::Arc;

use cdj_proto::DeviceName;
use tokio::task::JoinSet;
use tracing::{info, warn};

use crate::dbserver::{DbServer, DbServerConfig};
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
    /// Initial BPM applied to every player (hundredths: 12000 = 120.00).
    pub initial_bpm_hundredths: u16,
    /// If true, every player starts in the "playing" state and thus emits
    /// beat packets. Useful for standalone timecode dev without audio loaded.
    pub autoplay: bool,
    /// Optional track files, assigned sequentially to player 1, 2, 3, 4.
    /// Empty slots leave the corresponding player idle.
    pub tracks: Vec<Option<PathBuf>>,
    /// Beat-grid offset in milliseconds applied to every loaded track.
    /// Controls where beat 1 of bar 1 lands relative to playback start.
    /// Comes from `--beat-offset-ms`; per-track beat grids arrive with M3.
    pub beat_grid_offset_ms: u32,
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
        }
    }
}

pub struct Fleet {
    cfg: FleetConfig,
}

impl Fleet {
    pub fn new(cfg: FleetConfig) -> Self {
        Self { cfg }
    }

    pub async fn run(self) -> anyhow::Result<()> {
        assert!(
            (1..=4).contains(&self.cfg.num_players),
            "num_players must be in 1..=4"
        );
        // Sanity-check the model names up front so we fail fast before
        // spawning network tasks.
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

        for n in 1..=self.cfg.num_players {
            let mut mac = self.cfg.iface.mac;
            mac[5] = mac[5].wrapping_add(n);

            // Each virtual CDJ gets a unique IP (last octet = device number) so
            // that Pro DJ Link clients can distinguish devices by source address.
            let mut device_ip_octets = base;
            device_ip_octets[3] = n;
            let device_ip = std::net::Ipv4Addr::from(device_ip_octets);

            let state = Arc::new(PlayerState::new(self.cfg.initial_bpm_hundredths));
            if self.cfg.autoplay {
                state.set_playing(true);
            }
            state.set_beat_grid_offset_ms(self.cfg.beat_grid_offset_ms);

            let track = self
                .cfg
                .tracks
                .get(n as usize - 1)
                .cloned()
                .flatten();

            let announce_sock = Arc::new(bind_sender(device_ip, name).await?);
            let beat_sock = Arc::new(bind_sender(device_ip, name).await?);
            let status_sock = Arc::new(bind_sender(device_ip, name).await?);

            let cfg = VirtualCdjConfig {
                model_name: self.cfg.player_model.clone(),
                device_number: n,
                iface: self.cfg.iface.clone(),
                mac,
                ip: device_ip_octets,
                track,
            };
            let cdj = VirtualCdj::new(cfg, announce_sock, beat_sock, status_sock, state.clone());
            tasks.spawn(async move { cdj.run().await });

            // Each virtual CDJ also hosts a dbserver on its own IP so clients
            // (ShowKontrol, rekordbox) can fetch per-deck track metadata. The
            // TCP port is fixed at 1051 and advertised via the UDP 12523
            // port-discovery handshake.
            let db_cfg = DbServerConfig {
                device_number: n,
                ip: device_ip,
                player_model: self.cfg.player_model.clone(),
            };
            let db = DbServer::new(db_cfg, state);
            tasks.spawn(async move { db.run().await });
        }

        if self.cfg.include_mixer {
            let mut mac = self.cfg.iface.mac;
            mac[5] = mac[5].wrapping_add(33);

            // DJM gets last octet 33, matching its device number.
            let mut djm_ip_octets = base;
            djm_ip_octets[3] = 33;
            let djm_ip = std::net::Ipv4Addr::from(djm_ip_octets);

            let djm_announce_sock = Arc::new(bind_sender(djm_ip, name).await?);
            let djm_status_sock = Arc::new(bind_sender(djm_ip, name).await?);

            let cfg = VirtualDjmConfig {
                model_name: self.cfg.mixer_model.clone(),
                iface: self.cfg.iface.clone(),
                mac,
                ip: djm_ip_octets,
            };
            let djm = VirtualDjm::new(cfg, djm_announce_sock, djm_status_sock);
            tasks.spawn(async move { djm.run().await });
        }

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
}
