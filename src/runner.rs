use crate::monitor::{TaskMonitor, TaskMonitorOptions};
use crate::notifier::Notifier;
use anyhow::{Error, Result};
use futures::StreamExt;
use shiplift::tty::TtyChunk;
use shiplift::{Container, ContainerOptions, Docker, LogsOptions, PullOptions};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time;
use tracing::info;
use tracing::{error, warn};

/// @link: https://hub.docker.com/r/willhallonline/ansible
const ANSIBLE_IMAGE: &str = "willhallonline/ansible:alpine";
/// Used to filter containers by the monitor instance
const TASK_CONTAINER_NAME: &str = "svt-agent-task";
/// Prevent starting the monitoring when commands executes immediately
const MONITOR_START_TIMEOUT_MS: u64 = 2000;
const DEFAULT_MONITOR_PORT: u16 = 8888;
const DEFAULT_WORKING_DIR: &str = "/app";

#[derive(Debug)]
pub struct Task {
    /// The numeric identifier of the [Task]
    pub id: u64,
    /// Unique identifier, used as password for monitoring
    pub uuid: String,
    /// Ansible playbook name
    pub playbook: String,
    /// Variables added to the command in `--extra-vars`
    pub extra_vars: String,
}

pub struct TaskRunner {
    /// The [Task] queue
    queue: VecDeque<Task>,
    docker: Docker,
    working_dir: PathBuf,
    monitor_port: u16,
    monitor_start_timeout_ms: u64,
    container_name: String,
}

impl TaskRunner {
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            docker: Docker::new(),
            working_dir: PathBuf::from(DEFAULT_WORKING_DIR),
            monitor_start_timeout_ms: MONITOR_START_TIMEOUT_MS,
            monitor_port: DEFAULT_MONITOR_PORT,
            container_name: TASK_CONTAINER_NAME.to_string(),
        }
    }

    pub fn with_working_dir(&mut self, path: PathBuf) -> &mut Self {
        self.working_dir = path;
        self
    }

    pub fn with_monitor_port(&mut self, port: u16) -> &mut Self {
        self.monitor_port = port;
        self
    }

    /// Add new [task] to the [queue]
    pub fn add_task(&mut self, task: Task) {
        self.queue.push_back(task);
    }

    /// Ping the docker healthy
    async fn ping(&self) -> Result<()> {
        let res = self.docker.ping().await?;
        info!("Docker ping: {}", res);
        Ok(())
    }

    /// Prepare the runner before run tasks
    /// Check is valid `working_dir`
    /// Pull [ANSIBLE_IMAGE] if not exists
    #[tracing::instrument(skip(self))]
    pub async fn prepare(&self) -> Result<()> {
        if !self.working_dir.is_dir() {
            return Err(Error::msg(format!(
                "Working dir `{}` is not exists...",
                self.working_dir.to_str().unwrap()
            )));
        }

        // download ansible image first
        let mut stream = self
            .docker
            .images()
            .pull(&PullOptions::builder().image(ANSIBLE_IMAGE).build());

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

    /// Try to run the front task from the [queue] and start the monitor instance if needed.
    #[tracing::instrument(skip(self))]
    pub async fn run(&mut self) -> Result<Option<Task>> {
        self.ping().await?;

        if let Some(task) = self.queue.pop_front() {
            let mut notifier = Notifier::new(&task);
            let mut notify_error: Option<Error> = None;
            let mut status_code: Option<u64> = None;

            match self.run_task(&task).await {
                Ok(container) => {
                    let _ = notifier.notify_start().await;

                    let opts = TaskMonitorOptions::new()
                        .filter(&self.container_name)
                        .port(self.monitor_port)
                        .password(&task.uuid);

                    let mut monitor = TaskMonitor::new(&opts, &self.docker);

                    let monitor_start_timeout =
                        time::sleep(Duration::from_millis(self.monitor_start_timeout_ms));
                    tokio::pin!(monitor_start_timeout);

                    loop {
                        tokio::select! {
                            res = container.wait() => {
                                match res {
                                    Ok(exit) => {
                                        status_code = Some(exit.status_code);
                                        info!("Task finished (status code: {})...", exit.status_code);
                                    },
                                    Err(e) => notify_error = Some(Error::from(e))
                                }
                                break;
                            }
                            _ = &mut monitor_start_timeout, if !monitor_start_timeout.is_elapsed() => {
                                info!("Starting monitor...");
                                if let Err(e) = monitor.start().await {
                                    error!("Failed to start monitor. {}", e);
                                }
                            }
                            else => { break }
                        }
                    }

                    if let Some(status_code) = status_code {
                        let output = get_container_logs(
                            &container,
                            match status_code {
                                0 => ContainerLogFlags::STD_OUT,
                                1 => ContainerLogFlags::STD_ERR,
                                _ => ContainerLogFlags::ALL,
                            },
                        )
                        .await;
                        let _ = notifier.notify_finish(status_code, output).await;
                    }

                    // if let Err(e) = container.delete().await {
                    //     warn!("Failed to delete task container. {}", e);
                    // }

                    if monitor.is_started() {
                        if let Err(e) = monitor.stop().await {
                            warn!("Failed to stop monitor. {}", e);
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to run task. {:?}", e);
                    notify_error = Some(e);
                }
            }

            if let Some(e) = notify_error {
                let _ = notifier.notify_error(&e).await;
                return Err(e);
            }

            if status_code.is_some() {
                return Ok(Some(task));
            }
        }
        Ok(None)
    }

    #[tracing::instrument(skip(self))]
    async fn run_task(&self, task: &Task) -> Result<Container> {
        // Delete if exists
        if let Err(_e) = self
            .docker
            .containers()
            .get(&self.container_name)
            .delete()
            .await
        {
            // error!("Error: {}", e)
            // container is not exists...
        }

        let options = ContainerOptions::builder(ANSIBLE_IMAGE)
            .name(&self.container_name)
            .network_mode("host")
            // can't get container logs with auto_remove = true
            // .auto_remove(true)
            // .tty(true)
            // .privileged(true)
            .volumes(vec![
                &format!("{}:/ansible:ro", self.working_dir.to_str().unwrap()),
                // "~/.ansible/roles:/root/.ansible/roles",
                // "~/.ssh:/root/.ssh:ro",
                // "~/.ssh/id_rsa:/root/.ssh/id_rsa",
            ])
            .cmd(vec![
                "ansible-playbook",
                &format!("./playbooks/{}.yml", task.playbook),
                &format!("-e {}", task.extra_vars),
                // "-i 127.0.0.1,",
                // "--connection=local",
                // "-vvv",
            ])
            .build();

        info!("Creating task container... {:?}", &options);
        let info = self.docker.containers().create(&options).await?;
        let container = self.docker.containers().get(&info.id);
        info!("Container Id:{:?}", container.id());

        info!("Starting task container...");
        if let Err(e) = container.start().await {
            // automatic delete container if is failed
            container.delete().await?;
            error!("Error: {:?}", e);
            return Err(Error::from(e));
        }

        Ok(container)
    }
}

//noinspection RsModuleNaming
mod ContainerLogFlags {
    pub const STD_OUT: u8 = 0x01;
    pub const STD_ERR: u8 = 0x02;
    pub const ALL: u8 = 0xFF;
}

/// Try to get the [container] output
#[tracing::instrument(skip(container))]
async fn get_container_logs(container: &Container<'_>, mode: u8) -> String {
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
                    if mode & ContainerLogFlags::STD_OUT == 1 {
                        res.push_str(std::str::from_utf8(&bytes).unwrap());
                    }
                }
                TtyChunk::StdErr(bytes) => {
                    if mode & ContainerLogFlags::STD_ERR == 2 {
                        res.push_str(std::str::from_utf8(&bytes).unwrap());
                    }
                }
                _ => {}
            },
            Err(e) => error!("Failed to get container logs: {}", e),
        }
    }

    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use tracing_test::traced_test;

    #[tokio::test]
    #[traced_test]
    async fn test_runner() {
        let mut playbooks = env::current_dir().unwrap();
        playbooks.push("ansible");

        let mut runner = TaskRunner::new();
        runner.with_working_dir(playbooks);

        let task = Task {
            id: 0,
            uuid: "".to_string(),
            playbook: "test".to_string(),
            extra_vars: "sleep=10".to_string(),
        };

        runner.add_task(task);

        assert_eq!(runner.queue.len(), 1);

        // let res = runner.run_task(&task).await;

        let res = runner.run().await;
        println!("{:?}", res);
    }
}
