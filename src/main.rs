#![feature(fn_traits)]

mod constants;
mod encryption;
mod state;
mod utils;

use crate::encryption::{decrypt_cek, decrypt_message};
use anchor_client::solana_client::nonblocking::pubsub_client::PubsubClient;
use anchor_client::solana_client::nonblocking::rpc_client::RpcClient;
use anchor_client::solana_client::rpc_config::{
    RpcTransactionLogsConfig, RpcTransactionLogsFilter,
};
use anchor_client::solana_client::rpc_response::{Response, RpcLogsResponse};
use anchor_client::solana_sdk::commitment_config::CommitmentConfig;
use anchor_client::solana_sdk::pubkey::Pubkey;
use anchor_client::solana_sdk::signature::{read_keypair_file, Keypair, Signer};
use anchor_client::{ClientError, Cluster};
use anchor_lang::AnchorDeserialize;
use anyhow::{Error, Result};
use clap::Parser;
use constants::*;
use futures::prelude::*;
use regex::Regex;
use state::*;
use std::ffi::OsStr;
use std::fmt::Debug;
use std::path::PathBuf;
use std::str::FromStr;
use tracing::{debug, info};
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

        // TODO: Invalid Request: Only 1 address supported (-32602)
        let addresses = vec![self.channel_id.to_string()];

        loop {
            info!("Trying to connect `{}`...", self.cluster.ws_url());
            let pubsub_client = PubsubClient::new(self.cluster.ws_url()).await?;
            self.listen_commands(&pubsub_client, &addresses).await?;
            match pubsub_client.shutdown().await {
                Ok(_) => {
                    info!("Successfully disconnected.");
                }
                Err(e) => {
                    info!("Failed to disconnect: {}", e);
                }
            }
            info!("Waiting 3 sec...");
            tokio::time::sleep(std::time::Duration::from_millis(3000)).await;
        }
    }

    #[tracing::instrument(skip(self))]
    async fn load_commands(&self) -> Result<Vec<String>> {
        info!("Loading device...");
        let device = self.load_device().await?;

        info!("Decrypting CEK...");
        let cek = decrypt_cek(device.cek, self.keypair.secret().as_bytes())?;

        info!("Loading membership...");
        let membership = self.load_membership().await?;

        info!("Loading channel...");
        let channel = self.load_channel().await?;

        info!("Prepare messages...");
        let mut commands = vec![];
        for message in channel.messages {
            if message.id > membership.last_read_message_id {
                commands.push(String::from_utf8(decrypt_message(
                    message.content,
                    cek.as_slice(),
                )?)?);
            }
        }

        info!("Found {} commands...", commands.len());

        Ok(commands)
    }

    #[tracing::instrument(skip(self, client))]
    async fn listen_commands(&self, client: &PubsubClient, addresses: &[String]) -> Result<()> {
        let (mut stream, unsubscribe) = client
            .logs_subscribe(
                RpcTransactionLogsFilter::Mentions(addresses.to_vec()),
                RpcTransactionLogsConfig {
                    commitment: Some(CommitmentConfig::processed()),
                },
            )
            .await?;

        while let Some(log) = stream.next().await {
            self.handle_event::<NewMessageEvent>(log, |e| {
                info!("New Event {:?}", e);
            })
            .await
            .expect("Failed to handle `NewMessageEvent`");
        }

        info!("Unsubscribing...");
        FnOnce::call_once(unsubscribe, ());

        Ok(())
    }

    async fn handle_event<T>(
        &self,
        log: Response<RpcLogsResponse>,
        cb: fn(e: T) -> (),
    ) -> Result<()>
    where
        T: anchor_lang::Event + anchor_lang::AnchorDeserialize + Debug,
    {
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
                        cb(e);
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

    fn get_membership_pda(&self, channel: &Pubkey, authority: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(
            &[&channel.to_bytes(), &authority.to_bytes()],
            &self.program_id,
        )
    }

    fn get_device_pda(&self, membership: &Pubkey, key: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(&[&membership.to_bytes(), &key.to_bytes()], &self.program_id)
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
}

struct CommandRunnerCommand {
    id: u64,
    uuid: String,
    name: String,
    args: Vec<String>,
}

struct CommandRunner {
    cek: Vec<u8>,
    queue: Vec<CommandRunnerCommand>,
}

impl CommandRunner {
    pub fn new(cek: Vec<u8>) -> Self {
        Self {
            cek,
            queue: Vec::new(),
        }
    }

    pub fn push_message(&mut self, msg: Message) -> Result<()> {
        let cmd = String::from_utf8(decrypt_message(msg.content, self.cek.as_slice())?)?;

        let parts = cmd.split(COMMAND_DELIMITER).collect::<Vec<_>>();

        if parts.len() < 3 {
            return Err(Error::msg("Invalid command"));
        }

        let name = String::from(parts[0]);
        let args = parts[1]
            .split(',')
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        let uuid = String::from(parts[2]);

        self.queue.push(CommandRunnerCommand {
            id: msg.id,
            name,
            args,
            uuid,
        });

        Ok(())
    }

    pub fn run(&mut self) {
        if let Some(cmd) = self.queue.pop() {
            self.run_ansible_task();
            self.run_log_monitor();
        }
    }

    pub fn run_ansible_task(&self) -> Result<()> {
        let a = tokio::process::Command::new("docker")
            .arg("run")
            .arg("-a")
            .kill_on_drop(true)
            .spawn()?;

        Ok(())
    }

    pub fn run_log_monitor(&self) -> Result<()> {
        // TODO: run monitor
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
    let commands = agent.load_commands().await.unwrap();

    println!("{:?}", commands);
}
