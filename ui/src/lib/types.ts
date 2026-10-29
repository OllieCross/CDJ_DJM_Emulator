// Mirrors the Rust wire types in src-tauri/src/main.rs.

export type Layout = "SharedSubnet" | "LoopbackAliases";
export type CrossfaderAssign = "A" | "Thru" | "B";
export type Slot = "Usb" | "Sd";

export interface TrackSummary {
  id: number;
  title: string;
  artist: string;
  album: string;
  bpm: number;
  duration_s: number;
  key: string;
}

export interface SlotView {
  slot: Slot;
  name: string;
  root: string;
}

export interface LoadedView {
  track: TrackSummary;
  source_player: number;
  source_slot: Slot;
}

/// A rekordbox export that could go into a slot.
export interface SourceView {
  root: string;
  name: string;
  inserted: [number, Slot] | null;
}

export interface PlaylistView {
  id: number;
  parent_id: number;
  name: string;
  is_folder: boolean;
  track_ids: number[];
}

export interface MediaContents {
  name: string;
  tracks: TrackSummary[];
  playlists: PlaylistView[];
}

export interface NetworkView {
  layout: Layout;
  emulator_iface: string;
  client_iface: string;
  client_ip: string;
  status: {
    pair_exists: boolean;
    client_ip_up: boolean;
    missing_device_ips: string[];
    emulator_route_up: boolean;
  };
  ready: boolean;
}

export type TempoRange = "Six" | "Ten" | "Sixteen" | "Wide";

export interface HotCue {
  start_ms: number;
  loop_end_ms: number | null;
  color: [number, number, number] | null;
}

export interface LoopRegion {
  start_ms: number;
  end_ms: number;
  beats: number | null;
  active: boolean;
}

export interface PlayerView {
  number: number;
  running: boolean;
  /// NoTrack, Cued, Playing, Paused, Looping, Ended, CuePlaying, Off...
  play_state: string;
  playing: boolean;
  master: boolean;
  synced: boolean;
  on_air: boolean;
  bpm: number;
  track_bpm: number;
  pitch_percent: number;
  pitch_fader: number;
  tempo_range: TempoRange | null;
  master_tempo: boolean;
  reverse: boolean;
  quantize: boolean;
  beat_within_bar: number;
  beat_number: number;
  position_ms: number;
  length_ms: number | null;
  cue_ms: number;
  hot_cues: (HotCue | null)[];
  memory_cues: number[];
  active_loop: LoopRegion | null;
  /// LOOP IN pressed, waiting for LOOP OUT.
  loop_in_ms: number | null;
  loaded: LoadedView | null;
  media: SlotView[];
  /// Model id while on, e.g. "Cdj3000".
  model: string | null;
  model_name: string | null;
  /// Hot cues this model has (3 or 8).
  hot_cue_slots: number;
}

export interface MixerControls {
  channel_faders: number[];
  crossfader: number;
  assign: CrossfaderAssign[];
  trim_db: number[];
  eq_db: number[][];
  master: number;
}

export interface AudioView {
  device: string | null;
  muted: boolean;
}

export interface MixerView {
  running: boolean;
  /// Model id while on, e.g. "DjmA9".
  model: string | null;
  model_name: string | null;
  channels: number;
  controls: MixerControls;
}

/** The tempo master, when it is a client app rather than one of our players. */
export interface ExternalMaster {
  device_number: number;
  name: string;
  address: string;
  bpm: number | null;
}

export interface RemoteEvent {
  /** Unix time in ms. */
  at_ms: number;
  from: string;
  text: string;
}

/** Commands from client apps (Beat Link Trigger, ShowKontrol). */
export interface RemoteView {
  listening: boolean;
  master: ExternalMaster | null;
  /** Latest first. */
  log: RemoteEvent[];
}

export interface RigSnapshot {
  network: NetworkView;
  players: PlayerView[];
  mixer: MixerView;
  sources: SourceView[];
  audio: AudioView;
  remote: RemoteView;
}

export function slotLabel(slot: Slot): string {
  return slot === "Usb" ? "MEDIA" : "SD";
}

export function formatTime(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m}:${s.toString().padStart(2, "0")}`;
}

/// m:ss.t, the way CDJs show time.
export function formatTimeTenths(ms: number): string {
  const tenths = Math.max(0, Math.floor(ms / 100)) % 10;
  return `${formatTime(ms)}.${tenths}`;
}

export const HOT_CUE_NAMES = ["A", "B", "C", "D", "E", "F", "G", "H"];
