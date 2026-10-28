#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cdj_core::deck::{HotCue, LoopRegion, TempoRange, HOT_CUES};
use cdj_core::engine::AudioEngine;
use cdj_core::feth::{setup_plan, FethPlan, FethStatus, Layout};
use cdj_core::grid::BeatGrid;
use cdj_core::library::{Library, TrackInfo};
use cdj_core::media::{find_exports, MediaCatalog, Slot};
use cdj_core::mixer::{CrossfaderAssign, MixerControls};
use cdj_core::models::{MixerModel, PlayerModel};
use cdj_core::rig::{Rig, RigConfig, MAX_PLAYERS};
use clap::Parser;
use serde::Serialize;
use tauri::ipc::Response;
use tauri::{Emitter, Manager, State};

// ---------------------------------------------------------------------------
// CLI args
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(name = "cdjapp", about = "CDJ Emulator desktop app")]
struct Args {
    /// A rekordbox export folder to offer as media besides the volumes in
    /// /Volumes. Falls back to the CDJ_LIBRARY environment variable.
    #[arg(long)]
    library: Option<PathBuf>,
}

const SNAPSHOT_INTERVAL: Duration = Duration::from_millis(100);
const MEDIA_SCAN_INTERVAL: Duration = Duration::from_secs(2);
const VOLUMES: &str = "/Volumes";

// ---------------------------------------------------------------------------
// Shared Tauri state
// ---------------------------------------------------------------------------

struct AppState {
    plan: Mutex<FethPlan>,
    /// Built on first use; rebuilt (keeping inserted media) when the
    /// network layout changes.
    rig: Mutex<Option<Arc<Rig>>>,
    catalog: MediaCatalog,
    /// Export folders added by hand (or on the command line).
    folders: Mutex<Vec<PathBuf>>,
    /// Exports found by the last scan.
    sources: Mutex<Vec<PathBuf>>,
    /// Output device chosen in the app (`None`: system default).
    audio_device: Mutex<Option<String>>,
    /// Play no audio at all.
    audio_muted: Mutex<bool>,
}

impl AppState {
    fn rig(&self) -> Option<Arc<Rig>> {
        self.rig.lock().unwrap().clone()
    }

    fn running_rig(&self) -> Result<Arc<Rig>, String> {
        self.rig().ok_or_else(|| "nothing is running".to_string())
    }

    /// The rig, created now if needed. Creating it binds nothing; spawning
    /// devices fails until the network exists.
    fn rig_or_create(&self) -> Result<Arc<Rig>, String> {
        let mut rig = self.rig.lock().unwrap();
        if let Some(rig) = rig.as_ref() {
            return Ok(rig.clone());
        }
        let created = Arc::new(build_rig(&self.plan.lock().unwrap())?);
        self.apply_audio(&created);
        *rig = Some(created.clone());
        Ok(created)
    }

    /// Point a rig's audio at the chosen output.
    fn apply_audio(&self, rig: &Rig) {
        if *self.audio_muted.lock().unwrap() {
            let _ = rig.mute_output();
            return;
        }
        let device = self.audio_device.lock().unwrap().clone();
        if let Err(e) = rig.set_output_device(device.as_deref()) {
            tracing::warn!("audio output unavailable, running silently: {e:#}");
        }
    }

    fn rescan(&self) {
        let mut found = find_exports(Path::new(VOLUMES));
        found.extend(
            self.folders
                .lock()
                .unwrap()
                .iter()
                .filter(|p| Library::is_export(p))
                .cloned(),
        );
        *self.sources.lock().unwrap() = found;
        self.catalog.prune();
        if let Some(rig) = self.rig() {
            rig.eject_missing_media();
        }
    }
}

fn build_rig(plan: &FethPlan) -> Result<Rig, String> {
    let (topology, addressing) = plan.rig_network().map_err(|e| e.to_string())?;
    Ok(Rig::new(RigConfig {
        topology,
        addressing,
        initial_bpm_hundredths: 12000,
        beat_grid_offset_ms: 0,
    }))
}

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
struct TrackSummary {
    id: u32,
    title: String,
    artist: String,
    album: String,
    bpm: f32,
    duration_s: u32,
    key: String,
}

impl From<&TrackInfo> for TrackSummary {
    fn from(t: &TrackInfo) -> Self {
        Self {
            id: t.id,
            title: t.title.clone(),
            artist: t.artist.clone(),
            album: t.album.clone(),
            bpm: t.bpm_hundredths as f32 / 100.0,
            duration_s: t.duration_s,
            key: t.key.clone(),
        }
    }
}

#[derive(Serialize, Clone)]
struct NetworkView {
    layout: Layout,
    emulator_iface: String,
    client_iface: String,
    client_ip: String,
    status: FethStatus,
    ready: bool,
}

#[derive(Serialize, Clone)]
struct SlotView {
    slot: Slot,
    name: String,
    root: String,
}

#[derive(Serialize, Clone)]
struct LoadedView {
    track: TrackSummary,
    source_player: u8,
    source_slot: Slot,
}

#[derive(Serialize, Clone, Default)]
struct PlayerView {
    number: u8,
    running: bool,
    /// PlayState1 name: NoTrack, Cued, Playing, Paused, Looping, Ended...
    play_state: String,
    playing: bool,
    master: bool,
    synced: bool,
    on_air: bool,
    /// On-screen BPM (pitch applied).
    bpm: f64,
    track_bpm: f64,
    /// Pitch in percent.
    pitch_percent: f64,
    pitch_fader: f64,
    tempo_range: Option<TempoRange>,
    master_tempo: bool,
    reverse: bool,
    quantize: bool,
    beat_within_bar: u8,
    beat_number: u32,
    position_ms: f64,
    length_ms: Option<f64>,
    cue_ms: f64,
    hot_cues: Vec<Option<HotCue>>,
    memory_cues: Vec<f64>,
    active_loop: Option<LoopRegion>,
    loaded: Option<LoadedView>,
    /// What is in this player's slots (kept while it is off).
    media: Vec<SlotView>,
    /// The model it runs as, while on.
    model: Option<PlayerModel>,
    model_name: Option<String>,
    /// Hot cues this model has (3 or 8).
    hot_cue_slots: usize,
}

#[derive(Serialize, Clone)]
struct MixerView {
    running: bool,
    model: Option<MixerModel>,
    model_name: Option<String>,
    channels: u8,
    controls: MixerControls,
}

#[derive(Serialize, Clone)]
struct ModelInfo<M> {
    id: M,
    name: &'static str,
    notes: &'static str,
}

/// The models players and the mixer can be switched on as.
#[derive(Serialize, Clone)]
struct ModelsView {
    players: Vec<ModelInfo<PlayerModel>>,
    mixers: Vec<ModelInfo<MixerModel>>,
}

/// An export that could go into a slot.
#[derive(Serialize, Clone)]
struct SourceView {
    root: String,
    name: String,
    /// Where it is inserted, if anywhere.
    inserted: Option<(u8, Slot)>,
}

#[derive(Serialize, Clone)]
struct AudioView {
    /// Device playing the mix, `None` when silent.
    device: Option<String>,
    muted: bool,
}

#[derive(Serialize, Clone)]
struct RigSnapshot {
    network: NetworkView,
    players: Vec<PlayerView>,
    mixer: MixerView,
    sources: Vec<SourceView>,
    audio: AudioView,
}

#[derive(Serialize, Clone)]
struct PlaylistView {
    id: u32,
    parent_id: u32,
    name: String,
    is_folder: bool,
    track_ids: Vec<u32>,
}

/// Everything on one inserted export, for the browser.
#[derive(Serialize, Clone)]
struct MediaContents {
    name: String,
    tracks: Vec<TrackSummary>,
    playlists: Vec<PlaylistView>,
}

fn network_view(plan: &FethPlan) -> NetworkView {
    let status = plan.status();
    NetworkView {
        layout: plan.layout,
        emulator_iface: plan.a_name.clone(),
        client_iface: plan.b_name.clone(),
        client_ip: plan.b_ip.clone(),
        ready: status.ready(),
        status,
    }
}

fn snapshot(state: &AppState) -> RigSnapshot {
    let plan = state.plan.lock().unwrap().clone();
    let rig = state.rig();
    let media_of = |n: u8| -> Vec<SlotView> {
        rig.as_ref()
            .map(|r| r.media(n))
            .unwrap_or_default()
            .into_iter()
            .map(|(slot, m)| SlotView {
                slot,
                name: m.name.clone(),
                root: m.root.display().to_string(),
            })
            .collect()
    };
    let players = (1..=MAX_PLAYERS)
        .map(|n| {
            let media = media_of(n);
            let Some(h) = rig.as_ref().and_then(|r| r.player(n)) else {
                return PlayerView {
                    number: n,
                    play_state: "Off".into(),
                    hot_cues: vec![None; HOT_CUES],
                    hot_cue_slots: HOT_CUES,
                    media,
                    ..Default::default()
                };
            };
            let deck = &h.deck;
            let v = deck.view();
            PlayerView {
                number: n,
                running: true,
                play_state: format!("{:?}", v.play_state),
                playing: v.moving,
                master: deck.master(),
                synced: deck.synced(),
                on_air: deck.on_air(),
                bpm: v.effective_bpm(),
                track_bpm: f64::from(v.track_bpm_hundredths) / 100.0,
                pitch_percent: v.pitch * 100.0,
                pitch_fader: v.pitch_fader,
                tempo_range: Some(v.tempo_range),
                master_tempo: v.master_tempo,
                reverse: v.reverse,
                quantize: v.quantize,
                beat_within_bar: v.bar_beat,
                beat_number: v.beat_number(),
                position_ms: v.position_ms,
                length_ms: v.length_ms,
                cue_ms: v.cue_ms,
                hot_cues: v.hot_cues.to_vec(),
                hot_cue_slots: deck.hot_cue_slots,
                model: Some(h.model),
                model_name: Some(h.model.display_name().into()),
                memory_cues: v.memory_cues.clone(),
                active_loop: v.active_loop,
                loaded: v.loaded.as_ref().and_then(|t| {
                    Some(LoadedView {
                        track: t.library.track_by_id(t.track_id)?.into(),
                        source_player: t.source_player,
                        source_slot: t.source_slot,
                    })
                }),
                media,
            }
        })
        .collect::<Vec<_>>();

    let inserted: Vec<(String, u8, Slot)> = players
        .iter()
        .flat_map(|p| p.media.iter().map(move |m| (m.root.clone(), p.number, m.slot)))
        .collect();
    let sources = state
        .sources
        .lock()
        .unwrap()
        .iter()
        .map(|root| {
            let root = root.display().to_string();
            SourceView {
                name: Path::new(&root)
                    .file_name()
                    .map_or_else(|| root.clone(), |n| n.to_string_lossy().into_owned()),
                inserted: inserted.iter().find(|(r, _, _)| *r == root).map(|&(_, n, s)| (n, s)),
                root,
            }
        })
        .collect();

    RigSnapshot {
        network: network_view(&plan),
        players,
        mixer: MixerView {
            running: rig.as_ref().is_some_and(|r| r.mixer_running()),
            model: rig.as_ref().and_then(|r| r.mixer_model()),
            model_name: rig.as_ref().and_then(|r| r.mixer_model()).map(|m| m.device_name().into()),
            channels: rig.as_ref().and_then(|r| r.mixer_model()).map_or(4, |m| m.channels()),
            controls: rig.as_ref().map(|r| r.mixer_controls()).unwrap_or_default(),
        },
        sources,
        audio: AudioView {
            device: rig.and_then(|r| r.output_device()),
            muted: *state.audio_muted.lock().unwrap(),
        },
    }
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

#[tauri::command]
fn models() -> ModelsView {
    ModelsView {
        players: PlayerModel::ALL
            .into_iter()
            .map(|m| ModelInfo {
                id: m,
                name: m.display_name(),
                notes: m.notes(),
            })
            .collect(),
        mixers: MixerModel::ALL
            .into_iter()
            .map(|m| ModelInfo {
                id: m,
                name: m.device_name(),
                notes: m.notes(),
            })
            .collect(),
    }
}

#[tauri::command]
fn rig_snapshot(state: State<AppState>) -> RigSnapshot {
    snapshot(&state)
}

#[tauri::command]
async fn network_setup(layout: Layout, state: State<'_, AppState>) -> Result<NetworkView, String> {
    if state.rig().is_some_and(|r| !r.players().is_empty() || r.mixer_running()) {
        return Err("Switch off all devices before changing the network.".into());
    }
    let plan = state.plan.lock().unwrap().clone().with_layout(layout);
    let applied = plan.clone();
    tauri::async_runtime::spawn_blocking(move || applied.apply())
        .await
        .map_err(|e| e.to_string())?
        .map_err(err)?;

    // Rebuild the rig for the new layout, keeping the media where it was.
    let rebuilt = build_rig(&plan)?;
    if let Some(old) = state.rig() {
        for n in 1..=MAX_PLAYERS {
            for (slot, medium) in old.media(n) {
                rebuilt.insert_media(n, slot, medium).map_err(err)?;
            }
        }
    }
    state.apply_audio(&rebuilt);
    *state.rig.lock().unwrap() = Some(Arc::new(rebuilt));
    *state.plan.lock().unwrap() = plan.clone();
    Ok(network_view(&plan))
}

#[tauri::command]
async fn network_teardown(state: State<'_, AppState>) -> Result<NetworkView, String> {
    if let Some(rig) = state.rig() {
        rig.shutdown();
    }
    let plan = state.plan.lock().unwrap().clone();
    let removed = plan.clone();
    tauri::async_runtime::spawn_blocking(move || removed.teardown())
        .await
        .map_err(|e| e.to_string())?
        .map_err(err)?;
    Ok(network_view(&plan))
}

#[tauri::command]
async fn spawn_player(player: u8, model: PlayerModel, state: State<'_, AppState>) -> Result<(), String> {
    state.rig_or_create()?.spawn_player(player, model).await.map_err(err)
}

#[tauri::command]
fn despawn_player(player: u8, state: State<AppState>) -> Result<(), String> {
    state.running_rig()?.despawn_player(player);
    Ok(())
}

#[tauri::command]
async fn spawn_mixer(model: MixerModel, state: State<'_, AppState>) -> Result<(), String> {
    state.rig_or_create()?.spawn_mixer(model).await.map_err(err)
}

#[tauri::command]
fn despawn_mixer(state: State<AppState>) -> Result<(), String> {
    state.running_rig()?.despawn_mixer();
    Ok(())
}

#[tauri::command]
fn add_media_folder(path: String, state: State<AppState>) -> Result<(), String> {
    let path = PathBuf::from(path.trim());
    if !Library::is_export(&path) {
        return Err(format!(
            "{} has no rekordbox export (expected PIONEER/rekordbox/export.pdb)",
            path.display()
        ));
    }
    let mut folders = state.folders.lock().unwrap();
    if !folders.contains(&path) {
        folders.push(path);
    }
    drop(folders);
    state.rescan();
    Ok(())
}

#[tauri::command]
async fn insert_media(player: u8, slot: Slot, root: String, state: State<'_, AppState>) -> Result<(), String> {
    let rig = state.rig_or_create()?;
    let root = PathBuf::from(root);
    // Opening an export parses its database; keep that off the async runtime.
    let medium = tokio::task::block_in_place(|| state.catalog.open(&root)).map_err(err)?;
    rig.insert_media(player, slot, medium).map_err(err)
}

#[tauri::command]
fn eject_media(player: u8, slot: Slot, state: State<AppState>) -> Result<(), String> {
    state.running_rig()?.eject_media(player, slot).map_err(err)
}

#[tauri::command]
fn media_contents(player: u8, slot: Slot, state: State<AppState>) -> Result<MediaContents, String> {
    let rig = state.running_rig()?;
    let (_, medium) = rig
        .media(player)
        .into_iter()
        .find(|(s, _)| *s == slot)
        .ok_or_else(|| format!("player {player}'s {slot:?} slot is empty"))?;
    let lib = &medium.library;
    let mut playlists = Vec::new();
    let mut folders = vec![0u32];
    while let Some(folder) = folders.pop() {
        for node in lib.playlist_children(folder) {
            if node.is_folder {
                folders.push(node.id);
            }
            playlists.push(PlaylistView {
                id: node.id,
                parent_id: node.parent_id,
                name: node.name.clone(),
                is_folder: node.is_folder,
                track_ids: lib.playlist_tracks(node.id).to_vec(),
            });
        }
    }
    Ok(MediaContents {
        name: medium.name.clone(),
        tracks: lib.tracks.iter().map(TrackSummary::from).collect(),
        playlists,
    })
}

#[tauri::command]
async fn load_track(
    player: u8,
    source_player: u8,
    slot: Slot,
    track_id: u32,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state
        .running_rig()?
        .load_track(player, source_player, slot, track_id)
        .await
        .map_err(err)
}

/// Run `f` on the running rig, mapping errors for the UI.
fn with_rig(state: &AppState, f: impl FnOnce(&Rig) -> anyhow::Result<()>) -> Result<(), String> {
    let rig = state.running_rig()?;
    f(&rig).map_err(err)
}

#[tauri::command]
fn play(player: u8, state: State<AppState>) -> Result<(), String> {
    with_rig(&state, |r| r.play(player))
}

#[tauri::command]
fn pause(player: u8, state: State<AppState>) -> Result<(), String> {
    with_rig(&state, |r| r.pause(player))
}

#[tauri::command]
fn play_pause(player: u8, state: State<AppState>) -> Result<(), String> {
    with_rig(&state, |r| r.play_pause(player))
}

#[tauri::command]
fn cue_press(player: u8, state: State<AppState>) -> Result<(), String> {
    with_rig(&state, |r| r.cue_press(player))
}

#[tauri::command]
fn cue_release(player: u8, state: State<AppState>) -> Result<(), String> {
    with_rig(&state, |r| r.cue_release(player))
}

#[tauri::command]
fn seek(player: u8, ms: f64, state: State<AppState>) -> Result<(), String> {
    with_rig(&state, |r| r.seek(player, ms))
}

#[tauri::command]
fn hot_cue(player: u8, pad: usize, state: State<AppState>) -> Result<(), String> {
    with_rig(&state, |r| r.hot_cue(player, pad))
}

#[tauri::command]
fn clear_hot_cue(player: u8, pad: usize, state: State<AppState>) -> Result<(), String> {
    with_rig(&state, |r| r.clear_hot_cue(player, pad))
}

#[tauri::command]
fn memory_cue(player: u8, forward: bool, state: State<AppState>) -> Result<(), String> {
    with_rig(&state, |r| r.memory_cue(player, forward))
}

#[tauri::command]
fn beat_loop(player: u8, beats: f64, state: State<AppState>) -> Result<(), String> {
    with_rig(&state, |r| r.beat_loop(player, beats))
}

#[tauri::command]
fn loop_in(player: u8, state: State<AppState>) -> Result<(), String> {
    with_rig(&state, |r| r.loop_in(player))
}

#[tauri::command]
fn loop_out(player: u8, state: State<AppState>) -> Result<(), String> {
    with_rig(&state, |r| r.loop_out(player))
}

#[tauri::command]
fn exit_loop(player: u8, state: State<AppState>) -> Result<(), String> {
    with_rig(&state, |r| r.exit_loop(player))
}

#[tauri::command]
fn reloop(player: u8, state: State<AppState>) -> Result<(), String> {
    with_rig(&state, |r| r.reloop(player))
}

#[tauri::command]
fn resize_loop(player: u8, factor: f64, state: State<AppState>) -> Result<(), String> {
    with_rig(&state, |r| r.resize_loop(player, factor))
}

#[tauri::command]
fn set_pitch_fader(player: u8, position: f64, state: State<AppState>) -> Result<(), String> {
    with_rig(&state, |r| r.set_pitch_fader(player, position))
}

#[tauri::command]
fn set_tempo_range(player: u8, range: TempoRange, state: State<AppState>) -> Result<(), String> {
    with_rig(&state, |r| r.set_tempo_range(player, range))
}

#[tauri::command]
fn set_master_tempo(player: u8, on: bool, state: State<AppState>) -> Result<(), String> {
    with_rig(&state, |r| r.set_master_tempo(player, on))
}

#[tauri::command]
fn set_bend(player: u8, bend: f64, state: State<AppState>) -> Result<(), String> {
    with_rig(&state, |r| r.set_bend(player, bend))
}

/// The loaded track's library and info on `player`.
fn loaded_track(state: &AppState, player: u8) -> Result<(Arc<Library>, TrackInfo), String> {
    let deck = state
        .running_rig()?
        .player(player)
        .ok_or_else(|| format!("player {player} is not running"))?
        .deck;
    let loaded = deck.loaded().ok_or("no track loaded")?;
    let track = loaded
        .library
        .track_by_id(loaded.track_id)
        .cloned()
        .ok_or("track not in library")?;
    Ok((loaded.library, track))
}

/// Display waveforms of the loaded track, as raw bytes: overview column
/// count and detail column count (u32 LE), then `[height, r, g, b]` per
/// column for each. Empty when there is no analysis.
#[tauri::command]
fn track_waveforms(player: u8, state: State<AppState>) -> Result<Response, String> {
    let (lib, track) = loaded_track(&state, player)?;
    let overview = lib.overview_display(&track).unwrap_or_default();
    let detail = lib.detail_display(&track).unwrap_or_default();
    let mut out = Vec::with_capacity(8 + (overview.len() + detail.len()) * 4);
    out.extend_from_slice(&(overview.len() as u32).to_le_bytes());
    out.extend_from_slice(&(detail.len() as u32).to_le_bytes());
    out.extend(overview.iter().flatten());
    out.extend(detail.iter().flatten());
    Ok(Response::new(out))
}

/// The loaded track's beats as `[time_ms, bar_beat]`.
#[tauri::command]
fn track_grid(player: u8, state: State<AppState>) -> Result<Vec<[f64; 2]>, String> {
    let deck = state
        .running_rig()?
        .player(player)
        .ok_or_else(|| format!("player {player} is not running"))?
        .deck;
    let length = deck.view().length_ms.unwrap_or(600_000.0);
    Ok(match deck.grid().as_ref() {
        BeatGrid::Analysed(beats) => beats.iter().map(|b| [b.time_ms, f64::from(b.bar_beat)]).collect(),
        grid @ BeatGrid::Constant { .. } => (0..)
            .map(|i| [grid.time_of(i), f64::from(grid.bar_beat(i))])
            .take_while(|b| b[0] <= length)
            .collect(),
    })
}

/// The loaded track's artwork (JPEG bytes; empty when there is none).
#[tauri::command]
fn track_artwork(player: u8, state: State<AppState>) -> Result<Response, String> {
    let (lib, track) = loaded_track(&state, player)?;
    Ok(Response::new(lib.artwork_jpeg(track.artwork_id).unwrap_or_default()))
}

#[tauri::command]
fn set_reverse(player: u8, on: bool, state: State<AppState>) -> Result<(), String> {
    with_rig(&state, |r| r.set_reverse(player, on))
}

#[tauri::command]
fn set_quantize(player: u8, on: bool, state: State<AppState>) -> Result<(), String> {
    with_rig(&state, |r| r.set_quantize(player, on))
}

#[tauri::command]
fn set_master(player: u8, state: State<AppState>) -> Result<(), String> {
    with_rig(&state, |r| r.set_master(player))
}

#[tauri::command]
fn set_sync(player: u8, on: bool, state: State<AppState>) -> Result<(), String> {
    with_rig(&state, |r| r.set_synced(player, on))
}

#[tauri::command]
fn set_trim(channel: u8, db: f32, state: State<AppState>) -> Result<(), String> {
    with_rig(&state, |r| r.set_trim(channel, db))
}

#[tauri::command]
fn set_eq(channel: u8, band: usize, db: f32, state: State<AppState>) -> Result<(), String> {
    with_rig(&state, |r| r.set_eq(channel, band, db))
}

#[tauri::command]
fn set_master_level(level: f32, state: State<AppState>) -> Result<(), String> {
    state.running_rig()?.set_master_level(level);
    Ok(())
}

#[tauri::command]
fn audio_devices() -> Vec<String> {
    AudioEngine::output_devices()
}

/// Play through `device` (`None`: system default), or mute.
#[tauri::command]
fn set_audio_output(device: Option<String>, muted: bool, state: State<AppState>) -> Result<(), String> {
    *state.audio_device.lock().unwrap() = device;
    *state.audio_muted.lock().unwrap() = muted;
    if let Some(rig) = state.rig() {
        state.apply_audio(&rig);
    }
    Ok(())
}

#[tauri::command]
fn set_channel_fader(channel: u8, level: f32, state: State<AppState>) -> Result<(), String> {
    state.running_rig()?.set_channel_fader(channel, level).map_err(err)
}

#[tauri::command]
fn set_crossfader(position: f32, state: State<AppState>) -> Result<(), String> {
    state.running_rig()?.set_crossfader(position);
    Ok(())
}

#[tauri::command]
fn set_crossfader_assign(
    channel: u8,
    assign: CrossfaderAssign,
    state: State<AppState>,
) -> Result<(), String> {
    state.running_rig()?.set_crossfader_assign(channel, assign).map_err(err)
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,cdj_core=debug")),
        )
        .init();

    let args = Args::parse();
    let folders = args
        .library
        .or_else(|| std::env::var("CDJ_LIBRARY").ok().map(PathBuf::from))
        .into_iter()
        .collect();

    let state = AppState {
        plan: Mutex::new(setup_plan("feth0", "feth1", "10.77.77.1", "10.77.77.200", 24)),
        rig: Mutex::new(None),
        catalog: MediaCatalog::default(),
        folders: Mutex::new(folders),
        sources: Mutex::new(Vec::new()),
        audio_device: Mutex::new(None),
        audio_muted: Mutex::new(false),
    };
    state.rescan();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .setup(|app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(SNAPSHOT_INTERVAL).await;
                    let snap = snapshot(&handle.state::<AppState>());
                    let _ = handle.emit("rig-snapshot", snap);
                }
            });
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(MEDIA_SCAN_INTERVAL).await;
                    let handle = handle.clone();
                    let _ = tauri::async_runtime::spawn_blocking(move || {
                        handle.state::<AppState>().rescan();
                    })
                    .await;
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            rig_snapshot,
            network_setup,
            network_teardown,
            spawn_player,
            despawn_player,
            spawn_mixer,
            despawn_mixer,
            add_media_folder,
            insert_media,
            eject_media,
            media_contents,
            load_track,
            play,
            pause,
            play_pause,
            cue_press,
            cue_release,
            seek,
            hot_cue,
            clear_hot_cue,
            memory_cue,
            beat_loop,
            loop_in,
            loop_out,
            exit_loop,
            reloop,
            resize_loop,
            set_pitch_fader,
            set_tempo_range,
            set_master_tempo,
            set_reverse,
            set_bend,
            track_waveforms,
            track_grid,
            track_artwork,
            set_quantize,
            set_master,
            set_sync,
            set_channel_fader,
            set_crossfader,
            set_crossfader_assign,
            set_trim,
            set_eq,
            set_master_level,
            audio_devices,
            models,
            set_audio_output,
        ])
        .build(tauri::generate_context!())?
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                if let Some(rig) = app.state::<AppState>().rig() {
                    rig.shutdown();
                }
            }
        });
    Ok(())
}
