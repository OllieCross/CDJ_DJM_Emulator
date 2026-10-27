# Real-CDJ Wireshark Capture Plan

Goal: capture real Pro DJ Link traffic between two CDJ-3000s + DJM (+ optionally rekordbox laptop) so the emulator can be byte-diffed against ground truth. Every uncertain offset (status fields, dbserver responses, ANLZ payloads, artwork format) becomes verifiable.

## Hardware needed

1. **2x CDJ-3000** (minimum). Three or four is better. Mix of models OK but CDJ-3000 is the priority target.
2. **1x DJM** (any V10/A9/900NXS2 - V10 preferred since that's what we emulate).
3. **1x managed gigabit switch** with **port-mirroring** (a.k.a. SPAN). Examples: Netgear GS105Ev2/GS108Ev3, TP-Link TL-SG105E, Cisco SG250. Unmanaged switches will not work - they don't copy traffic to the capture port.
4. **1x ethernet cable per device** + 1 extra for the capture laptop.
5. **1x USB stick** with a rekordbox export containing at least 4 tracks (preferably the same "Abyss of Love - Franky Rizardo Remix" the emulator is being tested with, plus 3 others of varying BPM/key).
6. **1x capture laptop** running Wireshark (macOS/Linux/Windows all fine). Disk: each scenario is ~10-30 MB; 1 GB free is plenty.

## Switch wiring

```text
[CDJ-1] -- port 1 --|              |-- port 5 (MIRROR DESTINATION)  --> Capture Mac
[CDJ-2] -- port 2 --|  GS108Ev3   |
[CDJ-3] -- port 3 --|              |
[DJM]   -- port 4 --|              |
```

Configure the switch web UI:

- Source ports for mirroring: 1, 2, 3, 4 (all CDJ + DJM ports)
- Destination port: 5
- Direction: **both** (ingress + egress)

Verify by running `sudo tcpdump -i en0 -nn 'udp port 50000'` on the capture Mac. You should see 54-byte keep-alive bursts every ~1.5 s once a CDJ powers on.

## Wireshark setup on capture Mac

1. Install Wireshark
2. Open Wireshark, double-click the capture port interface (e.g. `en0` / `en7` / built-in ethernet).
3. Capture filter (BPF, fast):

   ```text
   udp port 50000 or udp port 50001 or udp port 50002 or tcp port 1051 or tcp port 12523 or port 2049 or port 111

   ```text
   (Port 2049/111 = NFS, in case CDJs serve NFS shares between each other.)

4. Display filter once recording (optional, just for browsing):

   ```text
   udp.port == 50002 or tcp.port == 1051
   ```

5. **Start a fresh capture per scenario below.** Save as `.pcapng`. Name files exactly as specified.

## Pre-flight checklist

- [ ] All devices on a fresh power cycle (no stale state).
- [ ] CDJs set to PRO DJ LINK / LINK mode. Channel numbers: CDJ-1 = ch 1, CDJ-2 = ch 2, etc.
- [ ] DJM in LINK mode (DJM-V10: Settings -> Pro DJ Link).
- [ ] USB inserted in CDJ-1 ONLY (initially). Other CDJs link via Pro DJ Link to read the same library.
- [ ] Wireshark recording before any CDJ is powered on.

## Scenarios to capture

Each scenario is ~30 s of capture unless noted. Hit **Stop** in Wireshark and **save** with the filename in backticks before moving to the next.

### 1. `01_announce.pcapng` - fresh fleet announce

1. Power on switch + DJM first.
2. Wireshark: Start capture.
3. Power on CDJ-1. Wait 15 s for it to come fully online (display shows "LINKED").
4. Power on CDJ-2. Wait 15 s.
5. Stop capture.

**Captures:** claim sequence (port 50000), keep-alive cadence, DJM announce.

### 2. `02_idle_fleet.pcapng` - steady idle

1. Wireshark: Start fresh capture.
2. All devices powered, no track loaded on either CDJ.
3. Wait 30 s.
4. Stop capture.

**Captures:** idle CDJ status (212-byte packets on 50002), DJM status, keep-alive rhythm.

### 3. `03_load_track.pcapng` - load track on CDJ-1

1. Wireshark: Start fresh capture.
2. Insert USB into CDJ-1 if not already; navigate to "Abyss of Love - Franky Rizardo Remix" (or any track).
3. Press LOAD onto deck 1.
4. Wait 5 s after waveform appears on CDJ display.
5. Stop capture.

**Captures:** dbserver port-discovery (TCP 12523), dbserver TCP 1051 session, ANLZ requests, metadata replies, waveform/beat-grid blobs, artwork.

### 4. `04_play.pcapng` - press play

1. With the same track still loaded:
2. Wireshark: Start fresh capture.
3. Press PLAY on CDJ-1.
4. Let it play for 30 s - try to be silent on the system otherwise.
5. Stop capture.

**Captures:** beat packets (port 50001) at the track's BPM, CDJ status with advancing beat_number / time, on-air flags.

### 5. `05_pause.pcapng` - pause mid-track

1. Wireshark: Start fresh capture.
2. CDJ-1 playing. Press PAUSE.
3. Wait 10 s.
4. Press PLAY again. Wait 10 s.
5. Stop capture.

**Captures:** status field changes around pause/play; whether beat packets stop on pause; what fields toggle.

### 6. `06_load_second.pcapng` - load different track

1. Wireshark: Start fresh capture.
2. On CDJ-1, load a DIFFERENT track from the USB (any other rekordbox track). Press LOAD.
3. Wait for waveform on CDJ-1 display. Hit PLAY.
4. Wait 10 s. Stop capture.

**Captures:** the load-track sequence we care about for Bug 3 - the order of status update vs dbserver ANLZ_TAG_REQ vs which track_id appears where.

### 7. `07_two_decks.pcapng` - both CDJs loaded + playing

1. Wireshark: Start fresh capture.
2. CDJ-1 already has a track. Press LINK on CDJ-2, browse to the USB, load any second track on CDJ-2.
3. Hit PLAY on CDJ-2 (CDJ-1 still playing or paused - either OK).
4. Cross the DJM crossfader fully right, then fully left. Then center.
5. Wait 10 s. Stop capture.

**Captures:** on-air flag toggling via DJM channels (Bug 2 cross-check + DJM channels_on_air field validation).

### 8. `08_master_change.pcapng` - master handoff

1. Wireshark: Start fresh capture.
2. Both CDJs playing. Press BEAT SYNC + MASTER on CDJ-2 to take over tempo master from CDJ-1.
3. Wait 10 s. Stop capture.

**Captures:** the master_handoff_source field on DJM status, the master flag (bit 5 at 0x89) toggling on CDJ status.

### 9. `09_browse_metadata.pcapng` - rekordbox laptop browsing (OPTIONAL - only if you also have a laptop running rekordbox on the same network)

1. Wireshark: Start fresh capture.
2. On the rekordbox laptop, open rekordbox -> LINK mode and let it browse the CDJ-1 USB library remotely.
3. Click around tracks: see title/artist/artwork/waveform load.
4. Stop capture.

**Captures:** real-PC dbserver client behavior, ARTWORK_REQ wire format, full waveform request flow.

### 10. `10_waveform_long.pcapng` - long play for full waveform data

1. Wireshark: Start fresh capture.
2. CDJ-1 loaded with the test track. Press PLAY from the start (or CUE then PLAY).
3. Let the entire track play to the end (~5 minutes).
4. Stop capture.

**Captures:** every status packet over a full track, beat number progression, time fields at known positions (useful for verifying time encoding for Bug 1).

## After capturing

1. Zip all .pcapng files: `zip -r realdata_captures_YYYYMMDD.zip *.pcapng`.
2. Drop into `app/captures/` in this repo (don't commit if files are large; rather keep locally and tell me the absolute path).
3. Note in a `captures/NOTES.md`:
   - Exact track titles used per scenario and their BPM/duration/key from rekordbox.
   - DJM channel assignments per CDJ.
   - Anything weird (e.g., CDJ flashed an error, USB had to be reinserted).
   - Time of day each capture started (so we can correlate packet timestamps).

## What I will do with the captures

- **Bug 1 (timer):** verify the byte offset and units of the beat-number / time-played fields on the wire. Confirm whether CDJ-3000 packs `M:S:F` separately or relies on `beat_number` + beat grid.
- **Bug 3 (waveform vanishes):** see exact ordering of dbserver track_id updates vs ANLZ_TAG_REQ traffic during a track switch.
- **Bug 5 (artwork):** byte-diff ARTWORK_REQ/RESP against the emulator. Confirm field ordering, leading bytes, any padding.
- **Bug 6 (waveform encoding):** byte-diff a real PWV4/PWV5/PWV6 response payload against what the emulator currently emits. The decompile already pinned the format (2 bytes/col packed BE for PWV5 RGB detail, 6 bytes/col raw for PWV4 RGB preview, 3 bytes/col for PWV6 THREE_BAND); the capture verifies those independently and catches anything the decompile missed.
- **Bug 7 (beat grid):** confirm the 20-byte leading header contents and the 16-byte per-record padding bytes.

## Quick capture without a switch (fallback)

If you can't get a managed switch:

1. **Hub option:** an old 10/100 ethernet **hub** (not switch) will broadcast every packet out every port. Cheap to find used. Limitation: 100 Mbps total, fine for Pro DJ Link.
2. **ARP-poisoning option:** run `bettercap` or `ettercap` on the capture laptop to spoof gateway. **Don't do this on a network you don't own.**
3. **Built-in macOS span:** if all devices route through a Mac with two ethernet ports, you can bridge en0/en1 in System Settings -> Sharing -> Internet Sharing and Wireshark the bridge interface. Messy but works.

Managed switch is the cleanest path. Roughly $35 used.
