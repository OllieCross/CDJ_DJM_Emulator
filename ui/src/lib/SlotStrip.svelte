<script lang="ts">
  import { slotLabel, type Slot, type SlotView, type SourceView } from "./types";

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

  const slots: Slot[] = ["Usb", "Sd"];
  const free = $derived(sources.filter((s) => s.inserted === null));
  let picked = $state<Record<Slot, string>>({ Usb: "", Sd: "" });

  function inserted(slot: Slot): SlotView | undefined {
    return media.find((m) => m.slot === slot);
  }
</script>

<div class="slots">
  {#each slots as slot}
    {@const m = inserted(slot)}
    <div class="slot" class:filled={m}>
      <span class="label">{slotLabel(slot)}</span>
      {#if m}
        <span class="name" title={m.root}>{m.name}</span>
        <button title="Eject" onclick={() => run("eject_media", { player, slot })}>⏏</button>
      {:else}
        <select bind:value={picked[slot]} disabled={free.length === 0}>
          <option value="">{free.length === 0 ? "no exports found" : "— insert —"}</option>
          {#each free as s (s.root)}
            <option value={s.root}>{s.name}</option>
          {/each}
        </select>
        <button
          disabled={!picked[slot]}
          onclick={() => {
            run("insert_media", { player, slot, root: picked[slot] });
            picked[slot] = "";
          }}
        >
          Insert
        </button>
      {/if}
    </div>
  {/each}
</div>

<style>
  .slots {
    display: flex;
    gap: 8px;
  }

  .slot {
    flex: 1;
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
    background: var(--panel-2);
    color: var(--text);
    border: 1px solid var(--line);
    border-radius: 3px;
    font: inherit;
    font-size: 11px;
    padding: 2px 4px;
  }

  button {
    padding: 2px 7px;
    font-size: 11px;
  }
</style>
