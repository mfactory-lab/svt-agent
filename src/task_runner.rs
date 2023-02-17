use crate::constants::{ANSIBLE_IMAGE, CONTAINER_NAME};
use crate::monitor::{TaskMonitor, TaskMonitorOptions};
use crate::notifier::{Notifier, NotifierOpts};
use crate::AgentArgs;
use anchor_client::Cluster;
use anchor_lang::prelude::*;
use anyhow::{Error, Result};
use futures::StreamExt;
use serde_json::json;
use shiplift::tty::TtyChunk;
use shiplift::{Container, ContainerOptions, Docker, LogsOptions, PullOptions};
use sled::Db;
use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::{Mutex, MutexGuard};
use tokio::time;
use tracing::{debug, info};
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
    pub args: HashMap<String, String>,
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

pub struct TaskRunnerOpts {
    /// Working directory with playbooks, config, etc
    pub working_dir: PathBuf,
    /// The name of the task container
    pub container_name: String,
    pub monitor_port: u16,
    pub monitor_start_timeout_ms: u64,
    pub channel_id: Pubkey,
    pub cluster: Cluster,
}

impl TaskRunnerOpts {
    pub fn new(args: &AgentArgs) -> Self {
        let working_dir = if let Some(working_dir) = &args.working_dir {
            working_dir.into()
        } else {
            PathBuf::from(DEFAULT_WORKING_DIR)
        };

        Self {
            working_dir,
            container_name: format!("{}-task", CONTAINER_NAME),
            monitor_start_timeout_ms: MONITOR_START_TIMEOUT_MS,
            monitor_port: args.monitor_port,
            channel_id: args.channel_id,
            cluster: args.cluster.clone(),
        }
    }
}

pub struct TaskRunner {
    /// The [Task] queue
    queue: VecDeque<Task>,
    /// The last added [Task] identifier
    last_added_task_id: u64,
    /// Current run state
    state: Mutex<RunState>,
    /// [Docker] instance (task running)
    docker: Docker,
    /// Sled [Db] instance for state persistence
    db: Option<Db>,
    /// Runner options
    opts: TaskRunnerOpts,
}

impl TaskRunner {
    pub fn new(opts: TaskRunnerOpts) -> Self {
        Self {
            queue: VecDeque::new(),
            last_added_task_id: Default::default(),
            state: Default::default(),
            docker: Docker::new(),
            db: None,
            opts,
        }
    }

    /// Ping the docker healthy
    async fn ping(&self) -> Result<()> {
        let res = self.docker.ping().await?;
        debug!("Docker ping: {}", res);
        Ok(())
    }

    /// Prepare the runner before run tasks
    /// Check is valid `working_dir`
    /// Pull [ANSIBLE_IMAGE] if not exists
    #[tracing::instrument(skip_all)]
    pub async fn init(&mut self) -> Result<()> {
        if !self.opts.working_dir.is_dir() {
            return Err(Error::msg(format!(
                "Working dir `{}` is not exists...",
                self.opts.working_dir.to_str().unwrap()
            )));
        }

        // initialize db
        let home = self.opts.working_dir.to_str().unwrap();
        self.db = Some(sled::open(format!("{home}/db"))?);

        if let Err(e) = self.load_state().await {
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

        // Setup monitoring
        TaskMonitor::init(&self.docker).await?;

        Ok(())
    }

    /// Retrieve the current run state
    pub async fn current_state(&self) -> RunState {
        self.state.lock().await.to_owned()
    }

    /// Try to run the front task from the [queue] and start the monitor instance if needed.
    /// Return [Task] if container status code present
    pub async fn run(&mut self) -> Result<RunState> {
        self.ping().await?;

        if let Some(task) = self.queue.pop_front() {
            let notifier_opts = NotifierOpts::new()
                .with_cluster(self.opts.cluster.clone())
                .with_channel_id(self.opts.channel_id);
            let mut notifier = Notifier::new(&notifier_opts, &task);
            // probably docker error
            let mut internal_error: Option<Error> = None;
            let mut status_code: Option<u64> = None;

            match self.run_task(&task).await {
                Ok(container) => {
                    let _ = notifier.notify_start().await;
                    self.set_state(RunState::Processing(task.clone()))
                        .await
                        .ok();

                    let monitor_opts = TaskMonitorOptions::new()
                        .filter(self.opts.container_name.as_ref())
                        .port(self.opts.monitor_port)
                        .secret(&task.secret);

                    let mut monitor = TaskMonitor::new(&monitor_opts, &self.docker);

                    let monitor_start_timeout =
                        time::sleep(Duration::from_millis(self.opts.monitor_start_timeout_ms));
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
                        if status_code == 0 {
                            self.set_state(RunState::Complete(task.clone())).await.ok();
                            let output =
                                get_container_logs(&container, ContainerLogFlags::StdOut).await;
                            let _ = notifier.notify_finish(status_code, output).await;
                        } else {
                            self.set_state(RunState::Error(task.clone())).await.ok();
                            let output = get_container_logs(
                                &container,
                                match status_code {
                                    1 => ContainerLogFlags::StdErr,
                                    _ => ContainerLogFlags::All,
                                },
                            )
                            .await;
                            notifier.notify_error(output).await.ok();
                        }
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
                self.set_state(RunState::Error(task.clone())).await.ok();
                notifier.notify_error(e.to_string()).await.ok();
                return Err(e);
            }
        }

        Ok(self.current_state().await)
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
            .get(&self.opts.container_name)
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

        let working_dir = "/app/ansible";

        let mut cmd = vec![
            "ansible-playbook".to_string(),
            // "--limit=localhost",
            // &format!("--inventory=./inventory/{}.yaml", self.opts.cluster),
            "--tags=agent".to_string(),
            format!(
                "--inventory={},",
                env::var("DOCKER_HOST_IP").unwrap_or("localhost".to_string())
            ),
        ];
        for file in &[
            "all.yml".to_string(),
            format!("{}_validators.yaml", self.opts.cluster),
        ] {
            let file = format!("{working_dir}/inventory/group_vars/{file}");
            if Path::new(&file).exists() {
                cmd.push(format!("--extra-vars=@{file}"));
            }
        }
        cmd.push(format!("--extra-vars={}", json!(task.args)));
        cmd.push(format!("./playbooks/{}.yaml", task.name));

        let options = ContainerOptions::builder(ANSIBLE_IMAGE)
            .name(&self.opts.container_name)
            .network_mode("host")
            .volumes_from(vec![CONTAINER_NAME])
            .working_dir(working_dir)
            .cmd(cmd.iter().map(|c| c.as_str()).collect())
            .env(["ANSIBLE_HOST_KEY_CHECKING=False"])
            .build();

        info!("Creating task container... {:?}", &options);
        let create_info = self.docker.containers().create(&options).await?;
        let container = self.docker.containers().get(&create_info.id);
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

    /// Deduplicate the [queue]
    pub fn dedup(&mut self) {
        let mut uniques = HashSet::new();
        self.queue.retain(|e| uniques.insert(e.id));
    }

    /// Clear the [queue]
    pub fn clear(&mut self) {
        self.queue.clear()
    }

    pub fn can_add_task(&mut self, task_id: u64) -> bool {
        task_id > self.last_added_task_id
    }

    /// Add new [task] to the [queue]
    pub fn add_task(&mut self, task: Task) {
        if self.can_add_task(task.id) {
            self.last_added_task_id = task.id;
            // prevent task duplication
            self.delete_task(task.id);
            self.queue.push_back(task);
        } else {
            warn!("Task #{} has already been added earlier", task.id);
        }
    }

    /// Delete [task] from the queue
    pub fn delete_task(&mut self, task_id: u64) {
        self.queue.retain(|t| t.id != task_id);
    }

    pub async fn notify_event(&self, task: &Task, event: &str) -> Result<()> {
        let opts = NotifierOpts::new()
            .with_cluster(self.opts.cluster.clone())
            .with_channel_id(self.opts.channel_id);
        Notifier::new(&opts, task).notify(event).await
    }

    #[tracing::instrument(skip(self))]
    pub async fn reset_state(&self) -> Result<()> {
        self.set_state(RunState::default()).await
    }

    #[tracing::instrument(skip(self))]
    async fn set_state(&self, state: RunState) -> Result<()> {
        info!("New state... {:?}", state);
        *self.state.lock().await = state;
        self.sync_state().await
    }

    #[tracing::instrument(skip(self))]
    async fn load_state(&self) -> Result<()> {
        if let Some(db) = &self.db {
            let bytes = db.get("runner_state")?;
            if let Some(bytes) = bytes {
                *self.state.lock().await = RunState::try_from_slice(&bytes)?;
            }
        }
        Ok(())
    }

    #[tracing::instrument(skip(self))]
    async fn sync_state(&self) -> Result<()> {
        if let Some(db) = &self.db {
            let bytes = self.state.lock().await.try_to_vec()?;
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

        let mut runner = TaskRunner::new(TaskRunnerOpts::new(&AgentArgs {
            keypair: Default::default(),
            cluster: Default::default(),
            channel_id: Default::default(),
            program_id: Default::default(),
            working_dir: None,
            monitor_port: 0,
        }));

        let task = Task {
            id: 0,
            secret: "".to_string(),
            name: "test".to_string(),
            args: HashMap::from([("sleep".to_string(), "10".to_string())]),
            action: "".to_string(),
        };

        runner.add_task(task);

        assert_eq!(runner.queue.len(), 1);

        // let res = runner.run_task(&task).await;

        let res = runner.run().await;
        println!("{:?}", res);
    }
}
