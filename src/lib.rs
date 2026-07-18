//! minion - a deliberately tiny coding agent for self-hosted or remote models.
//!
//! One binary, talks to any OpenAI-compatible endpoint. Survives models whose
//! native tool-calling isn't wired up yet by falling back to parsing
//! `[minion_tool_call]...[/minion_tool_call]` tags out of the text.

pub mod approval;
pub mod cli;
pub mod config;
pub mod envfile;
pub mod log;
pub mod prompt;
pub mod repl;
pub mod sessions;
pub mod util;

pub use approval::{ApprovalKind, ApprovalState, Level};
pub use config::{Runtime, Source};
pub use repl::banner;
pub use sessions::{
    delete_session, list_sessions, load_session, new_session_id, resolve_session, safe_title,
    session_summary_from_file, short_id, write_session, SessionSummary, SESSION_LIST_DEFAULT_LIMIT,
    SESSION_LIST_MAX_LIMIT,
};
