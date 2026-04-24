//! Helper for setting up macOS `feth` virtual ethernet interface pairs.
//!
//! Creates a feth pair on a /24 subnet. feth0 hosts the emulator with one IP
//! alias per virtual device (last octet = device number: .1-.4 for CDJs, .33
//! for DJM). feth1 is the client-app interface (ShowKontrol/BLT at .200).
//!
//! Each device needs a unique source IP so Pro DJ Link clients like ShowKontrol
//! can distinguish them - real CDJs each have a distinct IP address.
//!
//! `feth` requires `sudo`. We emit the commands for the user to review and run.
//!
//! Usage:
//!
//! ```no_run
//! let plan = cdj_core::feth::setup_plan("feth0", "feth1", "10.77.77.1", "10.77.77.200", 24);
//! for line in plan.commands() { println!("{line}"); }
//! ```

/// A ready-to-run shell plan to create a `feth` pair for the emulator.
pub struct FethPlan {
    pub a_name: String,
    pub b_name: String,
    pub a_ip: String,
    pub b_ip: String,
    pub prefix: u8,
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
    }
}

impl FethPlan {
    /// Shell commands to bring the pair up.
    ///
    /// feth0 gets the primary emulator IP plus alias IPs for each virtual
    /// device (CDJ 1-4 at .1-.4, DJM at .33). feth1 gets the client-app IP
    /// (.200 by default). The single /24 route is via feth1 so ShowKontrol can
    /// broadcast; emulator sockets use SO_DONTROUTE to bypass that route.
    pub fn commands(&self) -> Vec<String> {
        let mask = prefix_to_mask(self.prefix);
        let net = network_addr(&self.a_ip, self.prefix);

        // Derive alias IPs from the primary a_ip: same first 3 octets.
        let prefix3 = self.a_ip.rsplitn(2, '.').nth(1).unwrap_or("10.77.77");

        let cmds = vec![
            format!("sudo ifconfig {} create", self.a_name),
            format!("sudo ifconfig {} create", self.b_name),
            format!("sudo ifconfig {} peer {}", self.a_name, self.b_name),
            // Primary IP for feth0 (CDJ 1 + base interface address).
            format!(
                "sudo ifconfig {} inet {} netmask {} up",
                self.a_name, self.a_ip, mask
            ),
            // Alias IPs for CDJ 2, 3, 4, and DJM - each gets a unique source IP.
            format!(
                "sudo ifconfig {} inet alias {}.2 netmask {} up",
                self.a_name, prefix3, mask
            ),
            format!(
                "sudo ifconfig {} inet alias {}.3 netmask {} up",
                self.a_name, prefix3, mask
            ),
            format!(
                "sudo ifconfig {} inet alias {}.4 netmask {} up",
                self.a_name, prefix3, mask
            ),
            format!(
                "sudo ifconfig {} inet alias {}.33 netmask {} up",
                self.a_name, prefix3, mask
            ),
            // Client-app interface (ShowKontrol / BLT).
            format!(
                "sudo ifconfig {} inet {} netmask {} up",
                self.b_name, self.b_ip, mask
            ),
            // Only one route per subnet on macOS. Delete the auto-added feth0
            // connected route and add a feth1 route so the client app can send.
            // Emulator uses SO_DONTROUTE so it doesn't need this route.
            format!("sudo route -q delete -net {}/{} 2>/dev/null; true", net, self.prefix),
            format!(
                "sudo route -q add -net {}/{} -interface {}",
                net, self.prefix, self.b_name
            ),
        ];
        cmds
    }

    /// Commands to tear down the pair.
    pub fn teardown_commands(&self) -> Vec<String> {
        vec![
            format!("sudo ifconfig {} destroy", self.a_name),
            format!("sudo ifconfig {} destroy", self.b_name),
        ]
    }
}

fn network_addr(ip: &str, prefix: u8) -> String {
    let parts: Vec<u32> = ip.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    if parts.len() != 4 {
        return ip.to_string();
    }
    let ip_u32 = (parts[0] << 24) | (parts[1] << 16) | (parts[2] << 8) | parts[3];
    let mask_bits = if prefix == 0 { 0u32 } else { u32::MAX << (32 - prefix as u32) };
    let net = ip_u32 & mask_bits;
    let b = net.to_be_bytes();
    format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3])
}

fn prefix_to_mask(prefix: u8) -> String {
    let prefix = prefix.min(32) as u32;
    let bits = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    let octets = bits.to_be_bytes();
    format!("{}.{}.{}.{}", octets[0], octets[1], octets[2], octets[3])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_for_24_is_255_255_255_0() {
        assert_eq!(prefix_to_mask(24), "255.255.255.0");
    }

    #[test]
    fn mask_for_16_is_255_255_0_0() {
        assert_eq!(prefix_to_mask(16), "255.255.0.0");
    }

    #[test]
    fn plan_emits_correct_command_count() {
        let p = setup_plan("feth0", "feth1", "10.77.77.1", "10.77.77.200", 24);
        // create×2, peer, primary IP, 4 alias IPs, feth1 IP, route delete, route add
        assert_eq!(p.commands().len(), 11);
        assert_eq!(p.teardown_commands().len(), 2);
    }
}
