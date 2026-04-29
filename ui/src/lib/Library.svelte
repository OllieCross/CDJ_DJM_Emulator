<script lang="ts">
  import { createEventDispatcher } from "svelte";

  export let tracks: {
    id: number;
    title: string;
    artist: string;
    bpm: number;
    duration_s: number;
  }[] = [];

  export let players: { player: number; track: { id: number } | null }[] = [];

  let selectedTrack: number | null = null;
  let targetPlayer: number = 1;
  let search = "";

  const dispatch = createEventDispatcher();

  $: filtered = tracks.filter(
    (t) =>
      t.title.toLowerCase().includes(search.toLowerCase()) ||
      t.artist.toLowerCase().includes(search.toLowerCase())
  );

  function loadSelected() {
    if (selectedTrack === null) return;
    dispatch("load", { player: targetPlayer, trackId: selectedTrack });
    selectedTrack = null;
  }

  function fmt(s: number) {
    const m = Math.floor(s / 60);
    const sec = s % 60;
    return `${m}:${sec.toString().padStart(2, "0")}`;
  }

  function playerForTrack(id: number) {
    return players.find((p) => p.track?.id === id)?.player ?? null;
  }
</script>

<div class="library">
  <div class="lib-header">
    <span class="lib-title">LIBRARY</span>
    <span class="count">{tracks.length} tracks</span>
  </div>

  <div class="search-row">
    <input
      class="search"
      type="text"
      placeholder="Search..."
      bind:value={search}
    />
  </div>

  <div class="track-list">
    {#each filtered as t (t.id)}
      {@const onPlayer = playerForTrack(t.id)}
      <div
        class="track-row"
        class:selected={selectedTrack === t.id}
        class:loaded={onPlayer !== null}
        on:click={() => (selectedTrack = t.id)}
        on:dblclick={loadSelected}
        role="option"
        aria-selected={selectedTrack === t.id}
        tabindex="0"
        on:keydown={(e) => e.key === "Enter" && loadSelected()}
      >
        <div class="track-main">
          <span class="track-title">{t.title}</span>
          <span class="track-artist">{t.artist}</span>
        </div>
        <div class="track-meta">
          {#if onPlayer !== null}
            <span class="on-player">P{onPlayer}</span>
          {/if}
          <span class="bpm">{t.bpm.toFixed(0)}</span>
          <span class="dur">{fmt(t.duration_s)}</span>
        </div>
      </div>
    {/each}

    {#if filtered.length === 0}
      <div class="empty">No tracks found</div>
    {/if}
  </div>

  <div class="load-bar">
    <select class="player-select" bind:value={targetPlayer}>
      {#each players as p}
        <option value={p.player}>Player {p.player}</option>
      {/each}
    </select>
    <button
      class="load-btn"
      disabled={selectedTrack === null}
      on:click={loadSelected}
    >
      Load
    </button>
  </div>
</div>

<style>
  .library {
    display: flex;
    flex-direction: column;
    height: 100%;
    background: #131313;
  }

  .lib-header {
    padding: 10px 12px;
    display: flex;
    justify-content: space-between;
    align-items: center;
    border-bottom: 1px solid #2a2a2a;
  }

  .lib-title {
    font-size: 10px;
    letter-spacing: 0.12em;
    color: #555;
  }

  .count {
    font-size: 10px;
    color: #444;
  }

  .search-row {
    padding: 8px 10px;
    border-bottom: 1px solid #222;
  }

  .search {
    width: 100%;
    background: #1a1a1a;
    border: 1px solid #333;
    border-radius: 3px;
    color: #eee;
    font-family: inherit;
    font-size: 12px;
    padding: 5px 8px;
    outline: none;
  }

  .search:focus {
    border-color: #0af;
  }

  .track-list {
    flex: 1;
    overflow-y: auto;
  }

  .track-row {
    padding: 7px 10px;
    display: flex;
    justify-content: space-between;
    align-items: center;
    cursor: pointer;
    border-bottom: 1px solid #1c1c1c;
    gap: 8px;
  }

  .track-row:hover {
    background: #1e1e1e;
  }

  .track-row.selected {
    background: #1a2a3a;
    border-color: #0af4;
  }

  .track-row.loaded {
    opacity: 0.7;
  }

  .track-main {
    flex: 1;
    min-width: 0;
  }

  .track-title {
    display: block;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    font-size: 12px;
    color: #ddd;
  }

  .track-artist {
    display: block;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    font-size: 11px;
    color: #0af;
    margin-top: 2px;
  }

  .track-meta {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 2px;
    font-size: 10px;
    color: #555;
    flex-shrink: 0;
  }

  .on-player {
    color: #0f8;
    font-weight: 700;
  }

  .bpm {
    color: #f0c040;
  }

  .empty {
    padding: 20px;
    color: #444;
    text-align: center;
    font-size: 12px;
  }

  .load-bar {
    padding: 10px;
    border-top: 1px solid #2a2a2a;
    display: flex;
    gap: 8px;
  }

  .player-select {
    flex: 1;
    background: #1a1a1a;
    border: 1px solid #333;
    border-radius: 3px;
    color: #eee;
    font-family: inherit;
    font-size: 12px;
    padding: 6px 8px;
  }

  .load-btn {
    padding: 6px 14px;
    background: #0af2;
    border: 1px solid #0af;
    border-radius: 3px;
    color: #0af;
    font-family: inherit;
    font-size: 12px;
    cursor: pointer;
    transition: background 0.15s;
  }

  .load-btn:hover:not(:disabled) {
    background: #0af4;
  }

  .load-btn:disabled {
    opacity: 0.3;
    cursor: default;
    border-color: #444;
    color: #555;
    background: transparent;
  }
</style>
