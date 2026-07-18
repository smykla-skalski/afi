//! Chat-session persistence: one JSON file per session under
//! `~/.afi/sessions/` (or `AFI_SESSIONS_DIR` / `AFI_HOME`).
//!
//! Fresh, version-tagged schema (`"schema": "afi-1"`) - the Python
//! version's files will not resume. The file stores the exact `messages`
//! array the model sees plus a little metadata (id, title, description,
//! source, cwd, timestamps, optional metrics). Greppable, human-readable,
//! round-trips trivially.

pub mod store;
pub mod summary;

pub use store::{delete_session, load_session, session_files_newest, write_session};
pub use summary::{list_sessions, resolve_session, session_summary_from_file, SessionSummary};

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::Local;

/// Default page size for `afi sessions` / `/sessions`.
pub const SESSION_LIST_DEFAULT_LIMIT: usize = 10;
/// Upper bound on the page size (matches the Python `SESSION_LIST_MAX_LIMIT`).
pub const SESSION_LIST_MAX_LIMIT: usize = 100;

/// Where session files live. Honors `AFI_SESSIONS_DIR`, then
/// `AFI_HOME/sessions`, then `~/.afi/sessions`.
pub fn sessions_dir(env: &HashMap<String, String>) -> PathBuf {
    if let Some(d) = env.get("AFI_SESSIONS_DIR") {
        return PathBuf::from(d);
    }
    let home = afi_home(env);
    home.join("sessions")
}

/// Where memory files live. Always under `AFI_HOME/memories`.
pub fn memories_dir(env: &HashMap<String, String>) -> PathBuf {
    afi_home(env).join("memories")
}

/// `~/.afi` or `AFI_HOME`.
pub fn afi_home(env: &HashMap<String, String>) -> PathBuf {
    if let Some(d) = env.get("AFI_HOME") {
        return PathBuf::from(d);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".afi")
}

/// Short, unguessable, sortable-ish session id: `YYYYMMDD-HHMMSS-<6 hex>`.
pub fn new_session_id() -> String {
    let stamp = Local::now().format("%Y%m%d-%H%M%S");
    let hex: String = (0..3).map(|_| format!("{:02x}", rand_u8())).collect();
    format!("{}-{}", stamp, hex)
}

// Tiny, dependency-free RNG so we don't pull in `rand` for one 3-byte id.
// Uses a thread-local `Cell<u64>` seeded from the system time + thread id.
fn rand_u8() -> u8 {
    use std::cell::Cell;
    thread_local! {
        static STATE: Cell<u64> = Cell::new(seed());
    }
    STATE.with(|s| {
        let mut x = s.get();
        // xorshift64
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.set(x);
        (x & 0xff) as u8
    })
}

fn seed() -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0xdeadbeef);
    let tid = std::thread::current().id();
    let tid_hash = format!("{:?}", tid)
        .bytes()
        .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
    now ^ tid_hash
}

/// Turn the first user message into a filesystem-safe-ish title. Collapses
/// whitespace, strips control chars, clamps length. Returns `None` on empty
/// input. The id (not the title) is the filename, so a weird title can't
/// break lookup.
pub fn safe_title(text: Option<&str>, maxlen: usize) -> Option<String> {
    let text = text?;
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let filtered: String = collapsed.chars().filter(|c| !c.is_control()).collect();
    if filtered.is_empty() {
        return None;
    }
    if filtered.chars().count() > maxlen {
        let head: String = filtered.chars().take(maxlen - 1).collect();
        Some(format!("{}\u{2026}", head))
    } else {
        Some(filtered)
    }
}

/// The scannable tail of a session id (the 6-hex suffix), for listings.
pub fn short_id(session_id: &str) -> String {
    if let Some((_, tail)) = session_id.rsplit_once('-') {
        tail.to_string()
    } else {
        session_id.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_session_id_shape_and_uniqueness() {
        let mut ids = std::collections::HashSet::new();
        for _ in 0..50 {
            let id = new_session_id();
            assert!(id.contains('-'), "{}", id);
            assert!(id.len() >= 15, "{}", id);
            ids.insert(id);
        }
        assert_eq!(ids.len(), 50, "session ids collided");
    }

    #[test]
    fn safe_title_collapses_and_clamps() {
        assert_eq!(
            safe_title(Some("  hello\nworld  "), 60),
            Some("hello world".to_string())
        );
        let long = "x".repeat(200);
        let t = safe_title(Some(&long), 60).unwrap();
        let count = t.chars().count();
        assert_eq!(count, 60);
        assert!(t.ends_with('\u{2026}'));
        assert_eq!(safe_title(Some(""), 60), None);
        assert_eq!(safe_title(None, 60), None);
    }

    #[test]
    fn short_id_returns_tail() {
        assert_eq!(short_id("20250101-120000-abc123"), "abc123");
        assert_eq!(short_id("plain"), "plain");
    }
}
