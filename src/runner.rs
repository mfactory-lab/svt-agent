use crate::constants::COMMAND_DELIMITER;
use crate::encryption::decrypt_message;
use crate::notifier::Notifier;
use crate::state::Message;
use anyhow::{Error, Result};
use tokio::io;
use tokio::process::Child;
use tracing::info;

#[derive(Debug)]
pub struct Task {
    /// Command numeric identifier
    id: u64,
    /// Unique command identifier, used as password for monitoring
    uuid: String,
    /// Ansible playbook name
    playbook: String,
    /// Added to the ansible-playbook command as `--extra-vars`
    extra_vars: String,
}

pub struct TaskRunner {
    /// Used fom message decryption
    cek: Vec<u8>,
    /// Commands queue
    queue: Vec<Task>,
}

impl TaskRunner {
    pub fn new(cek: Vec<u8>) -> Self {
        Self {
            cek,
            queue: Vec::new(),
        }
    }

    pub fn watch() {
        todo!();
    }

    /// Add new [msg] to the commands queue
    pub fn push_message(&mut self, msg: Message) -> Result<()> {
        let cmd = String::from_utf8(decrypt_message(msg.content, self.cek.as_slice())?)?;

        let parts = cmd.split(COMMAND_DELIMITER).collect::<Vec<_>>();

        if parts.len() < 3 {
            return Err(Error::msg("Invalid command"));
        }

        let playbook = String::from(parts[0]);

        // TODO: validate
        let extra_vars = String::from(parts[1]);
        let uuid = String::from(parts[2]);

        self.queue.push(Task {
            id: msg.id,
            playbook,
            extra_vars,
            uuid,
        });

        Ok(())
    }

    /// Run command from the [queue]
    #[tracing::instrument(skip(self))]
    pub async fn run(&mut self) {
        if let Some(task) = self.queue.pop() {
            let notifier = Notifier::new(&task);
            notifier.notify_pre_start();
            match self.run_task(&task) {
                Ok(mut child) => {
                    notifier.notify_start();
                    let mut monitor = self
                        .start_monitor(&task)
                        .map_err(|e| {
                            notifier.notify_error(Error::from(e));
                        })
                        .expect("Failed to start monitor");
                    let status = child
                        .wait()
                        .await
                        .map_err(|e| {
                            notifier.notify_error(Error::from(e));
                        })
                        .expect("Failed to run task");
                    notifier.notify_finish(status);
                    monitor.kill();
                }
                Err(e) => {
                    notifier.notify_error(Error::from(e));
                }
            }
        }
    }

    #[tracing::instrument(skip(self))]
    fn run_task(&self, task: &Task) -> io::Result<Child> {
        // Command can be run isolated in the docker container
        // let cmd = tokio::process::Command::new("docker")
        //     .args(["run", "hugozzys/ansible-deploy:latest", "-e a=1", "-vvv"])
        //     .kill_on_drop(true)
        //     .spawn()?;
        let playbook = format!("~/playbooks/{}.yaml", task.playbook.as_str()).as_str();
        let verbose = "-vvv";
        // TODO: validate file exists
        tokio::process::Command::new("ansible-playbook")
            .args(&[
                playbook,
                "hugozzys/ansible-deploy:latest",
                format!("-e \"{}\"", task.extra_vars.as_str()).as_str(),
                verbose,
            ])
            .kill_on_drop(true)
            .spawn()
    }

    #[tracing::instrument(skip(self, task))]
    fn start_monitor(&self, task: &Task) -> io::Result<Child> {
        tokio::process::Command::new("docker")
            .args(&[
                "run",
                "-p 8888:8080",
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
