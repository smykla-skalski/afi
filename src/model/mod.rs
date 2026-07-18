//! Model interaction: streaming chat completions, context window probing,
//! usage normalization, and the model turn loop.
//!
//! Phase 5 implements the client, context window probing, SSE parsing, and
//! usage normalization. The full `model_turn` loop (with tool dispatch,
//! recovery, and compression) lands across phases 5-8.

pub mod client;
pub mod context_window;
pub mod stream;

// --- turn status constants (match the Python TURN_* values) ---------------

pub const TURN_DONE: &str = "done";
pub const TURN_ESC: &str = "esc";
pub const TURN_STREAM_CUT: &str = "stream_cut";
pub const TURN_EMPTY: &str = "empty";
pub const TURN_FORCE_FINAL: &str = "force_final";
pub const TURN_LOOP_CUT: &str = "loop_cut";

// --- forced-final tool schema -----------------------------------------------

use once_cell::sync::Lazy;
use serde_json::json;

pub static FINAL_ANSWER_TOOL: Lazy<serde_json::Value> = Lazy::new(|| {
    json!({
        "type": "function",
        "function": {
            "name": "final_answer",
            "description": "Return a complete, concise visible answer to the user. \
                Use this when no more tool calls are needed.",
            "parameters": {
                "type": "object",
                "properties": {
                    "answer": {
                        "type": "string",
                        "description": "The complete concise answer to show to the user. \
                            Keep it short and do not trail off."
                    },
                    "status": {"type": "string", "enum": ["answered", "blocked"]}
                },
                "required": ["answer"]
            }
        }
    })
});

pub static FINAL_ANSWER_TOOL_CHOICE: Lazy<serde_json::Value> = Lazy::new(|| {
    json!({
        "type": "function",
        "function": {"name": "final_answer"}
    })
});

// --- env-tunable constants (resolved at runtime from the env map) -----------

/// Resolve env-tunable constants from the env map. These mirror the Python
/// module-level constants that are resolved from `MINION_*` env vars.
#[derive(Debug, Clone)]
pub struct ModelConfig {
    pub reasoning_only_char_limit: usize,
    pub reasoning_only_time_limit: u64,
    pub reasoning_only_retry_limit: u32,
    pub malformed_stream_retry_limit: u32,
    pub empty_turn_retry_limit: u32,
    pub forced_final_max_tokens: u32,
    pub max_completion_tokens: u32,
    pub tool_result_chars: usize,
    pub session_desc_refresh: u32,
    pub recovery_temperature: f64,
    pub recovery_top_p: f64,
    pub recovery_min_p: Option<f64>,
    pub recovery_repeat_penalty: Option<f64>,
    pub recovery_repeat_last_n: Option<i64>,
    pub recovery_dry_multiplier: Option<f64>,
    pub recovery_dry_base: Option<f64>,
    pub recovery_dry_allowed_length: Option<i64>,
    pub autocompress_percent: u32,
    pub read_file_lines: i64,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            reasoning_only_char_limit: 36000,
            reasoning_only_time_limit: 120,
            reasoning_only_retry_limit: 3,
            malformed_stream_retry_limit: 2,
            empty_turn_retry_limit: 3,
            forced_final_max_tokens: 2048,
            max_completion_tokens: 16000,
            tool_result_chars: 20000,
            session_desc_refresh: 6,
            recovery_temperature: 1.0,
            recovery_top_p: 0.95,
            recovery_min_p: Some(0.02),
            recovery_repeat_penalty: Some(1.2),
            recovery_repeat_last_n: Some(512),
            recovery_dry_multiplier: Some(0.8),
            recovery_dry_base: Some(1.75),
            recovery_dry_allowed_length: Some(2),
            autocompress_percent: 85,
            read_file_lines: 400,
        }
    }
}

impl ModelConfig {
    /// Resolve from an env map (typically `Runtime::env`). Falls back to
    /// defaults when vars are missing or invalid.
    pub fn from_env(env: &std::collections::HashMap<String, String>) -> Self {
        let backend = env
            .get("MINION_BACKEND")
            .map(|v| v.trim().to_lowercase())
            .unwrap_or_default();
        let is_vllm = backend == "vllm";

        Self {
            reasoning_only_char_limit: env_int(env, "MINION_REASONING_ONLY_CHARS", 36000) as usize,
            reasoning_only_time_limit: env_int(env, "MINION_REASONING_ONLY_TIME", 120) as u64,
            reasoning_only_retry_limit: env_int(env, "MINION_REASONING_ONLY_RETRIES", 3) as u32,
            malformed_stream_retry_limit: env_int(env, "MINION_MALFORMED_STREAM_RETRIES", 2) as u32,
            empty_turn_retry_limit: env_int(env, "MINION_EMPTY_TURN_RETRIES", 3) as u32,
            forced_final_max_tokens: env_int(env, "MINION_FORCED_FINAL_MAX_TOKENS", 2048) as u32,
            max_completion_tokens: env_int(env, "MINION_MAX_TOKENS", 16000) as u32,
            tool_result_chars: env_int(env, "MINION_TOOL_RESULT_CHARS", 20000) as usize,
            session_desc_refresh: env_int(env, "MINION_SESSION_DESC_REFRESH", 6) as u32,
            recovery_temperature: env_float(env, "MINION_RECOVERY_TEMPERATURE", 1.0),
            recovery_top_p: env_float(env, "MINION_RECOVERY_TOP_P", 0.95),
            recovery_min_p: if is_vllm {
                None
            } else {
                Some(env_float(env, "MINION_RECOVERY_MIN_P", 0.02))
            },
            recovery_repeat_penalty: if is_vllm {
                None
            } else {
                Some(env_float(env, "MINION_RECOVERY_REPEAT_PENALTY", 1.2))
            },
            recovery_repeat_last_n: if is_vllm {
                None
            } else {
                Some(env_int(env, "MINION_RECOVERY_REPEAT_LAST_N", 512))
            },
            recovery_dry_multiplier: if is_vllm {
                None
            } else {
                Some(env_float(env, "MINION_RECOVERY_DRY_MULTIPLIER", 0.8))
            },
            recovery_dry_base: if is_vllm {
                None
            } else {
                Some(env_float(env, "MINION_RECOVERY_DRY_BASE", 1.75))
            },
            recovery_dry_allowed_length: if is_vllm {
                None
            } else {
                Some(env_int(env, "MINION_RECOVERY_DRY_ALLOWED_LENGTH", 2))
            },
            autocompress_percent: env_int(env, "MINION_AUTOCOMPRESS_PERCENT", 85).clamp(0, 100)
                as u32,
            read_file_lines: env_int(env, "MINION_READ_FILE_LINES", 400),
        }
    }
}

fn env_int(env: &std::collections::HashMap<String, String>, name: &str, default: i64) -> i64 {
    env.get(name)
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(default)
}

fn env_float(env: &std::collections::HashMap<String, String>, name: &str, default: f64) -> f64 {
    env.get(name)
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(default)
}
