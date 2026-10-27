<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import Fader from "./Fader.svelte";
  import Icon from "./Icon.svelte";
  import SlotStrip from "./SlotStrip.svelte";
  import WaveOverview from "./WaveOverview.svelte";
  import WaveZoom from "./WaveZoom.svelte";
  import {
    HOT_CUE_NAMES,
    formatTimeTenths,
    slotLabel,
    type PlayerView,
    type SourceView,
    type TempoRange,
  } from "./types";
  import { capture } from "./pointer";
  import { livePosition, loadVisuals, trackKey, type TrackVisuals } from "./visuals";

  let {
    player,
    sources,
    networkReady,
    run,
  }: {
    player: PlayerView;
    sources: SourceView[];
    networkReady: boolean;
    run: (command: string, args?: Record<string, unknown>) => Promise<void>;
  } = $props();

  const n = $derived(player.number);
  const track = $derived(player.loaded?.track ?? null);
  const length = $derived(player.length_ms ?? (track ? track.duration_s * 1000 : 0));
  const linked = $derived(player.loaded && player.loaded.source_player !== n);
  const ranges: [TempoRange, string][] = [
    ["Six", "6"],
    ["Ten", "10"],
    ["Sixteen", "16"],
    ["Wide", "WIDE"],
  ];

  // --- smooth playhead between 100 ms snapshots ---
  let receivedAt = 0;
  $effect(() => {
    void player.position_ms;
    void player.playing;
    receivedAt = performance.now();
  });
  const position = () => livePosition(player, receivedAt, performance.now());

  // --- waveforms, grid, artwork for the loaded track ---
  let visuals = $state<TrackVisuals | null>(null);
  const key = $derived(trackKey(player));
  $effect(() => {
    const k = key;
    visuals = null;
    if (!k) return;
    loadVisuals(n, k)
      .then((v) => {
        if (trackKey(player) === k) visuals = v;
      })
      .catch((e) => console.error("track visuals", e));
  });

  let zoom = $state(2);
  let showRemain = $state(true);

  /// Beats to the next memory cue, CDJ style ("8.1" = 8 bars and 1 beat).
  const countdown = $derived.by(() => {
    if (!visuals || !visuals.beats.length) return null;
    const next = player.memory_cues.find((c) => c > player.position_ms + 5);
    if (next === undefined) return null;
    const idx = (ms: number) => visuals!.beats.findLastIndex(([t]) => t <= ms);
    const beats = idx(next) - idx(player.position_ms);
    if (beats < 0 || beats > 256) return null;
    return `${Math.floor(beats / 4)}.${(beats % 4) + 1}`;
  });

  /// Fire-and-forget for controls used many times a second.
  function quick(command: string, args: Record<string, unknown> = {}) {
    invoke(command, { player: n, ...args }).catch((e) => console.error(command, e));
  }

  function padStyle(i: number): string {
    const c = player.hot_cues[i]?.color;
    return c ? `--pad: rgb(${c[0]}, ${c[1]}, ${c[2]})` : "";
  }

  // --- jog wheel: scrub while paused, nudge tempo while playing ---
  const JOG_MS_PER_TURN = 1800;
  let jog = $state<SVGSVGElement>()!;
  let jogLast: { angle: number; time: number } | null = null;
  let jogAngle = $state(0);
  $effect(() => {
    let frame = requestAnimationFrame(function spin() {
      jogAngle = ((position() / JOG_MS_PER_TURN) * 360) % 360;
      frame = requestAnimationFrame(spin);
    });
    return () => cancelAnimationFrame(frame);
  });

  function jogPointerAngle(e: PointerEvent): number {
    const r = jog.getBoundingClientRect();
    return (Math.atan2(e.clientY - (r.top + r.height / 2), e.clientX - (r.left + r.width / 2)) * 180) / Math.PI;
  }

  function jogDown(e: PointerEvent) {
    capture(jog, e);
    jogLast = { angle: jogPointerAngle(e), time: performance.now() };
  }

  function jogMove(e: PointerEvent) {
    if (!jogLast) return;
    const angle = jogPointerAngle(e);
    const now = performance.now();
    let delta = angle - jogLast.angle;
    if (delta > 180) delta -= 360;
    if (delta < -180) delta += 360;
    if (player.playing) {
      const degPerMs = delta / Math.max(1, now - jogLast.time);
      quick("set_bend", { bend: Math.max(-0.25, Math.min(0.25, degPerMs * 0.15)) });
    } else {
      quick("seek", { ms: Math.max(0, position() + (delta / 360) * JOG_MS_PER_TURN) });
    }
    jogLast = { angle, time: now };
  }

  function jogUp() {
    if (jogLast && player.playing) quick("set_bend", { bend: 0 });
    jogLast = null;
  }
</script>

<div class="cdj" class:running={player.running}>
  <div class="screen">
    <div class="hdr">
      <span class="num">{n}</span>
      {#if player.loaded}
        <span class="src" class:link={linked} title="Track source">
          <Icon name={linked ? "link" : player.loaded.source_slot === "Usb" ? "usb" : "sd"} size={12} />
          {linked ? `LINK P${player.loaded.source_player}` : slotLabel(player.loaded.source_slot)}
        </span>
      {/if}
      <span class="state">{player.running ? player.play_state.replace(/([a-z])([A-Z])/g, "$1 $2").toUpperCase() : "POWER OFF"}</span>
      <span class="spacer"></span>
      {#if player.running}
        <span class="tag master" class:lit={player.master}>MASTER</span>
        <span class="tag sync" class:lit={player.synced}>SYNC</span>
        <span class="tag onair" class:lit={player.on_air}>ON AIR</span>
      {/if}
      <button
        class="power"
        class:on={player.running}
        disabled={!player.running && !networkReady}
        title={player.running ? "Power off" : "Power on"}
        onclick={() => run(player.running ? "despawn_player" : "spawn_player", { player: n })}
      >
        <Icon name="power" size={13} />
      </button>
    </div>

    {#if player.running}
      <div class="track">
        <div class="art">
          {#if visuals?.artwork}<img src={visuals.artwork} alt="" />{/if}
        </div>
        <div class="names">
          {#if track}
            <div class="title">{track.title}</div>
            <div class="artist">{track.artist}{track.album ? ` — ${track.album}` : ""}</div>
          {:else}
            <div class="empty">NO TRACK</div>
          {/if}
        </div>
        <div class="tempo-read">
          {#if track?.key}<span class="key">{track.key}</span>{/if}
          <span class="bpm">{player.bpm.toFixed(1)}</span>
          <span class="pct">
            {player.pitch_percent >= 0 ? "+" : ""}{player.pitch_percent.toFixed(2)}%
            <small>±{ranges.find(([r]) => r === player.tempo_range)?.[1]}</small>
            {#if player.master_tempo}<small class="mt">MT</small>{/if}
          </span>
        </div>
      </div>

      {#if track}
        <div class="zoom">
          <WaveZoom {player} detail={visuals?.detail ?? null} beats={visuals?.beats ?? []} {position} {zoom} />
          <div class="zoom-ctl">
            <button title="Zoom in" onclick={() => (zoom = Math.max(0.5, zoom / 2))}>+</button>
            <button title="Zoom out" onclick={() => (zoom = Math.min(16, zoom * 2))}>−</button>
          </div>
        </div>
        <div class="overview">
          <WaveOverview {player} overview={visuals?.overview ?? null} {position} onseek={(ms) => quick("seek", { ms })} />
        </div>
      {:else}
        <div class="no-track">
          <Icon name={player.media.length ? "browse" : "usb"} size={22} />
          <span>{player.media.length ? "Load a track from BROWSE" : "Insert a USB or SD export below, or load over LINK from BROWSE"}</span>
        </div>
      {/if}

      <div class="info">
        <button class="time" title="Time mode" onclick={() => (showRemain = !showRemain)}>
          <small>{showRemain ? "REMAIN" : "TIME"}</small>
          {showRemain ? "-" : ""}{formatTimeTenths(showRemain ? Math.max(0, length - player.position_ms) : player.position_ms)}
        </button>
        <div class="phase" aria-label="Beat {player.beat_within_bar} of 4">
          {#each [1, 2, 3, 4] as b}<span class:lit={player.beat_within_bar === b}></span>{/each}
        </div>
        <div class="countdown">
          <small>NEXT CUE</small>
          {countdown ? `${countdown} BARS` : "--.-"}
        </div>
      </div>
    {:else}
      <button class="power-big" disabled={!networkReady} onclick={() => run("spawn_player", { player: n })}>
        <Icon name="power" size={34} />
        <span>{networkReady ? `Power on player ${n}` : "Set up the network first"}</span>
      </button>
    {/if}
  </div>

  <SlotStrip player={n} media={player.media} {sources} {run} />

  {#if player.running}
    <div class="surface">
      <div class="left">
        <div class="pads">
          {#each HOT_CUE_NAMES as name, i}
            <button
              class="pad"
              class:set={player.hot_cues[i]}
              style={padStyle(i)}
              title="Hot cue {name}: press to jump/play, or store here if empty. Shift-click clears."
              onclick={(e) => (e.shiftKey ? quick("clear_hot_cue", { pad: i }) : quick("hot_cue", { pad: i }))}
            >
              {name}
            </button>
          {/each}
        </div>
        <div class="loops">
          <button onclick={() => quick("loop_in")}>IN</button>
          <button onclick={() => quick("loop_out")}>OUT</button>
          <button class:active={player.active_loop} onclick={() => quick(player.active_loop ? "exit_loop" : "reloop")}>
            {player.active_loop ? "EXIT" : "RELOOP"}
          </button>
          <button class="beat" onclick={() => quick("beat_loop", { beats: 4 })}><Icon name="loop" size={11} /> 4</button>
          <button onclick={() => quick("resize_loop", { factor: 0.5 })}>½X</button>
          <button onclick={() => quick("resize_loop", { factor: 2 })}>2X</button>
          <button title="Previous memory cue" onclick={() => quick("memory_cue", { forward: false })}>◀ CUE</button>
          <button title="Next memory cue" onclick={() => quick("memory_cue", { forward: true })}>CUE ▶</button>
        </div>
        <div class="transport">
          <button
            class="cue"
            class:lit={player.play_state === "Cued" || player.play_state === "CuePlaying"}
            onpointerdown={() => quick("cue_press")}
            onpointerup={() => quick("cue_release")}
            onpointerleave={() => quick("cue_release")}
          >
            CUE
          </button>
          <button class="play" class:lit={player.playing} title="Play / pause" onclick={() => quick("play_pause")}>
            <Icon name="play-pause" size={22} />
          </button>
        </div>
      </div>

      <svg
        class="jog"
        class:on-air={player.on_air}
        bind:this={jog}
        viewBox="0 0 100 100"
        role="slider"
        aria-label="Jog wheel"
        aria-valuenow={player.position_ms}
        tabindex="-1"
        onpointerdown={jogDown}
        onpointermove={jogMove}
        onpointerup={jogUp}
        onpointercancel={jogUp}
      >
        <circle cx="50" cy="50" r="47" class="rim" />
        <circle cx="50" cy="50" r="39" class="platter" />
        <g transform="rotate({jogAngle} 50 50)">
          <rect x="48.5" y="12" width="3" height="10" rx="1.5" class="marker" />
        </g>
        <circle cx="50" cy="50" r="18" class="hub" />
        <text x="50" y="54" text-anchor="middle" class="hub-text">{n}</text>
      </svg>

      <div class="right">
        <div class="buttons">
          <button class:lit={player.master} class="master" title="Become tempo master" onclick={() => run("set_master", { player: n })}>
            <Icon name="master" size={11} /> MASTER
          </button>
          <button class:lit={player.synced} class="sync" onclick={() => run("set_sync", { player: n, on: !player.synced })}>
            <Icon name="sync" size={11} /> SYNC
          </button>
          <div class="ranges" role="group" aria-label="Tempo range">
            {#each ranges as [r, label]}
              <button class:lit={player.tempo_range === r} onclick={() => quick("set_tempo_range", { range: r })}>{label}</button>
            {/each}
          </div>
          <button class:lit={player.master_tempo} title="Master tempo (key lock)" onclick={() => quick("set_master_tempo", { on: !player.master_tempo })}>MT</button>
          <button class:lit={player.reverse} title="Reverse" onclick={() => quick("set_reverse", { on: !player.reverse })}>
            <Icon name="reverse" size={10} /> REV
          </button>
          <button class:lit={player.quantize} title="Quantize" onclick={() => quick("set_quantize", { on: !player.quantize })}>Q</button>
        </div>
        <!-- Like a CDJ: pulling the fader down (towards you) plays faster. -->
        <Fader
          label="Tempo fader"
          value={player.pitch_fader}
          min={-1}
          max={1}
          reset={0}
          topIsMax={false}
          centreMark
          length={128}
          title="Tempo (double-click: 0%)"
          onchange={(position) => quick("set_pitch_fader", { position })}
        />
      </div>
    </div>
  {/if}
</div>

<style>
  .cdj {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 8px;
    background: linear-gradient(#1a1a1d, #121214);
    border: 1px solid #2a2a2f;
    border-radius: 10px;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
  }

  /* ---- screen ---- */
  .screen {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 6px;
    background: #050507;
    border: 1px solid #23232a;
    border-radius: 4px;
    min-height: 0;
    flex: 1;
  }

  .hdr,
  .track,
  .info {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .num {
    font-size: 17px;
    font-weight: 800;
    color: #fff;
    background: #2a2a30;
    border-radius: 3px;
    padding: 0 6px;
  }

  .src {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font-size: 10px;
    font-weight: 700;
    color: var(--accent);
    border: 1px solid #1d4567;
    border-radius: 3px;
    padding: 1px 4px;
  }

  .src.link {
    color: #b98cff;
    border-color: #4a3470;
  }

  .state {
    font-size: 10px;
    color: var(--muted);
    letter-spacing: 0.06em;
  }

  .spacer {
    flex: 1;
  }

  .tag {
    font-size: 9px;
    font-weight: 800;
    letter-spacing: 0.05em;
    color: #3a3a42;
    padding: 1px 4px;
    border-radius: 2px;
  }

  .tag.master.lit {
    color: #000;
    background: var(--cue);
  }

  .tag.sync.lit {
    color: #000;
    background: var(--accent);
  }

  .tag.onair.lit {
    color: #fff;
    background: var(--on-air);
  }

  .power {
    padding: 3px 5px;
    color: var(--muted);
  }

  .power.on {
    color: var(--play);
  }

  .art {
    width: 34px;
    height: 34px;
    flex: none;
    background: #15151a;
    border-radius: 2px;
    overflow: hidden;
  }

  .art img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }

  .names {
    flex: 1;
    min-width: 0;
  }

  .title {
    font-size: 13px;
    font-weight: 700;
    color: #fff;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .artist,
  .empty {
    font-size: 11px;
    color: #9aa0aa;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .tempo-read {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    font-variant-numeric: tabular-nums;
  }

  .key {
    font-size: 10px;
    color: #4cd964;
  }

  .bpm {
    font-family: "SF Mono", Menlo, monospace;
    font-size: 16px;
    font-weight: 700;
    color: #fff;
    line-height: 1;
  }

  .pct {
    font-family: "SF Mono", Menlo, monospace;
    font-size: 10px;
    color: #9aa0aa;
  }

  .pct small {
    margin-left: 3px;
    color: #5a5a64;
  }

  .pct .mt {
    color: var(--cue);
  }

  .zoom {
    position: relative;
    flex: 1;
    min-height: 56px;
  }

  .zoom-ctl {
    position: absolute;
    right: 2px;
    top: 2px;
    display: flex;
    gap: 2px;
  }

  .zoom-ctl button {
    padding: 0 5px;
    font-size: 11px;
    line-height: 14px;
    background: rgba(0, 0, 0, 0.6);
  }

  .overview {
    height: 30px;
  }

  .no-track,
  .power-big {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
    color: #4a4a54;
    font-size: 11px;
    text-align: center;
  }

  .power-big {
    background: none;
    border: none;
  }

  .power-big:not(:disabled):hover {
    color: var(--play);
  }

  .info {
    justify-content: space-between;
  }

  .time {
    background: none;
    border: none;
    padding: 0;
    font-family: "SF Mono", Menlo, monospace;
    font-size: 17px;
    font-weight: 700;
    color: #fff;
    font-variant-numeric: tabular-nums;
  }

  .time small,
  .countdown small {
    display: block;
    font-family: -apple-system, sans-serif;
    font-size: 8px;
    font-weight: 600;
    letter-spacing: 0.08em;
    color: #5a5a64;
    text-align: left;
  }

  .phase {
    display: flex;
    gap: 3px;
  }

  .phase span {
    width: 18px;
    height: 6px;
    background: #1d1d22;
  }

  .phase span.lit {
    background: var(--cue);
  }

  .countdown {
    font-family: "SF Mono", Menlo, monospace;
    font-size: 12px;
    color: #ff6b5e;
    text-align: right;
  }

  /* ---- control surface ---- */
  .surface {
    display: flex;
    gap: 8px;
    align-items: center;
  }

  .left,
  .right {
    display: flex;
    flex-direction: column;
    gap: 5px;
    min-width: 0;
  }

  .left {
    flex: 1;
  }

  .pads {
    display: grid;
    grid-template-columns: repeat(8, 1fr);
    gap: 3px;
  }

  .pad {
    padding: 5px 0;
    font-size: 11px;
    font-weight: 800;
    color: #55555e;
    background: #1b1b1f;
    border-color: #2a2a30;
  }

  .pad.set {
    color: #000;
    background: var(--pad, var(--play));
    border-color: var(--pad, var(--play));
    box-shadow: 0 0 6px var(--pad, var(--play));
  }

  .loops {
    display: flex;
    flex-wrap: wrap;
    gap: 3px;
  }

  .loops button,
  .buttons button {
    padding: 3px 6px;
    font-size: 10px;
    font-weight: 600;
  }

  .loops .beat {
    display: inline-flex;
    align-items: center;
    gap: 2px;
  }

  button.active {
    color: var(--cue);
    border-color: var(--cue);
  }

  .transport {
    display: flex;
    gap: 10px;
  }

  .cue,
  .play {
    width: 52px;
    height: 52px;
    border-radius: 50%;
    font-weight: 800;
    display: grid;
    place-items: center;
    background: #18181b;
  }

  .cue {
    border: 3px solid #6b4310;
    color: var(--cue);
  }

  .cue.lit {
    border-color: var(--cue);
    box-shadow: 0 0 10px rgba(255, 154, 31, 0.5);
  }

  .play {
    border: 3px solid #1d5a32;
    color: var(--play);
  }

  .play.lit {
    border-color: var(--play);
    box-shadow: 0 0 10px rgba(54, 210, 106, 0.5);
  }

  .jog {
    width: 112px;
    height: 112px;
    flex: none;
    cursor: grab;
    touch-action: none;
  }

  .jog .rim {
    fill: #0d0d10;
    stroke: #34343c;
    stroke-width: 2;
  }

  .jog.on-air .rim {
    stroke: var(--on-air);
    filter: drop-shadow(0 0 3px var(--on-air));
  }

  .jog .platter {
    fill: #1c1c21;
    stroke: #2c2c33;
  }

  .jog .marker {
    fill: #e8e8ea;
  }

  .jog .hub {
    fill: #050507;
    stroke: #2c2c33;
  }

  .jog .hub-text {
    fill: #8a8a92;
    font-size: 16px;
    font-weight: 800;
  }

  .right {
    flex-direction: row;
    align-items: stretch;
  }

  .buttons {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }

  .ranges {
    display: flex;
    gap: 1px;
  }

  .ranges button {
    padding: 2px 3px;
  }

  button.lit {
    color: var(--cue);
    border-color: var(--cue);
  }

  .buttons .master.lit {
    color: #000;
    background: var(--cue);
  }

  .buttons .sync.lit {
    color: #000;
    background: var(--accent);
    border-color: var(--accent);
  }

</style>
