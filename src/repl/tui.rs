//! Fullscreen REPL driver. This is the only owner of terminal state, input,
//! rendering, and the terminal title while the interactive frontend is active.

use std::io;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use crossterm::event::{self, Event, EventStream};
use crossterm::execute;
use crossterm::terminal::SetTitle;
use tokio::runtime::Handle;
use tokio::sync::Notify;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::spawn_blocking;
use tokio::time::{Instant, Interval, MissedTickBehavior, interval_at};
use tokio_util::sync::CancellationToken;

use super::core::{CoreAction, ReplCore};
use crate::config::Runtime;
use crate::risk::ApprovalChoice;
use crate::term::tui::{InputAction, TuiApp};
use crate::term::{BackendEvent, ChannelUi};

mod update;
use update::RenderGate;

const BACKEND_CAPACITY: usize = 256;
const FRAME_INTERVAL: Duration = Duration::from_millis(33);
const THROBBER_INTERVAL: Duration = Duration::from_millis(80);

pub(super) async fn run(rt: Runtime) -> io::Result<Runtime> {
    let mut terminal = match ratatui::try_init() {
        Ok(terminal) => terminal,
        Err(error) => {
            ratatui::restore();
            return Err(error);
        }
    };
    let guard = TerminalGuard::enter()?;
    let (backend_tx, backend_rx) = mpsc::sync_channel(BACKEND_CAPACITY);
    let backend_notify = Arc::new(Notify::new());
    let mut ui = ChannelUi::new(backend_tx.clone(), backend_notify.clone());
    let core = ReplCore::new(rt, &mut ui);
    let mut driver = Driver::new(core, backend_tx, backend_rx, backend_notify);
    driver.drain_backend();
    let loop_result = driver.run(&mut terminal).await;
    if loop_result.is_err() {
        driver.cancel_current();
    }
    driver.deny_pending_approval();
    let outcome = loop_result.and_then(|()| driver.finish());
    drop(terminal);
    drop(guard);
    let (runtime, hint) = outcome?;
    if let Some(hint) = hint {
        println!("{hint}");
    }
    Ok(runtime)
}

struct Driver {
    app: TuiApp,
    core: Option<ReplCore>,
    backend_tx: mpsc::SyncSender<BackendEvent>,
    backend_rx: mpsc::Receiver<BackendEvent>,
    backend_notify: Arc<Notify>,
    result_tx: UnboundedSender<WorkerResult>,
    result_rx: UnboundedReceiver<WorkerResult>,
    cancel: Option<CancellationToken>,
    approval_reply: Option<mpsc::SyncSender<ApprovalChoice>>,
    shutdown_requested: bool,
    input_open: bool,
    exit_hint: Option<String>,
    worker_error: Option<String>,
    render: RenderGate,
    last_throbber: Instant,
    exit: bool,
}

impl Driver {
    fn new(
        core: ReplCore,
        backend_tx: mpsc::SyncSender<BackendEvent>,
        backend_rx: mpsc::Receiver<BackendEvent>,
        backend_notify: Arc<Notify>,
    ) -> Self {
        let (result_tx, result_rx) = unbounded_channel();
        Self {
            app: TuiApp::new(),
            core: Some(core),
            backend_tx,
            backend_rx,
            backend_notify,
            result_tx,
            result_rx,
            cancel: None,
            approval_reply: None,
            shutdown_requested: false,
            input_open: true,
            exit_hint: None,
            worker_error: None,
            render: RenderGate::requested(),
            last_throbber: Instant::now(),
            exit: false,
        }
    }

    async fn run(&mut self, terminal: &mut ratatui::DefaultTerminal) -> io::Result<()> {
        let mut events = EventStream::new();
        let mut frames = interval_at(Instant::now() + FRAME_INTERVAL, FRAME_INTERVAL);
        frames.set_missed_tick_behavior(MissedTickBehavior::Skip);
        self.render_if_requested(terminal)?;
        while !self.exit {
            self.wait_for_update(terminal, &mut events, &mut frames)
                .await?;
        }
        Ok(())
    }

    async fn wait_for_update(
        &mut self,
        terminal: &mut ratatui::DefaultTerminal,
        events: &mut EventStream,
        frames: &mut Interval,
    ) -> io::Result<()> {
        let next = update::next(
            self.input_open,
            events,
            &mut self.result_rx,
            &self.backend_notify,
            frames,
        )
        .await;
        match next {
            update::DriverUpdate::Frame => self.render_frame(terminal)?,
            update::DriverUpdate::Event(event) => self.handle_event(event)?,
            update::DriverUpdate::Result(result) => self.handle_result(result),
            update::DriverUpdate::Backend => self.drain_backend(),
        }
        Ok(())
    }

    fn render_if_requested(&mut self, terminal: &mut ratatui::DefaultTerminal) -> io::Result<()> {
        if self.render.take() {
            terminal.draw(|frame| self.app.render(frame))?;
        }
        Ok(())
    }

    fn render_frame(&mut self, terminal: &mut ratatui::DefaultTerminal) -> io::Result<()> {
        if self.app.is_busy() && self.last_throbber.elapsed() >= THROBBER_INTERVAL {
            self.app.tick();
            self.render.request();
            self.last_throbber = Instant::now();
        }
        self.render_if_requested(terminal)
    }

    fn handle_event(&mut self, event: Option<io::Result<Event>>) -> io::Result<()> {
        self.render.request();
        match event {
            Some(Ok(Event::Key(key))) => {
                let action = self.app.handle_key(key);
                self.resolve_approval();
                self.handle_action(action);
            }
            Some(Ok(Event::Paste(text))) => self.app.paste(&text),
            Some(Ok(Event::Mouse(mouse))) => self.app.handle_mouse(mouse),
            Some(Ok(_)) => {}
            Some(Err(error)) => return Err(error),
            None => {
                self.input_open = false;
                self.start_shutdown();
            }
        }
        Ok(())
    }

    fn handle_action(&mut self, action: InputAction) {
        match action {
            InputAction::None => {}
            InputAction::Submit(input) => self.start_job(CoreJob::Input(input)),
            InputAction::Quit => self.start_shutdown(),
            InputAction::CancelTask => {
                if let Some(cancel) = &self.cancel {
                    cancel.cancel();
                }
            }
        }
    }

    fn start_shutdown(&mut self) {
        self.shutdown_requested = true;
        self.answer_pending_approval(ApprovalChoice::Esc);
        if self.core.is_some() {
            self.start_job(CoreJob::Shutdown);
        } else if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
    }

    fn start_job(&mut self, job: CoreJob) {
        let Some(core) = self.core.take() else {
            return;
        };
        let cancel = CancellationToken::new();
        self.cancel = Some(cancel.clone());
        self.app.set_task_running(true);
        set_title("afi working");
        spawn_core(
            core,
            job,
            self.backend_tx.clone(),
            self.backend_notify.clone(),
            self.result_tx.clone(),
            cancel,
        );
    }

    fn handle_backend(&mut self, event: BackendEvent) {
        self.render.request();
        match event {
            BackendEvent::Output(output) => self.app.apply_output(output),
            BackendEvent::ActivityStarted { label, cancel } => {
                self.cancel = Some(cancel);
                self.app.set_activity(Some(label));
            }
            BackendEvent::ActivityStopped => {
                self.app.set_activity(None);
            }
            BackendEvent::ApprovalRequested { prompt, reply } => {
                if approval_is_cancelled(self.shutdown_requested, self.cancel.as_ref()) {
                    let _ = reply.send(ApprovalChoice::Esc);
                    return;
                }
                self.deny_pending_approval();
                self.approval_reply = Some(reply);
                self.app.set_approval(Some(prompt));
            }
        }
    }

    fn handle_result(&mut self, result: Option<WorkerResult>) {
        self.render.request();
        self.drain_backend();
        let Some(result) = result else {
            self.exit = true;
            return;
        };
        let (core, action) = match result {
            WorkerResult::Done(core, action) => (*core, action),
            WorkerResult::Failed(error) => {
                self.worker_error = Some(error);
                self.exit = true;
                return;
            }
        };
        if action.should_quit() {
            self.exit_hint = Some(core.resume_hint());
        }
        self.core = Some(core);
        self.cancel = None;
        self.app.set_activity(None);
        self.app.set_task_running(false);
        set_title("afi idle");
        if action.should_quit() {
            self.exit = true;
        } else if self.shutdown_requested {
            self.start_job(CoreJob::Shutdown);
        }
    }

    fn resolve_approval(&mut self) {
        let Some(choice) = self.app.take_approval_choice() else {
            return;
        };
        if let Some(reply) = self.approval_reply.take() {
            let _ = reply.send(choice);
        }
    }

    fn deny_pending_approval(&mut self) {
        self.answer_pending_approval(ApprovalChoice::No);
    }

    fn cancel_current(&self) {
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
    }

    fn answer_pending_approval(&mut self, choice: ApprovalChoice) {
        if let Some(reply) = self.approval_reply.take() {
            let _ = reply.send(choice);
        }
        self.app.set_approval(None);
    }

    fn drain_backend(&mut self) {
        for _ in 0..BACKEND_CAPACITY {
            let Ok(event) = self.backend_rx.try_recv() else {
                break;
            };
            self.handle_backend(event);
        }
    }

    fn finish(mut self) -> io::Result<(Runtime, Option<String>)> {
        if let Some(error) = self.worker_error {
            return Err(io::Error::other(error));
        }
        let core = self
            .core
            .take()
            .ok_or_else(|| io::Error::other("REPL worker stopped unexpectedly"))?;
        Ok((core.into_runtime(), self.exit_hint))
    }
}

fn approval_is_cancelled(shutdown_requested: bool, cancel: Option<&CancellationToken>) -> bool {
    shutdown_requested || cancel.is_some_and(CancellationToken::is_cancelled)
}

enum CoreJob {
    Input(String),
    Shutdown,
}

pub(super) enum WorkerResult {
    Done(Box<ReplCore>, CoreAction),
    Failed(String),
}

fn spawn_core(
    mut core: ReplCore,
    job: CoreJob,
    backend_tx: mpsc::SyncSender<BackendEvent>,
    backend_notify: Arc<Notify>,
    result_tx: UnboundedSender<WorkerResult>,
    cancel: CancellationToken,
) {
    let handle = Handle::current();
    let worker = spawn_blocking(move || {
        let mut ui = ChannelUi::with_cancel(backend_tx, backend_notify, cancel);
        let action = handle.block_on(async {
            match job {
                CoreJob::Input(input) => core.handle_input(&input, &mut ui).await,
                CoreJob::Shutdown => {
                    core.shutdown(&mut ui);
                    CoreAction::Quit
                }
            }
        });
        (core, action)
    });
    tokio::spawn(async move {
        let result = match worker.await {
            Ok((core, action)) => WorkerResult::Done(Box::new(core), action),
            Err(error) => WorkerResult::Failed(format!("REPL worker failed: {error}")),
        };
        let _ = result_tx.send(result);
    });
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        let guard = Self;
        execute!(
            io::stdout(),
            event::EnableBracketedPaste,
            event::EnableMouseCapture,
            SetTitle("afi idle")
        )?;
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            event::DisableMouseCapture,
            event::DisableBracketedPaste,
            SetTitle("afi idle")
        );
        ratatui::restore();
    }
}

fn set_title(title: &str) {
    let _ = execute!(io::stdout(), SetTitle(title));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_approval_is_cancelled_after_job_cancel() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(approval_is_cancelled(false, Some(&cancel)));
        assert!(approval_is_cancelled(true, None));
        assert!(!approval_is_cancelled(false, None));
    }

    #[test]
    fn repeated_updates_coalesce_into_one_frame() {
        let mut render = RenderGate::default();
        render.request();
        render.request();

        assert!(render.take());
        assert!(!render.take());
    }
}
