use crate::constants::*;
use crate::listener::Listener;
use crate::messenger::*;
use crate::task_runner::{RunState, Task, TaskRunner, TaskRunnerOpts};
use crate::utils::convert_message_to_task;
use crate::AgentArgs;

use anchor_client::solana_client::nonblocking::pubsub_client::{PubsubClient, PubsubClientResult};
use anchor_client::solana_client::rpc_config::RpcTransactionLogsFilter;
use anchor_client::solana_sdk::signature::read_keypair_file;

use crate::notifier::{Notifier, NotifierOpts};
use anchor_client::solana_sdk::signer::Signer;
use anchor_lang::prelude::Pubkey;
use anchor_lang::Event;
use anyhow::{Error, Result};
use futures::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{debug, error, info, warn};

const LOGS_SUBSCRIBE_TIMEOUT: u64 = 5;

pub struct Agent {
    ctx: Arc<AgentContext>,
}

impl Agent {
    #[tracing::instrument]
    pub fn new(args: &AgentArgs) -> Result<Self> {
        let keypair = read_keypair_file(&args.keypair).expect("Authority keypair file not found");
        let runner = TaskRunner::new(TaskRunnerOpts::new(args));
        let client = MessengerClient::new(args.cluster.clone(), args.program_id, keypair);

        Ok(Self {
            ctx: Arc::new(AgentContext {
                client,
                channel_id: args.channel_id,
                last_task_id: Default::default(),
                cek: Default::default(),
                runner: Mutex::new(runner),
            }),
        })
    }

    fn print_info(&self) {
        info!("Agent Version: {}", env!("CARGO_PKG_VERSION"));
        info!("Agent ID: {:?}", self.ctx.client.authority_pubkey().to_string());
        info!("Cluster: {}", self.ctx.client.cluster);
        info!("Program ID: {:?}", self.ctx.client.program_id);
        info!("Channel ID: {:?}", self.ctx.channel_id);
    }

    #[tracing::instrument(skip_all)]
    pub async fn run(&mut self) -> Result<()> {
        self.print_info();

        self.ctx.init().await?;

        let (rx_new, tx_new) = mpsc::unbounded_channel::<NewMessageEvent>();
        let (rx_update, tx_update) = mpsc::unbounded_channel::<UpdateMessageEvent>();
        let (rx_delete, tx_delete) = mpsc::unbounded_channel::<DeleteMessageEvent>();

        let (_, _, _) = tokio::join!(
            self.command_runner(),
            self.command_listener(rx_new, rx_update, rx_delete),
            self.command_processor(tx_new, tx_update, tx_delete),
        );

        Ok(())
    }

    /// This method waits and runs commands from the runner queue at some interval.
    /// TODO: retry on fail, read_message on fail
    #[tracing::instrument(skip_all)]
    fn command_runner(&self) -> JoinHandle<()> {
        tokio::spawn({
            let ctx = self.ctx.clone();

            async move {
                let mut prev_state = RunState::Pending;
                loop {
                    match check_balance(&ctx.client).await {
                        Ok(balance) => {
                            debug!("Balance: {} lamports", balance);
                        }
                        Err(e) => {
                            warn!("{e}");
                            time::sleep(Duration::from_millis(WAIT_BALANCE_INTERVAL)).await;
                            continue;
                        }
                    }

                    let mut runner = ctx.runner.lock().await;

                    match runner.current_state().await {
                        RunState::Processing(task) | RunState::Error(task) => {
                            // println!("... {:#?}", task);
                            info!("Previous Task #{} was failed, waiting for manual action...", task.id);
                            time::sleep(Duration::from_millis(WAIT_ACTION_INTERVAL)).await;
                        }
                        // previous task was complete, try to confirm it
                        RunState::Complete(task) => {
                            info!("Confirming Task #{}...", task.id);
                            match ctx.client.read_message(task.id, &ctx.channel_id).await {
                                Ok(sig) => {
                                    info!("Confirmed Task #{}! Signature: {}", task.id, sig);
                                    *ctx.last_task_id.lock().await = task.id;
                                    // reset state only if the command is confirmed
                                    runner.reset_state().await.ok();
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
                                    if prev_state != RunState::Pending {
                                        info!("Waiting a command...");
                                    }
                                    time::sleep(Duration::from_millis(WAIT_COMMAND_INTERVAL)).await;
                                    continue;
                                }
                            }
                            Err(e) => {
                                error!("Error: {}", e)
                            }
                        },
                    }

                    prev_state = runner.current_state().await;
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
    ) -> JoinHandle<()> {
        tokio::spawn({
            let ctx = self.ctx.clone();

            async move {
                loop {
                    tokio::select! {
                        Some(e) = new_receiver.recv() => {
                            let cek = ctx.cek.lock().await;
                            let mut runner = ctx.runner.lock().await;
                            match convert_message_to_task(e.message, cek.as_ref()) {
                                Ok(new_task) => {
                                    if !new_task.is_skipped() {
                                        info!("Task #{} was added...", new_task.id);
                                        runner.add_task(new_task);
                                    } else {
                                        warn!("Cannot add a task #{}, probably skipped...", new_task.id);
                                    }
                                }
                                Err(_) => {
                                    warn!("Failed to convert message to task");
                                }
                            }
                        }
                        Some(e) = update_receiver.recv() => {
                            let mut runner = ctx.runner.lock().await;
                            match runner.current_state().await {
                                RunState::Error(task) | RunState::Processing(task) => {
                                    let cek = ctx.cek.lock().await;
                                    match convert_message_to_task(e.message, cek.as_ref()) {
                                        Ok(new_task) => {
                                            if new_task.is_skipped() {
                                                info!("Task #{} was skipped...", new_task.id);
                                                if task.id == new_task.id {
                                                    runner.reset_state().await;
                                                    if ctx.notify_event("skip", Some(&task)).await.is_err() {
                                                        info!("Failed to send skip notify...");
                                                    }
                                                }
                                            }
                                        }
                                        Err(_) => {
                                            warn!("Failed to convert message to task");
                                        }
                                    }
                                },
                                _ => {}
                            }
                        }
                        Some(e) = delete_receiver.recv() => {
                            let mut runner = ctx.runner.lock().await;
                            match runner.current_state().await {
                                RunState::Error(task) | RunState::Processing(task) => {
                                    if task.id == e.id {
                                        runner.reset_state().await.ok();
                                    }
                                },
                                _ => {}
                            }
                            runner.delete_task(e.id);
                            info!("Task #{} was deleted...", e.id);
                        }
                    }
                }
            }
        })
    }

    /// Listen to channel events
    #[tracing::instrument(skip_all)]
    fn command_listener(
        &self,
        new_msg_sender: mpsc::UnboundedSender<NewMessageEvent>,
        update_msg_sender: mpsc::UnboundedSender<UpdateMessageEvent>,
        delete_msg_sender: mpsc::UnboundedSender<DeleteMessageEvent>,
    ) -> JoinHandle<()> {
        tokio::spawn({
            let ctx = self.ctx.clone();
            let url = ctx.client.cluster.ws_url().to_owned();
            let filter = RpcTransactionLogsFilter::Mentions(vec![ctx.channel_id.to_string()]);

            let mut interval = tokio::time::interval(Duration::from_secs(5));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

            async move {
                loop {
                    // On disconnect, retry every 5s.
                    interval.tick().await;

                    info!("Connecting to `{}`...", &url);

                    match PubsubClient::new(&url).await {
                        Ok(pub_sub_client) => {
                            info!("Connected!");

                            info!("Loading tasks...");
                            ctx.load_tasks().await;

                            let listener = Listener::new(&ctx.client.program_id, &pub_sub_client, &filter);

                            let logs_subscribe = listener
                                .logs_subscribe(Duration::from_secs(LOGS_SUBSCRIBE_TIMEOUT))
                                .await;

                            info!("Listen new events...");
                            match logs_subscribe {
                                Ok((mut stream, _unsubscribe)) => {
                                    while let Some(log) = stream.next().await {
                                        // skip simulation
                                        if log.value.signature
                                            == "1111111111111111111111111111111111111111111111111111111111111111"
                                        {
                                            info!("Probably simulation, skip...");
                                            continue;
                                        }

                                        listener
                                            .on::<NewMessageEvent>(&log, &|evt| {
                                                info!("{:?}", evt);
                                                if let Err(e) = new_msg_sender.send(evt) {
                                                    warn!("[NewMessageEvent] Failed to send... {}", e);
                                                }
                                            })
                                            .on::<DeleteMessageEvent>(&log, &|evt| {
                                                info!("{:?}", evt);
                                                if let Err(e) = delete_msg_sender.send(evt) {
                                                    warn!("[DeleteMessageEvent] Failed to send... {}", e);
                                                }
                                            })
                                            .on::<UpdateMessageEvent>(&log, &|evt| {
                                                info!("{:?}", evt);
                                                if let Err(e) = update_msg_sender.send(evt) {
                                                    warn!("[UpdateMessageEvent] Failed to send... {}", e);
                                                }
                                            });
                                    }
                                    // _unsubscribe().await;
                                }
                                Err(e) => {
                                    error!("Error: {:?}", e);
                                }
                            }

                            // match &pub_sub_client.shutdown().await {
                            //     Ok(_) => {}
                            //     Err(e) => {
                            //         info!("Failed to shutdown client ({})", e);
                            //     }
                            // }

                            // info!("Reconnecting...");
                        }
                        Err(e) => {
                            error!("{}", e);
                        }
                    };
                }
            }
        })
    }
}

pub struct AgentContext {
    /// Messenger client
    client: MessengerClient,
    /// Channel with tasks
    channel_id: Pubkey,
    /// ID of last successful task
    last_task_id: Mutex<u64>,
    /// Content Encryption Key
    cek: Mutex<Vec<u8>>,
    runner: Mutex<TaskRunner>,
}

impl AgentContext {
    #[tracing::instrument(skip_all)]
    async fn init(&self) -> Result<()> {
        {
            self.runner.lock().await.init().await?;
        }

        if self.notify_event("start", None).await.is_err() {
            info!("Failed to send `start` notify...");
        }

        loop {
            match self.authorize().await {
                Ok(_) => {
                    if self.notify_event("authorize", None).await.is_err() {
                        info!("Failed to send `authorize` notify...");
                    }
                    break;
                }
                Err(e) => {
                    warn!("Auth Error: {:?}", e);
                    time::sleep(Duration::from_millis(WAIT_AUTHORIZATION_INTERVAL)).await;
                }
            }
        }

        Ok(())
    }

    #[tracing::instrument(skip_all)]
    async fn authorize(&self) -> Result<()> {
        check_balance(&self.client).await?;
        match self.client.load_membership(&self.channel_id).await {
            Ok(membership) => {
                if membership.status == ChannelMembershipStatus::Authorized {
                    *self.last_task_id.lock().await = membership.last_read_message_id;
                    *self.cek.lock().await = self.client.load_cek(&self.channel_id).await?;
                    info!("Authorized!");
                    return Ok(());
                }
                Err(Error::msg("Awaiting authorization..."))
            }
            Err(e) => {
                warn!("{}", e);
                self.client
                    .join_channel(&self.channel_id, Some(AGENT_NAME.to_string()))
                    .await?;
                Err(e)
            }
        }
    }

    /// Load messages from the `channel` account,
    /// decrypt, convert to the [Task] and add to the runner queue
    #[tracing::instrument(skip_all)]
    async fn load_tasks(&self) -> Result<()> {
        info!("Loading channel...");
        let channel = self.client.load_channel(&self.channel_id).await?;
        let cek = self.cek.lock().await;
        let id_from = *self.last_task_id.lock().await;

        info!("Prepare tasks... (id_from: {})", id_from);

        let mut tasks = vec![];
        let mut need_reset = false;

        let mut runner = self.runner.lock().await;

        let curr_task_id = match runner.current_state().await {
            RunState::Processing(t) | RunState::Complete(t) | RunState::Error(t) => {
                info!("Current task id: {}", t.id);
                t.id
            }
            _ => 0,
        };

        info!("Found {} channel messages", channel.messages.len());

        for message in channel.messages {
            let msg_id = message.id;
            if msg_id > id_from {
                match convert_message_to_task(message, cek.as_ref()) {
                    Ok(task) => {
                        if task.is_skipped() {
                            if task.id == curr_task_id {
                                need_reset = true;
                            }
                        } else {
                            tasks.push(task);
                        }
                    }
                    Err(_) => {
                        warn!("Failed to convert message to task... Message#{}", msg_id);
                    }
                }
            }
        }

        if need_reset {
            info!("Reset current run state");
            if let Err(e) = runner.reset_state().await {
                warn!("Failed to reset run state... {}", e);
            }
        }

        let mut added = 0;
        for task in tasks {
            runner.add_task(task);
            added += 1;
        }

        runner.dedup();

        info!("Added {} tasks to the runner queue...", added);

        Ok(())
    }

    async fn notify_event(&self, event: &str, task: Option<&Task>) -> Result<()> {
        let agent_id = self.client.authority_pubkey();
        let cluster = self.client.cluster.clone();
        let opts = NotifierOpts::new()
            .with_agent_id(agent_id)
            .with_cluster(cluster)
            .with_channel_id(self.channel_id);

        {
            let n = Notifier::new(&opts);
            if let Some(t) = task {
                n.with_task(t)
            } else {
                n
            }
        }
        .notify(event)
        .await
    }
}

async fn check_balance(client: &MessengerClient) -> Result<u64> {
    match client.get_balance().await {
        Ok(balance) => {
            if balance < MIN_BALANCE_REQUIRED {
                return Err(Error::msg(format!(
                    "Insufficient balance! Required minimum {MIN_BALANCE_REQUIRED} lamports."
                )));
            }
            Ok(balance)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use anchor_client::Cluster;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_agent_init() {
        let keypair = PathBuf::from("./keypair.json");

        pub const CHANNEL_ID: &str = "Bk1EAvKminEwHjyVhG2QA7sQeU1W3zPnFE6rTnLWDKYJ";

        let agent = Agent::new(&AgentArgs {
            keypair,
            cluster: Cluster::Devnet,
            channel_id: CHANNEL_ID.parse().unwrap(),
            program_id: MESSENGER_PROGRAM_ID.parse().unwrap(),
            ..Default::default()
        })
        .unwrap();

        // let channel = agent.load_channel().await.unwrap();
        // let commands = agent.load_commands().await.unwrap();
        // println!("{:?}", commands);
    }
}
