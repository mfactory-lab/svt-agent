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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn test_monitor() {
        let mock_server = MockServer::start().await;

        let id = "ede54ee1afda366ab42f824e8a5ffd195155d853ceaec74a927f249ea270c743";

        Mock::given(method("POST"))
            .and(path("/containers/create"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "Id": id })))
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(path_regex(r"^/containers/(.+)/(start|stop)$"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock_server)
            .await;

        let docker = Docker::host(mock_server.uri().parse().unwrap());

        let opts = TaskMonitorOptions::new().filter("monitor");

        let mut monitor = TaskMonitor::new(&opts, &docker);

        let res = monitor.start().await;

        assert!(monitor.container.is_some());
        assert!(monitor.is_started());

        monitor.stop().await;

        assert!(monitor.container.is_none());
        assert!(!monitor.is_started());
    }
}
