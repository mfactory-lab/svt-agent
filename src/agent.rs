use crate::constants::*;
use crate::listener::Listener;
use crate::state::*;
use crate::task_runner::{RunState, Task, TaskRunner};
use crate::utils::{convert_message_to_task, MessengerClient};
use crate::RunArgs;

use anchor_client::solana_client::nonblocking::pubsub_client::PubsubClient;
use anchor_client::solana_client::rpc_config::RpcTransactionLogsFilter;
use anchor_client::solana_sdk::pubkey::Pubkey;
use anchor_client::solana_sdk::signature::read_keypair_file;
use anyhow::{Error, Result};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, RwLock};
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{error, info, warn};

pub struct Agent {
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
    pub fn new(args: &RunArgs) -> Result<Self> {
        let keypair = read_keypair_file(&args.keypair).expect("Authority keypair file not found");
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
    pub async fn run(&self) -> Result<()> {
        info!("Cluster: {:?}", self.client.cluster);
        info!("Agent ID: {:?}", self.client.authority_pubkey().to_string());
        info!("Program ID: {:?}", self.program_id);
        info!("Channel ID: {:?}", self.channel_id);

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
        let mut tasks = vec![];
        for message in channel.messages {
            if message.id > id_from {
                if let Ok(task) = convert_message_to_task(message, cek) {
                    // TODO: refactory
                    if task.action != "skip" {
                        tasks.push(task);
                    }
                }
            }
        }
        Ok(tasks)
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
                            time::sleep(Duration::from_millis(5000)).await;
                            continue;
                        }
                    }

                    let mut runner = runner.write().await;

                    // check previous task state
                    match runner.current_state() {
                        RunState::Processing(task) | RunState::Error(task) => {
                            println!("... {:#?}", task);
                            info!("Previous task was failed, wait for manual action...");
                            time::sleep(Duration::from_millis(5000)).await;
                        }
                        // previous task was complete, try to confirm it
                        RunState::Complete(task) => {
                            info!("Confirming Task #{}...", task.id);
                            match client.read_message(task.id, &channel_id).await {
                                Ok(sig) => {
                                    info!("Confirmed Task #{}! Signature: {}", task.id, sig);
                                    // reset state only if the command is confirmed
                                    runner.reset_state();
                                }
                                Err(e) => {
                                    error!("Failed to confirm... Error: {}", e);
                                }
                            }
                        }
                        RunState::Pending => match runner.run().await {
                            Ok(s) => {
                                if s == RunState::Pending {
                                    info!("Waiting a command...");
                                    time::sleep(Duration::from_millis(COMMAND_POLL_INTERVAL)).await;
                                }
                            }
                            Err(e) => {
                                error!("Error: {}", e)
                            }
                        },
                    }
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

#[cfg(test)]
mod test {
    use super::*;
    use anchor_client::Cluster;
    use std::path::PathBuf;

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
}
