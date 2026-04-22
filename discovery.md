# Discovery - CDJ Emulator

> Living document. Captures what the CDJ-3000 actually is, what's legally/technically accessible from the firmware drop AlphaTheta publishes, which public reverse-engineering efforts we can stand on, and what that means for our emulator architecture.

---

## 1. What the provided files actually are

### 1.1 `firmware_v322.zip` - `CDJ3Kv322.UPD` (~155 MB)

- **Format:** LUKS1 encrypted volume (`aes-xts-plain64`, `sha256`, UUID `9424e626-da90-4eb0-a72f-a1a32e8326e0`).
- Confirmed via `file(1)` and magic bytes (`LUKS\xba\xbe\x00\x01`) at offset 0.
- **Implication:** The real Pioneer application binaries, UI assets, and Pro DJ Link stack live inside this encrypted blob. Without the LUKS passphrase/key (held by AlphaTheta and loaded by the bootloader on-device), we cannot extract or analyse them on a PC. Only the device's own bootloader/TEE can unlock it.
- **Verdict:** Running the real firmware in a VM on macOS is **not feasible**. Even if it were unlocked, it is built for Renesas R-Car Gen3 (ARM64) with device-tree, GPU (PowerVR), DRM, FPGA and dedicated audio DSP dependencies no general-purpose hypervisor can satisfy.

### 1.2 `Source_code_v150.bz2` (~24 MB)

Tar contents (top-level):

```text
CDJ-3000/meta-cdj3k/        - Pioneer's own Yocto layer (only GPL-required bits)
CDJ-3000/meta-renesas/      - Renesas R-Car Gen3 BSP
CDJ-3000/meta-linaro/
CDJ-3000/meta-openembedded/
CDJ-3000/poky/              - Yocto build system
CDJ-3000/build/             - Build config
CDJ-3000/hfsprogs.tar.bz2
CDJ-3000/mpg123.tar.bz2
```

**What this is:** the GPL-mandated source drop - kernel 4.9, kernel patches, bootloader, open-source userspace (GStreamer, ALSA-utils, boost, gnutls, mpg123, hfsprogs, etc.).

**What this is NOT:** the proprietary CDJ-3000 DJ application, the Pro DJ Link stack, the rekordbox DB loader, the UI (JUCE-based), the touchscreen app, or anything that would let us just recompile a CDJ for x86. Those are proprietary and live only inside the encrypted `.UPD`.

**Still useful because it tells us what the device is made of:**

| Component | Detail |
| --- | --- |
| SoC | Renesas R-Car Gen3 (H3 / M3, ARM Cortex-A57 + A53, big.LITTLE) |
| Kernel | Linux 4.9, heavily patched (meta-renesas + Pioneer patches) |
| Distro | Yocto / Poky |
| UI framework | **JUCE** (inferred from kernel patch `add-juce-alsa-trace.patch`) |
| Audio decode | GStreamer 1.6 + OpenMAX IL; libalacdla-l (ALAC), libflacdla-l (FLAC), MP3/AAC via OMX |
| Audio I/O | ALSA + ADSP (dedicated audio DSP firmware, `adsp-fw-module.bb`) |
| Network switch | Realtek RTL8367 (the two "daisy-chain" Ethernet ports) |
| USB gadget modes | MIDI, UAC2 (audio class), HID (kernel patches `usb_gadget_midi.patch`, `usb_gadget_uac2.patch`, `usb_gadget_hid.patch`) |
| Pro DJ Link | Kernel config patch `add-config-pdj.patch` - some kernel-side awareness; actual stack is userspace/proprietary |
| Filesystem | HFS+ read support via `hfsprogs` (for Mac-formatted USB drives) |

**Takeaway:** the open-source tree confirms the platform but gives zero help for reproducing the proprietary DJ behaviour. Our value extraction from this tarball is limited to:

1. Knowing the audio pipeline reference (GStreamer + OMX + ALSA) - we can mirror its semantics.
2. Knowing the decoder set we must support (MP3, AAC/M4A, ALAC, FLAC, WAV, AIFF).
3. Confirming HFS+ and exFAT support requirements for mounted USB drives.

---

## 2. Pro DJ Link - the protocol we actually need to emulate

Pro DJ Link is Pioneer's proprietary link protocol, undocumented by the vendor but **extensively reverse-engineered** by the Deep-Symmetry project. Our emulator does not need Pioneer's code; it needs to speak this protocol convincingly enough that ShowKontrol, rekordbox, and other CDJs/DJMs treat our VMs as real devices.

### 2.1 Transport

- Pure Ethernet LAN, typically auto-IP `169.254.0.0/16` (link-local) but any subnet works.
- Three UDP ports on every device:

| Port | Name | Purpose |
| --- | --- | --- |
| **50000** | Announce / Discovery | Device-type broadcast packets announcing model, MAC, IP, channel number |
| **50001** | Beat / Sync | Beat broadcast packets (timestamps, BPM, bar position) - the time source for DJM master sync |
| **50002** | Status / Control | Player status (track, position, tempo slider, play state, on-air), and control messages (load track, start/stop, master handoff) |

Every device broadcasts on :50000 roughly every 1.5 s and on :50001 on **every beat** at the current BPM. :50002 is a rich mixed broadcast/directed port carrying CDJ-status and DJM-status packets (multiple packet sub-types identified by a 1-byte "kind" field following the magic header).

### 2.2 Packet framing

All packets start with the magic `51 73 70 74 31 57 6d 4a 4f 4c` ("Qspt1WmJOL") - this is the identifier Pioneer chose and is the single most important byte sequence we must emit correctly. Packet layout is covered in full in dysentery's `packets.pdf` analysis document.

### 2.3 The roles that must exist on the network

1. **Players (CDJs)** - channels 1..4, send CDJ status on :50002 and beats on :50001.
2. **Mixer (DJM)** - channel 33 in Pro DJ Link numbering, sends DJM status (fader positions, on-air flags, master BPM) on :50002, controls which player is tempo master.
3. **rekordbox / laptops** - higher channel numbers, behave like players but announce as "rekordbox".
4. **Tempo master** - exactly one device at a time; elected via a handoff dance on :50002.

For a credible 4-player emulator we need roles 1 + 2, plus correct tempo-master election so beat sync works.

### 2.4 Track database / metadata exchange (the "NFS" part)

This is what the prompt calls "NFS storage through a virtual ethernet device". The **real** mechanism is:

- Each CDJ exports its mounted media (USB slot, SD slot, rekordbox collection) over **Sun NFS v2 over UDP** (port 2049 + mountd on an ephemeral port announced via portmap/rpcbind on UDP 111).
- Other devices request tracks by mounting the remote share and reading raw files - rekordbox-prepared drives contain a `PIONEER/rekordbox/export.pdb` file (and siblings: `exportExt.pdb`, `.EXT` folder with waveforms and cue art).
- `export.pdb` is a proprietary binary database reverse-engineered by the **crate-digger** project: holds the playlist/track/artist/album tables, track filepaths, cue point tables, beat grids, waveform summary pointers.
- Per-track analysis files (`ANLZxxxx.DAT`, `.EXT`) hold beat grids, cue/loop points, waveform data.
- **Dbserver** (TCP 1051, "CDJ database query protocol") is an *alternative* to NFS for metadata fetch - a custom request/response protocol. Modern CDJs (including the 3000) primarily use the NFS+PDB path but still speak dbserver for backwards compatibility; rekordbox laptops expose dbserver, not NFS. Full emulation should cover both.

### 2.5 What ShowKontrol actually needs from us

ShowKontrol (Pioneer/AlphaTheta show-control software for timecode/video cue integration) connects to the Pro DJ Link network and reads:

- Player `status` packets (track ID, playhead position, BPM, play/pause) from :50002, and
- beat packets from :50001 to derive timecode.

It does *not* need NFS/dbserver to work. So the **smallest slice** that unblocks ShowKontrol integration is: announce + beat + CDJ-status + DJM-status packets for 4 players + 1 mixer with a real audio pipeline behind each player producing correctly-timed beat packets. NFS/dbserver/track loading are additive polish.

---

## 3. Existing open-source building blocks

| Project | Language | What it does | How we use it |
| --- | --- | --- | --- |
| **dysentery** (Deep-Symmetry) | Clojure + long-form PDF analysis | Packet format reference doc | Spec we implement against |
| **beat-link** | Java | Read-only client; parses all Pro DJ Link packets, renders beat grids, follows tempo master | Copy data structures & packet decoders; also test harness - point real beat-link at our emulator, it must see us as real CDJs |
| **crate-digger** | Java | Parse `export.pdb` + ANLZ files | Reuse directly to read user-supplied rekordbox drives, and later to generate our own when users upload raw MP3s |
| **prolink-connect** | TypeScript | Node client with NFS + dbserver implementations, track fetching | Reference for the NFS layer and dbserver protocol; possibly embed if we build in TS |
| **Deep-Symmetry/open-beat-control** | Clojure | Emits beat events over OSC | Optional bridge to ShowKontrol-style consumers |
| **libnfs** | C | Userspace NFSv2/3 client+server | Candidate for the NFS server side of each virtual CDJ (bundling an in-process NFS server avoids OS-level NFS exports) |
| **rtpMIDI / dsnode** | - | Ethernet audio-over-IP | *Not* what Pro DJ Link uses (PDL audio is analog via RCA on real units). Irrelevant. |

**Important clarification of a user assumption:** beat-link/prolink-connect are not "read-only observers by architectural limitation" - they are clients. There is no open-source server/emulator side today. That is precisely the gap this project fills. Our emulator will re-use their *parsers and data models* but invert them: where they decode packets into state, we encode our simulated state into packets.

---

## 4. Audio

### 4.1 Decoders to support

Minimum set (matches real CDJ-3000): **MP3, AAC/M4A, ALAC, FLAC, WAV, AIFF**. On macOS we get all of these free via `AVAudioFile` / CoreAudio's ExtAudioFile, or FFmpeg/libav if we want cross-platform symmetry for a later Windows port.

### 4.2 Tempo / pitch

Real CDJ-3000 does time-stretch without pitch change (master tempo) and pitched playback. We need a time-stretching library: **Rubber Band** (GPL/commercial) or **SoundTouch** (LGPL) are the obvious picks. SoundTouch is probably right for v1 (permissive license, good enough quality for pre-viz use).

### 4.3 Audio routing on the host

- 4 players × stereo + 1 mixer-out stereo = 10 channels worth of routing.
- On macOS, ship an **Audio Unit / CoreAudio virtual device** (Aggregate / BlackHole-style) so DAWs/ShowKontrol can capture mixer output.
- Each player also routes into an in-process virtual DJM which applies channel faders, crossfader, EQ, filter - master bus.
- Beat-accurate clock domain: the mixer is the master clock source; players phase-lock to it. This matches real Pro DJ Link semantics (master CDJ/DJM dictates beat; others slave).

---

## 5. Virtualisation / isolation strategy

The prompt asks for "mini-VMs". After the facts above, a true VM per player is pointless (no Pioneer binary to run, and each player is just a user-space audio+network process). Three viable levels of isolation, in increasing cost:

1. **Single process, multiple actors** (recommended v1). Each "virtual CDJ" is a struct/actor within one app, each with its own thread(s), virtual NIC (see §6), audio pipeline, and Pro DJ Link session. Fast, easy to debug, zero VM overhead.
2. **Sub-process per player.** Each CDJ runs in a child process, talks to a parent orchestrator. Better crash isolation; slightly higher IPC cost.
3. **True VMs via Apple `Virtualization.framework`.** Only buys real-hardware fidelity if/when we run actual Pioneer firmware - which we cannot. Skip.

Recommendation: **Option 1 with a clean actor/service boundary** so Option 2 remains possible without rewrites.

---

## 6. The "virtual ethernet" problem

Pro DJ Link devices identify each other by **source MAC + IP** on broadcast packets. Four emulated CDJs all sending from the host's single NIC with one MAC will collide in the protocol. Options:

- **`utun` / `feth` interfaces on macOS**: create 5 virtual interfaces (4 players + 1 mixer) each with its own link-local IPv4 and synthetic MAC; bridge them into a software switch. `feth` (available since macOS 10.13) is the right primitive - pairs of Ethernet-like interfaces. Can be configured fully from user space with root.
- **User-space L2 switch**: each virtual CDJ has a virtual NIC (tun/tap or an in-process socket pair), frames routed by an in-app switch. No kernel-level interface needed if external tools don't need to see the CDJs as separate hosts. Works fine for internal emulation; insufficient for connecting external hardware or another machine.
- **Hybrid:** internal soft-switch for inter-CDJ traffic, with a single bridged `feth` uplink so external ShowKontrol/rekordbox on the same LAN sees all five devices.

Decision to revisit during implementation: start with pure user-space soft-switch (fastest path to ShowKontrol-on-same-machine working) and add the bridged uplink when external hardware needs to see the virtual CDJs.

---

## 7. Recommended tech stack

| Layer | Choice | Why |
| --- | --- | --- |
| App shell | **Tauri** (Rust core + web UI) | Native on macOS, tiny, supports the web UI the user wants per-VM, and Rust gives us real-time-safe audio/networking primitives. Windows port later is basically free. |
| Protocol core | **Rust** | Binary packet wrangling, precise timing, no GC pauses during beat packet emission. Port beat-link's parsers. |
| Audio engine | **CPAL** (device I/O) + **Symphonia** (decode) + **Rubber Band bindings** *or* **SoundTouch** (time-stretch) | All Rust, all cross-platform, all permissive licences. |
| `export.pdb` / ANLZ parsing | Port or FFI to **crate-digger** (Java) - likely port to Rust, it's pure parsing of documented formats | Needed when user mounts an existing rekordbox USB drive |
| NFS server | Embed **libnfs** via FFI, or hand-roll NFSv2 (tiny spec) in Rust | Serve each virtual drive out to other virtual CDJs; libnfs is battle-tested |
| Virtual NICs | `feth` (macOS), tun/tap fallback | §6 |
| Frontend | **SvelteKit** or **React** in Tauri webview | Per-player deck UI + setup overview + track library |
| Packaging | `cargo-bundle` / Tauri bundler - signed `.app` + DMG | macOS-first delivery |

If the user strongly prefers a single-language stack, **Go** is the other reasonable pick (goroutines map nicely to per-player actors, `net` package is excellent for UDP/NFS, but real-time audio story is weaker than Rust + CPAL).

---

## 8. Milestone backlog

### M0 - Protocol spike (1-2 weeks)

- Implement Pro DJ Link packet encoders/decoders (announce, keep-alive, beat, CDJ-status, DJM-status) in Rust.
- Stand up 1 virtual CDJ + 1 virtual DJM in a single process, broadcasting on the host NIC.
- Verify with beat-link-trigger / rekordbox: both see our devices on the network.

### M1 - Multi-device isolation

- Virtual NIC layer (start user-space soft-switch).
- 4 players + 1 mixer concurrently on the virtual LAN.
- Tempo-master election implemented correctly.

### M2 - Audio pipeline

- Per-player decode + time-stretch + phase-locked beat emission tied to playhead.
- Virtual DJM mix - CoreAudio output device.
- Beat packets must align with audio within < 1 ms (this is what makes the emulator usable for timecode).

### M3 - Track storage

- Per-player virtual USB drive (user-uploaded MP3/FLAC/etc.).
- Auto-analysis to synthesise minimum viable `export.pdb` + ANLZ (beat grid via `aubio` or similar; cues default empty).
- Real rekordbox-prepared USB/SD passthrough.

### M4 - NFS + dbserver

- In-process NFSv2 server per player.
- Dbserver (TCP 1051) implementation for laptop-style clients.
- Cross-mounting: Player A can load a track from Player B's slot.

### M5 - ShowKontrol validation (the real deliverable)

- Boot emulator, connect ShowKontrol, fire timecode, verify audio output routes to show outputs (BlackHole-style virtual device or aggregate).

### M6 - Polish

- Web UI for each deck (waveform, playhead, cue buttons, loop, tempo slider).
- Library management for uploaded tracks.
- Persistence / session save.
- Installer, code signing, notarisation.

### M7 (stretch) - Other models

- Abstract the "device profile" (announce strings, packet quirks) so CDJ-2000 / 2000NXS / 2000NXS2 can be selected per-player.

### M8 (stretch) - Windows port

- Already mostly free thanks to Rust/Tauri; biggest deltas are the virtual NIC layer (wintun) and audio device (WASAPI, which CPAL already handles).

### M9 (stretch) - MIDI control surface support

- Map MIDI input to deck controls (play/cue/pitch/jog).

---

## 9. Known risks & open questions

1. **Pro DJ Link checksum / auth?** Some undocumented packets in newer firmware are suspected to carry integrity fields or model-dependent quirks. We will discover these the hard way by diffing our emissions against real-CDJ captures the user (or we) can obtain. Mitigation: pcap any sessions the user has access to; compare against beat-link-trigger logs.
2. **Time-stretch quality vs. licence.** Rubber Band R2 is better-sounding but GPL unless licensed commercially; SoundTouch is LGPL-2.1 and permissively linkable. For v1, SoundTouch.
3. **ShowKontrol specifics.** We have no reverse-engineered understanding of *exactly* which Pro DJ Link fields ShowKontrol reads. Practical plan: confirm with ShowKontrol on a bench using a real CDJ first, log its traffic, then aim our emulator at that trace.
4. **Signed virtual audio device on macOS.** Distributing a virtual CoreAudio device requires either a kernel extension (deprecated) or a DriverKit driver (signed, entitlements). Cheaper v1: instruct users to install BlackHole or similar; ship our own branded virtual device in M6.
5. **`export.pdb` write path.** Reading is well-understood (crate-digger). *Writing* a valid rekordbox DB so real CDJs could consume our virtual drives is a bigger effort - only needed if someone wants to use the emulator *as a source* for real CDJs, which is out of scope for the current goal.
6. **Legal standing.** Implementing a protocol (clean-room, no Pioneer code, no leaked keys) is defensible in most jurisdictions. We must never link against, ship, or attempt to decrypt the `.UPD` file. That file should be considered out of bounds for the build - keep it in the repo only as a reference artefact that we do not read at runtime.

---

## 10. Short version (for reminding future sessions)

- **Can't run Pioneer firmware directly:** `.UPD` is LUKS-encrypted, source drop is only GPL bits.
- **Clean-room reimplementation** of Pro DJ Link + NFS + dbserver + audio pipeline, per user consent.
- **Target 1:** CDJ-3000 model profile. Others later.
- **Architecture:** single Rust app, actor-per-virtual-device, in-process soft-switch, Tauri web UI, CoreAudio output, optional virtual-audio-device for ShowKontrol capture.
- **North star:** ShowKontrol connects, sees 4 CDJs + 1 DJM, reads timecode, and audio comes out the mixer bus.
