<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import Icon from "./Icon.svelte";
  import { formatTime, slotLabel, type MediaContents, type PlayerView, type TrackSummary } from "./types";

  let {
    players,
    run,
  }: {
    players: PlayerView[];
    run: (command: string, args?: Record<string, unknown>) => Promise<void>;
  } = $props();

  // Media any running player can reach: its own slots and, over the link,
  // the slots of the other running players.
  const sources = $derived(
    players
      .filter((p) => p.running)
      .flatMap((p) => p.media.map((m) => ({ player: p.number, slot: m.slot, name: m.name }))),
  );
  const running = $derived(players.filter((p) => p.running).map((p) => p.number));

  let sourceKey = $state("");
  const source = $derived(sources.find((s) => `${s.player}:${s.slot}` === sourceKey) ?? null);
  // A string, so it only changes when the source does (snapshots arrive
  // every 100 ms with fresh objects).
  const activeSource = $derived(source ? `${source.player}:${source.slot}:${source.name}` : "");

  let contents = $state<MediaContents | null>(null);
  let loadError = $state<string | null>(null);

  type Category = "PLAYLIST" | "TRACK" | "ARTIST" | "ALBUM";
  const categories: Category[] = ["PLAYLIST", "TRACK", "ARTIST", "ALBUM"];
  let category = $state<Category>("PLAYLIST");
  /// Drill-down inside the category: folder ids, or an artist/album name.
  let path = $state<{ label: string; key: string | number }[]>([]);
  let search = $state("");
  let selected = $state(0);
  let target = $state<number | null>(null);

  // Fetch the export's contents whenever the chosen source changes.
  $effect(() => {
    const key = activeSource;
    contents = null;
    loadError = null;
    path = [];
    selected = 0;
    if (!key) return;
    const [player, slot] = key.split(":");
    invoke<MediaContents>("media_contents", { player: Number(player), slot })
      .then((c) => (contents = c))
      .catch((e) => (loadError = String(e)));
  });

  // Keep a source selected as players come and go.
  $effect(() => {
    if (!source && sources.length > 0) sourceKey = `${sources[0].player}:${sources[0].slot}`;
  });

  type Row =
    | { kind: "folder"; label: string; key: string | number; detail?: string }
    | { kind: "track"; track: TrackSummary; position?: number };

  const byTitle = (a: TrackSummary, b: TrackSummary) => a.title.localeCompare(b.title);

  const rows = $derived.by((): Row[] => {
    if (!contents) return [];
    const c = contents;
    const q = search.trim().toLowerCase();
    if (q) {
      return c.tracks
        .filter((t) => `${t.title} ${t.artist} ${t.album}`.toLowerCase().includes(q))
        .sort(byTitle)
        .map((track) => ({ kind: "track", track }));
    }
    const tracks = (list: TrackSummary[], numbered = false): Row[] =>
      list.map((track, i) => ({ kind: "track", track, position: numbered ? i + 1 : undefined }));
    const groups = (field: "artist" | "album"): Row[] => {
      const counts = new Map<string, number>();
      for (const t of c.tracks) counts.set(t[field] || "(none)", (counts.get(t[field] || "(none)") ?? 0) + 1);
      return [...counts]
        .sort(([a], [b]) => a.localeCompare(b))
        .map(([name, n]) => ({ kind: "folder", label: name, key: name, detail: `${n}` }));
    };
    const last = path[path.length - 1];
    switch (category) {
      case "TRACK":
        return tracks([...c.tracks].sort(byTitle));
      case "ARTIST":
      case "ALBUM": {
        const field = category === "ARTIST" ? "artist" : "album";
        if (!last) return groups(field);
        return tracks(c.tracks.filter((t) => (t[field] || "(none)") === last.key).sort(byTitle));
      }
      case "PLAYLIST": {
        const parent = last ? Number(last.key) : 0;
        const node = c.playlists.find((p) => p.id === parent);
        if (node && !node.is_folder) {
          const byId = new Map(c.tracks.map((t) => [t.id, t]));
          return tracks(node.track_ids.map((id) => byId.get(id)).filter((t): t is TrackSummary => !!t), true);
        }
        return c.playlists
          .filter((p) => p.parent_id === parent)
          .map((p) => ({
            kind: "folder",
            label: p.name,
            key: p.id,
            detail: p.is_folder ? "" : `${p.track_ids.length}`,
          }));
      }
    }
  });

  $effect(() => {
    if (selected >= rows.length) selected = Math.max(0, rows.length - 1);
  });

  function setCategory(c: Category) {
    category = c;
    path = [];
    search = "";
    selected = 0;
  }

  function back() {
    if (path.length) {
      path = path.slice(0, -1);
      selected = 0;
    }
  }

  function open(row: Row) {
    if (row.kind === "folder") {
      path = [...path, { label: row.label, key: row.key }];
      selected = 0;
    } else {
      load(row.track.id);
    }
  }

  function load(trackId: number, deck = target ?? running[0]) {
    if (!source || deck === undefined) return;
    target = deck;
    run("load_track", { player: deck, sourcePlayer: source.player, slot: source.slot, trackId });
  }

  function loadSelected(deck: number) {
    const row = rows[selected];
    if (row?.kind === "track") load(row.track.id, deck);
  }

  function key(e: KeyboardEvent) {
    if (e.target instanceof HTMLInputElement) return;
    if (e.key === "ArrowDown") selected = Math.min(rows.length - 1, selected + 1);
    else if (e.key === "ArrowUp") selected = Math.max(0, selected - 1);
    else if (e.key === "Enter" || e.key === "ArrowRight") rows[selected] && open(rows[selected]);
    else if (e.key === "Backspace" || e.key === "ArrowLeft") back();
    else if (/^[1-4]$/.test(e.key)) loadSelected(Number(e.key));
    else return;
    e.preventDefault();
    document.getElementById(`browse-row-${selected}`)?.scrollIntoView({ block: "nearest" });
  }

  function deckWith(id: number): number[] {
    if (!source) return [];
    return players
      .filter(
        (p) =>
          p.loaded?.track.id === id && p.loaded.source_player === source.player && p.loaded.source_slot === source.slot,
      )
      .map((p) => p.number);
  }

  async function addFolder() {
    let path: string | null = null;
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const picked = await open({ directory: true, title: "Choose a rekordbox export (the folder that contains PIONEER)" });
      path = typeof picked === "string" ? picked : null;
    } catch {
      // Not in the app (browser preview): ask for a path instead.
      path = prompt("Path of a rekordbox export (the folder that contains PIONEER):");
    }
    if (path) run("add_media_folder", { path });
  }
</script>

<!-- Keyboard browsing like a CDJ's rotary: ↑↓ move, → / Enter open or load, ← back, 1-4 load to a deck. -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
<section class="browse" tabindex="0" onkeydown={key} aria-label="Browse">
  <div class="tabs">
    <span class="title"><Icon name="browse" size={12} /> BROWSE</span>
    {#each sources as s (`${s.player}:${s.slot}`)}
      <button
        class="tab"
        class:on={sourceKey === `${s.player}:${s.slot}`}
        title={s.name}
        onclick={() => (sourceKey = `${s.player}:${s.slot}`)}
      >
        <Icon name={s.slot === "Usb" ? "usb" : "sd"} size={11} />
        P{s.player} {slotLabel(s.slot)}
      </button>
    {/each}
    <span class="spacer"></span>
    <button class="add" title="Add an export folder from this Mac" onclick={addFolder}>+ FOLDER</button>
  </div>

  {#if contents}
    <div class="cats">
      {#each categories as c}
        <button class:on={category === c && !search} onclick={() => setCategory(c)}>{c}</button>
      {/each}
      <input type="search" placeholder="SEARCH" bind:value={search} oninput={() => (selected = 0)} />
    </div>

    <div class="crumbs">
      {#if path.length && !search}
        <button class="back" title="Back (←)" onclick={back}>‹</button>
      {/if}
      <span class="media">{source?.name}</span>
      <span>› {search ? `SEARCH “${search}”` : category}</span>
      {#if !search}
        {#each path as p}<span>› {p.label}</span>{/each}
      {/if}
      <span class="spacer"></span>
      <span class="count">{rows.length}</span>
    </div>

    <div class="list" role="listbox" aria-label="Tracks">
      {#each rows as row, i (row.kind === "track" ? `t${row.track.id}:${i}` : `f${row.key}`)}
        <div
          id="browse-row-{i}"
          class="row"
          class:sel={selected === i}
          role="option"
          aria-selected={selected === i}
          tabindex="-1"
          onclick={() => (selected = i)}
          ondblclick={() => open(row)}
          onkeydown={() => {}}
        >
          {#if row.kind === "folder"}
            <span class="glyph">{category === "PLAYLIST" ? (row.detail === "" ? "▸" : "♪") : "•"}</span>
            <span class="main">{row.label}</span>
            {#if row.detail}<span class="meta">{row.detail}</span>{/if}
            <span class="chev">›</span>
          {:else}
            {@const t = row.track}
            {#if row.position}<span class="pos">{row.position}</span>{/if}
            <span class="main">
              <span class="t">{t.title}</span>
              <span class="a">{t.artist}</span>
            </span>
            {#each deckWith(t.id) as d}<span class="deck">{d}</span>{/each}
            <span class="bpm">{t.bpm.toFixed(1)}</span>
            <span class="key">{t.key}</span>
            <span class="len">{formatTime(t.duration_s * 1000)}</span>
          {/if}
        </div>
      {:else}
        <div class="empty">{search ? "No matches" : "Empty"}</div>
      {/each}
    </div>
  {:else}
    <div class="empty big">
      {loadError ?? (sources.length ? "Reading media…" : "Power on a player, then insert a USB/SD export into one of its slots.")}
    </div>
  {/if}

  <div class="load">
    <span>LOAD TO</span>
    {#each [1, 2, 3, 4] as d}
      <button
        disabled={!running.includes(d) || rows[selected]?.kind !== "track"}
        class:last={target === d}
        title="Load the selected track onto player {d} (key {d})"
        onclick={() => loadSelected(d)}
      >
        {d}
      </button>
    {/each}
  </div>
</section>

<style>
  .browse {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
    background: #050507;
    outline: none;
  }

  .browse:focus-visible {
    box-shadow: inset 0 0 0 1px var(--accent);
  }

  .tabs,
  .cats,
  .crumbs,
  .load {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 5px 8px;
  }

  .tabs {
    background: #121215;
    border-bottom: 1px solid var(--line);
  }

  .title {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-weight: 800;
    font-size: 11px;
    letter-spacing: 0.1em;
    color: var(--accent);
    margin-right: 4px;
  }

  .tab,
  .cats button,
  .add,
  .load button {
    padding: 3px 7px;
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 0.04em;
  }

  .tab {
    display: inline-flex;
    align-items: center;
    gap: 3px;
  }

  .tab.on,
  .cats button.on {
    color: #fff;
    background: #1d5fa8;
    border-color: #2f7fd6;
  }

  .spacer {
    flex: 1;
  }

  .cats input {
    flex: 1;
    min-width: 60px;
    background: #121215;
    color: var(--text);
    border: 1px solid var(--line);
    border-radius: 3px;
    font: inherit;
    font-size: 11px;
    padding: 3px 6px;
  }

  .crumbs {
    font-size: 10px;
    color: var(--muted);
    border-bottom: 1px solid #1b1b20;
    white-space: nowrap;
    overflow: hidden;
  }

  .crumbs .media {
    color: #d0d0d6;
    font-weight: 700;
  }

  .back {
    padding: 0 6px;
    font-size: 12px;
  }

  .count {
    color: #5a5a64;
  }

  .list {
    flex: 1;
    overflow-y: auto;
  }

  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 5px 10px;
    border-bottom: 1px solid #111115;
    cursor: default;
    font-size: 12px;
  }

  .row:hover {
    background: #111117;
  }

  .row.sel {
    background: #1d5fa8;
  }

  .glyph,
  .chev,
  .pos {
    color: #5a5a64;
    width: 14px;
    text-align: center;
    font-size: 11px;
  }

  .row.sel .glyph,
  .row.sel .chev,
  .row.sel .pos {
    color: #cfe3ff;
  }

  .main {
    flex: 1;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .t,
  .a {
    display: block;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .a {
    font-size: 10px;
    color: #8fa2bb;
  }

  .row.sel .a {
    color: #cfe3ff;
  }

  .meta,
  .bpm,
  .key,
  .len {
    font-size: 10px;
    font-variant-numeric: tabular-nums;
    color: #8a8a92;
  }

  .bpm {
    width: 34px;
    text-align: right;
    color: #e5c35a;
  }

  .key {
    width: 26px;
    color: #4cd964;
  }

  .len {
    width: 30px;
    text-align: right;
  }

  .row.sel .bpm,
  .row.sel .key,
  .row.sel .len {
    color: #fff;
  }

  .deck {
    font-size: 9px;
    font-weight: 800;
    color: #000;
    background: var(--play);
    border-radius: 2px;
    padding: 0 4px;
  }

  .empty {
    padding: 16px;
    color: var(--muted);
    text-align: center;
    font-size: 11px;
  }

  .empty.big {
    flex: 1;
    display: grid;
    place-items: center;
  }

  .load {
    border-top: 1px solid var(--line);
    background: #121215;
    font-size: 10px;
    font-weight: 700;
    color: var(--muted);
  }

  .load button {
    flex: 1;
    font-size: 12px;
  }

  .load button.last:not(:disabled) {
    border-color: var(--accent);
    color: var(--accent);
  }
</style>
