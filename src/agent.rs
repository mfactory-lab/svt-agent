use crate::constants::*;
use crate::listener::Listener;
use crate::messenger::*;
use crate::task_runner::{RunState, Task, TaskRunner, TaskRunnerOpts};
use crate::utils::convert_message_to_task;
use crate::AgentArgs;

use anchor_client::solana_client::nonblocking::pubsub_client::{PubsubClient, PubsubClientResult};
use anchor_client::solana_client::rpc_config::RpcTransactionLogsFilter;
use anchor_client::solana_sdk::signature::read_keypair_file;

use crate::notifier::NotifierOpts;
use anyhow::{Error, Result};
use futures::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, RwLock};
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{debug, error, info, warn};

pub struct Agent<'a> {
    args: &'a AgentArgs,
    /// Messenger client instance
    client: Arc<MessengerClient>,
    /// Task runner instance
    runner: Arc<RwLock<TaskRunner>>,
}

impl<'a> Agent<'a> {
    #[tracing::instrument]
    pub fn new(args: &'a AgentArgs) -> Result<Self> {
        let keypair = read_keypair_file(&args.keypair).expect("Authority keypair file not found");
        let runner = TaskRunner::new(TaskRunnerOpts::new(args));
        let client = Arc::new(MessengerClient::new(
            args.cluster.clone(),
            args.program_id,
            keypair,
        ));

        Ok(Self {
            args,
            client,
            runner: Arc::new(RwLock::new(runner)),
        })
    }

    #[tracing::instrument(skip_all)]
    pub async fn run(&self) -> Result<()> {
        info!("Agent Version: {}", env!("CARGO_PKG_VERSION"));
        info!("Agent ID: {:?}", self.client.authority_pubkey().to_string());
        info!("Program ID: {:?}", self.args.program_id);
        info!("Channel ID: {:?}", self.args.channel_id);
        info!("Cluster: {}", self.client.cluster);

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

        let (rx_new, tx_new) = mpsc::unbounded_channel::<NewMessageEvent>();
        let (rx_update, tx_update) = mpsc::unbounded_channel::<UpdateMessageEvent>();
        let (rx_delete, tx_delete) = mpsc::unbounded_channel::<DeleteMessageEvent>();

        let (_, _, _) = tokio::join!(
            self.command_runner(),
            self.command_listener(rx_new, rx_update, rx_delete),
            self.command_processor(tx_new, tx_update, tx_delete, cek),
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
        let membership = self.client.load_membership(&self.args.channel_id).await;

        check_balance(&self.client).await?;

        match membership {
            Ok(membership) => {
                if membership.status == ChannelMembershipStatus::Authorized {
                    let cek = self.client.load_cek(&self.args.channel_id).await?;

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
                warn!("{}", e);
                self.client
                    .join_channel(&self.args.channel_id, Some(AGENT_NAME.to_string()))
                    .await?;
                Err(e)
            }
        }
    }

    /// Load commands from the `channel` account.
    /// Decrypt, convert to the [Task] and filter by `id_from`.
    #[tracing::instrument(skip(self, cek))]
    async fn load_commands(&self, cek: &[u8], id_from: u64) -> Result<Vec<Task>> {
        info!("Loading channel...");
        let channel = self.client.load_channel(&self.args.channel_id).await?;

        info!("Prepare commands...");

        let mut tasks = vec![];
        let mut need_reset = true;

        let curr_task_id = match self.runner.read().await.current_state() {
            RunState::Processing(t) | RunState::Complete(t) | RunState::Error(t) => {
                info!("Current task id: {}", t.id);
                t.id
            }
            _ => 0,
        };

        for message in channel.messages {
            let msg_id = message.id;
            if msg_id > id_from {
                if let Ok(task) = convert_message_to_task(message, cek) {
                    // check that the current task is valid
                    if task.id == curr_task_id {
                        need_reset = task.is_skipped();
                    }
                    if !task.is_skipped() {
                        tasks.push(task);
                    }
                } else {
                    warn!("Failed to convert message to task... Message#{}", msg_id);
                }
            }
        }

        if need_reset {
            info!("Reset current run state");
            if let Err(e) = self.runner.write().await.reset_state() {
                warn!("Failed to reset run state... {}", e);
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
            let channel_id = self.args.channel_id;

            async move {
                loop {
                    match check_balance(&client).await {
                        Ok(balance) => {
                            info!("Balance: {} lamports", balance);
                        }
                        Err(e) => {
                            warn!("{e}");
                            time::sleep(Duration::from_millis(WAIT_BALANCE_INTERVAL)).await;
                            continue;
                        }
                    }

                    let mut runner = runner.write().await;

                    match runner.current_state() {
                        RunState::Processing(task) | RunState::Error(task) => {
                            // println!("... {:#?}", task);
                            info!(
                                "Previous Task #{} was failed, waiting for manual action...",
                                task.id
                            );
                            time::sleep(Duration::from_millis(WAIT_ACTION_INTERVAL)).await;
                        }
                        // previous task was complete, try to confirm it
                        RunState::Complete(task) => {
                            info!("Confirming Task #{}...", task.id);
                            match client.read_message(task.id, &channel_id).await {
                                Ok(sig) => {
                                    info!("Confirmed Task #{}! Signature: {}", task.id, sig);
                                    // reset state only if the command is confirmed
                                    let _ = runner.reset_state();
                                }
                                Err(e) => {
                                    error!("Failed to confirm... Error: {}", e);
                                    time::sleep(Duration::from_millis(3000)).await;
                                }
                            }
                        }
                        // run the next task only if current status is pending
                        RunState::Pending => match runner.run().await {
                            Ok(s) => {
                                // state is not changed after run, wait a new task...
                                if s == RunState::Pending {
                                    info!("Waiting a command...");
                                    time::sleep(Duration::from_millis(WAIT_COMMAND_INTERVAL)).await;
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

    #[tracing::instrument(skip_all)]
    fn command_processor(
        &self,
        mut new_receiver: mpsc::UnboundedReceiver<NewMessageEvent>,
        mut update_receiver: mpsc::UnboundedReceiver<UpdateMessageEvent>,
        mut delete_receiver: mpsc::UnboundedReceiver<DeleteMessageEvent>,
        cek: Vec<u8>,
    ) -> JoinHandle<()> {
        tokio::spawn({
            let runner = self.runner.clone();

            async move {
                loop {
                    tokio::select! {
                        Some(e) = new_receiver.recv() => {
                            match convert_message_to_task(e.message, cek.as_slice()) {
                                Ok(task) => {
                                    if !task.is_skipped() {
                                        runner.write().await.add_task(task);
                                    }
                                }
                                Err(_) => {
                                    info!("Failed to convert message to task");
                                }
                            }
                        }
                        Some(e) = update_receiver.recv() => {
                            let state = runner.read().await.current_state();
                            match state {
                                RunState::Error(task) | RunState::Processing(task) => {
                                    if task.id == e.message.id && task.is_skipped() {
                                        info!("Task #{} was skipped...", task.id);
                                        let _ = runner.write().await.reset_state();
                                    }
                                },
                                _ => {}
                            }
                        }
                        Some(e) = delete_receiver.recv() => {
                            let state = runner.read().await.current_state();
                            match state {
                                RunState::Error(task) | RunState::Processing(task) => {
                                    if task.id == e.id {
                                        info!("Task #{} was deleted...", task.id);
                                        let _ = runner.write().await.reset_state();
                                    }
                                },
                                _ => {
                                    runner.write().await.delete_task(e.id);
                                }
                            }
                        }
                    }
                }
            }
        })
    }

    /// Listen for [NewMessageEvent] events and send them to the `sender`.
    #[tracing::instrument(skip_all)]
    fn command_listener(
        &self,
        new_msg_sender: mpsc::UnboundedSender<NewMessageEvent>,
        update_msg_sender: mpsc::UnboundedSender<UpdateMessageEvent>,
        delete_msg_sender: mpsc::UnboundedSender<DeleteMessageEvent>,
    ) -> JoinHandle<()> {
        tokio::spawn({
            let url = self.client.cluster.ws_url().to_owned();
            let program_id = self.args.program_id;
            let filter = RpcTransactionLogsFilter::Mentions(vec![self.args.channel_id.to_string()]);

            async move {
                loop {
                    info!("Connecting to `{}`...", url);

                    match PubsubClient::new(url.as_str()).await {
                        Ok(client) => {
                            info!("Waiting a command...");

                            let listener = Listener::new(&program_id, &client, &filter);

                            match listener.log_stream().await {
                                Ok((mut stream, _)) => {
                                    while let Some(log) = stream.next().await {
                                        listener
                                            .on::<NewMessageEvent>(&log, &|evt| {
                                                debug!("{:?}", evt);
                                                if let Err(e) = new_msg_sender.send(evt) {
                                                    warn!(
                                                        "[NewMessageEvent] Failed to send... {}",
                                                        e
                                                    );
                                                }
                                            })
                                            .on::<DeleteMessageEvent>(&log, &|evt| {
                                                debug!("{:?}", evt);
                                                if let Err(e) = delete_msg_sender.send(evt) {
                                                    warn!(
                                                        "[DeleteMessageEvent] Failed to send... {}",
                                                        e
                                                    );
                                                }
                                            })
                                            .on::<UpdateMessageEvent>(&log, &|evt| {
                                                debug!("{:?}", evt);
                                                if let Err(e) = update_msg_sender.send(evt) {
                                                    warn!(
                                                        "[UpdateMessageEvent] Failed to send... {}",
                                                        e
                                                    );
                                                }
                                            });
                                    }
                                }
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
                        Err(e) => {
                            warn!("{}", e);
                            time::sleep(Duration::from_millis(3000)).await;
                        }
                    }
                }
            }
        })
    }
}

async fn check_balance(client: &MessengerClient) -> Result<u64> {
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

        let agent = Agent::new(&AgentArgs {
            keypair,
            cluster: Cluster::Devnet,
            channel_id: DEFAULT_CHANNEL_ID.parse().unwrap(),
            program_id: MESSENGER_PROGRAM_ID.parse().unwrap(),
            ..Default::default()
        })
        .unwrap();

        // let channel = agent.load_channel().await.unwrap();
        // let commands = agent.load_commands().await.unwrap();
        // println!("{:?}", commands);
    }
}
