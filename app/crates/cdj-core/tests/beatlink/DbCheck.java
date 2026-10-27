import java.lang.reflect.Constructor;
import java.net.Socket;
import java.util.List;

import org.deepsymmetry.beatlink.CdjStatus;
import org.deepsymmetry.beatlink.data.BeatGrid;
import org.deepsymmetry.beatlink.data.CueList;
import org.deepsymmetry.beatlink.data.DataReference;
import org.deepsymmetry.beatlink.data.TrackMetadata;
import org.deepsymmetry.beatlink.data.WaveformDetail;
import org.deepsymmetry.beatlink.data.WaveformFinder;
import org.deepsymmetry.beatlink.data.WaveformPreview;
import org.deepsymmetry.beatlink.dbserver.Client;
import org.deepsymmetry.beatlink.dbserver.Message;
import org.deepsymmetry.beatlink.dbserver.NumberField;
import org.deepsymmetry.beatlink.dbserver.StringField;

/**
 * Talks to an emulator dbserver with beat-link's own client and parsers, the
 * way MetadataFinder, BeatGridFinder and WaveformFinder do, and prints what
 * beat-link makes of the replies as {@code cc <field>=<value>} lines.
 *
 * Arguments: port, player number, rekordbox ID (in the player's USB slot).
 */
public class DbCheck {
    static final CdjStatus.TrackSourceSlot USB = CdjStatus.TrackSourceSlot.USB_SLOT;
    static final CdjStatus.TrackType REKORDBOX = CdjStatus.TrackType.REKORDBOX;

    static void out(String key, Object value) {
        System.out.println("cc " + key + "=" + value);
    }

    @SuppressWarnings("unchecked")
    static <T> T construct(Class<T> type, Class<?>[] params, Object... args) throws Exception {
        Constructor<T> c = type.getDeclaredConstructor(params);
        c.setAccessible(true);
        return c.newInstance(args);
    }

    static List<String> labels(Client client, Message.KnownType request, NumberField... args) throws Exception {
        Message available = client.menuRequestTyped(request, Message.MenuIdentifier.MAIN_MENU, USB, REKORDBOX, args);
        List<Message> items = client.renderMenuItems(Message.MenuIdentifier.MAIN_MENU, USB, REKORDBOX, available);
        return items.stream().map(m -> ((StringField) m.arguments.get(3)).getValue()).toList();
    }

    public static void main(String[] a) throws Exception {
        int port = Integer.parseInt(a[0]);
        int player = Integer.parseInt(a[1]);
        int id = Integer.parseInt(a[2]);

        Client client = construct(Client.class, new Class<?>[] {Socket.class, int.class, int.class},
            new Socket("127.0.0.1", port), player, 5);
        if (!client.tryLockingForMenuOperations(5, java.util.concurrent.TimeUnit.SECONDS)) {
            throw new IllegalStateException("could not lock the client for menu operations");
        }
        DataReference ref = new DataReference(player, USB, id, REKORDBOX);
        NumberField track = new NumberField(id);

        Message cueReply = client.simpleRequest(Message.KnownType.CUE_LIST_EXT_REQ, null,
            client.buildRMST(Message.MenuIdentifier.DATA, USB), track, NumberField.WORD_0);
        out("cueReply", cueReply.knownType);
        CueList cues = new CueList(cueReply);
        out("cues", cues.entries.size());

        Message available = client.menuRequestTyped(Message.KnownType.REKORDBOX_METADATA_REQ,
            Message.MenuIdentifier.MAIN_MENU, USB, REKORDBOX, track);
        List<Message> items = client.renderMenuItems(Message.MenuIdentifier.MAIN_MENU, USB, REKORDBOX, available);
        TrackMetadata md = construct(TrackMetadata.class,
            new Class<?>[] {DataReference.class, CdjStatus.TrackType.class, List.class, CueList.class},
            ref, REKORDBOX, items, cues);
        out("title", md.getTitle());
        out("artist", md.getArtist() == null ? null : md.getArtist().label);
        out("album", md.getAlbum() == null ? null : md.getAlbum().label);
        out("genre", md.getGenre() == null ? null : md.getGenre().label);
        out("duration", md.getDuration());
        out("tempo", md.getTempo());
        out("rating", md.getRating());
        out("key", md.getKey() == null ? null : md.getKey().label);
        out("artworkId", md.getArtworkId());

        Message grid = client.simpleRequest(Message.KnownType.BEAT_GRID_REQ, null,
            client.buildRMST(Message.MenuIdentifier.DATA, USB), track);
        BeatGrid beatGrid = new BeatGrid(ref, grid);
        out("beats", beatGrid.beatCount);
        out("firstBeatMs", beatGrid.getTimeWithinTrack(1));
        out("firstBeatInBar", beatGrid.getBeatWithinBar(1));

        Message preview = client.simpleRequest(Message.KnownType.WAVE_PREVIEW_REQ, null,
            client.buildRMST(Message.MenuIdentifier.DATA, USB), NumberField.WORD_1, track, NumberField.WORD_0);
        WaveformPreview wp = construct(WaveformPreview.class,
            new Class<?>[] {DataReference.class, Message.class, WaveformFinder.WaveformStyle.class},
            ref, preview, WaveformFinder.WaveformStyle.BLUE);
        out("previewSegments", wp.segmentCount);

        Message detail = client.simpleRequest(Message.KnownType.WAVE_DETAIL_REQ, null,
            client.buildRMST(Message.MenuIdentifier.MAIN_MENU, USB), track, NumberField.WORD_0);
        WaveformDetail wd = new WaveformDetail(ref, detail, WaveformFinder.WaveformStyle.BLUE);
        out("detailFrames", wd.getFrameCount());

        Message missing = client.simpleRequest(Message.KnownType.BEAT_GRID_REQ, null,
            client.buildRMST(Message.MenuIdentifier.DATA, USB), new NumberField(999999));
        out("missingReply", missing.knownType);

        out("tracks", labels(client, Message.KnownType.TRACK_MENU_REQ, NumberField.WORD_0));
        out("root", labels(client, Message.KnownType.ROOT_MENU_REQ, NumberField.WORD_0, new NumberField(0xffffff)));
        System.exit(0);
    }
}
