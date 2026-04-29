# CDJ / DJM Emulator

## **! fully vibe-coded with claude code !**

A macOS-first emulator of a Pioneer / AlphaTheta CDJ + DJM Pro DJ Link setup.
Stands up to 4 virtual CDJ-3000 players and 1 virtual DJM on a software LAN
so that tools like **ShowKontrol**, **Beat Link Trigger**, **rekordbox**, and
other Pro DJ Link consumers see a credible CDJ setup without any physical
hardware.

**North-star use case:** a smaller rental company pre-visualising timecode
shows against individual tracks without owning or renting four CDJs and a DJM.

> Status: **pre-alpha, M3 in progress.** 4-CDJ + 1-DJM fleet, full Pro DJ
> Link protocol, per-player audio decode (Symphonia) + CoreAudio output
> (CPAL), beat clock phase-locked to the audio playhead, and a working
> **dbserver** that serves track metadata, waveform previews, beat grids,
> and album art to Beat Link Trigger / ShowKontrol. Pass `--library` to load
> a real Rekordbox USB export and get exact, per-track ANLZ data instead of
> synthesised waveforms.

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
read, linked, or redistributed.

## Repo layout

```text
app/                      Rust workspace (the emulator)
  crates/
    cdj-proto/            Pro DJ Link packet codec (no I/O)
    cdj-core/             Device orchestration, networking, timing, library reader
    cdjd/                 Development CLI
  vendor/
    rekordcrate/          Vendored rekordcrate 0.3.0 (fields made pub for PDB/ANLZ access)
prompt.md                 Original brief + clarifying Q&A
discovery.md              Living firmware / protocol / architecture notes
```

## Build & run

Requires Rust 1.75+ (tested on 1.91).

```sh
cd app
cargo test                 # run codec unit tests
cargo build                # build the CLI

# List network interfaces the emulator can bind on:
./target/debug/cdjd ifaces

# Full fleet: 4 CDJ-3000s + 1 DJM-V10 with synthetic metadata/waveforms.
./target/debug/cdjd run-fleet --iface feth0

# Same, but all 4 CDJs broadcast beat packets at 128 BPM immediately
# (no audio; timecode-only mode for ShowKontrol dev):
./target/debug/cdjd run-fleet --iface feth0 --bpm 128 --autoplay

# Load audio files into players 1 and 2, beats phase-locked to the playhead.
# --beat-offset-ms places beat 1 of bar 1 at the real downbeat of the track.
cargo build --release
./target/release/cdjd run-fleet --iface feth0 --bpm 128 --beat-offset-ms 100 \
    --track ~/Music/track1.wav \
    --track ~/Music/track2.flac

# Load a Rekordbox USB export: real waveforms, beat grids, and metadata from
# ANLZ/PDB files. Tracks assigned in PDB order to players 1..N.
# Export your library from rekordbox to a folder, then point --library at it.
./target/release/cdjd run-fleet --iface feth0 \
    --library /Volumes/MY_USB

# Single virtual CDJ (minimal mode; mostly for debugging):
./target/debug/cdjd run --iface feth0 --device-number 1 --model CDJ-3000
```

Verify the broadcasts on the wire (another terminal):

```sh
sudo tcpdump -i feth0 -nn -X 'udp port 50000 or udp port 50001 or udp port 50002'
```

You should see:

- 54-byte packets on :50000 every ~1.5 s per device (keep-alive, kind `0x06`).
- Short bursts of 44/50/38-byte packets on :50000 at startup (claim stages 1/2/3).
- 212-byte packets on :50002 every ~200 ms per CDJ (CDJ status, kind `0x0a`).
- 56-byte packets on :50002 every ~200 ms from the DJM (kind `0x29`).
- With `--autoplay`: 96-byte packets on :50001 at the BPM cadence per CDJ (beat, kind `0x28`).

All packets start with the `Qspt1WmJOL` magic
(`51 73 70 74 31 57 6d 4a 4f 4c`).

### Isolating the emulator with `feth` (recommended when also running ShowKontrol on this Mac)

`lo0` doesn't carry UDP broadcasts, so ShowKontrol can't see the emulator's
devices on loopback. Use a macOS `feth` virtual ethernet pair instead:

```sh
./target/debug/cdjd feth-plan                 # prints the sudo commands
./target/debug/cdjd feth-plan --teardown      # also prints teardown
```

Run the emitted commands, then point `cdjd run-fleet --iface feth0` and
ShowKontrol at the same feth interface.

## What Beat Link Trigger / ShowKontrol sees

When a dbserver client (BLT, ShowKontrol) queries a virtual deck:

| Feature | Without `--library` | With `--library` |
| ------- | ------------------- | --------------- |
| Title / Artist / Album / Genre / BPM | Synthetic ("Virtual Track 1") | Real PDB values |
| Waveform preview | Synthetic beat-pattern (blue bars) | Real ANLZ waveform |
| Beat grid | Synthesised from BPM | Real ANLZ beat grid |
| Album art | Placeholder (dark "CDJ EMUL" JPEG) | Placeholder (real art extraction pending) |

## Rekordbox USB export format

rekordbox writes exports in this layout:

```text
<root>/
  PIONEER/
    rekordbox.pdb               track/artist/album/genre database
    USBANLZ/<path>/ANLZ0000.DAT per-track analysis (beat grid, waveform preview)
    USBANLZ/<path>/ANLZ0000.EXT per-track extended analysis (colour waveform, cues)
```

The emulator reads `rekordbox.pdb` at startup (via the vendored `rekordcrate`
crate) and maps each track's `analyze_path` to its `.DAT` file on demand.

## Roadmap

Full milestone breakdown in [discovery.md §8](discovery.md). Summary:

- **M0** - Pro DJ Link packet codec + single-CDJ announce loop. **Done.**
- **M1** - 4 players + 1 mixer fleet; claim dance; idle CDJ/DJM status; `feth` helper. **Done.**
- **M2.1** - Beat packets on :50001, phase-locked `BeatClock`, `PlayerState` actor. **Done.**
- **M2.2** - Audio decode (Symphonia) + CPAL playback per player, shared playhead. **Done.**
- **M2.3** - Phase-locked `BeatClock` + `--beat-offset-ms`. **Done.**
- **M3.1** - dbserver (TCP 1051 + UDP 12523): metadata, waveform preview, beat grid, album art. **Done.**
- **M3.2** - Rekordbox USB export reader (`--library`): real PDB metadata + ANLZ waveforms/beat grids. **Done.**
- **M3.3** - Real album art from ANLZ `.EXT` files.
- **M3.4** - Full-detail colour waveform (EXT `WaveformColorPreview` / `WaveformDetail`).
- **M4** - Virtual DJM mix bus (sum players to a single output); crossfader / EQ / on-air flags.
- **M5** - Automatic beat-grid extraction (aubio) so `--beat-offset-ms` comes for free.
- **M6** - **ShowKontrol validation:** the real deliverable.
- **M7** - Tauri + Svelte desktop UI (deck view, mixer view, library browser).
- **M8** - Other CDJ models (2000 / 2000NXS / 2000NXS2).
- **M9** - Windows port.
- **M10** - MIDI control surface mapping.

## License

Source code: MIT OR Apache-2.0 (see per-crate `Cargo.toml`).

The AlphaTheta firmware and source-code drops referenced in `discovery.md`
are **not** part of this repository; they belong to AlphaTheta Corporation
and are covered by their own licences.
