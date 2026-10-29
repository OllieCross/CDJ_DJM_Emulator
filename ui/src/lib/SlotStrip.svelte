<script lang="ts">
  import Icon from "./Icon.svelte";
  import { slotLabel, type SlotView, type SourceView } from "./types";

  let {
    player,
    media,
    sources,
    run,
  }: {
    player: number;
    media: SlotView[];
    sources: SourceView[];
    run: (command: string, args?: Record<string, unknown>) => Promise<void>;
  } = $props();

  // One media slot per player. On the network it is the player's USB
  // slot, which is where client apps expect rekordbox exports.
  const slot = "Usb";
  const free = $derived(sources.filter((s) => s.inserted === null));
  const m = $derived(media.find((x) => x.slot === slot));
  let picked = $state("");
</script>

<div class="slot" class:filled={m}>
  <span class="label"><Icon name="drive" size={12} /> {slotLabel(slot)}</span>
  {#if m}
    <span class="name" title={m.root}>{m.name}</span>
    <button title="Eject" onclick={() => run("eject_media", { player, slot })}>⏏</button>
  {:else}
    <select bind:value={picked} disabled={free.length === 0} aria-label="Media for player {player}">
      <option value="">{free.length === 0 ? "No drives or folders with an export" : "Choose a drive or folder"}</option>
      {#each free as s (s.root)}
        <option value={s.root}>{s.name}</option>
      {/each}
    </select>
    <button
      disabled={!picked}
      onclick={() => {
        run("insert_media", { player, slot, root: picked });
        picked = "";
      }}
    >
      Insert
    </button>
  {/if}
</div>

<style>
  .slot {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    padding: 4px 6px;
    border: 1px dashed var(--line);
    border-radius: 4px;
  }

  .slot.filled {
    border-style: solid;
    border-color: #23476b;
    background: #0f1d2b;
  }

  .label {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 0.08em;
    color: var(--muted);
  }

  .slot.filled .label {
    color: var(--accent);
  }

  .name {
    flex: 1;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  select {
    flex: 1;
    min-width: 0;
    font-size: 11px;
    padding-top: 2px;
    padding-bottom: 2px;
  }

  button {
    padding: 2px 7px;
    font-size: 11px;
  }
</style>
