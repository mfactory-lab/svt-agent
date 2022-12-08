use crate::runner::Task;
use anyhow::Error;
use std::process::ExitStatus;
use tracing::info;

pub struct Notifier<'a> {
    task: &'a Task,
}

impl<'a> Notifier<'_> {
    pub fn new(task: &Task) -> Self {
        Self { task }
    }

    pub fn notify_pre_start(&self) {
        self.notify("pre_start");
    }

    pub fn notify_start(&self) {
        self.notify("start");
    }

    pub fn notify_finish(&self, status: ExitStatus) {
        info!("[Notifier] Finish: {}", status);
        self.notify("finish");
    }

    pub fn notify_error(&self, error: Error) {
        info!("[Notifier] Error: {}", error);
        self.notify("error");
    }

    pub fn notify(&self, event: &str) {
        todo!()
        // send influx state
        // send webhook
    }
}
