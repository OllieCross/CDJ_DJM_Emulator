import java.util.Map;
import java.util.Set;
import java.util.TreeMap;
import java.util.TreeSet;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.atomic.AtomicInteger;

import org.deepsymmetry.beatlink.BeatFinder;
import org.deepsymmetry.beatlink.CdjStatus;
import org.deepsymmetry.beatlink.DeviceAnnouncement;
import org.deepsymmetry.beatlink.DeviceFinder;
import org.deepsymmetry.beatlink.MixerStatus;
import org.deepsymmetry.beatlink.VirtualCdj;
import org.deepsymmetry.beatlink.data.BeatGrid;
import org.deepsymmetry.beatlink.data.BeatGridFinder;
import org.deepsymmetry.beatlink.data.MetadataFinder;
import org.deepsymmetry.beatlink.data.TrackMetadata;
import org.deepsymmetry.beatlink.data.WaveformDetail;
import org.deepsymmetry.beatlink.data.WaveformFinder;
import org.deepsymmetry.beatlink.data.WaveformPreview;

/**
 * Headless stand-in for Beat Link Trigger: goes online with beat-link,
 * listens for a few seconds and prints what it sees from the emulator.
 *
 * Run with Beat Link Trigger closed (both need ports 50000-50002):
 *
 *   java -cp "/Applications/Beat Link Trigger.app/Contents/app/beat-link-trigger.jar" \
 *       app/tools/beatlink-probe/Probe.java [seconds]
 */
public class Probe {
    public static void main(String[] args) throws Exception {
        int seconds = args.length > 0 ? Integer.parseInt(args[0]) : 10;

        Map<Integer, CdjStatus> players = new ConcurrentHashMap<>();
        Map<Integer, MixerStatus> mixers = new ConcurrentHashMap<>();
        Map<Integer, AtomicInteger> beats = new ConcurrentHashMap<>();
        Map<Integer, AtomicInteger> positions = new ConcurrentHashMap<>();
        Map<Integer, String> lastPosition = new ConcurrentHashMap<>();
        Set<Integer> onAir = ConcurrentHashMap.newKeySet();

        DeviceFinder.getInstance().start();
        BeatFinder beatFinder = BeatFinder.getInstance();
        beatFinder.addBeatListener(b -> beats.computeIfAbsent(b.getDeviceNumber(), k -> new AtomicInteger()).incrementAndGet());
        beatFinder.addPrecisePositionListener(p -> {
            positions.computeIfAbsent(p.getDeviceNumber(), k -> new AtomicInteger()).incrementAndGet();
            lastPosition.put(p.getDeviceNumber(), p.getPlaybackPosition() + " ms of " + p.getTrackLength() + " s");
        });
        beatFinder.addOnAirListener(channels -> {
            onAir.clear();
            onAir.addAll(channels);
        });
        beatFinder.start();

        VirtualCdj vcdj = VirtualCdj.getInstance();
        vcdj.addUpdateListener(update -> {
            if (update instanceof CdjStatus s) players.put(s.getDeviceNumber(), s);
            if (update instanceof MixerStatus m) mixers.put(m.getDeviceNumber(), m);
        });
        if (!vcdj.start()) {
            System.out.println("VirtualCdj could not go online (no devices found?)");
        }
        // What Beat Link Trigger's player windows use: track metadata, beat
        // grids and waveforms fetched from each player's dbserver.
        MetadataFinder.getInstance().start();
        BeatGridFinder.getInstance().start();
        WaveformFinder.getInstance().start();
        Thread.sleep(seconds * 1000L);

        System.out.println("== beat-link chose " + vcdj.getLocalAddress() + " (device " + vcdj.getDeviceNumber() + ")");
        System.out.println("== devices announcing:");
        for (DeviceAnnouncement d : DeviceFinder.getInstance().getCurrentDevices()) {
            System.out.printf("   %2d %-10s %s peers=%d%n", d.getDeviceNumber(), d.getDeviceName(), d.getAddress().getHostAddress(), d.getPeerCount());
        }
        System.out.println("== player status:");
        for (CdjStatus s : new TreeMap<>(players).values()) {
            System.out.printf("   %d %-10s %s track=%d slot=%s bpm=%.2f beat=%d bar=%d playing=%b master=%b onAir=%b%n",
                s.getDeviceNumber(), s.getDeviceName(), s.getPlayState1(), s.getRekordboxId(), s.getTrackSourceSlot(),
                s.getBpm() / 100.0, s.getBeatNumber(), s.getBeatWithinBar(), s.isPlaying(), s.isTempoMaster(), s.isOnAir());
        }
        System.out.println("== track data fetched from the players:");
        for (int n = 1; n <= 4; n++) {
            TrackMetadata md = MetadataFinder.getInstance().getLatestMetadataFor(n);
            if (md == null) {
                System.out.printf("   %d (none)%n", n);
                continue;
            }
            BeatGrid grid = BeatGridFinder.getInstance().getLatestBeatGridFor(n);
            WaveformPreview preview = WaveformFinder.getInstance().getLatestPreviewFor(n);
            WaveformDetail detail = WaveformFinder.getInstance().getLatestDetailFor(n);
            System.out.printf("   %d \"%s\" by %s, %d s, %.2f BPM, key %s, %d cues, grid %s beats, preview %s, detail %s%n",
                n, md.getTitle(), md.getArtist() == null ? "?" : md.getArtist().label, md.getDuration(), md.getTempo() / 100.0,
                md.getKey() == null ? "?" : md.getKey().label, md.getCueList() == null ? 0 : md.getCueList().entries.size(),
                grid == null ? "-" : grid.beatCount, preview == null ? "-" : preview.segmentCount + " segments",
                detail == null ? "-" : detail.getFrameCount() + " frames");
        }
        System.out.println("== mixer status:");
        for (MixerStatus m : mixers.values()) {
            System.out.printf("   %d %-10s bpm=%.2f bar=%d master=%b%n",
                m.getDeviceNumber(), m.getDeviceName(), m.getBpm() / 100.0, m.getBeatWithinBar(), m.isTempoMaster());
        }
        System.out.println("== beats per device over " + seconds + " s: " + new TreeMap<>(beats));
        System.out.println("== absolute-position packets: " + new TreeMap<>(positions) + " last=" + new TreeMap<>(lastPosition));
        System.out.println("== channels on air: " + new TreeSet<>(onAir));
        System.exit(0);
    }
}
