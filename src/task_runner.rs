use crate::constants::{
    ANSIBLE_DEFAULT_TAG, ANSIBLE_IMAGE, CONTAINER_NAME, TASK_EXTRA_VARS, TASK_EXTRA_VARS_CHECKED, TASK_VERSION_ARG,
    TASK_WORKING_DIR,
};
use crate::monitor::{TaskMonitor, TaskMonitorOptions};
use crate::notifier::{Notifier, NotifierOpts};
use crate::utils::{get_container_logs, pull_image, ContainerLogFlags};
use crate::AgentArgs;
use anchor_client::Cluster;
use anchor_lang::prelude::*;
use anyhow::{Error, Result};
use clap::builder::Str;
use futures::StreamExt;
use serde_json::json;
use shiplift::tty::TtyChunk;
use shiplift::{Container, ContainerOptions, Docker, LogsOptions, PullOptions};
use sled::Db;
use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::{Mutex, MutexGuard};
use tokio::time;
use tracing::{debug, error, info, warn};

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

    pub fn version(&self) -> String {
        let default = env::var("PLAYBOOK_VERSION").unwrap_or(ANSIBLE_DEFAULT_TAG.to_string());
        self.args.get(TASK_VERSION_ARG).map_or(&default, |v| v).to_owned()
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

impl From<&AgentArgs> for TaskRunnerOpts {
    fn from(args: &AgentArgs) -> Self {
        let working_dir = if let Some(path) = &args.working_dir {
            PathBuf::from(path)
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
    #[tracing::instrument(skip_all)]
    pub async fn init(&mut self) -> Result<()> {
        if !self.opts.working_dir.is_dir() {
            return Err(Error::msg(format!(
                "Working dir `{}` is not exists...",
                self.opts.working_dir.to_str().unwrap()
            )));
        }

        // initialize db
        self.db = {
            let home = self.opts.working_dir.to_str().unwrap();
            Some(sled::open(format!("{home}/db"))?)
        };

        self.load_state().await?;

        // pull ansible image if needed
        // pull_image(ANSIBLE_IMAGE).await?;

        // setup monitoring
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
            let mut notifier = self.notifier().with_task(&task);
            let mut internal_error: Option<Error> = None;
            let mut status_code: Option<u64> = None;

            match self.run_task(&task).await {
                Ok(container) => {
                    let _ = notifier.notify_task_start().await;
                    self.set_state(RunState::Processing(task.clone())).await.ok();

                    let monitor_opts = TaskMonitorOptions::new()
                        .filter(self.opts.container_name.as_ref())
                        .port(self.opts.monitor_port)
                        .secret(&task.secret);

                    let mut monitor = TaskMonitor::new(&monitor_opts, &self.docker);

                    let monitor_start_timeout = time::sleep(Duration::from_millis(self.opts.monitor_start_timeout_ms));
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
                            let output = get_container_logs(&container, ContainerLogFlags::StdOut).await;
                            let _ = notifier.notify_task_finish(status_code, output).await;
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
                            notifier.notify_task_error(output).await.ok();
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
                notifier.notify_task_error(e.to_string()).await.ok();
                return Err(e);
            }
        }

        Ok(self.current_state().await)
    }

    /// Run the [task] isolated through docker container
    #[tracing::instrument(skip_all)]
    async fn run_task(&self, task: &Task) -> Result<Container> {
        // Delete if exists
        if let Err(e) = self.docker.containers().get(&self.opts.container_name).delete().await {
            // error!("Error: Failed to delete container. {}", e)
        }

        // pull ansible image if needed
        pull_image(&self.docker, &self.build_image_name(task)).await?;

        let cmd = self.build_cmd(task);

        let options = ContainerOptions::builder(&self.build_image_name(task))
            .name(&self.opts.container_name)
            .working_dir(TASK_WORKING_DIR)
            .network_mode("host")
            .volumes_from(vec![CONTAINER_NAME])
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

        info!("Done");

        Ok(container)
    }

    fn build_image_name(&self, task: &Task) -> String {
        format!("{}:{}", ANSIBLE_IMAGE, task.version())
    }

    fn create_inventory_file(&self, path: impl AsRef<Path>) -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path.as_ref())?;

        file.write_all(
            format!(
                "[{}_validators]\nhost ansible_ssh_host={}",
                &self.opts.cluster.to_string(),
                env::var("DOCKER_HOST_IP").unwrap_or("localhost".to_string())
            )
            .as_bytes(),
        )
    }

    fn build_cmd(&self, task: &Task) -> Vec<String> {
        let host_file = format!("/etc/sv_manager/{}_host", &self.opts.cluster.to_string());

        match self.create_inventory_file(&host_file) {
            Ok(_) => {
                info!("New inventory file `{}` created!", &host_file);
            }
            Err(e) => {
                warn!("Failed to create inventory file. {}", e);
            }
        }

        let mut cmd = vec![
            "ansible-playbook".to_string(),
            // "--limit=localhost",
            // "--tags=agent".to_string(),
            format!("--inventory={}", &host_file),
        ];

        // for &file in TASK_EXTRA_VARS {
        //     let file = file
        //         .replace("{home}", TASK_WORKING_DIR)
        //         .replace("{cluster}", &self.opts.cluster.to_string());
        //
        //     cmd.push(format!("--extra-vars=@{}", file));
        // }
        //
        // for &file in TASK_EXTRA_VARS_CHECKED {
        //     if Path::new(file).exists() {
        //         cmd.push(format!("--extra-vars=@{}", file));
        //     }
        // }

        if !task.args.is_empty() {
            cmd.push(format!("--extra-vars={}", json!(task.args)));
        }

        cmd.push(format!("./playbooks/{}.yaml", task.name));

        cmd
    }

    fn notifier(&self) -> Notifier {
        let notifier_opts = NotifierOpts::new()
            .with_cluster(self.opts.cluster.clone())
            .with_channel_id(self.opts.channel_id)
            .with_logs_path(self.opts.working_dir.join("logs"));

        Notifier::new(notifier_opts)
    }

    /// Deduplicate the [queue]
    pub fn dedup(&mut self) {
        if !self.queue.is_empty() {
            let mut uniques = HashSet::new();
            self.queue.retain(|e| uniques.insert(e.id));
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use tracing_test::traced_test;

    // #[test]
    // fn can_create_inventory_file() {
    //     let runner = get_task_runner();
    //     runner.create_inventory_file("test.txt")
    // }

    fn get_task_runner() -> TaskRunner {
        TaskRunner::new(TaskRunnerOpts::from(&AgentArgs {
            keypair: Default::default(),
            cluster: Default::default(),
            channel_id: Default::default(),
            program_id: Default::default(),
            working_dir: None,
            monitor_port: 0,
        }))
    }

    #[test]
    fn test_build_name() {
        let mut runner = get_task_runner();

        let version = "test";

        let mut task = Task {
            id: 1,
            name: "test".to_string(),
            args: Default::default(),
            secret: "".to_string(),
            action: "".to_string(),
        };

        let cmd = runner.build_image_name(&task);
        assert_eq!(cmd, format!("{}:{}", ANSIBLE_IMAGE, ANSIBLE_DEFAULT_TAG));

        task.args = HashMap::from([(TASK_VERSION_ARG.to_string(), version.to_string())]);
        let cmd = runner.build_image_name(&task);
        assert_eq!(cmd, format!("{}:{}", ANSIBLE_IMAGE, version));
    }

    #[test]
    fn test_build_cmd() {
        let mut runner = get_task_runner();

        let mut task = Task {
            id: 1,
            name: "restart".to_string(),
            args: Default::default(),
            secret: "".to_string(),
            action: "".to_string(),
        };

        let cmd = runner.build_cmd(&task);

        assert_eq!(cmd[0], "ansible-playbook");
        assert_eq!(cmd[1], "--inventory=localhost,");
        assert_eq!(
            cmd[2 + TASK_EXTRA_VARS.len()],
            format!("./playbooks/{}.yaml", task.name)
        );

        env::set_var("DOCKER_HOST_IP", "1.2.3.4");
        task.args = HashMap::from([("a".to_string(), "1".to_string())]);

        let cmd = runner.build_cmd(&task);

        assert_eq!(cmd[1], "--inventory=1.2.3.4,");
        assert_eq!(cmd[2 + TASK_EXTRA_VARS.len()], "--extra-vars={\"a\":\"1\"}");
    }

    #[tokio::test]
    #[traced_test]
    async fn test_runner() {
        let mut playbooks = env::current_dir().unwrap();
        playbooks.push("ansible");

        let mut runner = get_task_runner();

        let task = Task {
            id: 1,
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

    #[test]
    fn test_notifier() {
        let mut runner = get_task_runner();
        let n = runner.notifier();
        assert_eq!(n.opts.logs_path, Some(PathBuf::from("/app/logs")));
    }
}
