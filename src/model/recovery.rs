//! Recovery samplers and nudge strings for broken/looping model output.
//!
//! Recovery retries (malformed-stream, reasoning-only-stall, `/recover`) swap
//! in a higher-entropy, anti-repetition sampler so the model doesn't collapse
//! back into the same broken output.

use serde_json::{json, Value};

use crate::model::ModelConfig;

/// Nudge for reasoning-only stalls: tell the model to emit a visible answer.
pub const FORCED_FINAL_NUDGE: &str = "Your previous streamed response produced reasoning only. \
    Do not continue private reasoning. Use the final_answer tool if available. \
    Return a complete visible answer now in at most six short bullets or paragraphs. \
    If you are blocked, say exactly what is blocking you and what input is needed.";

/// Nudge for empty turns (no content, no tool calls).
pub const EMPTY_TURN_NUDGE: &str = "Your previous response was empty - you produced no visible \
    text and no tool call, so nothing happened. Do not stop. If the task is not finished, \
    emit the next tool call now. If you have everything you need, use the final_answer tool \
    (or reply with a concise visible answer). Do not repeat the empty turn.";

/// Nudge for `/recover` (manual recovery).
pub const MANUAL_RECOVERY_NUDGE: &str = "Manual recovery requested by the user because the \
    previous response appeared off the rails or corrupted. Discard any corrupted \
    reasoning/output and do not continue private reasoning. Use the final_answer tool \
    if available. Return a bounded visible checkpoint with: (1) the last valid result \
    you can rely on, (2) the next concrete action you would take, and (3) any blocker \
    or uncertainty. Keep it concise.";

/// Build the recovery sampler opts as a JSON Value to merge into the request.
///
/// `temperature` and `top_p` are standard OpenAI params (go top-level).
/// `min_p`, `repeat_penalty`, `repeat_last_n`, and the DRY family are
/// llama.cpp extensions that ride in `extra_body`. Non-llama.cpp endpoints
/// ignore unknown keys. Set any `RECOVERY_*` value negative to omit it.
/// When `MINION_BACKEND=vllm` the llama.cpp-only knobs are `None` so they're
/// omitted automatically.
pub fn recovery_sampling_opts(config: &ModelConfig) -> Value {
    let mut opts = json!({});

    if config.recovery_temperature >= 0.0 {
        opts["temperature"] = json!(config.recovery_temperature);
    }
    if config.recovery_top_p >= 0.0 {
        opts["top_p"] = json!(config.recovery_top_p);
    }

    let mut extra = json!({});
    if let Some(min_p) = config.recovery_min_p {
        if min_p >= 0.0 {
            extra["min_p"] = json!(min_p);
        }
    }
    if let Some(rp) = config.recovery_repeat_penalty {
        if rp >= 0.0 {
            extra["repeat_penalty"] = json!(rp);
            if let Some(rln) = config.recovery_repeat_last_n {
                extra["repeat_last_n"] = json!(rln);
            }
        }
    }
    if let Some(drm) = config.recovery_dry_multiplier {
        if drm > 0.0 {
            extra["dry_multiplier"] = json!(drm);
            if let Some(db) = config.recovery_dry_base {
                extra["dry_base"] = json!(db);
            }
            if let Some(dal) = config.recovery_dry_allowed_length {
                extra["dry_allowed_length"] = json!(dal);
            }
            // DRY sequence breakers: path/code punctuation so a long file path
            // the model must emit verbatim is never penalized as repetition.
            extra["dry_sequence_breakers"] = json!(["\n", ":", "\"", "*", "/", "\\", "`", "'"]);
        }
    }

    if !extra.as_object().map(|m| m.is_empty()).unwrap_or(true) {
        opts["extra_body"] = extra;
    }

    opts
}

/// Strip any prior `[Runtime note: ...]` from the latest user turn and append
/// `nudge`. If there's no user turn, append a new one.
pub fn nudge_current_user_turn(messages: &mut Vec<Value>, nudge: &str) {
    let note = format!("[Runtime note: {}]", nudge);

    // Walk backwards to find the last user message with string content.
    for msg in messages.iter_mut().rev() {
        if msg.get("role").and_then(|r| r.as_str()) != Some("user") {
            continue;
        }
        if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
            // Strip any existing runtime note.
            let cleaned = strip_runtime_notes(content);
            let new_content = if cleaned.is_empty() {
                note.clone()
            } else {
                format!("{}\n\n{}", cleaned.trim_end(), note)
            };
            msg["content"] = json!(new_content);
            return;
        }
    }

    // No user message found - append a new one.
    messages.push(json!({"role": "user", "content": note}));
}

/// Remove `[Runtime note: ...]` blocks from a string.
fn strip_runtime_notes(content: &str) -> String {
    let re = regex::Regex::new(r#"\[Runtime note:[^\]]*\]"#).unwrap();
    re.replace_all(content, "").to_string()
}

/// True if the last message is a `tool` result with no assistant turn after it.
/// This is the layout right after a tool ran and before the model follows up:
/// an empty assistant turn here is most suspicious.
pub fn last_is_dangling_tool(messages: &[Value]) -> bool {
    match messages.last() {
        Some(msg) => msg.get("role").and_then(|r| r.as_str()) == Some("tool"),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_config() -> ModelConfig {
        ModelConfig::default()
    }

    #[test]
    fn recovery_sampling_opts_includes_temperature_and_top_p() {
        let config = make_config();
        let opts = recovery_sampling_opts(&config);
        assert_eq!(opts["temperature"], json!(1.0));
        assert_eq!(opts["top_p"], json!(0.95));
    }

    #[test]
    fn recovery_sampling_opts_includes_llamacpp_extras() {
        let config = make_config();
        let opts = recovery_sampling_opts(&config);
        let extra = &opts["extra_body"];
        assert_eq!(extra["min_p"], json!(0.02));
        assert_eq!(extra["repeat_penalty"], json!(1.2));
        assert_eq!(extra["repeat_last_n"], json!(512));
        assert_eq!(extra["dry_multiplier"], json!(0.8));
        assert_eq!(extra["dry_base"], json!(1.75));
        assert_eq!(extra["dry_allowed_length"], json!(2));
        assert!(extra["dry_sequence_breakers"].is_array());
    }

    #[test]
    fn recovery_sampling_opts_vllm_omits_llamacpp_knobs() {
        let mut config = make_config();
        config.recovery_min_p = None;
        config.recovery_repeat_penalty = None;
        config.recovery_repeat_last_n = None;
        config.recovery_dry_multiplier = None;
        config.recovery_dry_base = None;
        config.recovery_dry_allowed_length = None;
        let opts = recovery_sampling_opts(&config);
        // Should still have temperature and top_p but no extra_body.
        assert_eq!(opts["temperature"], json!(1.0));
        assert_eq!(opts["top_p"], json!(0.95));
        assert!(
            opts.get("extra_body").is_none() || opts["extra_body"].as_object().unwrap().is_empty()
        );
    }

    #[test]
    fn nudge_appends_to_last_user_turn() {
        let mut messages = vec![
            json!({"role": "system", "content": "sys"}),
            json!({"role": "user", "content": "hello"}),
            json!({"role": "assistant", "content": "hi"}),
        ];
        nudge_current_user_turn(&mut messages, "do something");
        assert_eq!(messages[1]["role"], "user");
        assert!(messages[1]["content"].as_str().unwrap().contains("hello"));
        assert!(messages[1]["content"]
            .as_str()
            .unwrap()
            .contains("[Runtime note: do something]"));
    }

    #[test]
    fn nudge_replaces_existing_runtime_note() {
        let mut messages =
            vec![json!({"role": "user", "content": "hello\n\n[Runtime note: old nudge]"})];
        nudge_current_user_turn(&mut messages, "new nudge");
        let content = messages[0]["content"].as_str().unwrap();
        assert!(content.contains("hello"));
        assert!(!content.contains("old nudge"));
        assert!(content.contains("[Runtime note: new nudge]"));
    }

    #[test]
    fn nudge_creates_user_turn_if_none_exists() {
        let mut messages = vec![
            json!({"role": "system", "content": "sys"}),
            json!({"role": "assistant", "content": "hi"}),
        ];
        nudge_current_user_turn(&mut messages, "test nudge");
        assert_eq!(messages.last().unwrap()["role"], "user");
        assert!(messages.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("[Runtime note: test nudge]"));
    }

    #[test]
    fn last_is_dangling_tool_true() {
        let messages = vec![
            json!({"role": "user", "content": "hi"}),
            json!({"role": "assistant", "tool_calls": []}),
            json!({"role": "tool", "content": "result"}),
        ];
        assert!(last_is_dangling_tool(&messages));
    }

    #[test]
    fn last_is_dangling_tool_false() {
        let messages = vec![
            json!({"role": "user", "content": "hi"}),
            json!({"role": "assistant", "content": "hi"}),
        ];
        assert!(!last_is_dangling_tool(&messages));
    }
}
