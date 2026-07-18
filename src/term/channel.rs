//! Channel-backed UI adapter used by async REPL workers.

use std::sync::{
    Arc,
    mpsc::{self, SyncSender},
};

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use super::{OutputEvent, UserInterface};
use crate::risk::ApprovalChoice;

/// Backend-to-frontend event. Only frontend owns terminal and input stream.
pub(crate) enum BackendEvent {
    Output(OutputEvent),
    ActivityStarted {
        label: String,
        cancel: CancellationToken,
    },
    ActivityStopped,
    ApprovalRequested {
        prompt: String,
        reply: mpsc::SyncSender<ApprovalChoice>,
    },
}

/// Sends typed events from model/repl workers to fullscreen app.
pub(crate) struct ChannelUi {
    tx: SyncSender<BackendEvent>,
    notify: Arc<Notify>,
    task_cancel: CancellationToken,
}

impl ChannelUi {
    #[must_use]
    pub(crate) fn new(tx: SyncSender<BackendEvent>, notify: Arc<Notify>) -> Self {
        Self::with_cancel(tx, notify, CancellationToken::new())
    }

    #[must_use]
    pub(crate) fn with_cancel(
        tx: SyncSender<BackendEvent>,
        notify: Arc<Notify>,
        task_cancel: CancellationToken,
    ) -> Self {
        Self {
            tx,
            notify,
            task_cancel,
        }
    }

    fn send(&self, event: BackendEvent) -> bool {
        if self.tx.send(event).is_ok() {
            self.notify.notify_one();
            true
        } else {
            false
        }
    }
}

impl UserInterface for ChannelUi {
    fn emit(&mut self, event: OutputEvent) {
        self.send(BackendEvent::Output(event));
    }

    fn start_activity(&mut self, label: &str) -> CancellationToken {
        let cancel = self.task_cancel.clone();
        self.send(BackendEvent::ActivityStarted {
            label: label.to_string(),
            cancel: cancel.clone(),
        });
        cancel
    }

    fn stop_activity(&mut self) {
        self.send(BackendEvent::ActivityStopped);
    }

    fn approve(&mut self, prompt: &str) -> ApprovalChoice {
        if self.task_cancel.is_cancelled() {
            return ApprovalChoice::Esc;
        }
        let (reply, answer) = mpsc::sync_channel(1);
        if !self.send(BackendEvent::ApprovalRequested {
            prompt: prompt.to_string(),
            reply,
        }) {
            return ApprovalChoice::No;
        }
        answer.recv().unwrap_or(ApprovalChoice::No)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use tokio::runtime::Handle;
    use tokio::task::spawn_blocking;

    fn message(text: &str) -> OutputEvent {
        OutputEvent::Message {
            kind: super::super::MessageKind::Info,
            text: text.to_string(),
        }
    }

    #[test]
    fn activity_uses_job_level_cancellation() {
        let (tx, _rx) = mpsc::sync_channel(2);
        let task_cancel = CancellationToken::new();
        let mut ui = ChannelUi::with_cancel(tx, Arc::new(Notify::new()), task_cancel.clone());
        let activity_cancel = ui.start_activity("thinking");
        task_cancel.cancel();
        assert!(activity_cancel.is_cancelled());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn saturated_channel_is_safe_inside_async_worker() {
        let (tx, rx) = mpsc::sync_channel(1);
        let notify = Arc::new(Notify::new());
        let handle = Handle::current();
        let worker = spawn_blocking(move || {
            handle.block_on(async move {
                let mut ui = ChannelUi::new(tx, notify);
                ui.emit(message("first"));
                ui.emit(message("second"));
            });
        });

        let first = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let second = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        worker.await.unwrap();

        assert!(matches!(first, BackendEvent::Output(event) if event == message("first")));
        assert!(matches!(second, BackendEvent::Output(event) if event == message("second")));
    }
}
