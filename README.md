# CDJEmulator

A macOS-first emulator of a Pioneer / AlphaTheta CDJ + DJM Pro DJ Link setup.
Stands up up to 4 virtual CDJ-3000 players and 1 virtual DJM on a software LAN
so that tools like **ShowKontrol**, **rekordbox**, and other Pro DJ Link
consumers see a credible CDJ setup without any physical hardware.

**North-star use case:** a smaller rental company pre-visualising timecode
shows against individual tracks without owning or renting four CDJs and a DJM.

> Status: **pre-alpha, M1 complete.** Full 4-CDJ + 1-DJM fleet in a single
> process. Claim dance, keep-alive (:50000), and idle-state status
> (:50002) broadcasting. No audio, no track DB, no NFS yet.
> See [discovery.md](discovery.md) for the full backlog.

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
    cdj-core/             Device orchestration, networking, timing
    cdjd/                 Development CLI
prompt.md                 Original brief + clarifying Q&A
discovery.md              Living firmware / protocol / architecture notes
```

## Build & run

Requires Rust 1.75+ (tested on 1.91).

```sh
cd app
cargo test                 # run codec unit tests (15 passing)
cargo build                # build the CLI

# List network interfaces the emulator can bind on:
./target/debug/cdjd ifaces

# Full fleet: 4 CDJ-3000s + 1 DJM-V10 on the chosen iface.
./target/debug/cdjd run-fleet --iface en0

# Single virtual CDJ (M0 behaviour; mostly for debugging):
./target/debug/cdjd run --iface en0 --device-number 1 --model CDJ-3000
```

Verify the broadcasts on the wire (another terminal):

```sh
sudo tcpdump -i en0 -nn -X udp port 50000 or udp port 50002
```

You should see:

- 54-byte packets on :50000 every ~1.5 s per device (keep-alive, kind `0x06`).
- Short bursts of 44/50/38-byte packets on :50000 at startup (claim stages 1/2/3).
- 212-byte packets on :50002 every ~200 ms per CDJ (CDJ status, kind `0x0a`).
- 56-byte packets on :50002 every ~200 ms from the DJM (kind `0x29`).

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

## Roadmap

Full milestone breakdown in [discovery.md §8](discovery.md). Summary:

- **M0** - Pro DJ Link packet codec + single-CDJ announce loop. **Done.**
- **M1** - 4 players + 1 mixer fleet; claim dance; idle CDJ/DJM status; `feth` helper. **Done.**
- **M2** - Audio decode + time-stretch + phase-locked beat emission on :50001.
- **M3** - Per-player track storage (upload / USB passthrough), minimum-viable `export.pdb`.
- **M4** - In-process NFSv2 server + dbserver (TCP 1051).
- **M5** - **ShowKontrol validation:** the real deliverable.
- **M6** - Tauri + Svelte web UI (deck view, mixer view, library).
- **M7** - Other CDJ models (2000 / 2000NXS / 2000NXS2).
- **M8** - Windows port.
- **M9** - MIDI control surface mapping.

## License

Source code: MIT OR Apache-2.0 (see per-crate `Cargo.toml`).

The AlphaTheta firmware and source-code drops referenced in `discovery.md`
are **not** part of this repository; they belong to AlphaTheta Corporation
and are covered by their own licences.
