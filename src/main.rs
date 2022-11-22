#![feature(fn_traits)]

use anchor_client::solana_client::nonblocking::pubsub_client::PubsubClient;
use anchor_client::solana_client::rpc_config::{
    RpcTransactionLogsConfig, RpcTransactionLogsFilter,
};
use anchor_client::solana_client::rpc_response::{Response, RpcLogsResponse};
use anchor_client::solana_sdk::commitment_config::CommitmentConfig;
use anchor_client::solana_sdk::pubkey::Pubkey;
use anchor_client::{ClientError, Cluster};
use anchor_lang::prelude::borsh;
use anchor_lang::{event, AnchorDeserialize, AnchorSerialize};
use clap::Parser;
use futures::prelude::*;
use regex::Regex;
use std::error::Error;
use tracing::info;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Registry};
use tracing_tree::HierarchicalLayer;

#[derive(Parser)]
#[command(name = "SVT Agent")]
#[command(version = "1.0")]
#[command(about = "SVT Agent", long_about = None)]
struct Cli {
    // #[arg(short, long, value_name = "KEYPAIR")]
    // keypair: PathBuf,
    #[arg(short, long, value_name = "CLUSTER", default_value = "d")]
    cluster: Cluster,
}

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
        //         .with_writer(|| File::create("/tmp/log.json").unwrap()),
        // )
        .init();

    run_server().await
}

#[tracing::instrument]
async fn run_server() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();

    let program_id = "6RSutwAoRcQPAMwyxZdNeG76fdAxzhgxkCJXpqKCBPdm";
    // TODO: from config or cli arg
    let channel_id = "DSoQM4bxPAFqwX9rjMXizYxuMMxTCL32L3cokJC2W3mJ";

    info!("Program ID {:?}", program_id);
    info!("Channel ID {:?}", channel_id);

    // TODO:
    //  - Get latest commands from blockchain
    //  - Run or skip each command

    // TODO: Invalid Request: Only 1 address supported (-32602)
    let addresses = vec![channel_id.to_string()];

    loop {
        info!("Trying to connect `{}`...", cli.cluster.ws_url());
        let pubsub_client = PubsubClient::new(cli.cluster.ws_url()).await?;

        handle_events(&pubsub_client, &addresses).await?;

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

    // tokio::spawn(async move {
    //     loop {
    //         match receiver.recv() {
    //             Ok(logs) => {
    //                 let mut logs = &logs.value.logs[..];
    //                 println!("{:?}", logs)
    //             }
    //             Err(e) => {
    //                 println!("{e}")
    //             }
    //         }
    //     }
    // });

    // tokio::spawn(async move {
    //     loop {
    //             if let Err(err) = handle_event(client).await {
    //                 error!(%err, "Error handling connection" );
    //             }
    //     }
    //     });

    // let addr: SocketAddr = "0.0.0.0:3779".parse()?;
    // info!("Listening on http://{}", addr);
    // let listener = TcpListener::bind(addr).await?;
    // loop {
    //     let (stream, addr) = listener.accept().instrument(info_span!("accept")).await?;
    //     // handle_connection(stream, addr).await?;
    //     tokio::spawn(async move {
    //         if let Err(err) = handle_connection(stream, addr).await {
    //             error!(%err, "Error handling connection" );
    //         }
    //     });
    // }
}

#[tracing::instrument(skip(client))]
async fn handle_events(client: &PubsubClient, addresses: &[String]) -> Result<(), Box<dyn Error>> {
    info!("Subscribe...");
    let (mut stream, unsubscribe) = client
        .logs_subscribe(
            RpcTransactionLogsFilter::Mentions(addresses.to_vec()),
            RpcTransactionLogsConfig {
                commitment: Some(CommitmentConfig::processed()),
            },
        )
        .await?;

    while let Some(log) = stream.next().await {
        handle_event(log).await;
    }

    info!("Unsubscribing...");
    FnOnce::call_once(unsubscribe, ());

    Ok(())
}

#[tracing::instrument()]
async fn handle_event(log: Response<RpcLogsResponse>) -> Result<(), Box<dyn Error>> {
    info!(
        "  Status: {}",
        log.value
            .err
            .map(|err| err.to_string())
            .unwrap_or_else(|| "Success".into())
    );

    let mut logs = &log.value.logs[..];

    // TODO: this.program_id
    let self_program_str = "6RSutwAoRcQPAMwyxZdNeG76fdAxzhgxkCJXpqKCBPdm";

    if !logs.is_empty() {
        info!("Try to parse logs...");
        if let Ok(mut execution) = Execution::new(&mut logs) {
            info!("{} logs found...", logs.len());
            for l in logs {
                // Parse the log.
                let (event, new_program, did_pop) = {
                    if self_program_str == execution.program() {
                        handle_program_log::<NewMessageEvent>(&self_program_str, l).unwrap_or_else(
                            |e| {
                                info!("Unable to parse log: {}", e);
                                (None, None, false)
                            },
                        )
                    } else {
                        let (program, did_pop) = handle_system_log(&self_program_str, l);
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

// #[tracing::instrument(skip(stream))]
// async fn handle_connection(mut stream: TcpStream, addr: SocketAddr) -> Result<(), Box<dyn Error>> {
//     let req = read_http_request(&mut stream).await?;
//     debug!(%req, "Got HTTP request");
//     write_http_response(&mut stream).await?;
//
//     Ok(())
// }
//
// #[tracing::instrument(skip(stream))]
// async fn read_http_request(mut stream: impl AsyncRead + Unpin) -> Result<String, Box<dyn Error>> {
//     let mut incoming = vec![];
//
//     loop {
//         let mut buf = vec![0u8; 1024];
//         let read = stream.read(&mut buf).await?;
//         incoming.extend_from_slice(&buf[..read]);
//
//         if incoming.len() > 4 && &incoming[incoming.len() - 4..] == b"\r\n\r\n" {
//             break;
//         }
//     }
//
//     Ok(String::from_utf8(incoming)?)
// }
//
// #[tracing::instrument(skip(stream))]
// async fn write_http_response(mut stream: impl AsyncWrite + Unpin) -> Result<(), Box<dyn Error>> {
//     stream.write_all(b"HTTP/1.1 200 OK\r\n").await?;
//     stream.write_all(b"\r\n").await?;
//     stream.write_all(b"Hello from plaque!\n").await?;
//     Ok(())
// }

// #[tokio::main]
// async fn main2() -> Result<(), Box<dyn Error>> {
//     let cli = Cli::parse();
//
//     env_logger::init();
//
//     run(cli).await;
//
//     // let daemonize = Daemonize::new()
//     //     .pid_file("svt-agent.pid")
//     //     .working_directory(".")
//     //     // .pid_file("/tmp/test.pid") // Every method except `new` and `start`
//     //     // .chown_pid_file(true)      // is optional, see `Daemonize` documentation
//     //     // .working_directory("/tmp") // for default behaviour.
//     //     // .user("nobody")
//     //     // .group("daemon") // Group name
//     //     // .group(2)        // or group id.
//     //     .umask(0o777)
//     //     .privileged_action(|| "Executed before drop privileges");
//     //
//     // if daemonize.start().is_ok() {
//     //     run(cli).await;
//     // }
// }

// async fn run(cli: Cli) -> Result<(), Box<dyn Error>> {
//     let program_id = Pubkey::from_str("6RSutwAoRcQPAMwyxZdNeG76fdAxzhgxkCJXpqKCBPdm").expect("Expected messenger program id");
//     let payer=  read_keypair_file(cli.keypair).expect("Keypair is required");
//
//     let client = Client::new_with_options(cli.cluster, Rc::new(payer), CommitmentConfig::processed());
//     let program = client.program(program_id);
//
//     info!("DEBUG INFO");
//
//     let (tx, mut rx) = mpsc::unbounded_channel();
//
//     let addresses = vec![program_id.to_string()];
//     let filter = RpcTransactionLogsFilter::Mentions(addresses);
//     let ws_url = cli.cluster.ws_url().to_string();
//     let cfg = RpcTransactionLogsConfig {
//         commitment: Some(CommitmentConfig::processed()),
//     };
//     let self_program_str = program_id.to_string();
//     let (client, receiver) = PubsubClient::logs_subscribe(&ws_url, filter, cfg)?;
//     std::thread::spawn(move || {
//         loop {
//             match receiver.recv() {
//                 Ok(logs) => {
//                     let ctx = EventContext {
//                         signature: logs.value.signature.parse().unwrap(),
//                         slot: logs.context.slot,
//                     };
//                     let mut logs = &logs.value.logs[..];
//                     if !logs.is_empty() {
//                         // if let Ok(mut execution) = Execution::new(&mut logs) {
//                         //     for l in logs {
//                         //         // Parse the log.
//                         //         let (event, new_program, did_pop) = {
//                         //             if self_program_str == execution.program() {
//                         //                 handle_program_log(&self_program_str, l).unwrap_or_else(
//                         //                     |e| {
//                         //                         println!("Unable to parse log: {}", e);
//                         //                         std::process::exit(1);
//                         //                     },
//                         //                 )
//                         //             } else {
//                         //                 let (program, did_pop) =
//                         //                     handle_system_log(&self_program_str, l);
//                         //                 (None, program, did_pop)
//                         //             }
//                         //         };
//                         //         // Emit the event.
//                         //         if let Some(e) = event {
//                         //             f(&ctx, e);
//                         //         }
//                         //         // Switch program context on CPI.
//                         //         if let Some(new_program) = new_program {
//                         //             execution.push(new_program);
//                         //         }
//                         //         // Program returned.
//                         //         if did_pop {
//                         //             execution.pop();
//                         //         }
//                         //     }
//                         // }
//                     }
//                 }
//                 Err(_err) => {
//                     return;
//                 }
//             }
//         }
//     });
//
//     // let handle = program.on(move |_ctx: &EventContext, event: NewMessageEvent| {
//     //     tx.send(event);
//     // }).expect("Cannot init event handler");
//     //
//     // info!("Listen events...");
//     //
//     // loop {
//     //     let message = rx.recv().await;
//     //     if let Some(e) = message {
//     //         info!("Event: {:?}", e);
//     //     }
//     // }
//
//     // TODO: remove once https://github.com/solana-labs/solana/issues/16102
//     //  is addressed. Until then, drop the subscription handle in another
//     //  thread so that we deadlock in the other thread as to not block
//     //  this thread.
//     // std::thread::spawn(move || {
//     //     drop(handle);
//     // });
//
//     Ok(())
// }

#[event]
#[derive(Debug)]
pub struct NewMessageEvent {
    pub channel: Pubkey,
    pub message: Message,
}

#[derive(AnchorSerialize, AnchorDeserialize, Debug)]
pub struct Message {
    pub id: u32,
    pub sender: Pubkey,
    pub created_at: i64,
    pub flags: u8,
    pub content: String,
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
