use crate::constants::CONTAINER_NAME;
use crate::task_runner::Task;
use anyhow::Error;
use anyhow::Result;
use chrono::Utc;
use influxdb::{InfluxDbWriteable, Timestamp};
use serde_json::json;
use std::collections::HashMap;
use tracing::info;

pub struct Notifier<'a> {
    task: &'a Task,
    webhook_url: &'a str,
    /// Data that will be send
    params: HashMap<&'a str, String>,
}

impl<'a> Notifier<'a> {
    pub fn new(task: &'a Task) -> Self {
        Self {
            task,
            webhook_url: Default::default(),
            params: HashMap::default(),
        }
    }

    pub fn with_webhook(mut self, url: &'a str) -> Self {
        self.webhook_url = url;
        self
    }

    #[tracing::instrument(skip(self))]
    pub async fn notify_start(&self) -> Result<()> {
        self.notify("start").await
    }

    #[tracing::instrument(skip(self))]
    pub async fn notify_finish(&mut self, status_code: u64, output: String) -> Result<()> {
        self.params.insert("status_code", status_code.to_string());
        self.params.insert("output", output);
        self.notify("finish").await
    }

    #[tracing::instrument(skip(self))]
    pub async fn notify_error(&mut self, error: &'a Error) -> Result<()> {
        self.params.insert("error", error.to_string());
        self.notify("error").await
    }

    async fn notify(&self, event: &str) -> Result<()> {
        info!("Task#{} notify_{}", self.task.id, event);

        let (_, webhook_result) =
            tokio::join!(self.notify_influx(event), self.notify_webhook(event));

        webhook_result?;

        Ok(())
    }

    /// Send notification to custom url
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
    async fn notify_influx<T: Into<String>>(&self, event: T) -> Result<()> {
        let client = {
            let url = std::env::var("AGENT_NOTIFY_INFLUX_URL").unwrap_or_default();
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
