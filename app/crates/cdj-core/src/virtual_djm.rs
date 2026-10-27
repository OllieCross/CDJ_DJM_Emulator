//! A single virtual DJM mixer (device number 33).
//!
//! Every 200 ms it sends mixer status on :50002 (passing along the tempo
//! master's BPM and beat, as real mixers do) and a channels-on-air packet on
//! :50001 so players and clients see which decks are audible.

use std::sync::Arc;

use cdj_proto::{announce::MIXER_NUM, ChannelsOnAir, DeviceName, DjmStatus};
use tokio::task::JoinSet;
use tokio::time;
use tracing::{debug, info, warn};

use crate::link::{DeviceLink, Port};
use crate::rig::RigShared;
use crate::virtual_cdj::{claim, keep_alive_loop, STATUS_INTERVAL};

#[derive(Debug, Clone)]
pub struct VirtualDjmConfig {
    pub model_name: String,
    pub mac: [u8; 6],
    /// Mixer channels: 4, or 6 for the DJM-V10.
    pub channels: u8,
}

pub struct VirtualDjm {
    cfg: VirtualDjmConfig,
    link: Arc<DeviceLink>,
    rig: Arc<RigShared>,
}

impl VirtualDjm {
    pub(crate) fn new(
        cfg: VirtualDjmConfig,
        link: Arc<DeviceLink>,
        rig: Arc<RigShared>,
    ) -> Self {
        Self { cfg, link, rig }
    }

    pub async fn run(self) -> anyhow::Result<()> {
        let device_name = DeviceName::new(&self.cfg.model_name)
            .map_err(|e| anyhow::anyhow!("device name invalid: {e}"))?;

        info!(device = %self.cfg.model_name, num = MIXER_NUM, ip = %self.link.ip, "virtual DJM starting claim sequence");
        claim(&self.link, &device_name, self.cfg.mac, MIXER_NUM).await?;
        info!(num = MIXER_NUM, "virtual DJM online");

        let mut tasks: JoinSet<anyhow::Result<()>> = JoinSet::new();
        tasks.spawn(keep_alive_loop(
            self.link.clone(),
            device_name.clone(),
            MIXER_NUM,
            self.cfg.mac,
            self.rig.peers_seen.clone(),
        ));

        let link = self.link.clone();
        let rig = self.rig.clone();
        let channels = self.cfg.channels;
        tasks.spawn(async move {
            let mut tick = time::interval(STATUS_INTERVAL);
            tick.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                rig.refresh_mixer();

                // Mixers pass along the tempo master's on-screen BPM and beat.
                let master = rig.tempo_master().map(|d| d.view());
                let status = DjmStatus {
                    bpm_hundredths: master
                        .as_ref()
                        .map_or(12000, |v| (v.effective_bpm() * 100.0).round() as u16),
                    beat_within_bar: master.as_ref().map_or(1, |v| v.bar_beat.max(1)),
                    ..DjmStatus::idle(device_name.clone())
                };
                if let Err(e) = link.send_update(Port::Status, &status.encode()).await {
                    warn!("DJM status send failed: {e}");
                }

                let on_air = ChannelsOnAir {
                    device_name: device_name.clone(),
                    device_number: MIXER_NUM,
                    channels: rig.channels_on_air(channels),
                };
                if let Err(e) = link.send_update(Port::Beat, &on_air.encode()).await {
                    warn!("DJM on-air send failed: {e}");
                }
                debug!("DJM status sent");
            }
        });

        loop {
            match tasks.join_next().await {
                Some(Ok(Ok(()))) => {}
                Some(Ok(Err(e))) => return Err(e),
                Some(Err(e)) => return Err(anyhow::anyhow!("task panicked: {e}")),
                None => return Ok(()),
            }
        }
    }
}
