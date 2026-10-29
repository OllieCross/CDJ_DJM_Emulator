//! The rig: up to four virtual CDJs and one virtual DJM, spawned and
//! despawned at runtime, sharing one audio engine.
//!
//! Each running device is a set of tokio tasks (protocol loops, dbserver);
//! despawning aborts them, which is what a real unit does when it is
//! switched off: its packets stop and clients time it out.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::Duration;

use anyhow::{bail, Context};
use cdj_proto::announce::MIXER_NUM;
use cdj_proto::DeviceName;
use tokio::task::AbortHandle;
use tracing::{error, info, warn};

use crate::dbserver::{DbServer, DbServerConfig};
use crate::deck::{Deck, LoadedTrack, TempoRange};
use crate::engine::{AudioEngine, MIX_CHANNELS};
use crate::grid::BeatGrid;
use crate::link::{DeviceLink, Topology};
use crate::media::{Medium, PlayerMedia, Slot};
use crate::mixer::{CrossfaderAssign, MixerControls, MAX_CHANNELS};
use crate::models::{MixerModel, PlayerModel};
use crate::remote::{ListenPorts, Remote, RemoteView};
use crate::track_audio::TrackAudio;
use crate::virtual_cdj::{VirtualCdj, VirtualCdjConfig};
use crate::virtual_djm::{VirtualDjm, VirtualDjmConfig};

pub const MAX_PLAYERS: u8 = 4;
/// How often synced players follow the tempo master.
const SYNC_INTERVAL: Duration = Duration::from_millis(50);

/// Which source address each device sends from.
#[derive(Debug, Clone, Copy)]
pub enum Addressing {
    /// One address per device in this /24: player N uses `.N`, the mixer
    /// `.33` (the aliases the feth setup creates). Clients need distinct
    /// addresses to tell devices apart.
    Subnet([u8; 3]),
    /// Every device uses this address. Only for tests and debugging.
    Fixed(Ipv4Addr),
}

#[derive(Debug, Clone)]
pub struct RigConfig {
    pub topology: Topology,
    pub addressing: Addressing,
    /// Tempo of an empty deck (timecode without a track).
    pub initial_bpm_hundredths: u16,
    /// Where beat 1 falls in files played without rekordbox analysis.
    pub beat_grid_offset_ms: u32,
    /// Ports players take commands from client apps on (see
    /// [`crate::remote`]); `None` to ignore commands.
    pub listen_ports: Option<ListenPorts>,
}

impl RigConfig {
    pub fn device_ip(&self, device_number: u8) -> Ipv4Addr {
        match self.addressing {
            Addressing::Subnet([a, b, c]) => Ipv4Addr::new(a, b, c, device_number),
            Addressing::Fixed(ip) => ip,
        }
    }
}

/// State the devices read from each other: the running decks, which media
/// is in which slot, the mixer controls, and how many devices are on the
/// network.
pub(crate) struct RigShared {
    players: RwLock<BTreeMap<u8, Arc<Deck>>>,
    models: RwLock<BTreeMap<u8, PlayerModel>>,
    /// Slots of players 1..=4. Media stays in a player while it is off.
    media: BTreeMap<u8, Arc<PlayerMedia>>,
    mixer: Mutex<MixerControls>,
    mixer_running: AtomicBool,
    engine: Arc<AudioEngine>,
    pub(crate) peers_seen: Arc<AtomicU8>,
    pub(crate) remote: Remote,
}

impl RigShared {
    pub(crate) fn deck(&self, n: u8) -> anyhow::Result<Arc<Deck>> {
        self.players
            .read()
            .unwrap()
            .get(&n)
            .cloned()
            .with_context(|| format!("player {n} is not running"))
    }

    pub(crate) fn player_numbers(&self) -> Vec<u8> {
        self.players.read().unwrap().keys().copied().collect()
    }

    /// Whether `device_number` is one of our running devices.
    pub(crate) fn is_own_device(&self, device_number: u8) -> bool {
        self.players.read().unwrap().contains_key(&device_number)
            || (device_number == MIXER_NUM && self.mixer_running.load(Ordering::Relaxed))
    }

    /// The name player `n` announces itself with.
    pub(crate) fn device_name(&self, n: u8) -> DeviceName {
        let model = self.models.read().unwrap().get(&n).copied().unwrap_or(PlayerModel::Cdj3000);
        DeviceName::new(model.device_name()).expect("model names fit")
    }

    pub(crate) fn media(&self, player: u8) -> Arc<PlayerMedia> {
        self.media.get(&player).cloned().unwrap_or_default()
    }

    /// Whether another running player has media this one can load over
    /// the link.
    pub(crate) fn link_media_available(&self, player: u8) -> bool {
        self.players
            .read()
            .unwrap()
            .keys()
            .any(|&n| n != player && !self.media(n).is_empty())
    }

    /// Apply the mixer to the decks and the audio: each player's on-air
    /// flag (as players set it from the mixer's channels-on-air packet) and
    /// each channel's gains. Without a mixer nothing is on air and the decks
    /// play straight out.
    pub(crate) fn refresh_mixer(&self) {
        let mixer = self.mixer.lock().unwrap().clone();
        let running = self.mixer_running.load(Ordering::Relaxed);
        for (&n, deck) in self.players.read().unwrap().iter() {
            deck.set_on_air(running && mixer.on_air(n));
        }
        let gains = self.engine.gains();
        for (i, ch) in gains.channels.iter().enumerate().take(MIX_CHANNELS) {
            ch.level.set(if running { mixer.channel_gain(i) } else { 1.0 });
            let eq = if running { mixer.eq_gains(i) } else { [1.0; 3] };
            for (band, g) in ch.eq.iter().zip(eq) {
                band.set(g);
            }
        }
        gains.master.set(if running { mixer.master } else { 1.0 });
    }

    pub(crate) fn tempo_master(&self) -> Option<Arc<Deck>> {
        self.players.read().unwrap().values().find(|d| d.master()).cloned()
    }

    /// Channels the mixer reports on air: open, with a player switched on
    /// behind them (an empty channel has nothing to put on air).
    pub(crate) fn channels_on_air(&self, channels: u8) -> Vec<bool> {
        let mixer = self.mixer.lock().unwrap().clone();
        let players = self.players.read().unwrap();
        (1..=channels).map(|ch| mixer.on_air(ch) && players.contains_key(&ch)).collect()
    }

    /// Synced players play at the tempo master's BPM, whether the master is
    /// one of ours or a client app.
    fn follow_master(&self) {
        let bpm = match self.tempo_master() {
            Some(master) => master.view().effective_bpm(),
            None => match self.remote.external_master().and_then(|m| m.bpm) {
                Some(bpm) => bpm,
                None => return,
            },
        };
        for deck in self.players.read().unwrap().values() {
            if deck.synced() && !deck.master() && deck.loaded().is_some() {
                deck.follow_bpm(bpm);
            }
        }
    }

    /// Put a synced player on the tempo master's tempo and beat.
    fn sync_to_master(&self, deck: &Arc<Deck>, align: bool) {
        if let Some(m) = self.tempo_master().filter(|m| !Arc::ptr_eq(m, deck)) {
            deck.follow_bpm(m.view().effective_bpm());
            if align && m.is_playing() {
                deck.align_phase(m.beat_phase());
            }
        } else if self.tempo_master().is_none() {
            if let Some(bpm) = self.remote.external_master().and_then(|m| m.bpm) {
                deck.follow_bpm(bpm);
                if let Some(phase) = self.remote.master_phase().filter(|_| align) {
                    deck.align_phase(phase);
                }
            }
        }
    }

    /// See [`Rig::play`].
    pub(crate) fn play(&self, n: u8) -> anyhow::Result<()> {
        let deck = self.deck(n)?;
        if deck.synced() && !deck.master() {
            self.sync_to_master(&deck, true);
        }
        if self.tempo_master().is_none() && self.remote.external_master().is_none() {
            deck.set_master(true);
        }
        deck.play();
        Ok(())
    }

    /// See [`Rig::set_synced`].
    pub(crate) fn set_synced(&self, n: u8, synced: bool) -> anyhow::Result<()> {
        let deck = self.deck(n)?;
        deck.set_synced(synced);
        if synced && !deck.master() {
            self.sync_to_master(&deck, deck.is_playing());
        }
        Ok(())
    }

    /// See [`Rig::set_master`].
    pub(crate) fn become_master(&self, n: u8) -> anyhow::Result<()> {
        let deck = self.deck(n)?;
        if deck.master() {
            return Ok(());
        }
        match self.remote.external_master() {
            Some(master) => self.remote.request_master(n, &self.device_name(n), &master),
            None => self.take_master(n),
        }
        Ok(())
    }

    /// Player `n` becomes tempo master; whoever had the role among ours
    /// gives it up.
    pub(crate) fn take_master(&self, n: u8) {
        let players = self.players.read().unwrap();
        let Some(deck) = players.get(&n) else {
            return;
        };
        for d in players.values() {
            d.set_master(false);
        }
        deck.set_master(true);
        // The master leads; it doesn't follow anyone.
        deck.set_synced(false);
    }

    /// See [`Rig::load_track`].
    pub(crate) async fn load_track(&self, n: u8, source_player: u8, slot: Slot, track_id: u32) -> anyhow::Result<()> {
        let deck = self.deck(n)?;
        if source_player != n && !self.players.read().unwrap().contains_key(&source_player) {
            bail!("player {source_player} is off, so its media can't be reached over the link");
        }
        let medium = self
            .media(source_player)
            .get(slot)
            .with_context(|| format!("player {source_player}'s {slot:?} slot is empty"))?;
        let library = medium.library.clone();
        let track = library
            .track_by_id(track_id)
            .with_context(|| format!("track {track_id} is not on {}", medium.name))?
            .clone();

        // Reading the analysis and starting the decoder touch the disk.
        let (audio, grid, cues) = tokio::task::spawn_blocking({
            let library = library.clone();
            let track = track.clone();
            move || {
                let audio = if track.audio_path.exists() {
                    TrackAudio::open(&track.audio_path)
                        .map_err(|e| warn!(track_id, "audio unavailable, metadata-only deck: {e:#}"))
                        .ok()
                } else {
                    warn!(track_id, path = %track.audio_path.display(), "audio file missing, metadata-only deck");
                    None
                };
                let grid = library
                    .beats_for(&track)
                    .map(|beats| BeatGrid::from_analysis(&beats))
                    .unwrap_or_else(|_| BeatGrid::constant(track.bpm_hundredths, 0.0));
                let cues = library.cues_for(&track).unwrap_or_default();
                (audio, grid, cues)
            }
        })
        .await
        .context("loading track panicked")?;

        deck.load(
            LoadedTrack {
                library,
                track_id,
                source_player,
                source_slot: slot,
            },
            audio,
            grid,
            &cues,
            Some(f64::from(track.duration_s) * 1000.0),
        );
        Ok(())
    }
}

/// Handle the UI and CLI use to drive one player.
#[derive(Clone)]
pub struct PlayerHandle {
    pub player_number: u8,
    pub model: PlayerModel,
    pub deck: Arc<Deck>,
}

/// Tasks of one running device; dropping it aborts them.
struct Running(Vec<AbortHandle>);

impl Drop for Running {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

fn spawn_logged<F>(label: String, fut: F) -> AbortHandle
where
    F: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
{
    tokio::spawn(async move {
        if let Err(e) = fut.await {
            error!("{label} stopped: {e:#}");
        }
    })
    .abort_handle()
}

pub struct Rig {
    cfg: RigConfig,
    mac: [u8; 6],
    shared: Arc<RigShared>,
    players: Mutex<BTreeMap<u8, (PlayerHandle, Running)>>,
    mixer: Mutex<Option<(MixerModel, Running)>>,
    /// The command listener, started with the first device.
    listener: Mutex<Option<Running>>,
}

impl Rig {
    /// A rig with nothing running. Audio keeps time silently until
    /// [`set_output_device`](Self::set_output_device) picks a device.
    pub fn new(cfg: RigConfig) -> Self {
        let mac = mac_address::mac_address_by_name(&cfg.topology.iface)
            .ok()
            .flatten()
            .map(|m| m.bytes())
            .unwrap_or([0x02, 0x00, 0x00, 0x00, 0x00, 0x00]);
        let engine = Arc::new(AudioEngine::new());
        if let Err(e) = engine.start_silent() {
            warn!("audio engine could not start: {e:#}");
        }
        let shared = Arc::new(RigShared {
            players: RwLock::new(BTreeMap::new()),
            models: RwLock::new(BTreeMap::new()),
            media: (1..=MAX_PLAYERS).map(|n| (n, Arc::default())).collect(),
            mixer: Mutex::new(MixerControls::default()),
            mixer_running: AtomicBool::new(false),
            engine,
            peers_seen: Arc::new(AtomicU8::new(0)),
            remote: Remote::new(),
        });
        shared.refresh_mixer();
        spawn_sync_thread(Arc::downgrade(&shared));
        Self {
            cfg,
            mac,
            shared,
            players: Mutex::new(BTreeMap::new()),
            mixer: Mutex::new(None),
            listener: Mutex::new(None),
        }
    }

    /// Start taking commands from client apps (needs the tokio runtime, so
    /// it starts with the first device rather than in [`Rig::new`]).
    fn ensure_listener(&self) {
        let Some(ports) = self.cfg.listen_ports else {
            return;
        };
        let mut listener = self.listener.lock().unwrap();
        if listener.is_none() {
            let task = tokio::spawn(crate::remote::run(Arc::downgrade(&self.shared), self.cfg.clone(), ports));
            *listener = Some(Running(vec![task.abort_handle()]));
        }
    }

    #[cfg(test)]
    pub(crate) fn shared(&self) -> &Arc<RigShared> {
        &self.shared
    }

    /// Commands from client apps: whether the players are listening, an
    /// outside tempo master, and what was asked lately.
    pub fn remote_view(&self) -> RemoteView {
        self.shared.remote.view()
    }

    pub fn config(&self) -> &RigConfig {
        &self.cfg
    }

    fn device_mac(&self, device_number: u8) -> [u8; 6] {
        let mut mac = self.mac;
        mac[5] = mac[5].wrapping_add(device_number);
        mac
    }

    /// Devices a keep-alive reports seeing: our own plus the client apps.
    fn update_peers_seen(&self) {
        let players = self.players.lock().unwrap().len();
        let mixer = usize::from(self.mixer_running());
        let total = players + mixer + self.cfg.topology.peers.len();
        self.shared.peers_seen.store(total.min(255) as u8, Ordering::Relaxed);
    }

    // ---------- audio output ----------

    /// Play through `device` (`None`: the system default). On failure the
    /// rig keeps time silently and the error says why.
    pub fn set_output_device(&self, device: Option<&str>) -> anyhow::Result<String> {
        self.shared.engine.start_output(device)
    }

    /// Stop playing audio; beats and positions carry on.
    pub fn mute_output(&self) -> anyhow::Result<()> {
        self.shared.engine.start_silent()
    }

    pub fn output_device(&self) -> Option<String> {
        self.shared.engine.device()
    }

    // ---------- lifecycle ----------

    /// Switch on player `n` as `model`.
    pub async fn spawn_player(&self, n: u8, model: PlayerModel) -> anyhow::Result<()> {
        if !(1..=MAX_PLAYERS).contains(&n) {
            bail!("player number must be 1..={MAX_PLAYERS}, got {n}");
        }
        if self.players.lock().unwrap().contains_key(&n) {
            bail!("player {n} is already running");
        }
        let ip = self.cfg.device_ip(n);
        let link = DeviceLink::bind(ip, self.cfg.topology.clone())
            .await
            .with_context(|| format!("binding player {n} to {ip} (is the network set up?)"))?;

        let deck = Arc::new(Deck::new(
            n,
            self.shared.engine.clone(),
            self.cfg.initial_bpm_hundredths,
            model.hot_cues(),
        ));
        let cdj = VirtualCdj::new(
            VirtualCdjConfig {
                model,
                device_number: n,
                mac: self.device_mac(n),
            },
            Arc::new(link),
            deck.clone(),
            self.shared.clone(),
        );
        let db = DbServer::new(
            DbServerConfig {
                device_number: n,
                ip,
                tcp_port: crate::dbserver::DBSERVER_TCP_PORT,
                player_model: model.device_name().into(),
            },
            self.shared.media(n),
        );
        let running = Running(vec![
            spawn_logged(format!("player {n}"), cdj.run()),
            spawn_logged(format!("player {n} dbserver"), db.run()),
        ]);

        let handle = PlayerHandle {
            player_number: n,
            model,
            deck: deck.clone(),
        };
        {
            let mut players = self.players.lock().unwrap();
            if players.contains_key(&n) {
                bail!("player {n} is already running");
            }
            players.insert(n, (handle, running));
        }
        self.shared.models.write().unwrap().insert(n, model);
        self.shared.players.write().unwrap().insert(n, deck);
        self.update_peers_seen();
        self.shared.refresh_mixer();
        self.ensure_listener();
        info!(player = n, model = model.display_name(), %ip, "player spawned");
        Ok(())
    }

    /// Switch a player off. Returns false if it wasn't running.
    pub fn despawn_player(&self, n: u8) -> bool {
        let Some((handle, running)) = self.players.lock().unwrap().remove(&n) else {
            return false;
        };
        drop(running);
        handle.deck.unload();
        handle.deck.set_master(false);
        self.shared.players.write().unwrap().remove(&n);
        self.shared.models.write().unwrap().remove(&n);
        self.update_peers_seen();
        info!(player = n, "player despawned");
        true
    }

    /// Switch on the mixer as `model`.
    pub async fn spawn_mixer(&self, model: MixerModel) -> anyhow::Result<()> {
        if self.mixer_running() {
            bail!("the mixer is already running");
        }
        let ip = self.cfg.device_ip(MIXER_NUM);
        let link = DeviceLink::bind(ip, self.cfg.topology.clone())
            .await
            .with_context(|| format!("binding the mixer to {ip} (is the network set up?)"))?;
        let djm = VirtualDjm::new(
            VirtualDjmConfig {
                model,
                mac: self.device_mac(MIXER_NUM),
            },
            Arc::new(link),
            self.shared.clone(),
        );
        let running = Running(vec![spawn_logged("mixer".into(), djm.run())]);
        {
            let mut mixer = self.mixer.lock().unwrap();
            if mixer.is_some() {
                bail!("the mixer is already running");
            }
            *mixer = Some((model, running));
        }
        self.shared.mixer_running.store(true, Ordering::Relaxed);
        self.update_peers_seen();
        self.shared.refresh_mixer();
        info!(model = model.device_name(), %ip, "mixer spawned");
        Ok(())
    }

    /// Switch the mixer off. Returns false if it wasn't running.
    pub fn despawn_mixer(&self) -> bool {
        let Some((_, running)) = self.mixer.lock().unwrap().take() else {
            return false;
        };
        drop(running);
        self.shared.mixer_running.store(false, Ordering::Relaxed);
        self.update_peers_seen();
        self.shared.refresh_mixer();
        info!("mixer despawned");
        true
    }

    /// Switch everything off.
    pub fn shutdown(&self) {
        let numbers: Vec<u8> = self.players.lock().unwrap().keys().copied().collect();
        for n in numbers {
            self.despawn_player(n);
        }
        self.despawn_mixer();
    }

    pub fn mixer_running(&self) -> bool {
        self.mixer.lock().unwrap().is_some()
    }

    /// The running mixer's model.
    pub fn mixer_model(&self) -> Option<MixerModel> {
        self.mixer.lock().unwrap().as_ref().map(|(m, _)| *m)
    }

    pub fn player(&self, n: u8) -> Option<PlayerHandle> {
        self.players.lock().unwrap().get(&n).map(|(h, _)| h.clone())
    }

    pub fn players(&self) -> Vec<PlayerHandle> {
        self.players.lock().unwrap().values().map(|(h, _)| h.clone()).collect()
    }

    fn deck(&self, n: u8) -> anyhow::Result<Arc<Deck>> {
        self.player(n)
            .map(|h| h.deck)
            .with_context(|| format!("player {n} is not running"))
    }

    // ---------- media ----------

    /// What is in player `n`'s slots.
    pub fn media(&self, n: u8) -> Vec<(Slot, Arc<Medium>)> {
        self.shared.media(n).contents()
    }

    /// Put an export into a player's slot. Players don't need to be on.
    pub fn insert_media(&self, player: u8, slot: Slot, medium: Arc<Medium>) -> anyhow::Result<()> {
        let media = self
            .shared
            .media
            .get(&player)
            .with_context(|| format!("player number must be 1..={MAX_PLAYERS}, got {player}"))?;
        if let Some(existing) = media.get(slot) {
            bail!("player {player}'s {slot:?} slot already holds {}", existing.name);
        }
        for (&n, other) in &self.shared.media {
            if let Some((s, _)) = other.contents().into_iter().find(|(_, m)| m.root == medium.root) {
                bail!("{} is already in player {n}'s {s:?} slot", medium.name);
            }
        }
        info!(player, ?slot, medium = %medium.name, "media inserted");
        media.insert(slot, medium);
        Ok(())
    }

    /// Take an export out of a slot. Players with a track from it loaded
    /// unload it.
    pub fn eject_media(&self, player: u8, slot: Slot) -> anyhow::Result<()> {
        let removed = self
            .shared
            .media
            .get(&player)
            .and_then(|m| m.remove(slot))
            .with_context(|| format!("player {player}'s {slot:?} slot is empty"))?;
        for h in self.players() {
            let from_ejected = h
                .deck
                .loaded()
                .is_some_and(|t| t.source_player == player && t.source_slot == slot);
            if from_ejected {
                h.deck.unload();
            }
        }
        info!(player, ?slot, medium = %removed.name, "media ejected");
        Ok(())
    }

    /// Eject media whose files have gone (the volume was unmounted).
    pub fn eject_missing_media(&self) {
        for (&n, media) in &self.shared.media {
            for (slot, medium) in media.contents() {
                if !crate::library::Library::is_export(&medium.root) {
                    let _ = self.eject_media(n, slot);
                }
            }
        }
    }

    // ---------- loading ----------

    /// Load track `track_id` from the media in `source_player`'s `slot`
    /// onto player `n`, cued and ready. Loading from another player's media
    /// goes over the link, so that player has to be on.
    pub async fn load_track(&self, n: u8, source_player: u8, slot: Slot, track_id: u32) -> anyhow::Result<()> {
        self.shared.load_track(n, source_player, slot, track_id).await
    }

    /// Play a file that isn't on any media (CLI `--track`), on a constant
    /// grid at the rig's tempo.
    pub async fn load_file(&self, n: u8, path: PathBuf) -> anyhow::Result<()> {
        let deck = self.deck(n)?;
        let audio = tokio::task::spawn_blocking(move || TrackAudio::open(&path))
            .await
            .context("loading file panicked")??;
        let grid = BeatGrid::constant(self.cfg.initial_bpm_hundredths, f64::from(self.cfg.beat_grid_offset_ms));
        deck.load_unlisted(audio, grid);
        Ok(())
    }

    pub fn unload_track(&self, n: u8) -> anyhow::Result<()> {
        self.deck(n)?.unload();
        Ok(())
    }

    // ---------- transport ----------

    /// Start playback. With no tempo master on the network, the first player
    /// to play takes the role, as real players do. A synced player lines its
    /// beat up with the master's.
    pub fn play(&self, n: u8) -> anyhow::Result<()> {
        self.shared.play(n)
    }

    pub fn pause(&self, n: u8) -> anyhow::Result<()> {
        self.deck(n)?.pause();
        Ok(())
    }

    pub fn play_pause(&self, n: u8) -> anyhow::Result<()> {
        if self.deck(n)?.is_playing() {
            self.pause(n)
        } else {
            self.play(n)
        }
    }

    pub fn cue_press(&self, n: u8) -> anyhow::Result<()> {
        self.deck(n)?.cue_press();
        Ok(())
    }

    pub fn cue_release(&self, n: u8) -> anyhow::Result<()> {
        self.deck(n)?.cue_release();
        Ok(())
    }

    pub fn seek(&self, n: u8, ms: f64) -> anyhow::Result<()> {
        self.deck(n)?.seek(ms);
        Ok(())
    }

    /// Hot cue pad `pad` (0 = A .. 7 = H).
    pub fn hot_cue(&self, n: u8, pad: usize) -> anyhow::Result<()> {
        self.deck(n)?.hot_cue(pad);
        Ok(())
    }

    pub fn clear_hot_cue(&self, n: u8, pad: usize) -> anyhow::Result<()> {
        self.deck(n)?.clear_hot_cue(pad);
        Ok(())
    }

    pub fn memory_cue(&self, n: u8, forward: bool) -> anyhow::Result<()> {
        self.deck(n)?.memory_cue(forward);
        Ok(())
    }

    pub fn beat_loop(&self, n: u8, beats: f64) -> anyhow::Result<()> {
        self.deck(n)?.beat_loop(beats);
        Ok(())
    }

    pub fn loop_in(&self, n: u8) -> anyhow::Result<()> {
        self.deck(n)?.loop_in();
        Ok(())
    }

    pub fn loop_out(&self, n: u8) -> anyhow::Result<()> {
        self.deck(n)?.loop_out();
        Ok(())
    }

    pub fn exit_loop(&self, n: u8) -> anyhow::Result<()> {
        self.deck(n)?.exit_loop();
        Ok(())
    }

    pub fn reloop(&self, n: u8) -> anyhow::Result<()> {
        self.deck(n)?.reloop();
        Ok(())
    }

    pub fn resize_loop(&self, n: u8, factor: f64) -> anyhow::Result<()> {
        self.deck(n)?.resize_loop(factor);
        Ok(())
    }

    // ---------- tempo ----------

    pub fn set_pitch_fader(&self, n: u8, position: f64) -> anyhow::Result<()> {
        self.deck(n)?.set_pitch_fader(position);
        Ok(())
    }

    pub fn set_tempo_range(&self, n: u8, range: TempoRange) -> anyhow::Result<()> {
        self.deck(n)?.set_tempo_range(range);
        Ok(())
    }

    pub fn set_master_tempo(&self, n: u8, on: bool) -> anyhow::Result<()> {
        self.deck(n)?.set_master_tempo(on);
        Ok(())
    }

    /// Jog-wheel nudge while playing; 0.0 on release.
    pub fn set_bend(&self, n: u8, bend: f64) -> anyhow::Result<()> {
        self.deck(n)?.set_bend(bend);
        Ok(())
    }

    pub fn set_reverse(&self, n: u8, on: bool) -> anyhow::Result<()> {
        self.deck(n)?.set_reverse(on);
        Ok(())
    }

    pub fn set_quantize(&self, n: u8, on: bool) -> anyhow::Result<()> {
        self.deck(n)?.set_quantize(on);
        Ok(())
    }

    /// Make player `n` tempo master; whoever had the role gives it up. When
    /// a client app is master, the player asks it for the role and takes it
    /// once the app agrees, as real players do.
    pub fn set_master(&self, n: u8) -> anyhow::Result<()> {
        self.shared.become_master(n)
    }

    /// Sync on: follow the master's tempo and line up with its beat.
    pub fn set_synced(&self, n: u8, synced: bool) -> anyhow::Result<()> {
        self.shared.set_synced(n, synced)
    }

    // ---------- mixer ----------

    pub fn mixer_controls(&self) -> MixerControls {
        self.shared.mixer.lock().unwrap().clone()
    }

    fn update_mixer(&self, change: impl FnOnce(&mut MixerControls)) {
        change(&mut self.shared.mixer.lock().unwrap());
        self.shared.refresh_mixer();
    }

    pub fn set_channel_fader(&self, channel: u8, level: f32) -> anyhow::Result<()> {
        let i = channel_index(channel)?;
        self.update_mixer(|m| m.channel_faders[i] = level.clamp(0.0, 1.0));
        Ok(())
    }

    pub fn set_crossfader(&self, position: f32) {
        self.update_mixer(|m| m.crossfader = position.clamp(0.0, 1.0));
    }

    pub fn set_crossfader_assign(&self, channel: u8, assign: CrossfaderAssign) -> anyhow::Result<()> {
        let i = channel_index(channel)?;
        self.update_mixer(|m| m.assign[i] = assign);
        Ok(())
    }

    pub fn set_trim(&self, channel: u8, db: f32) -> anyhow::Result<()> {
        let i = channel_index(channel)?;
        self.update_mixer(|m| m.trim_db[i] = db.clamp(crate::mixer::TRIM_MIN_DB, crate::mixer::TRIM_MAX_DB));
        Ok(())
    }

    /// EQ band `band` (0 low, 1 mid, 2 high) of `channel`, in dB.
    pub fn set_eq(&self, channel: u8, band: usize, db: f32) -> anyhow::Result<()> {
        let i = channel_index(channel)?;
        if band > 2 {
            bail!("EQ band must be 0 (low), 1 (mid) or 2 (high)");
        }
        self.update_mixer(|m| m.eq_db[i][band] = db.clamp(crate::mixer::EQ_KILL_DB, crate::mixer::EQ_MAX_DB));
        Ok(())
    }

    pub fn set_master_level(&self, level: f32) {
        self.update_mixer(|m| m.master = level.clamp(0.0, 1.0));
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Keep synced players on the master's tempo until the rig goes away.
fn spawn_sync_thread(shared: Weak<RigShared>) {
    let _ = std::thread::Builder::new()
        .name("cdj-sync".into())
        .spawn(move || {
            while let Some(shared) = shared.upgrade() {
                shared.follow_master();
                drop(shared);
                std::thread::sleep(SYNC_INTERVAL);
            }
        });
}

fn channel_index(channel: u8) -> anyhow::Result<usize> {
    match channel {
        1..=6 if (channel as usize) <= MAX_CHANNELS => Ok(channel as usize - 1),
        _ => bail!("mixer channel must be 1..={MAX_CHANNELS}, got {channel}"),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn rig() -> Rig {
        Rig::new(RigConfig {
            topology: Topology {
                iface: "lo0".into(),
                broadcast: Ipv4Addr::new(127, 255, 255, 255),
                peers: vec![],
            },
            addressing: Addressing::Fixed(Ipv4Addr::LOCALHOST),
            initial_bpm_hundredths: 12000,
            beat_grid_offset_ms: 0,
            listen_ports: None,
        })
    }

    #[test]
    fn subnet_addressing_follows_device_numbers() {
        let mut cfg = rig().config().clone();
        cfg.addressing = Addressing::Subnet([10, 77, 77]);
        assert_eq!(cfg.device_ip(2), Ipv4Addr::new(10, 77, 77, 2));
        assert_eq!(cfg.device_ip(MIXER_NUM), Ipv4Addr::new(10, 77, 77, 33));
    }

    #[test]
    fn controlling_a_player_that_isnt_running_is_an_error() {
        let r = rig();
        assert!(r.play(1).is_err());
        assert!(r.set_master(3).is_err());
        assert!(!r.despawn_player(2));
        assert!(!r.despawn_mixer());
    }

    #[tokio::test]
    async fn spawn_play_and_despawn_players() {
        let r = rig();
        r.spawn_player(1, PlayerModel::Cdj3000).await.unwrap();
        r.spawn_player(2, PlayerModel::Cdj3000).await.unwrap();
        assert!(r.spawn_player(2, PlayerModel::Cdj3000).await.is_err(), "number already taken");
        assert!(r.spawn_player(5, PlayerModel::Cdj3000).await.is_err(), "only 4 players");

        r.play(2).unwrap();
        let p2 = r.player(2).unwrap().deck;
        assert!(p2.is_playing() && p2.master(), "first player to play becomes master");
        r.play(1).unwrap();
        assert!(!r.player(1).unwrap().deck.master(), "master already exists");

        r.set_master(1).unwrap();
        assert!(r.player(1).unwrap().deck.master() && !p2.master());

        assert!(r.despawn_player(2));
        assert!(r.player(2).is_none());
        assert_eq!(r.players().len(), 1);
    }

    #[tokio::test]
    async fn players_and_mixer_run_as_the_chosen_models() {
        let r = rig();
        r.spawn_player(3, PlayerModel::Cdj2000Nxs).await.unwrap();
        let h = r.player(3).unwrap();
        assert_eq!(h.model, PlayerModel::Cdj2000Nxs);
        assert_eq!(h.deck.hot_cue_slots, 3);
        assert_eq!(r.mixer_model(), None);
        r.spawn_mixer(MixerModel::DjmA9).await.unwrap();
        assert_eq!(r.mixer_model(), Some(MixerModel::DjmA9));
        r.despawn_mixer();
        assert_eq!(r.mixer_model(), None);
    }

    #[tokio::test]
    async fn media_slots_and_linked_loading() {
        use crate::library::tests::{demo_export, demo_library};
        let r = rig();
        let usb = Medium::open(&demo_export()).unwrap();
        let lib = demo_library();
        let track = &lib.tracks[0];

        // Media can go into a player that is off; it stays put.
        r.insert_media(1, Slot::Usb, usb.clone()).unwrap();
        assert!(r.insert_media(2, Slot::Sd, usb.clone()).is_err(), "one place at a time");
        assert!(r.insert_media(5, Slot::Usb, usb.clone()).is_err());

        r.spawn_player(2, PlayerModel::Cdj3000).await.unwrap();
        let err = r.load_track(2, 1, Slot::Usb, track.id).await.unwrap_err();
        assert!(err.to_string().contains("off"), "player 1 must be on to share its USB: {err}");

        r.spawn_player(1, PlayerModel::Cdj3000).await.unwrap();
        assert!(r.load_track(2, 1, Slot::Sd, track.id).await.is_err(), "SD slot is empty");
        // The demo export has no audio files, so this is a metadata-only load.
        r.load_track(2, 1, Slot::Usb, track.id).await.unwrap();
        let deck = r.player(2).unwrap().deck;
        let loaded = deck.loaded().unwrap();
        assert_eq!((loaded.source_player, loaded.source_slot, loaded.track_id), (1, Slot::Usb, track.id));
        let beats = lib.beats_for(track).unwrap();
        assert_eq!(deck.grid().time_of(0), f64::from(beats[0].0), "the analysed grid");
        assert!(r.shared.link_media_available(2));
        assert!(!r.shared.link_media_available(1), "only player 1 has media");

        r.eject_media(1, Slot::Usb).unwrap();
        assert!(deck.loaded().is_none(), "ejecting unloads the track");
        assert!(r.media(1).is_empty());
        assert!(r.eject_media(1, Slot::Usb).is_err());
    }

    #[tokio::test]
    async fn mixer_drives_on_air_and_gains() {
        let r = rig();
        r.spawn_player(1, PlayerModel::Cdj3000).await.unwrap();
        r.spawn_player(2, PlayerModel::Cdj3000).await.unwrap();
        let (p1, p2) = (r.player(1).unwrap().deck, r.player(2).unwrap().deck);
        assert!(!p1.on_air(), "nothing is on air without a mixer");
        r.set_channel_fader(2, 0.0).unwrap();
        assert_eq!(r.shared.engine.gains().channels[1].level.get(), 1.0, "no mixer: straight out");

        r.spawn_mixer(MixerModel::DjmV10).await.unwrap();
        assert!(p1.on_air() && !p2.on_air());
        assert_eq!(
            r.shared.channels_on_air(4),
            [true, false, false, false],
            "channels without a player aren't on air"
        );
        assert_eq!(r.shared.engine.gains().channels[1].level.get(), 0.0);

        r.set_eq(1, 0, crate::mixer::EQ_KILL_DB).unwrap();
        assert_eq!(r.shared.engine.gains().channels[0].eq[0].get(), 0.0);

        r.set_crossfader_assign(1, CrossfaderAssign::B).unwrap();
        r.set_crossfader(0.0);
        assert!(!p1.on_air());

        assert!(r.despawn_mixer());
        r.set_crossfader(1.0);
        assert!(!p1.on_air(), "no mixer, no on-air");
    }

    /// Deck -> audio engine -> beat events -> beat packets on the wire, sent
    /// as each beat is heard. Skipped if port 50001 is taken (Beat Link
    /// Trigger running).
    #[tokio::test]
    async fn beat_packets_follow_the_playing_deck() {
        let Ok(listener) = tokio::net::UdpSocket::bind("127.0.0.1:50001").await else {
            eprintln!("skipping: 127.0.0.1:50001 is in use");
            return;
        };
        let mut cfg = rig().config().clone();
        // Unicast "broadcasts" to ourselves: loopback has no broadcast.
        cfg.topology.broadcast = Ipv4Addr::LOCALHOST;
        let r = Rig::new(cfg);
        r.spawn_player(1, PlayerModel::Cdj3000).await.unwrap();
        let deck = r.player(1).unwrap().deck;
        deck.load(crate::deck::tests::test_track(), None, BeatGrid::constant(12000, 0.0), &[], None);
        r.play(1).unwrap();

        // The claim sequence takes ~2.7 s before the device starts sending.
        let mut beats = Vec::new();
        let mut buf = [0u8; 128];
        let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        while beats.len() < 5 {
            let got = tokio::time::timeout_at(deadline, listener.recv(&mut buf)).await;
            let n = got.expect("beat packets arrive").unwrap();
            if let Ok(beat) = cdj_proto::Beat::decode(&buf[..n]) {
                beats.push((std::time::Instant::now(), beat));
            }
        }
        assert!(beats.iter().all(|(_, b)| b.device_number == 1 && b.bpm_hundredths == 12000));
        let worst = beats
            .windows(2)
            .map(|p| ((p[1].0 - p[0].0).as_secs_f64() - 0.5).abs() * 1000.0)
            .fold(0.0, f64::max);
        eprintln!("beat spacing within {worst:.2} ms of 500 ms");
        for pair in beats.windows(2) {
            let gap = pair[1].0 - pair[0].0;
            assert!(
                (gap.as_secs_f64() - 0.5).abs() < 0.03,
                "beats 500 ms apart at 120 BPM, got {gap:?}"
            );
            assert_eq!(pair[1].1.beat_within_bar, pair[0].1.beat_within_bar % 4 + 1);
        }
    }

    #[tokio::test]
    async fn sync_follows_the_master_tempo() {
        let r = rig();
        r.spawn_player(1, PlayerModel::Cdj3000).await.unwrap();
        r.spawn_player(2, PlayerModel::Cdj3000).await.unwrap();
        let grid = || BeatGrid::constant(12000, 0.0);
        let (p1, p2) = (r.player(1).unwrap().deck, r.player(2).unwrap().deck);
        p1.load(crate::deck::tests::test_track(), None, BeatGrid::constant(12500, 0.0), &[], None);
        p2.load(crate::deck::tests::test_track(), None, grid(), &[], None);
        r.set_master(1).unwrap();
        r.set_synced(2, true).unwrap();
        std::thread::sleep(SYNC_INTERVAL * 3);
        let v = p2.view();
        assert!((v.effective_bpm() - 125.0).abs() < 1e-6, "synced to 125: {}", v.effective_bpm());
        r.set_synced(2, false).unwrap();
        assert!((p2.view().effective_bpm() - 120.0).abs() < 1e-6);
    }
}
