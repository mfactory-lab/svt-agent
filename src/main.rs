#![feature(fn_traits)]

mod constants;
mod encryption;
mod listener;
mod monitor;
mod notifier;
mod runner;
mod state;
mod utils;

use crate::listener::Listener;
use crate::runner::{Task, TaskRunner};
use crate::utils::{convert_message_to_task, messenger::MessengerClient};
use anchor_client::solana_client::nonblocking::pubsub_client::PubsubClient;
use anchor_client::solana_client::rpc_config::RpcTransactionLogsFilter;
use anchor_client::solana_sdk::pubkey::Pubkey;
use anchor_client::solana_sdk::signature::read_keypair_file;
use anchor_client::Cluster;
use anyhow::{Error, Result};
use clap::Parser;
use constants::*;
use state::*;
use std::fmt::Debug;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, RwLock};
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{error, info, warn};
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
    #[arg(long, value_name = "CLUSTER", default_value = "d")]
    cluster: Cluster,
    #[arg(short, long, value_name = "CHANNEL", default_value = DEFAULT_CHANNEL_ID)]
    channel_id: String,
    #[arg(short, long, value_name = "MESSENGER_PROGRAM", default_value = MESSENGER_PROGRAM_ID)]
    messenger_program_id: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    // Initialize tracing
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
    /// Messenger program id
    program_id: Pubkey,
    /// The [Channel] identifier
    channel_id: Pubkey,
    /// Messenger client instance
    client: Arc<MessengerClient>,
    /// Task runner instance
    runner: Arc<RwLock<TaskRunner>>,
}

impl Agent {
    #[tracing::instrument]
    fn new(cli: Cli) -> Result<Self> {
        let keypair = read_keypair_file(cli.keypair).expect("Authority keypair required");
        let program_id = Pubkey::from_str(&cli.messenger_program_id)?;
        let channel_id = Pubkey::from_str(&cli.channel_id)?;

        let runner = Arc::new(RwLock::new(TaskRunner::new()));
        let client = Arc::new(MessengerClient::new(cli.cluster, program_id, keypair));

        Ok(Self {
            program_id,
            channel_id,
            runner,
            client,
        })
    }

    #[tracing::instrument(skip(self))]
    async fn run(&self) -> Result<()> {
        info!("Program ID {:?}", self.program_id);
        info!("Channel ID {:?}", self.channel_id);

        let cek = loop {
            match self.prepare().await {
                Ok(cek) => break cek,
                Err(e) => {
                    info!("{}", e);
                    time::sleep(Duration::from_millis(WAIT_AUTHORIZATION_INTERVAL)).await;
                }
            }
        };

        let (sender, receiver) = mpsc::unbounded_channel::<NewMessageEvent>();

        let (_, _, _) = tokio::join!(
            self.command_runner(),
            self.command_listener(sender),
            self.command_processor(receiver, cek),
        );

        Ok(())
    }

    /// This method try to load the [ChannelMembership] account.
    /// If the membership doesn't exists, a `join request` will be sent.
    /// If membership status is authorized, try to load [ChannelDevice] and return the `CEK`.
    async fn prepare(&self) -> Result<Vec<u8>> {
        let membership = self.client.load_membership(&self.channel_id).await;

        match membership {
            Ok(membership) => {
                if membership.status == ChannelMembershipStatus::Authorized {
                    let cek = self.client.load_cek(&self.channel_id).await?;

                    let initial_commands = self
                        .load_commands(cek.as_slice(), membership.last_read_message_id)
                        .await?;

                    info!(
                        "Added {} initial commands to the queue...",
                        initial_commands.len()
                    );

                    for command in initial_commands {
                        self.runner.write().await.add_task(command);
                    }

                    return Ok(cek);
                }
                Err(Error::msg("Awaiting authorization..."))
            }
            Err(e) => {
                error!("[prepare] Error: {}", e);
                self.client.join_channel(&self.channel_id).await?;
                Err(e)
            }
        }
    }

    /// Load commands from the `channel` decrypt, convert to the [Task] and filter by `id_from`.
    #[tracing::instrument(skip(self, cek))]
    async fn load_commands(&self, cek: &[u8], id_from: u64) -> Result<Vec<Task>> {
        info!("Loading channel...");
        let channel = self.client.load_channel(&self.channel_id).await?;
        info!("Prepare commands...");
        let mut commands = vec![];
        for message in channel.messages {
            if message.id > id_from {
                if let Ok(cmd) = convert_message_to_task(message, cek) {
                    commands.push(cmd);
                }
            }
        }
        Ok(commands)
    }

    /// This method waits and runs commands from the runner queue at some interval.
    /// TODO: retry on fail, read_message on fail
    fn command_runner(&self) -> JoinHandle<()> {
        tokio::spawn({
            let runner = self.runner.clone();
            let client = self.client.clone();
            let channel_id = self.channel_id;

            async move {
                loop {
                    match runner.write().await.run().await {
                        Ok(maybe_task) => {
                            if let Some(task) = maybe_task {
                                info!("[command_runner] Confirming Task#{}...", task.id);
                                match client.read_message(task.id, &channel_id).await {
                                    Ok(sig) => {
                                        info!(
                                            "[command_runner] Confirmed Task#{}! Signature: {}",
                                            task.id, sig
                                        );
                                    }
                                    Err(e) => {
                                        error!("[command_runner][read_message] Error: {}", e);
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            error!("[command_runner] Error: {}", e)
                        }
                    };
                    info!("[command_runner] Waiting a command...");
                    time::sleep(Duration::from_millis(COMMAND_POLL_INTERVAL)).await;
                }
            }
        })
    }

    /// Process [NewMessageEvent] events convert to the [Task] and add to the runner queue.
    fn command_processor(
        &self,
        mut receiver: mpsc::UnboundedReceiver<NewMessageEvent>,
        cek: Vec<u8>,
    ) -> JoinHandle<()> {
        tokio::spawn({
            let runner = self.runner.clone();
            async move {
                while let Some(e) = receiver.recv().await {
                    match convert_message_to_task(e.message, cek.as_slice()) {
                        Ok(task) => {
                            runner.write().await.add_task(task);
                        }
                        Err(_) => {
                            info!("[command_processor] Failed to convert message to task");
                        }
                    }
                }
            }
        })
    }

    /// Listen for [NewMessageEvent] events and send them to the `sender`.
    fn command_listener(&self, sender: mpsc::UnboundedSender<NewMessageEvent>) -> JoinHandle<()> {
        tokio::spawn({
            let url = self.client.cluster.ws_url().to_owned();
            let program_id = self.program_id;
            let filter = RpcTransactionLogsFilter::Mentions(vec![self.channel_id.to_string()]);
            async move {
                loop {
                    info!("Connecting to `{}`...", url);
                    let client = PubsubClient::new(url.as_str())
                        .await
                        .expect("Failed to init `PubsubClient`");

                    info!("[command_listener] Waiting a command...");

                    let listener = Listener::new(&program_id, &client, &filter);
                    match listener
                        .on::<NewMessageEvent>(|e| {
                            info!("[command_listener] {:?}", e);
                            match sender.send(e) {
                                Ok(_) => {}
                                Err(e) => {
                                    warn!("Failed to send event... {}", e);
                                }
                            }
                        })
                        .await
                    {
                        Ok(_) => {}
                        Err(e) => {
                            error!("[command_listener] Error: {:?}", e);
                        }
                    }
                    match &client.shutdown().await {
                        Ok(_) => {}
                        Err(e) => {
                            info!("[command_listener] Failed to shutdown client ({})", e);
                        }
                    }
                    info!("[command_listener] Reconnecting...");
                    time::sleep(Duration::from_millis(3000)).await;
                }
            }
        })
    }
}

#[test]
fn test_channel() {
    let mut _data: &[u8] = &[
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

    // let ch = Channel::deserialize(&mut data).unwrap();
    // println!("{:?}", ch);
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
