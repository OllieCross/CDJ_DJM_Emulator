//! A single virtual CDJ device on the network.
//!
//! Lifecycle:
//!
//! 1. **Claim** three stages on :50000 (3x each at 300 ms) - see
//!    [`cdj_proto::claim`].
//! 2. **Steady state** - concurrent tasks:
//!    * keep-alive broadcast on :50000 every 1.5 s
//!    * CDJ status on :50002 every 200 ms, from the [`Deck`]
//!    * a beat packet on :50001 for every beat the deck plays, sent when
//!      that beat is heard
//!    * absolute position on :50001 every 30 ms while a track is loaded, as
//!      CDJ-3000s send
//!
//! The rig stops a device by aborting its `run` task; dropping the task set
//! aborts the loops above.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use cdj_proto::{
    claim::{ClaimStage1, ClaimStage2, ClaimStage3, CLAIM_PACKET_SPACING_MS, CLAIM_REPEATS},
    ActiveLoop, Beat, CdjStatus, DeviceName, KeepAlive, PlayState1, PlayState3, PrecisePosition,
    TrackSourceSlot, TrackType, PITCH_NEUTRAL,
};
use tokio::task::JoinSet;
use tokio::time;
use tracing::{debug, info, trace, warn};

use crate::deck::{Deck, DeckView};
use crate::link::{DeviceLink, Port};
use crate::media::{PlayerMedia, Slot};
use crate::models::PlayerModel;
use crate::rig::RigShared;

pub const KEEPALIVE_INTERVAL: Duration = Duration::from_millis(1500);
pub const STATUS_INTERVAL: Duration = Duration::from_millis(200);
pub const POSITION_INTERVAL: Duration = Duration::from_millis(30);

#[derive(Debug, Clone)]
pub struct VirtualCdjConfig {
    pub model: PlayerModel,
    pub device_number: u8,
    pub mac: [u8; 6],
}

pub struct VirtualCdj {
    cfg: VirtualCdjConfig,
    link: Arc<DeviceLink>,
    deck: Arc<Deck>,
    rig: Arc<RigShared>,
}

impl VirtualCdj {
    pub(crate) fn new(cfg: VirtualCdjConfig, link: Arc<DeviceLink>, deck: Arc<Deck>, rig: Arc<RigShared>) -> Self {
        Self { cfg, link, deck, rig }
    }

    pub async fn run(self) -> anyhow::Result<()> {
        let model = self.cfg.model;
        let device_name = DeviceName::new(model.device_name())
            .map_err(|e| anyhow::anyhow!("device name invalid: {e}"))?;
        let number = self.cfg.device_number;

        info!(device = model.device_name(), num = number, ip = %self.link.ip, "virtual CDJ starting claim sequence");
        claim(&self.link, &device_name, self.cfg.mac, number).await?;
        info!(num = number, "virtual CDJ online");

        let mut tasks: JoinSet<anyhow::Result<()>> = JoinSet::new();
        tasks.spawn(keep_alive_loop(
            self.link.clone(),
            device_name.clone(),
            number,
            self.cfg.mac,
            self.rig.peers_seen.clone(),
        ));

        {
            let (link, name, deck, rig) = (self.link.clone(), device_name.clone(), self.deck.clone(), self.rig.clone());
            tasks.spawn(async move {
                let mut tick = time::interval(STATUS_INTERVAL);
                tick.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
                let mut counter: u32 = 0;
                loop {
                    tick.tick().await;
                    counter = counter.wrapping_add(1);
                    let media = rig.media(number);
                    let status = current_status(
                        CdjStatus {
                            firmware: model.firmware(),
                            packet_counter: counter,
                            master_handoff_to: rig.remote.yielding_to(number),
                            ..CdjStatus::idle(name.clone(), number)
                        },
                        &deck,
                        &deck.view(),
                        &media,
                        rig.link_media_available(number),
                    );
                    if let Err(e) = link.send_update(Port::Status, &status.encode_len(model.status_len())).await {
                        warn!(num = number, "CDJ status broadcast failed: {e}");
                    }
                    debug!(num = number, counter, "status sent");
                }
            });
        }

        // Only CDJ-3000-generation players send absolute positions.
        if model.sends_position() {
            let (link, name, deck) = (self.link.clone(), device_name.clone(), self.deck.clone());
            tasks.spawn(async move {
                let mut tick = time::interval(POSITION_INTERVAL);
                tick.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
                loop {
                    tick.tick().await;
                    if let Some(position) = current_position(&name, number, &deck.view()) {
                        link.to_peers(Port::Beat, &position.encode()).await;
                    }
                }
            });
        }

        {
            let (link, name, deck) = (self.link.clone(), device_name.clone(), self.deck.clone());
            let mut beats = deck.beat_events();
            tasks.spawn(async move {
                while let Some(beat) = beats.recv().await {
                    time::sleep_until(beat.at.into()).await;
                    let view = deck.view();
                    let pkt = Beat {
                        device_name: name.clone(),
                        device_number: number,
                        bpm_hundredths: view.track_bpm_hundredths,
                        pitch: pitch_word(view.pitch),
                        beat_within_bar: beat.bar_beat,
                    }
                    .encode();
                    if let Err(e) = link.send_update(Port::Beat, &pkt).await {
                        warn!(num = number, "beat send failed: {e}");
                    }
                    trace!(num = number, beat = beat.index, bar = beat.bar_beat, "beat sent");
                }
                Ok(())
            });
        }

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

/// Pitch as status and beat packets carry it: 0x100000 = +0%.
fn pitch_word(pitch: f64) -> u32 {
    (f64::from(PITCH_NEUTRAL) * (1.0 + pitch)).round().max(0.0) as u32
}

/// The three-stage device-number claim, each stage sent 3x at 300 ms.
pub(crate) async fn claim(link: &DeviceLink, name: &DeviceName, mac: [u8; 6], device_number: u8) -> anyhow::Result<()> {
    let spacing = Duration::from_millis(CLAIM_PACKET_SPACING_MS);
    for step in 1..=CLAIM_REPEATS {
        let pkt = ClaimStage1 {
            device_name: name.clone(),
            step,
            mac,
        }
        .encode();
        link.broadcast(Port::Announce, &pkt).await?;
        time::sleep(spacing).await;
    }
    for step in 1..=CLAIM_REPEATS {
        let pkt = ClaimStage2 {
            device_name: name.clone(),
            step,
            mac,
            ip: link.ip.octets(),
            device_number,
            user_assigned: true,
        }
        .encode();
        link.broadcast(Port::Announce, &pkt).await?;
        time::sleep(spacing).await;
    }
    for step in 1..=CLAIM_REPEATS {
        let pkt = ClaimStage3 {
            device_name: name.clone(),
            step,
            device_number,
        }
        .encode();
        link.broadcast(Port::Announce, &pkt).await?;
        time::sleep(spacing).await;
    }
    Ok(())
}

/// Broadcast a keep-alive every [`KEEPALIVE_INTERVAL`] until aborted.
pub(crate) async fn keep_alive_loop(
    link: Arc<DeviceLink>,
    name: DeviceName,
    device_number: u8,
    mac: [u8; 6],
    peers_seen: Arc<AtomicU8>,
) -> anyhow::Result<()> {
    let mut tick = time::interval(KEEPALIVE_INTERVAL);
    tick.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let keepalive = KeepAlive {
            device_name: name.clone(),
            device_number,
            mac,
            ip: link.ip.octets(),
            peer_count: peers_seen.load(Ordering::Relaxed),
        }
        .encode();
        if let Err(e) = link.broadcast(Port::Announce, &keepalive).await {
            warn!(num = device_number, "keep-alive send failed: {e}");
        }
    }
}

/// The status packet for the deck's current state. `base` carries what
/// doesn't come from the deck: who is sending (name, number, firmware) and
/// the packet counter.
fn current_status(
    base: CdjStatus,
    deck: &Deck,
    view: &DeckView,
    media: &PlayerMedia,
    link_media_available: bool,
) -> CdjStatus {
    let loaded = view.loaded.as_ref();
    let moving = view.moving;
    // An empty deck playing (timecode only) still reports tempo and beats.
    let beats_known = loaded.is_some() || moving;
    let play_state3 = match (beats_known, moving, view.reverse) {
        (false, _, _) => PlayState3::NoTrack,
        (true, true, false) => PlayState3::ForwardCdj,
        (true, _, _) => PlayState3::PausedOrReverse,
    };
    CdjStatus {
        active: moving || view.play_state == PlayState1::Loading,
        track_source_player: loaded.map_or(0, |t| t.source_player),
        track_source_slot: loaded.map_or(TrackSourceSlot::NoTrack, |t| t.source_slot.source_slot()),
        track_type: if loaded.is_some() { TrackType::Rekordbox } else { TrackType::NoTrack },
        rekordbox_id: loaded.map_or(0, |t| t.track_id),
        track_number: loaded.and_then(|t| t.library.track_number(t.track_id)).unwrap_or(0),
        usb_loaded: media.get(Slot::Usb).is_some(),
        sd_loaded: media.get(Slot::Sd).is_some(),
        link_media_available,
        play_state1: if loaded.is_none() && moving { PlayState1::Playing } else { view.play_state },
        moving,
        play_state3,
        playing: moving,
        master: deck.master(),
        synced: deck.synced(),
        on_air: deck.on_air(),
        pitch: pitch_word(view.pitch),
        bpm_hundredths: beats_known.then_some(view.track_bpm_hundredths),
        beat_number: beats_known.then(|| view.beat_number()),
        cue_countdown: view.cue_countdown,
        beat_within_bar: if beats_known { view.bar_beat } else { 0 },
        active_loop: view.active_loop.map(|l| ActiveLoop {
            start_ms: l.start_ms as u32,
            end_ms: l.end_ms as u32,
            beats: l.beats.map_or(0, |b| b.round() as u16),
        }),
        ..base
    }
}

/// The absolute-position packet, or `None` when no track is loaded.
fn current_position(name: &DeviceName, number: u8, view: &DeckView) -> Option<PrecisePosition> {
    let loaded = view.loaded.as_ref()?;
    let track = loaded.library.track_by_id(loaded.track_id)?;
    let length_s = view.length_ms.map_or(track.duration_s, |ms| (ms / 1000.0) as u32);
    Some(PrecisePosition {
        device_name: name.clone(),
        device_number: number,
        track_length_s: length_s,
        playhead_ms: view.position_ms.max(0.0) as u32,
        pitch_hundredths_percent: (view.pitch * 10_000.0).round() as i32,
        effective_bpm_tenths: Some((view.effective_bpm() * 10.0).round() as u32),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deck::tests::test_track;
    use crate::engine::AudioEngine;
    use crate::grid::BeatGrid;

    fn name() -> DeviceName {
        DeviceName::new("CDJ-3000").unwrap()
    }

    fn deck(number: u8) -> Deck {
        Deck::new(number, Arc::new(AudioEngine::new()), 12000, crate::deck::HOT_CUES)
    }

    fn status(deck: &Deck, number: u8, link: bool) -> CdjStatus {
        let base = CdjStatus {
            packet_counter: 1,
            ..CdjStatus::idle(name(), number)
        };
        current_status(base, deck, &deck.view(), &PlayerMedia::default(), link)
    }

    #[test]
    fn empty_player_reports_no_track() {
        let d = deck(1);
        let s = status(&d, 1, false);
        assert_eq!(s.play_state1, PlayState1::NoTrack);
        assert_eq!(s.rekordbox_id, 0);
        assert_eq!(s.bpm_hundredths, None);
        assert_eq!(s.beat_number, None);
        assert_eq!(s.pitch, PITCH_NEUTRAL);
        assert!(!s.usb_loaded);
        assert!(current_position(&name(), 1, &d.view()).is_none());
    }

    #[test]
    fn track_loaded_over_the_link_names_its_source_player() {
        let d = deck(2);
        d.load(test_track(), None, BeatGrid::constant(12000, 0.0), &[], Some(60_000.0));
        let s = status(&d, 2, true);
        assert_eq!(s.track_source_player, 1, "clients ask player 1's dbserver");
        assert_eq!(s.track_source_slot, TrackSourceSlot::Usb);
        assert_eq!(s.rekordbox_id, 7);
        assert_eq!(s.track_number, 1);
        assert_eq!(s.bpm_hundredths, Some(12000));
        assert_eq!(s.beat_number, Some(1));
        assert!(!s.usb_loaded, "player 2's own USB slot is empty");
        assert!(s.link_media_available);
    }

    #[test]
    fn pitch_and_flags_reach_the_packets() {
        let d = deck(1);
        d.load(test_track(), None, BeatGrid::constant(12000, 0.0), &[], Some(60_000.0));
        d.set_pitch_fader(0.5); // +5% on the default ±10% range
        d.set_master(true);
        d.set_on_air(true);
        let s = status(&d, 1, false);
        assert!(s.master && s.on_air);
        assert_eq!(s.pitch, 0x10_cccd, "1.05 x 0x100000");

        let p = current_position(&name(), 1, &d.view()).unwrap();
        assert_eq!(p.track_length_s, 60);
        assert_eq!(p.pitch_hundredths_percent, 500);
        assert_eq!(p.effective_bpm_tenths, Some(1260));
    }
}
