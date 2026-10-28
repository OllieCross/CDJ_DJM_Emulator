<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onMount } from "svelte";
  import Browser from "./lib/Browser.svelte";
  import Deck from "./lib/Deck.svelte";
  import Mixer from "./lib/Mixer.svelte";
  import Network from "./lib/Network.svelte";
  import type { ModelsView } from "./lib/models";
  import type { RigSnapshot } from "./lib/types";

  let snapshot = $state<RigSnapshot | null>(null);
  let models = $state<ModelsView>({ players: [], mixers: [] });
  let error = $state<string | null>(null);
  let busy = $state(false);

  /// Run a backend command, surfacing its error in the banner.
  async function run(command: string, args: Record<string, unknown> = {}) {
    busy = true;
    error = null;
    try {
      await invoke(command, args);
      snapshot = await invoke<RigSnapshot>("rig_snapshot");
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  onMount(() => {
    invoke<RigSnapshot>("rig_snapshot").then((s) => (snapshot = s));
    invoke<ModelsView>("models").then((m) => (models = m));
    const unlisten = listen<RigSnapshot>("rig-snapshot", (e) => (snapshot = e.payload));
    return () => {
      unlisten.then((f) => f());
    };
  });
</script>

<main>
  <header>
    <h1>CDJ Emulator</h1>
    {#if snapshot}
      <Network network={snapshot.network} {busy} {run} />
    {/if}
  </header>

  {#if error}
    <div class="error" role="alert">
      <span>{error}</span>
      <button onclick={() => (error = null)} aria-label="Dismiss">×</button>
    </div>
  {/if}

  {#if snapshot}
    <div class="layout">
      <section class="decks">
        {#each snapshot.players as player (player.number)}
          <Deck {player} models={models.players} sources={snapshot.sources} networkReady={snapshot.network.ready} {run} />
        {/each}
      </section>
      <aside>
        <Mixer
          mixer={snapshot.mixer}
          models={models.mixers}
          players={snapshot.players}
          audio={snapshot.audio}
          networkReady={snapshot.network.ready}
          {run}
        />
        <Browser players={snapshot.players} {run} />
      </aside>
    </div>
  {/if}
</main>

<style>
  :global(*, *::before, *::after) {
    box-sizing: border-box;
    margin: 0;
    padding: 0;
  }

  :global(:root) {
    --bg: #0b0b0c;
    --panel: #151517;
    --panel-2: #1d1d20;
    --line: #2b2b30;
    --text: #e8e8ea;
    --muted: #8a8a92;
    --accent: #2fa8ff;
    --play: #36d26a;
    --cue: #ff9a1f;
    --master: #ffcc33;
    --on-air: #ff3b30;
  }

  :global(body) {
    background: var(--bg);
    color: var(--text);
    font-family: -apple-system, "Helvetica Neue", sans-serif;
    font-size: 13px;
  }

  :global(button) {
    font: inherit;
    color: var(--text);
    background: var(--panel-2);
    border: 1px solid var(--line);
    border-radius: 4px;
    padding: 6px 10px;
    cursor: pointer;
  }

  :global(button:disabled) {
    opacity: 0.4;
    cursor: default;
  }

  main {
    display: flex;
    flex-direction: column;
    height: 100vh;
    overflow: hidden;
  }

  header {
    display: flex;
    align-items: center;
    gap: 24px;
    padding: 10px 16px;
    background: var(--panel);
    border-bottom: 1px solid var(--line);
  }

  h1 {
    font-size: 13px;
    font-weight: 700;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    color: var(--accent);
    white-space: nowrap;
  }

  .error {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 8px 16px;
    background: #3a1214;
    color: #ffb3ae;
    border-bottom: 1px solid #6b2226;
  }

  .error button {
    background: none;
    border: none;
    color: inherit;
    font-size: 16px;
  }

  .layout {
    display: flex;
    flex: 1;
    min-height: 0;
  }

  .decks {
    flex: 1;
    display: grid;
    grid-template-columns: 1fr 1fr;
    grid-template-rows: 1fr 1fr;
    gap: 8px;
    padding: 8px;
    min-width: 0;
  }

  aside {
    width: 390px;
    display: flex;
    flex-direction: column;
    border-left: 1px solid var(--line);
    min-height: 0;
  }
</style>
