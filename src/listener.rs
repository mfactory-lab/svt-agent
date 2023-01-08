use anchor_client::solana_client::nonblocking::pubsub_client::{PubsubClient, PubsubClientResult};
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
use tracing::info;

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
                commitment: Some(CommitmentConfig::processed()),
            },
            filter,
            client,
        }
    }

    pub async fn log_stream(
        &self,
    ) -> PubsubClientResult<(
        BoxStream<'_, Response<RpcLogsResponse>>,
        Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>,
    )> {
        self.client
            .logs_subscribe(self.filter.clone(), self.config.clone())
            .await
    }

    // TODO: find a better way to handle many events in single method
    // pub async fn on<T: Event>(&self, cb: impl Fn(T)) -> Result<()> {
    //     let (mut stream, _unsubscribe) = self
    //         .client
    //         .logs_subscribe(self.filter.clone(), self.config.clone())
    //         .await?;
    //
    //     while let Some(log) = stream.next().await {
    //         match self.handle_event::<T>(&log, &cb) {
    //             Ok(_) => {}
    //             Err(e) => {
    //                 error!("Error: {}", e);
    //             }
    //         }
    //     }
    //
    //     info!("Unsubscribing...");
    //     // FnOnce::call_once(unsubscribe, ());
    //
    //     Ok(())
    // }

    pub fn on<T>(&self, log: &Response<RpcLogsResponse>, cb: &impl Fn(T)) -> Result<()>
    where
        T: Event,
    {
        let mut logs = &log.value.logs[..];
        let self_program_str = self.program_id.to_string();

        if !logs.is_empty() {
            info!("Try to parse logs...");
            if let Ok(mut execution) = Execution::new(&mut logs) {
                info!("{} logs found...", logs.len());
                for l in logs {
                    // Parse the log.
                    let (event, new_program, did_pop) = {
                        if self_program_str == execution.program() {
                            handle_program_log::<T>(self_program_str.as_str(), l).unwrap_or_else(
                                |e| {
                                    info!("Unable to parse log: {}", e);
                                    (None, None, false)
                                },
                            )
                        } else {
                            let (program, did_pop) =
                                handle_system_log(self_program_str.as_str(), l);
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

fn handle_program_log<T: Event>(
    program_id: &str,
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
