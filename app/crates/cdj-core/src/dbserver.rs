//! Dbserver (TCP 1051, port discovery on TCP 12523) for one virtual CDJ.
//!
//! Clients (Beat Link Trigger, ShowKontrol, other players) ask the player
//! whose media holds a track for its metadata, artwork, beat grid, cues and
//! waveforms, and can browse that media's menus. Every request names the
//! media slot and rekordbox ID it wants, and this server answers for the
//! export in that slot of its own player. Missing data gets an `UNAVAILABLE`
//! reply, as real players send.
//!
//! The wire codec lives in `cdj_proto::dbserver`; request and response
//! layouts follow beat-link's `Message.KnownType`.

use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Arc;

use cdj_proto::dbserver::{self as proto, port_discovery, Field, Message, GREETING, HEADER_LEN};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, info, trace, warn};

use crate::library::{AnlzFile, Cue, Library, TrackInfo};
use crate::media::{PlayerMedia, Slot};

/// TCP port the dbserver listens on (advertised via the 12523 handshake).
pub const DBSERVER_TCP_PORT: u16 = 1051;

/// Port clients use to discover the dbserver TCP port.
pub const PORT_DISCOVERY_PORT: u16 = 12523;

/// Bytes before the data in a blue waveform-detail reply; beat-link skips them.
const WAVE_DETAIL_LEADING_BYTES: usize = 19;

/// Configuration for one virtual CDJ's dbserver.
#[derive(Debug, Clone)]
pub struct DbServerConfig {
    pub device_number: u8,
    pub ip: Ipv4Addr,
    /// Usually [`DBSERVER_TCP_PORT`]; 0 picks a free port.
    pub tcp_port: u16,
    pub player_model: String,
}

pub struct DbServer {
    cfg: DbServerConfig,
    media: Arc<PlayerMedia>,
}

impl DbServer {
    pub fn new(cfg: DbServerConfig, media: Arc<PlayerMedia>) -> Self {
        Self { cfg, media }
    }

    /// Listen for port discovery and dbserver connections until either fails.
    pub async fn run(self) -> anyhow::Result<()> {
        let listener = TcpListener::bind(SocketAddrV4::new(self.cfg.ip, self.cfg.tcp_port)).await?;
        let port = listener.local_addr()?.port();
        let discovery = run_port_discovery(self.cfg.ip, port, self.cfg.device_number);
        tokio::select! {
            r = discovery => r,
            r = self.serve(listener) => r,
        }
    }

    /// Serve dbserver connections on an already-bound listener.
    pub async fn serve(self, listener: TcpListener) -> anyhow::Result<()> {
        info!(num = self.cfg.device_number, addr = %listener.local_addr()?, "dbserver tcp listening");
        loop {
            let (stream, peer) = listener.accept().await?;
            debug!(num = self.cfg.device_number, %peer, "dbserver client connected");
            let session = Session::new(self.cfg.device_number, self.media.clone());
            tokio::spawn(async move {
                if let Err(e) = handle_connection(stream, session).await {
                    debug!(%peer, "dbserver client: {e}");
                }
            });
        }
    }
}

/// TCP port-discovery listener. Clients open a connection to port 12523,
/// send the 19-byte "RemoteDBServer" query and read a 2-byte port number.
async fn run_port_discovery(ip: Ipv4Addr, tcp_port: u16, device_number: u8) -> anyhow::Result<()> {
    let listener = TcpListener::bind(SocketAddrV4::new(ip, PORT_DISCOVERY_PORT)).await?;
    info!(num = device_number, addr = %listener.local_addr()?, "dbserver port-discovery listening");
    let reply = port_discovery::reply(tcp_port);
    loop {
        let (mut stream, peer) = listener.accept().await?;
        tokio::spawn(async move {
            let mut buf = [0u8; 19];
            match stream.read_exact(&mut buf).await {
                Ok(_) if port_discovery::is_query(&buf) => {
                    if let Err(e) = stream.write_all(&reply).await {
                        warn!(num = device_number, %peer, "port-discovery reply failed: {e}");
                    }
                }
                Ok(_) => trace!(num = device_number, %peer, bytes = ?buf, "port-discovery: unknown query"),
                Err(e) => debug!(num = device_number, %peer, "port-discovery read failed: {e}"),
            }
        });
    }
}

async fn handle_connection(mut stream: TcpStream, mut session: Session) -> anyhow::Result<()> {
    // Both sides send the greeting (a 4-byte number field holding 1).
    stream.write_all(&GREETING).await?;
    let mut echo = [0u8; 5];
    stream.read_exact(&mut echo).await?;
    if echo != GREETING {
        warn!(got = ?echo, "unexpected client greeting, proceeding anyway");
    }

    let mut buf: Vec<u8> = Vec::with_capacity(512);
    let mut read_buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut read_buf).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&read_buf[..n]);

        while buf.len() >= HEADER_LEN {
            let (msg, consumed) = match Message::decode(&buf) {
                Ok(decoded) => decoded,
                Err(cdj_proto::DecodeError::TooShort { .. }) => break,
                Err(e) => {
                    warn!(err = %e, head = ?&buf[..buf.len().min(32)], "dbserver decode failed, dropping connection");
                    return Ok(());
                }
            };
            buf.drain(..consumed);
            trace!(txid = msg.transaction_id, msg_type = format!("{:#06x}", msg.message_type), "dbserver rx");
            if msg.message_type == proto::MSG_TEARDOWN_REQ {
                return Ok(());
            }
            for reply in session.respond(&msg) {
                stream.write_all(&reply.encode()).await?;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Request handling
// ---------------------------------------------------------------------------

/// One row of a menu, as sent in a MENU_ITEM message.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MenuItem {
    /// e.g. the artist of a track row.
    parent_id: u32,
    id: u32,
    label1: String,
    label2: String,
    item_type: u32,
    artwork_id: u32,
    /// Position in a playlist (1-based), else 0.
    position: u32,
}

impl MenuItem {
    fn new(item_type: u32, id: u32, label1: impl Into<String>) -> Self {
        Self {
            parent_id: 0,
            id,
            label1: label1.into(),
            label2: String::new(),
            item_type,
            artwork_id: 0,
            position: 0,
        }
    }

    fn track_row(t: &TrackInfo, position: u32) -> Self {
        Self {
            parent_id: t.artist_id,
            label2: t.artist.clone(),
            artwork_id: t.artwork_id,
            position,
            ..Self::new(proto::ITEM_TRACK_TITLE_AND_ARTIST, t.id, t.title.clone())
        }
    }

    fn to_message(&self, txid: u32) -> Message {
        // Byte size of a label as sent: UTF-16 with a NUL terminator.
        let size = |s: &str| ((s.encode_utf16().count() + 1) * 2) as u32;
        Message::new(
            txid,
            proto::MSG_MENU_ITEM,
            vec![
                Field::Number4(self.parent_id),
                Field::Number4(self.id),
                Field::Number4(size(&self.label1)),
                Field::String(self.label1.clone()),
                Field::Number4(size(&self.label2)),
                Field::String(self.label2.clone()),
                Field::Number4(self.item_type),
                Field::Number4(0),
                Field::Number4(self.artwork_id),
                Field::Number4(self.position),
                Field::Number4(0),
                Field::Number4(0),
            ],
        )
    }
}

/// The `r:m:s:t` argument every request starts with.
#[derive(Debug, Clone, Copy)]
struct Rmst {
    slot: u8,
}

impl Rmst {
    fn of(msg: &Message) -> Option<Self> {
        msg.number(0).map(|v| Self {
            slot: (v >> 8) as u8,
        })
    }
}

/// One client connection: remembers the last menu so RENDER_MENU_REQ can
/// page through it.
struct Session {
    device_number: u8,
    media: Arc<PlayerMedia>,
    menu: Vec<MenuItem>,
}

impl Session {
    fn new(device_number: u8, media: Arc<PlayerMedia>) -> Self {
        Self {
            device_number,
            media,
            menu: Vec::new(),
        }
    }

    /// The export in the slot a request names, if any.
    fn library(&self, msg: &Message) -> Option<Arc<Library>> {
        let slot = Slot::from_protocol(Rmst::of(msg)?.slot)?;
        Some(self.media.get(slot)?.library.clone())
    }

    /// The export and track a request names by rekordbox ID in argument `arg`.
    fn track(&self, msg: &Message, arg: usize) -> Option<(Arc<Library>, TrackInfo)> {
        let library = self.library(msg)?;
        let track = library.track_by_id(msg.number(arg)?)?.clone();
        Some((library, track))
    }

    /// The replies to one request.
    fn respond(&mut self, msg: &Message) -> Vec<Message> {
        let txid = msg.transaction_id;
        let request = msg.message_type;
        let unavailable = || vec![Message::new(txid, proto::MSG_UNAVAILABLE, vec![Field::Number4(request.into())])];
        // Reply carrying one blob, as the data responses do.
        let data = |resp_type: u16, bytes: Vec<u8>, trailer: Option<u32>| {
            let mut args = vec![
                Field::Number4(request.into()),
                Field::Number4(0),
                Field::Number4(bytes.len() as u32),
                Field::Binary(bytes),
            ];
            args.extend(trailer.map(Field::Number4));
            vec![Message::new(txid, resp_type, args)]
        };

        match request {
            proto::MSG_SETUP_REQ => vec![Message::new(
                txid,
                proto::MSG_MENU_AVAILABLE,
                vec![Field::Number4(0), Field::Number4(self.device_number.into())],
            )],

            proto::MSG_RENDER_MENU_REQ => {
                let offset = msg.number(1).unwrap_or(0) as usize;
                let limit = msg.number(2).unwrap_or(0) as usize;
                let mut replies = vec![Message::new(txid, proto::MSG_MENU_HEADER, vec![])];
                replies.extend(
                    self.menu
                        .iter()
                        .skip(offset)
                        .take(limit)
                        .map(|item| item.to_message(txid)),
                );
                replies.push(Message::new(txid, proto::MSG_MENU_FOOTER, vec![]));
                replies
            }

            proto::MSG_ARTWORK_REQ => {
                let jpeg = self
                    .library(msg)
                    .zip(msg.number(1))
                    .and_then(|(lib, id)| lib.artwork_jpeg(id));
                match jpeg {
                    Some(bytes) => data(proto::MSG_ARTWORK_RESP, bytes, None),
                    None => unavailable(),
                }
            }

            proto::MSG_WAVEFORM_PREVIEW_REQ => {
                match self.track(msg, 2).and_then(|(lib, t)| lib.waveform_preview_for(&t).ok()) {
                    Some(bytes) => data(proto::MSG_WAVE_PREVIEW, bytes, None),
                    None => unavailable(),
                }
            }

            proto::MSG_WAVEFORM_DETAIL_REQ => {
                match self.track(msg, 1).and_then(|(lib, t)| lib.waveform_detail_for(&t).ok()) {
                    Some(bytes) => {
                        let mut blob = vec![0u8; WAVE_DETAIL_LEADING_BYTES];
                        blob.extend(bytes);
                        data(proto::MSG_WAVE_DETAIL_RESP, blob, None)
                    }
                    None => unavailable(),
                }
            }

            proto::MSG_BEAT_GRID_REQ => {
                match self.track(msg, 1).and_then(|(lib, t)| lib.beat_grid_for(&t).ok()) {
                    Some(bytes) => data(proto::MSG_BEAT_GRID_RESP, bytes, Some(0)),
                    None => unavailable(),
                }
            }

            proto::MSG_CUE_LIST_EXT_REQ => {
                match self.track(msg, 1).and_then(|(lib, t)| lib.cues_for(&t).ok()) {
                    Some(cues) => {
                        let count = cues.len() as u32;
                        data(proto::MSG_CUE_LIST_EXT, encode_extended_cues(&cues), Some(count))
                    }
                    None => unavailable(),
                }
            }

            proto::MSG_ANLZ_TAG_REQ => {
                let tag = self.track(msg, 1).and_then(|(lib, t)| {
                    let fourcc = msg.number(2)?.to_le_bytes();
                    let ext = msg.number(3)?.to_le_bytes();
                    let ext = std::str::from_utf8(&ext).ok()?.trim_end_matches('\0');
                    lib.anlz_tag(&t, AnlzFile::from_extension(ext)?, fourcc).ok().flatten()
                });
                match tag {
                    Some(section) => {
                        // Four bytes the client skips, then the raw tag.
                        let mut blob = vec![0u8; 4];
                        blob.extend(section);
                        data(proto::MSG_ANLZ_TAG, blob, Some(1))
                    }
                    None => unavailable(),
                }
            }

            // Clients that get the extended list never need this one.
            proto::MSG_CUE_LIST_REQ => unavailable(),

            _ => match self.menu_for(msg) {
                Some(items) => {
                    let count = items.len() as u32;
                    self.menu = items;
                    vec![Message::new(
                        txid,
                        proto::MSG_MENU_AVAILABLE,
                        vec![Field::Number4(request.into()), Field::Number4(count)],
                    )]
                }
                None => unavailable(),
            },
        }
    }

    /// Items of the menu a request asks for; `None` if this server can't
    /// answer it (no media in the slot, unknown request).
    fn menu_for(&self, msg: &Message) -> Option<Vec<MenuItem>> {
        let library = self.library(msg)?;
        let lib = library.as_ref();
        let tracks = |filter: &dyn Fn(&TrackInfo) -> bool| {
            let mut rows: Vec<&TrackInfo> = lib.tracks.iter().filter(|t| filter(t)).collect();
            rows.sort_by_cached_key(|t| t.title.to_lowercase());
            rows.into_iter().map(|t| MenuItem::track_row(t, 0)).collect::<Vec<_>>()
        };
        let named = |item_type: u32, names: &std::collections::BTreeMap<u32, String>, used: &dyn Fn(&TrackInfo) -> u32| {
            let mut rows: Vec<MenuItem> = names
                .iter()
                .filter(|(id, _)| lib.tracks.iter().any(|t| used(t) == **id))
                .map(|(&id, name)| MenuItem::new(item_type, id, name.clone()))
                .collect();
            rows.sort_by_cached_key(|i| i.label1.to_lowercase());
            rows
        };

        let items = match msg.message_type {
            proto::MSG_REKORDBOX_METADATA_REQ => metadata_items(lib.track_by_id(msg.number(1)?)?),
            proto::MSG_ROOT_MENU_REQ => [
                (proto::ITEM_PLAYLIST_MENU, "PLAYLIST"),
                (proto::ITEM_TRACK_MENU, "TRACK"),
                (proto::ITEM_ARTIST_MENU, "ARTIST"),
                (proto::ITEM_ALBUM_MENU, "ALBUM"),
                (proto::ITEM_GENRE_MENU, "GENRE"),
                (proto::ITEM_SEARCH_MENU, "SEARCH"),
            ]
            .into_iter()
            // Players wrap menu names in U+FFFA / U+FFFB.
            .map(|(item_type, name)| MenuItem::new(item_type, item_type, format!("\u{fffa}{name}\u{fffb}")))
            .collect(),
            proto::MSG_TRACK_MENU_REQ => tracks(&|_| true),
            proto::MSG_ARTIST_MENU_REQ => named(proto::ITEM_ARTIST, &lib.artists, &|t| t.artist_id),
            proto::MSG_ALBUM_MENU_REQ => named(proto::ITEM_ALBUM, &lib.albums, &|t| t.album_id),
            proto::MSG_GENRE_MENU_REQ => named(proto::ITEM_GENRE, &lib.genres, &|t| t.genre_id),
            proto::MSG_ALBUM_MENU_FOR_ARTIST_REQ => {
                let artist = msg.number(2)?;
                named(proto::ITEM_ALBUM, &lib.albums, &|t| {
                    if t.artist_id == artist { t.album_id } else { u32::MAX }
                })
            }
            proto::MSG_TRACK_MENU_FOR_ALBUM_REQ => {
                let album = msg.number(2)?;
                tracks(&|t| t.album_id == album)
            }
            proto::MSG_TRACK_MENU_FOR_ARTIST_AND_ALBUM_REQ => {
                let (artist, album) = (msg.number(2)?, msg.number(3)?);
                tracks(&|t| t.artist_id == artist && (album == u32::MAX || t.album_id == album))
            }
            proto::MSG_SEARCH_MENU_REQ => {
                let needle = msg.string(3)?.to_lowercase();
                tracks(&|t| t.title.to_lowercase().contains(&needle) || t.artist.to_lowercase().contains(&needle))
            }
            proto::MSG_PLAYLIST_REQ => {
                let (id, is_folder) = (msg.number(2)?, msg.number(3)? == 1);
                if is_folder {
                    lib.playlist_children(id)
                        .into_iter()
                        .map(|p| MenuItem {
                            parent_id: p.parent_id,
                            ..MenuItem::new(
                                if p.is_folder { proto::ITEM_FOLDER } else { proto::ITEM_PLAYLIST },
                                p.id,
                                p.name.clone(),
                            )
                        })
                        .collect()
                } else {
                    lib.playlist_tracks(id)
                        .iter()
                        .filter_map(|&tid| lib.track_by_id(tid))
                        .enumerate()
                        .map(|(i, t)| MenuItem::track_row(t, i as u32 + 1))
                        .collect()
                }
            }
            // Other menus (BPM, key, history...) exist on real players but
            // are empty here.
            other if (0x1000..=0x1fff).contains(&other) => Vec::new(),
            _ => return None,
        };
        Some(items)
    }
}

/// The rows of a track-metadata menu, which clients read by item type.
fn metadata_items(t: &TrackInfo) -> Vec<MenuItem> {
    vec![
        MenuItem {
            parent_id: t.artist_id,
            artwork_id: t.artwork_id,
            ..MenuItem::new(proto::ITEM_TITLE, t.id, t.title.clone())
        },
        MenuItem::new(proto::ITEM_ARTIST, t.artist_id, t.artist.clone()),
        MenuItem::new(proto::ITEM_ALBUM, t.album_id, t.album.clone()),
        MenuItem::new(proto::ITEM_DURATION, t.duration_s, ""),
        MenuItem::new(proto::ITEM_TEMPO, t.bpm_hundredths.into(), ""),
        MenuItem::new(proto::ITEM_COMMENT, 0, t.comment.clone()),
        MenuItem::new(proto::ITEM_KEY, 0, t.key.clone()),
        MenuItem::new(proto::ITEM_RATING, t.rating.into(), ""),
        MenuItem::new(proto::ITEM_COLOR_NONE + u32::from(t.color.min(8)), 0, ""),
        MenuItem::new(proto::ITEM_GENRE, t.genre_id, t.genre.clone()),
        MenuItem::new(proto::ITEM_DATE_ADDED, 0, t.date_added.clone()),
    ]
}

/// Cue entries as beat-link's `CueList` reads an extended cue-list reply.
/// Each entry is little-endian:
///
/// ```text
/// 0x00 u32  entry length
/// 0x04 u8   hot cue (0 = memory point, 1 = A, ...)
/// 0x06 u8   1 = point, 2 = loop
/// 0x0c u32  time, ms            0x10 u32  loop end, ms
/// 0x22 u8   memory-point colour (rekordbox colour index)
/// 0x48 u16  comment size in bytes (UTF-16 incl. NUL), 0 if none
/// 0x4a      comment, UTF-16LE
/// 0x4e+size hot-cue colour index, then red, green, blue
/// ```
fn encode_extended_cues(cues: &[Cue]) -> Vec<u8> {
    let mut out = Vec::new();
    for cue in cues {
        let comment: Vec<u8> = if cue.comment.is_empty() {
            Vec::new()
        } else {
            cue.comment
                .encode_utf16()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect()
        };
        let size = comment.len();
        let len = (0x52 + size + 3) & !3;
        let mut entry = vec![0u8; len];
        entry[0x00..0x04].copy_from_slice(&(len as u32).to_le_bytes());
        entry[0x04] = cue.hot_cue;
        entry[0x06] = if cue.loop_end_ms.is_some() { 2 } else { 1 };
        entry[0x0c..0x10].copy_from_slice(&cue.time_ms.to_le_bytes());
        entry[0x10..0x14].copy_from_slice(&cue.loop_end_ms.unwrap_or(0).to_le_bytes());
        entry[0x22] = cue.color_id;
        entry[0x48..0x4a].copy_from_slice(&(size as u16).to_le_bytes());
        entry[0x4a..0x4a + size].copy_from_slice(&comment);
        if let Some((index, [r, g, b])) = cue.hot_cue_color {
            entry[0x4e + size..0x52 + size].copy_from_slice(&[index, r, g, b]);
        }
        out.extend(entry);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::tests::{demo_export, demo_library};
    use crate::media::Medium;

    const USB: u32 = 0x03;

    /// A session for player 1 with the demo export in its USB slot.
    fn session() -> Session {
        let media = Arc::new(PlayerMedia::default());
        media.insert(Slot::Usb, Medium::open(&demo_export()).unwrap());
        Session::new(1, media)
    }

    /// `r:m:s:t` for player 5 asking about the given slot.
    fn rmst(slot: u32) -> Field {
        Field::Number4((5 << 24) | (1 << 16) | (slot << 8) | 1)
    }

    fn request(msg_type: u16, args: Vec<Field>) -> Message {
        Message::new(7, msg_type, args)
    }

    fn first_track() -> u32 {
        demo_library().tracks[0].id
    }

    #[test]
    fn metadata_is_for_the_requested_track_and_pages_through_render() {
        let mut s = session();
        let lib = demo_library();
        let wanted = &lib.tracks[1];
        let r = s.respond(&request(proto::MSG_REKORDBOX_METADATA_REQ, vec![rmst(USB), Field::Number4(wanted.id)]));
        assert_eq!(r[0].message_type, proto::MSG_MENU_AVAILABLE);
        assert_eq!(r[0].number(1), Some(11));

        let r = s.respond(&request(
            proto::MSG_RENDER_MENU_REQ,
            vec![rmst(USB), Field::Number4(0), Field::Number4(11), Field::Number4(0), Field::Number4(11), Field::Number4(0)],
        ));
        assert_eq!(r.first().unwrap().message_type, proto::MSG_MENU_HEADER);
        assert_eq!(r.last().unwrap().message_type, proto::MSG_MENU_FOOTER);
        let title = &r[1];
        assert_eq!(title.message_type, proto::MSG_MENU_ITEM);
        assert_eq!(title.number(6), Some(proto::ITEM_TITLE));
        assert_eq!(title.number(1), Some(wanted.id));
        assert_eq!(title.string(3), Some(wanted.title.as_str()));
        let size = (wanted.title.encode_utf16().count() as u32 + 1) * 2;
        assert_eq!(title.number(2), Some(size), "label 1 byte size");
    }

    #[test]
    fn missing_media_or_track_is_unavailable() {
        let mut s = session();
        const SD: u32 = 0x02;
        for (slot, id) in [(SD, first_track()), (USB, 999_999)] {
            let r = s.respond(&request(proto::MSG_BEAT_GRID_REQ, vec![rmst(slot), Field::Number4(id)]));
            assert_eq!(r[0].message_type, proto::MSG_UNAVAILABLE);
            assert_eq!(r[0].number(0), Some(proto::MSG_BEAT_GRID_REQ.into()));
        }
    }

    #[test]
    fn data_replies_carry_the_blob_in_argument_3() {
        let mut s = session();
        let id = first_track();
        let preview = s.respond(&request(
            proto::MSG_WAVEFORM_PREVIEW_REQ,
            vec![rmst(USB), Field::Number4(4), Field::Number4(id), Field::Number4(0)],
        ));
        assert_eq!(preview[0].message_type, proto::MSG_WAVE_PREVIEW);
        assert!(matches!(&preview[0].arguments[3], Field::Binary(b) if b.len() == 800));

        let detail = s.respond(&request(proto::MSG_WAVEFORM_DETAIL_REQ, vec![rmst(USB), Field::Number4(id)]));
        assert_eq!(detail[0].message_type, proto::MSG_WAVE_DETAIL_RESP);

        let grid = s.respond(&request(proto::MSG_BEAT_GRID_REQ, vec![rmst(USB), Field::Number4(id)]));
        assert_eq!(grid[0].arguments.len(), 5);

        let tag = s.respond(&request(
            proto::MSG_ANLZ_TAG_REQ,
            vec![
                rmst(USB),
                Field::Number4(id),
                Field::Number4(u32::from_le_bytes(*b"PWV4")),
                Field::Number4(u32::from_le_bytes(*b"EXT\0")),
            ],
        ));
        assert_eq!(tag[0].message_type, proto::MSG_ANLZ_TAG);
        let Field::Binary(blob) = &tag[0].arguments[3] else { panic!("binary") };
        assert_eq!(&blob[4..8], b"PWV4", "raw tag after 4 leading bytes");
    }

    #[test]
    fn menus_list_tracks_and_playlists() {
        let mut s = session();
        let r = s.respond(&request(proto::MSG_TRACK_MENU_REQ, vec![rmst(USB), Field::Number4(0)]));
        assert_eq!(r[0].number(1), Some(2));
        let r = s.respond(&request(proto::MSG_ROOT_MENU_REQ, vec![rmst(USB), Field::Number4(0), Field::Number4(0xffffff)]));
        assert_eq!(r[0].number(1), Some(6));
        let r = s.respond(&request(
            proto::MSG_PLAYLIST_REQ,
            vec![rmst(USB), Field::Number4(0), Field::Number4(0), Field::Number4(1)],
        ));
        assert_eq!(r[0].message_type, proto::MSG_MENU_AVAILABLE);
    }

    /// beat-link's own dbserver client and parsers (from an installed Beat
    /// Link Trigger, or `BEATLINK_JAR`) read this server's replies. Skipped
    /// when Java or the jar isn't available.
    #[tokio::test(flavor = "multi_thread")]
    async fn beatlink_client_reads_our_replies() {
        let jar = std::env::var_os("BEATLINK_JAR").map(std::path::PathBuf::from).unwrap_or_else(|| {
            "/Applications/Beat Link Trigger.app/Contents/app/beat-link-trigger.jar".into()
        });
        if !jar.exists() || std::process::Command::new("java").arg("-version").output().is_err() {
            eprintln!("skipping: needs java and a beat-link jar (set BEATLINK_JAR)");
            return;
        }

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let media = Arc::new(PlayerMedia::default());
        media.insert(Slot::Usb, Medium::open(&demo_export()).unwrap());
        let cfg = DbServerConfig {
            device_number: 1,
            ip: Ipv4Addr::LOCALHOST,
            tcp_port: port,
            player_model: "CDJ-3000".into(),
        };
        tokio::spawn(DbServer::new(cfg, media).serve(listener));

        let lib = demo_library();
        let track = lib.tracks[0].clone();
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/beatlink/DbCheck.java");
        let out = tokio::task::spawn_blocking(move || {
            std::process::Command::new("java")
                .arg("-cp")
                .arg(&jar)
                .arg(source)
                .args([port.to_string(), "1".into(), track.id.to_string()])
                .output()
                .unwrap()
        })
        .await
        .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "DbCheck failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
        let seen: std::collections::HashMap<&str, &str> = stdout
            .lines()
            .filter_map(|l| l.strip_prefix("cc ")?.split_once('='))
            .collect();
        let field = |k: &str| seen.get(k).copied().unwrap_or("<missing>");

        let t = &lib.tracks[0];
        assert_eq!(field("cueReply"), "CUE_LIST_EXT");
        assert_eq!(field("cues"), lib.cues_for(t).unwrap().len().to_string());
        assert_eq!(field("title"), t.title);
        assert_eq!(field("artist"), t.artist);
        assert_eq!(field("album"), t.album);
        assert_eq!(field("duration"), t.duration_s.to_string());
        assert_eq!(field("tempo"), t.bpm_hundredths.to_string());
        assert_eq!(field("artworkId"), t.artwork_id.to_string());
        let beats = lib.beats_for(t).unwrap();
        assert_eq!(field("beats"), beats.len().to_string());
        assert_eq!(field("firstBeatMs"), beats[0].0.to_string());
        assert_eq!(field("firstBeatInBar"), beats[0].1.to_string());
        assert_eq!(field("previewSegments"), "400");
        assert_eq!(field("detailFrames"), lib.waveform_detail_for(t).unwrap().len().to_string());
        assert_eq!(field("missingReply"), "UNAVAILABLE");
        let titles: Vec<String> = {
            let mut v: Vec<String> = lib.tracks.iter().map(|t| t.title.clone()).collect();
            v.sort_by_key(|s| s.to_lowercase());
            v
        };
        assert_eq!(field("tracks"), format!("[{}]", titles.join(", ")));
        assert!(field("root").contains("PLAYLIST"), "root menu: {}", field("root"));
    }

    #[test]
    fn extended_cue_entries_match_beatlink_layout() {
        let cues = [
            Cue {
                hot_cue: 0,
                time_ms: 1500,
                loop_end_ms: None,
                color_id: 3,
                hot_cue_color: None,
                comment: String::new(),
            },
            Cue {
                hot_cue: 2,
                time_ms: 64_000,
                loop_end_ms: Some(72_000),
                color_id: 0,
                hot_cue_color: Some((0x22, [0xff, 0xa0, 0x00])),
                comment: "Drop".into(),
            },
        ];
        let blob = encode_extended_cues(&cues);
        let le32 = |o: usize| u32::from_le_bytes(blob[o..o + 4].try_into().unwrap());
        let first = le32(0) as usize;
        assert_eq!(first, 0x54, "0x52 rounded up to 4");
        assert_eq!((blob[4], blob[6], le32(0x0c), blob[0x22]), (0, 1, 1500, 3));

        let e = first;
        assert_eq!((blob[e + 4], blob[e + 6]), (2, 2), "hot cue B, loop");
        assert_eq!((le32(e + 0x0c), le32(e + 0x10)), (64_000, 72_000));
        let size = u16::from_le_bytes([blob[e + 0x48], blob[e + 0x49]]) as usize;
        assert_eq!(size, 10, "\"Drop\" + NUL in UTF-16");
        assert_eq!(&blob[e + 0x4e + size..e + 0x52 + size], &[0x22, 0xff, 0xa0, 0x00]);
        assert_eq!(blob.len(), e + le32(e) as usize);
    }
}
