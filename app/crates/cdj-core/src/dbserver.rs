//! Dbserver (TCP 1051 + UDP 12523 port-discovery) for a single virtual CDJ.
//!
//! Pro DJ Link clients use this service to fetch track metadata (title, artist,
//! BPM, duration, ...) from whichever player has a given track loaded.
//! ShowKontrol in particular will show a deck as UNLOADED with BPM 0.00 until
//! a metadata query to the dbserver returns a plausible track, even if the
//! on-:50002 CDJ-status packet already carries a non-zero BPM field.
//!
//! The protocol is documented in `cdj_proto::dbserver`. This module is the
//! server side: two concurrent listeners bound to the CDJ's own IP.
//!
//! We serve synthetic metadata because audio-file-backed tracks, per-track
//! BPM, and the rekordbox-DB parser are M3 milestones; for the dev loop the
//! goal is "plausible enough that ShowKontrol transitions the deck out of
//! UNLOADED".

use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Arc;

use cdj_proto::dbserver::{
    self as proto, port_discovery, Field, Message, GREETING, HEADER_LEN,
    ITEM_ALBUM, ITEM_ARTIST, ITEM_COMMENT, ITEM_DATE_ADDED, ITEM_DURATION, ITEM_GENRE, ITEM_KEY,
    ITEM_RATING, ITEM_TEMPO, ITEM_TITLE, MSG_MENU_AVAILABLE, MSG_MENU_FOOTER, MSG_MENU_HEADER,
    MSG_MENU_ITEM, MSG_REKORDBOX_METADATA_REQ, MSG_SETUP_REQ,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, info, trace, warn};

use crate::player_state::PlayerState;

/// TCP port the real CDJs historically serve dbserver on. The actual port is
/// discovered via the UDP 12523 handshake, but we hard-code this and advertise
/// it in the reply.
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
/// expect a 2-byte big-endian port number back. (An earlier version did UDP
/// here; that's wrong - clients never send UDP and silently give up after
/// 4 retries, leaving the deck without a known dbserver port and skipping
/// metadata fetches entirely.)
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
                // Most disconnects show up as UnexpectedEof; log at debug, not warn.
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
    // 1. Greeting: server sends first, then waits for the client's echo.
    //    ShowKontrol (and beat-link-derived clients) sit silent on the newly-
    //    opened TCP connection until the dbserver speaks; a previous attempt
    //    here did read-then-write and ShowKontrol closed the socket after ~6ms
    //    of silence.
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

    // 2. Process messages in a loop. We accumulate bytes until we have a full
    //    message (since TCP doesn't preserve message boundaries).
    let mut buf: Vec<u8> = Vec::with_capacity(512);
    let mut read_buf = [0u8; 1024];

    loop {
        let n = stream.read(&mut read_buf).await?;
        if n == 0 {
            return Ok(()); // peer closed
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
                    // Partial read OR unparseable. Distinguish so partials keep
                    // waiting while a malformed packet resets the buffer.
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
            // Client tells us its player number; we confirm with MENU_AVAILABLE.
            // Second arg is the server's own player number; clients verify this
            // against the deck they're querying, so it must reflect this CDJ.
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
            // Client is asking for a specific track's metadata. Send an
            // AVAILABLE_DATA reply first (advertising 11 items) then let the
            // client follow up with RENDER_MENU_REQ. Some clients skip the
            // render step and expect items directly; we send both.
            let reply = Message::new(
                msg.transaction_id,
                MSG_MENU_AVAILABLE,
                vec![
                    Field::Number4(MSG_REKORDBOX_METADATA_REQ as u32),
                    Field::Number4(TRACK_METADATA_ITEM_COUNT as u32),
                ],
            );
            stream.write_all(&reply.encode()).await?;
            send_metadata_items(stream, cfg, state, msg.transaction_id).await?;
            debug!(
                num = cfg.device_number,
                "dbserver: handled REKORDBOX_METADATA_REQ"
            );
        }
        proto::MSG_RENDER_MENU_REQ => {
            // If we get this as a follow-up after MENU_AVAILABLE, render items.
            send_metadata_items(stream, cfg, state, msg.transaction_id).await?;
            debug!(num = cfg.device_number, "dbserver: handled RENDER_MENU_REQ");
        }
        proto::MSG_TEARDOWN_REQ => {
            debug!(num = cfg.device_number, "dbserver: client requested teardown");
            // No reply expected; the client will close the socket.
        }
        other => {
            debug!(
                num = cfg.device_number,
                msg_type = format!("{:#06x}", other),
                args = ?msg.arguments,
                "dbserver: unhandled message type, replying with empty menu"
            );
            // Reply with a zero-item MENU_AVAILABLE so the client doesn't hang.
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

/// Number of metadata rows we emit for a track (title, artist, album, genre,
/// duration, tempo, key, rating, comment, date-added). Matches the order in
/// [`send_metadata_items`].
const TRACK_METADATA_ITEM_COUNT: usize = 10;

async fn send_metadata_items(
    stream: &mut TcpStream,
    cfg: &DbServerConfig,
    state: &PlayerState,
    txid: u32,
) -> anyhow::Result<()> {
    // MENU_HEADER: "items start here, first offset = 0".
    let header = Message::new(txid, MSG_MENU_HEADER, vec![Field::Number4(0)]);
    stream.write_all(&header.encode()).await?;

    let track_id = cfg.device_number as u32;
    let title = format!("Virtual Track {}", cfg.device_number);
    let bpm_display = format!("{:.2}", state.bpm_hundredths() as f32 / 100.0);

    // (item_type_byte, short_label, long_label). Owned Strings keep lifetimes simple.
    let items: [(u8, String, String); TRACK_METADATA_ITEM_COUNT] = [
        (ITEM_TITLE, "CDJ Emulator".into(), title),
        (ITEM_ARTIST, String::new(), "CDJ Emulator".into()),
        (ITEM_ALBUM, String::new(), "Virtual".into()),
        (ITEM_GENRE, String::new(), "Electronic".into()),
        (ITEM_DURATION, String::new(), "05:00".into()),
        (ITEM_TEMPO, String::new(), bpm_display),
        (ITEM_KEY, String::new(), "8m".into()),
        (ITEM_RATING, String::new(), "0".into()),
        (ITEM_COMMENT, String::new(), String::new()),
        (ITEM_DATE_ADDED, String::new(), "2026-01-01".into()),
    ];

    for (type_byte, short_label, long_label) in items {
        let item = build_menu_item(txid, track_id, type_byte, &short_label, &long_label);
        stream.write_all(&item.encode()).await?;
    }

    let footer = Message::new(
        txid,
        MSG_MENU_FOOTER,
        vec![Field::Number4(TRACK_METADATA_ITEM_COUNT as u32)],
    );
    stream.write_all(&footer.encode()).await?;
    Ok(())
}

fn build_menu_item(
    txid: u32,
    track_id: u32,
    type_byte: u8,
    short_label: &str,
    long_label: &str,
) -> Message {
    // Menu items carry a fixed 12-argument payload. Only a subset matters for
    // metadata rendering; the rest are zero / padding.
    Message::new(
        txid,
        MSG_MENU_ITEM,
        vec![
            Field::Number4(0),                          // parent ID
            Field::Number4(track_id),                   // main ID
            Field::Number4(0),                          // flags
            Field::Number4(0),                          // more flags
            Field::String(short_label.to_string()),     // short label
            Field::String(long_label.to_string()),      // long label
            Field::Number4(0),                          // unused
            Field::Number1(type_byte),                  // item kind
            Field::Number4(0),                          // sort rank
            Field::Number4(0),                          // artwork id
            Field::Number2(0),                          // padding
            Field::Number2(0),                          // padding
        ],
    )
}
