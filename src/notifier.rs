use crate::runner::Task;
use anyhow::Error;
use std::process::ExitStatus;
use tracing::info;

pub struct Notifier<'a> {
    task: &'a Task,
}

impl<'a> Notifier<'a> {
    pub fn new(task: &'a Task) -> Self {
        Self { task }
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
    pub fn notify_finish(&self, status: &ExitStatus) {
        info!("[Notifier] Finish: {}", status);
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
