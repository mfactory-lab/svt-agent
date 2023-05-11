use crate::messenger::NewMessageEvent;
use anchor_client::solana_client::nonblocking::pubsub_client::{PubsubClient, PubsubClientResult};
use anchor_client::solana_client::rpc_client::RpcClient;
use anchor_client::solana_client::rpc_config::{
    RpcTransactionLogsConfig, RpcTransactionLogsFilter,
};
use anchor_client::solana_client::rpc_response::{Response, RpcLogsResponse};
use anchor_client::solana_sdk::commitment_config::CommitmentConfig;
use anchor_client::solana_sdk::pubkey::Pubkey;
use anchor_client::ClientError;
use anchor_lang::AnchorDeserialize;
use anchor_lang::Event;
use anyhow::Result;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use regex::Regex;
use tracing::{debug, info, warn};

/// Listen to events in the blockchain through program logs
/// The event is the serialized data in the "Program data:" section.
pub struct Listener<'a> {
    /// The program ID that should be listened
    program_id: Pubkey,
    /// Nonblocking Solana pub/sub client
    client: &'a PubsubClient,
    filter: &'a RpcTransactionLogsFilter,
    config: RpcTransactionLogsConfig,
}

impl<'a> Listener<'a> {
    pub fn new(
        program_id: &Pubkey,
        client: &'a PubsubClient,
        filter: &'a RpcTransactionLogsFilter,
    ) -> Self {
        Self {
            program_id: *program_id,
            config: RpcTransactionLogsConfig {
                commitment: Some(CommitmentConfig::confirmed()),
            },
            filter,
            client,
        }
    }

    pub async fn logs_subscribe(
        &self,
    ) -> PubsubClientResult<(
        BoxStream<'_, Response<RpcLogsResponse>>,
        Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>,
    )> {
        self.client
            .logs_subscribe(self.filter.clone(), self.config.clone())
            .await
    }

    pub fn on<T: Event>(&self, log: &Response<RpcLogsResponse>, f: &impl Fn(T)) -> &Self {
        parse_logs::<T>(self.program_id.to_string(), log.to_owned().value.logs, f);
        self
    }
}

#[inline]
fn parse_logs<T: Event>(program_id: String, logs: Vec<String>, f: &impl Fn(T)) {
    let mut logs = &logs[..];
    if !logs.is_empty() {
        debug!("Try to parse logs...");
        if let Ok(mut execution) = Execution::new(&mut logs) {
            debug!("{} logs found...", logs.len());
            for l in logs {
                // Parse the log.
                let (event, new_program, did_pop) = {
                    if program_id == execution.program() {
                        handle_program_log::<T>(program_id.as_str(), l).unwrap_or_else(|e| {
                            warn!("Unable to parse log: {} ({})", l, e);
                            (None, None, false)
                        })
                    } else {
                        let (program, did_pop) = handle_system_log(program_id.as_str(), l);
                        (None, program, did_pop)
                    }
                };
                // Emit the event.
                if let Some(e) = event {
                    f(e);
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
}

const PROGRAM_LOG: &str = "Program log: ";
const PROGRAM_DATA: &str = "Program data: ";

#[inline]
fn handle_program_log<T: anchor_lang::Event + anchor_lang::AnchorDeserialize>(
    self_program_str: &str,
    l: &str,
) -> Result<(Option<T>, Option<String>, bool), ClientError> {
    // Log emitted from the current program.
    if let Some(log) = l
        .strip_prefix(PROGRAM_LOG)
        .or_else(|| l.strip_prefix(PROGRAM_DATA))
    {
        let borsh_bytes = match anchor_lang::__private::base64::decode(log) {
            Ok(borsh_bytes) => borsh_bytes,
            _ => {
                warn!("Could not base64 decode log: {}", log);
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
            let e: T = anchor_lang::AnchorDeserialize::deserialize(&mut slice)
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

#[inline]
fn handle_system_log(this_program_str: &str, log: &str) -> (Option<String>, bool) {
    if log.starts_with(&format!("Program {this_program_str} log:")) {
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
        // assert!(!self.stack.is_empty());
        if self.stack.is_empty() {
            return Default::default();
        }
        self.stack[self.stack.len() - 1].clone()
    }

    pub fn push(&mut self, new_program: String) {
        self.stack.push(new_program);
    }

    pub fn pop(&mut self) {
        if !self.stack.is_empty() {
            self.stack.pop().unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::MESSENGER_PROGRAM_ID;
    use crate::messenger::{DeleteMessageEvent, UpdateMessageEvent};
    use anchor_client::solana_sdk::signature::Signature;
    use std::str::FromStr;

    #[test]
    fn test1() {
        // let client = RpcClient::new("https://api.testnet.solana.com");
        // let sig = Signature::from_str("48rf44gyATMKVjnzaf2hP3k5SS7q7Qj8YqQxiMxtHURJJNEHrd7fzoerKjejbDag49aSNTfDwPwQ7KSeWNtQxA1N").unwrap();
        // let tx = client
        //     .get_transaction_with_config(&sig, Default::default())
        //     .unwrap();

        let program_id = MESSENGER_PROGRAM_ID;

        let logs = vec![
            "Program CgRaMXqqRHNT3Zo2uVZfX72TuxUgcLb8E3A8KrXnbXAC invoke [1]".to_string(),
            "Program log: Instruction: PostMessage".to_string(),
            "Program data: f+oFlw98HI/XJ7lRjgJLxQEwYGUh2xIw9QZIsLuauTpvLXnjZfTp0QQAAAAAAAAAIA85nxmXZazQMMhPkyUosaAQ2L5bWdpx10IvN+LP+WYlOO9jAAAAAAGQAAAAUHZUZm11UzNBSkRtTzJCTVZpTW1rdCtEVlYvRDJQTkwzRG5zcUl6Q1FaVlBQdnNOUU1CaGdtTmVwR1hJWjBPeVhSMkZTVlcyRkxKWlV4eHIycjNDVnFPaVFRYUV6bkNpMm9sdkluUFBuNHVQdXE3R0pVN0xBRVREMFFKMXpDVFFibzVudXU0TE9hcHZ5MzRt".to_string(),
            "Program CgRaMXqqRHNT3Zo2uVZfX72TuxUgcLb8E3A8KrXnbXAC consumed 19775 of 200000 compute units".to_string(),
            "Program CgRaMXqqRHNT3Zo2uVZfX72TuxUgcLb8E3A8KrXnbXAC success".to_string(),
        ];

        parse_logs::<NewMessageEvent>(program_id.to_string(), logs.clone(), &|e| {
            println!("{:?}", e);
            assert_eq!(e.message.id, 4)
        });
        parse_logs::<UpdateMessageEvent>(program_id.to_string(), logs.clone(), &|e| {
            println!("{:?}", e);
        });
        parse_logs::<DeleteMessageEvent>(program_id.to_string(), logs, &|e| {
            println!("{:?}", e);
        });
    }
}
