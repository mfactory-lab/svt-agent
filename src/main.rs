#![feature(fn_traits)]

mod constants;
mod encryption;
mod state;

use anchor_client::solana_client::nonblocking::pubsub_client::PubsubClient;
use anchor_client::solana_client::rpc_config::{
    RpcTransactionLogsConfig, RpcTransactionLogsFilter,
};
use anchor_client::solana_client::rpc_response::{Response, RpcLogsResponse};
use anchor_client::solana_sdk::commitment_config::CommitmentConfig;
use anchor_client::solana_sdk::pubkey::Pubkey;
use anchor_client::solana_sdk::signature::{read_keypair_file, Keypair};
use anchor_client::{ClientError, Cluster};
use anchor_lang::AnchorDeserialize;
use clap::Parser;
use constants::*;
use futures::prelude::*;
use regex::Regex;
use state::*;
use std::error::Error;
use std::fmt::Debug;
use std::path::PathBuf;
use std::str::FromStr;
use tracing::info;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Registry};
use tracing_tree::HierarchicalLayer;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
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

    Agent::init()?.run().await
}

#[derive(Parser)]
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

#[derive(Debug)]
struct Agent {
    /// Keypair is used for decrypting messages from the channel
    keypair: Keypair,
    /// Messenger program id
    program_id: Pubkey,
    /// Channel identifier
    channel_id: Pubkey,
    /// Solana cluster
    cluster: Cluster,
}

impl Agent {
    #[tracing::instrument]
    fn init() -> Result<Self, Box<dyn Error>> {
        let cli = Cli::parse();
        let keypair = read_keypair_file(cli.keypair)?;
        let program_id = Pubkey::from_str(&cli.messenger_program_id)?;
        let channel_id = Pubkey::from_str(&cli.channel_id)?;
        Ok(Self {
            program_id,
            channel_id,
            keypair,
            cluster: cli.cluster,
        })
    }

    fn parse_cek(&self) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    fn load_channel(&self) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    fn load_membership(&self) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    #[tracing::instrument]
    async fn run(&self) -> Result<(), Box<dyn Error>> {
        info!("Program ID {:?}", self.program_id);
        info!("Channel ID {:?}", self.channel_id);

        // TODO:
        //  - Get latest commands from blockchain
        //  - Run or skip each command

        // TODO: Invalid Request: Only 1 address supported (-32602)
        let addresses = vec![self.channel_id.to_string()];

        loop {
            info!("Trying to connect `{}`...", self.cluster.ws_url());
            let pubsub_client = PubsubClient::new(self.cluster.ws_url()).await?;

            self.listen_events(&pubsub_client, &addresses).await?;

            info!("Disconnecting...");
            match pubsub_client.shutdown().await {
                Ok(_) => {
                    info!("Successfully disconnected.");
                }
                Err(e) => {
                    info!("Failed to disconnect: {}", e);
                }
            }

            info!("Waiting 5 sec...");
            tokio::time::sleep(std::time::Duration::from_millis(5000)).await;
        }
    }

    #[tracing::instrument(skip(client))]
    async fn listen_events(
        &self,
        client: &PubsubClient,
        addresses: &[String],
    ) -> Result<(), Box<dyn Error>> {
        info!("Listening events...");
        let (mut stream, unsubscribe) = client
            .logs_subscribe(
                RpcTransactionLogsFilter::Mentions(addresses.to_vec()),
                RpcTransactionLogsConfig {
                    commitment: Some(CommitmentConfig::processed()),
                },
            )
            .await?;

        while let Some(log) = stream.next().await {
            self.handle_event::<NewMessageEvent>(log)
                .await
                .expect("Failed to handle event");
        }

        info!("Unsubscribing...");
        FnOnce::call_once(unsubscribe, ());

        Ok(())
    }

    #[tracing::instrument()]
    async fn handle_event<T>(&self, log: Response<RpcLogsResponse>) -> Result<(), Box<dyn Error>>
    where
        T: anchor_lang::Event + anchor_lang::AnchorDeserialize + Debug,
    {
        info!(
            "  Status: {}",
            log.value
                .err
                .map(|err| err.to_string())
                .unwrap_or_else(|| "Success".into())
        );

        let mut logs = &log.value.logs[..];
        let self_program_str = std::str::from_utf8(self.program_id.as_ref())?;

        if !logs.is_empty() {
            info!("Try to parse logs...");
            if let Ok(mut execution) = Execution::new(&mut logs) {
                info!("{} logs found...", logs.len());
                for l in logs {
                    // Parse the log.
                    let (event, new_program, did_pop) = {
                        if self_program_str == execution.program() {
                            handle_program_log::<T>(self_program_str, l).unwrap_or_else(|e| {
                                info!("Unable to parse log: {}", e);
                                (None, None, false)
                            })
                        } else {
                            let (program, did_pop) = handle_system_log(self_program_str, l);
                            (None, program, did_pop)
                        }
                    };
                    // Emit the event.
                    if let Some(e) = event {
                        info!("New Event!");
                        info!("{:?}", e);
                    }
                    // Switch program context on CPI.
                    if let Some(new_program) = new_program {
                        execution.push(new_program);
                    }
                    // Program returned.
                    if did_pop {
                        execution.pop();
                    }
                }
            }
        }

        Ok(())
    }
}

struct CommandRunner {
    keypair: Keypair,
}

impl CommandRunner {
    pub fn parse(&self, msg: &str) {
        // TODO: parse and decrypt message
    }

    pub fn run_ansible_task() -> Result<(), Box<dyn Error>> {
        let a = tokio::process::Command::new("docker")
            .arg("run")
            .arg("-a")
            .kill_on_drop(true)
            .spawn()?;

        Ok(())
    }

    pub fn run_log_watcher() -> Result<(), Box<dyn Error>> {
        // TODO: run watcher
        Ok(())
    }
}

const PROGRAM_LOG: &str = "Program log: ";
const PROGRAM_DATA: &str = "Program data: ";

fn handle_program_log<T: anchor_lang::Event + anchor_lang::AnchorDeserialize>(
    self_program_str: &str,
    l: &str,
) -> Result<(Option<T>, Option<String>, bool), ClientError> {
    // Log emitted from the current program.
    if let Some(log) = l
        .strip_prefix(PROGRAM_LOG)
        .or_else(|| l.strip_prefix(PROGRAM_DATA))
    {
        let borsh_bytes = match anchor_lang::__private::base64::decode(&log) {
            Ok(borsh_bytes) => borsh_bytes,
            _ => {
                #[cfg(feature = "debug")]
                println!("Could not base64 decode log: {}", log);
                return Ok((None, None, false));
            }
        };

        let mut slice: &[u8] = &borsh_bytes[..];
        let disc: [u8; 8] = {
            let mut disc = [0; 8];
            disc.copy_from_slice(&borsh_bytes[..8]);
            slice = &slice[8..];
            disc
        };
        let mut event = None;
        if disc == T::discriminator() {
            let e: T = AnchorDeserialize::deserialize(&mut slice)
                .map_err(|e| ClientError::LogParseError(e.to_string()))?;
            event = Some(e);
        }
        Ok((event, None, false))
    }
    // System log.
    else {
        let (program, did_pop) = handle_system_log(self_program_str, l);
        Ok((None, program, did_pop))
    }
}

fn handle_system_log(this_program_str: &str, log: &str) -> (Option<String>, bool) {
    if log.starts_with(&format!("Program {} log:", this_program_str)) {
        (Some(this_program_str.to_string()), false)
    } else if log.contains("invoke") {
        (Some("cpi".to_string()), false) // Any string will do.
    } else {
        let re = Regex::new(r"^Program (.*) success*$").unwrap();
        if re.is_match(log) {
            (None, true)
        } else {
            (None, false)
        }
    }
}

struct Execution {
    stack: Vec<String>,
}

impl Execution {
    pub fn new(logs: &mut &[String]) -> Result<Self, ClientError> {
        let l = &logs[0];
        *logs = &logs[1..];

        let re = Regex::new(r"^Program (.*) invoke.*$").unwrap();
        let c = re
            .captures(l)
            .ok_or_else(|| ClientError::LogParseError(l.to_string()))?;
        let program = c
            .get(1)
            .ok_or_else(|| ClientError::LogParseError(l.to_string()))?
            .as_str()
            .to_string();
        Ok(Self {
            stack: vec![program],
        })
    }

    pub fn program(&self) -> String {
        assert!(!self.stack.is_empty());
        self.stack[self.stack.len() - 1].clone()
    }

    pub fn push(&mut self, new_program: String) {
        self.stack.push(new_program);
    }

    pub fn pop(&mut self) {
        assert!(!self.stack.is_empty());
        self.stack.pop().unwrap();
    }
}

#[test]
fn test() {}
