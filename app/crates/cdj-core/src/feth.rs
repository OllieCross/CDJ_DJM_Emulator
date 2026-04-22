//! Helper for setting up macOS `feth` virtual ethernet interface pairs.
//!
//! Because a single Mac has one Wi-Fi / ethernet NIC and ShowKontrol wants
//! broadcast traffic (which does not flow across `lo0`), we create a pair of
//! `feth` interfaces (Apple's in-kernel fake-ethernet) with a link-local
//! subnet. The emulator binds to one, ShowKontrol to the other — or both bind
//! to the same one; either works because feth broadcasts are visible to every
//! listener on the pair.
//!
//! `feth` is only configurable via `ifconfig` and requires `sudo`. Rather than
//! invoking sudo ourselves (and prompting the user mid-run), we emit the
//! commands so the user can review and run them.
//!
//! Usage:
//!
//! ```no_run
//! let plan = cdj_core::feth::setup_plan("feth0", "feth1", "169.254.77.1", "169.254.77.2", 24);
//! for line in plan.commands() { println!("{line}"); }
//! ```

/// A ready-to-run shell plan to create a `feth` pair, bring both up, assign
/// IPs, and bond them.
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
    /// Shell commands to run (as root) to bring the pair up.
    pub fn commands(&self) -> Vec<String> {
        let mask = prefix_to_mask(self.prefix);
        vec![
            format!("sudo ifconfig {} create", self.a_name),
            format!("sudo ifconfig {} create", self.b_name),
            format!("sudo ifconfig {} peer {}", self.a_name, self.b_name),
            format!(
                "sudo ifconfig {} inet {} netmask {} up",
                self.a_name, self.a_ip, mask
            ),
            format!(
                "sudo ifconfig {} inet {} netmask {} up",
                self.b_name, self.b_ip, mask
            ),
        ]
    }

    /// Commands to tear down the pair.
    pub fn teardown_commands(&self) -> Vec<String> {
        vec![
            format!("sudo ifconfig {} destroy", self.a_name),
            format!("sudo ifconfig {} destroy", self.b_name),
        ]
    }
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
    fn plan_emits_five_commands() {
        let p = setup_plan("feth0", "feth1", "169.254.77.1", "169.254.77.2", 24);
        assert_eq!(p.commands().len(), 5);
        assert_eq!(p.teardown_commands().len(), 2);
    }
}
