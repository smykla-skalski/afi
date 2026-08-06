//! afi - a deliberately tiny coding agent for self-hosted or remote models.
//!
//! One binary, talks to any OpenAI-compatible endpoint. Survives models whose
//! native tool-calling isn't wired up yet by falling back to parsing
//! `[afi_tool_call]...[/afi_tool_call]` tags out of the text.

// The whole crate is safe Rust. Detached command execution uses the safe
// `CommandExt::process_group` instead of a pre_exec setsid, so no module needs
// `unsafe`; forbid it so a regression can't reintroduce one.
#![forbid(unsafe_code)]

pub mod approval;
pub mod cli;
pub mod config;
pub mod envfile;
pub mod log;
pub mod memory;
pub mod metrics;
pub mod model;
pub mod prompt;
pub mod repl;
pub mod risk;
pub mod sessions;
pub mod summary;
pub mod term;
pub mod tools;
pub mod util;

pub use approval::{ApprovalKind, ApprovalState, Level};
pub use config::{Runtime, Source};
pub use repl::banner;
pub use sessions::{
    SESSION_LIST_DEFAULT_LIMIT, SESSION_LIST_MAX_LIMIT, SessionSummary, delete_session,
    list_sessions, load_session, new_session_id, resolve_session, safe_title,
    session_summary_from_file, short_id, write_session,
};
