//! A single virtual CDJ device.
//!
//! Lifecycle:
//!
//! 1. **Claim** three stages on :50000 (3x each at 300 ms) - see
//!    [`cdj_proto::claim`].
//! 2. **Steady state** - concurrent tasks:
//!    * keep-alive broadcast on :50000 every 1.5 s
//!    * CDJ status broadcast on :50002 every ~200 ms, reflecting [`PlayerState`]
//!    * [`BeatClock`] on :50001, emitting beat packets phase-locked to BPM
//!      whenever `PlayerState::playing()` is true
//!    * (optional) audio playback of a loaded track, advancing
//!      `PlayerState::playhead_frames` so the beat clock can phase-lock in
//!      later milestones.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use cdj_proto::{
    claim::{ClaimStage1, ClaimStage2, ClaimStage3, CLAIM_PACKET_SPACING_MS, CLAIM_REPEATS},
    CdjStatus, DeviceName, KeepAlive,
};
use tokio::net::UdpSocket;
use tokio::task::JoinSet;
use tokio::time;
use tracing::{debug, info};

use crate::audio::AudioHandle;
use crate::beat_clock::BeatClock;
use crate::net::{broadcast_addr, Interface, PORT_ANNOUNCE, PORT_STATUS};
use crate::player_state::PlayerState;

pub const KEEPALIVE_INTERVAL: Duration = Duration::from_millis(1500);
pub const STATUS_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Debug, Clone)]
pub struct VirtualCdjConfig {
    pub model_name: String,
    pub device_number: u8,
    pub iface: Interface,
    pub mac: [u8; 6],
    pub ip: [u8; 4],
    /// Optional pre-loaded track path. If `Some`, the audio engine starts on
    /// run() and `PlayerState::set_playing(true)` is called.
    pub track: Option<PathBuf>,
}

pub struct VirtualCdj {
    cfg: VirtualCdjConfig,
    announce_sock: Arc<UdpSocket>,
    beat_sock: Arc<UdpSocket>,
    status_sock: Arc<UdpSocket>,
    state: Arc<PlayerState>,
}

impl VirtualCdj {
    pub fn new(
        cfg: VirtualCdjConfig,
        announce_sock: Arc<UdpSocket>,
        beat_sock: Arc<UdpSocket>,
        status_sock: Arc<UdpSocket>,
        state: Arc<PlayerState>,
    ) -> Self {
        Self {
            cfg,
            announce_sock,
            beat_sock,
            status_sock,
            state,
        }
    }

    pub fn state(&self) -> Arc<PlayerState> {
        self.state.clone()
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
        info!(num = self.cfg.device_number, "virtual CDJ online");

        // The audio engine owns a dedicated std::thread that holds the
        // (!Send) CPAL stream. Spawning is blocking because decode + stream
        // open can be slow; tokio::spawn_blocking lets us await without
        // stalling the tokio runtime.
        let audio_guard = if let Some(path) = self.cfg.track.clone() {
            let state = self.state.clone();
            let num = self.cfg.device_number;
            let handle = tokio::task::spawn_blocking(move || {
                AudioHandle::spawn(path, state)
            })
            .await
            .map_err(|e| anyhow::anyhow!("audio task join: {e}"))??;
            self.state.set_playing(true);
            info!(num, "audio engine started");
            Some(handle)
        } else {
            None
        };

        let mut tasks: JoinSet<anyhow::Result<()>> = JoinSet::new();

        {
            let sock = self.announce_sock.clone();
            let dest = broadcast_addr(&self.cfg.iface, PORT_ANNOUNCE);
            let keepalive = KeepAlive {
                device_name: device_name.clone(),
                device_number: self.cfg.device_number,
                mac: self.cfg.mac,
                ip: self.cfg.ip,
            }
            .encode();
            tasks.spawn(async move {
                let mut tick = time::interval(KEEPALIVE_INTERVAL);
                tick.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
                loop {
                    tick.tick().await;
                    if let Err(e) = sock.send_to(&keepalive, dest).await {
                        tracing::warn!("keep-alive send failed: {e}");
                    }
                }
            });
        }

        {
            let sock = self.status_sock.clone();
            let dest = broadcast_addr(&self.cfg.iface, PORT_STATUS);
            let name = device_name.clone();
            let num = self.cfg.device_number;
            let state = self.state.clone();
            tasks.spawn(async move {
                let mut tick = time::interval(STATUS_INTERVAL);
                tick.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
                let mut ctr: u32 = 0;
                loop {
                    tick.tick().await;
                    ctr = ctr.wrapping_add(1);
                    let status = CdjStatus {
                        device_name: name.clone(),
                        device_number: num,
                        bpm_hundredths: state.bpm_hundredths(),
                        playing: state.playing(),
                        master: state.master(),
                        on_air: state.on_air(),
                        packet_counter: ctr,
                        beat_within_bar: state.beat_within_bar(),
                    };
                    let bytes = status.encode();
                    if let Err(e) = sock.send_to(&bytes, dest).await {
                        tracing::warn!("CDJ status send failed: {e}");
                    }
                    debug!(num, ctr, "status sent");
                }
            });
        }

        {
            let clock = BeatClock {
                device_name: device_name.clone(),
                device_number: self.cfg.device_number,
                state: self.state.clone(),
                iface: self.cfg.iface.clone(),
                socket: self.beat_sock.clone(),
            };
            tasks.spawn(async move { clock.run().await });
        }

        let result = loop {
            match tasks.join_next().await {
                Some(Ok(Ok(()))) => {}
                Some(Ok(Err(e))) => break Err(e),
                Some(Err(e)) => break Err(anyhow::anyhow!("task panicked: {e}")),
                None => break Ok(()),
            }
        };

        drop(audio_guard);
        result
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
