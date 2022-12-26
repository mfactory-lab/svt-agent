use crate::monitor::{TaskMonitor, TaskMonitorOptions};
use crate::notifier::Notifier;
use anyhow::{Error, Result};
use futures::StreamExt;
use shiplift::tty::TtyChunk;
use shiplift::{Container, ContainerOptions, Docker, LogsOptions};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time;
use tracing::error;
use tracing::info;

const ANSIBLE_IMAGE: &str = "spy86/ansible:latest";
/// Used to filter containers by the monitor instance
const TASK_CONTAINER_NAME: &str = "svt-agent-task";
/// Prevent starting the monitoring when commands executes immediately
const MONITOR_START_TIMEOUT_MS: u64 = 2000;
const MONITOR_DEFAULT_PORT: u16 = 8888;

#[derive(Debug)]
pub struct Task {
    /// Command numeric identifier
    pub id: u64,
    /// Unique command identifier, used as password for monitoring
    pub uuid: String,
    /// Ansible playbook name
    pub playbook: String,
    /// Added to the ansible-playbook command as `--extra-vars`
    pub extra_vars: String,
}

pub struct TaskRunner {
    /// The [Task] queue
    queue: VecDeque<Task>,
    docker: Docker,
    playbook_path: PathBuf,
    monitor_port: u16,
    monitor_start_timeout_ms: u64,
    container_name: String,
}

impl TaskRunner {
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            docker: Docker::new(),
            playbook_path: PathBuf::from("/app/playbooks"),
            monitor_start_timeout_ms: MONITOR_START_TIMEOUT_MS,
            monitor_port: MONITOR_DEFAULT_PORT,
            container_name: TASK_CONTAINER_NAME.to_string(),
        }
    }

    pub fn with_playbook_path(&mut self, path: PathBuf) -> &mut Self {
        self.playbook_path = path;
        self
    }

    pub fn with_monitor_port(&mut self, port: u16) -> &mut Self {
        self.monitor_port = port;
        self
    }

    /// Try to run the front task from the [queue] and start the monitor instance if needed.
    #[tracing::instrument(skip(self))]
    pub async fn run(&mut self) -> Result<Option<Task>> {
        if let Some(task) = self.queue.pop_front() {
            let mut notifier = Notifier::new(&task);
            let mut _err: Option<Error> = None;
            let mut is_success = false;

            match self.run_task(&task).await {
                Ok(container) => {
                    notifier.notify_start().await;

                    let mut opts = TaskMonitorOptions::new();
                    opts.filter(&self.container_name);
                    opts.port(self.monitor_port);
                    opts.password(&task.uuid);

                    let mut monitor = TaskMonitor::new(&opts, &self.docker);

                    let monitor_start_timeout =
                        time::sleep(Duration::from_millis(self.monitor_start_timeout_ms));
                    tokio::pin!(monitor_start_timeout);

                    loop {
                        tokio::select! {
                            res = container.wait() => {
                                match res {
                                    Ok(exit) => {
                                        info!("Task finished...");
                                        is_success = exit.status_code == 0;
                                    },
                                    Err(e) => _err = Some(Error::from(e))
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

                    if is_success {
                        let output = get_container_logs(&container).await;
                        // println!("output: {:?}", output);
                        notifier.notify_finish(output).await;
                    }

                    if let Err(e) = container.delete().await {
                        error!("Failed to delete task container. {}", e);
                    }

                    if let Err(e) = monitor.stop().await {
                        error!("Failed to stop monitor. {}", e);
                    }
                }
                Err(e) => {
                    info!("Failed to run command");
                    println!("{:?}", e);
                    _err = Some(e);
                }
            }

            if let Some(e) = _err {
                notifier.notify_error(&e).await;
                return Err(e);
            }

            if is_success {
                return Ok(Some(task));
            }
        }
        Ok(None)
    }

    #[tracing::instrument(skip(self))]
    async fn run_task(&self, task: &Task) -> Result<Container> {
        if !self.playbook_path.is_dir() {
            return Err(Error::msg(format!(
                "Playbooks path `{}` is invalid...",
                self.playbook_path.to_str().unwrap()
            )));
        }

        let options = ContainerOptions::builder(ANSIBLE_IMAGE)
            .name(&self.container_name)
            .volumes(vec![
                &format!("{}:/work:ro", self.playbook_path.to_str().unwrap()),
                // "~/.ansible/roles:/root/.ansible/roles",
                // "~/.ssh:/root/.ssh:ro",
            ])
            .cmd(vec![
                "ansible-playbook",
                &format!("{}.yml", task.playbook),
                &format!("-e {}", task.extra_vars),
                "-i 127.0.0.1,",
                "--connection=local",
                "-vvv",
            ])
            // can't get container logs with auto_remove = true
            // .auto_remove(true)
            .build();

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

        let info = self.docker.containers().create(&options).await?;
        let container = self.docker.containers().get(&info.id);
        info!("Container Id:{:?}", container.id());

        if let Err(e) = container.start().await {
            // automatic delete container if is failed
            container.delete().await?;
            error!("Error: {:?}", e);
            return Err(Error::from(e));
        }

        Ok(container)
    }

    /// Add new [task] to the [queue]
    pub fn add_task(&mut self, task: Task) -> &mut Self {
        self.queue.push_back(task);
        self
    }
}

/// Try to get the [container] output
#[tracing::instrument(skip(container))]
async fn get_container_logs(container: &Container<'_>) -> String {
    let mut res = String::new();
    let mut logs_stream = container.logs(&LogsOptions::builder().stdout(true).stderr(true).build());

    while let Some(log_result) = logs_stream.next().await {
        match log_result {
            Ok(chunk) => {
                if let TtyChunk::StdOut(bytes) = chunk {
                    res.push_str(std::str::from_utf8(&bytes).unwrap());
                }
            }
            Err(e) => error!("Error: {}", e),
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
        playbooks.push("playbooks");

        let mut runner = TaskRunner::new();
        runner.with_playbook_path(playbooks);

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
