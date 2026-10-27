<script lang="ts">
  import { onMount } from "svelte";
  import { HOT_CUE_NAMES, type PlayerView } from "./types";
  import { DETAIL_COLS_PER_SECOND, rgb } from "./visuals";

  let {
    player,
    detail,
    beats,
    position,
    zoom = 2,
  }: {
    player: PlayerView;
    detail: Uint8Array | null;
    /// `[time_ms, bar_beat]`.
    beats: [number, number][];
    /// Current playhead, ms (called every frame).
    position: () => number;
    /// Detail columns per pixel (bigger shows more of the track).
    zoom?: number;
  } = $props();

  let canvas: HTMLCanvasElement;

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
    ctx.fillStyle = "#050507";
    ctx.fillRect(0, 0, w, h);

    const pos = position();
    const msPerPx = (zoom * 1000) / DETAIL_COLS_PER_SECOND;
    const centre = w / 2;
    const x = (ms: number) => centre + (ms - pos) / msPerPx;
    const from = pos - centre * msPerPx;
    const to = pos + centre * msPerPx;

    // Loop region.
    const loop = player.active_loop;
    if (loop) {
      ctx.fillStyle = "rgba(255, 154, 31, 0.2)";
      ctx.fillRect(x(loop.start_ms), 0, x(loop.end_ms) - x(loop.start_ms), h);
    }

    // Beat grid: bars (beat 1) brighter, a short tick on other beats.
    for (const [ms, bar] of beats) {
      if (ms < from) continue;
      if (ms > to) break;
      const bx = Math.round(x(ms)) + 0.5;
      ctx.strokeStyle = bar === 1 ? "rgba(255, 255, 255, 0.55)" : "rgba(255, 255, 255, 0.2)";
      ctx.beginPath();
      ctx.moveTo(bx, 0);
      ctx.lineTo(bx, bar === 1 ? h : 8);
      ctx.moveTo(bx, h - (bar === 1 ? 0 : 8));
      ctx.lineTo(bx, h);
      ctx.stroke();
    }

    // Waveform, mirrored around the middle.
    if (detail && detail.length >= 4) {
      const cols = detail.length / 4;
      for (let px = 0; px < w; px++) {
        const col = Math.floor(((from + px * msPerPx) / 1000) * DETAIL_COLS_PER_SECOND);
        if (col < 0 || col >= cols) continue;
        const c = col * 4;
        const height = (detail[c] / 255) * (h - 6);
        ctx.fillStyle = rgb(detail[c + 1], detail[c + 2], detail[c + 3]);
        ctx.fillRect(px, (h - height) / 2, 1, height);
      }
    }

    // Cue points.
    ctx.fillStyle = "#ff3b30";
    for (const m of player.memory_cues) {
      if (m < from || m > to) continue;
      ctx.fillRect(x(m) - 1, 0, 2, h);
    }
    ctx.fillStyle = "#ff9a1f";
    ctx.beginPath();
    ctx.moveTo(x(player.cue_ms) - 6, 0);
    ctx.lineTo(x(player.cue_ms) + 6, 0);
    ctx.lineTo(x(player.cue_ms), 9);
    ctx.fill();
    ctx.font = "bold 10px -apple-system, sans-serif";
    player.hot_cues.forEach((cue, i) => {
      if (!cue || cue.start_ms < from || cue.start_ms > to) return;
      const c = cue.color ?? [54, 210, 106];
      ctx.fillStyle = rgb(c[0], c[1], c[2]);
      ctx.fillRect(x(cue.start_ms), h - 14, 13, 14);
      ctx.fillRect(x(cue.start_ms), 0, 1, h);
      ctx.fillStyle = "#000";
      ctx.fillText(HOT_CUE_NAMES[i], x(cue.start_ms) + 3, h - 3);
    });

    // Playhead, fixed in the centre.
    ctx.fillStyle = "#fff";
    ctx.fillRect(centre - 1, 0, 2, h);
  }

  onMount(() => {
    let frame = requestAnimationFrame(function loop() {
      draw();
      frame = requestAnimationFrame(loop);
    });
    return () => cancelAnimationFrame(frame);
  });
</script>

<canvas bind:this={canvas}></canvas>

<style>
  canvas {
    display: block;
    width: 100%;
    height: 100%;
  }
</style>
