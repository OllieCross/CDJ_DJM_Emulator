//! Network interface discovery and UDP socket helpers.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};

use socket2::{Domain, Protocol, Socket, Type};
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

/// Create a UDP send socket bound to `local_ip:0` (OS-assigned ephemeral port)
/// with broadcast enabled.
///
/// `SO_DONTROUTE` is set so sends bypass the routing table and go directly out
/// the interface that owns `local_ip`. This is required on macOS when using a
/// `feth` pair: we delete the emulator-side connected route so BLT can hold the
/// only route for the /30 subnet, and without `SO_DONTROUTE` the kernel returns
/// EHOSTUNREACH on the very first send.
pub async fn bind_sender(local_ip: Ipv4Addr, iface_name: &str) -> std::io::Result<UdpSocket> {
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_broadcast(true)?;
    // On macOS the feth-plan deletes the connected route for our subnet so the
    // client app (BLT/ShowKontrol) holds the only route via feth1. We need to
    // force every send out of feth0 anyway, including unicast to peers on the
    // same subnet (e.g. 10.77.77.200) - SO_DONTROUTE alone returns ENETUNREACH
    // for unicast because the kernel can't resolve the destination without a
    // route. IP_BOUND_IF (macOS/iOS) pins the socket to feth0 directly,
    // bypassing the route table for both broadcast and unicast.
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        // SO_DONTROUTE: bypass the route table. With the feth-plan setup the
        // connected /24 route is removed (so the client app can hold the only
        // route via feth1), and broadcasts already worked via DONTROUTE.
        let val: libc::c_int = 1;
        unsafe {
            libc::setsockopt(
                sock.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_DONTROUTE,
                &val as *const libc::c_int as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
    }
    // On macOS additionally pin the outgoing interface with IP_BOUND_IF. With
    // DONTROUTE alone, broadcast sends work but unicast to subnet peers (e.g.
    // 10.77.77.200) returns ENETUNREACH because there is no connected route to
    // resolve the destination. IP_BOUND_IF tells the kernel "always send out
    // this interface", which makes unicast across the feth peer succeed.
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        use std::os::unix::io::AsRawFd;
        const IP_BOUND_IF: libc::c_int = 25;
        let cname = CString::new(iface_name).expect("iface_name has no NUL");
        let idx: libc::c_uint = unsafe { libc::if_nametoindex(cname.as_ptr()) };
        if idx == 0 {
            return Err(std::io::Error::last_os_error());
        }
        unsafe {
            let r = libc::setsockopt(
                sock.as_raw_fd(),
                libc::IPPROTO_IP,
                IP_BOUND_IF,
                &idx as *const libc::c_uint as *const libc::c_void,
                std::mem::size_of::<libc::c_uint>() as libc::socklen_t,
            );
            if r != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
    }
    #[cfg(not(unix))]
    let _ = iface_name;
    sock.set_nonblocking(true)?;
    sock.bind(&SocketAddr::V4(SocketAddrV4::new(local_ip, 0)).into())?;
    let std_sock: std::net::UdpSocket = sock.into();
    UdpSocket::from_std(std_sock)
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
