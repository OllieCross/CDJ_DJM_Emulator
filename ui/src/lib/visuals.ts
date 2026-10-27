// Track visuals (waveforms, beat grid, artwork) fetched once per loaded
// track and cached.

import { invoke } from "@tauri-apps/api/core";
import type { PlayerView } from "./types";

/// Detail waveforms have 150 columns per second of track.
export const DETAIL_COLS_PER_SECOND = 150;

export interface TrackVisuals {
  /// `[height, r, g, b]` per column.
  overview: Uint8Array;
  detail: Uint8Array;
  /// Beat times (ms) and their position in the bar.
  beats: [number, number][];
  /// Object URL of the artwork JPEG, if any.
  artwork: string | null;
}

const cache = new Map<string, Promise<TrackVisuals>>();

/// Identifies the track on a deck (changes when another one is loaded).
export function trackKey(p: PlayerView): string | null {
  const l = p.loaded;
  return l ? `${p.number}:${l.source_player}:${l.source_slot}:${l.track.id}` : null;
}

export function loadVisuals(player: number, key: string): Promise<TrackVisuals> {
  let entry = cache.get(key);
  if (!entry) {
    entry = (async () => {
      const [waves, beats, art] = await Promise.all([
        invoke<ArrayBuffer>("track_waveforms", { player }),
        invoke<[number, number][]>("track_grid", { player }),
        invoke<ArrayBuffer>("track_artwork", { player }),
      ]);
      const header = new DataView(waves, 0, 8);
      const overviewCols = header.getUint32(0, true);
      const detailCols = header.getUint32(4, true);
      const overview = new Uint8Array(waves, 8, overviewCols * 4);
      const detail = new Uint8Array(waves, 8 + overviewCols * 4, detailCols * 4);
      const artwork =
        art.byteLength > 0 ? URL.createObjectURL(new Blob([art], { type: "image/jpeg" })) : null;
      return { overview, detail, beats, artwork };
    })();
    entry.catch(() => cache.delete(key));
    cache.set(key, entry);
  }
  return entry;
}

/// Playhead position now, extrapolated from the last snapshot (taken at
/// `receivedAt`, a `performance.now()` time) so drawing stays smooth.
export function livePosition(p: PlayerView, receivedAt: number, now: number): number {
  if (!p.playing) return p.position_ms;
  const speed = (1 + p.pitch_percent / 100) * (p.reverse ? -1 : 1);
  const ms = p.position_ms + (now - receivedAt) * speed;
  const end = p.length_ms ?? Infinity;
  return Math.min(Math.max(0, ms), end);
}

export function rgb(r: number, g: number, b: number, a = 1): string {
  return `rgba(${r}, ${g}, ${b}, ${a})`;
}
