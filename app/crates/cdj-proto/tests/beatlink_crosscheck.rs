//! Cross-checks the packet encoders against beat-link's own parsers (the
//! Java library behind Beat Link Trigger).
//!
//! Needs `java` (11+) on PATH and a jar containing beat-link: set
//! `BEATLINK_JAR`, or have Beat Link Trigger installed in `/Applications`.
//! Skipped (with a note on stderr) when neither is available.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use cdj_proto::{
    ActiveLoop, Beat, CdjStatus, DeviceName, DjmStatus, PlayState1, PlayState3, PrecisePosition,
    TrackSourceSlot, TrackType,
};

const BLT_JAR: &str = "/Applications/Beat Link Trigger.app/Contents/app/beat-link-trigger.jar";

fn beatlink_jar() -> Option<PathBuf> {
    if let Some(jar) = std::env::var_os("BEATLINK_JAR") {
        let jar = PathBuf::from(jar);
        assert!(jar.exists(), "BEATLINK_JAR points at a missing file: {}", jar.display());
        return Some(jar);
    }
    let default = Path::new(BLT_JAR);
    default.exists().then(|| default.to_path_buf())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Run `CrossCheck.java` over `(kind, packet)` pairs and collect its
/// `cc <index>.<field>=<value>` lines.
fn parse_with_beatlink(jar: &Path, packets: &[(&str, &[u8])]) -> HashMap<String, String> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/beatlink/CrossCheck.java");
    let mut cmd = Command::new("java");
    cmd.arg("-cp").arg(jar).arg(source);
    for (kind, bytes) in packets {
        cmd.arg(kind).arg(hex(bytes));
    }
    let out = cmd.output().expect("running java");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "CrossCheck.java failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // beat-link logs a warning when a packet's length field disagrees with
    // its real length, or the length is not one it expects; treat that as a
    // failure too. The one exception: its expected CDJ status sizes stop at
    // 0x124, yet it only reads the CDJ-3000 loop fields from packets of at
    // least 458 bytes, so real CDJ-3000s (0x200) trigger the same one-time
    // "unexpected lengths 512" notice.
    let all = format!("{stdout}{}", String::from_utf8_lossy(&out.stderr));
    for line in all.lines() {
        let tolerated = line.contains("CDJ Status packets with unexpected lengths 512.");
        let warned = line.contains("packet with reported") || line.contains("unexpected lengths");
        assert!(!warned || tolerated, "beat-link warned:\n{all}");
    }
    stdout
        .lines()
        .filter_map(|l| l.strip_prefix("cc "))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// The jar to test against, or `None` (after saying why) when the
/// cross-check can't run here.
fn crosscheck_jar() -> Option<PathBuf> {
    let Some(jar) = beatlink_jar() else {
        eprintln!("skipping: no beat-link jar (set BEATLINK_JAR or install Beat Link Trigger)");
        return None;
    };
    if Command::new("java").arg("-version").output().is_err() {
        eprintln!("skipping: `java` not found on PATH");
        return None;
    }
    Some(jar)
}

#[test]
fn beatlink_parses_our_beat_and_mixer_packets() {
    let Some(jar) = crosscheck_jar() else { return };

    let beat = Beat {
        device_name: DeviceName::new("CDJ-3000").unwrap(),
        device_number: 2,
        bpm_hundredths: 12800,
        pitch: cdj_proto::PITCH_NEUTRAL,
        beat_within_bar: 3,
    }
    .encode();
    let mixer = DjmStatus {
        device_name: DeviceName::new("DJM-V10").unwrap(),
        device_number: 33,
        bpm_hundredths: 12450,
        master: true,
        master_handoff_to: None,
        beat_within_bar: 2,
    }
    .encode();
    let yielding = DjmStatus {
        master: false,
        master_handoff_to: Some(3),
        ..DjmStatus::idle(DeviceName::new("DJM-V10").unwrap())
    }
    .encode();

    let seen = parse_with_beatlink(
        &jar,
        &[("beat", &beat), ("mixer", &mixer), ("mixer", &yielding)],
    );
    let field = |k: &str| seen.get(k).map(String::as_str).unwrap_or("<missing>");

    // Beat: 128.00 BPM is 468.75 ms per beat; player is on beat 3, so the
    // next bar is 2 beats away and the one after that 6.
    assert_eq!(field("0.name"), "CDJ-3000");
    assert_eq!(field("0.device"), "2");
    assert_eq!(field("0.bpm"), "12800");
    assert_eq!(field("0.pitch"), "1048576");
    assert_eq!(field("0.beatWithinBar"), "3");
    assert_eq!(field("0.nextBeat"), "468");
    assert_eq!(field("0.secondBeat"), "937");
    assert_eq!(field("0.nextBar"), "937");
    assert_eq!(field("0.fourthBeat"), "1875");
    assert_eq!(field("0.secondBar"), "2812");
    assert_eq!(field("0.eighthBeat"), "3750");
    assert_eq!(field("0.effectiveTempo"), "128.0");

    assert_eq!(field("1.name"), "DJM-V10");
    assert_eq!(field("1.device"), "33");
    assert_eq!(field("1.bpm"), "12450");
    assert_eq!(field("1.pitch"), "1048576");
    assert_eq!(field("1.master"), "true");
    assert_eq!(field("1.beatWithinBar"), "2");
    assert_eq!(field("1.yieldingTo"), "null");
    assert_eq!(field("1.effectiveTempo"), "124.5");

    assert_eq!(field("2.master"), "false");
    assert_eq!(field("2.yieldingTo"), "3");
}

#[test]
fn beatlink_parses_our_cdj_status_and_position_packets() {
    let Some(jar) = crosscheck_jar() else { return };

    let name = DeviceName::new("CDJ-3000").unwrap();
    let idle = CdjStatus::idle(name.clone(), 1).encode();
    let playing_status = CdjStatus {
        active: true,
        track_source_player: 2,
        track_source_slot: TrackSourceSlot::Usb,
        track_type: TrackType::Rekordbox,
        rekordbox_id: 42,
        track_number: 7,
        usb_loaded: true,
        link_media_available: true,
        play_state1: PlayState1::Playing,
        moving: true,
        play_state3: PlayState3::ForwardCdj,
        sync_number: 3,
        playing: true,
        master: true,
        on_air: true,
        bpm_hundredths: Some(12800),
        beat_number: Some(256),
        beat_within_bar: 4,
        packet_counter: 99,
        ..CdjStatus::idle(name.clone(), 2)
    };
    let playing = playing_status.encode();
    let looping = CdjStatus {
        play_state1: PlayState1::Looping,
        active_loop: Some(ActiveLoop {
            start_ms: 65_536,
            end_ms: 131_072,
            beats: 4,
        }),
        ..playing_status
    }
    .encode();
    let position = PrecisePosition {
        device_name: name,
        device_number: 3,
        track_length_s: 245,
        playhead_ms: 61_234,
        pitch_hundredths_percent: 0,
        effective_bpm_tenths: Some(1280),
    }
    .encode();

    let seen = parse_with_beatlink(
        &jar,
        &[("cdj", &idle), ("cdj", &playing), ("cdj", &looping), ("position", &position)],
    );
    let field = |k: &str| seen.get(k).map(String::as_str).unwrap_or("<missing>");

    // An empty deck must not look like it has a track loaded.
    assert_eq!(field("0.name"), "CDJ-3000");
    assert_eq!(field("0.device"), "1");
    assert_eq!(field("0.trackLoaded"), "false");
    assert_eq!(field("0.playState1"), "NO_TRACK");
    assert_eq!(field("0.playing"), "false");
    assert_eq!(field("0.busy"), "false");
    assert_eq!(field("0.usbLoaded"), "false");
    assert_eq!(field("0.beatNumber"), "-1");
    assert_eq!(field("0.yieldingTo"), "null");

    assert_eq!(field("1.device"), "2");
    assert_eq!(field("1.busy"), "true");
    assert_eq!(field("1.sourcePlayer"), "2");
    assert_eq!(field("1.sourceSlot"), "USB_SLOT");
    assert_eq!(field("1.trackType"), "REKORDBOX");
    assert_eq!(field("1.rekordboxId"), "42");
    assert_eq!(field("1.trackNumber"), "7");
    assert_eq!(field("1.trackLoaded"), "true");
    assert_eq!(field("1.usbLoaded"), "true");
    assert_eq!(field("1.sdEmpty"), "true");
    assert_eq!(field("1.linkMedia"), "true");
    assert_eq!(field("1.playState1"), "PLAYING");
    assert_eq!(field("1.playState2"), "MOVING");
    assert_eq!(field("1.playState3"), "FORWARD_CDJ");
    assert_eq!(field("1.playing"), "true");
    assert_eq!(field("1.master"), "true");
    assert_eq!(field("1.synced"), "false");
    assert_eq!(field("1.onAir"), "true");
    assert_eq!(field("1.firmware"), "3.22");
    assert_eq!(field("1.syncNumber"), "3");
    assert_eq!(field("1.bpm"), "12800");
    assert_eq!(field("1.pitch"), "1048576");
    assert_eq!(field("1.effectiveTempo"), "128.0");
    assert_eq!(field("1.beatNumber"), "256");
    assert_eq!(field("1.cueCountdown"), "511");
    assert_eq!(field("1.beatWithinBar"), "4");
    assert_eq!(field("1.packetNumber"), "99");
    assert_eq!(field("1.canReportLooping"), "true");
    assert_eq!(field("1.looping"), "false");

    assert_eq!(field("2.playState1"), "LOOPING");
    assert_eq!(field("2.looping"), "true");
    assert_eq!(field("2.loopStart"), "65536");
    assert_eq!(field("2.loopEnd"), "131072");
    assert_eq!(field("2.loopBeats"), "4");

    assert_eq!(field("3.device"), "3");
    assert_eq!(field("3.trackLength"), "245");
    assert_eq!(field("3.position"), "61234");
    assert_eq!(field("3.pitch"), "1048576");
    assert_eq!(field("3.bpm"), "12800");
    assert_eq!(field("3.effectiveTempo"), "128.0");
}
