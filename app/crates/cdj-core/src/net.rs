//! Network interface discovery and UDP socket helpers.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};

use thiserror::Error;
use tokio::net::UdpSocket;

pub const PORT_ANNOUNCE: u16 = 50000;
pub const PORT_BEAT: u16 = 50001;
pub const PORT_STATUS: u16 = 50002;

#[derive(Debug, Error)]
pub enum NetError {
    #[error("no IPv4 address on interface {0}")]
    NoIpv4(String),
    #[error("interface {0} not found")]
    InterfaceNotFound(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    MacAddr(#[from] mac_address::MacAddressError),
}

/// A resolved network interface we can emit Pro DJ Link traffic on.
#[derive(Debug, Clone)]
pub struct Interface {
    pub name: String,
    pub ipv4: Ipv4Addr,
    pub broadcast: Ipv4Addr,
    pub mac: [u8; 6],
}

impl Interface {
    /// Resolve an interface by its OS name (e.g. `en0`, `feth0`).
    pub fn by_name(name: &str) -> Result<Self, NetError> {
        let ifaces = if_addrs::get_if_addrs()?;
        let matched: Vec<_> = ifaces.iter().filter(|i| i.name == name).collect();
        if matched.is_empty() {
            return Err(NetError::InterfaceNotFound(name.to_string()));
        }
        let v4 = matched
            .iter()
            .find_map(|i| match i.addr {
                if_addrs::IfAddr::V4(ref a) => Some(a.clone()),
                _ => None,
            })
            .ok_or_else(|| NetError::NoIpv4(name.to_string()))?;

        let broadcast = v4.broadcast.unwrap_or_else(|| {
            // Fallback: compute from ip + netmask.
            let ip = v4.ip.octets();
            let mask = v4.netmask.octets();
            Ipv4Addr::new(
                ip[0] | !mask[0],
                ip[1] | !mask[1],
                ip[2] | !mask[2],
                ip[3] | !mask[3],
            )
        });

        let mac = mac_address::mac_address_by_name(name)?
            .map(|m| m.bytes())
            .unwrap_or([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);

        Ok(Self {
            name: name.to_string(),
            ipv4: v4.ip,
            broadcast,
            mac,
        })
    }

    /// List candidate interfaces (IPv4, not loopback).
    pub fn list() -> Result<Vec<String>, NetError> {
        let ifaces = if_addrs::get_if_addrs()?;
        let mut names: Vec<String> = ifaces
            .into_iter()
            .filter(|i| matches!(i.addr, if_addrs::IfAddr::V4(_)) && !i.is_loopback())
            .map(|i| i.name)
            .collect();
        names.sort();
        names.dedup();
        Ok(names)
    }
}

/// Bind a UDP socket on the given interface's IPv4 + port, with broadcast
/// enabled. Using `0.0.0.0` as the bind address (not the interface IP) lets us
/// receive directed and broadcast traffic on that port regardless of source
/// subnet — real CDJs are not picky about which interface announcements arrive
/// on.
pub async fn bind_broadcast(port: u16) -> std::io::Result<UdpSocket> {
    let sock = UdpSocket::bind(SocketAddr::V4(SocketAddrV4::new(
        Ipv4Addr::UNSPECIFIED,
        port,
    )))
    .await?;
    sock.set_broadcast(true)?;
    Ok(sock)
}

pub fn broadcast_addr(iface: &Interface, port: u16) -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(iface.broadcast, port))
}

pub fn _ip_octets(ip: IpAddr) -> [u8; 4] {
    match ip {
        IpAddr::V4(v4) => v4.octets(),
        IpAddr::V6(_) => [0; 4],
    }
}
