<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import Fader from "./Fader.svelte";
  import Icon from "./Icon.svelte";
  import Knob from "./Knob.svelte";
  import { DEFAULT_MIXER_MODEL, rememberModel, rememberedModel, type ModelInfo } from "./models";
  import type { AudioView, CrossfaderAssign, MixerView, PlayerView } from "./types";

  let {
    mixer,
    models,
    players,
    audio,
    networkReady,
    run,
  }: {
    mixer: MixerView;
    models: ModelInfo[];
    players: PlayerView[];
    audio: AudioView;
    networkReady: boolean;
    run: (command: string, args?: Record<string, unknown>) => Promise<void>;
  } = $props();

  // Channel N carries player N; the rig has four players.
  const channels = [1, 2, 3, 4];
  const assigns: CrossfaderAssign[] = ["A", "Thru", "B"];
  // Top to bottom as on the mixer.
  const bands: [number, string][] = [
    [2, "HI"],
    [1, "MID"],
    [0, "LOW"],
  ];
  const EQ_KILL = -26;

  let chosenModel = $state(rememberedModel("mixer", DEFAULT_MIXER_MODEL));
  const chosenInfo = $derived(models.find((m) => m.id === chosenModel));

  function powerOn() {
    rememberModel("mixer", chosenModel);
    run("spawn_mixer", { model: chosenModel });
  }

  let devices = $state<string[]>([]);
  $effect(refreshDevices);

  function quick(command: string, args: Record<string, unknown>) {
    invoke(command, args).catch((e) => console.error(command, e));
  }

  function onAir(ch: number): boolean {
    return players.find((p) => p.number === ch)?.on_air ?? false;
  }

  function refreshDevices() {
    invoke<string[]>("audio_devices").then((d) => (devices = d));
  }

  // "" = system default, "-" = muted, otherwise a device name.
  const outputValue = $derived(audio.muted ? "-" : (audio.device ?? ""));

  function chooseOutput(value: string) {
    run("set_audio_output", {
      device: value === "" || value === "-" ? null : value,
      muted: value === "-",
    });
  }
</script>

<section class="djm">
  <div class="top">
    {#if mixer.running}
      <span class="title">{mixer.model_name}</span>
    {:else}
      <select class="model" bind:value={chosenModel} aria-label="Mixer model" title={chosenInfo?.notes}>
        {#each models as m (m.id)}
          <option value={m.id}>{m.name}</option>
        {/each}
      </select>
      <span class="hint">off: decks play straight out</span>
    {/if}
    <span class="spacer"></span>
    <button
      class="power"
      class:on={mixer.running}
      disabled={!mixer.running && !networkReady}
      title={mixer.running ? "Power off" : "Power on"}
      onclick={() => (mixer.running ? run("despawn_mixer") : powerOn())}
    >
      <Icon name="power" size={13} />
    </button>
  </div>

  <label class="output">
    <span>OUT</span>
    <select value={outputValue} onfocus={refreshDevices} onchange={(e) => chooseOutput(e.currentTarget.value)}>
      <option value="">System default</option>
      {#each devices.includes(audio.device ?? "") || !audio.device ? devices : [audio.device, ...devices] as d}
        <option value={d}>{d}</option>
      {/each}
      <option value="-">Muted</option>
    </select>
  </label>

  {#if mixer.running}
    <div class="strips">
      {#each channels as ch}
        {@const i = ch - 1}
        <div class="strip">
          <span class="ch">{ch}</span>
          <Knob
            label="TRIM"
            value={mixer.controls.trim_db[i]}
            min={-26}
            max={9}
            title="Trim {mixer.controls.trim_db[i].toFixed(1)} dB"
            onchange={(db) => quick("set_trim", { channel: ch, db })}
          />
          {#each bands as [band, name]}
            {@const db = mixer.controls.eq_db[i][band]}
            <Knob
              label={name}
              value={db}
              min={EQ_KILL}
              max={6}
              color={db <= EQ_KILL ? "#ff3b30" : "#2fa8ff"}
              title="{name} {db <= EQ_KILL ? 'kill' : `${db.toFixed(1)} dB`}"
              onchange={(v) => quick("set_eq", { channel: ch, band, db: v })}
            />
          {/each}
          <span class="led" class:lit={onAir(ch)} title="On air"></span>
          <Fader
            label="Channel {ch} fader"
            value={mixer.controls.channel_faders[i]}
            length={112}
            onchange={(level) => quick("set_channel_fader", { channel: ch, level })}
          />
          <div class="assign" role="group" aria-label="Crossfader assign">
            {#each assigns as a}
              <button
                class:lit={mixer.controls.assign[i] === a}
                onclick={() => run("set_crossfader_assign", { channel: ch, assign: a })}
              >
                {a === "Thru" ? "THRU" : a}
              </button>
            {/each}
          </div>
        </div>
      {/each}
      <div class="strip master">
        <span class="ch">M</span>
        <Knob
          label="MASTER"
          value={mixer.controls.master}
          min={0}
          max={1}
          reset={1}
          color="#ff9a1f"
          title="Master {(mixer.controls.master * 100).toFixed(0)}%"
          onchange={(level) => quick("set_master_level", { level })}
        />
      </div>
    </div>

    <label class="xf">
      <span>A</span>
      <Fader
        label="Crossfader"
        vertical={false}
        value={mixer.controls.crossfader}
        reset={0.5}
        centreMark
        length={200}
        title="Crossfader (double-click: centre)"
        onchange={(position) => quick("set_crossfader", { position })}
      />
      <span>B</span>
    </label>
  {/if}
</section>

<style>
  .djm {
    padding: 8px 10px;
    background: linear-gradient(#1c1c20, #141417);
    border-bottom: 1px solid var(--line);
  }

  .top,
  .output,
  .xf {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .title {
    font-weight: 800;
    letter-spacing: 0.12em;
    color: #d0d0d6;
  }

  .model {
    font-weight: 700;
    padding-top: 3px;
    padding-bottom: 3px;
  }

  .hint {
    color: var(--muted);
    font-size: 10px;
  }

  .spacer {
    flex: 1;
  }

  .power {
    padding: 3px 5px;
    color: var(--muted);
  }

  .power.on {
    color: var(--play);
  }

  .output {
    margin-top: 6px;
    font-size: 9px;
    font-weight: 700;
    color: var(--muted);
  }

  .output select {
    flex: 1;
    min-width: 0;
    font-weight: 400;
    font-size: 11px;
    padding-top: 3px;
    padding-bottom: 3px;
  }

  .strips {
    display: flex;
    justify-content: space-around;
    margin-top: 8px;
  }

  .strip {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 3px;
    width: 64px;
  }

  .strip.master {
    justify-content: flex-start;
  }

  .ch {
    font-weight: 800;
    font-size: 12px;
    color: #d0d0d6;
  }

  .led {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: #2a2a30;
  }

  .led.lit {
    background: var(--on-air);
    box-shadow: 0 0 6px var(--on-air);
  }


  .assign {
    display: flex;
    gap: 1px;
  }

  .assign button {
    padding: 1px 3px;
    font-size: 8px;
    font-weight: 700;
  }

  .assign button.lit {
    color: #000;
    background: var(--accent);
    border-color: var(--accent);
  }

  .xf {
    margin-top: 8px;
    color: var(--muted);
    font-weight: 700;
    font-size: 11px;
  }

  .xf {
    justify-content: center;
  }
</style>
