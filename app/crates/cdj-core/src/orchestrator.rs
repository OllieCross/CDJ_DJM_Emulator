//! Fleet orchestrator: run N virtual CDJs + 1 virtual DJM in a single process.
//!
//! All devices share three UDP sockets (one per Pro DJ Link port) with
//! broadcast enabled. Packets still carry per-device MAC and IP in their
//! payloads, which is what the Pro DJ Link protocol keys off.

use std::path::PathBuf;
use std::sync::Arc;

use cdj_proto::DeviceName;
use tokio::net::UdpSocket;
use tokio::task::JoinSet;
use tracing::{info, warn};

use crate::net::{bind_broadcast, Interface, PORT_ANNOUNCE, PORT_BEAT, PORT_STATUS};
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

        let announce_sock: Arc<UdpSocket> = Arc::new(bind_broadcast(PORT_ANNOUNCE).await?);
        let beat_sock: Arc<UdpSocket> = Arc::new(bind_broadcast(PORT_BEAT).await?);
        let status_sock: Arc<UdpSocket> = Arc::new(bind_broadcast(PORT_STATUS).await?);

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

            let state = Arc::new(PlayerState::new(self.cfg.initial_bpm_hundredths));
            if self.cfg.autoplay {
                state.set_playing(true);
            }

            let track = self
                .cfg
                .tracks
                .get(n as usize - 1)
                .cloned()
                .flatten();
            let cfg = VirtualCdjConfig {
                model_name: self.cfg.player_model.clone(),
                device_number: n,
                iface: self.cfg.iface.clone(),
                mac,
                ip: self.cfg.iface.ipv4.octets(),
                track,
            };
            let cdj = VirtualCdj::new(
                cfg,
                announce_sock.clone(),
                beat_sock.clone(),
                status_sock.clone(),
                state,
            );
            tasks.spawn(async move { cdj.run().await });
        }

        if self.cfg.include_mixer {
            let mut mac = self.cfg.iface.mac;
            mac[5] = mac[5].wrapping_add(33);
            let cfg = VirtualDjmConfig {
                model_name: self.cfg.mixer_model.clone(),
                iface: self.cfg.iface.clone(),
                mac,
                ip: self.cfg.iface.ipv4.octets(),
            };
            let djm = VirtualDjm::new(cfg, announce_sock.clone(), status_sock.clone());
            tasks.spawn(async move { djm.run().await });
        }

        // Passive receiver on :50000 for debugging / future dispatcher.
        {
            let sock = announce_sock.clone();
            tasks.spawn(async move {
                let mut buf = [0u8; 1500];
                loop {
                    match sock.recv_from(&mut buf).await {
                        Ok((n, src)) => {
                            tracing::trace!(bytes = n, from = %src, "rx :50000");
                        }
                        Err(e) => {
                            warn!("announce recv: {e}");
                            return Err(e.into());
                        }
                    }
                }
            });
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
