use crate::constants::CONTAINER_NAME;
use anyhow::Error;
use anyhow::Result;
use futures::StreamExt;
use shiplift::{Container, ContainerOptions, Docker, PullOptions};
use std::borrow::Cow;
use tracing::info;

const MONITOR_IMAGE: &str = "amir20/dozzle:latest";
const DEFAULT_HOST_PORT: u16 = 8888;

pub struct TaskMonitor<'a, 'b> {
    opts: &'a TaskMonitorOptions<'b>,
    docker: &'a Docker,
    container: Option<Container<'a>>,
}

impl<'a, 'b> TaskMonitor<'a, 'b> {
    pub fn new(opts: &'a TaskMonitorOptions<'b>, docker: &'a Docker) -> Self {
        Self {
            opts,
            docker,
            container: None,
        }
    }

    #[tracing::instrument(skip_all)]
    pub async fn init(docker: &Docker) -> Result<()> {
        let mut stream = docker
            .images()
            .pull(&PullOptions::builder().image(MONITOR_IMAGE).build());

        while let Some(pull_result) = stream.next().await {
            match pull_result {
                Ok(output) => info!("{:?}", output),
                Err(e) => {
                    info!("Error: {}", e);
                    return Err(Error::from(e));
                }
            }
        }

        Ok(())
    }

    #[tracing::instrument(skip_all)]
    pub async fn start(&mut self) -> Result<()> {
        let container_name = format!("{}-monitor", CONTAINER_NAME);

        let container = self.docker.containers().get(&container_name);

        // trying to stop previously started container
        let _ = container.stop(None).await;

        let options = ContainerOptions::builder(MONITOR_IMAGE)
            .name(&container_name)
            .volumes(vec!["/var/run/docker.sock:/var/run/docker.sock"])
            .expose(8080, "tcp", self.opts.port as u32)
            .auto_remove(true)
            .env([
                "DOZZLE_NO_ANALYTICS=true",
                &format!(
                    "DOZZLE_BASE=/{}",
                    if !self.opts.secret.is_empty() {
                        &self.opts.secret
                    } else {
                        ""
                    }
                ),
                &format!("DOZZLE_FILTER=name={}", self.opts.filter),
            ])
            .build();

        let info = self.docker.containers().create(&options).await?;
        info!("Monitor container was created ({:?})", info);

        // let container = self.docker.containers().get(&info.id);
        container.start().await?;
        info!("Monitor was started...");

        self.container = Some(container);

        Ok(())
    }

    #[tracing::instrument(skip_all)]
    pub async fn stop(&mut self) -> Result<()> {
        if let Some(container) = &self.container {
            info!("Stopping monitoring...");
            container.stop(None).await?;
            self.container = None;
        } else {
            info!("Monitor is not started...");
        }
        Ok(())
    }

    pub fn is_started(&self) -> bool {
        self.container.is_some()
    }
}

#[derive(Default)]
pub struct TaskMonitorOptions<'a> {
    port: u16,
    secret: Cow<'a, str>,
    filter: Cow<'a, str>,
}

impl<'a> TaskMonitorOptions<'a> {
    pub fn new() -> Self {
        Self {
            port: DEFAULT_HOST_PORT,
            ..Default::default()
        }
    }

    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    pub fn secret(mut self, secret: &'a str) -> Self {
        self.secret = Cow::from(secret);
        self
    }

    pub fn filter(mut self, filter: &'a str) -> Self {
        self.filter = Cow::from(filter);
        self
    }
}

#[tokio::test]
async fn test_monitor() {
    let docker = Docker::new();

    let opts = TaskMonitorOptions::new().filter("monitor");

    let mut monitor = TaskMonitor::new(&opts, &docker);

    let r = monitor.start().await;

    println!("{:?}", r);
}
