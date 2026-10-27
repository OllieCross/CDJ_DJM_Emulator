//! Where a virtual device's packets go.
//!
//! Every device sends from its own source IP (one alias per device, so client
//! apps can tell the players apart) out of the emulator side of the feth
//! pair.
//!
//! Discovery packets (claims, keep-alives) are broadcast. Everything else
//! (status, beats, positions, mixer packets) goes straight to the client
//! apps when their address is configured: macOS hands a sender a local copy
//! of each broadcast on a "simplex" interface such as feth0, so a client on
//! the same Mac would otherwise get every broadcast twice and count each
//! beat double. Without configured clients those packets are broadcast.
//!
//! Devices send from ephemeral ports and never bind 50000-50002: Beat Link
//! Trigger binds those ports as wildcard sockets without address reuse, so a
//! bound emulator socket would stop it from starting.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::net::UdpSocket;
use tracing::warn;

use crate::net::{bind_sender, PORT_ANNOUNCE, PORT_BEAT, PORT_STATUS};

/// Network layout shared by every device in the rig.
#[derive(Debug, Clone)]
pub struct Topology {
    /// Interface packets leave through (the emulator side of the feth pair).
    pub iface: String,
    /// Broadcast destination on that interface.
    pub broadcast: Ipv4Addr,
    /// Client apps (BLT, ShowKontrol) that should receive unicast packets.
    pub peers: Vec<Ipv4Addr>,
}

#[derive(Debug, Clone, Copy)]
pub enum Port {
    Announce,
    Beat,
    Status,
}

impl Port {
    fn number(self) -> u16 {
        match self {
            Port::Announce => PORT_ANNOUNCE,
            Port::Beat => PORT_BEAT,
            Port::Status => PORT_STATUS,
        }
    }
}

/// One device's sockets plus where to send.
pub struct DeviceLink {
    pub ip: Ipv4Addr,
    topology: Topology,
    announce: UdpSocket,
    beat: UdpSocket,
    status: UdpSocket,
    /// A send to a client failed; reported once, not every 200 ms.
    peer_failed: AtomicBool,
}

impl DeviceLink {
    pub async fn bind(ip: Ipv4Addr, topology: Topology) -> std::io::Result<Self> {
        Ok(Self {
            announce: bind_sender(ip, &topology.iface).await?,
            beat: bind_sender(ip, &topology.iface).await?,
            status: bind_sender(ip, &topology.iface).await?,
            ip,
            topology,
            peer_failed: AtomicBool::new(false),
        })
    }

    pub fn peer_count(&self) -> usize {
        self.topology.peers.len()
    }

    fn socket(&self, port: Port) -> &UdpSocket {
        match port {
            Port::Announce => &self.announce,
            Port::Beat => &self.beat,
            Port::Status => &self.status,
        }
    }

    /// Send to everyone on the link.
    pub async fn broadcast(&self, port: Port, bytes: &[u8]) -> std::io::Result<()> {
        let dest = SocketAddr::V4(SocketAddrV4::new(self.topology.broadcast, port.number()));
        self.socket(port).send_to(bytes, dest).await.map(|_| ())
    }

    /// Send to each client app directly. Failures are per peer, so one
    /// unreachable address doesn't stop the others; the first is logged.
    pub async fn to_peers(&self, port: Port, bytes: &[u8]) {
        for &peer in &self.topology.peers {
            let dest = SocketAddr::V4(SocketAddrV4::new(peer, port.number()));
            match self.socket(port).send_to(bytes, dest).await {
                Ok(_) => {
                    self.peer_failed.store(false, Ordering::Relaxed);
                }
                Err(e) => {
                    if !self.peer_failed.swap(true, Ordering::Relaxed) {
                        warn!(from = %self.ip, %dest, "can't reach client app: {e} (is the network set up?)");
                    }
                }
            }
        }
    }

    /// Send a non-discovery packet: to the client apps if any are
    /// configured, else to everyone (see the module docs).
    pub async fn send_update(&self, port: Port, bytes: &[u8]) -> std::io::Result<()> {
        if self.topology.peers.is_empty() {
            self.broadcast(port, bytes).await
        } else {
            self.to_peers(port, bytes).await;
            Ok(())
        }
    }
}
