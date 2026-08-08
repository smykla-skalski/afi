//! Resuming a saved session: which one, and what of it this run keeps.
//!
//! Split from `core` because it answers a question of its own - a resumed run is
//! still the run the operator just configured, so some of what was saved is replayed
//! verbatim and some is re-decided from this invocation. The system prompt is
//! re-decided; the conversation is not.

use std::path::Path;

use serde_json::Value;

use crate::cli::session_id_from_args;
use crate::config::{Runtime, nested};
use crate::sessions;
use crate::term::{MessageKind, UserInterface};

pub(super) fn resume_session(
    rt: &mut Runtime,
    dir: &Path,
    ui: &mut dyn UserInterface,
) -> Option<(Vec<Value>, String)> {
    let target = rt.resume.clone()?;
    let sid = if let Some(target) = target {
        session_id_from_args(&["--resume".to_string(), target], &rt.env)?
    } else {
        let Some(summary) = sessions::list_sessions(dir, Some(1), 0, None)
            .first()
            .cloned()
        else {
            ui.message(
                MessageKind::Info,
                "no saved sessions to resume - starting fresh".to_string(),
            );
            return None;
        };
        summary.id
    };
    let data = sessions::load_session(dir, &sid)?;
    let stored = data.get("messages").and_then(Value::as_array)?;
    let mut messages: Vec<Value> = stored
        .iter()
        .filter(|message| message.get("role").and_then(Value::as_str) != Some("system"))
        .cloned()
        .collect();
    // This run's prompt, not the one the session was saved under: a resumed run
    // is still the run the operator just configured, and the stored system
    // message was filtered out above for exactly that reason.
    messages.insert(0, rt.prompt().message());
    // The subtree half rides in tool messages, replayed verbatim, so it cannot be
    // re-decided the same way. Taken from what the session recorded rather than from
    // the messages, which are not afi's to trust - see `nested::adopt`.
    nested::adopt(data.get("instructions").unwrap_or(&Value::Null));
    if let Some(source) = data.get("source").and_then(Value::as_str) {
        rt.restore_source(Some(source), data.get("model").and_then(Value::as_str));
    }
    ui.message(
        MessageKind::Info,
        format!("↻ resumed session {sid} ({} messages)", messages.len() - 1),
    );
    Some((messages, sid))
}

pub(crate) fn restore_prompt_resume(rt: &mut Runtime) {
    let Some(target) = rt.resume.clone() else {
        return;
    };
    let dir = sessions::sessions_dir(&rt.env);
    let sid = if let Some(target) = target {
        session_id_from_args(&["--resume".to_string(), target], &rt.env)
    } else {
        sessions::list_sessions(&dir, Some(1), 0, None)
            .first()
            .map(|summary| summary.id.clone())
    };
    let Some(data) = sid.and_then(|sid| sessions::load_session(&dir, &sid)) else {
        return;
    };
    if let Some(source) = data.get("source").and_then(Value::as_str) {
        rt.restore_source(Some(source), data.get("model").and_then(Value::as_str));
    }
}
