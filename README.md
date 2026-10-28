# CDJ / DJM Emulator

## **! fully vibe-coded with claude code !**

A macOS app that runs up to 4 virtual CDJ players and 1 virtual DJM mixer
on a Pro DJ Link network inside your Mac, so that **ShowKontrol**, **Beat Link
Trigger** and other Pro DJ Link software see a credible DJ setup without any
physical hardware. Decks load tracks from rekordbox USB/SD exports, play them
through a virtual mixer, and report everything (status, beats, playhead, track
metadata, waveforms) on the network the way real players do.

**North-star use case:** a smaller rental company programming timecode,
lighting or video shows against individual tracks without owning or renting
four CDJs and a DJM.

> Status: **pre-alpha.** Verified against Beat Link Trigger's own code
> (beat-link) on the live virtual network: it sees all players and the mixer,
> their status, beats, positions and mixer data, and fetches track metadata,
> beat grids and waveforms from the players. **Not yet checked with
> ShowKontrol or against captures of real gear.**

## What it does

- **Rig:** switch players 1-4 and the mixer on and off at runtime, each as
  the model you pick. Each device announces itself and sends status, beats
  and (CDJ-3000 style) absolute-position packets from its own address.
- **Models:** players can be a CDJ-3000, CDJ-3000X, CDJ-2000NXS2,
  CDJ-2000NXS, CDJ-2000 or XDJ-1000MK2; the mixer a DJM-V10, DJM-A9,
  DJM-900NXS2 or DJM-750MK2. What changes per model is what's publicly
  known to differ on the network: the device name, the status packet length
  (0x200 / 0x11c / 0xd4 / 0xd0), absolute-position packets (CDJ-3000s only),
  3 or 8 hot cues, and 4 or 6 mixer channels. `cdjd models` lists what is
  still a guess (e.g. the CDJ-3000X's exact name on the network).
- **Media:** every player has a USB and an SD slot. Exports on mounted drives
  (`/Volumes`) are found automatically; folders can be added. Any player can
  load from any other player's media over LINK.
- **Decks:** CUE (with hold-to-preview), hot cues A-H and memory cues from
  rekordbox, beat loops and manual loops, tempo fader with ±6/10/16/WIDE,
  master tempo (key lock), reverse, quantize, sync and tempo master, needle
  search and jog wheel. Beats follow the track's rekordbox beat grid and each
  beat packet is sent when that beat is heard.
- **Mixer:** trim, 3-band EQ with kill, channel faders, crossfader with A/THRU/B
  assign, master level; on-air state goes out to players and clients. Audio
  plays through one output device of your choice (or none).
- **dbserver:** answers metadata, artwork, beat grid, cue, waveform and
  analysis-tag requests and menu browsing for the media in each player.

## Requirements

- macOS (Apple Silicon or Intel), an administrator password (for the virtual
  network).
- Rust 1.77+, Node.js 20.19+ and the Xcode Command Line Tools (the
  time-stretcher is C++) to build.
- Optional: Java 17+ and [Beat Link Trigger](https://github.com/Deep-Symmetry/beat-link-trigger)
  (for the cross-check tests and the probe), Wireshark (for captures).

## Run the app

```sh
npm --prefix ui install
npm --prefix ui run tauri dev
```

Then, in the app:

1. **Set up network** (top bar). macOS asks for an administrator password
   once; this creates the virtual cable (`feth0`/`feth1`, see below).
2. Pick a model and **power on** each player and the mixer.
3. **Insert** a rekordbox export into a player's USB or SD slot (drives under
   `/Volumes` are listed; **+ FOLDER** in BROWSE adds any export folder).
4. In **BROWSE**, pick a track and **LOAD TO** a player, then play it.
5. Point ShowKontrol / Beat Link Trigger at the interface **feth1**
   (`10.77.77.200`). Pick an audio output under the mixer's **OUT**.

`npm --prefix ui run dev` opens the UI in a normal browser against a mock
backend (demo decks, no network or audio) for working on the interface.

## Command line

```sh
cd app
cargo run --bin cdjd -- net-up              # create the virtual network (admin prompt)
cargo run --bin cdjd -- run-fleet --library /Volumes/MY_USB
cargo run --bin cdjd -- run-fleet --autoplay --silent   # timecode only, no tracks
cargo run --bin cdjd -- run-fleet --player-model CDJ-3000X,CDJ-3000X,CDJ-2000NXS2 --mixer-model DJM-A9
cargo run --bin cdjd -- models              # player and mixer models
cargo run --bin cdjd -- audio-devices       # list outputs for --audio-device
cargo run --bin cdjd -- net-down            # remove the virtual network
```

`run-fleet --library` puts the export in player 1's USB slot and loads its
first tracks onto the players (2-4 over LINK). `feth-plan` prints the network
commands instead of running them.

## The virtual network

Client apps need each virtual device at its own IP address, on a network
interface they can listen on. The app creates a macOS `feth` pair, a virtual
Ethernet cable:

| Interface | Addresses | Who |
| --- | --- | --- |
| `feth0` | `10.77.77.1`-`.4` (players), `.33` (mixer) | the emulator |
| `feth1` | `10.77.77.200` | ShowKontrol, Beat Link Trigger |

Details that matter (all handled by the setup):

- The subnet's route belongs to `feth1` so client apps can reach the devices;
  an extra route scoped to `feth0` lets the emulator reach the client apps.
- Discovery packets (claims, keep-alives) are broadcast. Status, beats,
  positions and mixer packets go straight to `10.77.77.200`: macOS hands a
  sender a local copy of every broadcast on a virtual interface, so a client
  on the same Mac would otherwise receive everything twice.
- The emulator never binds UDP 50000-50002, so Beat Link Trigger or
  ShowKontrol can run on the same Mac. Beat Link Trigger warns that `feth0`
  sees the same network; that is expected and harmless.

## Repo layout

```text
app/                      Rust workspace
  crates/
    cdj-proto/            Pro DJ Link packet codecs (no I/O)
    cdj-core/             Rig, decks, audio engine, mixer, media, dbserver, network
    cdjd/                 Command line
  vendor/rekordcrate/     rekordcrate 0.3.0 with some fields made public
  tools/
    beatlink-probe/       Goes online with beat-link and reports what it sees
    capture-cdj.sh        Guided capture of real gear (see below)
ui/                       Tauri app: Svelte UI (src/) and Rust shell (src-tauri/)
discovery.md              Firmware / protocol / architecture notes
```

## Testing

```sh
cd app && cargo test --workspace
```

Packet layouts are checked byte for byte against beat-link's own templates;
with Beat Link Trigger installed (or `BEATLINK_JAR` set) the tests also parse
the emulator's packets and dbserver replies with beat-link itself, using the
real rekordbox demo export in `vendor/rekordcrate/data`.

On a live setup, with Beat Link Trigger closed:

```sh
java -cp "/Applications/Beat Link Trigger.app/Contents/app/beat-link-trigger.jar" \
    app/tools/beatlink-probe/Probe.java 15
```

**Captures of real gear** (CDJs, DJM and this Mac on one switch):
`app/tools/capture-cdj.sh <interface>` walks through power-on, loading,
mixing, master/sync and deck controls, one file per step, into `captures/`
(git-ignored: recordings contain your devices' MAC addresses).

## Known limits

- Client apps can't send commands to the virtual decks yet (become master,
  sync, load track, media queries): the emulator doesn't listen on UDP
  50000-50002 so those apps can bind them.
- No NFS file sharing; clients use the dbserver. Whether ShowKontrol needs
  NFS is not known yet.
- Model differences beyond those listed above (per-model menus, the DJM-V10's
  4-band EQ, older players' track-data limits) aren't emulated; several
  per-model details are educated guesses until checked against real units.
- macOS only.

## Why not run the real firmware?

We looked. [discovery.md §1](discovery.md#1-what-the-provided-files-actually-are)
has the detail; short version:

- The official `.UPD` firmware file is a LUKS-encrypted volume. AlphaTheta
  holds the key; no PC-side unlock path exists.
- The "open source" tarball AlphaTheta publishes is only the GPL-mandated
  Yocto / kernel bits. The CDJ DJ application itself is proprietary and
  encrypted inside that `.UPD`.

So this project is a **clean-room reimplementation of the Pro DJ Link
protocol** based on the public analysis by the Deep-Symmetry project
(dysentery, beat-link, crate-digger). No Pioneer / AlphaTheta source code is
read, linked, or redistributed. The UI uses its own icons and artwork.

## License

Source code: MIT OR Apache-2.0 (see per-crate `Cargo.toml`). The vendored
rekordcrate is MPL-2.0; the Signalsmith Stretch time-stretcher is MIT.

The AlphaTheta firmware and source-code drops referenced in `discovery.md`
are **not** part of this repository; they belong to AlphaTheta Corporation
and are covered by their own licences.
