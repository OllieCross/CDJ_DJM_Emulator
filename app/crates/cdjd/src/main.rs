//! `cdjd` - development CLI for the CDJ emulator.

use anyhow::{Context, Result};
use cdj_core::feth::{setup_plan, FethPlan, Layout};
use cdj_core::media::{Medium, Slot};
use cdj_core::models::{MixerModel, PlayerModel};
use cdj_core::net::Interface;
use cdj_core::rig::{Rig, RigConfig};
use cdj_proto::announce::MIXER_NUM;
use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(name = "cdjd", about = "CDJ emulator daemon (dev CLI)", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum LayoutArg {
    /// Device addresses on feth0, same /24 as feth1 (known to work).
    Shared,
    /// Device addresses as /32 aliases on lo0; feth0 has no IPv4 (experimental).
    Loopback,
}

/// The feth pair the emulator runs on.
#[derive(Args, Debug)]
struct NetArgs {
    /// Emulator-side interface.
    #[arg(long = "iface", default_value = "feth0")]
    a: String,
    /// Client-side interface (ShowKontrol / Beat Link Trigger use it).
    #[arg(long = "client-iface", default_value = "feth1")]
    b: String,
    /// Address of player 1; the other devices use the same /24.
    #[arg(long, default_value = "10.77.77.1")]
    a_ip: String,
    /// Client-side address; status and position packets are sent here too.
    #[arg(long = "client-ip", default_value = "10.77.77.200")]
    b_ip: String,
    #[arg(long, default_value_t = 24)]
    prefix: u8,
    #[arg(long, value_enum, default_value_t = LayoutArg::Shared)]
    layout: LayoutArg,
}

impl NetArgs {
    fn plan(&self) -> FethPlan {
        let layout = match self.layout {
            LayoutArg::Shared => Layout::SharedSubnet,
            LayoutArg::Loopback => Layout::LoopbackAliases,
        };
        setup_plan(&self.a, &self.b, &self.a_ip, &self.b_ip, self.prefix).with_layout(layout)
    }
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// List IPv4 network interfaces.
    Ifaces,

    /// Print the shell commands that create the feth pair.
    FethPlan {
        #[command(flatten)]
        net: NetArgs,
        #[arg(long)]
        teardown: bool,
    },

    /// Create the feth pair (asks for an administrator password).
    NetUp {
        #[command(flatten)]
        net: NetArgs,
    },

    /// Remove the feth pair (asks for an administrator password).
    NetDown {
        #[command(flatten)]
        net: NetArgs,
    },

    /// Run a full rig: N virtual CDJs + (optionally) a virtual DJM.
    RunFleet {
        #[command(flatten)]
        net: NetArgs,
        #[arg(long, default_value_t = 4)]
        players: u8,
        /// Omit the virtual DJM.
        #[arg(long)]
        no_mixer: bool,
        /// Player models, one per player (the last repeats): e.g.
        /// CDJ-3000X,CDJ-3000X,CDJ-2000NXS2. See `cdjd models`.
        #[arg(long, default_value = "CDJ-3000", value_delimiter = ',', value_parser = player_model)]
        player_model: Vec<PlayerModel>,
        #[arg(long, default_value = "DJM-V10", value_parser = mixer_model)]
        mixer_model: MixerModel,
        /// Initial tempo in BPM (e.g. 128.0).
        #[arg(long, default_value_t = 120.0)]
        bpm: f32,
        /// Start every player playing so they emit beats immediately
        /// (timecode-only mode).
        #[arg(long)]
        autoplay: bool,
        /// Audio files, one per player (up to 4): --track a.flac --track b.mp3.
        #[arg(long = "track")]
        tracks: Vec<std::path::PathBuf>,
        /// Offset from playback start to the first beat of bar 1, in ms.
        #[arg(long, default_value_t = 0)]
        beat_offset_ms: u32,
        /// Audio output device (see `cdjd audio-devices`); default: the
        /// system output.
        #[arg(long)]
        audio_device: Option<String>,
        /// Don't play audio (beats and positions still run).
        #[arg(long)]
        silent: bool,
        /// Rekordbox export root (e.g. /Volumes/MY_USB). It goes into player
        /// 1's USB slot; players 1..N load its first N tracks (2..N over the
        /// link).
        #[arg(long)]
        library: Option<std::path::PathBuf>,
    },

    /// List audio output devices.
    AudioDevices,

    /// Run a single virtual CDJ.
    Run {
        #[command(flatten)]
        net: NetArgs,
        #[arg(short = 'n', long, default_value_t = 1)]
        device_number: u8,
        #[arg(short, long, default_value = "CDJ-3000", value_parser = player_model)]
        model: PlayerModel,
    },

    /// List the player and mixer models the emulator can be.
    Models,
}

fn player_model(name: &str) -> Result<PlayerModel, String> {
    PlayerModel::from_name(name).ok_or_else(|| {
        format!("unknown player model; choose from {}", PlayerModel::ALL.map(|m| m.display_name()).join(", "))
    })
}

fn mixer_model(name: &str) -> Result<MixerModel, String> {
    MixerModel::from_name(name).ok_or_else(|| {
        format!("unknown mixer model; choose from {}", MixerModel::ALL.map(|m| m.device_name()).join(", "))
    })
}

fn bpm_to_hundredths(bpm: f32) -> u16 {
    (bpm * 100.0).round().clamp(0.0, u16::MAX as f32) as u16
}

fn rig_config(net: &NetArgs) -> Result<RigConfig> {
    let plan = net.plan();
    let status = plan.status();
    if !status.ready() {
        anyhow::bail!(
            "network not ready ({status:?}); run `cdjd net-up` first, or `cdjd feth-plan` to see the commands"
        );
    }
    let (topology, addressing) = plan.rig_network()?;
    Ok(RigConfig {
        topology,
        addressing,
        initial_bpm_hundredths: 12000,
        beat_grid_offset_ms: 0,
        listen_ports: Some(Default::default()),
    })
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
        Cmd::FethPlan { net, teardown } => {
            let plan = net.plan();
            println!("# Bring up a feth pair for the emulator. Requires sudo.");
            for c in plan.commands() {
                println!("sudo sh -c '{c}'");
            }
            if teardown {
                println!("\n# Teardown:");
                for c in plan.teardown_commands() {
                    println!("sudo sh -c '{c}'");
                }
            }
        }
        Cmd::NetUp { net } => {
            let plan = net.plan();
            plan.apply()?;
            println!("network up: {:?}", plan.status());
        }
        Cmd::NetDown { net } => {
            net.plan().teardown()?;
            println!("network removed");
        }
        Cmd::RunFleet {
            net,
            players,
            no_mixer,
            player_model,
            mixer_model,
            bpm,
            autoplay,
            tracks,
            beat_offset_ms,
            audio_device,
            silent,
            library,
        } => {
            let mut cfg = rig_config(&net)?;
            cfg.initial_bpm_hundredths = bpm_to_hundredths(bpm);
            cfg.beat_grid_offset_ms = beat_offset_ms;

            let rig = Rig::new(cfg);
            if !silent {
                match rig.set_output_device(audio_device.as_deref()) {
                    Ok(name) => println!("audio out: {name}"),
                    Err(e) => eprintln!("no audio output ({e:#}); running silently"),
                }
            }
            for n in 1..=players.min(4) {
                let model = player_model
                    .get(usize::from(n - 1))
                    .or(player_model.last())
                    .copied()
                    .unwrap_or(PlayerModel::Cdj3000);
                rig.spawn_player(n, model).await?;
            }
            if !no_mixer {
                rig.spawn_mixer(mixer_model).await?;
            }
            if let Some(root) = library {
                let medium = tokio::task::spawn_blocking(move || Medium::open(&root))
                    .await
                    .context("library loader panicked")?
                    .context("loading rekordbox export")?;
                rig.insert_media(1, Slot::Usb, medium.clone())?;
                for (n, track) in (1..=players.min(4)).zip(&medium.library.tracks) {
                    rig.load_track(n, 1, Slot::Usb, track.id).await?;
                }
            }
            for (n, path) in (1..=players.min(4)).zip(tracks) {
                rig.load_file(n, path).await?;
            }
            if autoplay {
                for n in 1..=players.min(4) {
                    rig.play(n)?;
                }
            }
            println!("rig running on {} (device {}..{}, mixer {MIXER_NUM}); Ctrl-C to stop", net.a, 1, players.min(4));
            tokio::signal::ctrl_c().await?;
            rig.shutdown();
        }
        Cmd::Models => {
            println!("Players:");
            for m in PlayerModel::ALL {
                println!("  {:<14} {}", m.display_name(), m.notes());
            }
            println!("Mixers:");
            for m in MixerModel::ALL {
                println!("  {:<14} {}", m.device_name(), m.notes());
            }
        }
        Cmd::AudioDevices => {
            for name in cdj_core::engine::AudioEngine::output_devices() {
                println!("{name}");
            }
        }
        Cmd::Run {
            net,
            device_number,
            model,
        } => {
            let rig = Rig::new(rig_config(&net)?);
            rig.spawn_player(device_number, model).await?;
            println!("player {device_number} running; Ctrl-C to stop");
            tokio::signal::ctrl_c().await?;
            rig.shutdown();
        }
    }
    Ok(())
}
