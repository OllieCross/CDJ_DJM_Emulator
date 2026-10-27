//! Setting up the macOS `feth` virtual ethernet pair the rig runs on.
//!
//! `feth0` is the emulator side, `feth1` the client side (ShowKontrol / Beat
//! Link Trigger bind its address, `.200` by default). Broadcasts the
//! emulator sends out of `feth0` arrive on `feth1`, which is how client apps
//! on this Mac discover the virtual devices. Each device needs its own source
//! address (`.1`-`.4` for players, `.33` for the mixer) so clients can tell
//! them apart.
//!
//! Two layouts:
//!
//! * [`Layout::SharedSubnet`] - device addresses live on `feth0` in the same
//!   /24 as `feth1`. Known to work with Beat Link Trigger, but client apps
//!   see two interfaces on the subnet and may pick `feth0`, filtering out
//!   the emulator's packets as their own: start them while `feth0` is down.
//! * [`Layout::LoopbackAliases`] - device addresses are /32 aliases on
//!   `lo0` and `feth0` carries no IPv4 address, so `feth1` is the only
//!   candidate interface clients can pick. Broadcasts go to
//!   255.255.255.255 out of `feth0`. Experimental.
//!
//! Creating interfaces needs root: [`FethPlan::apply`] runs the commands
//! through one macOS administrator prompt.
//!
//! ```no_run
//! let plan = cdj_core::feth::setup_plan("feth0", "feth1", "10.77.77.1", "10.77.77.200", 24);
//! for line in plan.commands() { println!("sudo {line}"); }
//! ```

use std::net::Ipv4Addr;
use std::process::Command;

use anyhow::{bail, Context};

use crate::link::Topology;
use crate::rig::Addressing;

/// Device numbers that get an address: players 1-4 and the mixer.
pub const DEVICE_NUMBERS: [u8; 5] = [1, 2, 3, 4, 33];

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Layout {
    SharedSubnet,
    LoopbackAliases,
}

/// A ready-to-run plan for the emulator's feth pair.
#[derive(Debug, Clone)]
pub struct FethPlan {
    pub a_name: String,
    pub b_name: String,
    pub a_ip: String,
    pub b_ip: String,
    pub prefix: u8,
    pub layout: Layout,
}

pub fn setup_plan(
    a_name: impl Into<String>,
    b_name: impl Into<String>,
    a_ip: impl Into<String>,
    b_ip: impl Into<String>,
    prefix: u8,
) -> FethPlan {
    FethPlan {
        a_name: a_name.into(),
        b_name: b_name.into(),
        a_ip: a_ip.into(),
        b_ip: b_ip.into(),
        prefix,
        layout: Layout::SharedSubnet,
    }
}

/// What is currently set up, as far as an unprivileged process can tell.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FethStatus {
    pub pair_exists: bool,
    pub client_ip_up: bool,
    pub missing_device_ips: Vec<Ipv4Addr>,
    /// The emulator side can send straight to the client apps.
    pub emulator_route_up: bool,
}

impl FethStatus {
    pub fn ready(&self) -> bool {
        self.pair_exists && self.client_ip_up && self.missing_device_ips.is_empty() && self.emulator_route_up
    }
}

impl FethPlan {
    pub fn with_layout(mut self, layout: Layout) -> Self {
        self.layout = layout;
        self
    }

    fn prefix3(&self) -> &str {
        self.a_ip.rsplit_once('.').map_or("10.77.77", |(p, _)| p)
    }

    pub fn device_ip(&self, device_number: u8) -> String {
        format!("{}.{device_number}", self.prefix3())
    }

    /// Where the rig sends from and to on this layout.
    pub fn rig_network(&self) -> anyhow::Result<(Topology, Addressing)> {
        let parse = |s: &str| s.parse::<Ipv4Addr>().with_context(|| format!("bad IPv4 address {s}"));
        let a = parse(&self.a_ip)?.octets();
        let broadcast = match self.layout {
            Layout::SharedSubnet => {
                let mask = u32::MAX.checked_shl(32 - u32::from(self.prefix.min(32))).unwrap_or(0);
                Ipv4Addr::from(u32::from(Ipv4Addr::from(a)) | !mask)
            }
            Layout::LoopbackAliases => Ipv4Addr::BROADCAST,
        };
        Ok((
            Topology {
                iface: self.a_name.clone(),
                broadcast,
                peers: vec![parse(&self.b_ip)?],
            },
            Addressing::Subnet([a[0], a[1], a[2]]),
        ))
    }

    /// Commands that bring the pair up (to run as root). Safe to re-run.
    pub fn commands(&self) -> Vec<String> {
        let mask = prefix_to_mask(self.prefix);
        let net = network_addr(&self.a_ip, self.prefix);
        let (a, b) = (&self.a_name, &self.b_name);

        let mut cmds = vec![
            format!("ifconfig {a} >/dev/null 2>&1 || ifconfig {a} create"),
            format!("ifconfig {b} >/dev/null 2>&1 || ifconfig {b} create"),
            format!("ifconfig {a} peer {b}"),
        ];
        match self.layout {
            Layout::SharedSubnet => {
                cmds.push(format!("ifconfig {a} inet {} netmask {mask} up", self.a_ip));
                for n in DEVICE_NUMBERS.iter().filter(|&&n| self.device_ip(n) != self.a_ip) {
                    cmds.push(format!("ifconfig {a} inet alias {} netmask {mask} up", self.device_ip(*n)));
                }
                cmds.push(format!("ifconfig {b} inet {} netmask {mask} up", self.b_ip));
                // macOS keeps one route per subnet. Give it to feth1 so client
                // apps can send; the emulator's sockets are pinned to feth0.
                cmds.push(format!("route -q delete -net {net}/{} >/dev/null 2>&1 || true", self.prefix));
                cmds.push(format!("route -q add -net {net}/{} -interface {b}", self.prefix));
                cmds.extend(self.emulator_route(&net));
            }
            Layout::LoopbackAliases => {
                cmds.push(format!("ifconfig {a} up"));
                for n in DEVICE_NUMBERS {
                    cmds.push(format!("ifconfig lo0 alias {} netmask 255.255.255.255", self.device_ip(n)));
                }
                cmds.push(format!("ifconfig {b} inet {} netmask {mask} up", self.b_ip));
                cmds.extend(self.emulator_route(&net));
            }
        }
        cmds
    }

    /// A route only the emulator's sockets (pinned to the emulator side)
    /// use, so they can send straight to the client apps; the subnet's main
    /// route belongs to the client side.
    fn emulator_route(&self, net: &str) -> [String; 2] {
        let (a, prefix) = (&self.a_name, self.prefix);
        [
            format!("route -q delete -net {net}/{prefix} -ifscope {a} >/dev/null 2>&1 || true"),
            format!("route -q add -net {net}/{prefix} -interface {a} -ifscope {a}"),
        ]
    }

    /// Commands that remove everything [`commands`](Self::commands) set up.
    pub fn teardown_commands(&self) -> Vec<String> {
        let mut cmds = vec![
            format!("ifconfig {} destroy >/dev/null 2>&1 || true", self.a_name),
            format!("ifconfig {} destroy >/dev/null 2>&1 || true", self.b_name),
        ];
        if self.layout == Layout::LoopbackAliases {
            for n in DEVICE_NUMBERS {
                cmds.push(format!("ifconfig lo0 -alias {} >/dev/null 2>&1 || true", self.device_ip(n)));
            }
        }
        cmds
    }

    /// Bring the pair up, asking for an administrator password.
    pub fn apply(&self) -> anyhow::Result<()> {
        run_privileged(&self.commands())
    }

    /// Remove the pair, asking for an administrator password.
    pub fn teardown(&self) -> anyhow::Result<()> {
        run_privileged(&self.teardown_commands())
    }

    pub fn status(&self) -> FethStatus {
        let addrs = if_addrs::get_if_addrs().unwrap_or_default();
        let has_ip = |iface: Option<&str>, ip: &str| {
            addrs.iter().any(|i| {
                iface.map_or(true, |name| i.name == name) && i.ip().to_string() == ip
            })
        };
        FethStatus {
            pair_exists: interface_exists(&self.a_name) && interface_exists(&self.b_name),
            client_ip_up: has_ip(Some(&self.b_name), &self.b_ip),
            missing_device_ips: DEVICE_NUMBERS
                .iter()
                .map(|&n| self.device_ip(n))
                .filter(|ip| !has_ip(None, ip))
                .filter_map(|ip| ip.parse().ok())
                .collect(),
            emulator_route_up: self.emulator_route_up(),
        }
    }

    /// Whether sockets pinned to the emulator side have a route to the
    /// client address (see [`emulator_route`](Self::emulator_route)).
    fn emulator_route_up(&self) -> bool {
        Command::new("/sbin/route")
            .args(["-n", "get", "-ifscope", &self.a_name, &self.b_ip])
            .output()
            .is_ok_and(|out| {
                out.status.success()
                    && String::from_utf8_lossy(&out.stdout)
                        .lines()
                        .any(|l| l.trim() == format!("interface: {}", self.a_name))
            })
    }
}

fn interface_exists(name: &str) -> bool {
    let Ok(cname) = std::ffi::CString::new(name) else {
        return false;
    };
    // SAFETY: `cname` is a valid NUL-terminated string for the call's duration.
    unsafe { libc::if_nametoindex(cname.as_ptr()) != 0 }
}

/// Run shell commands as root via the macOS administrator prompt.
fn run_privileged(cmds: &[String]) -> anyhow::Result<()> {
    let shell = format!("set -e\n{}", cmds.join("\n"));
    let escaped = shell.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!("do shell script \"{escaped}\" with administrator privileges");
    let out = Command::new("/usr/bin/osascript")
        .args(["-e", &script])
        .output()
        .context("running osascript")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("-128") {
            bail!("cancelled at the administrator prompt");
        }
        bail!("network setup failed: {}", err.trim());
    }
    Ok(())
}

fn network_addr(ip: &str, prefix: u8) -> String {
    let Ok(ip) = ip.parse::<Ipv4Addr>() else {
        return ip.to_string();
    };
    let mask = u32::MAX.checked_shl(32 - u32::from(prefix.min(32))).unwrap_or(0);
    Ipv4Addr::from(u32::from(ip) & mask).to_string()
}

fn prefix_to_mask(prefix: u8) -> String {
    let mask = u32::MAX.checked_shl(32 - u32::from(prefix.min(32))).unwrap_or(0);
    Ipv4Addr::from(mask).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> FethPlan {
        setup_plan("feth0", "feth1", "10.77.77.1", "10.77.77.200", 24)
    }

    #[test]
    fn mask_for_24_is_255_255_255_0() {
        assert_eq!(prefix_to_mask(24), "255.255.255.0");
    }

    #[test]
    fn mask_for_16_is_255_255_0_0() {
        assert_eq!(prefix_to_mask(16), "255.255.0.0");
    }

    #[test]
    fn shared_subnet_puts_device_aliases_on_feth0() {
        let cmds = plan().commands();
        // create x2, peer, primary IP, 4 aliases, client IP, route delete +
        // add, feth0-scoped route delete + add
        assert_eq!(cmds.len(), 13);
        assert!(cmds.contains(&"route -q add -net 10.77.77.0/24 -interface feth0 -ifscope feth0".into()));
        assert!(cmds.contains(&"ifconfig feth0 inet alias 10.77.77.33 netmask 255.255.255.0 up".into()));
        assert!(cmds.contains(&"route -q add -net 10.77.77.0/24 -interface feth1".into()));
        assert_eq!(plan().teardown_commands().len(), 2);
    }

    #[test]
    fn loopback_layout_keeps_device_addresses_off_feth0() {
        let p = plan().with_layout(Layout::LoopbackAliases);
        let cmds = p.commands();
        assert!(cmds.contains(&"ifconfig feth0 up".into()));
        assert!(cmds.contains(&"ifconfig lo0 alias 10.77.77.4 netmask 255.255.255.255".into()));
        assert!(!cmds.iter().any(|c| c.starts_with("ifconfig feth0 inet")));
        assert_eq!(p.teardown_commands().len(), 2 + DEVICE_NUMBERS.len());
    }

    #[test]
    fn rig_network_matches_layout() {
        let (topology, addressing) = plan().rig_network().unwrap();
        assert_eq!(topology.broadcast, Ipv4Addr::new(10, 77, 77, 255));
        assert_eq!(topology.peers, vec![Ipv4Addr::new(10, 77, 77, 200)]);
        assert!(matches!(addressing, Addressing::Subnet([10, 77, 77])));

        let (topology, _) = plan().with_layout(Layout::LoopbackAliases).rig_network().unwrap();
        assert_eq!(topology.broadcast, Ipv4Addr::BROADCAST);
    }
}
