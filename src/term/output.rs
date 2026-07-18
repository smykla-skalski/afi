//! Typed output shared by fullscreen and plain terminal frontends.

use crate::risk::ApprovalChoice;
use tokio_util::sync::CancellationToken;

/// Semantic style for a complete transcript message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    Info,
    Warning,
    Error,
    Stats,
    Tool,
}

/// Semantic style for incremental model text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    Assistant,
    Reasoning,
}

/// Output mutation understood by every frontend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputEvent {
    Header(String),
    Message { kind: MessageKind, text: String },
    Stream { kind: StreamKind, delta: String },
    StreamFinished,
    ToolStarted { name: String, action: String },
    ToolFinished { name: String, summary: String },
}

/// Model/REPL output boundary. Implementations either update Ratatui state or
/// write plain terminal output; business logic never writes to the terminal.
pub trait UserInterface: Send {
    fn emit(&mut self, event: OutputEvent);

    /// Start cancellable work and return its cancellation token.
    fn start_activity(&mut self, label: &str) -> CancellationToken;

    /// End current activity row.
    fn stop_activity(&mut self);

    /// Ask user to approve an action. Safe default is denial.
    fn approve(&mut self, prompt: &str) -> ApprovalChoice;

    fn header(&mut self, text: String) {
        self.emit(OutputEvent::Header(text));
    }

    fn message(&mut self, kind: MessageKind, text: String) {
        self.emit(OutputEvent::Message { kind, text });
    }

    fn stream(&mut self, kind: StreamKind, delta: String) {
        self.emit(OutputEvent::Stream { kind, delta });
    }

    fn finish_stream(&mut self) {
        self.emit(OutputEvent::StreamFinished);
    }
}
