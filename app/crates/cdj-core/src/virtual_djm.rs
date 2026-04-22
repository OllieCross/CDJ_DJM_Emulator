//! A single virtual DJM mixer (device number 33).
//!
//! Mirrors [`crate::virtual_cdj::VirtualCdj`] but emits DJM status packets on
//! :50002 rather than CDJ status.

use std::sync::Arc;
use std::time::Duration;

use cdj_proto::{
    announce::MIXER_NUM,
    claim::{ClaimStage1, ClaimStage2, ClaimStage3, CLAIM_PACKET_SPACING_MS, CLAIM_REPEATS},
    DeviceName, DjmStatus, KeepAlive,
};
use tokio::net::UdpSocket;
use tokio::time;
use tracing::{debug, info};

use crate::net::{broadcast_addr, Interface, PORT_ANNOUNCE, PORT_STATUS};

pub const KEEPALIVE_INTERVAL: Duration = Duration::from_millis(1500);
pub const STATUS_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Debug, Clone)]
pub struct VirtualDjmConfig {
    pub model_name: String,
    pub iface: Interface,
    pub mac: [u8; 6],
    pub ip: [u8; 4],
}

pub struct VirtualDjm {
    cfg: VirtualDjmConfig,
    announce_sock: Arc<UdpSocket>,
    status_sock: Arc<UdpSocket>,
}

impl VirtualDjm {
    pub fn new(
        cfg: VirtualDjmConfig,
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

        info!(device = %self.cfg.model_name, num = MIXER_NUM, "virtual DJM starting claim sequence");

        self.run_claim(&device_name).await?;

        let announce_dest = broadcast_addr(&self.cfg.iface, PORT_ANNOUNCE);
        let status_dest = broadcast_addr(&self.cfg.iface, PORT_STATUS);
        let keepalive = KeepAlive {
            device_name: device_name.clone(),
            device_number: MIXER_NUM,
            mac: self.cfg.mac,
            ip: self.cfg.ip,
        }
        .encode();

        let status = DjmStatus::idle(device_name);

        let mut keepalive_tick = time::interval(KEEPALIVE_INTERVAL);
        keepalive_tick.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
        let mut status_tick = time::interval(STATUS_INTERVAL);
        status_tick.set_missed_tick_behavior(time::MissedTickBehavior::Delay);

        info!(num = MIXER_NUM, "virtual DJM online");

        loop {
            tokio::select! {
                _ = keepalive_tick.tick() => {
                    if let Err(e) = self.announce_sock.send_to(&keepalive, announce_dest).await {
                        tracing::warn!("DJM keep-alive send failed: {e}");
                    }
                }
                _ = status_tick.tick() => {
                    let bytes = status.encode();
                    if let Err(e) = self.status_sock.send_to(&bytes, status_dest).await {
                        tracing::warn!("DJM status send failed: {e}");
                    }
                    debug!("DJM status sent");
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
                device_number: MIXER_NUM,
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
                device_number: MIXER_NUM,
            }
            .encode();
            self.announce_sock.send_to(&pkt, dest).await?;
            time::sleep(spacing).await;
        }
        Ok(())
    }
}
