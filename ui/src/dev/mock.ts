// Browser preview: a fake backend so the UI can be opened with `npm run dev`
// in a normal browser (no Tauri, no network, no audio). Only loaded in dev
// builds when the Tauri runtime is absent.

import { mockIPC } from "@tauri-apps/api/mocks";
import { emit } from "@tauri-apps/api/event";
import type {
  CrossfaderAssign,
  MediaContents,
  PlayerView,
  RigSnapshot,
  Slot,
  TempoRange,
  TrackSummary,
} from "../lib/types";

const TRACKS: TrackSummary[] = [
  { id: 1, title: "Midnight Signal", artist: "Northbound", album: "Low Light", bpm: 124, duration_s: 372, key: "8A" },
  { id: 2, title: "Concrete Bloom", artist: "Vela Moss", album: "Strata", bpm: 128, duration_s: 401, key: "11B" },
  { id: 3, title: "Tidal Lines", artist: "Oro & Kesh", album: "Tidal Lines EP", bpm: 122, duration_s: 345, key: "5A" },
  { id: 4, title: "Afterglow (Club Mix)", artist: "Halden", album: "Afterglow", bpm: 126, duration_s: 418, key: "2A" },
  { id: 5, title: "Paper Satellites", artist: "Northbound", album: "Low Light", bpm: 124, duration_s: 389, key: "8B" },
  { id: 6, title: "Hollow Bodies", artist: "Ines Varga", album: "Hollow Bodies", bpm: 130, duration_s: 356, key: "9A" },
];

const PLAYER_MODELS = [
  { id: "Cdj3000", name: "CDJ-3000", notes: "", hotCues: 8 },
  { id: "Cdj3000X", name: "CDJ-3000X", notes: "Treated like a CDJ-3000; its name on the network is unverified.", hotCues: 8 },
  { id: "Cdj2000Nxs2", name: "CDJ-2000NXS2", notes: "No absolute-position packets.", hotCues: 8 },
  { id: "Cdj2000Nxs", name: "CDJ-2000NXS", notes: "3 hot cues, no absolute-position packets.", hotCues: 3 },
  { id: "Cdj2000", name: "CDJ-2000", notes: "Pre-nexus: 3 hot cues, no absolute-position packets.", hotCues: 3 },
  { id: "Xdj1000Mk2", name: "XDJ-1000MK2", notes: "Status packet length unverified.", hotCues: 8 },
];
const MIXER_MODELS = [
  { id: "DjmV10", name: "DJM-V10", notes: "6 channels; the emulator's EQ is 3-band (the V10 has 4).", channels: 6 },
  { id: "DjmA9", name: "DJM-A9", notes: "", channels: 4 },
  { id: "Djm900Nxs2", name: "DJM-900NXS2", notes: "", channels: 4 },
  { id: "Djm750Mk2", name: "DJM-750MK2", notes: "", channels: 4 },
];

const CONTENTS: MediaContents = {
  name: "DEMO_USB",
  tracks: TRACKS,
  playlists: [
    { id: 10, parent_id: 0, name: "Show Opener", is_folder: false, track_ids: [3, 1, 5] },
    { id: 11, parent_id: 0, name: "Peak Time", is_folder: true, track_ids: [] },
    { id: 12, parent_id: 11, name: "Bangers", is_folder: false, track_ids: [2, 6, 4] },
    { id: 13, parent_id: 0, name: "Warm-up", is_folder: false, track_ids: [3, 5] },
    { id: 14, parent_id: 0, name: "Timecode cues", is_folder: false, track_ids: [1, 2, 4, 6] },
    { id: 15, parent_id: 0, name: "Lasers + Pyro", is_folder: false, track_ids: [6, 4] },
    { id: 16, parent_id: 0, name: "Closing", is_folder: false, track_ids: [4] },
  ],
};

function emptyPlayer(n: number): PlayerView {
  return {
    number: n,
    running: false,
    play_state: "Off",
    playing: false,
    master: false,
    synced: false,
    on_air: false,
    bpm: 0,
    track_bpm: 0,
    pitch_percent: 0,
    pitch_fader: 0,
    tempo_range: "Ten",
    master_tempo: false,
    reverse: false,
    quantize: true,
    beat_within_bar: 0,
    beat_number: 0,
    position_ms: 0,
    length_ms: null,
    cue_ms: 0,
    hot_cues: Array(8).fill(null),
    memory_cues: [],
    active_loop: null,
    loop_in_ms: null,
    loaded: null,
    media: [],
    model: null,
    model_name: null,
    hot_cue_slots: 8,
  };
}

const RANGE: Record<TempoRange, number> = { Six: 0.06, Ten: 0.1, Sixteen: 0.16, Wide: 1 };

const state: RigSnapshot = {
  network: {
    layout: "SharedSubnet",
    emulator_iface: "feth0",
    client_iface: "feth1",
    client_ip: "10.77.77.200",
    status: { pair_exists: true, client_ip_up: true, missing_device_ips: [], emulator_route_up: true },
    ready: true,
  },
  players: [1, 2, 3, 4].map(emptyPlayer),
  mixer: {
    running: true,
    model: "DjmV10",
    model_name: "DJM-V10",
    channels: 6,
    controls: {
      channel_faders: [1, 1, 0.8, 0, 1, 1],
      crossfader: 0.5,
      assign: ["Thru", "Thru", "A", "B", "Thru", "Thru"] as CrossfaderAssign[],
      trim_db: [0, 0, 0, 0, 0, 0],
      eq_db: [
        [0, 0, 0],
        [-26, 0, 2],
        [0, 0, 0],
        [0, 0, 0],
        [0, 0, 0],
        [0, 0, 0],
      ],
      master: 0.9,
    },
  },
  sources: [{ root: "/Volumes/DEMO_USB", name: "DEMO_USB", inserted: [1, "Usb"] }],
  audio: { device: "MacBook Pro Speakers", muted: false },
  remote: {
    listening: true,
    master: null,
    log: [
      { at_ms: Date.now() - 4_000, from: "beat-link", text: "turned sync on for player 2" },
      { at_ms: Date.now() - 19_000, from: "beat-link", text: "loaded track 4 from player 1's USB on player 3" },
      { at_ms: Date.now() - 21_000, from: "beat-link", text: "asked what is in player 1's USB slot" },
    ],
  },
};

function player(n: number): PlayerView {
  return state.players[n - 1];
}

function load(n: number, id: number, source: number, slot: Slot) {
  const p = player(n);
  const t = TRACKS.find((t) => t.id === id)!;
  const beat = 60_000 / t.bpm;
  Object.assign(p, {
    loaded: { track: t, source_player: source, source_slot: slot },
    play_state: "Cued",
    playing: false,
    length_ms: t.duration_s * 1000,
    position_ms: beat * 16,
    cue_ms: beat * 16,
    track_bpm: t.bpm,
    memory_cues: [beat * 16, beat * 80, beat * 208],
    hot_cues: [
      { start_ms: beat * 48, loop_end_ms: null, color: [255, 55, 111] },
      { start_ms: beat * 112, loop_end_ms: null, color: [48, 210, 110] },
      { start_ms: beat * 176, loop_end_ms: beat * 184, color: [255, 160, 0] },
      null,
      null,
      null,
      null,
      null,
    ],
    active_loop: null,
    loop_in_ms: null,
  });
}

function power(n: number, on: boolean, modelId = "Cdj3000") {
  const p = player(n);
  const media = p.media;
  const model = PLAYER_MODELS.find((m) => m.id === modelId) ?? PLAYER_MODELS[0];
  Object.assign(p, emptyPlayer(n), {
    media,
    running: on,
    play_state: on ? "NoTrack" : "Off",
    model: on ? model.id : null,
    model_name: on ? model.name : null,
    hot_cue_slots: on ? model.hotCues : 8,
  });
}

// Demo rig: players 1-3 on and loaded, 1 (master) and 3 playing, 4 off.
power(1, true, "Cdj3000X");
power(2, true, "Cdj3000X");
power(3, true, "Cdj2000Nxs");
player(1).media = [{ slot: "Usb", name: "DEMO_USB", root: "/Volumes/DEMO_USB" }];
load(1, 1, 1, "Usb");
load(2, 2, 1, "Usb");
load(3, 3, 1, "Usb");
Object.assign(player(1), { playing: true, play_state: "Playing", master: true, pitch_fader: 0.2, position_ms: 96_000 });
Object.assign(player(2), { synced: true });
Object.assign(player(3), { playing: true, play_state: "Playing", position_ms: 182_000 });

function tick(dtMs: number) {
  const mixer = state.mixer;
  for (const p of state.players) {
    if (!p.running) continue;
    const pitch = p.synced ? 0 : p.pitch_fader * RANGE[p.tempo_range ?? "Ten"];
    p.pitch_percent = pitch * 100;
    p.bpm = p.loaded ? p.track_bpm * (1 + pitch) : 0;
    const ch = p.number - 1;
    const a = mixer.controls.assign[ch];
    const xfOpen = a === "Thru" || (a === "A" ? mixer.controls.crossfader < 1 : mixer.controls.crossfader > 0);
    p.on_air = mixer.running && mixer.controls.channel_faders[ch] > 0 && xfOpen;
    if (!p.loaded) continue;
    if (p.playing) {
      p.position_ms += dtMs * (1 + pitch) * (p.reverse ? -1 : 1);
      const loop = p.active_loop;
      if (loop && p.position_ms >= loop.end_ms) p.position_ms = loop.start_ms + (p.position_ms - loop.end_ms);
      if (p.position_ms >= (p.length_ms ?? Infinity)) {
        p.position_ms = p.length_ms!;
        p.playing = false;
        p.play_state = "Ended";
      }
    }
    const beatMs = 60_000 / p.track_bpm;
    const index = Math.floor(p.position_ms / beatMs);
    p.beat_number = index + 1;
    p.beat_within_bar = (index % 4) + 1;
  }
}

/// Repeatable pseudo-random 0..1 for integer `n`.
function noise(n: number): number {
  const x = Math.sin(n * 12.9898 + 78.233) * 43758.5453;
  return x - Math.floor(x);
}

/// Band energies (low, mid, high; 0..1) at `ms` of a club track: intro and
/// outro with kick and hats, a breakdown without kick, full drops.
function bands(t: TrackSummary, ms: number): [number, number, number] {
  const beatMs = 60_000 / t.bpm;
  const f = ms / (t.duration_s * 1000);
  const beat = ms / beatMs;
  const phase = beat - Math.floor(beat);
  const bar = Math.floor(beat / 4);
  const section = f < 0.1 ? "intro" : f < 0.2 ? "build" : f < 0.45 ? "drop" : f < 0.55 ? "break" : f < 0.85 ? "drop" : "outro";
  const kick = section === "break" ? 0 : Math.exp(-phase * 6);
  const bass = section === "drop" ? 0.55 * (1 - Math.exp(-phase * 3)) : 0;
  const hats = section === "intro" ? 0.25 : 0.35 + 0.45 * Math.exp(-Math.abs(phase - 0.5) * 12);
  const pad = section === "break" ? 0.55 + 0.25 * Math.sin(beat / 3) : section === "drop" ? 0.45 : section === "build" ? 0.2 + 0.5 * ((f - 0.1) / 0.1) : 0.15;
  const jitter = 0.85 + 0.3 * noise(Math.floor(ms / 7) + bar);
  return [
    Math.min(1, (0.95 * kick + bass) * jitter),
    Math.min(1, pad * jitter),
    Math.min(1, hats * (0.8 + 0.4 * noise(Math.floor(ms / 3)))),
  ];
}

/// `[height, r, g, b]` for band energies, coloured the way rekordbox's RGB
/// waveform is (lows red, mids green, highs blue).
function column([low, mid, high]: [number, number, number], out: number[]) {
  const top = Math.max(low, mid, high, 0.01);
  out.push(255 * top, (255 * low) / top, (200 * mid) / top, (255 * high) / top);
}

/// Synthetic waveforms in the layout `track_waveforms` returns.
function waveforms(t: TrackSummary): ArrayBuffer {
  const lengthMs = t.duration_s * 1000;
  const overview: number[] = [];
  for (let i = 0; i < 400; i++) {
    // Average a few points per column so kicks don't alias.
    const sum: [number, number, number] = [0, 0, 0];
    for (let k = 0; k < 8; k++) {
      const b = bands(t, ((i + k / 8) / 400) * lengthMs);
      sum[0] += b[0] / 8;
      sum[1] += b[1] / 8;
      sum[2] += b[2] / 8;
    }
    column(sum, overview);
  }
  const detail: number[] = [];
  for (let i = 0; i < t.duration_s * 150; i++) column(bands(t, (i / 150) * 1000), detail);
  const buf = new ArrayBuffer(8 + overview.length + detail.length);
  const view = new DataView(buf);
  view.setUint32(0, overview.length / 4, true);
  view.setUint32(4, detail.length / 4, true);
  new Uint8Array(buf, 8).set([...overview, ...detail].map((v) => Math.max(0, Math.min(255, Math.round(v)))));
  return buf;
}

type Args = Record<string, any>;

function handle(cmd: string, a: Args = {}): unknown {
  const p = a.player ? player(a.player) : null;
  switch (cmd) {
    case "rig_snapshot":
      return structuredClone(state);
    case "spawn_player":
      return power(a.player, true, a.model);
    case "despawn_player":
      return power(a.player, false);
    case "spawn_mixer": {
      const m = MIXER_MODELS.find((x) => x.id === a.model) ?? MIXER_MODELS[0];
      Object.assign(state.mixer, { running: true, model: m.id, model_name: m.name, channels: m.channels });
      return;
    }
    case "models":
      return {
        players: PLAYER_MODELS.map(({ id, name, notes }) => ({ id, name, notes })),
        mixers: MIXER_MODELS.map(({ id, name, notes }) => ({ id, name, notes })),
      };
    case "despawn_mixer":
      Object.assign(state.mixer, { running: false, model: null, model_name: null });
      return;
    case "media_contents":
      return CONTENTS;
    case "load_track":
      return load(a.player, a.trackId, a.sourcePlayer, a.slot);
    case "insert_media": {
      const src = state.sources.find((s) => s.root === a.root);
      if (src) src.inserted = [a.player, a.slot];
      p!.media.push({ slot: a.slot, name: src?.name ?? "USB", root: a.root });
      return;
    }
    case "eject_media":
      p!.media = p!.media.filter((m) => m.slot !== a.slot);
      state.sources.forEach((s) => {
        if (s.inserted && s.inserted[0] === a.player && s.inserted[1] === a.slot) s.inserted = null;
      });
      return;
    case "play_pause":
    case "play":
    case "pause":
      if (!p?.loaded) return;
      p.playing = cmd === "play" || (cmd === "play_pause" && !p.playing);
      p.play_state = p.playing ? (p.active_loop ? "Looping" : "Playing") : "Paused";
      if (p.playing && !state.players.some((x) => x.master)) p.master = true;
      return;
    case "cue_press":
      if (!p?.loaded) return;
      if (p.playing) {
        p.playing = false;
        p.position_ms = p.cue_ms;
      } else p.cue_ms = p.position_ms;
      p.play_state = "Cued";
      return;
    case "seek":
      if (p) p.position_ms = Math.max(0, a.ms);
      return;
    case "hot_cue": {
      const cue = p?.hot_cues[a.pad];
      if (!p || !cue) return;
      p.position_ms = cue.start_ms;
      p.playing = true;
      p.active_loop = cue.loop_end_ms ? { start_ms: cue.start_ms, end_ms: cue.loop_end_ms, beats: null, active: true } : null;
      p.play_state = p.active_loop ? "Looping" : "Playing";
      return;
    }
    case "beat_loop": {
      if (!p?.loaded) return;
      const beat = 60_000 / p.track_bpm;
      const start = Math.floor(p.position_ms / beat) * beat;
      p.active_loop = { start_ms: start, end_ms: start + beat * a.beats, beats: a.beats, active: true };
      return;
    }
    case "exit_loop":
      if (p) p.active_loop = null;
      return;
    case "loop_in":
      if (p?.loaded) p.loop_in_ms = p.cue_ms = p.position_ms;
      return;
    case "loop_out":
      if (p?.loop_in_ms != null && p.position_ms > p.loop_in_ms) {
        p.active_loop = { start_ms: p.loop_in_ms, end_ms: p.position_ms, beats: null, active: true };
        p.position_ms = p.loop_in_ms;
      }
      if (p) p.loop_in_ms = null;
      return;
    case "clear_hot_cue":
      if (p) p.hot_cues[a.pad] = null;
      return;
    case "set_pitch_fader":
      if (p) p.pitch_fader = a.position;
      return;
    case "set_tempo_range":
      if (p) p.tempo_range = a.range;
      return;
    case "set_master_tempo":
      if (p) p.master_tempo = a.on;
      return;
    case "set_reverse":
      if (p) p.reverse = a.on;
      return;
    case "set_quantize":
      if (p) p.quantize = a.on;
      return;
    case "set_master":
      state.players.forEach((x) => (x.master = x.number === a.player));
      return;
    case "set_sync":
      if (p) p.synced = a.on;
      return;
    case "set_channel_fader":
      state.mixer.controls.channel_faders[a.channel - 1] = a.level;
      return;
    case "set_crossfader":
      state.mixer.controls.crossfader = a.position;
      return;
    case "set_crossfader_assign":
      state.mixer.controls.assign[a.channel - 1] = a.assign;
      return;
    case "set_trim":
      state.mixer.controls.trim_db[a.channel - 1] = a.db;
      return;
    case "set_eq":
      state.mixer.controls.eq_db[a.channel - 1][a.band] = a.db;
      return;
    case "set_master_level":
      state.mixer.controls.master = a.level;
      return;
    case "audio_devices":
      return ["MacBook Pro Speakers", "BlackHole 2ch", "JBL Xtreme 2"];
    case "set_audio_output":
      state.audio = { device: a.muted ? null : (a.device ?? "MacBook Pro Speakers"), muted: a.muted };
      return;
    case "track_waveforms":
      return p?.loaded ? waveforms(p.loaded.track) : new ArrayBuffer(8);
    case "track_grid": {
      if (!p?.loaded) return [];
      const beat = 60_000 / p.track_bpm;
      return Array.from({ length: Math.floor((p.length_ms ?? 0) / beat) }, (_, i) => [i * beat, (i % 4) + 1]);
    }
    case "track_artwork":
      return new ArrayBuffer(0);
    case "network_setup":
    case "network_teardown":
    case "add_media_folder":
    case "pick_media_folder":
      return;
    default:
      return;
  }
}

export function installMockBackend() {
  mockIPC((cmd, args) => handle(cmd, args as Args), { shouldMockEvents: true });
  let last = performance.now();
  setInterval(() => {
    const now = performance.now();
    tick(now - last);
    last = now;
    emit("rig-snapshot", structuredClone(state));
  }, 100);
  console.info("CDJ Emulator: browser preview with a mock backend");
}
