<script lang="ts">
  // Small original line icons, drawn on a 16x16 grid.
  let { name, size = 14 }: { name: string; size?: number } = $props();
</script>

<svg width={size} height={size} viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
  {#if name === "play"}
    <path d="M4 2.5 13 8l-9 5.5z" fill="currentColor" stroke="none" />
  {:else if name === "pause"}
    <path d="M5 3v10M11 3v10" stroke-width="2.4" />
  {:else if name === "play-pause"}
    <path d="M2 3.5 8 8l-6 4.5z" fill="currentColor" stroke="none" /><path d="M10.5 4v8M14 4v8" stroke-width="1.8" />
  {:else if name === "usb"}
    <rect x="4.5" y="6" width="7" height="8.5" rx="1" /><path d="M6 6V2h4v4M7 3.5h.01M9 3.5h.01" />
  {:else if name === "drive"}
    <rect x="1.5" y="4.5" width="13" height="7" rx="1.5" /><path d="M4 8h4" /><path d="M11.5 8h.01" stroke-width="2" />
  {:else if name === "link"}
    <path d="M6.5 9.5 9.5 6.5M7 4.5l1.3-1.3a2.8 2.8 0 0 1 4 4L11 8.5M9 11.5l-1.3 1.3a2.8 2.8 0 0 1-4-4L5 7.5" />
  {:else if name === "loop"}
    <path d="M3 8a5 5 0 0 1 8.5-3.5M13 8a5 5 0 0 1-8.5 3.5" /><path d="M11.5 1.5v3h-3M4.5 14.5v-3h3" />
  {:else if name === "sync"}
    <path d="M2.5 6h9l-2.5-2.5M13.5 10h-9L7 12.5" />
  {:else if name === "master"}
    <path d="M2.5 12.5h11M3 12l-.5-7 3 2.5L8 3l2.5 4.5 3-2.5-.5 7" />
  {:else if name === "power"}
    <path d="M8 1.5v6M4.5 4a5 5 0 1 0 7 0" />
  {:else if name === "eject"}
    <path d="M3 9.5 8 4l5 5.5z" fill="currentColor" stroke="none" /><path d="M3 12.5h10" stroke-width="2" />
  {:else if name === "browse"}
    <path d="M2.5 4h11M2.5 8h11M2.5 12h7" />
  {:else if name === "reverse"}
    <path d="M13 2.5 4 8l9 5.5z" />
  {/if}
</svg>
