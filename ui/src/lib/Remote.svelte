<script lang="ts">
  import type { RemoteView } from "./types";

  let { remote }: { remote: RemoteView } = $props();

  let open = $state(false);
  let now = $state(Date.now());

  $effect(() => {
    if (!open) return;
    const timer = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(timer);
  });

  function ago(ms: number): string {
    const s = Math.max(0, Math.round((now - ms) / 1000));
    if (s < 60) return `${s}s`;
    if (s < 3600) return `${Math.floor(s / 60)}m`;
    return `${Math.floor(s / 3600)}h`;
  }

  const title = $derived(
    remote.listening
      ? "A client app holds the Pro DJ Link ports, so the players take its commands"
      : "No client app on the ports yet; the players start listening when Beat Link Trigger or ShowKontrol starts",
  );
</script>

<div class="remote">
  <button class="chip" class:listening={remote.listening} {title} onclick={() => (open = !open)} aria-expanded={open}>
    <span class="dot"></span>
    Commands
    {#if remote.master}
      <span class="master">
        Master: {remote.master.name}{remote.master.bpm != null ? ` · ${remote.master.bpm.toFixed(1)}` : ""}
      </span>
    {/if}
  </button>

  {#if open}
    <div class="panel" role="dialog" aria-label="Commands from client apps">
      <p class="state">
        {remote.listening ? "Listening for commands from client apps." : "Not listening: no client app holds the ports."}
      </p>
      {#if remote.log.length}
        <ul>
          {#each remote.log as event (event.at_ms + event.text)}
            <li>
              <span class="when">{ago(event.at_ms)}</span>
              <span class="from">{event.from}</span>
              <span class="text">{event.text}</span>
            </li>
          {/each}
        </ul>
      {:else}
        <p class="empty">
          Nothing asked yet. Tempo master handoffs, sync, fader start, track loads and media queries from Beat Link
          Trigger or ShowKontrol show up here.
        </p>
      {/if}
    </div>
  {/if}
</div>

<style>
  .remote {
    position: relative;
    flex: none;
  }

  .chip {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--muted);
  }

  .chip.listening {
    color: var(--text);
  }

  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--line);
  }

  .listening .dot {
    background: var(--play);
  }

  .master {
    color: var(--master);
    font-weight: 600;
  }

  .panel {
    position: absolute;
    right: 0;
    top: calc(100% + 6px);
    z-index: 10;
    width: 420px;
    max-width: calc(100vw - 32px);
    background: var(--panel-2);
    border: 1px solid var(--line);
    border-radius: 6px;
    padding: 10px 12px;
    box-shadow: 0 8px 24px rgb(0 0 0 / 0.5);
  }

  .state,
  .empty {
    color: var(--muted);
    line-height: 1.4;
  }

  ul {
    list-style: none;
    margin-top: 8px;
    display: grid;
    gap: 6px;
  }

  li {
    display: grid;
    grid-template-columns: 34px auto 1fr;
    gap: 8px;
    align-items: baseline;
  }

  .when {
    color: var(--muted);
    font-variant-numeric: tabular-nums;
    text-align: right;
  }

  .from {
    color: var(--accent);
    white-space: nowrap;
  }
</style>
