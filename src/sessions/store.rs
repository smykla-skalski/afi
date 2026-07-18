//! Atomic write / load / listing primitives for session files.

use std::fs;
use std::io::Write;
use std::path::Path;

use serde_json::{Map, Value};

/// Returns true if `msg` is an assistant turn with neither visible content
/// nor tool calls - such turns are pruned before save/load/stream so they
/// don't break chat templates on the next request.
pub fn is_empty_assistant_message(msg: &Value) -> bool {
    let obj = match msg.as_object() {
        Some(o) => o,
        None => return false,
    };
    if obj.get("role").and_then(|v| v.as_str()) != Some("assistant") {
        return false;
    }
    if obj.contains_key("tool_calls") {
        // has tool_calls (even if empty array) → not empty
        return false;
    }
    match obj.get("content") {
        None => true,
        Some(Value::Null) => true,
        Some(Value::String(s)) => s.trim().is_empty(),
        Some(Value::Array(arr)) => {
            // empty if no part has non-whitespace text
            arr.iter().all(|p| match p {
                Value::String(s) => s.trim().is_empty(),
                Value::Object(map) => match map.get("text") {
                    Some(Value::String(s)) => s.trim().is_empty(),
                    _ => true,
                },
                _ => true,
            })
        }
        _ => false,
    }
}

/// Drop assistant turns that have neither visible content nor tool calls.
/// Returns the number removed; mutates `messages` in place.
pub fn prune_empty_assistant_messages(messages: &mut Vec<Value>) -> usize {
    let before = messages.len();
    messages.retain(|m| !is_empty_assistant_message(m));
    before - messages.len()
}

/// Session file path inside `dir`.
pub fn session_path(dir: &Path, session_id: &str) -> std::path::PathBuf {
    dir.join(format!("{}.json", session_id))
}

/// Persist `messages` to `<dir>/<session_id>.json`. Atomic (temp + rename).
///
/// `meta` is an optional JSON object merged into the stored metadata; `None`
/// values in `meta` are skipped (so a partial write doesn't clobber fields a
/// prior save wrote). Existing `created_at` is preserved. `updated_at` is set
/// to now (or the value in `meta`).
pub fn write_session(
    dir: &Path,
    session_id: &str,
    messages: &mut Vec<Value>,
    meta: Option<&Value>,
) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    prune_empty_assistant_messages(messages);

    let path = session_path(dir, session_id);
    let now = chrono::Local::now().timestamp_millis() as f64 / 1000.0;

    // Read existing file to preserve `created_at` and any other meta not
    // touched by this write.
    let existing: Map<String, Value> = match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default(),
        Err(_) => Map::new(),
    };

    let mut data = existing;
    data.insert(
        "schema".to_string(),
        Value::String("minion-rs-1".to_string()),
    );
    data.insert("id".to_string(), Value::String(session_id.to_string()));
    data.insert("messages".to_string(), Value::Array(messages.clone()));
    let created_at = data.get("created_at").cloned().unwrap_or(Value::Number(
        serde_json::Number::from_f64(now).unwrap_or_else(|| serde_json::Number::from(0)),
    ));
    data.insert("created_at".to_string(), created_at);
    data.insert(
        "updated_at".to_string(),
        Value::Number(
            serde_json::Number::from_f64(now).unwrap_or_else(|| serde_json::Number::from(0)),
        ),
    );

    if let Some(Value::Object(meta_obj)) = meta {
        for (k, v) in meta_obj {
            if v.is_null() {
                continue;
            }
            data.insert(k.clone(), v.clone());
        }
    }

    // Deterministic key ordering isn't required but helps diffs. We keep the
    // insertion order (existing keys preserved, new keys appended).
    let updated_at_val = data.get("updated_at").cloned();
    let tmp = path.with_extension("json.tmp");
    let serialized = serde_json::to_string(&Value::Object(data))?;
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(serialized.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, &path)?;

    // Align mtime with updated_at so newest-first listing is cheap. Use std
    // FileTimes (stable since 1.75) to avoid pulling in a `filetime` crate.
    if let Some(updated) = updated_at_val.as_ref().and_then(|v| v.as_f64()) {
        if let Ok(f) = fs::OpenOptions::new().write(true).open(&path) {
            let secs = updated as i64;
            let nanos = ((updated - secs as f64) * 1e9) as u32;
            let times = fs::FileTimes::new()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::new(secs as u64, nanos));
            let _ = f.set_times(times);
        }
    }
    Ok(())
}

/// Read a session file. Returns the parsed object (with `messages` pruned)
/// or `None` on missing / parse error.
pub fn load_session(dir: &Path, session_id: &str) -> Option<Value> {
    let path = session_path(dir, session_id);
    let bytes = fs::read(&path).ok()?;
    let mut data: Value = serde_json::from_slice(&bytes).ok()?;
    if let Some(messages) = data.get_mut("messages").and_then(|m| m.as_array_mut()) {
        prune_empty_assistant_messages(messages);
    }
    Some(data)
}

/// List `*.json` filenames newest-first using filesystem mtimes. Cheap index
/// for recent-session browsing - no parsing required.
pub fn session_files_newest(dir: &Path) -> Vec<String> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return vec![],
    };
    let mut files: Vec<(f64, String)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".json") {
            continue;
        }
        let is_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false);
        if !is_file {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        files.push((mtime, name));
    }
    // Sort newest-first; ties broken by name for determinism.
    files.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.1.cmp(&a.1))
    });
    files.into_iter().map(|(_, n)| n).collect()
}

/// Remove a session file. Returns true if something was deleted.
pub fn delete_session(dir: &Path, session_id: &str) -> bool {
    fs::remove_file(session_path(dir, session_id)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn msg(role: &str, content: &str) -> Value {
        json!({"role": role, "content": content})
    }

    #[test]
    fn write_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let sid = "20250101-120000-abc123";
        let mut messages = vec![
            json!({"role": "system", "content": "SYS"}),
            msg("user", "hello there"),
            msg("assistant", "hi!"),
        ];
        let meta = json!({"title": "greeting"});
        write_session(dir, sid, &mut messages, Some(&meta)).unwrap();
        let loaded = load_session(dir, sid).expect("load returned None after write");
        assert_eq!(loaded["messages"], Value::Array(messages));
        assert_eq!(loaded["title"], "greeting");
        assert_eq!(loaded["id"], sid);
        assert_eq!(loaded["schema"], "minion-rs-1");
    }

    #[test]
    fn write_prunes_empty_assistant_messages() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let sid = "20250101-120000-empty0";
        let mut messages = vec![
            msg("user", "hello"),
            msg("assistant", ""),
            msg("assistant", "   "),
            msg("assistant", "real reply"),
        ];
        write_session(dir, sid, &mut messages, None).unwrap();
        let loaded = load_session(dir, sid).unwrap();
        assert_eq!(
            loaded["messages"],
            Value::Array(vec![msg("user", "hello"), msg("assistant", "real reply"),])
        );
        // The caller's `messages` is also pruned in place (matches Python).
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn write_is_atomic_and_merges_meta() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let sid = "20250101-120000-def456";
        let mut m1 = vec![msg("user", "first")];
        write_session(dir, sid, &mut m1, Some(&json!({"source": "local"}))).unwrap();
        // second write should preserve created_at + source, update updated_at + messages
        let mut m2 = vec![msg("user", "first"), msg("assistant", "reply")];
        write_session(dir, sid, &mut m2, None).unwrap();
        let loaded = load_session(dir, sid).unwrap();
        assert_eq!(loaded["source"], "local");
        assert_eq!(loaded["messages"].as_array().unwrap().len(), 2);
        let created = loaded["created_at"].as_f64().unwrap();
        let updated = loaded["updated_at"].as_f64().unwrap();
        assert!(created <= updated);
    }

    #[test]
    fn load_missing_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(load_session(tmp.path(), "does-not-exist-999").is_none());
    }

    #[test]
    fn delete_session_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let sid = "20250101-120000-del123";
        let mut messages = vec![msg("user", "bye")];
        write_session(dir, sid, &mut messages, None).unwrap();
        assert!(load_session(dir, sid).is_some());
        assert!(delete_session(dir, sid));
        assert!(load_session(dir, sid).is_none());
        assert!(!delete_session(dir, sid));
    }

    #[test]
    fn session_files_newest_orders_by_mtime() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // Three files, written with ascending mtimes via std FileTimes.
        for (i, name) in ["old.json", "mid.json", "new.json"].iter().enumerate() {
            let p = dir.join(name);
            fs::write(&p, "{}").unwrap();
            let f = fs::OpenOptions::new().write(true).open(&p).unwrap();
            let times = fs::FileTimes::new()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::new(100 + i as u64, 0));
            f.set_times(times).unwrap();
        }
        let files = session_files_newest(dir);
        assert_eq!(files, vec!["new.json", "mid.json", "old.json"]);
    }
}
