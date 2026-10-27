<script lang="ts">
  import { onMount } from "svelte";
  import { HOT_CUE_NAMES, type PlayerView } from "./types";
  import { rgb } from "./visuals";

  let {
    player,
    overview,
    position,
    onseek,
  }: {
    player: PlayerView;
    overview: Uint8Array | null;
    /// Current playhead, ms (called every frame).
    position: () => number;
    onseek: (ms: number) => void;
  } = $props();

  let canvas: HTMLCanvasElement;
  const length = $derived(player.length_ms ?? (player.loaded ? player.loaded.track.duration_s * 1000 : 0));

  function draw() {
    const ctx = canvas?.getContext("2d");
    if (!ctx) return;
    const dpr = window.devicePixelRatio || 1;
    const w = canvas.clientWidth;
    const h = canvas.clientHeight;
    if (canvas.width !== w * dpr || canvas.height !== h * dpr) {
      canvas.width = w * dpr;
      canvas.height = h * dpr;
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    const x = (ms: number) => (length > 0 ? (ms / length) * w : 0);
    const pos = position();

    // Loop region.
    const loop = player.active_loop;
    if (loop) {
      ctx.fillStyle = "rgba(255, 154, 31, 0.25)";
      ctx.fillRect(x(loop.start_ms), 0, x(loop.end_ms) - x(loop.start_ms), h);
    }

    // Waveform, mirrored around the middle; the played part dimmed.
    if (overview && overview.length >= 4) {
      const cols = overview.length / 4;
      const played = x(pos);
      for (let px = 0; px < w; px++) {
        const c = Math.min(cols - 1, Math.floor((px / w) * cols)) * 4;
        const height = (overview[c] / 255) * (h - 10);
        ctx.fillStyle = rgb(overview[c + 1], overview[c + 2], overview[c + 3], px < played ? 0.35 : 1);
        ctx.fillRect(px, (h - height) / 2, 1, height);
      }
    }

    // Memory cues (red, bottom) and the cue point (orange, top).
    ctx.fillStyle = "#ff3b30";
    for (const m of player.memory_cues) {
      ctx.beginPath();
      ctx.moveTo(x(m) - 4, h);
      ctx.lineTo(x(m) + 4, h);
      ctx.lineTo(x(m), h - 6);
      ctx.fill();
    }
    ctx.fillStyle = "#ff9a1f";
    ctx.beginPath();
    ctx.moveTo(x(player.cue_ms) - 5, 0);
    ctx.lineTo(x(player.cue_ms) + 5, 0);
    ctx.lineTo(x(player.cue_ms), 7);
    ctx.fill();

    // Hot cues: lettered flags in their colours.
    ctx.font = "bold 8px -apple-system, sans-serif";
    player.hot_cues.forEach((cue, i) => {
      if (!cue) return;
      const c = cue.color ?? [54, 210, 106];
      ctx.fillStyle = rgb(c[0], c[1], c[2]);
      ctx.fillRect(x(cue.start_ms), 0, 9, 9);
      ctx.fillStyle = "#000";
      ctx.fillText(HOT_CUE_NAMES[i], x(cue.start_ms) + 2, 8);
    });

    // Playhead.
    ctx.fillStyle = "#fff";
    ctx.fillRect(x(pos) - 1, 0, 2, h);
  }

  onMount(() => {
    let frame = requestAnimationFrame(function loop() {
      draw();
      frame = requestAnimationFrame(loop);
    });
    return () => cancelAnimationFrame(frame);
  });

  function click(e: MouseEvent) {
    const rect = canvas.getBoundingClientRect();
    onseek(((e.clientX - rect.left) / rect.width) * length);
  }
</script>

<canvas bind:this={canvas} onclick={click} title="Click to jump (needle search)"></canvas>

<style>
  canvas {
    display: block;
    width: 100%;
    height: 100%;
    cursor: pointer;
  }
</style>
