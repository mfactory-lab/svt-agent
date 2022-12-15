use crate::runner::Task;
use anyhow::Error;
use std::process::Output;
use tracing::info;

pub struct Notifier<'a> {
    task: &'a Task,
    webhook_url: &'a str,
}

impl<'a> Notifier<'a> {
    pub fn new(task: &'a Task) -> Self {
        Self {
            task,
            webhook_url: "",
        }
    }

    #[tracing::instrument(skip(self))]
    pub fn notify_pre_start(&self) {
        self.notify("pre_start");
    }

    #[tracing::instrument(skip(self))]
    pub fn notify_start(&self) {
        self.notify("start");
    }

    #[tracing::instrument(skip(self))]
    pub fn notify_finish(&self, output: &Output) {
        info!("[Notifier] Finish: {}", output.status);
        self.notify("finish");
    }

    #[tracing::instrument(skip(self))]
    pub fn notify_error(&self, error: &Error) {
        info!("[Notifier] Error: {}", error);
        self.notify("error");
    }

    pub fn notify(&self, event: &str) {
        // TODO: send influx state
        // TODO: send webhook
    }
}
