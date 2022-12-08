use crate::state::NewMessageEvent;
use anchor_client::solana_client::nonblocking::pubsub_client::PubsubClient;
use anchor_client::solana_client::rpc_config::{
    RpcTransactionLogsConfig, RpcTransactionLogsFilter,
};
use anchor_client::solana_client::rpc_response::{Response, RpcLogsResponse};
use anchor_client::solana_sdk::commitment_config::CommitmentConfig;
use anchor_client::solana_sdk::pubkey::Pubkey;
use anchor_client::ClientError;
use anchor_lang::AnchorDeserialize;
use anyhow::Result;
use futures::StreamExt;
use regex::Regex;
use std::fmt::Debug;
use tracing::info;

pub struct Listener {
    program_id: Pubkey,
}

impl Listener {
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
                // TODO: handle
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
}

const PROGRAM_LOG: &str = "Program log: ";
const PROGRAM_DATA: &str = "Program data: ";

fn handle_program_log<T: anchor_lang::Event + anchor_lang::AnchorDeserialize>(
    program_id: &str,
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
        let (program, did_pop) = handle_system_log(program_id, l);
        Ok((None, program, did_pop))
    }
}

fn handle_system_log(program_id: &str, log: &str) -> (Option<String>, bool) {
    if log.starts_with(&format!("Program {} log:", program_id)) {
        (Some(program_id.to_string()), false)
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
