use crate::runner::Task;
use anyhow::Error;
use anyhow::Result;
use serde_json::json;
use tracing::info;

pub struct Notifier<'a> {
    task: &'a Task,
    webhook_url: &'a str,
    error: Option<&'a Error>,
    output: String,
}

impl<'a> Notifier<'a> {
    pub fn new(task: &'a Task) -> Self {
        Self {
            task,
            webhook_url: "",
            error: None,
            output: "".to_string(),
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
    pub async fn notify_finish(&mut self, output: String) -> Result<()> {
        self.output = output;
        self.notify("finish").await
    }

    #[tracing::instrument(skip(self))]
    pub async fn notify_error(&mut self, error: &'a Error) -> Result<()> {
        self.error = Some(error);
        self.notify("error").await
    }

    async fn notify(&self, event: &str) -> Result<()> {
        info!("Task#{} notify_{}", self.task.id, event);

        // TODO: send influx state

        if !self.webhook_url.is_empty() {
            let client = hyper::Client::new();

            let error = self.error.map(|e| e.to_string());

            let data = json!({
                "task": self.task.id,
                "event": event,
                "output": self.output,
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
