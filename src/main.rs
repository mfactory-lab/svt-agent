#![feature(fn_traits)]

mod constants;
mod encryption;
mod listener;
mod notifier;
mod runner;
mod state;
mod utils;

use crate::encryption::decrypt_cek;
use crate::listener::Listener;
use crate::runner::{Task, TaskRunner};
use anchor_client::solana_client::nonblocking::pubsub_client::PubsubClient;
use anchor_client::solana_client::nonblocking::rpc_client::RpcClient;
use anchor_client::solana_sdk::commitment_config::CommitmentConfig;
use anchor_client::solana_sdk::pubkey::Pubkey;
use anchor_client::solana_sdk::signature::{read_keypair_file, Keypair, Signer};
use anchor_client::Cluster;
use anchor_lang::AnchorDeserialize;
use anyhow::Result;
use clap::Parser;
use constants::*;
use state::*;
use std::fmt::Debug;
use std::path::PathBuf;
use std::str::FromStr;
use tracing::info;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Registry};
use tracing_tree::HierarchicalLayer;

#[derive(Parser, Debug)]
#[command(name = "SVT Agent")]
#[command(version = "1.0")]
#[command(about = "SVT Agent", long_about = None)]
struct Cli {
    #[arg(short, long, value_name = "KEYPAIR")]
    keypair: PathBuf,
    #[arg(short, long, value_name = "CLUSTER", default_value = "d")]
    cluster: Cluster,
    #[arg(short, long, value_name = "CHANNEL", default_value = DEFAULT_CHANNEL_ID)]
    channel_id: String,
    #[arg(short, long, value_name = "MESSENGER_PROGRAM", default_value = MESSENGER_PROGRAM_ID)]
    messenger_program_id: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
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

    Agent::new(Cli::parse())?.run().await?;

    Ok(())
}

struct Agent {
    /// Keypair is used for decrypting messages from the channel
    keypair: Keypair,
    /// Messenger program id
    program_id: Pubkey,
    /// Channel identifier
    channel_id: Pubkey,
    /// Solana cluster
    cluster: Cluster,
    rpc: RpcClient,
}

impl Agent {
    #[tracing::instrument]
    fn new(cli: Cli) -> Result<Self> {
        let keypair = read_keypair_file(cli.keypair).expect("Keypair required");
        let program_id = Pubkey::from_str(&cli.messenger_program_id)?;
        let channel_id = Pubkey::from_str(&cli.channel_id)?;

        let rpc = RpcClient::new_with_commitment(
            cli.cluster.url().to_string(),
            CommitmentConfig::confirmed(),
        );

        Ok(Self {
            program_id,
            channel_id,
            keypair,
            cluster: cli.cluster,
            rpc,
        })
    }

    #[tracing::instrument(skip(self))]
    async fn run(&self) -> Result<()> {
        info!("Program ID {:?}", self.program_id);
        info!("Channel ID {:?}", self.channel_id);

        let mut runner = TaskRunner::new();

        let cek = self.load_cek().await?;
        let commands = self.load_commands(cek.as_slice()).await?;

        info!("Added {} commands to the runner...", commands.len());

        for command in commands {
            runner.add_task(command);
        }

        // tokio::spawn({
        //     async move {
        //         self.listen_commands();
        //     }
        // });

        runner.start().await;

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    async fn listen_commands(&self) -> Result<()> {
        let listener = Listener::new(&self.program_id);
        // Invalid Request: Only 1 address supported (-32602)
        let mentions = vec![self.channel_id.to_string()];
        let url = self.cluster.ws_url();
        loop {
            info!("Trying to connect `{}`...", url);
            let pubsub_client = PubsubClient::new(url).await?;
            listener.listen_commands(&pubsub_client, &mentions).await?;
            // listener.
            match pubsub_client.shutdown().await {
                Ok(_) => {}
                Err(e) => {
                    info!("Failed to disconnect: {}", e);
                }
            }
            info!("Reconnecting...");
            tokio::time::sleep(std::time::Duration::from_millis(3000)).await;
        }
    }

    #[tracing::instrument(skip(self))]
    async fn load_cek(&self) -> Result<Vec<u8>> {
        info!("Loading device...");
        let device = self.load_device().await?;
        info!("Decrypting CEK...");
        decrypt_cek(device.cek, self.keypair.secret().as_bytes())
    }

    #[tracing::instrument(skip(self))]
    async fn load_commands(&self, cek: &[u8]) -> Result<Vec<Task>> {
        info!("Loading membership...");
        let membership = self.load_membership().await?;

        info!("Loading channel...");
        let channel = self.load_channel().await?;

        info!("Prepare commands...");
        let mut commands = vec![];
        for message in channel.messages {
            if message.id > membership.last_read_message_id {
                commands.push(message.convert_to_task(cek)?);
            }
        }

        Ok(commands)
    }

    async fn load_membership(&self) -> Result<ChannelMembership> {
        let pda = self.get_membership_pda(&self.channel_id, &self.keypair.pubkey());
        self.load_account(&pda.0).await
    }

    async fn load_device(&self) -> Result<ChannelDevice> {
        let membership_pda = self.get_membership_pda(&self.channel_id, &self.keypair.pubkey());
        let pda = self.get_device_pda(&membership_pda.0, &self.keypair.pubkey());
        self.load_account(&pda.0).await
    }

    async fn load_channel(&self) -> Result<Channel> {
        self.load_account(&self.channel_id).await
    }

    async fn load_account<T: AnchorDeserialize>(&self, addr: &Pubkey) -> Result<T> {
        let data = self.rpc.get_account_data(addr).await?;
        // skip anchor discriminator
        let account = T::deserialize(&mut &data.as_slice()[8..])?;
        Ok(account)
    }

    fn get_membership_pda(&self, channel: &Pubkey, authority: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(
            &[&channel.to_bytes(), &authority.to_bytes()],
            &self.program_id,
        )
    }

    fn get_device_pda(&self, membership: &Pubkey, key: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(&[&membership.to_bytes(), &key.to_bytes()], &self.program_id)
    }
}

#[test]
fn test_channel() {
    let mut data: &[u8] = &[
        0, 0, 0, 0, 5, 0, 0, 0, 116, 101, 115, 116, 50, 212, 187, 201, 36, 193, 154, 106, 252, 42,
        57, 224, 82, 49, 66, 121, 115, 239, 73, 20, 146, 111, 168, 32, 147, 213, 73, 8, 203, 3,
        111, 154, 39, 45, 27, 137, 99, 0, 0, 0, 0, 129, 30, 137, 99, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0,
        0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 212, 187, 201, 36, 193, 154, 106,
        252, 42, 57, 224, 82, 49, 66, 121, 115, 239, 73, 20, 146, 111, 168, 32, 147, 213, 73, 8,
        203, 3, 111, 154, 39, 129, 30, 137, 99, 0, 0, 0, 0, 1, 60, 0, 0, 0, 48, 54, 114, 84, 69,
        79, 99, 102, 121, 48, 72, 86, 99, 83, 71, 119, 47, 105, 73, 86, 116, 75, 47, 88, 67, 69,
        49, 71, 115, 87, 118, 107, 56, 103, 98, 87, 78, 99, 84, 88, 109, 117, 113, 119, 50, 109,
        67, 55, 74, 55, 68, 103, 101, 100, 55, 107, 107, 119, 61, 61, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];

    let ch = Channel::deserialize(&mut data).unwrap();

    println!("{:?}", ch);
}

#[tokio::test]
async fn test() {
    let keypair = PathBuf::from("./keypair.json");

    let agent = Agent::new(Cli {
        keypair,
        cluster: Cluster::Devnet,
        channel_id: DEFAULT_CHANNEL_ID.to_string(),
        messenger_program_id: MESSENGER_PROGRAM_ID.to_string(),
    })
    .unwrap();

    // let channel = agent.load_channel().await.unwrap();
    // let commands = agent.load_commands().await.unwrap();
    // println!("{:?}", commands);
}
