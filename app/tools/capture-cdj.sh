#!/bin/sh
# Record real Pro DJ Link traffic between real players/mixer and this Mac,
# one file per scenario, so the emulator can be compared byte for byte with
# real gear.
#
# Wiring: CDJs, DJM and this Mac on the same switch (a plain unmanaged switch
# is fine) or one CDJ straight to the Mac. Find the Mac's interface with
# `ifconfig` (the one that gets a 169.254.x.x address once the gear is on).
#
# A plain switch only passes this Mac broadcasts and traffic addressed to it,
# so Beat Link Trigger goes online from step 02 on: the gear then sends its
# status, position and mixer packets to the Mac. Traffic between the players
# themselves (LINK loads, master handoff) stays invisible; its effects show up
# in their status packets.
#
# Close the CDJ emulator (and anything else on ports 50000-50002) first.
#
# Usage: sudo app/tools/capture-cdj.sh <interface, e.g. en7> [output folder]
set -eu

IFACE=${1:?usage: sudo $0 <interface, e.g. en7> [output folder]}
OUT=${2:-captures/$(date +%Y%m%d-%H%M)}
mkdir -p "$OUT"

if [ "$(id -u)" -ne 0 ]; then
  echo "Run with sudo (tcpdump needs it)." >&2
  exit 1
fi

{
  echo "date: $(date)"
  echo "interface: $IFACE"
  ifconfig "$IFACE" | grep -E "inet |ether" || true
} > "$OUT/notes.txt"
printf "Gear and firmware (players: MENU > UTILITY), e.g. CDJ-3000X 1.10 x2, DJM-A9 1.20: "
read -r fw
echo "gear: $fw" >> "$OUT/notes.txt"
printf "Player numbers and mixer channels, e.g. CDJ1 on ch1, CDJ2 on ch2: "
read -r wiring
echo "wiring: $wiring" >> "$OUT/notes.txt"

scenario() {
  name=$1
  steps=$2
  echo
  echo "=== $name"
  echo "$steps"
  printf "Enter = start recording, s = skip: "
  read -r answer
  [ "$answer" = "s" ] && return 0
  tcpdump -i "$IFACE" -s 0 -U -w "$OUT/$name.pcap" 2>/dev/null &
  pid=$!
  sleep 1
  printf "Recording. Do the steps, then press Enter to stop: "
  read -r _
  kill -INT "$pid"
  wait "$pid" 2>/dev/null || true
  echo "saved $OUT/$name.pcap"
}

scenario 01_power_on \
"Beat Link Trigger and ShowKontrol closed. Switch everything off, start
recording, switch the mixer on, then each player. Wait until all screens
are up, then 20 s more."

scenario 02_blt_online \
"Open Beat Link Trigger, pick interface $IFACE, go online. Wait 20 s."

scenario 03_load_local \
"Insert a rekordbox USB into player 1. On player 1, load a track that has
hot cues, memory cues, a loop and artwork. Wait until BLT shows its title,
waveforms, artwork and cues (about 15 s)."

scenario 04_load_link \
"On player 2, browse player 1's USB over LINK and load a different track.
Wait until BLT shows it (about 15 s). Skip with s if you have one player."

scenario 05_play_mix \
"Play both players. On the mixer: channel 2 fader down and up, crossfader
fully to one side and back to the middle (with channels assigned A/B),
then a channel EQ low kill and back. Note roughly when you did what."

scenario 06_master_sync \
"Both playing. Press MASTER on player 2, wait 5 s, then SYNC on player 1
and move player 2's tempo fader a little. Then MASTER back on player 1."

scenario 07_transport \
"On player 1: PAUSE, CUE, PLAY, hot cue A, a 4-beat loop for two turns then
EXIT, tempo fader to about +3% and back, MASTER TEMPO on then off, REVERSE
on and off, play into a memory cue."

scenario 08_eject \
"Stop both players, eject the USB from player 1 (hold its USB stop button
until it says safe), wait 10 s. Note what player 2 does."

scenario 09_showkontrol \
"Only if ShowKontrol is installed (else s): quit BLT, open ShowKontrol,
connect it to the gear, re-insert the USB, load a track, play 20 s."

echo
echo "Done. Captures are in $OUT. Tell Claude that folder's path."
