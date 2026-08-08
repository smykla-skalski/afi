//! Working out what a fold would do, separately from doing it.
//!
//! Split in two because the summary comes from the model and the request that
//! fetches it is asynchronous, while the surgery on `messages` either side of it
//! is not. [`plan_compression`] decides the split and renders the prompt;
//! [`CompressionPlan::apply`] rebuilds the conversation once the summary is back.
//! Between the two, a caller can await, show a spinner, or be interrupted -
//! and a run that is interrupted has changed nothing, because the plan owns a
//! copy of the tail and the conversation is only replaced at the end.

use serde_json::{Value, json};
use std::collections::HashSet;

use super::CompressResult;

/// A fold that has been worked out but not performed: which turns go into the
/// summary, which survive verbatim, and the prompt that asks for the summary.
pub struct CompressionPlan {
    prompt: String,
    /// Whether the conversation opens with a system message, which is kept
    /// whatever else goes.
    has_sys: bool,
    /// The turns that survive verbatim, already trimmed to something a chat
    /// template can render. Its length is the kept count - the requested `keep`
    /// is only where that started, because trimming can take a turn back off.
    tail: Vec<Value>,
    summarized_n: usize,
}

impl CompressionPlan {
    /// What to send the model. The whole point of the split: a caller can put
    /// this on the wire however it likes.
    #[must_use]
    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    /// Replace the summarized span with `summary`, keeping the system message and
    /// the tail. `None` - and `messages` untouched - when the summary is empty,
    /// which is a model that answered without answering.
    ///
    /// Takes `messages` again rather than holding a borrow across the caller's
    /// await, which is also what makes an interrupted fold a no-op.
    #[must_use]
    pub fn apply(self, messages: &mut Vec<Value>, summary: &str) -> Option<CompressResult> {
        let summary = summary.trim();
        if summary.is_empty() {
            return None;
        }
        // The tail is what survived, so it is what the header counts. Reporting
        // the requested `keep` instead would overstate whenever
        // `trim_unrenderable_tail_head` took one back off.
        let kept_n = self.tail.len();
        let header = format!(
            "[Compressed context - {} earlier turns summarized; last {kept_n} turns kept verbatim]",
            self.summarized_n
        );
        let mut new_messages = Vec::with_capacity(2 + kept_n);
        if self.has_sys {
            new_messages.push(messages[0].clone());
        }
        new_messages.push(json!({"role": "user", "content": format!("{header}\n\n{summary}")}));
        new_messages.extend(self.tail);
        *messages = new_messages;
        Some(CompressResult {
            kept_n,
            summarized_n: self.summarized_n,
            summary_chars: summary.len(),
        })
    }
}

/// Work out the fold: what to summarize, what to keep, and what to ask.
///
/// `None` when there is nothing to fold - a conversation no longer than the turns
/// it would keep. When `auto` is set the keep count is raised above `keep` to
/// roughly the last third of the conversation, so an automatic fold is more
/// conservative than a manual `/compress`: in-progress work and recent tool
/// results survive it.
#[must_use]
pub fn plan_compression(
    messages: &[Value],
    mut keep: usize,
    auto: bool,
) -> Option<CompressionPlan> {
    // Layout: [system?, ..., user, assistant, tool, ..., user, assistant(tool_calls)?, ...]
    let has_sys = messages
        .first()
        .is_some_and(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"));

    if auto {
        keep = keep.max((messages.len() - usize::from(has_sys)) / 3);
    }
    if messages.len() <= 1 + keep {
        return None;
    }
    let body_start = usize::from(has_sys);
    if messages.len() - body_start <= keep {
        return None;
    }

    let split = messages.len() - keep;
    let head: &[Value] = &messages[body_start..split];
    let mut tail: Vec<Value> = messages[split..].to_vec();
    let mut summarized_n = head.len();

    // The tail must start on a turn the chat template can render. A `tool` turn
    // with no preceding assistant(tool_calls) parent - or an assistant(tool_calls)
    // turn whose result got cut off into `head` - makes the template raise. Walk
    // from the front of the tail and drop any leading tool/half-tool-call turns
    // until we land on something safe.
    summarized_n += trim_unrenderable_tail_head(&mut tail);

    let rendered = render_messages(head);
    let prompt = format!(
        "Summarize the following conversation history for context retention. \
        Preserve: the original user goal/task, key decisions made, file paths \
        and identifiers touched, current state of any in-progress work, and \
        any unresolved questions. Drop: raw tool outputs, full file contents, \
        and verbose back-and-forth - keep it dense and information-rich. \
        Write in the same language as the conversation. Output ONLY the \
        summary, no preamble.\n\n---\n{rendered}\n---"
    );
    Some(CompressionPlan {
        prompt,
        has_sys,
        tail,
        summarized_n,
    })
}

/// Drop leading tail turns the chat template can't render on their own: orphan
/// `tool` turns, or an assistant `tool_calls` turn whose matching results were
/// cut into the head. Returns how many extra turns were folded into the summary.
fn trim_unrenderable_tail_head(tail: &mut Vec<Value>) -> usize {
    let mut dropped = 0;
    while let Some(first) = tail.first() {
        let role = first.get("role").and_then(|r| r.as_str()).unwrap_or("");
        let drop_leading = role == "tool"
            || (role == "assistant"
                && first.get("tool_calls").is_some()
                && tail_head_has_orphan_calls(first, tail));
        if !drop_leading {
            break;
        }
        tail.remove(0);
        dropped += 1;
    }
    dropped
}

/// True when `first`'s `tool_calls` have any id without a matching `tool` result
/// later in `tail` (so rendering the tail as-is would break the chat template).
fn tail_head_has_orphan_calls(first: &Value, tail: &[Value]) -> bool {
    let Some(tcs) = first.get("tool_calls").and_then(|t| t.as_array()) else {
        return false;
    };
    let ids: HashSet<&str> = tcs
        .iter()
        .filter_map(|tc| tc.get("id").and_then(|i| i.as_str()))
        .collect();
    let seen: HashSet<&str> = tail
        .iter()
        .filter_map(|m| {
            if m.get("role").and_then(|r| r.as_str()) == Some("tool") {
                m.get("tool_call_id").and_then(|i| i.as_str())
            } else {
                None
            }
        })
        .collect();
    !ids.is_empty() && !ids.is_subset(&seen)
}

/// Render messages as plain text for the summary prompt. Tool outputs are
/// truncated to 2000 chars each so a single huge `read_file` doesn't blow up the
/// summary prompt itself.
fn render_messages(msgs: &[Value]) -> String {
    msgs.iter()
        .filter_map(render_message_line)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Truncate a rendered turn to 2000 bytes on a char boundary.
fn truncate_2000(s: &str) -> &str {
    if s.len() <= 2000 {
        return s;
    }
    let mut end = 2000;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Render one message as a `[role] ...` line, or `None` when there is nothing
/// renderable (e.g. an assistant turn whose `tool_calls` is not an array).
fn render_message_line(m: &Value) -> Option<String> {
    let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("?");
    if m.get("content").is_none() && m.get("tool_calls").is_some() {
        let tcs = m.get("tool_calls").and_then(|t| t.as_array())?;
        let calls: Vec<String> = tcs.iter().filter_map(render_tool_call).collect();
        return Some(format!("[{role}] -> {}", calls.join(", ")));
    }
    if let Some(content) = m.get("content").and_then(|c| c.as_str()) {
        return Some(format!("[{role}] {}", truncate_2000(content)));
    }
    if let Some(arr) = m.get("content").and_then(|c| c.as_array()) {
        let joined: String = arr
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect();
        return Some(format!("[{role}] {}", truncate_2000(&joined)));
    }
    None
}

/// Render one `tool_calls` entry as `name(args)`.
fn render_tool_call(tc: &Value) -> Option<String> {
    let f = tc.get("function")?;
    let name = f.get("name").and_then(|n| n.as_str()).unwrap_or("");
    let args = f.get("arguments").and_then(|a| a.as_str()).unwrap_or("");
    Some(format!("{name}({args})"))
}
