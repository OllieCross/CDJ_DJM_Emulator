<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount, onDestroy } from "svelte";
  import Deck from "./lib/Deck.svelte";
  import Library from "./lib/Library.svelte";

  interface TrackSummary {
    id: number;
    title: string;
    artist: string;
    album: string;
    bpm: number;
    duration_s: number;
  }

  interface PlayerStatus {
    player: number;
    playing: boolean;
    track: TrackSummary | null;
  }

  let players: PlayerStatus[] = [];
  let tracks: TrackSummary[] = [];
  let pollInterval: ReturnType<typeof setInterval>;

  async function refresh() {
    try {
      [players, tracks] = await Promise.all([
        invoke<PlayerStatus[]>("get_players"),
        invoke<TrackSummary[]>("list_tracks"),
      ]);
    } catch (e) {
      console.error("refresh failed:", e);
    }
  }

  async function handleLoad(e: CustomEvent<{ player: number; trackId: number }>) {
    try {
      await invoke("load_track", { player: e.detail.player, trackId: e.detail.trackId });
      await refresh();
    } catch (e) {
      console.error("load_track failed:", e);
    }
  }

  async function handlePlay(e: CustomEvent<{ player: number }>) {
    await invoke("play", { player: e.detail.player });
    await refresh();
  }

  async function handlePause(e: CustomEvent<{ player: number }>) {
    await invoke("pause", { player: e.detail.player });
    await refresh();
  }

  onMount(() => {
    refresh();
    pollInterval = setInterval(refresh, 500);
  });

  onDestroy(() => clearInterval(pollInterval));
</script>

<main>
  <header>
    <h1>CDJ Emulator</h1>
  </header>

  <div class="layout">
    <section class="decks">
      {#each players as p (p.player)}
        <Deck
          status={p}
          on:load={handleLoad}
          on:play={handlePlay}
          on:pause={handlePause}
        />
      {/each}
    </section>

    <aside class="library-panel">
      <Library {tracks} {players} on:load={handleLoad} />
    </aside>
  </div>
</main>

<style>
  :global(*, *::before, *::after) {
    box-sizing: border-box;
    margin: 0;
    padding: 0;
  }

  :global(body) {
    background: #111;
    color: #eee;
    font-family: "SF Mono", "Fira Code", monospace;
    font-size: 13px;
  }

  main {
    display: flex;
    flex-direction: column;
    height: 100vh;
    overflow: hidden;
  }

  header {
    padding: 10px 16px;
    background: #1a1a1a;
    border-bottom: 1px solid #333;
    display: flex;
    align-items: center;
  }

  header h1 {
    font-size: 14px;
    font-weight: 600;
    letter-spacing: 0.08em;
    color: #0af;
    text-transform: uppercase;
  }

  .layout {
    display: flex;
    flex: 1;
    overflow: hidden;
  }

  .decks {
    flex: 1;
    display: grid;
    grid-template-columns: 1fr 1fr;
    grid-template-rows: 1fr 1fr;
    gap: 1px;
    background: #222;
    overflow: hidden;
  }

  .library-panel {
    width: 320px;
    border-left: 1px solid #333;
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }
</style>
