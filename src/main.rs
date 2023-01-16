#![allow(unused)]

mod agent;
mod constants;
mod listener;
mod messenger;
mod monitor;
mod notifier;
mod task_runner;
mod utils;

use crate::agent::Agent;
use crate::constants::*;

use anchor_client::solana_sdk::pubkey::Pubkey;
use anchor_client::solana_sdk::signature::{read_keypair_file, Keypair, Signer};
use anchor_client::Cluster;
use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use messenger::MessengerClient;
use rand::rngs::OsRng;
use std::path::PathBuf;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Registry};
use tracing_tree::HierarchicalLayer;

#[derive(Parser, Debug)]
#[command(name = env!("CARGO_PKG_NAME"))]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = env!("CARGO_PKG_DESCRIPTION"), long_about = None)]
struct Cli {
    #[command(subcommand)]
    pub(crate) command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    GenerateKeypair,
    ShowPubkey {
        #[arg(
            long,
            short,
            value_name = "KEYPAIR",
            env = "AGENT_KEYPAIR",
            default_value = "/app/keypair.json"
        )]
        keypair: PathBuf,
    },
    CheckChannel {
        #[arg(long, value_name = "CLUSTER", default_value = "devnet")]
        cluster: Cluster,
        #[arg(short, long, value_name = "CHANNEL")]
        channel_id: Pubkey,
        #[arg(short, long, value_name = "MESSENGER_PROGRAM", default_value = MESSENGER_PROGRAM_ID)]
        program_id: Pubkey,
    },
    Run(AgentArgs),
}

#[derive(Args, Debug, Default)]
pub struct AgentArgs {
    #[arg(
        long,
        short,
        value_name = "KEYPAIR",
        env = "AGENT_KEYPAIR",
        default_value = "/app/keypair.json"
    )]
    pub keypair: PathBuf,
    #[arg(
        long,
        value_name = "CLUSTER",
        env = "AGENT_CLUSTER",
        default_value = "devnet"
    )]
    pub cluster: Cluster,
    #[arg(short, long, value_name = "CHANNEL", env = "AGENT_CHANNEL_ID", default_value = DEFAULT_CHANNEL_ID)]
    pub channel_id: Pubkey,
    #[arg(short, long, value_name = "MESSENGER_PROGRAM", env = "AGENT_MESSENGER_PROGRAM", default_value = MESSENGER_PROGRAM_ID)]
    pub program_id: Pubkey,
    #[arg(short, long, value_name = "WORKING_DIR")]
    pub working_dir: Option<PathBuf>,
    #[arg(
        long,
        value_name = "MONITOR_PORT",
        env = "AGENT_MONITOR_PORT",
        default_value = DEFAULT_MONITOR_PORT
    )]
    pub monitor_port: u16,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match &cli.command {
        Commands::CheckChannel {
            channel_id,
            cluster,
            program_id,
        } => {
            let client = MessengerClient::new(cluster.clone(), *program_id, Keypair::new());
            let channel = client.load_channel(channel_id).await;
            println!("{}", if channel.is_ok() { "OK" } else { "INVALID" })
        }
        Commands::ShowPubkey { keypair } => {
            let keypair = read_keypair_file(keypair).expect("Authority keypair file not found");
            let pubkey = keypair.pubkey().to_string();
            println!("{pubkey}")
        }
        Commands::GenerateKeypair => {
            let kp = Keypair::generate(&mut OsRng);
            println!("{:?}", kp.to_bytes())
        }
        Commands::Run(args) => {
            Registry::default()
                .with(EnvFilter::from_default_env())
                .with(
                    HierarchicalLayer::new(2)
                        .with_targets(true)
                        .with_bracketed_fields(true),
                )
                // .with(
                //     tracing_subscriber::fmt::layer()
                //         .json()
                //         .with_writer(|| File::create("/var/log/agent-log.json").unwrap()),
                // )
                .init();
            Agent::new(args)?.run().await?;
        }
    }

    Ok(())
}
