mod constants;
mod encryption;
mod listener;
mod monitor;
mod notifier;
mod runner;
mod state;
mod utils;

use crate::listener::Listener;
use crate::runner::{RunState, Task, TaskRunner};
use crate::utils::{convert_message_to_task, MessengerClient};
use anchor_client::solana_client::nonblocking::pubsub_client::PubsubClient;
use anchor_client::solana_client::rpc_config::RpcTransactionLogsFilter;
use anchor_client::solana_sdk::pubkey::Pubkey;
use anchor_client::solana_sdk::signature::{read_keypair_file, Keypair};
use anchor_client::Cluster;
use anyhow::{Error, Result};
use clap::{Args, Parser, Subcommand};
use constants::*;
use rand::rngs::OsRng;
use state::*;
use std::fmt::Debug;
use std::path::PathBuf;
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
#[command(name = env!("CARGO_PKG_NAME"))]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = env!("CARGO_PKG_DESCRIPTION"), long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    GenerateKeypair,
    CheckChannel {
        #[arg(long, value_name = "CLUSTER", default_value = "devnet")]
        cluster: Cluster,
        #[arg(short, long, value_name = "CHANNEL")]
        channel_id: Pubkey,
        #[arg(short, long, value_name = "MESSENGER_PROGRAM", default_value = MESSENGER_PROGRAM_ID)]
        program_id: Pubkey,
    },
    Run(RunArgs),
}

#[derive(Args, Debug)]
struct RunArgs {
    #[arg(
        long,
        short,
        value_name = "KEYPAIR",
        default_value = "/app/keypair.json"
    )]
    keypair: PathBuf,
    #[arg(long, value_name = "CLUSTER", default_value = "devnet")]
    cluster: Cluster,
    #[arg(short, long, value_name = "CHANNEL", default_value = DEFAULT_CHANNEL_ID)]
    channel_id: Pubkey,
    #[arg(short, long, value_name = "MESSENGER_PROGRAM", default_value = MESSENGER_PROGRAM_ID)]
    program_id: Pubkey,
    #[arg(short, long, value_name = "WORKING_DIR")]
    working_dir: Option<PathBuf>,
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
            println!("{}", if channel.is_ok() { "OK" } else { "ERROR" })
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
    fn new(args: &RunArgs) -> Result<Self> {
        let keypair = read_keypair_file(&args.keypair).expect("Authority keypair required");
        let program_id = args.program_id;
        let channel_id = args.channel_id;

        let mut runner = TaskRunner::new();

        if let Some(working_dir) = &args.working_dir {
            runner.with_working_dir(working_dir.into());
        }

        let client = Arc::new(MessengerClient::new(
            args.cluster.clone(),
            args.program_id,
            keypair,
        ));

        Ok(Self {
            program_id,
            channel_id,
            runner: Arc::new(RwLock::new(runner)),
            client,
        })
    }

    #[tracing::instrument(skip_all)]
    async fn run(&self) -> Result<()> {
        info!("Cluster {:?}", self.client.cluster);
        info!("Agent ID {:?}", self.client.authority_pubkey().to_string());
        info!("Program ID {:?}", self.program_id);
        info!("Channel ID {:?}", self.channel_id);

        for (key, value) in std::env::vars() {
            println!("ENV: {key}: {value}");
        }

        self.init().await?;

        // Trying to access a channel
        let cek = loop {
            match self.authorize().await {
                Ok(cek) => break cek,
                Err(e) => {
                    warn!("Error: {}", e);
                    time::sleep(Duration::from_millis(WAIT_AUTHORIZATION_INTERVAL)).await;
                }
            }
        };

        // info!("CEK: {:?}", cek);

        // The channel that handle `NewMessageEvent` events
        let (sender, receiver) = mpsc::unbounded_channel::<NewMessageEvent>();

        let (_, _, _) = tokio::join!(
            self.command_runner(),
            self.command_listener(sender),
            self.command_processor(receiver, cek),
        );

        Ok(())
    }

    #[tracing::instrument(skip_all)]
    async fn init(&self) -> Result<()> {
        self.runner.write().await.init().await?;
        Ok(())
    }

    /// This method try to load the [ChannelMembership] account.
    /// If the membership doesn't exists, a `join request` will be sent.
    /// If membership status is authorized, try to load [ChannelDevice] and return the `CEK`.
    #[tracing::instrument(skip_all)]
    async fn authorize(&self) -> Result<Vec<u8>> {
        let membership = self.client.load_membership(&self.channel_id).await;

        check_min_balance(&self.client).await?;

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
                error!("Error: {}", e);
                self.client.join_channel(&self.channel_id).await?;
                Err(e)
            }
        }
    }

    /// Load commands from the `channel` account.
    /// Decrypt, convert to the [Task] and filter by `id_from`.
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
    #[tracing::instrument(skip_all)]
    fn command_runner(&self) -> JoinHandle<()> {
        tokio::spawn({
            let runner = self.runner.clone();
            let client = self.client.clone();
            let channel_id = self.channel_id;

            async move {
                loop {
                    match check_min_balance(&client).await {
                        Ok(balance) => {
                            info!("Balance: {} lamports", balance);
                        }
                        Err(e) => {
                            warn!("{e}");
                            time::sleep(Duration::from_millis(COMMAND_POLL_INTERVAL)).await;
                            continue;
                        }
                    }

                    let mut runner = runner.write().await;

                    match runner.run().await {
                        Ok(res) => match res {
                            RunState::Complete(task_id) => {
                                info!("Confirming Task#{}...", task_id);
                                match client.read_message(task_id, &channel_id).await {
                                    Ok(sig) => {
                                        info!("Confirmed Task#{}! Signature: {}", task_id, sig);
                                        // reset state only if the command is confirmed
                                        runner.reset_state();
                                    }
                                    Err(e) => {
                                        error!("ReadMessage Error: {}", e);
                                    }
                                }
                            }
                            RunState::Error(_) => {
                                // probably internal docker error
                                // TODO: is it need to be confirmed ?
                                runner.reset_state();
                            }
                            _ => {}
                        },
                        Err(e) => {
                            error!("Error: {}", e)
                        }
                    };

                    info!("Waiting a command...");
                    time::sleep(Duration::from_millis(COMMAND_POLL_INTERVAL)).await;
                }
            }
        })
    }

    /// Receives a [NewMessageEvent] from the [receiver],
    /// convert it to the [Task] and add to the runner queue.
    #[tracing::instrument(skip_all)]
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
                            info!("Failed to convert message to task");
                        }
                    }
                }
            }
        })
    }

    /// Listen for [NewMessageEvent] events and send them to the `sender`.
    #[tracing::instrument(skip_all)]
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

                    info!("Waiting a command...");

                    let listener = Listener::new(&program_id, &client, &filter);
                    match listener
                        .on::<NewMessageEvent>(|e| {
                            info!("{:?}", e);
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
                            error!("Error: {:?}", e);
                        }
                    }
                    match &client.shutdown().await {
                        Ok(_) => {}
                        Err(e) => {
                            info!("Failed to shutdown client ({})", e);
                        }
                    }
                    info!("Reconnecting...");
                    time::sleep(Duration::from_millis(3000)).await;
                }
            }
        })
    }
}

async fn check_min_balance(client: &MessengerClient) -> Result<u64> {
    let balance = client.get_balance().await;
    if balance < MIN_BALANCE_REQUIRED {
        return Err(Error::msg(format!(
            "Insufficient balance! Required minimum {} lamports.",
            MIN_BALANCE_REQUIRED
        )));
    }
    Ok(balance)
}

#[tokio::test]
async fn test_agent_init() {
    let keypair = PathBuf::from("./keypair.json");

    let agent = Agent::new(&RunArgs {
        keypair,
        cluster: Cluster::Devnet,
        channel_id: DEFAULT_CHANNEL_ID.parse().unwrap(),
        program_id: MESSENGER_PROGRAM_ID.parse().unwrap(),
        working_dir: None,
    })
    .unwrap();

    // let channel = agent.load_channel().await.unwrap();
    // let commands = agent.load_commands().await.unwrap();
    // println!("{:?}", commands);
}
