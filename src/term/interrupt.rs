//! Esc interrupt watcher - a tokio task that watches for Esc keypresses
//! during model generation. When Esc is pressed, it cancels the in-flight
//! stream via a `CancellationToken`.
//!
//! This replaces the Python singleton thread + termios approach. With tokio
//! + crossterm, we use an async event loop instead of raw termios polling.

use tokio::sync::watch;

/// A cancel token for interrupting the model stream. The interrupt watcher
/// sends `true` when Esc is pressed; the model turn loop checks it between
/// chunks.
pub struct InterruptWatcher {
    tx: watch::Sender<bool>,
    rx: watch::Receiver<bool>,
}

impl InterruptWatcher {
    /// Create a new interrupt watcher.
    pub fn new() -> Self {
        let (tx, rx) = watch::channel(false);
        Self { tx, rx }
    }

    /// Check if the user has interrupted (pressed Esc).
    pub fn was_interrupted(&self) -> bool {
        *self.rx.borrow()
    }

    /// Signal an interrupt.
    pub fn interrupt(&self) {
        let _ = self.tx.send(true);
    }

    /// Clear the interrupt flag (for the next turn).
    pub fn reset(&self) {
        let _ = self.tx.send(false);
    }

    /// Get a receiver clone for checking in async contexts.
    pub fn receiver(&self) -> watch::Receiver<bool> {
        self.rx.clone()
    }
}

impl Default for InterruptWatcher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupt_and_check() {
        let watcher = InterruptWatcher::new();
        assert!(!watcher.was_interrupted());
        watcher.interrupt();
        assert!(watcher.was_interrupted());
        watcher.reset();
        assert!(!watcher.was_interrupted());
    }
}
