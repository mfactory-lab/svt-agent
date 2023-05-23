use anyhow::{Error, Result};
use futures::StreamExt;
use serde_json::json;
use serde_json::Value;
use shiplift::tty::TtyChunk;
use shiplift::{Container, Docker, LogsOptions, PullOptions};
use tracing::{error, info};

/// Pull docker [image] and show result info
pub async fn pull_image(docker: &Docker, image: &str) -> Result<()> {
    info!("Pulling image `{}`...", image);

    let mut stream = docker.images().pull(&PullOptions::builder().image(image).build());

    while let Some(pull_result) = stream.next().await {
        match pull_result {
            Ok(output) => info!("{}", output),
            Err(e) => {
                info!("Error: {:?}", e);
                return Err(Error::from(e));
            }
        }
    }

    Ok(())
}

#[allow(non_snake_case, non_upper_case_globals)]
pub mod ContainerLogFlags {
    pub const StdOut: u8 = 0x01;
    pub const StdErr: u8 = 0x02;
    pub const All: u8 = 0xFF;
}

/// Try to get the [container] output
#[tracing::instrument(skip(container))]
pub async fn get_container_logs(container: &Container<'_>, mode: u8) -> String {
    let mut res = String::new();

    let mut logs_stream = container.logs(
        &LogsOptions::builder()
            .timestamps(true)
            .stdout(true)
            .stderr(true)
            .build(),
    );

    while let Some(log_result) = logs_stream.next().await {
        match log_result {
            Ok(chunk) => match chunk {
                TtyChunk::StdOut(bytes) => {
                    if mode & ContainerLogFlags::StdOut == 1 {
                        res.push_str(std::str::from_utf8(&bytes).unwrap());
                    }

                    info!("StdOut: {}", std::str::from_utf8(&bytes).unwrap());
                }
                TtyChunk::StdErr(bytes) => {
                    if mode & ContainerLogFlags::StdErr == 2 {
                        res.push_str(std::str::from_utf8(&bytes).unwrap());
                    }

                    info!("StdErr: {}", std::str::from_utf8(&bytes).unwrap());
                }
                _ => {}
            },
            Err(e) => error!("Failed to get container logs: {}", e),
        }
    }

    res
}
