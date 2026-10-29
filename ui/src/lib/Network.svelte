<script lang="ts">
  import type { Layout, NetworkView } from "./types";

  let {
    network,
    busy,
    run,
  }: {
    network: NetworkView;
    busy: boolean;
    run: (command: string, args?: Record<string, unknown>) => Promise<void>;
  } = $props();

  // Starts at the current layout, then follows the user's choice.
  // svelte-ignore state_referenced_locally
  let layout = $state<Layout>(network.layout);

  const summary = $derived(
    network.ready
      ? `Online — clients on ${network.client_iface} (${network.client_ip})`
      : network.status.pair_exists
        ? `Incomplete — ${
            network.status.missing_device_ips.length
              ? `missing ${network.status.missing_device_ips.join(", ")}`
              : !network.status.client_ip_up
                ? "missing client address"
                : "no route to the client apps"
          }. Click Re-apply.`
        : "Offline — no virtual cable",
  );
</script>

<div class="network">
  <span class="dot" class:ready={network.ready}></span>
  <span class="summary">{summary}</span>
  <select bind:value={layout} disabled={busy} title="Network layout">
    <option value="SharedSubnet">Shared subnet (proven)</option>
    <option value="LoopbackAliases">Loopback aliases (experimental)</option>
  </select>
  <button disabled={busy} onclick={() => run("network_setup", { layout })}>
    {network.ready ? "Re-apply" : "Set up network"}
  </button>
  <button disabled={busy || !network.status.pair_exists} onclick={() => run("network_teardown")}>
    Remove
  </button>
</div>

<style>
  .network {
    display: flex;
    align-items: center;
    gap: 10px;
    flex: 1;
    min-width: 0;
  }

  .dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--on-air);
    flex: none;
  }

  .dot.ready {
    background: var(--play);
  }

  .summary {
    color: var(--muted);
    flex: 1;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
