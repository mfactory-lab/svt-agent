use crate::runner::Task;
use anyhow::Error;
use anyhow::Result;
use serde_json::json;
use std::process::Output;
use tracing::info;

pub struct Notifier<'a> {
    task: &'a Task,
    webhook_url: &'a str,
    error: Option<&'a Error>,
    output: Option<&'a Output>,
}

impl<'a> Notifier<'a> {
    pub fn new(task: &'a Task) -> Self {
        Self {
            task,
            webhook_url: "",
            error: None,
            output: None,
        }
    }

    #[tracing::instrument(skip(self))]
    pub async fn notify_pre_start(&self) -> Result<()> {
        self.notify("pre_start").await
    }

    #[tracing::instrument(skip(self))]
    pub async fn notify_start(&self) -> Result<()> {
        self.notify("start").await
    }

    #[tracing::instrument(skip(self))]
    pub async fn notify_finish(&mut self, output: &'a Output) -> Result<()> {
        self.output = Some(output);
        self.notify("finish").await
    }

    #[tracing::instrument(skip(self))]
    pub async fn notify_error(&mut self, error: &'a Error) -> Result<()> {
        self.error = Some(error);
        self.notify("error").await
    }

    pub async fn notify(&self, event: &str) -> Result<()> {
        // TODO: send influx state

        if !self.webhook_url.is_empty() {
            let client = hyper::Client::new();

            let status = self.output.map(|s| s.status.to_string());
            let error = self.error.map(|e| e.to_string());
            let stdout = self
                .output
                .map(|s| String::from_utf8(s.stdout.clone()).unwrap());

            let data = json!({
                "task": self.task.id,
                "event": event,
                "status": status,
                "stdout": stdout,
                "error": error,
            });

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
}
