// Player and mixer models, as the backend's `models` command lists them.

export interface ModelInfo {
  /// Backend id, e.g. "Cdj3000", "DjmA9".
  id: string;
  name: string;
  /// What about this model is a guess; empty when nothing.
  notes: string;
}

export interface ModelsView {
  players: ModelInfo[];
  mixers: ModelInfo[];
}

export const DEFAULT_PLAYER_MODEL = "Cdj3000";
export const DEFAULT_MIXER_MODEL = "DjmV10";

/// The model last chosen for `key` (a deck or the mixer), remembered in
/// this browser so a deck powers on as what it was last time.
export function rememberedModel(key: string, fallback: string): string {
  try {
    return localStorage.getItem(`model:${key}`) ?? fallback;
  } catch {
    return fallback;
  }
}

export function rememberModel(key: string, id: string) {
  try {
    localStorage.setItem(`model:${key}`, id);
  } catch {
    // Not stored; the choice still applies now.
  }
}
