use crate::notifier::Notifier;
use anyhow::{Context, Error, Result};
use std::process::Stdio;
use tokio::io::AsyncBufReadExt;
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
            monitor_port: 12345,
        }
    }

    // pub async fn start(&mut self) {
    //     loop {
    //         match self.run().await {
    //             Ok(_) => {}
    //             Err(e) => {
    //                 error!("Error: {}", e)
    //             }
    //         };
    //         info!("Waiting a command...");
    //         tokio::time::sleep(std::time::Duration::from_millis(5000)).await;
    //     }
    // }

    /// Run command from the [queue]
    #[tracing::instrument(skip(self))]
    pub async fn run(&mut self) -> Result<()> {
        if let Some(task) = self.queue.pop() {
            let notifier = Notifier::new(&task);
            notifier.notify_pre_start();

            let mut _err: Option<Error> = None;
            match self.run_task(&task) {
                Ok(mut child) => {
                    notifier.notify_start();

                    let stdout = child
                        .stdout
                        .take()
                        .expect("child did not have a handle to stdout");

                    let stderr = child
                        .stderr
                        .take()
                        .expect("child did not have a handle to stderr");

                    let mut reader_stdout = tokio::io::BufReader::new(stdout).lines();
                    let mut reader_stderr = tokio::io::BufReader::new(stderr).lines();

                    // Ensure the child process is spawned in the runtime so it can
                    // make progress on its own while we await for any output.
                    let join_handle = tokio::spawn(async move {
                        let exit_status = child
                            .wait()
                            .await
                            .expect("child process encountered an error");

                        exit_status
                    });

                    tokio::select! {
                        msg = reader_stdout.next_line() => {
                            match msg.unwrap() {
                                Some(line) => {
                                    info!(line);
                                },
                                None => {
                                    info!("EOL");
                                },
                            };
                        },
                        msg = reader_stderr.next_line() => {
                            match msg.unwrap() {
                                Some(line) => {
                                    info!(line);
                                },
                                None => {
                                    info!("EOL");
                                },
                            };
                        },
                    }

                    let exit_status = join_handle.await.ok();

                    println!("exit_status: {:?}", exit_status);

                    // tokio::select! {
                    //     child_res = child.wait() => {
                    //         info!("Finished");
                    //         if let Err(err) = child_res {
                    //             info!("Failed to run task on the replay. Error: {}", err);
                    //         }
                    //
                    //         if let Some(mut stderr) = child.stderr {
                    //             let mut res = String::new();
                    //             if stderr.read_to_string(&mut res).await.is_ok() {
                    //                 info!("Command stderr: {res}");
                    //             }
                    //             info!("Finished command stderr");
                    //         }
                    //     }
                    // }

                    // let tasklet = tokio::spawn(async { child.wait_with_output().await });

                    // this delay should give tokio::spawn plenty of time to spin up
                    // and call `wait` on the child (closing stdin)
                    // time::sleep(Duration::from_millis(2000)).await;
                    //
                    // match tasklet.await {
                    //     Ok(exit_result) => match exit_result {
                    //         Ok(exit_status) => eprintln!("exit_status: {:?}", exit_status),
                    //         Err(terminate_error) => {
                    //             eprintln!("terminate_error: {}", terminate_error)
                    //         }
                    //     },
                    //     Err(join_error) => eprintln!("join_error: {}", join_error),
                    // }

                    // let monitor = self.start_monitor(&task);

                    // match child.wait_with_output().await {
                    //     Ok(output) => {
                    //         println!("OOOKKKK");
                    //         notifier.notify_finish(&output.status);
                    //     }
                    //     Err(e) => {
                    //         println!("ERRRRROOORRR");
                    //         _err = Some(Error::from(e));
                    //     }
                    // }

                    // if monitor.is_ok() {
                    //     monitor.unwrap().kill().await?;
                    // }
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
            "-v ~/playbooks:/work:ro",
            // "-v ~/.ansible/roles:/root/.ansible/roles",
            "-v ~/.ssh:/root/.ssh:ro",
            "--rm",
            "spy86/ansible:latest",
            "ansible-playbook",
            format!("{}.yml", task.playbook).as_str(),
            format!("-e \"{}\"", task.extra_vars.as_str()).as_str(),
        ]);

        // verbosity
        // cmd.args(["-vvv"]);

        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);
        cmd.spawn().context("Running task")
    }

    #[tracing::instrument(skip(self, task))]
    fn start_monitor(&self, task: &Task) -> Result<Child> {
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
            .context("Running monitoring")
    }
}
