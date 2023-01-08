use crate::constants::CONTAINER_NAME;
use crate::task_runner::Task;
use anyhow::Error;
use anyhow::Result;
use chrono::Utc;
use influxdb::{InfluxDbWriteable, Timestamp};
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tracing::{error, info, warn};

pub struct Notifier<'a> {
    task: &'a Task,
    webhook_url: &'a str,
    logs_path: Option<PathBuf>,
    params: HashMap<&'a str, String>,
}

impl<'a> Notifier<'a> {
    pub fn new(task: &'a Task) -> Self {
        Self {
            task,
            webhook_url: Default::default(),
            logs_path: None,
            params: HashMap::default(),
        }
    }

    pub fn with_webhook(mut self, url: &'a str) -> Self {
        self.webhook_url = url;
        self
    }

    pub fn with_logs_path(mut self, path: PathBuf) -> Self {
        self.logs_path = Some(path);
        self
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
            warn!("[File] Error: {}", e);
        }

        if let Err(e) = influx_result {
            warn!("[Influx] Error: {}", e);
        }

        webhook_result?;

        Ok(())
    }

    /// Try to save output to log file
    #[tracing::instrument(skip_all)]
    async fn save_to_file(&self) -> Result<()> {
        if let Some(path) = &self.logs_path {
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
        if !self.webhook_url.is_empty() {
            let client = hyper::Client::new();

            let mut params = self.params.clone();
            params.insert("task_id", self.task.id.to_string());
            params.insert("event", event.into());

            let data = json!(params);

            let req = hyper::Request::builder()
                .method(hyper::Method::POST)
                .uri(self.webhook_url)
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
        let client = {
            let url = std::env::var("AGENT_NOTIFY_INFLUX_URL").unwrap_or_default();

            if url.is_empty() {
                return Err(Error::msg("[Influx] url is not specified"));
            }

            let db = std::env::var("AGENT_NOTIFY_INFLUX_DB")
                .unwrap_or_else(|_| CONTAINER_NAME.to_string());
            let user = std::env::var("AGENT_NOTIFY_INFLUX_USER").unwrap_or_default();
            let password = std::env::var("AGENT_NOTIFY_INFLUX_PASSWORD").unwrap_or_default();

            let client = influxdb::Client::new(url, db);
            if !user.is_empty() && !password.is_empty() {
                client.with_auth(user, password)
            } else {
                client
            }
        };

        // let query = format!("CREATE DATABASE {}", client.database_name());
        // client
        //     .query(ReadQuery::new(query))
        //     .await
        //     .expect("could not setup db");

        let write_query = Timestamp::from(Utc::now())
            .into_query("task_logs")
            .add_field("task_id", self.task.id)
            .add_field("task_name", self.task.name.to_string())
            .add_tag("event", event.into());

        info!("[Influx] Request `{:?}` ...", write_query);

        let res = client.query(write_query).await?;

        info!("[Influx] Response `{}` ...", res);

        Ok(())
    }
}
