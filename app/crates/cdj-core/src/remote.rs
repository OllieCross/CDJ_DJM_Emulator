//! Commands from client apps (Beat Link Trigger, ShowKontrol) to the
//! virtual players.
//!
//! What arrives, and what a player does with it (as beat-link expects of
//! real CDJs):
//!
//! | port  | packet                        | the player...                                  |
//! |-------|-------------------------------|------------------------------------------------|
//! | 50001 | master handoff request (0x26) | if it is tempo master: says yes (0x27) and names the requester in its status (0x9f) until the requester takes the role |
//! | 50001, 50002 | handoff response (0x27) | takes the role it asked for                    |
//! | 50001 | sync control (0x2a)           | turns sync on or off, or becomes tempo master  |
//! | 50001 broadcast | fader start (0x02)  | plays, or goes back to its cue and stops       |
//! | 50001 broadcast | beat (0x28)         | from an outside tempo master: synced players line up with it |
//! | 50002 | CDJ status (0x0a)             | learns who the tempo master is, and its tempo  |
//! | 50002 | media query (0x05)            | answers with what is in the slot (0x06)        |
//! | 50002 | load track (0x19)             | acknowledges (0x1a) and loads the track        |
//!
//! ## Sharing the ports with Beat Link Trigger
//!
//! Each player listens on its own address (`10.77.77.N`), and on the beat
//! port also on the broadcast address. macOS lets a socket bound to one
//! address share a port with a wildcard socket (`*:50001`, which is how
//! Beat Link Trigger listens) only if the wildcard socket was bound first:
//! binding ours first would stop Beat Link Trigger from starting. So the
//! players only listen on a port while a client app holds it. Every
//! [`CHECK_INTERVAL`] a probe checks whether `*:port` or a client's own
//! address is taken (the probe sets `SO_REUSEADDR`, so it only collides with
//! a socket bound to exactly that address and never with ours); the players
//! bind when one is and let go as soon as it isn't, so a client that
//! restarts can bind again. Clients listen on the ports they send commands
//! from, so no command is missed.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cdj_proto::{
    update_kind, Beat, CdjStatus, DeviceName, FaderStart, FaderStartAction, LoadTrack, LoadTrackAck,
    MasterHandoffRequest, MasterHandoffResponse, MediaQuery, MediaResponse, SyncCommand, SyncControl,
    TrackType, UpdateHeader, PITCH_NEUTRAL,
};
use serde::Serialize;
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::task::AbortHandle;
use tokio::time;
use tracing::{debug, info, warn};

use crate::media::Slot;
use crate::net::{bind_listener, PORT_BEAT, PORT_STATUS};
use crate::rig::{RigConfig, RigShared};

/// How often to check whether client apps hold the ports.
pub const CHECK_INTERVAL: Duration = Duration::from_millis(250);
/// A client that stops sending status is forgotten after this.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(4);
/// A tempo-master handoff that hasn't finished by now is dropped.
const HANDOFF_TIMEOUT: Duration = Duration::from_secs(3);
/// Broadcasts reach the emulator side of a feth pair twice; a repeat within
/// this window is the same packet.
const DUPLICATE_WINDOW: Duration = Duration::from_millis(50);
const LOG_LEN: usize = 10;

/// The ports players listen on for commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListenPorts {
    pub beat: u16,
    pub status: u16,
}

impl Default for ListenPorts {
    fn default() -> Self {
        Self {
            beat: PORT_BEAT,
            status: PORT_STATUS,
        }
    }
}

impl ListenPorts {
    fn number(self, port: PortKind) -> u16 {
        match port {
            PortKind::Beat => self.beat,
            PortKind::Status => self.status,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PortKind {
    Beat,
    Status,
}

/// Where a packet arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Endpoint {
    /// The player whose address it was sent to; `None` for the broadcast
    /// address, or when players share one address.
    pub device: Option<u8>,
    pub port: PortKind,
    pub broadcast: bool,
}

/// A packet for the listener to send from a player's socket, to the same
/// port at `to`.
#[derive(Debug)]
pub(crate) struct Outgoing {
    pub from: u8,
    pub port: PortKind,
    pub to: Ipv4Addr,
    pub bytes: Vec<u8>,
}

/// A device that isn't one of ours sending CDJ status: Beat Link Trigger
/// when it sends status, or a real player.
#[derive(Debug, Clone)]
struct Client {
    name: String,
    address: Ipv4Addr,
    master: bool,
    bpm: Option<f64>,
    seen: Instant,
}

#[derive(Debug, Clone, Copy)]
struct Handoff {
    /// Our player.
    player: u8,
    /// The device on the other side.
    other: u8,
    since: Instant,
}

#[derive(Debug, Clone, Copy)]
struct MasterBeat {
    at: Instant,
    bpm: f64,
}

#[derive(Default)]
struct State {
    clients: BTreeMap<u8, Client>,
    /// One of our players is handing the master role to another device.
    yielding: Option<Handoff>,
    /// One of our players asked another device for the master role.
    requesting: Option<Handoff>,
    master_beat: Option<MasterBeat>,
    recent: VecDeque<(u64, Instant)>,
    log: VecDeque<RemoteEvent>,
}

/// The tempo master, when it isn't one of our players.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExternalMaster {
    pub device_number: u8,
    pub name: String,
    pub address: Ipv4Addr,
    /// Effective tempo, if it reports one.
    pub bpm: Option<f64>,
}

/// Something a client app asked the players to do.
#[derive(Debug, Clone, Serialize)]
pub struct RemoteEvent {
    /// Unix time in ms.
    pub at_ms: u64,
    /// The sender's device name.
    pub from: String,
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RemoteView {
    /// A client app holds the ports, so the players are listening.
    pub listening: bool,
    pub master: Option<ExternalMaster>,
    /// Latest first.
    pub log: Vec<RemoteEvent>,
}

/// The players' side of the conversation with client apps.
pub(crate) struct Remote {
    state: Mutex<State>,
    outbox: UnboundedSender<Outgoing>,
    outbox_rx: Mutex<Option<UnboundedReceiver<Outgoing>>>,
    listening: AtomicBool,
}

impl Remote {
    pub(crate) fn new() -> Self {
        let (outbox, rx) = unbounded_channel();
        Self {
            state: Mutex::default(),
            outbox,
            outbox_rx: Mutex::new(Some(rx)),
            listening: AtomicBool::new(false),
        }
    }

    pub(crate) fn view(&self) -> RemoteView {
        RemoteView {
            listening: self.listening.load(Ordering::Relaxed),
            master: self.external_master(),
            log: self.state.lock().unwrap().log.iter().cloned().collect(),
        }
    }

    fn log(&self, from: &str, text: String) {
        info!(from, "{text}");
        let at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let mut st = self.state.lock().unwrap();
        st.log.push_front(RemoteEvent {
            at_ms,
            from: from.to_string(),
            text,
        });
        st.log.truncate(LOG_LEN);
    }

    fn send(&self, out: Outgoing) {
        let _ = self.outbox.send(out);
    }

    /// The tempo master, if a client holds the role.
    pub(crate) fn external_master(&self) -> Option<ExternalMaster> {
        let st = self.state.lock().unwrap();
        st.clients
            .iter()
            .find(|(_, c)| c.master && c.seen.elapsed() < CLIENT_TIMEOUT)
            .map(|(&device_number, c)| ExternalMaster {
                device_number,
                name: c.name.clone(),
                address: c.address,
                bpm: c.bpm,
            })
    }

    /// Who player `n` is handing the master role to (its status byte 0x9f).
    pub(crate) fn yielding_to(&self, n: u8) -> Option<u8> {
        self.state
            .lock()
            .unwrap()
            .yielding
            .filter(|h| h.player == n)
            .map(|h| h.other)
    }

    /// Where an outside tempo master is within its beat (0.0 ..< 1.0),
    /// from its last beat packet.
    pub(crate) fn master_phase(&self) -> Option<f64> {
        let beat = self.state.lock().unwrap().master_beat?;
        let beat_ms = 60_000.0 / beat.bpm;
        let age_ms = beat.at.elapsed().as_secs_f64() * 1000.0;
        (beat_ms.is_finite() && beat_ms > 0.0 && age_ms < 2.0 * beat_ms).then(|| (age_ms / beat_ms).fract())
    }

    /// Ask the outside tempo master to hand the role to player `n`.
    pub(crate) fn request_master(&self, n: u8, name: &DeviceName, master: &ExternalMaster) {
        self.state.lock().unwrap().requesting = Some(Handoff {
            player: n,
            other: master.device_number,
            since: Instant::now(),
        });
        let request = MasterHandoffRequest {
            device_name: name.clone(),
            device_number: n,
        };
        self.send(Outgoing {
            from: n,
            port: PortKind::Beat,
            to: master.address,
            bytes: request.encode().to_vec(),
        });
        self.log(&master.name, format!("player {n} asked for tempo master"));
    }

    /// Drop handoffs that never finished and clients that went quiet.
    fn expire(&self) {
        let mut st = self.state.lock().unwrap();
        let stale = |h: &Handoff| h.since.elapsed() > HANDOFF_TIMEOUT;
        if st.yielding.as_ref().is_some_and(stale) {
            st.yielding = None;
        }
        if st.requesting.as_ref().is_some_and(stale) {
            st.requesting = None;
        }
        st.clients.retain(|_, c| c.seen.elapsed() < CLIENT_TIMEOUT);
    }

    /// Whether this broadcast was already handled (see [`DUPLICATE_WINDOW`]).
    fn is_repeat(&self, packet: &[u8]) -> bool {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        packet.hash(&mut h);
        let hash = h.finish();
        let mut st = self.state.lock().unwrap();
        st.recent.retain(|(_, at)| at.elapsed() < DUPLICATE_WINDOW);
        if st.recent.iter().any(|(h, _)| *h == hash) {
            return true;
        }
        st.recent.push_back((hash, Instant::now()));
        false
    }
}

// ---------- handling packets ----------

/// Tempo with pitch applied, from a packet's BPM and pitch words.
fn effective_bpm(bpm_hundredths: u16, pitch: u32) -> f64 {
    f64::from(bpm_hundredths) / 100.0 * f64::from(pitch) / f64::from(PITCH_NEUTRAL)
}

/// Act on a packet that arrived at `at` from `src`.
pub(crate) fn handle(rig: &Arc<RigShared>, packet: &[u8], src: Ipv4Addr, at: Endpoint) {
    let Ok(h) = UpdateHeader::decode(packet) else {
        return;
    };
    // Our own devices' broadcasts loop back; they aren't commands.
    if rig.is_own_device(h.device_number) {
        return;
    }
    if at.broadcast && rig.remote.is_repeat(packet) {
        return;
    }
    let from = h.device_name.as_str_lossy().into_owned();
    let result = match (at.port, h.kind) {
        (PortKind::Beat, update_kind::MASTER_HANDOFF_REQUEST) => handoff_request(rig, &from, packet, src, at),
        // beat-link sends its answer to the status port, real players to
        // the beat port.
        (_, update_kind::MASTER_HANDOFF_RESPONSE) => handoff_response(rig, &from, packet, at),
        (PortKind::Beat, update_kind::SYNC_CONTROL) => sync_control(rig, &from, packet, at),
        (PortKind::Beat, update_kind::FADER_START) => fader_start(rig, &from, packet),
        (PortKind::Beat, update_kind::BEAT) => master_beat(rig, packet),
        (PortKind::Status, update_kind::CDJ_STATUS) => client_status(rig, &from, packet, src),
        (PortKind::Status, update_kind::MEDIA_QUERY) => media_query(rig, &from, packet, src),
        (PortKind::Status, update_kind::LOAD_TRACK) => load_track(rig, &from, packet, src, at),
        _ => Ok(()),
    };
    if let Err(e) = result {
        debug!(from, kind = h.kind, "ignored packet: {e:#}");
    }
}

fn handoff_request(rig: &RigShared, from: &str, packet: &[u8], src: Ipv4Addr, at: Endpoint) -> anyhow::Result<()> {
    let req = MasterHandoffRequest::decode(packet)?;
    let Some(n) = at.device.or_else(|| rig.tempo_master().map(|d| d.number)) else {
        return Ok(());
    };
    if !rig.deck(n)?.master() {
        rig.remote
            .log(from, format!("asked player {n} for tempo master, but it isn't master"));
        return Ok(());
    }
    rig.remote.state.lock().unwrap().yielding = Some(Handoff {
        player: n,
        other: req.device_number,
        since: Instant::now(),
    });
    let response = MasterHandoffResponse {
        device_name: rig.device_name(n),
        device_number: n,
        yielding: true,
    };
    rig.remote.send(Outgoing {
        from: n,
        port: PortKind::Beat,
        to: src,
        bytes: response.encode().to_vec(),
    });
    rig.remote
        .log(from, format!("asked player {n} for tempo master: handing it over"));
    Ok(())
}

fn handoff_response(rig: &RigShared, from: &str, packet: &[u8], at: Endpoint) -> anyhow::Result<()> {
    let resp = MasterHandoffResponse::decode(packet)?;
    let request = {
        let mut st = rig.remote.state.lock().unwrap();
        let ours = st
            .requesting
            .filter(|r| r.other == resp.device_number && at.device.map_or(true, |d| d == r.player));
        if ours.is_some() {
            st.requesting = None;
        }
        ours
    };
    let Some(request) = request else {
        return Ok(());
    };
    if resp.yielding {
        rig.take_master(request.player);
        rig.remote
            .log(from, format!("handed tempo master to player {}", request.player));
    } else {
        rig.remote
            .log(from, format!("refused to hand tempo master to player {}", request.player));
    }
    Ok(())
}

fn client_status(rig: &RigShared, from: &str, packet: &[u8], src: Ipv4Addr) -> anyhow::Result<()> {
    let s = CdjStatus::decode(packet)?;
    let was_master = {
        let mut st = rig.remote.state.lock().unwrap();
        let client = Client {
            name: from.to_string(),
            address: src,
            master: s.master,
            bpm: s.bpm_hundredths.map(|b| effective_bpm(b, s.pitch)),
            seen: Instant::now(),
        };
        st.clients
            .insert(s.device_number, client)
            .is_some_and(|c| c.master)
    };
    if !s.master {
        return Ok(());
    }
    match s.master_handoff_to {
        // It holds the role, so none of our players does.
        None => {
            let mut gave_up = Vec::new();
            for n in rig.player_numbers() {
                if let Ok(deck) = rig.deck(n) {
                    if deck.master() {
                        deck.set_master(false);
                        gave_up.push(n);
                    }
                }
            }
            let mut st = rig.remote.state.lock().unwrap();
            if st.yielding.is_some_and(|h| h.other == s.device_number) {
                st.yielding = None;
            }
            drop(st);
            if let Some(n) = gave_up.first() {
                rig.remote.log(from, format!("took tempo master from player {n}"));
            } else if !was_master {
                rig.remote.log(from, "is tempo master".into());
            }
        }
        // It is handing the role to player `t`: take it if we asked.
        Some(t) => {
            let granted = {
                let mut st = rig.remote.state.lock().unwrap();
                let ours = st.requesting.is_some_and(|r| r.player == t && r.other == s.device_number);
                if ours {
                    st.requesting = None;
                }
                ours
            };
            if granted {
                rig.take_master(t);
                rig.remote.log(from, format!("handed tempo master to player {t}"));
            }
        }
    }
    Ok(())
}

fn sync_control(rig: &RigShared, from: &str, packet: &[u8], at: Endpoint) -> anyhow::Result<()> {
    let sc = SyncControl::decode(packet)?;
    // Players sharing one address can't tell which of them it was for.
    let Some(n) = at.device else {
        return Ok(());
    };
    match sc.command {
        SyncCommand::SyncOn => {
            rig.set_synced(n, true)?;
            rig.remote.log(from, format!("turned sync on for player {n}"));
        }
        SyncCommand::SyncOff => {
            rig.set_synced(n, false)?;
            rig.remote.log(from, format!("turned sync off for player {n}"));
        }
        SyncCommand::BecomeMaster => {
            rig.remote
                .log(from, format!("told player {n} to become tempo master"));
            rig.become_master(n)?;
        }
        SyncCommand::Unknown(b) => debug!(from, command = b, "unknown sync command"),
    }
    Ok(())
}

fn fader_start(rig: &RigShared, from: &str, packet: &[u8]) -> anyhow::Result<()> {
    let fs = FaderStart::decode(packet)?;
    let (mut started, mut stopped) = (Vec::new(), Vec::new());
    for (n, action) in (1u8..).zip(fs.players) {
        let Ok(deck) = rig.deck(n) else { continue };
        match action {
            FaderStartAction::Start if !deck.is_playing() => {
                rig.play(n)?;
                started.push(n.to_string());
            }
            // Fader closed: back to the cue point, as the CUE button does.
            FaderStartAction::Stop if deck.is_playing() => {
                deck.cue_press();
                deck.cue_release();
                stopped.push(n.to_string());
            }
            _ => {}
        }
    }
    if !started.is_empty() {
        rig.remote
            .log(from, format!("fader start: player {}", started.join(", ")));
    }
    if !stopped.is_empty() {
        rig.remote
            .log(from, format!("fader stop: player {}", stopped.join(", ")));
    }
    Ok(())
}

fn master_beat(rig: &RigShared, packet: &[u8]) -> anyhow::Result<()> {
    let beat = Beat::decode(packet)?;
    let mut st = rig.remote.state.lock().unwrap();
    if st.clients.get(&beat.device_number).is_some_and(|c| c.master) {
        st.master_beat = Some(MasterBeat {
            at: Instant::now(),
            bpm: effective_bpm(beat.bpm_hundredths, beat.pitch),
        });
    }
    Ok(())
}

fn media_query(rig: &RigShared, from: &str, packet: &[u8], src: Ipv4Addr) -> anyhow::Result<()> {
    let q = MediaQuery::decode(packet)?;
    let n = q.player;
    rig.deck(n)?;
    let slot = Slot::from_protocol(q.slot.to_byte())
        .ok_or_else(|| anyhow::anyhow!("no such slot {:?}", q.slot))?;
    let Some(medium) = rig.media(n).get(slot) else {
        rig.remote
            .log(from, format!("asked about player {n}'s {} slot, which is empty", slot.label()));
        return Ok(());
    };
    let s = medium.summary();
    let response = MediaResponse {
        device_name: rig.device_name(n),
        device_number: n,
        player: n,
        slot: q.slot,
        name: s.name,
        creation_date: s.created,
        track_count: s.tracks.min(usize::from(u16::MAX)) as u16,
        color: 0,
        media_type: TrackType::Rekordbox,
        has_my_settings: s.my_settings,
        playlist_count: s.playlists.min(usize::from(u16::MAX)) as u16,
        total_bytes: s.total_bytes,
        free_bytes: s.free_bytes,
    };
    rig.remote.send(Outgoing {
        from: n,
        port: PortKind::Status,
        to: if q.requester.is_unspecified() { src } else { q.requester },
        bytes: response.encode().to_vec(),
    });
    rig.remote
        .log(from, format!("asked what is in player {n}'s {} slot", slot.label()));
    Ok(())
}

fn load_track(rig: &Arc<RigShared>, from: &str, packet: &[u8], src: Ipv4Addr, at: Endpoint) -> anyhow::Result<()> {
    let cmd = LoadTrack::decode(packet)?;
    let n = at.device.unwrap_or(cmd.target_player);
    rig.deck(n)?;
    let Some(slot) = Slot::from_protocol(cmd.source_slot.to_byte()) else {
        rig.remote.log(
            from,
            format!("asked player {n} to load from {:?}; only USB and SD hold tracks here", cmd.source_slot),
        );
        return Ok(());
    };
    let ack = LoadTrackAck {
        device_name: rig.device_name(n),
        device_number: n,
    };
    rig.remote.send(Outgoing {
        from: n,
        port: PortKind::Status,
        to: src,
        bytes: ack.encode().to_vec(),
    });
    let (rig, from) = (rig.clone(), from.to_string());
    let (id, source) = (cmd.rekordbox_id, cmd.source_player);
    tokio::spawn(async move {
        let text = match rig.load_track(n, source, slot, id).await {
            Ok(()) => format!("loaded track {id} from player {source}'s {} on player {n}", slot.label()),
            Err(e) => format!("couldn't load track {id} on player {n}: {e:#}"),
        };
        rig.remote.log(&from, text);
    });
    Ok(())
}

// ---------- the network side ----------

/// One socket a player (or the broadcast address) listens on.
struct Bound {
    endpoint: Endpoint,
    socket: Arc<UdpSocket>,
    task: AbortHandle,
}

impl Drop for Bound {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Listen for commands while client apps hold the ports, and send the
/// players' answers, until the rig goes away.
pub(crate) async fn run(rig: Weak<RigShared>, cfg: RigConfig, ports: ListenPorts) {
    let Some(mut outbox) = rig
        .upgrade()
        .and_then(|r| r.remote.outbox_rx.lock().unwrap().take())
    else {
        return;
    };
    let mut bound: HashMap<SocketAddrV4, Bound> = HashMap::new();
    let mut reported: HashSet<SocketAddrV4> = HashSet::new();
    let mut tick = time::interval(CHECK_INTERVAL);
    tick.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let Some(shared) = rig.upgrade() else { break };
                shared.remote.expire();
                let wanted = wanted_endpoints(&shared, &cfg, ports);
                bound.retain(|addr, b| wanted.get(addr) == Some(&b.endpoint));
                for (addr, endpoint) in wanted {
                    if bound.contains_key(&addr) {
                        continue;
                    }
                    match bind_listener(addr, &cfg.topology.iface) {
                        Ok(socket) => {
                            reported.remove(&addr);
                            let socket = Arc::new(socket);
                            let task = tokio::spawn(receive(rig.clone(), socket.clone(), endpoint)).abort_handle();
                            bound.insert(addr, Bound { endpoint, socket, task });
                        }
                        // The broadcast address of a loopback test setup
                        // isn't one we can bind; don't nag about it.
                        Err(e) if endpoint.broadcast => {
                            if reported.insert(addr) {
                                debug!(%addr, "can't listen for broadcasts: {e}");
                            }
                        }
                        Err(e) => {
                            if reported.insert(addr) {
                                warn!(%addr, "can't listen for commands: {e}");
                            }
                        }
                    }
                }
                let listening = !bound.is_empty();
                if shared.remote.listening.swap(listening, Ordering::Relaxed) != listening {
                    if listening {
                        info!("client app on the network: players listen for commands");
                    } else {
                        info!("no client app on the ports: players stopped listening");
                    }
                }
            }
            Some(out) = outbox.recv() => {
                let socket = bound
                    .values()
                    .find(|b| {
                        b.endpoint.port == out.port
                            && !b.endpoint.broadcast
                            && b.endpoint.device.map_or(true, |d| d == out.from)
                    })
                    .map(|b| b.socket.clone());
                let dest = SocketAddrV4::new(out.to, ports.number(out.port));
                match socket {
                    Some(socket) => {
                        if let Err(e) = socket.send_to(&out.bytes, dest).await {
                            warn!(from = out.from, %dest, "can't answer client app: {e}");
                        }
                    }
                    None => debug!(from = out.from, %dest, "player isn't listening; answer dropped"),
                }
            }
        }
    }
}

/// The addresses players should listen on now: their own (and on the beat
/// port the broadcast address), on each port a client app holds.
fn wanted_endpoints(rig: &RigShared, cfg: &RigConfig, ports: ListenPorts) -> HashMap<SocketAddrV4, Endpoint> {
    let players = rig.player_numbers();
    let mut wanted = HashMap::new();
    if players.is_empty() {
        return wanted;
    }
    for port in [PortKind::Beat, PortKind::Status] {
        let number = ports.number(port);
        if !held_by_client(number, &cfg.topology.peers) {
            continue;
        }
        for &n in &players {
            wanted
                .entry(SocketAddrV4::new(cfg.device_ip(n), number))
                .and_modify(|e: &mut Endpoint| e.device = None)
                .or_insert(Endpoint {
                    device: Some(n),
                    port,
                    broadcast: false,
                });
        }
        if port == PortKind::Beat {
            wanted
                .entry(SocketAddrV4::new(cfg.topology.broadcast, number))
                .or_insert(Endpoint {
                    device: None,
                    port,
                    broadcast: true,
                });
        }
    }
    wanted
}

/// Whether a client app holds `port`, on the wildcard address or its own.
fn held_by_client(port: u16, clients: &[Ipv4Addr]) -> bool {
    std::iter::once(Ipv4Addr::UNSPECIFIED)
        .chain(clients.iter().copied())
        .any(|ip| address_taken(SocketAddrV4::new(ip, port)))
}

/// Whether a socket is bound to exactly `addr`. The probe sets
/// `SO_REUSEADDR`, so it only collides with an exact match.
fn address_taken(addr: SocketAddrV4) -> bool {
    let Ok(probe) = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)) else {
        return false;
    };
    if probe.set_reuse_address(true).is_err() {
        return false;
    }
    matches!(probe.bind(&SocketAddr::V4(addr).into()), Err(e) if e.kind() == std::io::ErrorKind::AddrInUse)
}

async fn receive(rig: Weak<RigShared>, socket: Arc<UdpSocket>, at: Endpoint) {
    let mut buf = [0u8; 1500];
    loop {
        let (len, src) = match socket.recv_from(&mut buf).await {
            Ok(got) => got,
            Err(e) => {
                debug!(?at, "receive failed: {e}");
                time::sleep(Duration::from_millis(10)).await;
                continue;
            }
        };
        let SocketAddr::V4(src) = src else { continue };
        let Some(rig) = rig.upgrade() else { return };
        handle(&rig, &buf[..len], *src.ip(), at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_sees_wildcard_holders_but_not_specific_neighbours() {
        let wildcard = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
        let port = wildcard.local_addr().unwrap().port();
        assert!(held_by_client(port, &[]), "a wildcard socket holds the port");
        drop(wildcard);

        // A player's own socket on a specific address isn't a client.
        let ours = bind_specific(port);
        assert!(!held_by_client(port, &[]));
        // A socket on a client's address counts as that client.
        assert!(held_by_client(port, &[Ipv4Addr::LOCALHOST]));
        drop(ours);
        assert!(!held_by_client(port, &[Ipv4Addr::LOCALHOST]));
    }

    fn bind_specific(port: u16) -> Socket {
        let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).unwrap();
        s.set_reuse_address(true).unwrap();
        s.bind(&SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).into())
            .unwrap();
        s
    }

    use crate::deck::tests::test_track;
    use crate::grid::BeatGrid;
    use crate::media::Medium;
    use crate::models::PlayerModel;
    use crate::rig::tests::rig;
    use crate::rig::Rig;
    use cdj_proto::TrackSourceSlot;

    const BLT: u8 = 5;
    const BLT_IP: Ipv4Addr = Ipv4Addr::new(10, 77, 77, 200);

    fn blt() -> DeviceName {
        DeviceName::new("beat-link").unwrap()
    }

    fn to_player(n: u8, port: PortKind) -> Endpoint {
        Endpoint {
            device: Some(n),
            port,
            broadcast: false,
        }
    }

    fn broadcast() -> Endpoint {
        Endpoint {
            device: None,
            port: PortKind::Beat,
            broadcast: true,
        }
    }

    async fn players(numbers: &[u8]) -> (Rig, UnboundedReceiver<Outgoing>) {
        let r = rig();
        for &n in numbers {
            r.spawn_player(n, PlayerModel::Cdj3000).await.unwrap();
            let deck = r.player(n).unwrap().deck;
            deck.load(test_track(), None, BeatGrid::constant(12000, 0.0), &[], Some(60_000.0));
        }
        let outbox = r.shared().remote.outbox_rx.lock().unwrap().take().unwrap();
        (r, outbox)
    }

    fn blt_status(master: bool, handoff_to: Option<u8>, bpm_hundredths: u16) -> Vec<u8> {
        CdjStatus {
            master,
            master_handoff_to: handoff_to,
            bpm_hundredths: Some(bpm_hundredths),
            ..CdjStatus::idle(blt(), BLT)
        }
        .encode_len(0x11c)
    }

    fn sent(outbox: &mut UnboundedReceiver<Outgoing>) -> Outgoing {
        outbox.try_recv().expect("an answer was sent")
    }

    #[tokio::test]
    async fn client_takes_tempo_master_from_our_player() {
        let (r, mut outbox) = players(&[1, 2]).await;
        let shared = r.shared();
        r.play(1).unwrap();
        let p1 = r.player(1).unwrap().deck;
        assert!(p1.master());

        let request = MasterHandoffRequest {
            device_name: blt(),
            device_number: BLT,
        };
        handle(shared, &request.encode(), BLT_IP, to_player(1, PortKind::Beat));
        let answer = sent(&mut outbox);
        assert_eq!((answer.from, answer.port, answer.to), (1, PortKind::Beat, BLT_IP));
        let response = MasterHandoffResponse::decode(&answer.bytes).unwrap();
        assert!(response.yielding && response.device_number == 1);
        assert_eq!(shared.remote.yielding_to(1), Some(BLT), "status names the new master");
        assert!(p1.master(), "still master until the client claims the role");

        handle(shared, &blt_status(true, None, 12500), BLT_IP, to_player(1, PortKind::Status));
        assert!(!p1.master());
        assert_eq!(shared.remote.yielding_to(1), None);
        let master = r.remote_view().master.unwrap();
        assert_eq!((master.device_number, master.name.as_str()), (BLT, "beat-link"));
        assert_eq!(master.bpm, Some(125.0));

        // A player that starts now doesn't claim the role, and a synced one
        // plays at the client's tempo.
        r.set_synced(2, true).unwrap();
        r.play(2).unwrap();
        let p2 = r.player(2).unwrap().deck;
        assert!(!p2.master());
        assert!((p2.view().effective_bpm() - 125.0).abs() < 1e-6);
    }

    #[tokio::test]
    async fn our_player_asks_the_client_for_tempo_master() {
        let (r, mut outbox) = players(&[1, 2]).await;
        let shared = r.shared();
        handle(shared, &blt_status(true, None, 12800), BLT_IP, to_player(2, PortKind::Status));

        r.set_master(2).unwrap();
        let p2 = r.player(2).unwrap().deck;
        assert!(!p2.master(), "has to ask first");
        let ask = sent(&mut outbox);
        assert_eq!((ask.from, ask.port, ask.to), (2, PortKind::Beat, BLT_IP));
        assert_eq!(MasterHandoffRequest::decode(&ask.bytes).unwrap().device_number, 2);

        // beat-link answers on the status port.
        let yes = MasterHandoffResponse {
            device_name: blt(),
            device_number: BLT,
            yielding: true,
        };
        handle(shared, &yes.encode(), BLT_IP, to_player(2, PortKind::Status));
        assert!(p2.master());

        // The client stops claiming the role once it sees ours.
        handle(shared, &blt_status(false, None, 12800), BLT_IP, to_player(2, PortKind::Status));
        assert!(r.remote_view().master.is_none());
        assert!(p2.master());
    }

    #[tokio::test]
    async fn handoff_also_completes_from_the_clients_status() {
        let (r, _outbox) = players(&[3]).await;
        let shared = r.shared();
        handle(shared, &blt_status(true, None, 12800), BLT_IP, to_player(3, PortKind::Status));
        r.set_master(3).unwrap();
        handle(shared, &blt_status(true, Some(3), 12800), BLT_IP, to_player(3, PortKind::Status));
        assert!(r.player(3).unwrap().deck.master());
    }

    #[tokio::test]
    async fn sync_control_and_fader_start() {
        let (r, _outbox) = players(&[1, 2]).await;
        let shared = r.shared();
        let sync = |command| {
            SyncControl {
                device_name: blt(),
                device_number: BLT,
                command,
            }
            .encode()
        };
        let p2 = r.player(2).unwrap().deck;
        handle(shared, &sync(SyncCommand::SyncOn), BLT_IP, to_player(2, PortKind::Beat));
        assert!(p2.synced());
        handle(shared, &sync(SyncCommand::SyncOff), BLT_IP, to_player(2, PortKind::Beat));
        assert!(!p2.synced());
        handle(shared, &sync(SyncCommand::BecomeMaster), BLT_IP, to_player(2, PortKind::Beat));
        assert!(p2.master());

        use FaderStartAction::*;
        let fader = |players| {
            FaderStart {
                device_name: blt(),
                device_number: BLT,
                players,
            }
            .encode()
        };
        let p1 = r.player(1).unwrap().deck;
        p1.seek(0.0);
        handle(shared, &fader([Start, NoChange, NoChange, NoChange]), BLT_IP, broadcast());
        assert!(p1.is_playing());
        // The second copy of a broadcast is the same packet.
        handle(shared, &fader([Stop, NoChange, NoChange, NoChange]), BLT_IP, broadcast());
        handle(shared, &fader([Stop, NoChange, NoChange, NoChange]), BLT_IP, broadcast());
        assert!(!p1.is_playing());
        // The audio thread applies the jump on its next block.
        for _ in 0..50 {
            if p1.view().position_ms == p1.view().cue_ms {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(p1.view().position_ms, p1.view().cue_ms, "back at the cue");
        assert!(r.remote_view().log.iter().any(|e| e.text == "fader start: player 1"));
    }

    #[tokio::test]
    async fn media_query_describes_the_slot() {
        let (r, mut outbox) = players(&[1]).await;
        let shared = r.shared();
        let usb = Medium::open(&crate::library::tests::demo_export()).unwrap();
        r.insert_media(1, Slot::Usb, usb.clone()).unwrap();
        let query = |slot| {
            MediaQuery {
                device_name: blt(),
                device_number: BLT,
                requester: BLT_IP,
                player: 1,
                slot,
            }
            .encode()
        };
        handle(shared, &query(TrackSourceSlot::Usb), BLT_IP, to_player(1, PortKind::Status));
        let answer = sent(&mut outbox);
        assert_eq!((answer.from, answer.port, answer.to), (1, PortKind::Status, BLT_IP));
        let details = MediaResponse::decode(&answer.bytes).unwrap();
        assert_eq!((details.player, details.slot), (1, TrackSourceSlot::Usb));
        assert_eq!(details.name, "demo_tracks");
        assert_eq!(usize::from(details.track_count), usb.library.tracks.len());
        assert_eq!(details.media_type, TrackType::Rekordbox);

        handle(shared, &query(TrackSourceSlot::Sd), BLT_IP, to_player(1, PortKind::Status));
        assert!(outbox.try_recv().is_err(), "nothing to say about an empty slot");
    }

    #[tokio::test]
    async fn load_track_command_loads_and_acknowledges() {
        let (r, mut outbox) = players(&[1, 2]).await;
        let shared = r.shared();
        let usb = Medium::open(&crate::library::tests::demo_export()).unwrap();
        let track = usb.library.tracks[1].id;
        r.insert_media(1, Slot::Usb, usb).unwrap();
        let load = LoadTrack {
            device_name: blt(),
            device_number: BLT,
            source_player: 1,
            source_slot: TrackSourceSlot::Usb,
            track_type: TrackType::Rekordbox,
            rekordbox_id: track,
            target_player: 2,
        };
        handle(shared, &load.encode(), BLT_IP, to_player(2, PortKind::Status));
        let ack = sent(&mut outbox);
        assert_eq!((ack.from, ack.port, ack.to, ack.bytes[0x0a]), (2, PortKind::Status, BLT_IP, 0x1a));

        let deck = r.player(2).unwrap().deck;
        let deadline = Instant::now() + Duration::from_secs(5);
        while deck.loaded().map(|t| t.track_id) != Some(track) {
            assert!(Instant::now() < deadline, "track loads");
            time::sleep(Duration::from_millis(20)).await;
        }
        let loaded = deck.loaded().unwrap();
        assert_eq!((loaded.source_player, loaded.source_slot), (1, Slot::Usb));
    }

    #[tokio::test]
    async fn our_own_packets_are_not_commands() {
        let (r, mut outbox) = players(&[1]).await;
        r.play(1).unwrap();
        let own = MasterHandoffRequest {
            device_name: DeviceName::new("CDJ-3000").unwrap(),
            device_number: 1,
        };
        handle(r.shared(), &own.encode(), BLT_IP, to_player(1, PortKind::Beat));
        assert!(outbox.try_recv().is_err());
        assert_eq!(r.shared().remote.yielding_to(1), None);
    }

    /// Over real sockets: the player listens only while a "client" holds the
    /// ports, takes a command, and lets go when the client leaves so it can
    /// bind again.
    #[tokio::test]
    async fn players_listen_only_while_a_client_holds_the_ports() {
        let beat_client = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
        let status_client = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
        let ports = ListenPorts {
            beat: beat_client.local_addr().unwrap().port(),
            status: status_client.local_addr().unwrap().port(),
        };
        let mut cfg = rig().config().clone();
        cfg.listen_ports = Some(ports);
        let r = Rig::new(cfg);
        r.spawn_player(1, PlayerModel::Cdj3000).await.unwrap();

        let wait_for = |want: bool| {
            let r = &r;
            async move {
                let deadline = Instant::now() + Duration::from_secs(3);
                while r.remote_view().listening != want {
                    assert!(Instant::now() < deadline, "listening = {want}");
                    time::sleep(Duration::from_millis(20)).await;
                }
            }
        };
        wait_for(true).await;

        let sync_on = SyncControl {
            device_name: blt(),
            device_number: BLT,
            command: SyncCommand::SyncOn,
        };
        let sender = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        sender.send_to(&sync_on.encode(), ("127.0.0.1", ports.beat)).unwrap();
        let deck = r.player(1).unwrap().deck;
        let deadline = Instant::now() + Duration::from_secs(2);
        while !deck.synced() {
            assert!(Instant::now() < deadline, "the command reached the player");
            time::sleep(Duration::from_millis(20)).await;
        }

        drop((beat_client, status_client));
        wait_for(false).await;
        time::sleep(Duration::from_millis(50)).await;
        std::net::UdpSocket::bind(("0.0.0.0", ports.beat)).expect("a restarted client can bind again");
    }

    #[test]
    fn effective_bpm_applies_pitch() {
        assert_eq!(effective_bpm(12800, PITCH_NEUTRAL), 128.0);
        assert!((effective_bpm(12000, PITCH_NEUTRAL + PITCH_NEUTRAL / 10) - 132.0).abs() < 1e-3);
    }

    #[test]
    fn repeated_broadcasts_are_dropped_once() {
        let r = Remote::new();
        assert!(!r.is_repeat(b"fader start"));
        assert!(r.is_repeat(b"fader start"), "second copy of the same broadcast");
        assert!(!r.is_repeat(b"something else"));
        std::thread::sleep(DUPLICATE_WINDOW + Duration::from_millis(10));
        assert!(!r.is_repeat(b"fader start"), "a later identical packet is a new one");
    }
}
