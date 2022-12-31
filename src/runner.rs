use crate::constants::ANSIBLE_IMAGE;
use crate::monitor::{TaskMonitor, TaskMonitorOptions};
use crate::notifier::Notifier;
use anchor_lang::prelude::*;
use anyhow::{Error, Result};
use futures::StreamExt;
use shiplift::tty::TtyChunk;
use shiplift::{Container, ContainerOptions, Docker, LogsOptions, PullOptions};
use sled::Db;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time;
use tracing::info;
use tracing::{error, warn};

/// Used to filter containers by the monitor instance
const TASK_CONTAINER_NAME: &str = "svt-agent-task";
/// Prevent starting the monitoring when commands executes immediately
const MONITOR_START_TIMEOUT_MS: u64 = 2000;
const DEFAULT_MONITOR_PORT: u16 = 8888;
const DEFAULT_WORKING_DIR: &str = "/app";

#[derive(Debug, Clone, AnchorSerialize, AnchorDeserialize, Eq, PartialEq)]
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

#[derive(Debug, Clone, AnchorSerialize, AnchorDeserialize, Eq, PartialEq)]
pub enum RunState {
    Complete(u64),
    Error(u64),
    Pending,
}

impl Default for RunState {
    fn default() -> Self {
        Self::Pending
    }
}

pub struct TaskRunner {
    /// The [Task] queue
    queue: VecDeque<Task>,
    state: RunState,
    /// [Docker] instance (task running)
    docker: Docker,
    /// [Db] instance (state persistence)
    db: Option<Db>,
    /// Working directory with playbooks, config, etc
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
            state: RunState::default(),
            working_dir: PathBuf::from(DEFAULT_WORKING_DIR),
            monitor_start_timeout_ms: MONITOR_START_TIMEOUT_MS,
            monitor_port: DEFAULT_MONITOR_PORT,
            container_name: TASK_CONTAINER_NAME.to_string(),
            db: None,
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

    /// Ping the docker healthy
    async fn ping(&self) -> Result<()> {
        let res = self.docker.ping().await?;
        info!("Docker ping: {}", res);
        Ok(())
    }

    /// Prepare the runner before run tasks
    /// Check is valid `working_dir`
    /// Pull [ANSIBLE_IMAGE] if not exists
    #[tracing::instrument(skip_all)]
    pub async fn init(&mut self) -> Result<()> {
        if !self.working_dir.is_dir() {
            return Err(Error::msg(format!(
                "Working dir `{}` is not exists...",
                self.working_dir.to_str().unwrap()
            )));
        }

        // initialize db
        let home = self.working_dir.to_str().unwrap();
        self.db = Some(sled::open(format!("{}/db", home))?);

        if let Err(e) = self.load_state() {
            info!("Failed to load initial state. {}", e);
        }

        // pull ansible image first
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
    /// Return [Task] if container status code present
    #[tracing::instrument(skip_all)]
    pub async fn run(&mut self) -> Result<RunState> {
        self.ping().await?;

        // if current state is complete or error, just return it
        match &self.state {
            RunState::Complete(_) | RunState::Error(_) => {
                return Ok(self.state.clone());
            }
            _ => {}
        }

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

                    // retrieve logs only if status code is present
                    if let Some(status_code) = status_code {
                        let output = get_container_logs(
                            &container,
                            match status_code {
                                0 => ContainerLogFlags::StdOut,
                                1 => ContainerLogFlags::StdErr,
                                _ => ContainerLogFlags::All,
                            },
                        )
                        .await;
                        let _ = notifier.notify_finish(status_code, output).await;
                    }

                    if let Err(e) = container.delete().await {
                        warn!("Failed to delete task container. {}", e);
                    }

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
                self.set_state(RunState::Error(task.id));
                let _ = notifier.notify_error(&e).await;
                return Err(e);
            }

            if status_code.is_some() {
                self.set_state(RunState::Complete(task.id));
                return Ok(self.state.clone());
            }
        }

        Ok(RunState::Pending)
    }

    /// Add new [task] to the [queue]
    pub fn add_task(&mut self, task: Task) {
        self.queue.push_back(task);
    }

    /// Run the [task] isolated through docker container
    #[tracing::instrument(skip_all)]
    async fn run_task(&self, task: &Task) -> Result<Container> {
        // TODO: think about uniq name for each task
        // let container_name = format!("{}_{}", TASK_CONTAINER_NAME, task.id);

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

        // let container = self.docker.containers().get("svt-agent");
        // let res = container
        //     .inspect()
        //     .await
        //     .expect("Failed to inspect container");

        let options = ContainerOptions::builder(ANSIBLE_IMAGE)
            .name(&self.container_name)
            .network_mode("host")
            // can't get container logs with auto_remove = true
            // .auto_remove(true)
            // .tty(true)
            // .privileged(true)
            .volumes(vec![
                &format!("{}:/ansible:ro", self.working_dir.to_str().unwrap()),
                "/root/.ssh/id_rsa:/root/.ssh/id_rsa:ro",
                // "~/.ansible/roles:/root/.ansible/roles",
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
            // container.delete().await?;
            error!("Error: {:?}", e);
            return Err(Error::from(e));
        }

        Ok(container)
    }

    pub fn reset_state(&mut self) -> Result<()> {
        self.set_state(RunState::default())
    }

    fn set_state(&mut self, state: RunState) -> Result<()> {
        info!("New state... {:?}", &state);
        self.state = state;
        self.sync_state()
    }

    fn load_state(&mut self) -> Result<()> {
        if let Some(db) = &self.db {
            let bytes = db.get("runner_state")?;
            if let Some(bytes) = bytes {
                self.state = RunState::try_from_slice(&bytes)?;
            }
        }
        Ok(())
    }

    fn sync_state(&self) -> Result<()> {
        if let Some(db) = &self.db {
            let bytes = self.state.try_to_vec()?;
            db.insert("runner_state", bytes)?;
        }
        Ok(())
    }
}

#[allow(non_snake_case, non_upper_case_globals)]
mod ContainerLogFlags {
    pub const StdOut: u8 = 0x01;
    pub const StdErr: u8 = 0x02;
    pub const All: u8 = 0xFF;
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
                    if mode & ContainerLogFlags::StdOut == 1 {
                        res.push_str(std::str::from_utf8(&bytes).unwrap());
                    }
                }
                TtyChunk::StdErr(bytes) => {
                    if mode & ContainerLogFlags::StdErr == 2 {
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
