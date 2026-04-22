//! Fleet orchestrator: run N virtual CDJs + 1 virtual DJM in a single process.
//!
//! All devices share two UDP sockets (one per port, `SO_REUSEADDR` + broadcast
//! enabled) rather than each device binding its own. This avoids
//! `EADDRINUSE` on :50000/:50002 and keeps the broadcast-reception story
//! simple. Packets still carry per-device MAC and IP in their payloads, which
//! is what the Pro DJ Link protocol actually keys off.

use std::sync::Arc;

use tokio::net::UdpSocket;
use tokio::task::JoinSet;
use tracing::{info, warn};

use crate::net::{bind_broadcast, Interface, PORT_ANNOUNCE, PORT_STATUS};
use crate::virtual_cdj::{VirtualCdj, VirtualCdjConfig};
use crate::virtual_djm::{VirtualDjm, VirtualDjmConfig};

#[derive(Debug, Clone)]
pub struct FleetConfig {
    pub iface: Interface,
    /// Number of virtual CDJs (1..=4). Device numbers assigned sequentially.
    pub num_players: u8,
    /// Include a virtual DJM (device 33) in addition to the players.
    pub include_mixer: bool,
    /// Model string for players.
    pub player_model: String,
    /// Model string for the mixer.
    pub mixer_model: String,
}

impl FleetConfig {
    pub fn default_four_plus_mixer(iface: Interface) -> Self {
        Self {
            iface,
            num_players: 4,
            include_mixer: true,
            player_model: "CDJ-3000".to_string(),
            mixer_model: "DJM-V10".to_string(),
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

        let announce_sock: Arc<UdpSocket> = Arc::new(bind_broadcast(PORT_ANNOUNCE).await?);
        let status_sock: Arc<UdpSocket> = Arc::new(bind_broadcast(PORT_STATUS).await?);

        info!(
            iface = %self.cfg.iface.name,
            ip = %self.cfg.iface.ipv4,
            players = self.cfg.num_players,
            mixer = self.cfg.include_mixer,
            "fleet starting"
        );

        let mut tasks = JoinSet::new();

        for n in 1..=self.cfg.num_players {
            // Synthesise per-device MAC by perturbing the interface MAC's
            // low byte. Keeps real OUI prefix intact so the fleet looks
            // LAN-local, but each virtual CDJ has a distinct link-layer
            // identity in its packets.
            let mut mac = self.cfg.iface.mac;
            mac[5] = mac[5].wrapping_add(n);
            let cfg = VirtualCdjConfig {
                model_name: self.cfg.player_model.clone(),
                device_number: n,
                iface: self.cfg.iface.clone(),
                mac,
                ip: self.cfg.iface.ipv4.octets(),
            };
            let cdj = VirtualCdj::new(cfg, announce_sock.clone(), status_sock.clone());
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

        // Also spawn a passive receiver on :50000 that logs whatever comes in.
        // Useful for debugging and will later dispatch to per-device handlers.
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
