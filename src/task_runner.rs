use crate::constants::{ANSIBLE_IMAGE, CONTAINER_NAME};
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
use std::sync::Mutex;
use std::time::Duration;
use tokio::time;
use tracing::info;
use tracing::{error, warn};

/// Prevent starting the monitoring when commands executes immediately
const MONITOR_START_TIMEOUT_MS: u64 = 2000;
const DEFAULT_WORKING_DIR: &str = "/app";

#[derive(Debug, Default, Clone, AnchorSerialize, AnchorDeserialize, Eq, PartialEq)]
pub struct Task {
    /// The numeric identifier of the [Task]
    pub id: u64,
    /// Task name (ansible playbook)
    pub name: String,
    /// Arguments of the task (ansible `--extra-vars`)
    pub args: String,
    /// Secret key, used as password for monitoring
    pub secret: String,
    /// Manual task action
    pub action: String,
}

impl Task {
    pub fn is_skipped(&self) -> bool {
        self.action == "skip"
    }
}

#[derive(Debug, Clone, AnchorSerialize, AnchorDeserialize, Eq, PartialEq)]
pub enum RunState {
    Processing(Task),
    Complete(Task),
    Error(Task),
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
    state: Mutex<RunState>,
    /// [Docker] instance (task running)
    docker: Docker,
    /// [Db] instance (state persistence)
    db: Option<Db>,
    /// Working directory with playbooks, config, etc
    working_dir: PathBuf,
    /// The name of the container with witch the task is run
    container_name: String,
    monitor_port: u16,
    monitor_start_timeout_ms: u64,
}

impl TaskRunner {
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            docker: Docker::new(),
            state: Mutex::new(RunState::default()),
            working_dir: PathBuf::from(DEFAULT_WORKING_DIR),
            container_name: format!("{}-task", CONTAINER_NAME),
            monitor_start_timeout_ms: MONITOR_START_TIMEOUT_MS,
            monitor_port: 8888,
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

    pub fn current_state(&self) -> RunState {
        self.state.lock().unwrap().to_owned()
    }

    /// Try to run the front task from the [queue] and start the monitor instance if needed.
    /// Return [Task] if container status code present
    #[tracing::instrument(skip_all)]
    pub async fn run(&mut self) -> Result<RunState> {
        self.ping().await?;

        if let Some(task) = self.queue.pop_front() {
            let mut notifier = Notifier::new(&task).with_logs_path(self.working_dir.join("logs"));
            // probably docker error
            let mut internal_error: Option<Error> = None;
            let mut status_code: Option<u64> = None;

            match self.run_task(&task).await {
                Ok(container) => {
                    let _ = notifier.notify_start().await;

                    let _ = self.set_state(RunState::Processing(task.clone()));

                    let opts = TaskMonitorOptions::new()
                        .filter(self.container_name.as_ref())
                        .port(self.monitor_port)
                        .password(&task.secret);

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
                                    Err(e) => internal_error = Some(Error::from(e))
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

                    // retrieve task output only if status code is present
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
                    internal_error = Some(e);
                }
            }

            if let Some(e) = internal_error {
                let _ = self.set_state(RunState::Error(task.clone()));
                let _ = notifier.notify_error(&e).await;
                return Err(e);
            }

            if let Some(code) = status_code {
                let _ = self.set_state(if code == 0 {
                    RunState::Complete(task.clone())
                } else {
                    RunState::Error(task.clone())
                });
                return Ok(self.current_state());
            }
        }

        Ok(RunState::Pending)
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
            // for local tests...
            // .volumes(vec![
            //     &format!("{}:/ansible:ro", self.working_dir.to_str().unwrap()),
            //     "/root/.ssh/id_rsa:/root/.ssh/id_rsa:ro",
            //     // "~/.ansible/roles:/root/.ansible/roles",
            // ])
            .volumes_from(vec![CONTAINER_NAME])
            .working_dir("/app/ansible")
            .cmd(vec![
                "ansible-playbook",
                &format!("./playbooks/{}.yml", task.name),
                &format!("-e {}", task.args),
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

    /// Add new [task] to the [queue]
    pub fn add_task(&mut self, task: Task) {
        self.queue.push_back(task);
    }

    /// Delete [task] from the queue
    pub fn delete_task(&mut self, task_id: u64) {
        self.queue.retain(|t| t.id != task_id);
    }

    /// Clear the [queue]
    pub fn clear(&mut self) {
        self.queue.clear()
    }

    pub fn reset_state(&self) -> Result<()> {
        self.set_state(RunState::default())
    }

    fn set_state(&self, state: RunState) -> Result<()> {
        info!("New state... {:?}", state);
        *self.state.lock().unwrap() = state;
        self.sync_state()
    }

    fn load_state(&mut self) -> Result<()> {
        if let Some(db) = &self.db {
            let bytes = db.get("runner_state")?;
            if let Some(bytes) = bytes {
                self.state = Mutex::new(RunState::try_from_slice(&bytes)?);
            }
        }
        Ok(())
    }

    fn sync_state(&self) -> Result<()> {
        if let Some(db) = &self.db {
            let bytes = self.state.lock().unwrap().try_to_vec()?;
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
            secret: "".to_string(),
            name: "test".to_string(),
            args: "sleep=10".to_string(),
            action: "".to_string(),
        };

        runner.add_task(task);

        assert_eq!(runner.queue.len(), 1);

        // let res = runner.run_task(&task).await;

        let res = runner.run().await;
        println!("{:?}", res);
    }
}
