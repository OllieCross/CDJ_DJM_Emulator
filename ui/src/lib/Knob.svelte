<script lang="ts">
  // A rotary knob: drag up/down (or scroll) to turn, double-click to reset.
  import { capture } from "./pointer";

  let {
    value,
    min,
    max,
    reset = 0,
    label,
    title = "",
    color = "#e8e8ea",
    size = 34,
    onchange,
  }: {
    value: number;
    min: number;
    max: number;
    reset?: number;
    label: string;
    title?: string;
    color?: string;
    size?: number;
    onchange: (v: number) => void;
  } = $props();

  const SWEEP = 270;
  // While being turned, show where the hand is rather than the last
  // snapshot from the backend (which trails by up to 100 ms).
  let local = $state<number | null>(null);
  let settle: ReturnType<typeof setTimeout> | undefined;
  const shown = $derived(local ?? value);
  const angle = $derived(-SWEEP / 2 + ((shown - min) / (max - min)) * SWEEP);
  const resetAngle = $derived(-SWEEP / 2 + ((reset - min) / (max - min)) * SWEEP);

  let drag: { y: number; value: number } | null = null;

  function set(v: number) {
    local = Math.min(max, Math.max(min, v));
    clearTimeout(settle);
    settle = setTimeout(() => (local = null), 400);
    onchange(local);
  }

  function down(e: PointerEvent) {
    if (e.button !== 0) return;
    capture(e.currentTarget as Element, e);
    drag = { y: e.clientY, value: shown };
  }

  function move(e: PointerEvent) {
    if (!drag) return;
    // 150 px of travel covers the whole range; shift for fine control.
    const range = (max - min) * (e.shiftKey ? 0.2 : 1);
    set(drag.value + ((drag.y - e.clientY) / 150) * range);
  }

  function wheel(e: WheelEvent) {
    e.preventDefault();
    set(shown - Math.sign(e.deltaY) * (max - min) / 50);
  }

  function arc(from: number, to: number, r: number): string {
    const p = (a: number) => {
      const rad = ((a - 90) * Math.PI) / 180;
      return `${16 + r * Math.cos(rad)} ${16 + r * Math.sin(rad)}`;
    };
    const large = Math.abs(to - from) > 180 ? 1 : 0;
    const sweep = to > from ? 1 : 0;
    return `M ${p(from)} A ${r} ${r} 0 ${large} ${sweep} ${p(to)}`;
  }
</script>

<div class="knob" {title}>
  <svg
    width={size}
    height={size}
    viewBox="0 0 32 32"
    role="slider"
    aria-label={label}
    aria-valuenow={shown}
    aria-valuemin={min}
    aria-valuemax={max}
    tabindex="0"
    onpointerdown={down}
    onpointermove={move}
    onpointerup={() => (drag = null)}
    ondblclick={() => set(reset)}
    onwheel={wheel}
  >
    <path d={arc(-SWEEP / 2, SWEEP / 2, 13)} stroke="#2b2b30" stroke-width="3" fill="none" stroke-linecap="round" />
    {#if Math.abs(angle - resetAngle) > 0.5}
      <path d={arc(resetAngle, angle, 13)} stroke={color} stroke-width="3" fill="none" stroke-linecap="round" />
    {/if}
    <circle cx="16" cy="16" r="9.5" fill="#1d1d20" stroke="#3a3a40" />
    <line x1="16" y1="16" x2="16" y2="8" stroke={color} stroke-width="2" stroke-linecap="round" transform="rotate({angle} 16 16)" />
  </svg>
  <span>{label}</span>
</div>

<style>
  .knob {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 1px;
    font-size: 9px;
    letter-spacing: 0.06em;
    color: var(--muted);
    user-select: none;
  }

  svg {
    cursor: ns-resize;
    touch-action: none;
  }
</style>
