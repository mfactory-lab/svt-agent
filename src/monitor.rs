use shiplift::{Container, ContainerOptions, Docker};
use tracing::info;

///
/// Think about web endpoint with `docker logs` output
///

const MONITOR_IMAGE: &str = "amir20/dozzle:latest";
const DEFAULT_USERNAME: &str = "admin";
const DEFAULT_HOST_PORT: u16 = 8888;
const CONTAINER_NAME: &str = "svt-agent-monitor";

#[derive(Default)]
pub struct TaskMonitorOptions<'a> {
    port: u16,
    username: &'a str,
    password: &'a str,
    filter: &'a str,
}

impl<'a> TaskMonitorOptions<'a> {
    pub fn new() -> Self {
        Self {
            filter: "",
            password: "",
            username: DEFAULT_USERNAME,
            port: DEFAULT_HOST_PORT,
        }
    }

    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    pub fn password(mut self, password: &'a str) -> Self {
        self.password = password;
        self
    }

    pub fn filter(mut self, filter: &'a str) -> Self {
        self.filter = filter;
        self
    }
}

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

    #[tracing::instrument(skip(self))]
    pub async fn start(&mut self) -> anyhow::Result<()> {
        let container = self.docker.containers().get(CONTAINER_NAME);

        // trying to stop and delete previously started container
        if (container.stop(None).await).is_ok() {
            container.delete().await?;
            info!("Deleted existing container...");
        }

        let options = ContainerOptions::builder(MONITOR_IMAGE)
            .name(CONTAINER_NAME)
            .volumes(vec!["/var/run/docker.sock:/var/run/docker.sock"])
            .expose(8080, "tcp", self.opts.port as u32)
            .auto_remove(true)
            .env([
                "DOZZLE_NO_ANALYTICS=true",
                &format!(
                    "DOZZLE_USERNAME={}",
                    if !self.opts.password.is_empty() {
                        self.opts.username
                    } else {
                        ""
                    }
                ),
                &format!("DOZZLE_PASSWORD={}", self.opts.password),
                &format!("DOZZLE_FILTER=name={}", self.opts.filter),
            ])
            .build();

        let info = self.docker.containers().create(&options).await?;
        info!("Monitor container was created ({:?})", info);

        let container = self.docker.containers().get(&info.id);
        container.start().await?;
        info!("Monitor was started...");

        self.container = Some(container);

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub async fn stop(&mut self) -> anyhow::Result<()> {
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

#[tokio::test]
async fn test_monitor() {
    let docker = Docker::new();

    let opts = TaskMonitorOptions::new()
        .filter("monitor")
        .password("admin");

    let mut monitor = TaskMonitor::new(&opts, &docker);

    monitor.start().await;
}
