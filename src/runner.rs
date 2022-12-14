use crate::monitor::TaskMonitor;
use crate::notifier::Notifier;
use anyhow::{Context, Error, Result};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Child;
use tokio::process::Command;
use tokio::time;
use tracing::error;
use tracing::info;

const TASK_CONTAINER_NAME: &str = "svt-agent-task";
const MONITOR_DEFAULT_PORT: u16 = 8888;
const MONITOR_START_TIMEOUT_MS: u64 = 2000;

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
    /// Commands queue
    queue: Vec<Task>,
    home_path: PathBuf,
    monitor_port: u16,
    monitor_start_timeout_ms: u64,
}

impl TaskRunner {
    pub fn new() -> Self {
        Self {
            queue: Vec::new(),
            // TODO: fixme
            home_path: PathBuf::from("/Users/tiamo/IdeaProjects/svt-agent"),
            monitor_start_timeout_ms: MONITOR_START_TIMEOUT_MS,
            monitor_port: MONITOR_DEFAULT_PORT,
        }
    }

    pub fn with_home_path(&mut self, path: PathBuf) -> &mut Self {
        self.home_path = path;
        self
    }

    pub fn with_monitor_port(&mut self, port: u16) -> &mut Self {
        self.monitor_port = port;
        self
    }

    /// Run command from the [queue]
    #[tracing::instrument(skip(self))]
    pub async fn run(&mut self) -> Result<()> {
        if let Some(task) = self.queue.pop() {
            let notifier = Notifier::new(&task);
            notifier.notify_pre_start();

            let mut _err: Option<Error> = None;
            match self.run_task(&task) {
                Ok(child) => {
                    notifier.notify_start();

                    let mut monitor = TaskMonitor::new(
                        self.monitor_port,
                        TASK_CONTAINER_NAME,
                        Some(task.uuid.as_str()),
                    );

                    let monitor_start_timeout =
                        time::sleep(Duration::from_millis(self.monitor_start_timeout_ms));
                    tokio::pin!(monitor_start_timeout);

                    let mut join_handle = tokio::spawn(async move {
                        // time::sleep(Duration::from_millis(7000)).await;
                        child.wait_with_output().await
                    });

                    loop {
                        tokio::select! {
                            join_res = &mut join_handle => {
                                match join_res {
                                    Ok(res) => match res {
                                        Ok(output) => {
                                            notifier.notify_finish(&output.status);
                                            // eprintln!("output: {:?}", output)
                                        }
                                        Err(e) => {
                                            _err = Some(Error::from(e));
                                        }
                                    },
                                    Err(e) => {
                                        _err = Some(Error::from(e));
                                    }
                                }
                                break;
                            }
                            _ = &mut monitor_start_timeout, if !monitor_start_timeout.is_elapsed() => {
                                if let Err(e) = monitor.start() {
                                    error!("Failed to start monitor. {}", e);
                                }
                            }
                            else => { break }
                        }
                    }

                    if let Err(e) = monitor.stop().await {
                        error!("Failed to stop monitor. {}", e);
                    }
                }
                Err(e) => {
                    info!("Failed to run command");
                    _err = Some(e);
                }
            }
            if let Some(e) = _err {
                notifier.notify_error(&e);
                return Err(e);
            }
        }
        Ok(())
    }

    /// Add new [task] to the [queue]
    pub fn add_task(&mut self, task: Task) {
        self.queue.push(task);
    }

    #[tracing::instrument(skip(self))]
    fn run_task(&self, task: &Task) -> Result<Child> {
        let mut cmd = Command::new("docker");

        cmd.args([
            "run",
            "-v",
            &format!("{}/playbooks:/work:ro", self.home_path.to_str().unwrap()),
            // "-v ~/.ansible/roles:/root/.ansible/roles",
            // "-v ~/.ssh:/root/.ssh:ro",
            "--rm",
            "--name",
            TASK_CONTAINER_NAME,
            "spy86/ansible:latest",
        ]);

        cmd.args([
            "ansible-playbook",
            &format!("{}.yml", "test"),
            &format!("-e \"{}\"", task.extra_vars.as_str()),
            "-vvv",
        ]);

        // verbosity
        // cmd.args(["-vvv"]);

        info!("Executing... {:?}", cmd.as_std());

        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);
        cmd.spawn().context("Running task")
    }
}
