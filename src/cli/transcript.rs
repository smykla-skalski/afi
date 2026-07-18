//! Session transcript rendering: a one-line-per-message recap used on resume
//! and by the `/sessions <id>` detail view.

use std::io::Write;

use serde_json::Value;

use crate::repl::{CYAN, DIM, GREEN, RESET};

/// Render a session's message history as a one-line-per-message recap. Used
/// on resume and by `/sessions <id>` for its detail view.
pub fn print_transcript<W: Write>(out: &mut W, messages: &[Value], max_chars: usize) -> usize {
    let mut printed = 0;
    for m in messages {
        let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("?");
        if role == "system" {
            continue;
        }
        let recap = message_recap(m).trim().replace('\n', " ");
        let content = truncate_chars(&recap, max_chars);
        let _ = writeln!(out, "  {}{role:>9}{RESET}  {content}", role_color(role));
        printed += 1;
    }
    printed
}

/// A one-line recap of a message's content (tool-call names, joined text
/// parts, or the plain string content).
fn message_recap(m: &Value) -> String {
    if m.get("content").is_none() && m.get("tool_calls").is_some() {
        if let Some(arr) = m.get("tool_calls").and_then(|t| t.as_array()) {
            let names: Vec<String> = arr
                .iter()
                .filter_map(|tc| {
                    tc.get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .map(|n| format!("{n}(...)"))
                })
                .collect();
            return format!("\u{2192} {}", names.join(", "));
        }
    }
    if let Some(Value::Array(parts)) = m.get("content") {
        return parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(" ");
    }
    m.get("content")
        .and_then(|c| c.as_str())
        .map(String::from)
        .unwrap_or_default()
}

fn role_color(role: &str) -> &'static str {
    match role {
        "user" => CYAN,
        "assistant" => GREEN,
        "tool" => DIM,
        _ => "",
    }
}

/// Truncate to `max` chars on a char boundary, appending an ellipsis.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let head: String = s.chars().take(max - 1).collect();
        format!("{head}\u{2026}")
    } else {
        s.to_string()
    }
}
