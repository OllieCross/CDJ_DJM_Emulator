//! A single virtual CDJ device.
//!
//! Runs the Pro DJ Link lifecycle for one player:
//!
//! 1. **Claim** three stages on :50000 (3× each at 300 ms) - see
//!    [`cdj_proto::claim`].
//! 2. **Steady state** - broadcast keep-alive on :50000 every 1.5 s and CDJ
//!    status on :50002 every ~200 ms.
//!
//! The device is cancellation-safe: drop the returned `JoinHandle` task to
//! stop it.

use std::sync::Arc;
use std::time::Duration;

use cdj_proto::{
    claim::{ClaimStage1, ClaimStage2, ClaimStage3, CLAIM_PACKET_SPACING_MS, CLAIM_REPEATS},
    CdjStatus, DeviceName, KeepAlive,
};
use tokio::net::UdpSocket;
use tokio::time;
use tracing::{debug, info};

use crate::net::{broadcast_addr, Interface, PORT_ANNOUNCE, PORT_STATUS};

pub const KEEPALIVE_INTERVAL: Duration = Duration::from_millis(1500);
pub const STATUS_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Debug, Clone)]
pub struct VirtualCdjConfig {
    pub model_name: String,
    pub device_number: u8,
    pub iface: Interface,
    /// Per-device MAC override. Lets one process host several virtual CDJs on
    /// the same physical NIC with distinct link-layer identities in packets.
    pub mac: [u8; 6],
    pub ip: [u8; 4],
}

pub struct VirtualCdj {
    cfg: VirtualCdjConfig,
    announce_sock: Arc<UdpSocket>,
    status_sock: Arc<UdpSocket>,
}

impl VirtualCdj {
    pub fn new(
        cfg: VirtualCdjConfig,
        announce_sock: Arc<UdpSocket>,
        status_sock: Arc<UdpSocket>,
    ) -> Self {
        Self {
            cfg,
            announce_sock,
            status_sock,
        }
    }

    pub async fn run(self) -> anyhow::Result<()> {
        let device_name = DeviceName::new(&self.cfg.model_name)
            .map_err(|e| anyhow::anyhow!("device name invalid: {e}"))?;

        info!(
            device = %self.cfg.model_name,
            num = self.cfg.device_number,
            ip = ?self.cfg.ip,
            "virtual CDJ starting claim sequence"
        );

        self.run_claim(&device_name).await?;

        let announce_dest = broadcast_addr(&self.cfg.iface, PORT_ANNOUNCE);
        let status_dest = broadcast_addr(&self.cfg.iface, PORT_STATUS);
        let keepalive = KeepAlive {
            device_name: device_name.clone(),
            device_number: self.cfg.device_number,
            mac: self.cfg.mac,
            ip: self.cfg.ip,
        }
        .encode();

        let mut status = CdjStatus::idle(device_name, self.cfg.device_number);

        let mut keepalive_tick = time::interval(KEEPALIVE_INTERVAL);
        keepalive_tick.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
        let mut status_tick = time::interval(STATUS_INTERVAL);
        status_tick.set_missed_tick_behavior(time::MissedTickBehavior::Delay);

        info!(num = self.cfg.device_number, "virtual CDJ online");

        loop {
            tokio::select! {
                _ = keepalive_tick.tick() => {
                    if let Err(e) = self.announce_sock.send_to(&keepalive, announce_dest).await {
                        tracing::warn!("keep-alive send failed: {e}");
                    }
                }
                _ = status_tick.tick() => {
                    status.packet_counter = status.packet_counter.wrapping_add(1);
                    let bytes = status.encode();
                    if let Err(e) = self.status_sock.send_to(&bytes, status_dest).await {
                        tracing::warn!("CDJ status send failed: {e}");
                    }
                    debug!(num = self.cfg.device_number, ctr = status.packet_counter, "status sent");
                }
            }
        }
    }

    async fn run_claim(&self, name: &DeviceName) -> anyhow::Result<()> {
        let dest = broadcast_addr(&self.cfg.iface, PORT_ANNOUNCE);
        let spacing = Duration::from_millis(CLAIM_PACKET_SPACING_MS);

        for step in 1..=CLAIM_REPEATS {
            let pkt = ClaimStage1 {
                device_name: name.clone(),
                step,
                mac: self.cfg.mac,
            }
            .encode();
            self.announce_sock.send_to(&pkt, dest).await?;
            time::sleep(spacing).await;
        }
        for step in 1..=CLAIM_REPEATS {
            let pkt = ClaimStage2 {
                device_name: name.clone(),
                step,
                mac: self.cfg.mac,
                ip: self.cfg.ip,
                device_number: self.cfg.device_number,
                user_assigned: true,
            }
            .encode();
            self.announce_sock.send_to(&pkt, dest).await?;
            time::sleep(spacing).await;
        }
        for step in 1..=CLAIM_REPEATS {
            let pkt = ClaimStage3 {
                device_name: name.clone(),
                step,
                device_number: self.cfg.device_number,
            }
            .encode();
            self.announce_sock.send_to(&pkt, dest).await?;
            time::sleep(spacing).await;
        }
        Ok(())
    }
}
