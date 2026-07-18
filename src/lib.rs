//! minion - a deliberately tiny coding agent for self-hosted or remote models.
//!
//! One binary, talks to any OpenAI-compatible endpoint. Survives models whose
//! native tool-calling isn't wired up yet by falling back to parsing
//! `[minion_tool_call]...[/minion_tool_call]` tags out of the text.

pub mod approval;
pub mod config;
pub mod envfile;
pub mod log;
pub mod prompt;
pub mod repl;
pub mod util;

pub use approval::{ApprovalKind, ApprovalState, Level};
pub use config::{Runtime, Source};
pub use repl::banner;
