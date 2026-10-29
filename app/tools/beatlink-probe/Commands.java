import java.util.Collections;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.ConcurrentHashMap;
import java.util.function.BooleanSupplier;

import org.deepsymmetry.beatlink.BeatFinder;
import org.deepsymmetry.beatlink.CdjStatus;
import org.deepsymmetry.beatlink.DeviceFinder;
import org.deepsymmetry.beatlink.MediaDetails;
import org.deepsymmetry.beatlink.VirtualCdj;
import org.deepsymmetry.beatlink.data.SlotReference;

/**
 * Sends the emulator's players the commands Beat Link Trigger can send,
 * with beat-link itself, and checks each one took effect in the players'
 * status. Run with Beat Link Trigger closed, the network set up, and a rig
 * of three players with the demo export, e.g.:
 *
 *   cdjd run-fleet --players 3 --silent --library app/vendor/rekordcrate/data/complete_export/demo_tracks
 *   java -cp "/Applications/Beat Link Trigger.app/Contents/app/beat-link-trigger.jar" \
 *       app/tools/beatlink-probe/Commands.java
 *
 * The probe joins as player 4 and sends status, as Beat Link Trigger does
 * when it has to take part in tempo-master handoffs.
 */
public class Commands {
    static final Map<Integer, CdjStatus> players = new ConcurrentHashMap<>();
    static int failures = 0;

    static boolean waitFor(BooleanSupplier condition) throws InterruptedException {
        for (int i = 0; i < 40; i++) {
            if (condition.getAsBoolean()) return true;
            Thread.sleep(100);
        }
        return false;
    }

    static void check(String what, BooleanSupplier condition) throws InterruptedException {
        boolean ok = waitFor(condition);
        if (!ok) failures++;
        System.out.printf("%s  %s%n", ok ? "PASS" : "FAIL", what);
    }

    static CdjStatus player(int n) {
        return players.get(n);
    }

    public static void main(String[] args) throws Exception {
        DeviceFinder.getInstance().start();
        BeatFinder.getInstance().start();
        VirtualCdj vcdj = VirtualCdj.getInstance();
        vcdj.addUpdateListener(update -> {
            if (update instanceof CdjStatus s) players.put(s.getDeviceNumber(), s);
        });
        Map<String, MediaDetails> media = new ConcurrentHashMap<>();
        vcdj.addMediaDetailsListener(d -> media.put(d.slotReference.toString(), d));
        if (!vcdj.start((byte) 4)) {
            System.out.println("FAIL  VirtualCdj could not go online (is the rig running?)");
            System.exit(1);
        }
        vcdj.setSendingStatus(true);
        check("players 1-3 report status", () -> player(1) != null && player(2) != null && player(3) != null);
        System.out.println("      beat-link is " + vcdj.getLocalAddress() + " as device " + vcdj.getDeviceNumber());

        SlotReference usb = SlotReference.getSlotReference(1, CdjStatus.TrackSourceSlot.USB_SLOT);
        vcdj.sendMediaQuery(usb);
        check("media query: player 1 describes its USB", () -> media.containsKey(usb.toString()));
        MediaDetails details = media.get(usb.toString());
        if (details != null) {
            System.out.printf("      \"%s\" created %s, %d tracks, %d playlists, %s, %d of %d MB free%n",
                details.name, details.creationDate, details.trackCount, details.playlistCount, details.mediaType,
                details.freeSpace >> 20, details.totalSize >> 20);
        }

        // The demo export has two tracks: players 1 and 2 have one each.
        int track = player(1).getRekordboxId();
        vcdj.sendLoadTrackCommand(2, track, 1, CdjStatus.TrackSourceSlot.USB_SLOT, CdjStatus.TrackType.REKORDBOX);
        check("load track: player 2 loads track " + track + " from player 1's USB",
            () -> player(2).getRekordboxId() == track && player(2).getTrackSourcePlayer() == 1);

        vcdj.sendSyncModeCommand(2, true);
        check("sync on: player 2 syncs", () -> player(2).isSynced());
        vcdj.sendSyncModeCommand(2, false);
        check("sync off: player 2 stops syncing", () -> !player(2).isSynced());

        vcdj.appointTempoMaster(3);
        check("become master: player 3 is tempo master", () -> player(3).isTempoMaster() && !player(1).isTempoMaster());

        vcdj.becomeTempoMaster();
        check("handoff to beat-link: player 3 yields, beat-link is master",
            () -> vcdj.isTempoMaster() && !player(3).isTempoMaster());

        vcdj.appointTempoMaster(1);
        check("handoff back: player 1 asks beat-link and takes the role",
            () -> player(1).isTempoMaster() && !vcdj.isTempoMaster());

        vcdj.sendFaderStartCommand(Set.of(2), Collections.emptySet());
        check("fader start: player 2 plays", () -> player(2).isPlaying());
        vcdj.sendFaderStartCommand(Collections.emptySet(), Set.of(2));
        check("fader stop: player 2 stops at its cue", () -> !player(2).isPlaying() && player(2).getPlayState1() == CdjStatus.PlayState1.CUED);

        System.out.println(failures == 0 ? "== all commands worked" : "== " + failures + " failed");
        System.exit(failures == 0 ? 0 : 1);
    }
}
