//! Context compression: fold older turns into a summary, keep the last N
//! verbatim. Manual `/compress` keeps 2; auto-compress keeps ~⅓.

use serde_json::{json, Value};

/// How many recent turns to leave untouched in a manual `/compress`.
pub const COMPRESS_KEEP: usize = 2;

/// The result of a successful compression: `(kept_n, summarized_n, summary_chars)`.
pub struct CompressResult {
    pub kept_n: usize,
    pub summarized_n: usize,
    pub summary_chars: usize,
}

/// Ask the model to summarize everything except system + last `keep` turns.
///
/// Mutates `messages` in place on success: replaces the middle slice with a
/// single user-role summary turn. Returns `Some(CompressResult)` on success,
/// or `None` on failure (in which case `messages` is untouched).
///
/// When `auto=True` the keep count is raised above `COMPRESS_KEEP` so
/// auto-compression is more conservative than a manual `/compress` - it keeps
/// roughly the last third of the conversation verbatim so in-progress work
/// and recent tool results survive the fold.
///
/// `summarize` is a closure that takes the summary prompt and returns the
/// model's summary text (or `None` on failure). This abstracts the HTTP call
/// so the function is testable without a live server.
pub fn compress<F>(
    messages: &mut Vec<Value>,
    mut keep: usize,
    auto: bool,
    summarize: F,
) -> Option<CompressResult>
where
    F: FnOnce(&str) -> Option<String>,
{
    // Layout: [system?, ..., user, assistant, tool, ..., user, assistant(tool_calls)?, ...]
    let has_sys = messages
        .first()
        .map(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
        .unwrap_or(false);

    if auto {
        let body_len = messages.len() - if has_sys { 1 } else { 0 };
        keep = keep.max(body_len / 3);
    }

    if messages.len() <= 1 + keep {
        return None;
    }

    let body_start = if has_sys { 1 } else { 0 };
    let body_len = messages.len() - body_start;
    if body_len <= keep {
        return None;
    }

    let split = messages.len() - keep;
    let head: &[Value] = &messages[body_start..split];
    let mut tail: Vec<Value> = messages[split..].to_vec();
    let mut summarized_n = head.len();

    // The tail must start on a turn the chat template can render. A `tool`
    // turn with no preceding assistant(tool_calls) parent — or an
    // assistant(tool_calls) turn whose result got cut off into `head` —
    // makes the template raise. Walk from the front of the tail and drop any
    // leading tool/half-tool-call turns until we land on something safe.
    while let Some(first) = tail.first() {
        let role = first.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if role == "tool" {
            tail.remove(0);
            summarized_n += 1;
            continue;
        }
        if role == "assistant" && first.get("tool_calls").is_some() {
            // Only safe if every tool_call has its matching tool result later in the tail.
            if let Some(tcs) = first.get("tool_calls").and_then(|t| t.as_array()) {
                let ids: std::collections::HashSet<&str> = tcs
                    .iter()
                    .filter_map(|tc| tc.get("id").and_then(|i| i.as_str()))
                    .collect();
                let seen: std::collections::HashSet<&str> = tail
                    .iter()
                    .filter_map(|m| {
                        if m.get("role").and_then(|r| r.as_str()) == Some("tool") {
                            m.get("tool_call_id").and_then(|i| i.as_str())
                        } else {
                            None
                        }
                    })
                    .collect();
                if !ids.is_empty() && !ids.is_subset(&seen) {
                    tail.remove(0);
                    summarized_n += 1;
                    continue;
                }
            }
        }
        break;
    }

    // Render the head as plain text for the model to summarize.
    let rendered = render_messages(head);
    let summary_prompt = format!(
        "Summarize the following conversation history for context retention. \
        Preserve: the original user goal/task, key decisions made, file paths \
        and identifiers touched, current state of any in-progress work, and \
        any unresolved questions. Drop: raw tool outputs, full file contents, \
        and verbose back-and-forth - keep it dense and information-rich. \
        Write in the same language as the conversation. Output ONLY the \
        summary, no preamble.\n\n---\n{}\n---",
        rendered
    );

    let summary = summarize(&summary_prompt)?;
    let summary = summary.trim();
    if summary.is_empty() {
        return None;
    }

    let header = format!(
        "[Compressed context - {} earlier turns summarized; last {} turns kept verbatim]",
        summarized_n, keep
    );
    let new_mid = json!({"role": "user", "content": format!("{}\n\n{}", header, summary)});

    // Rebuild messages: [sys?] + [summary] + tail
    let mut new_messages = Vec::with_capacity(1 + 1 + tail.len());
    if has_sys {
        new_messages.push(messages[0].clone());
    }
    new_messages.push(new_mid);
    new_messages.extend(tail);

    let kept_n = new_messages.len() - if has_sys { 2 } else { 1 };
    let summary_chars = summary.len();
    *messages = new_messages;

    Some(CompressResult {
        kept_n,
        summarized_n,
        summary_chars,
    })
}

/// Render messages as plain text for the summary prompt. Tool outputs are
/// truncated to 2000 chars each so a single huge read_file doesn't blow up
/// the summary prompt itself.
fn render_messages(msgs: &[Value]) -> String {
    let mut out: Vec<String> = Vec::new();
    for m in msgs {
        let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("?");
        if m.get("content").is_none() && m.get("tool_calls").is_some() {
            if let Some(tcs) = m.get("tool_calls").and_then(|t| t.as_array()) {
                let calls: Vec<String> = tcs
                    .iter()
                    .filter_map(|tc| {
                        tc.get("function").map(|f| {
                            let name = f.get("name").and_then(|n| n.as_str()).unwrap_or("");
                            let args = f.get("arguments").and_then(|a| a.as_str()).unwrap_or("");
                            format!("{}({})", name, args)
                        })
                    })
                    .collect();
                out.push(format!("[{}] -> {}", role, calls.join(", ")));
            }
        } else if let Some(content) = m.get("content").and_then(|c| c.as_str()) {
            let truncated = if content.len() > 2000 {
                &content[..2000]
            } else {
                content
            };
            out.push(format!("[{}] {}", role, truncated));
        } else if let Some(arr) = m.get("content").and_then(|c| c.as_array()) {
            let joined: String = arr
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()).map(String::from))
                .collect::<Vec<_>>()
                .join("");
            let truncated = if joined.len() > 2000 {
                &joined[..2000]
            } else {
                &joined
            };
            out.push(format!("[{}] {}", role, truncated));
        }
    }
    out.join("\n\n")
}

/// Silently compress when context usage crosses the `autocompress_percent`
/// threshold. Returns `true` if a compression happened.
///
/// `compress_fn` is the closure that does the actual compression (abstracted
/// so this is testable without a live server).
pub fn maybe_autocompress<F>(
    messages: &mut Vec<Value>,
    prompt_tokens: u64,
    autocompress_percent: u32,
    context_window: Option<u64>,
    compress_fn: F,
) -> bool
where
    F: FnOnce(&mut Vec<Value>) -> Option<CompressResult>,
{
    if autocompress_percent == 0 {
        return false;
    }
    if prompt_tokens == 0 {
        return false;
    }
    let mx = match context_window {
        Some(m) if m > 0 => m,
        _ => return false,
    };
    let ratio = prompt_tokens as f64 / mx as f64;
    if ratio * 100.0 < autocompress_percent as f64 {
        return false;
    }
    compress_fn(messages).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> Value {
        json!({"role": role, "content": content})
    }

    fn tc_msg(id: &str, name: &str, args: &str) -> Value {
        json!({"role": "assistant", "tool_calls": [{"id": id, "function": {"name": name, "arguments": args}}]})
    }

    fn tool_result(id: &str, content: &str) -> Value {
        json!({"role": "tool", "tool_call_id": id, "content": content})
    }

    #[test]
    fn compress_too_short_returns_none() {
        let mut messages = vec![
            json!({"role": "system", "content": "sys"}),
            msg("user", "hi"),
            msg("assistant", "hello"),
        ];
        let result = compress(&mut messages, COMPRESS_KEEP, false, |_| {
            Some("summary".to_string())
        });
        assert!(result.is_none());
    }

    #[test]
    fn compress_summarizes_head_and_keeps_tail() {
        let mut messages = vec![
            json!({"role": "system", "content": "sys"}),
            msg("user", "do task"),
            msg("assistant", "ok"),
            msg("user", "step 2"),
            msg("assistant", "done"),
            msg("user", "step 3"),
            msg("assistant", "done"),
        ];
        let result = compress(&mut messages, COMPRESS_KEEP, false, |prompt| {
            assert!(prompt.contains("do task"));
            Some("summary of earlier".to_string())
        });
        let result = result.unwrap();
        assert_eq!(result.summarized_n, 4); // 4 head messages summarized
                                            // Should have system + summary + last 2 turns
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[1]["role"], "user");
        assert!(messages[1]["content"]
            .as_str()
            .unwrap()
            .contains("summary of earlier"));
    }

    #[test]
    fn compress_auto_keeps_third() {
        let mut messages = vec![json!({"role": "system", "content": "sys"})];
        for i in 0..20 {
            messages.push(msg("user", &format!("turn {}", i)));
            messages.push(msg("assistant", &format!("reply {}", i)));
        }
        // 1 system + 40 body = 41 total. body_len=40, keep = max(2, 40/3) = 13
        let result = compress(&mut messages, COMPRESS_KEEP, true, |_| {
            Some("summary".to_string())
        });
        assert!(result.is_some());
        // Should keep ~13 of 40 body messages + system + summary = ~15
        assert!(messages.len() <= 16);
        assert!(messages.len() >= 14);
    }

    #[test]
    fn compress_drops_leading_tool_from_tail() {
        let mut messages = vec![
            json!({"role": "system", "content": "sys"}),
            msg("user", "do task"),
            tc_msg("call_1", "read_file", r#"{"path":"x"}"#),
            tool_result("call_1", "file content"),
            // tail starts here (keep=2) but the first is a tool result with
            // no preceding assistant(tool_calls) in the tail
            msg("user", "next"),
            msg("assistant", "done"),
        ];
        let result = compress(&mut messages, COMPRESS_KEEP, false, |_| {
            Some("summary".to_string())
        });
        // compress returns Some but the tool result must be dropped from the tail
        assert!(result.is_some());
        // Check no tool message is in the result
        for m in &messages {
            if m.get("role").and_then(|r| r.as_str()) == Some("tool") {
                panic!("tool message should have been dropped from tail");
            }
        }
    }

    #[test]
    fn compress_returns_none_on_empty_summary() {
        let mut messages = vec![
            json!({"role": "system", "content": "sys"}),
            msg("user", "do task"),
            msg("assistant", "ok"),
            msg("user", "step 2"),
            msg("assistant", "done"),
            msg("user", "step 3"),
            msg("assistant", "done"),
        ];
        let result = compress(&mut messages, COMPRESS_KEEP, false, |_| {
            Some("".to_string())
        });
        assert!(result.is_none());
    }

    #[test]
    fn maybe_autocompress_disabled() {
        let mut messages = vec![msg("user", "hi")];
        let result = maybe_autocompress(&mut messages, 100000, 0, Some(200000), |_| {
            Some(CompressResult {
                kept_n: 2,
                summarized_n: 3,
                summary_chars: 42,
            })
        });
        assert!(!result);
    }

    #[test]
    fn maybe_autocompress_below_threshold() {
        let mut messages = vec![msg("user", "hi")];
        let result = maybe_autocompress(&mut messages, 1000, 85, Some(200000), |_| {
            Some(CompressResult {
                kept_n: 2,
                summarized_n: 3,
                summary_chars: 42,
            })
        });
        assert!(!result); // 1000/200000 = 0.5% << 85%
    }

    #[test]
    fn maybe_autocompress_above_threshold() {
        let mut messages = vec![msg("user", "hi")];
        let result = maybe_autocompress(&mut messages, 180000, 85, Some(200000), |_| {
            Some(CompressResult {
                kept_n: 2,
                summarized_n: 3,
                summary_chars: 42,
            })
        });
        assert!(result); // 180000/200000 = 90% > 85%
    }

    #[test]
    fn maybe_autocompress_no_context_window() {
        let mut messages = vec![msg("user", "hi")];
        let result = maybe_autocompress(&mut messages, 180000, 85, None, |_| {
            Some(CompressResult {
                kept_n: 2,
                summarized_n: 3,
                summary_chars: 42,
            })
        });
        assert!(!result);
    }

    #[test]
    fn maybe_autocompress_zero_prompt_tokens() {
        let mut messages = vec![msg("user", "hi")];
        let result = maybe_autocompress(&mut messages, 0, 85, Some(200000), |_| {
            Some(CompressResult {
                kept_n: 2,
                summarized_n: 3,
                summary_chars: 42,
            })
        });
        assert!(!result);
    }
}
