use anyhow::Context;
use std::process::Stdio;
use tokio::process::{Child, Command};
use tracing::info;

///
/// Think about web endpoint with `docker logs` output
///

const DEFAULT_USERNAME: &str = "admin";

pub struct TaskMonitor<'a> {
    port: u16,
    username: &'a str,
    password: Option<&'a str>,
    filter: &'a str,
    process: Option<Child>,
}

impl<'a> TaskMonitor<'a> {
    pub fn new(port: u16, filter: &'a str, password: Option<&'a str>) -> Self {
        Self {
            port,
            filter,
            password,
            username: DEFAULT_USERNAME,
            process: None,
        }
    }

    #[tracing::instrument(skip(self))]
    pub fn start(&mut self) -> anyhow::Result<()> {
        let mut cmd = Command::new("docker");

        cmd.args([
            "run",
            "-v",
            "/var/run/docker.sock:/var/run/docker.sock",
            "-p",
            &format!("{}:8080", self.port),
            "--rm",
            "amir20/dozzle:latest",
            "--filter",
            &format!("name={}", self.filter),
            "--no-analytics",
        ]);

        if let Some(password) = self.password {
            if !password.is_empty() {
                cmd.args(["--username", self.username, "--password", password]);
            }
        }

        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
        cmd.kill_on_drop(true);

        self.process = Some(cmd.spawn().context("TaskMonitor")?);

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub async fn stop(&mut self) -> std::io::Result<()> {
        if let Some(child) = &mut self.process {
            Command::new("kill")
                .args(["-s", "TERM", &child.id().unwrap().to_string()])
                .spawn()?
                .wait()
                .await?;
            // TODO: `child.kill` doesnt work
            // return child.kill().await;
        }
        info!("Monitor is not started...");
        Ok(())
    }
}
