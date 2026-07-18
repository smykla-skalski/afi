//! Append-only JSONL traffic log.
//!
//! Replaces the Python `llamacpp.log` that lived next to the script. Lives at
//! `~/.afi/logs/traffic.jsonl` so an installed binary (which has no "next
//! to the script") still has a stable, discoverable log location. Same
//! `{"ts","dir","data"}` event schema as the Python `_log_event` so existing
//! log tooling keeps working after a path update.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use serde_json::Value;

use crate::util::now_secs_f64;
use std::fs;

/// Where the traffic log lives: `~/.afi/logs/traffic.jsonl`.
#[must_use]
pub fn log_path() -> PathBuf {
    let mut p = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    p.push(".afi");
    p.push("logs");
    p.push("traffic.jsonl");
    p
}

/// Append one event line. Best-effort: a missing directory is created, a
/// missing file is created, any I/O error is dropped (the log must never
/// break the REPL).
pub fn log_event(direction: &str, payload: &Value) {
    let path = log_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let ts = now_secs_f64();
    let line = serde_json::json!({"ts": ts, "dir": direction, "data": payload});
    if let Ok(mut f) = OpenOptions::new().append(true).create(true).open(&path) {
        let _ = writeln!(f, "{line}");
    }
}
