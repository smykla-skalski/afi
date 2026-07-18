//! Scheduling boundary for terminal input, worker events, and frame cadence.

use std::{io, mem};

use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use tokio::sync::Notify;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Interval;

use super::WorkerResult;

#[derive(Default)]
pub(super) struct RenderGate {
    requested: bool,
}

impl RenderGate {
    pub(super) fn requested() -> Self {
        Self { requested: true }
    }

    pub(super) fn request(&mut self) {
        self.requested = true;
    }

    pub(super) fn take(&mut self) -> bool {
        mem::take(&mut self.requested)
    }
}

pub(super) enum DriverUpdate {
    Frame,
    Event(Option<io::Result<Event>>),
    Result(Option<WorkerResult>),
    Backend,
}

pub(super) async fn next(
    input_open: bool,
    events: &mut EventStream,
    results: &mut UnboundedReceiver<WorkerResult>,
    backend_notify: &Notify,
    frames: &mut Interval,
) -> DriverUpdate {
    tokio::select! {
        biased;
        _ = frames.tick() => DriverUpdate::Frame,
        event = events.next(), if input_open => DriverUpdate::Event(event),
        result = results.recv() => DriverUpdate::Result(result),
        () = backend_notify.notified() => DriverUpdate::Backend,
    }
}
