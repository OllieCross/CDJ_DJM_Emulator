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
# Records with Wireshark's dumpcap when Wireshark is installed (no sudo
# needed, files open straight in Wireshark), else with tcpdump (needs sudo).
#
# Usage: app/tools/capture-cdj.sh <interface, e.g. en7> [output folder]
set -eu

IFACE=${1:?usage: $0 <interface, e.g. en7> [output folder]}
OUT=${2:-captures/$(date +%Y%m%d-%H%M)}
DUMPCAP=/Applications/Wireshark.app/Contents/MacOS/dumpcap

if [ -x "$DUMPCAP" ]; then
  EXT=pcapng
  record() { "$DUMPCAP" -q -i "$IFACE" -w "$1"; }
elif [ "$(id -u)" -eq 0 ]; then
  EXT=pcap
  record() { tcpdump -i "$IFACE" -s 0 -U -w "$1"; }
else
  echo "Install Wireshark, or run with sudo to record with tcpdump." >&2
  exit 1
fi
mkdir -p "$OUT"

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
  record "$OUT/$name.$EXT" 2>/dev/null &
  pid=$!
  sleep 1
  printf "Recording. Do the steps, then press Enter to stop: "
  read -r _
  kill -INT "$pid"
  wait "$pid" 2>/dev/null || true
  echo "saved $OUT/$name.$EXT"
}

scenario 01_power_on \
"BLT and ShowKontrol closed, all gear off, USB not inserted.
After Enter: DJM on, wait 10 s; player 1 on, wait 10 s; player 2 on.
Wait until both players show their home screen, then 30 s more."

scenario 02_blt_online \
"Open Beat Link Trigger, choose $IFACE, go online. Open Network >
Show Player Status. Wait 30 s without touching the gear."

scenario 03_load_local \
"Insert the USB into player 1, wait 5 s. On player 1 load track A (has hot
cues, memory cues, a saved loop, artwork). Wait until BLT's player status
shows title, artwork and waveform, then 15 s more. Don't press PLAY."

scenario 04_load_link \
"On player 2 open SOURCE, pick player 1's USB, load track B (different
BPM). Wait until BLT shows it, then 15 s more. Don't press PLAY."

scenario 05_play_mix \
"5 s between actions. Mixer: ch1+ch2 faders up, crossfader centre, ch1
assign A, ch2 assign B. PLAY player 1, PLAY player 2, wait 10 s.
Ch2 fader down, wait 5 s, up. Crossfader fully A, 5 s, fully B, 5 s,
centre. Ch1 LOW EQ fully down, 5 s, back to centre. Ch2 TRIM down, back."

scenario 06_master_sync \
"Both still playing, 5 s between actions. MASTER on player 2, wait 10 s.
SYNC on player 1, wait 10 s. Player 2 tempo fader to about +4%, wait
10 s, back to 0. SYNC off on player 1. MASTER on player 1, wait 10 s."

scenario 07_transport \
"Player 1 only, 5 s between actions. Pause player 2 first.
PAUSE, CUE, PLAY 5 s, hot cue A, hot cue B, saved loop (RELOOP or its
hot cue) two turns then EXIT, 4-beat AUTO LOOP two turns then EXIT, tempo
range 6% > 10% > 16% > WIDE > back, tempo fader +3% then back, MASTER
TEMPO on then off, REVERSE on 5 s then off, CALL next memory cue, touch
the jog top while playing for 2 s, needle-search to the last 20 s and let
the track end, wait 10 s."

scenario 08_eject \
"Player 2: load another track from player 1's USB over LINK, PLAY it.
Then press STOP-USB on player 1 until it says it's safe, pull the USB,
wait 15 s and note what player 2 shows."

scenario 09_showkontrol \
"Only if ShowKontrol is installed (else s): quit BLT, re-insert the USB,
open ShowKontrol and connect it, load track A on player 1, PLAY 30 s,
PAUSE, CUE."

echo
echo "Done. Captures are in $OUT. Tell Claude that folder's path."
