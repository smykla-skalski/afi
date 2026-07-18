//! Scheduling boundary for terminal input, worker events, and frame cadence.

use std::{io, mem};

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent};
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

fn classify_event(event: Option<io::Result<Event>>, frames: &mut Interval) -> DriverUpdate {
    if matches!(
        &event,
        Some(Ok(Event::Resize(_, _)
            | Event::Key(KeyEvent {
                code: KeyCode::Up | KeyCode::Down,
                ..
            })))
    ) {
        frames.reset_immediately();
    }
    DriverUpdate::Event(event)
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
        event = events.next(), if input_open => classify_event(event, frames),
        result = results.recv() => DriverUpdate::Result(result),
        () = backend_notify.notified() => DriverUpdate::Backend,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crossterm::event::KeyModifiers;
    use tokio::time::{Instant, interval_at, timeout};

    use super::*;

    async fn assert_immediate_frame(event: Event) {
        let minute = Duration::from_mins(1);
        let mut frames = interval_at(Instant::now() + minute, minute);

        let update = classify_event(Some(Ok(event)), &mut frames);

        assert!(matches!(update, DriverUpdate::Event(Some(Ok(_)))));
        timeout(Duration::from_millis(50), frames.tick())
            .await
            .expect("layout frame should be ready");
    }

    #[tokio::test]
    async fn layout_sensitive_events_schedule_an_immediate_frame() {
        assert_immediate_frame(Event::Resize(80, 24)).await;
        assert_immediate_frame(Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))).await;
        assert_immediate_frame(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::ALT))).await;
    }
}
