//! `cdjd` - development CLI for the CDJ emulator.

use anyhow::{Context, Result};
use cdj_core::feth::setup_plan;
use cdj_core::library::Library;
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
        #[arg(long, default_value = "10.77.77.1")]
        a_ip: String,
        #[arg(long, default_value = "10.77.77.200")]
        b_ip: String,
        #[arg(long, default_value_t = 24)]
        prefix: u8,
        #[arg(long)]
        teardown: bool,
    },

    /// Run a full fleet: N virtual CDJs + (optionally) a virtual DJM.
    RunFleet {
        #[arg(short, long)]
        iface: String,
        #[arg(long, default_value_t = 4)]
        players: u8,
        /// Omit the virtual DJM from the fleet.
        #[arg(long)]
        no_mixer: bool,
        #[arg(long, default_value = "CDJ-3000")]
        player_model: String,
        #[arg(long, default_value = "DJM-V10")]
        mixer_model: String,
        /// Initial tempo in BPM (e.g. 128.0).
        #[arg(long, default_value_t = 120.0)]
        bpm: f32,
        /// Start every player in the "playing" state so they emit beat
        /// packets immediately. Useful for timecode / ShowKontrol dev.
        #[arg(long)]
        autoplay: bool,
        /// Track files to load, one per player (up to 4). Pass the flag
        /// multiple times: --track a.flac --track b.mp3. Players without a
        /// track stay idle.
        #[arg(long = "track")]
        tracks: Vec<std::path::PathBuf>,
        /// Offset from playback start to the first beat of bar 1, in ms.
        /// Applies to every loaded track. Per-track beat grids arrive with M3.
        #[arg(long, default_value_t = 0)]
        beat_offset_ms: u32,
        /// Path to a Rekordbox USB export root (e.g. /Volumes/MY_USB or a
        /// folder you exported into). Tracks are assigned in PDB order to
        /// players 1..N and the dbserver serves real waveform/beat-grid data.
        #[arg(long)]
        library: Option<std::path::PathBuf>,
    },

    /// Run a single virtual CDJ (M0-style helper).
    Run {
        #[arg(short, long)]
        iface: String,
        #[arg(short = 'n', long, default_value_t = 1)]
        device_number: u8,
        #[arg(short, long, default_value = "CDJ-3000")]
        model: String,
    },
}

fn bpm_to_hundredths(bpm: f32) -> u16 {
    (bpm * 100.0).round().clamp(0.0, u16::MAX as f32) as u16
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
            no_mixer,
            player_model,
            mixer_model,
            bpm,
            autoplay,
            tracks,
            beat_offset_ms,
            library,
        } => {
            let iface = Interface::by_name(&iface)
                .with_context(|| format!("resolving interface {iface}"))?;
            let tracks = tracks.into_iter().map(Some).collect();
            let library = if let Some(root) = library {
                let lib = tokio::task::spawn_blocking(move || Library::open(&root))
                    .await
                    .context("library loader panicked")?
                    .context("loading rekordbox library")?;
                Some(lib)
            } else {
                None
            };
            let cfg = FleetConfig {
                iface,
                num_players: players,
                include_mixer: !no_mixer,
                player_model,
                mixer_model,
                initial_bpm_hundredths: bpm_to_hundredths(bpm),
                autoplay,
                tracks,
                beat_grid_offset_ms: beat_offset_ms,
                library,
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
            let cfg = FleetConfig {
                iface,
                num_players: device_number.clamp(1, 4),
                include_mixer: false,
                player_model: model,
                mixer_model: "DJM-V10".to_string(),
                initial_bpm_hundredths: 12000,
                autoplay: false,
                tracks: Vec::new(),
                beat_grid_offset_ms: 0,
                library: None,
            };
            Fleet::new(cfg).run().await?;
        }
    }
    Ok(())
}
