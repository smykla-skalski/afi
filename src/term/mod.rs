//! Terminal presentation boundaries.
//!
//! Interactive sessions use one fullscreen Ratatui owner. Pipes and prompt
//! files use the independent line-oriented frontend.

mod channel;
mod output;
pub mod plain;
pub mod tui;

pub(crate) use channel::{BackendEvent, ChannelUi};
pub use output::{MessageKind, OutputEvent, StreamKind, UserInterface};
