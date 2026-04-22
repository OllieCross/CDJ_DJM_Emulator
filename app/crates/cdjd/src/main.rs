//! `cdjd` - development CLI for the CDJ emulator.

use anyhow::{Context, Result};
use cdj_core::feth::setup_plan;
use cdj_core::net::Interface;
use cdj_core::orchestrator::{Fleet, FleetConfig};
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "cdjd", about = "CDJ emulator daemon (dev CLI)", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// List IPv4 network interfaces.
    Ifaces,

    /// Print the shell commands needed to create a macOS `feth` virtual
    /// ethernet pair for isolating the emulator's broadcasts.
    FethPlan {
        #[arg(long, default_value = "feth0")]
        a: String,
        #[arg(long, default_value = "feth1")]
        b: String,
        #[arg(long, default_value = "169.254.77.1")]
        a_ip: String,
        #[arg(long, default_value = "169.254.77.2")]
        b_ip: String,
        #[arg(long, default_value_t = 24)]
        prefix: u8,
        /// Also print teardown commands.
        #[arg(long)]
        teardown: bool,
    },

    /// Run a full fleet: 4 virtual CDJs + 1 virtual DJM on the chosen iface.
    RunFleet {
        #[arg(short, long)]
        iface: String,
        #[arg(long, default_value_t = 4)]
        players: u8,
        #[arg(long, default_value_t = true)]
        mixer: bool,
        #[arg(long, default_value = "CDJ-3000")]
        player_model: String,
        #[arg(long, default_value = "DJM-V10")]
        mixer_model: String,
    },

    /// Run a single virtual CDJ (M0 behaviour; useful for basic sanity
    /// checks).
    Run {
        #[arg(short, long)]
        iface: String,
        #[arg(short = 'n', long, default_value_t = 1)]
        device_number: u8,
        #[arg(short, long, default_value = "CDJ-3000")]
        model: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,cdj_core=debug,cdjd=debug")),
        )
        .init();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Ifaces => {
            let names = Interface::list().context("listing interfaces")?;
            for n in names {
                match Interface::by_name(&n) {
                    Ok(i) => println!(
                        "{}\tip={}\tbroadcast={}\tmac={:02x?}",
                        i.name, i.ipv4, i.broadcast, i.mac
                    ),
                    Err(e) => println!("{}\t(resolve failed: {e})", n),
                }
            }
        }
        Cmd::FethPlan {
            a,
            b,
            a_ip,
            b_ip,
            prefix,
            teardown,
        } => {
            let plan = setup_plan(a, b, a_ip, b_ip, prefix);
            println!("# Bring up a feth pair for the emulator. Requires sudo.");
            for c in plan.commands() {
                println!("{c}");
            }
            if teardown {
                println!("\n# Teardown:");
                for c in plan.teardown_commands() {
                    println!("{c}");
                }
            }
        }
        Cmd::RunFleet {
            iface,
            players,
            mixer,
            player_model,
            mixer_model,
        } => {
            let iface = Interface::by_name(&iface)
                .with_context(|| format!("resolving interface {iface}"))?;
            let cfg = FleetConfig {
                iface,
                num_players: players,
                include_mixer: mixer,
                player_model,
                mixer_model,
            };
            Fleet::new(cfg).run().await?;
        }
        Cmd::Run {
            iface,
            device_number,
            model,
        } => {
            let iface = Interface::by_name(&iface)
                .with_context(|| format!("resolving interface {iface}"))?;
            // Single-device run = a Fleet with 1 player and no mixer.
            let cfg = FleetConfig {
                iface,
                num_players: device_number.clamp(1, 4),
                include_mixer: false,
                player_model: model,
                mixer_model: "DJM-V10".to_string(),
            };
            // Note: device_number arg only controls how many players are
            // spawned (1..=4); use RunFleet for precise control.
            Fleet::new(cfg).run().await?;
        }
    }
    Ok(())
}
