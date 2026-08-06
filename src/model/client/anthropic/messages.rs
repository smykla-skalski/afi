//! `OpenAI`-shape conversation history -> Anthropic Messages API shape.
//!
//! afi keeps one canonical history format (`Vec<Value>` in `OpenAI` chat shape)
//! and translates at the client boundary, so sessions, compression, and
//! transcripts are unaffected by which protocol serves a turn.
//!
//! Five structural differences have to be reconciled:
//!
//! 1. `system` is a top-level request field, not a message.
//! 2. Tool results are `tool_result` content blocks inside a **user** message,
//!    and *all* results for one assistant turn must share a single message.
//! 3. Tool calls are `tool_use` blocks whose `input` is a parsed object, where
//!    `OpenAI` carries `arguments` as a JSON string.
//! 4. Every `tool_use` needs a matching `tool_result` and vice versa; an orphan
//!    on either side is a 400.
//! 5. Thinking blocks have no `OpenAI` equivalent at all. They ride alongside
//!    the turn that produced them and are replayed verbatim, first in the
//!    assistant's content array - see [`super::thinking`].

use std::collections::{HashSet, VecDeque};
use std::mem;

use serde_json::{Value, json};

use super::thinking::{self, Thinking};

/// Placeholder used when history would otherwise open with a non-user turn or
/// be empty - Anthropic requires at least one message, starting with `user`.
const CONTINUE_TEXT: &str = "(continue)";

/// Stand-in for a tool result whose content was empty. Anthropic rejects empty
/// text blocks, and "nothing" is still information the model needs.
const EMPTY_TOOL_OUTPUT: &str = "(no output)";

/// A translated request payload.
pub(super) struct Translated {
    pub(super) system: Option<String>,
    pub(super) messages: Vec<Value>,
}

/// One normalized history entry, before tool results are grouped.
enum Item {
    User(Vec<Value>),
    Assistant(Vec<Value>),
    ToolResult { id: String, text: String },
}

/// Issues stable ids for tool calls the model left unidentified.
///
/// `turn_dispatch` writes `tool_call_id` as `unwrap_or_default()`, so a model
/// that omits ids yields `""` on both the call and its result. Anthropic needs a
/// non-empty id that matches on both sides, so ids are issued in document order
/// on the call side and consumed in the same order on the result side.
#[derive(Default)]
struct SynthIds {
    next: usize,
    pending: VecDeque<String>,
}

impl SynthIds {
    fn issue(&mut self) -> String {
        self.next = self.next.saturating_add(1);
        let id = format!("afi_toolu_{}", self.next);
        self.pending.push_back(id.clone());
        id
    }

    fn take(&mut self) -> String {
        self.pending.pop_front().unwrap_or_else(|| {
            self.next = self.next.saturating_add(1);
            format!("afi_toolu_orphan_{}", self.next)
        })
    }

    /// Drop ids issued by an earlier assistant turn that no result claimed.
    ///
    /// Tool results always immediately follow the turn that produced them, so
    /// anything still pending when a new assistant turn starts belongs to a turn
    /// whose results were cut away. Leaving them queued would shift this turn's
    /// results onto the previous turn's calls.
    fn start_turn(&mut self) {
        self.pending.clear();
    }
}

/// Translate an `OpenAI`-shape history into a top-level `system` string plus
/// Anthropic-shape messages.
///
/// `thinking` says whether the request asked the model to reason; stored
/// thinking blocks are replayed only when it did.
pub(super) fn translate(history: &[Value], thinking: Thinking) -> Translated {
    let mut system_parts: Vec<String> = Vec::new();
    let mut items: Vec<Item> = Vec::new();
    let mut ids = SynthIds::default();

    for message in history {
        classify(message, &mut system_parts, &mut items, &mut ids, thinking);
    }
    prune_orphans(&mut items);

    Translated {
        system: join_system(&system_parts),
        messages: ensure_opens_with_user(group_tool_results(items)),
    }
}

/// Sort one history message into the system prompt or an [`Item`].
fn classify(
    message: &Value,
    system_parts: &mut Vec<String>,
    items: &mut Vec<Item>,
    ids: &mut SynthIds,
    thinking: Thinking,
) {
    let content = message.get("content");
    match message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        // Hoisted wherever it appears: mid-conversation `role:"system"` is
        // model-gated on Anthropic and unsupported on most models.
        "system" => push_text(system_parts, plain_text(content)),
        "tool" => items.push(tool_result_item(message, ids)),
        "user" => push_blocks(items, Item::User(content_blocks(content))),
        "assistant" => {
            ids.start_turn();
            let blocks = assistant_blocks(message, ids, thinking);
            push_blocks(items, Item::Assistant(blocks));
        }
        // Unknown roles are dropped rather than guessed at.
        _ => {}
    }
}

fn push_text(parts: &mut Vec<String>, text: String) {
    if !text.trim().is_empty() {
        parts.push(text);
    }
}

/// Keep an item only if it carries something the model can act on.
///
/// Thinking blocks do not count. An assistant turn that is nothing but replayed
/// reasoning says nothing the next turn needs and is not a shape the API
/// expects, so it is dropped along with its blocks.
fn push_blocks(items: &mut Vec<Item>, item: Item) {
    let empty = match &item {
        Item::User(blocks) | Item::Assistant(blocks) => !has_substance(blocks),
        Item::ToolResult { .. } => false,
    };
    if !empty {
        items.push(item);
    }
}

/// True when at least one block is something other than replayed thinking.
fn has_substance(blocks: &[Value]) -> bool {
    blocks.iter().any(|block| !thinking::is_block(block))
}

/// Thinking blocks, then text, then `tool_use`. `content` is `Value::Null`
/// whenever the assistant produced only tool calls.
///
/// Thinking comes first because the API requires it: a turn that reasoned
/// before acting has to be replayed in that order, byte for byte.
fn assistant_blocks(message: &Value, ids: &mut SynthIds, thinking: Thinking) -> Vec<Value> {
    let mut blocks = match thinking {
        Thinking::Replay => thinking::stored_blocks(message),
        Thinking::Drop => Vec::new(),
    };
    blocks.extend(content_blocks(message.get("content")));
    let Some(calls) = message.get("tool_calls").and_then(Value::as_array) else {
        return blocks;
    };
    for call in calls {
        if let Some(block) = tool_use_block(call, ids) {
            blocks.push(block);
        }
    }
    blocks
}

/// One `OpenAI` tool call -> a `tool_use` block. Calls without a usable name are
/// dropped: Anthropic rejects the whole request over a single malformed tool.
fn tool_use_block(call: &Value, ids: &mut SynthIds) -> Option<Value> {
    // Resolve the id before deciding whether to keep the call. `turn_dispatch`
    // pushes a result for every call it dispatched, including one whose name was
    // empty, and the result side consumes synthetic ids in issue order. Skipping
    // an issue for a dropped call would hand that call's result to the *next*
    // call instead - a silently wrong tool result rather than a 400.
    let raw_id = call.get("id").and_then(Value::as_str).unwrap_or_default();
    let id = if raw_id.is_empty() {
        ids.issue()
    } else {
        raw_id.to_string()
    };
    let name = call.pointer("/function/name").and_then(Value::as_str)?;
    if name.is_empty() {
        return None;
    }
    let arguments = call
        .pointer("/function/arguments")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Some(json!({
        "type": "tool_use",
        "id": id,
        "name": name,
        "input": parse_input(arguments),
    }))
}

/// `arguments` is a JSON *string* on the wire. Anthropic needs `input` to be an
/// object, so anything empty, truncated, or non-object degrades to `{}` rather
/// than failing the request.
fn parse_input(arguments: &str) -> Value {
    match serde_json::from_str::<Value>(arguments) {
        Ok(Value::Object(map)) => Value::Object(map),
        _ => json!({}),
    }
}

fn tool_result_item(message: &Value, ids: &mut SynthIds) -> Item {
    let raw_id = message
        .get("tool_call_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let id = if raw_id.is_empty() {
        ids.take()
    } else {
        raw_id.to_string()
    };
    let text = plain_text(message.get("content"));
    let text = if text.trim().is_empty() {
        EMPTY_TOOL_OUTPUT.to_string()
    } else {
        text
    };
    Item::ToolResult { id, text }
}

/// Drop `tool_use` blocks with no matching result and `tool_result`s with no
/// matching call.
///
/// `/compress` rebuilds history by slicing the last few turns without trimming,
/// so it can leave either orphan behind. Both are 400s on Anthropic, so they are
/// pruned here rather than surfacing as a confusing provider error.
fn prune_orphans(items: &mut Vec<Item>) {
    let result_ids: HashSet<String> = items
        .iter()
        .filter_map(|item| match item {
            Item::ToolResult { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();

    for item in items.iter_mut() {
        if let Item::Assistant(blocks) = item {
            blocks.retain(|block| keep_block(block, &result_ids));
        }
    }

    let call_ids = surviving_call_ids(items);
    items.retain(|item| match item {
        Item::ToolResult { id, .. } => call_ids.contains(id),
        // `has_substance` rather than `is_empty`: pruning an unanswered
        // `tool_use` can leave a turn holding nothing but its thinking blocks,
        // and the reasoning behind an action that was cut away is not worth
        // replaying on its own.
        Item::User(blocks) | Item::Assistant(blocks) => has_substance(blocks),
    });
}

/// Keep any block that is not an unanswered `tool_use`.
fn keep_block(block: &Value, result_ids: &HashSet<String>) -> bool {
    if block.get("type").and_then(Value::as_str) != Some("tool_use") {
        return true;
    }
    block
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(|id| result_ids.contains(id))
}

fn surviving_call_ids(items: &[Item]) -> HashSet<String> {
    let mut ids = HashSet::new();
    for item in items {
        if let Item::Assistant(blocks) = item {
            ids.extend(blocks.iter().filter_map(tool_use_id));
        }
    }
    ids
}

fn tool_use_id(block: &Value) -> Option<String> {
    if block.get("type").and_then(Value::as_str) != Some("tool_use") {
        return None;
    }
    block.get("id").and_then(Value::as_str).map(String::from)
}

/// Collapse each consecutive run of tool results into one user message.
///
/// Anthropic requires every `tool_result` for an assistant turn in a single
/// message; splitting them across messages trains the model out of parallel
/// tool calls.
fn group_tool_results(items: Vec<Item>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut results: Vec<Value> = Vec::new();
    for item in items {
        match item {
            Item::ToolResult { id, text } => results.push(json!({
                "type": "tool_result",
                "tool_use_id": id,
                "content": text,
            })),
            Item::User(blocks) => {
                flush_results(&mut results, &mut out);
                out.push(message("user", blocks));
            }
            Item::Assistant(blocks) => {
                flush_results(&mut results, &mut out);
                out.push(message("assistant", blocks));
            }
        }
    }
    flush_results(&mut results, &mut out);
    out
}

fn flush_results(results: &mut Vec<Value>, out: &mut Vec<Value>) {
    if results.is_empty() {
        return;
    }
    out.push(message("user", mem::take(results)));
}

fn message(role: &str, content: Vec<Value>) -> Value {
    json!({"role": role, "content": Value::Array(content)})
}

/// Anthropic requires a non-empty `messages` opening with a user turn.
///
/// Consecutive same-role messages are *legal* - the API combines them into one
/// turn - so no merging is done here.
fn ensure_opens_with_user(mut messages: Vec<Value>) -> Vec<Value> {
    let opens_with_user = messages
        .first()
        .and_then(|m| m.get("role"))
        .and_then(Value::as_str)
        == Some("user");
    if !opens_with_user {
        messages.insert(0, message("user", vec![text_block_value(CONTINUE_TEXT)]));
    }
    messages
}

/// Content blocks for a user or assistant message. Images and documents are
/// dropped - afi never produces them, and passing them through unchecked would
/// send an unrecognized block shape.
fn content_blocks(content: Option<&Value>) -> Vec<Value> {
    match content {
        Some(Value::String(text)) => text_block(text).into_iter().collect(),
        Some(Value::Array(parts)) => parts.iter().filter_map(part_text_block).collect(),
        _ => Vec::new(),
    }
}

fn part_text_block(part: &Value) -> Option<Value> {
    match part {
        Value::String(text) => text_block(text),
        _ => text_block(part.get("text").and_then(Value::as_str)?),
    }
}

/// Anthropic rejects empty text blocks, so blank text yields no block at all.
fn text_block(text: &str) -> Option<Value> {
    if text.trim().is_empty() {
        return None;
    }
    Some(text_block_value(text))
}

fn text_block_value(text: &str) -> Value {
    json!({"type": "text", "text": text})
}

/// Flatten content to plain text, for the system prompt and tool results.
fn plain_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(part_plain_text)
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn part_plain_text(part: &Value) -> Option<String> {
    match part {
        Value::String(text) => Some(text.clone()),
        _ => part.get("text").and_then(Value::as_str).map(String::from),
    }
}

fn join_system(parts: &[String]) -> Option<String> {
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("\n\n"))
}

#[cfg(test)]
mod tests;
