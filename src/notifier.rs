use crate::constants::*;
use crate::task_runner::Task;
use anchor_client::solana_sdk::pubkey::Pubkey;
use anchor_client::Cluster;
use anyhow::Error;
use anyhow::Result;
use chrono::Utc;
use influxdb::{InfluxDbWriteable, ReadQuery, Timestamp};
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tracing::{error, info, warn};
use tracing_subscriber::filter::combinator::Not;

#[derive(Default, Clone)]
pub struct NotifierOpts {
    pub channel_id: Pubkey,
    pub cluster: Cluster,
    pub logs_path: Option<PathBuf>,
    pub webhook_url: String,
    pub influx_url: String,
    pub influx_db: String,
    pub influx_user: String,
    pub influx_pass: String,
}

impl NotifierOpts {
    pub fn new() -> Self {
        Self {
            influx_url: std::env::var("AGENT_NOTIFY_INFLUX_URL")
                .unwrap_or_else(|_| NOTIFY_INFLUX_URL.to_string()),
            influx_db: std::env::var("AGENT_NOTIFY_INFLUX_DB")
                .unwrap_or_else(|_| NOTIFY_INFLUX_DB.to_string()),
            influx_user: std::env::var("AGENT_NOTIFY_INFLUX_USER")
                .unwrap_or_else(|_| NOTIFY_INFLUX_USER.to_string()),
            influx_pass: std::env::var("AGENT_NOTIFY_INFLUX_PASSWORD")
                .unwrap_or_else(|_| NOTIFY_INFLUX_PASS.to_string()),
            ..Default::default()
        }
    }

    pub fn with_influx<T: Into<String>>(mut self, url: T, db: T) -> Self {
        self.influx_url = url.into();
        self.influx_db = db.into();
        self
    }

    pub fn with_influx_auth<T: Into<String>>(mut self, user: T, pass: T) -> Self {
        self.influx_user = user.into();
        self.influx_pass = pass.into();
        self
    }

    pub fn with_channel_id(mut self, channel_id: Pubkey) -> Self {
        self.channel_id = channel_id;
        self
    }

    pub fn with_cluster(mut self, cluster: Cluster) -> Self {
        self.cluster = cluster;
        self
    }

    pub fn with_webhook<T: Into<String>>(mut self, url: T) -> Self {
        self.webhook_url = url.into();
        self
    }

    pub fn with_logs_path(mut self, path: PathBuf) -> Self {
        self.logs_path = Some(path);
        self
    }
}

pub struct Notifier<'a> {
    /// The [Task] which will be notified
    task: &'a Task,
    /// Notifier options
    opts: &'a NotifierOpts,
    /// Parameters to be sent
    params: HashMap<&'a str, String>,
}

impl<'a> Notifier<'a> {
    pub fn new(opts: &'a NotifierOpts, task: &'a Task) -> Self {
        Self {
            task,
            opts,
            params: Default::default(),
        }
    }

    pub async fn notify_start(&self) -> Result<()> {
        self.notify("start").await
    }

    pub async fn notify_finish(&mut self, status_code: u64, output: String) -> Result<()> {
        self.params.insert("status_code", status_code.to_string());
        self.params.insert("output", output);
        self.notify("finish").await
    }

    pub async fn notify_error(&mut self, error: &'a Error) -> Result<()> {
        self.params.insert("error", error.to_string());
        self.notify("error").await
    }

    #[tracing::instrument(skip_all)]
    async fn notify(&self, event: &str) -> Result<()> {
        info!("Task #{} notify({})", self.task.id, event);

        let (save_result, influx_result, webhook_result) = tokio::join!(
            self.save_to_file(),
            self.notify_influx(event),
            self.notify_webhook(event),
        );

        if let Err(e) = save_result {
            warn!("[File] {}", e);
        }

        if let Err(e) = influx_result {
            warn!("[Influx] {}", e);
        }

        webhook_result?;

        Ok(())
    }

    /// Try to save output to log file
    #[tracing::instrument(skip_all)]
    async fn save_to_file(&self) -> Result<()> {
        if let Some(path) = &self.opts.logs_path {
            if let Some(data) = self
                .params
                .get("output")
                .or_else(|| self.params.get("error"))
            {
                let file_name = format!("{}_{}.log", self.task.id, Utc::now().format("%Y%m%d%H%M"));

                let path = path.join(&file_name);

                match OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(&path)
                    .await
                {
                    Ok(mut file) => {
                        if let Err(e) = file.write_all(data.as_bytes()).await {
                            error!("Unable to write log data. {}", e);
                        }
                    }
                    Err(e) => {
                        error!(
                            "Unable to open log file ({}). {}",
                            path.to_str().unwrap_or("?"),
                            e
                        );
                    }
                }
            }
        }
        Ok(())
    }

    /// Send notification to custom url
    #[tracing::instrument(skip_all)]
    async fn notify_webhook<T: Into<String>>(&self, event: T) -> Result<()> {
        if !self.opts.webhook_url.is_empty() {
            let client = hyper::Client::new();

            let mut params = self.params.clone();
            params.insert("cluster", self.opts.cluster.to_string());
            params.insert("channel_id", self.opts.channel_id.to_string());
            params.insert("task_id", self.task.id.to_string());
            params.insert("event", event.into());

            let data = json!(params);

            let req = hyper::Request::builder()
                .method(hyper::Method::POST)
                .uri(&self.opts.webhook_url)
                .header("content-type", "application/json")
                .body(hyper::Body::from(data.to_string()))?;

            info!("[Webhook] Request: {:?}", req);

            let resp = client.request(req).await?;

            info!("[Webhook] Response: {:?}", resp);
        }
        Ok(())
    }

    /// Send notification to the influx
    #[tracing::instrument(skip_all)]
    async fn notify_influx<T: Into<String>>(&self, event: T) -> Result<()> {
        let client = self.get_influx_client()?;

        let channel_id = self.opts.channel_id.to_string();
        if channel_id.is_empty() {
            return Err(Error::msg("Channel id required"));
        }

        let write_query = Timestamp::from(Utc::now())
            .into_query("task_logs")
            .add_field("cluster", self.opts.cluster.to_string())
            .add_field("channel_id", self.opts.channel_id.to_string())
            .add_field("task_id", self.task.id)
            .add_field("task_name", self.task.name.to_string())
            .add_tag("event", event.into());

        info!("[Influx] Request `{:?}` ...", write_query);

        let res = client.query(write_query).await?;

        info!("[Influx] Response `{}` ...", res);

        Ok(())
    }

    fn get_influx_client(&self) -> Result<influxdb::Client> {
        let is_active = std::env::var("AGENT_NOTIFY_INFLUX")
            .map(|v| v != "false")
            .unwrap_or(true);

        if !is_active {
            return Err(Error::msg("Influx is inactive"));
        }

        let client = influxdb::Client::new(&self.opts.influx_url, &self.opts.influx_db);
        if !&self.opts.influx_user.is_empty() && !&self.opts.influx_pass.is_empty() {
            Ok(client.with_auth(&self.opts.influx_user, &self.opts.influx_pass))
        } else {
            Ok(client)
        }
    }
}

#[tokio::test]
async fn test_notify() {
    let task = Task {
        id: 0,
        name: "restart".to_string(),
        args: Default::default(),
        secret: "".to_string(),
        action: "".to_string(),
    };

    // let client = Notifier::get_influx_client().unwrap();
    //
    // let query = format!("CREATE DATABASE {}", client.database_name());
    // client
    //     .query(ReadQuery::new(query))
    //     .await
    //     .expect("could not setup db");
    //
    // let notifier = Notifier::new(&task)
    //     .with_webhook("http://localhost")
    //     .with_logs_path(PathBuf::from("./"));
    //
    // notifier.notify_start().await;
    //
    // // Let's see if the data we wrote is there
    // let read_query = ReadQuery::new("SELECT * FROM task_logs");
    // let read_result = client.query(read_query).await;
    //
    // println!("{:?}", read_result);
}
