<script lang="ts">
  import { createEventDispatcher } from "svelte";

  export let status: {
    player: number;
    playing: boolean;
    track: { id: number; title: string; artist: string; bpm: number; duration_s: number } | null;
  };

  const dispatch = createEventDispatcher();

  function toggle() {
    if (status.playing) {
      dispatch("pause", { player: status.player });
    } else {
      dispatch("play", { player: status.player });
    }
  }

  function fmt(s: number) {
    const m = Math.floor(s / 60);
    const sec = s % 60;
    return `${m}:${sec.toString().padStart(2, "0")}`;
  }
</script>

<div class="deck" class:playing={status.playing}>
  <div class="player-num">PLAYER {status.player}</div>

  {#if status.track}
    <div class="meta">
      <div class="title">{status.track.title}</div>
      <div class="artist">{status.track.artist}</div>
    </div>

    <div class="stats">
      <span class="bpm">{status.track.bpm.toFixed(2)} BPM</span>
      <span class="dur">{fmt(status.track.duration_s)}</span>
    </div>

    <div class="waveform-placeholder">
      <div class="playhead"></div>
    </div>

    <button class="play-btn" on:click={toggle}>
      {status.playing ? "|| PAUSE" : "> PLAY"}
    </button>
  {:else}
    <div class="empty">No track loaded</div>
    <div class="hint">Click a track in the library →</div>
  {/if}
</div>

<style>
  .deck {
    background: #161616;
    display: flex;
    flex-direction: column;
    padding: 14px;
    gap: 10px;
    position: relative;
    border: 1px solid #2a2a2a;
    transition: border-color 0.2s;
  }

  .deck.playing {
    border-color: #0af;
  }

  .player-num {
    font-size: 10px;
    letter-spacing: 0.12em;
    color: #555;
    text-transform: uppercase;
  }

  .deck.playing .player-num {
    color: #0af;
  }

  .meta {
    flex: 1;
  }

  .title {
    font-size: 15px;
    font-weight: 700;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    color: #fff;
  }

  .artist {
    font-size: 12px;
    color: #0af;
    margin-top: 3px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .stats {
    display: flex;
    gap: 16px;
    font-size: 12px;
    color: #888;
  }

  .bpm {
    color: #f0c040;
    font-weight: 600;
  }

  .waveform-placeholder {
    height: 48px;
    background: #1e1e1e;
    border-radius: 3px;
    position: relative;
    overflow: hidden;
    border: 1px solid #2a2a2a;
  }

  .playhead {
    position: absolute;
    left: 0;
    top: 0;
    bottom: 0;
    width: 2px;
    background: #0af;
    opacity: 0.7;
  }

  .play-btn {
    padding: 8px 0;
    background: #1e1e1e;
    color: #eee;
    border: 1px solid #333;
    border-radius: 4px;
    cursor: pointer;
    font-family: inherit;
    font-size: 12px;
    letter-spacing: 0.08em;
    transition: background 0.15s, border-color 0.15s;
  }

  .play-btn:hover {
    background: #252525;
    border-color: #0af;
  }

  .deck.playing .play-btn {
    border-color: #0af;
    color: #0af;
  }

  .empty {
    flex: 1;
    color: #444;
    font-size: 13px;
    display: flex;
    align-items: center;
  }

  .hint {
    font-size: 11px;
    color: #333;
  }
</style>
