use crate::notifier::Notifier;
use anyhow::{Error, Result};
use tokio::io;
use tokio::process::Child;
use tokio::process::Command;
use tracing::{error, info};

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
    monitor_port: u16,
}

impl TaskRunner {
    pub fn new() -> Self {
        Self {
            queue: Vec::new(),
            monitor_port: 8888,
        }
    }

    pub async fn start(&mut self) {
        loop {
            info!("Try to run a command...");
            match self.run().await {
                Ok(_) => {}
                Err(e) => {
                    error!("Error: {}", e)
                }
            };
            info!("Waiting...");
            tokio::time::sleep(std::time::Duration::from_millis(5000)).await;
        }
    }

    /// Run command from the [queue]
    #[tracing::instrument(skip(self))]
    pub async fn run(&mut self) -> Result<()> {
        if let Some(task) = self.queue.pop() {
            let notifier = Notifier::new(&task);
            notifier.notify_pre_start();
            match self.run_task(&task) {
                Ok(mut child) => {
                    notifier.notify_start();
                    let mut monitor = self.start_monitor(&task).map_err(|e| {
                        let err = Error::from(e);
                        notifier.notify_error(&err);
                        err
                    })?;
                    let status = child.wait().await.map_err(|e| {
                        let err = Error::from(e);
                        notifier.notify_error(&err);
                        err
                    })?;
                    notifier.notify_finish(&status);
                    monitor.kill().await?;
                }
                Err(e) => {
                    notifier.notify_error(&Error::from(e));
                }
            }
        }
        Ok(())
    }

    /// Add new [task] to the [queue]
    pub fn add_task(&mut self, task: Task) {
        self.queue.push(task);
    }

    #[tracing::instrument(skip(self))]
    fn run_task(&self, task: &Task) -> io::Result<Child> {
        // Command can be run isolated in the docker container
        // let cmd = tokio::process::Command::new("docker")
        //     .args(["run", "hugozzys/ansible-deploy:latest", "-e a=1", "-vvv"])
        //     .kill_on_drop(true)
        //     .spawn()?;
        let verbose = "-vvv";
        // TODO: validate file exists
        Command::new("ansible-playbook")
            .args([
                format!("~/playbooks/{}.yaml", task.playbook).as_str(),
                "hugozzys/ansible-deploy:latest",
                format!("-e \"{}\"", task.extra_vars.as_str()).as_str(),
                verbose,
            ])
            .kill_on_drop(true)
            .spawn()
    }

    #[tracing::instrument(skip(self, task))]
    fn start_monitor(&self, task: &Task) -> io::Result<Child> {
        Command::new("docker")
            .args([
                "run",
                format!("-p {}:8080", self.monitor_port).as_str(),
                "-v /var/run/docker.sock:/var/run/docker.sock",
                "amir20/dozzle:latest",
            ])
            .env("DOZZLE_FILTER", "svt-agent")
            .env("DOZZLE_NO_ANALYTICS", "true")
            .env("DOZZLE_USERNAME", "admin")
            .env("DOZZLE_PASSWORD", task.uuid.as_str())
            .kill_on_drop(true)
            .spawn()
    }
}
