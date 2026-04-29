//! Dbserver (TCP 1051 + UDP 12523 port-discovery) for a single virtual CDJ.
//!
//! Pro DJ Link clients use this service to fetch track metadata (title, artist,
//! BPM, duration, ...) from whichever player has a given track loaded.
//!
//! The protocol is documented in `cdj_proto::dbserver`. This module is the
//! server side: two concurrent listeners bound to the CDJ's own IP.

use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Arc;

use cdj_proto::dbserver::{
    self as proto, port_discovery, Field, Message, GREETING, HEADER_LEN,
    ITEM_ALBUM, ITEM_ARTIST, ITEM_COMMENT, ITEM_DATE_ADDED, ITEM_DURATION, ITEM_GENRE, ITEM_KEY,
    ITEM_RATING, ITEM_TEMPO, ITEM_TITLE, MSG_ARTWORK_REQ, MSG_ARTWORK_RESP,
    MSG_BEAT_GRID_REQ, MSG_BEAT_GRID_RESP, MSG_MENU_AVAILABLE, MSG_MENU_FOOTER, MSG_MENU_HEADER,
    MSG_MENU_ITEM, MSG_REKORDBOX_METADATA_REQ, MSG_SETUP_REQ, MSG_WAVE_DETAIL_RESP,
    MSG_WAVE_PREVIEW, MSG_WAVEFORM_DETAIL_REQ, MSG_WAVEFORM_PREVIEW_REQ,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, info, trace, warn};

use crate::player_state::PlayerState;

/// TCP port the dbserver listens on (advertised via the UDP 12523 handshake).
pub const DBSERVER_TCP_PORT: u16 = 1051;

/// Port clients use to discover the dbserver TCP port.
pub const PORT_DISCOVERY_PORT: u16 = 12523;

/// Configuration for one virtual CDJ's dbserver.
#[derive(Debug, Clone)]
pub struct DbServerConfig {
    pub device_number: u8,
    pub ip: Ipv4Addr,
    pub player_model: String,
}

pub struct DbServer {
    cfg: DbServerConfig,
    state: Arc<PlayerState>,
}

impl DbServer {
    pub fn new(cfg: DbServerConfig, state: Arc<PlayerState>) -> Self {
        Self { cfg, state }
    }

    /// Run both listeners concurrently. Returns when either fails.
    pub async fn run(self) -> anyhow::Result<()> {
        let Self { cfg, state } = self;
        let discovery_fut = run_port_discovery(cfg.ip, DBSERVER_TCP_PORT, cfg.device_number);
        let tcp_fut = run_tcp_listener(cfg.clone(), state);
        tokio::select! {
            r = discovery_fut => r,
            r = tcp_fut => r,
        }
    }
}

/// TCP port-discovery listener. Beat-link / ShowKontrol open a fresh TCP
/// connection to port 12523, send the 19-byte "RemoteDBServer" query, and
/// expect a 2-byte big-endian port number back.
async fn run_port_discovery(
    ip: Ipv4Addr,
    tcp_port: u16,
    device_number: u8,
) -> anyhow::Result<()> {
    let listener = TcpListener::bind(SocketAddrV4::new(ip, PORT_DISCOVERY_PORT)).await?;
    info!(
        num = device_number,
        addr = %listener.local_addr()?,
        "dbserver port-discovery listening (tcp)"
    );
    let reply = port_discovery::reply(tcp_port);
    loop {
        let (mut stream, peer) = listener.accept().await?;
        let reply = reply;
        tokio::spawn(async move {
            let mut buf = [0u8; 19];
            match stream.read_exact(&mut buf).await {
                Ok(_) if port_discovery::is_query(&buf) => {
                    if let Err(e) = stream.write_all(&reply).await {
                        warn!(num = device_number, %peer, "port-discovery reply failed: {e}");
                    } else {
                        debug!(num = device_number, %peer, "port-discovery replied 1051");
                    }
                }
                Ok(_) => {
                    trace!(
                        num = device_number,
                        %peer,
                        bytes = ?buf,
                        "port-discovery: unknown query"
                    );
                }
                Err(e) => {
                    debug!(num = device_number, %peer, "port-discovery read failed: {e}");
                }
            }
        });
    }
}

async fn run_tcp_listener(cfg: DbServerConfig, state: Arc<PlayerState>) -> anyhow::Result<()> {
    let listener = TcpListener::bind(SocketAddrV4::new(cfg.ip, DBSERVER_TCP_PORT)).await?;
    info!(
        num = cfg.device_number,
        addr = %listener.local_addr()?,
        "dbserver tcp listening"
    );
    loop {
        let (stream, peer) = listener.accept().await?;
        debug!(num = cfg.device_number, %peer, "dbserver client connected");
        let cfg = cfg.clone();
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, cfg.clone(), state).await {
                debug!(num = cfg.device_number, %peer, "dbserver client: {e}");
            }
        });
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    cfg: DbServerConfig,
    state: Arc<PlayerState>,
) -> anyhow::Result<()> {
    // Server sends greeting first; clients wait for it before speaking.
    stream.write_all(&GREETING).await?;
    let mut echo = [0u8; 5];
    stream.read_exact(&mut echo).await?;
    if echo != GREETING {
        warn!(
            num = cfg.device_number,
            got = ?echo,
            "unexpected client greeting echo, proceeding anyway"
        );
    }

    let mut buf: Vec<u8> = Vec::with_capacity(512);
    let mut read_buf = [0u8; 1024];

    loop {
        let n = stream.read(&mut read_buf).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&read_buf[..n]);

        while buf.len() >= HEADER_LEN {
            match Message::decode(&buf) {
                Ok((msg, consumed)) => {
                    trace!(
                        num = cfg.device_number,
                        txid = format!("{:#010x}", msg.transaction_id),
                        msg_type = format!("{:#06x}", msg.message_type),
                        argc = msg.arguments.len(),
                        "dbserver rx"
                    );
                    handle_message(&mut stream, &cfg, &state, &msg).await?;
                    buf.drain(..consumed);
                }
                Err(e) => {
                    use cdj_proto::error::DecodeError;
                    if matches!(e, DecodeError::TooShort { .. }) {
                        break;
                    }
                    warn!(
                        num = cfg.device_number,
                        err = %e,
                        head = ?&buf[..buf.len().min(32)],
                        "dbserver decode failed, dropping connection"
                    );
                    return Ok(());
                }
            }
        }
    }
}

async fn handle_message(
    stream: &mut TcpStream,
    cfg: &DbServerConfig,
    state: &PlayerState,
    msg: &Message,
) -> anyhow::Result<()> {
    match msg.message_type {
        MSG_SETUP_REQ => {
            let reply = Message::new(
                msg.transaction_id,
                MSG_MENU_AVAILABLE,
                vec![
                    Field::Number4(MSG_SETUP_REQ as u32),
                    Field::Number4(cfg.device_number as u32),
                ],
            );
            stream.write_all(&reply.encode()).await?;
            debug!(num = cfg.device_number, "dbserver: handled SETUP_REQ");
        }
        MSG_REKORDBOX_METADATA_REQ => {
            let count = if state.loaded_track().is_some() {
                TRACK_METADATA_ITEM_COUNT as u32
            } else {
                0
            };
            let reply = Message::new(
                msg.transaction_id,
                MSG_MENU_AVAILABLE,
                vec![
                    Field::Number4(MSG_REKORDBOX_METADATA_REQ as u32),
                    Field::Number4(count),
                ],
            );
            stream.write_all(&reply.encode()).await?;
            debug!(num = cfg.device_number, "dbserver: handled REKORDBOX_METADATA_REQ");
        }
        proto::MSG_RENDER_MENU_REQ => {
            send_metadata_items(stream, cfg, state, msg.transaction_id).await?;
            debug!(num = cfg.device_number, "dbserver: handled RENDER_MENU_REQ");
        }
        MSG_WAVEFORM_PREVIEW_REQ => {
            let data = if let Some((lib, id)) = state.loaded_track() {
                match lib.track_by_id(id) {
                    Some(t) => match lib.waveform_preview_for(t) {
                        Ok(d) => { info!(num = cfg.device_number, bytes = d.len(), "dbserver: waveform preview ok"); d }
                        Err(e) => { warn!(num = cfg.device_number, "dbserver: waveform_preview_for failed: {e}"); vec![] }
                    }
                    None => { warn!(num = cfg.device_number, track_id = id, "dbserver: track not found for waveform"); vec![] }
                }
            } else {
                vec![]
            };
            send_anlz_response(stream, msg.transaction_id, MSG_WAVE_PREVIEW, MSG_WAVEFORM_PREVIEW_REQ, data).await?;
        }
        MSG_WAVEFORM_DETAIL_REQ => {
            let data = if let Some((lib, id)) = state.loaded_track() {
                match lib.track_by_id(id) {
                    Some(t) => match lib.waveform_detail_for(t) {
                        Ok(d) => { info!(num = cfg.device_number, bytes = d.len(), "dbserver: waveform detail ok"); d }
                        Err(e) => { warn!(num = cfg.device_number, "dbserver: waveform_detail_for failed: {e}"); vec![] }
                    }
                    None => { warn!(num = cfg.device_number, track_id = id, "dbserver: track not found for waveform detail"); vec![] }
                }
            } else {
                vec![]
            };
            send_anlz_response(stream, msg.transaction_id, MSG_WAVE_DETAIL_RESP, MSG_WAVEFORM_DETAIL_REQ, data).await?;
        }
        MSG_BEAT_GRID_REQ => {
            let data = if let Some((lib, id)) = state.loaded_track() {
                match lib.track_by_id(id) {
                    Some(t) => match lib.beat_grid_for(t) {
                        Ok(d) => { info!(num = cfg.device_number, bytes = d.len(), "dbserver: beat grid ok"); d }
                        Err(e) => { warn!(num = cfg.device_number, "dbserver: beat_grid_for failed: {e}"); vec![] }
                    }
                    None => { warn!(num = cfg.device_number, track_id = id, "dbserver: track not found for beat grid"); vec![] }
                }
            } else {
                vec![]
            };
            send_anlz_response(stream, msg.transaction_id, MSG_BEAT_GRID_RESP, MSG_BEAT_GRID_REQ, data).await?;
        }
        MSG_ARTWORK_REQ => {
            // Beat-link sends: [player(0), slot(1), track_type(2), artwork_id(3)]
            let artwork_id = msg.arguments.get(3)
                .and_then(|f| if let Field::Number4(n) = f { Some(*n) } else { None })
                .unwrap_or(0);
            let jpeg: Vec<u8> = state.loaded_track()
                .and_then(|(lib, _)| {
                    if artwork_id != 0 {
                        lib.artwork_jpeg(artwork_id)
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| PLACEHOLDER_ART.to_vec());
            let reply = Message::new(
                msg.transaction_id,
                MSG_ARTWORK_RESP,
                vec![
                    Field::Number4(MSG_ARTWORK_REQ as u32),
                    Field::Number4(0),
                    Field::Number4(jpeg.len() as u32),
                    Field::Binary(jpeg),
                ],
            );
            stream.write_all(&reply.encode()).await?;
            debug!(num = cfg.device_number, artwork_id, "dbserver: handled ARTWORK_REQ");
        }
        proto::MSG_TEARDOWN_REQ => {
            debug!(num = cfg.device_number, "dbserver: client requested teardown");
        }
        other => {
            debug!(
                num = cfg.device_number,
                msg_type = format!("{:#06x}", other),
                args = ?msg.arguments,
                "dbserver: unhandled message type"
            );
            let reply = Message::new(
                msg.transaction_id,
                MSG_MENU_AVAILABLE,
                vec![Field::Number4(other as u32), Field::Number4(0)],
            );
            stream.write_all(&reply.encode()).await?;
        }
    }
    Ok(())
}

/// Number of metadata rows per track (title, artist, album, genre, duration,
/// tempo, key, rating, comment, date-added).
const TRACK_METADATA_ITEM_COUNT: usize = 10;

async fn send_metadata_items(
    stream: &mut TcpStream,
    cfg: &DbServerConfig,
    state: &PlayerState,
    txid: u32,
) -> anyhow::Result<()> {
    let Some((lib, id)) = state.loaded_track() else {
        // No track loaded: header + empty footer.
        stream.write_all(&Message::new(txid, MSG_MENU_HEADER, vec![Field::Number4(0)]).encode()).await?;
        stream.write_all(&Message::new(txid, MSG_MENU_FOOTER, vec![Field::Number4(0)]).encode()).await?;
        return Ok(());
    };
    let Some(track) = lib.track_by_id(id) else {
        stream.write_all(&Message::new(txid, MSG_MENU_HEADER, vec![Field::Number4(0)]).encode()).await?;
        stream.write_all(&Message::new(txid, MSG_MENU_FOOTER, vec![Field::Number4(0)]).encode()).await?;
        return Ok(());
    };

    stream.write_all(&Message::new(txid, MSG_MENU_HEADER, vec![Field::Number4(0)]).encode()).await?;

    let dev = cfg.device_number as u32;
    let items: [(u8, u32, String, String); TRACK_METADATA_ITEM_COUNT] = [
        (ITEM_TITLE,      dev, track.title.clone(),           track.title.clone()),
        (ITEM_ARTIST,     dev, track.artist.clone(),          track.artist.clone()),
        (ITEM_ALBUM,      dev, track.album.clone(),           track.album.clone()),
        (ITEM_GENRE,      dev, track.genre.clone(),           track.genre.clone()),
        (ITEM_DURATION,   track.duration_s,                   String::new(), String::new()),
        (ITEM_TEMPO,      track.bpm_hundredths as u32,        String::new(), String::new()),
        (ITEM_KEY,        dev, "8m".into(),                   "8m".into()),
        (ITEM_RATING,     0,   String::new(),                 String::new()),
        (ITEM_COMMENT,    dev, track.comment.clone(),         track.comment.clone()),
        (ITEM_DATE_ADDED, dev, "2026-01-01".into(),           "2026-01-01".into()),
    ];

    for (type_byte, item_id, label1, label2) in &items {
        stream.write_all(&build_menu_item(txid, *item_id, *type_byte, label1, label2, track.artwork_id).encode()).await?;
    }

    stream.write_all(
        &Message::new(txid, MSG_MENU_FOOTER, vec![Field::Number4(TRACK_METADATA_ITEM_COUNT as u32)]).encode()
    ).await?;
    Ok(())
}

fn build_menu_item(
    txid: u32,
    track_id: u32,
    type_byte: u8,
    short_label: &str,
    long_label: &str,
    artwork_id: u32,
) -> Message {
    // MENU_ITEM layout (12 args):
    //   [0] parent_id  [1] item_id  [2] label1 size  [3] label1
    //   [4] label2 size  [5] label2  [6] item_type  [7] flags
    //   [8] artwork_id  [9] playlist_position  [10] pad  [11] pad
    let label2_byte_size = ((long_label.encode_utf16().count() + 1) * 2) as u32;
    Message::new(
        txid,
        MSG_MENU_ITEM,
        vec![
            Field::Number4(0),
            Field::Number4(track_id),
            Field::Number4(2),
            Field::String(short_label.to_string()),
            Field::Number4(label2_byte_size),
            Field::String(long_label.to_string()),
            Field::Number1(type_byte),
            Field::Number4(0),
            Field::Number4(artwork_id),
            Field::Number4(0),
            Field::Number2(0),
            Field::Number2(0),
        ],
    )
}

/// Embedded placeholder album art (128x128 JPEG).
static PLACEHOLDER_ART: &[u8] = include_bytes!("assets/placeholder_art.jpg");

async fn send_anlz_response(
    stream: &mut TcpStream,
    txid: u32,
    resp_type: u16,
    req_type: u16,
    data: Vec<u8>,
) -> anyhow::Result<()> {
    let reply = Message::new(
        txid,
        resp_type,
        vec![
            Field::Number4(req_type as u32),
            Field::Number4(0),
            Field::Number4(data.len() as u32),
            Field::Binary(data),
        ],
    );
    stream.write_all(&reply.encode()).await?;
    Ok(())
}
