import java.net.DatagramPacket;
import java.net.InetAddress;
import java.util.HexFormat;

import org.deepsymmetry.beatlink.Beat;
import org.deepsymmetry.beatlink.CdjStatus;
import org.deepsymmetry.beatlink.MixerStatus;
import org.deepsymmetry.beatlink.PrecisePosition;

/**
 * Parses packets with beat-link's own classes and prints what it sees, one
 * {@code cc <index>.<field>=<value>} line per field. Driven by
 * {@code tests/beatlink_crosscheck.rs}; arguments are (kind, hex) pairs where
 * kind is {@code beat}, {@code mixer}, {@code cdj} or {@code position}.
 */
public class CrossCheck {
    public static void main(String[] args) {
        for (int i = 0; i + 1 < args.length; i += 2) {
            byte[] bytes = HexFormat.of().parseHex(args[i + 1]);
            DatagramPacket packet =
                new DatagramPacket(bytes, bytes.length, InetAddress.getLoopbackAddress(), 50001);
            String p = "cc " + (i / 2) + ".";
            switch (args[i]) {
                case "beat" -> {
                    Beat b = new Beat(packet);
                    System.out.println(p + "name=" + b.getDeviceName());
                    System.out.println(p + "device=" + b.getDeviceNumber());
                    System.out.println(p + "bpm=" + b.getBpm());
                    System.out.println(p + "pitch=" + b.getPitch());
                    System.out.println(p + "beatWithinBar=" + b.getBeatWithinBar());
                    System.out.println(p + "nextBeat=" + b.getNextBeat());
                    System.out.println(p + "secondBeat=" + b.getSecondBeat());
                    System.out.println(p + "nextBar=" + b.getNextBar());
                    System.out.println(p + "fourthBeat=" + b.getFourthBeat());
                    System.out.println(p + "secondBar=" + b.getSecondBar());
                    System.out.println(p + "eighthBeat=" + b.getEighthBeat());
                    System.out.println(p + "effectiveTempo=" + b.getEffectiveTempo());
                }
                case "mixer" -> {
                    MixerStatus m = new MixerStatus(packet);
                    System.out.println(p + "name=" + m.getDeviceName());
                    System.out.println(p + "device=" + m.getDeviceNumber());
                    System.out.println(p + "bpm=" + m.getBpm());
                    System.out.println(p + "pitch=" + m.getPitch());
                    System.out.println(p + "master=" + m.isTempoMaster());
                    System.out.println(p + "beatWithinBar=" + m.getBeatWithinBar());
                    System.out.println(p + "yieldingTo=" + m.getDeviceMasterIsBeingYieldedTo());
                    System.out.println(p + "effectiveTempo=" + m.getEffectiveTempo());
                }
                case "cdj" -> {
                    CdjStatus s = new CdjStatus(packet);
                    System.out.println(p + "name=" + s.getDeviceName());
                    System.out.println(p + "device=" + s.getDeviceNumber());
                    System.out.println(p + "busy=" + s.isBusy());
                    System.out.println(p + "sourcePlayer=" + s.getTrackSourcePlayer());
                    System.out.println(p + "sourceSlot=" + s.getTrackSourceSlot());
                    System.out.println(p + "trackType=" + s.getTrackType());
                    System.out.println(p + "rekordboxId=" + s.getRekordboxId());
                    System.out.println(p + "trackNumber=" + s.getTrackNumber());
                    System.out.println(p + "trackLoaded=" + s.isTrackLoaded());
                    System.out.println(p + "usbLoaded=" + s.isLocalUsbLoaded());
                    System.out.println(p + "sdEmpty=" + s.isLocalSdEmpty());
                    System.out.println(p + "linkMedia=" + s.isLinkMediaAvailable());
                    System.out.println(p + "playState1=" + s.getPlayState1());
                    System.out.println(p + "playState2=" + s.getPlayState2());
                    System.out.println(p + "playState3=" + s.getPlayState3());
                    System.out.println(p + "playing=" + s.isPlaying());
                    System.out.println(p + "master=" + s.isTempoMaster());
                    System.out.println(p + "synced=" + s.isSynced());
                    System.out.println(p + "onAir=" + s.isOnAir());
                    System.out.println(p + "firmware=" + s.getFirmwareVersion());
                    System.out.println(p + "syncNumber=" + s.getSyncNumber());
                    System.out.println(p + "bpm=" + s.getBpm());
                    System.out.println(p + "pitch=" + s.getPitch());
                    System.out.println(p + "effectiveTempo=" + s.getEffectiveTempo());
                    System.out.println(p + "yieldingTo=" + s.getDeviceMasterIsBeingYieldedTo());
                    System.out.println(p + "beatNumber=" + s.getBeatNumber());
                    System.out.println(p + "cueCountdown=" + s.getCueCountdown());
                    System.out.println(p + "beatWithinBar=" + s.getBeatWithinBar());
                    System.out.println(p + "packetNumber=" + s.getPacketNumber());
                    System.out.println(p + "canReportLooping=" + s.canReportLooping());
                    System.out.println(p + "looping=" + s.isLooping());
                    System.out.println(p + "loopStart=" + s.getLoopStart());
                    System.out.println(p + "loopEnd=" + s.getLoopEnd());
                    System.out.println(p + "loopBeats=" + s.getActiveLoopBeats());
                }
                case "position" -> {
                    PrecisePosition pp = new PrecisePosition(packet);
                    System.out.println(p + "name=" + pp.getDeviceName());
                    System.out.println(p + "device=" + pp.getDeviceNumber());
                    System.out.println(p + "trackLength=" + pp.getTrackLength());
                    System.out.println(p + "position=" + pp.getPlaybackPosition());
                    System.out.println(p + "pitch=" + pp.getPitch());
                    System.out.println(p + "bpm=" + pp.getBpm());
                    System.out.println(p + "effectiveTempo=" + pp.getEffectiveTempo());
                }
                default -> throw new IllegalArgumentException("unknown packet kind " + args[i]);
            }
        }
    }
}
