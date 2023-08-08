use crate::messenger::NewMessageEvent;
use anchor_client::ClientError;
use anchor_lang::{AnchorDeserialize, Event};
use anyhow::Result;
use clap::builder::Str;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use futures::StreamExt;
use regex::internal::Input;
use regex::{Captures, Regex};
use solana_client::nonblocking::pubsub_client::{PubsubClient, PubsubClientError, PubsubClientResult};
use solana_client::rpc_client::RpcClient;
use solana_client::rpc_config::{RpcTransactionLogsConfig, RpcTransactionLogsFilter};
use solana_client::rpc_response::{Response, RpcLogsResponse};
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::pubkey::Pubkey;
use std::borrow::Cow;
use std::cell::OnceCell;
use std::sync::OnceLock;
use std::time::Duration;
use tracing::{debug, info, warn};

type UnsubscribeFn = Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>;
type SubscribeResult<'a, T> = PubsubClientResult<(BoxStream<'a, T>, UnsubscribeFn)>;

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
    pub fn new(program_id: &Pubkey, client: &'a PubsubClient, filter: &'a RpcTransactionLogsFilter) -> Self {
        Self {
            program_id: *program_id,
            config: RpcTransactionLogsConfig {
                commitment: Some(CommitmentConfig::confirmed()),
            },
            filter,
            client,
        }
    }

    pub async fn logs_subscribe(&self, timeout: Duration) -> SubscribeResult<Response<RpcLogsResponse>> {
        let task = self.client.logs_subscribe(self.filter.clone(), self.config.clone());

        tokio::select! {
            res = task => {
                info!("Subscribed!");
                res
            }
            _ = tokio::time::sleep(timeout) => {
                Err(PubsubClientError::ConnectionClosed(format!("timeout exceeded: {}s", timeout.as_secs())))
            }
        }
    }

    pub fn on<T: Event>(&self, log: &Response<RpcLogsResponse>, f: &impl Fn(T)) -> &Self {
        parse_logs::<T>(&self.program_id.to_string(), &log.value.logs, f);
        self
    }
}

#[inline]
fn parse_logs<E: Event>(program_id_str: &str, logs: impl IntoIterator<Item = impl Into<String>>, f: &impl Fn(E)) {
    for (pid, log) in Execution::new(logs) {
        if program_id_str == pid {
            let (event, _, _) = handle_program_log::<E>(program_id_str, &log).unwrap_or_else(|e| {
                warn!("Unable to parse log: {} ({})", log, e);
                (None, None, false)
            });
            if let Some(e) = event {
                f(e);
            }
        }
    }
}

const PROGRAM_LOG: &str = "Program log: ";
const PROGRAM_DATA: &str = "Program data: ";

static PROGRAM_INVOKE_RE: OnceLock<Regex> = OnceLock::new();
static PROGRAM_SUCCESS_RE: OnceLock<Regex> = OnceLock::new();

#[inline]
fn handle_program_log<T: anchor_lang::Event + anchor_lang::AnchorDeserialize>(
    self_program_str: &str,
    l: &str,
) -> Result<(Option<T>, Option<String>, bool), ClientError> {
    // Log emitted from the current program.
    if let Some(log) = l.strip_prefix(PROGRAM_LOG).or_else(|| l.strip_prefix(PROGRAM_DATA)) {
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
        let re = PROGRAM_SUCCESS_RE.get_or_init(|| Regex::new(r"^Program (.*) success*$").unwrap());
        if re.is_match(log) {
            (None, true)
        } else {
            (None, false)
        }
    }
}

#[derive(Debug)]
struct Execution {
    logs: Vec<String>,
    program_id: String,
    index: usize,
}

impl Iterator for Execution {
    type Item = (String, String);

    fn next(&mut self) -> Option<Self::Item> {
        let re = PROGRAM_INVOKE_RE.get_or_init(|| Regex::new(r"^Program (.*) invoke.*$").unwrap());
        if let Some(log) = self.logs.get(self.index) {
            if let Some(c) = re.captures(log).and_then(|l| l.get(1)) {
                self.program_id = c.as_str().to_owned();
            }
            self.index += 1;
            Some((self.program_id.to_owned(), log.to_owned()))
        } else {
            None
        }
    }
}

impl Execution {
    pub fn new<T, I>(logs: T) -> Self
    where
        I: Into<String>,
        T: IntoIterator<Item = I>,
    {
        Self {
            logs: logs.into_iter().map(|i| i.into()).collect(),
            program_id: Default::default(),
            index: 0,
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
    fn can_parse_simple_new_message_log() {
        let program_id = MESSENGER_PROGRAM_ID;

        let logs = vec![
            "Program CgRaMXqqRHNT3Zo2uVZfX72TuxUgcLb8E3A8KrXnbXAC invoke [1]",
            "Program log: Instruction: PostMessage",
            "Program data: f+oFlw98HI/XJ7lRjgJLxQEwYGUh2xIw9QZIsLuauTpvLXnjZfTp0QQAAAAAAAAAIA85nxmXZazQMMhPkyUosaAQ2L5bWdpx10IvN+LP+WYlOO9jAAAAAAGQAAAAUHZUZm11UzNBSkRtTzJCTVZpTW1rdCtEVlYvRDJQTkwzRG5zcUl6Q1FaVlBQdnNOUU1CaGdtTmVwR1hJWjBPeVhSMkZTVlcyRkxKWlV4eHIycjNDVnFPaVFRYUV6bkNpMm9sdkluUFBuNHVQdXE3R0pVN0xBRVREMFFKMXpDVFFibzVudXU0TE9hcHZ5MzRt",
            "Program CgRaMXqqRHNT3Zo2uVZfX72TuxUgcLb8E3A8KrXnbXAC consumed 19775 of 200000 compute units",
            "Program CgRaMXqqRHNT3Zo2uVZfX72TuxUgcLb8E3A8KrXnbXAC success",
        ];

        parse_logs::<NewMessageEvent>(program_id, logs.clone(), &|e| {
            // println!("{:?}", e);
            assert_eq!(e.message.id, 4);
        });
    }

    #[test]
    fn can_parse_new_message_log() {
        let program_id = MESSENGER_PROGRAM_ID;

        let logs = vec![
            "Program ComputeBudget111111111111111111111111111111 invoke [1]",
            "Program ComputeBudget111111111111111111111111111111 success",
            "Program ComputeBudget111111111111111111111111111111 invoke [1]",
            "Program ComputeBudget111111111111111111111111111111 success",
            "Program CgRaMXqqRHNT3Zo2uVZfX72TuxUgcLb8E3A8KrXnbXAC invoke [1]",
            "Program log: Instruction: PostMessage",
            "Program data: f+oFlw98HI/RQ5wPA9epyN8cp7mjF0G0EywpKJ1Ft5g7ccc2hGxpmgIAAAAAAAAA1LvJJMGaavwqOeBSMUJ5c+9JFJJvqCCT1UkIywNvmieXmNJkAAAAAAFMAAAAWXFqY1V0OU1uelg1VFF1T05aY2dPUEd0aU5mWU8rN1krbkZZRTdrZzRvMU1xR0g3YUFpRFNqbExCdlJscWZtODFtbmxwOWo2NXhrPQ==",
            "Program CgRaMXqqRHNT3Zo2uVZfX72TuxUgcLb8E3A8KrXnbXAC consumed 14281 of 200000 compute units",
            "Program CgRaMXqqRHNT3Zo2uVZfX72TuxUgcLb8E3A8KrXnbXAC success"
        ];

        parse_logs::<NewMessageEvent>(program_id, logs, &|e| {
            // println!("{:?}", e);
            assert_eq!(e.message.id, 2)
        });
    }

    #[test]
    fn can_parse_delete_message_log() {
        let program_id = MESSENGER_PROGRAM_ID;

        let logs = vec![
            "Program ComputeBudget111111111111111111111111111111 invoke [1]",
            "Program ComputeBudget111111111111111111111111111111 success",
            "Program ComputeBudget111111111111111111111111111111 invoke [1]",
            "Program ComputeBudget111111111111111111111111111111 success",
            "Program CgRaMXqqRHNT3Zo2uVZfX72TuxUgcLb8E3A8KrXnbXAC invoke [1]",
            "Program log: Instruction: DeleteMessage",
            "Program data: gsiCEypVahHRQ5wPA9epyN8cp7mjF0G0EywpKJ1Ft5g7ccc2hGxpmtS7ySTBmmr8KjngUjFCeXPvSRSSb6ggk9VJCMsDb5onAwAAAAAAAAA=",
            "Program CgRaMXqqRHNT3Zo2uVZfX72TuxUgcLb8E3A8KrXnbXAC consumed 13928 of 200000 compute units",
            "Program CgRaMXqqRHNT3Zo2uVZfX72TuxUgcLb8E3A8KrXnbXAC success",
        ];

        parse_logs::<DeleteMessageEvent>(program_id, logs, &|e| {
            // println!("{:?}", e);
            assert_eq!(e.id, 3);
        });
    }
}
