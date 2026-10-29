<script lang="ts">
  // A hardware-style fader: drag the cap, click the slot to jump, scroll to
  // nudge, double-click or right-click to reset. Shift drags finely.
  import { capture } from "./pointer";

  let {
    value,
    min = 0,
    max = 1,
    reset,
    vertical = true,
    /// Vertical faders: true when the top is `max` (channel faders); false
    /// puts `min` at the top (CDJ tempo fader: pull down to go faster).
    topIsMax = true,
    /// Draw a centre mark (tempo, crossfader).
    centreMark = false,
    length = 110,
    label,
    title = "",
    onchange,
  }: {
    value: number;
    min?: number;
    max?: number;
    reset?: number;
    vertical?: boolean;
    topIsMax?: boolean;
    centreMark?: boolean;
    length?: number;
    label: string;
    title?: string;
    onchange: (v: number) => void;
  } = $props();

  const CAP = 18;
  let slot = $state<HTMLDivElement>()!;

  // While being moved, show where the hand is rather than the last
  // snapshot from the backend (which trails by up to 100 ms).
  let local = $state<number | null>(null);
  let settle: ReturnType<typeof setTimeout> | undefined;
  const shown = $derived(local ?? value);

  /// Where the cap sits along the travel, 0 (start: top or left) ..= 1.
  const fraction = $derived.by(() => {
    const f = (shown - min) / (max - min);
    return vertical && topIsMax ? 1 - f : f;
  });

  function emit(v: number) {
    local = v;
    clearTimeout(settle);
    settle = setTimeout(() => (local = null), 400);
    onchange(v);
  }

  function set(f: number) {
    const clamped = Math.min(1, Math.max(0, f));
    const g = vertical && topIsMax ? 1 - clamped : clamped;
    emit(min + g * (max - min));
  }

  function fractionAt(e: PointerEvent): number {
    const r = slot.getBoundingClientRect();
    return vertical ? (e.clientY - r.top - CAP / 2) / (r.height - CAP) : (e.clientX - r.left - CAP / 2) / (r.width - CAP);
  }

  let drag: { start: number; fraction: number } | null = null;

  function down(e: PointerEvent) {
    if (e.button !== 0) return;
    capture(slot, e);
    const onCap = Math.abs(fractionAt(e) - fraction) * (length - CAP) < CAP;
    if (!onCap) set(fractionAt(e));
    drag = { start: vertical ? e.clientY : e.clientX, fraction: onCap ? fraction : fractionAt(e) };
  }

  function move(e: PointerEvent) {
    if (!drag) return;
    const delta = ((vertical ? e.clientY : e.clientX) - drag.start) / (length - CAP);
    set(drag.fraction + delta * (e.shiftKey ? 0.2 : 1));
  }

  function wheel(e: WheelEvent) {
    e.preventDefault();
    set(fraction + Math.sign(vertical ? e.deltaY : e.deltaX || e.deltaY) * 0.02);
  }

  function key(e: KeyboardEvent) {
    const step = e.shiftKey ? 0.005 : 0.02;
    const towardsEnd = vertical ? e.key === "ArrowDown" : e.key === "ArrowRight";
    const towardsStart = vertical ? e.key === "ArrowUp" : e.key === "ArrowLeft";
    if (towardsEnd || towardsStart) {
      e.preventDefault();
      set(fraction + (towardsEnd ? step : -step));
    }
  }
</script>

<div
  class="fader"
  class:vertical
  style="{vertical ? 'height' : 'width'}: {length}px"
  bind:this={slot}
  role="slider"
  aria-label={label}
  aria-valuenow={shown}
  aria-valuemin={min}
  aria-valuemax={max}
  aria-orientation={vertical ? "vertical" : "horizontal"}
  tabindex="0"
  {title}
  onpointerdown={down}
  onpointermove={move}
  onpointerup={() => (drag = null)}
  onpointercancel={() => (drag = null)}
  ondblclick={() => reset !== undefined && emit(reset)}
  oncontextmenu={(e) => {
    e.preventDefault();
    if (reset !== undefined) emit(reset);
  }}
  onwheel={wheel}
  onkeydown={key}
>
  <div class="slot"></div>
  {#if centreMark}<div class="centre"></div>{/if}
  <div class="cap" style="{vertical ? 'top' : 'left'}: calc({fraction} * (100% - {CAP}px))"></div>
</div>

<style>
  .fader {
    position: relative;
    flex: none;
    touch-action: none;
    cursor: pointer;
    outline: none;
  }

  .fader:not(.vertical) {
    height: 22px;
  }

  .fader.vertical {
    width: 22px;
  }

  .slot {
    position: absolute;
    background: #000;
    border: 1px solid #4a4a54;
    border-radius: 2px;
  }

  .vertical .slot {
    left: 9px;
    width: 4px;
    top: 4px;
    bottom: 4px;
  }

  .fader:not(.vertical) .slot {
    top: 9px;
    height: 4px;
    left: 4px;
    right: 4px;
  }

  .centre {
    position: absolute;
    background: #5a5a64;
  }

  .vertical .centre {
    top: calc(50% - 1px);
    height: 2px;
    left: 0;
    right: 0;
  }

  .fader:not(.vertical) .centre {
    left: calc(50% - 1px);
    width: 2px;
    top: 0;
    bottom: 0;
  }

  .cap {
    position: absolute;
    background: linear-gradient(#4a4a52, #26262b);
    border: 1px solid #5c5c66;
    border-radius: 2px;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.6);
  }

  .vertical .cap {
    left: 0;
    width: 22px;
    height: 18px;
    background:
      linear-gradient(transparent 8px, #e8e8ea 8px, #e8e8ea 10px, transparent 10px),
      linear-gradient(#4a4a52, #26262b);
  }

  .fader:not(.vertical) .cap {
    top: 0;
    height: 22px;
    width: 18px;
    background:
      linear-gradient(90deg, transparent 8px, #e8e8ea 8px, #e8e8ea 10px, transparent 10px),
      linear-gradient(#4a4a52, #26262b);
  }

  .fader:focus-visible .cap {
    border-color: var(--accent);
  }
</style>
