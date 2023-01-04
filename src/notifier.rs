use crate::runner::Task;
use anyhow::Error;
use anyhow::Result;
use serde_json::json;
use std::collections::HashMap;
use tracing::info;

pub struct Notifier<'a> {
    task: &'a Task,
    webhook_url: &'a str,
    params: HashMap<&'a str, String>,
}

impl<'a> Notifier<'a> {
    pub fn new(task: &'a Task) -> Self {
        Self {
            task,
            webhook_url: "",
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

        self.notify_influx(event);

        if !self.webhook_url.is_empty() {
            let client = hyper::Client::new();

            let mut params = self.params.clone();
            params.insert("task_id", self.task.id.to_string());
            params.insert("event", event.to_string());

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

    fn notify_influx(&self, event: &str) {
        // ignore self.params['output']
        // TODO:
    }
}
