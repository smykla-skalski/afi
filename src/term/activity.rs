//! Responsive activity loop shown while a model request is in flight.
//!
//! `run_during_generation` races the request future against a Ratatui inline
//! viewport that animates the [`LifeSpinner`] and watches the keyboard: pressing
//! Esc sets the [`InterruptWatcher`] and returns [`Generation::Interrupted`],
//! which the turn loop treats as an Esc-to-chat. The request future is dropped
//! on interrupt, cancelling the in-flight HTTP call.
//!
//! When stdout is not a TTY (one-shot / piped runs) the future is simply
//! awaited with no spinner and no key handling.

use std::future::Future;
use std::io::{self, IsTerminal};
use std::time::Duration;
use tokio::time::interval;

use ratatui::crossterm::cursor::MoveTo;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{Clear, ClearType};
use ratatui::{DefaultTerminal, TerminalOptions, Viewport};

use crate::term::interrupt::InterruptWatcher;
use crate::term::life::LifeSpinner;

/// One animation tick of the spinner, in milliseconds.
const TICK_MS: u64 = 90;

/// The outcome of awaiting a future under the activity loop.
pub enum Generation<T> {
    /// The future finished; here is its value.
    Completed(T),
    /// The user pressed Esc before the future finished.
    Interrupted,
}

/// Does this key event mean "interrupt the current generation"? Esc only.
fn is_interrupt_key(key: KeyEvent) -> bool {
    key.kind == KeyEventKind::Press && key.code == KeyCode::Esc
}

/// Restores the terminal (raw mode off, cursor shown) on every exit path.
struct RestoreGuard;

impl Drop for RestoreGuard {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

/// Await `fut` while animating a spinner labelled `label` and watching for Esc.
///
/// On Esc the `interrupt` watcher is set and `Generation::Interrupted` is
/// returned. Falls back to a plain `await` when stdout is not a TTY or the
/// terminal cannot be initialised.
pub async fn run_during_generation<F, T>(
    interrupt: &InterruptWatcher,
    label: &str,
    fut: F,
) -> Generation<T>
where
    F: Future<Output = T>,
{
    interrupt.reset();
    if !io::stdout().is_terminal() {
        return Generation::Completed(fut.await);
    }
    let Ok(terminal) = ratatui::try_init_with_options(TerminalOptions {
        viewport: Viewport::Inline(1),
    }) else {
        return Generation::Completed(fut.await);
    };
    spinner_loop(terminal, interrupt, label, fut).await
}

async fn spinner_loop<F, T>(
    mut terminal: DefaultTerminal,
    interrupt: &InterruptWatcher,
    label: &str,
    fut: F,
) -> Generation<T>
where
    F: Future<Output = T>,
{
    let _guard = RestoreGuard;
    let mut spinner = LifeSpinner::new(label);
    let mut ticker = interval(Duration::from_millis(TICK_MS));
    tokio::pin!(fut);

    let outcome = loop {
        tokio::select! {
            value = &mut fut => break Generation::Completed(value),
            _ = ticker.tick() => {
                if drain_for_interrupt() {
                    interrupt.interrupt();
                    break Generation::Interrupted;
                }
                let _ = terminal.draw(|frame| spinner.render(frame, frame.area()));
                super::set_working_title(spinner.frame());
                spinner.tick();
            }
        }
    };

    // Collapse the spinner row so the reply prints cleanly below it.
    let vp = terminal.get_frame().area();
    let _ = execute!(
        io::stdout(),
        MoveTo(vp.x, vp.y),
        Clear(ClearType::FromCursorDown)
    );
    outcome
}

/// Non-blocking drain of pending input events; returns true if Esc was seen.
fn drain_for_interrupt() -> bool {
    while event::poll(Duration::ZERO).unwrap_or(false) {
        match event::read() {
            Ok(Event::Key(key)) if is_interrupt_key(key) => return true,
            Ok(_) => {}
            Err(_) => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyModifiers;

    #[test]
    fn esc_is_an_interrupt_other_keys_are_not() {
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert!(is_interrupt_key(esc));
        let a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(!is_interrupt_key(a));
    }

    #[test]
    fn release_esc_is_not_an_interrupt() {
        let mut esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        esc.kind = KeyEventKind::Release;
        assert!(!is_interrupt_key(esc));
    }

    #[tokio::test]
    async fn non_tty_completes_without_a_terminal() {
        // Under `cargo test` stdout is not a TTY, so the future is awaited
        // directly and its value returned.
        let watcher = InterruptWatcher::new();
        let out = run_during_generation(&watcher, "thinking", async { 7u32 }).await;
        match out {
            Generation::Completed(v) => assert_eq!(v, 7),
            Generation::Interrupted => panic!("should not interrupt a non-TTY run"),
        }
        assert!(!watcher.was_interrupted());
    }
}
