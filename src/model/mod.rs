//! Model interaction: streaming chat completions, context window probing,
//! usage normalization, and the model turn loop.
//!
//! Phase 5 implements the client, context window probing, SSE parsing, and
//! usage normalization. The full `model_turn` loop (with tool dispatch,
//! recovery, and compression) lands across phases 5-8.

use crate::summary::{ErrorKind, RunError};
use crate::term::{MessageKind, UserInterface};

pub mod client;
pub mod compress;
pub mod context_window;
pub mod recovery;
pub mod stream;
pub mod turn;
pub mod turn_dispatch;
pub mod turn_finalize;
pub mod turn_loop;
pub mod turn_stats;
pub mod turn_stream;
pub mod usage_totals;

// --- turn status constants (match the Python TURN_* values) ---------------

pub const TURN_DONE: &str = "done";
pub const TURN_TOOL: &str = "tool";
pub const TURN_ESC: &str = "esc";
pub const TURN_STREAM_CUT: &str = "stream_cut";
pub const TURN_EMPTY: &str = "empty";
pub const TURN_FORCE_FINAL: &str = "force_final";
pub const TURN_STALL: &str = "stall";
/// The request itself failed - unreachable server, HTTP error, bad config.
///
/// Terminal like `TURN_DONE`, but distinct from it so a caller can tell a failed
/// run from a finished one. Client errors used to report `TURN_DONE`, which made
/// a one-shot run exit 0 after printing an HTTP error.
pub const TURN_FAILED: &str = "failed";

/// How a turn ended: its TURN_* status, why it failed when it did, and how much
/// context it was charged for.
///
/// The status is what the retry loop branches on. The failure rides along because
/// it has to reach the run summary, and the `ClientError` explaining it is gone by
/// then - rendered to the ui and dropped. The token count rides along for the
/// same kind of reason: the loop decides whether the next turn still fits, and the
/// only authority on how full the context is is the provider that just counted it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnOutcome {
    /// One of the `TURN_*` constants. Borrowed rather than owned, so a status
    /// can only ever be one of them - and so a turn costs no allocation to
    /// describe.
    pub status: &'static str,
    /// Private, so nothing can set a reason without a failing status or the
    /// reverse. [`Self::error`] is the only read, and it covers a `TURN_FAILED`
    /// that reached here by some other route.
    error: Option<RunError>,
    /// What the provider counted as this turn's prompt, `0` for a turn it
    /// reported nothing for. Private for the same reason as `error`:
    /// [`Self::with_prompt_tokens`] is the only way in, and [`Self::prompt_tokens`]
    /// is the only way out.
    prompt_tokens: u64,
}

impl TurnOutcome {
    /// A turn that ended on `status` without failing.
    #[must_use]
    pub fn new(status: &'static str) -> Self {
        // `failed` is the only way to reach TURN_FAILED, so that a status and a
        // reason cannot disagree. Debug-only because the release fallback in
        // `error` already covers it, and losing the summary is worse than
        // reporting it as `internal`.
        debug_assert_ne!(status, TURN_FAILED, "a failure must carry its reason");
        Self {
            status,
            error: None,
            prompt_tokens: 0,
        }
    }

    /// A turn that failed outright, carrying the reason to report for it.
    #[must_use]
    pub fn failed(error: RunError) -> Self {
        Self {
            status: TURN_FAILED,
            error: Some(error),
            prompt_tokens: 0,
        }
    }

    /// Record what the provider counted as this turn's prompt.
    ///
    /// Set once, by the turn that read the usage off the stream, whatever status
    /// that turn ended on: a turn that was paid for has a context size worth
    /// knowing even when it produced no answer.
    #[must_use]
    pub fn with_prompt_tokens(mut self, prompt_tokens: u64) -> Self {
        self.prompt_tokens = prompt_tokens;
        self
    }

    /// How full the context was on this turn's request, `None` when the provider
    /// reported nothing - a turn that never reached one, or one whose answer
    /// carried no usage.
    ///
    /// Zero is reported as `None` rather than as a count: no request has an empty
    /// prompt, so a zero is a provider that declined to say.
    #[must_use]
    pub fn prompt_tokens(&self) -> Option<u64> {
        (self.prompt_tokens > 0).then_some(self.prompt_tokens)
    }

    /// Fail a turn and say so on the ui, in one sentence sent to both.
    ///
    /// The log line and the summary's `error` are the same string by
    /// construction. Reporting them separately is how they drift, and a CI job
    /// correlating a JSON field against a log line is the work `error_kind`
    /// exists to remove.
    pub fn report(ui: &mut dyn UserInterface, message: String, kind: ErrorKind) -> Self {
        ui.message(MessageKind::Error, message.clone());
        Self::failed(RunError::new(message, kind))
    }

    /// Fail a turn the model was paid for but produced no answer from.
    pub fn no_answer(ui: &mut dyn UserInterface, message: String) -> Self {
        Self::report(ui, message, ErrorKind::NoAnswer)
    }

    /// Whether the turn failed outright.
    #[must_use]
    pub fn is_failure(&self) -> bool {
        self.status == TURN_FAILED
    }

    /// The reason to report, or `None` when the turn did not fail.
    ///
    /// A failed turn always explains itself, so the fallback is only reachable
    /// through an afi bug - a `TURN_FAILED` built by another route, which is what
    /// `Internal` is for. Never `None` for a failure: `ok: false` with no reason
    /// would put a caller straight back to guessing.
    #[must_use]
    pub fn error(&self) -> Option<RunError> {
        if !self.is_failure() {
            return None;
        }
        Some(self.error.clone().unwrap_or_else(|| {
            RunError::new("the turn failed without saying why", ErrorKind::Internal)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_with_no_reason_still_reports_one() {
        // Built as a literal because `new` refuses this state and `failed` cannot
        // produce it - the point is what a release build does if an afi bug ever
        // gets one past the debug assertion. Reporting `ok: false` with a null
        // reason would send a caller back to the substring matching `error_kind`
        // exists to end.
        let outcome = TurnOutcome {
            status: TURN_FAILED,
            error: None,
            prompt_tokens: 0,
        };
        let error = outcome.error().expect("a failure explains itself");
        assert_eq!(error.kind, ErrorKind::Internal);
        assert!(!error.message.is_empty());
    }

    #[test]
    fn a_finished_turn_has_nothing_to_report() {
        assert!(TurnOutcome::new(TURN_DONE).error().is_none());
    }
}

// --- forced-final tool schema -----------------------------------------------

use serde_json::json;
use std::collections::HashMap;

use crate::tools::policy::ToolPolicy;
use std::sync::LazyLock;

pub static FINAL_ANSWER_TOOL: LazyLock<serde_json::Value> = LazyLock::new(|| {
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

pub static FINAL_ANSWER_TOOL_CHOICE: LazyLock<serde_json::Value> = LazyLock::new(|| {
    json!({
        "type": "function",
        "function": {"name": "final_answer"}
    })
});

// --- env-tunable constants (resolved at runtime from the env map) -----------

/// Resolve env-tunable constants from the env map. These mirror the Python
/// module-level constants that are resolved from `AFI_*` env vars.
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
    /// Which tools this run may call. Resolved from the env so every
    /// `from_env` caller agrees; `Runtime::build` writes the CLI flags into the
    /// env map first, which is what makes the flags win.
    pub tool_policy: ToolPolicy,
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
            tool_policy: ToolPolicy::default(),
        }
    }
}

impl ModelConfig {
    /// Resolve from an env map (typically `Runtime::env`). Falls back to
    /// defaults when vars are missing or invalid.
    #[must_use]
    pub fn from_env(env: &HashMap<String, String>) -> Self {
        let backend = env
            .get("AFI_BACKEND")
            .map(|v| v.trim().to_lowercase())
            .unwrap_or_default();
        let is_vllm = backend == "vllm";

        Self {
            reasoning_only_char_limit: env_usize(env, "AFI_REASONING_ONLY_CHARS", 36000),
            reasoning_only_time_limit: env_u64(env, "AFI_REASONING_ONLY_TIME", 120),
            reasoning_only_retry_limit: env_u32(env, "AFI_REASONING_ONLY_RETRIES", 3),
            malformed_stream_retry_limit: env_u32(env, "AFI_MALFORMED_STREAM_RETRIES", 2),
            empty_turn_retry_limit: env_u32(env, "AFI_EMPTY_TURN_RETRIES", 3),
            forced_final_max_tokens: env_u32(env, "AFI_FORCED_FINAL_MAX_TOKENS", 2048),
            max_completion_tokens: env_u32(env, "AFI_MAX_TOKENS", 16000),
            tool_result_chars: env_usize(env, "AFI_TOOL_RESULT_CHARS", 20000),
            session_desc_refresh: env_u32(env, "AFI_SESSION_DESC_REFRESH", 6),
            recovery_temperature: env_float(env, "AFI_RECOVERY_TEMPERATURE", 1.0),
            recovery_top_p: env_float(env, "AFI_RECOVERY_TOP_P", 0.95),
            recovery_min_p: if is_vllm {
                None
            } else {
                Some(env_float(env, "AFI_RECOVERY_MIN_P", 0.02))
            },
            recovery_repeat_penalty: if is_vllm {
                None
            } else {
                Some(env_float(env, "AFI_RECOVERY_REPEAT_PENALTY", 1.2))
            },
            recovery_repeat_last_n: if is_vllm {
                None
            } else {
                Some(env_int(env, "AFI_RECOVERY_REPEAT_LAST_N", 512))
            },
            recovery_dry_multiplier: if is_vllm {
                None
            } else {
                Some(env_float(env, "AFI_RECOVERY_DRY_MULTIPLIER", 0.8))
            },
            recovery_dry_base: if is_vllm {
                None
            } else {
                Some(env_float(env, "AFI_RECOVERY_DRY_BASE", 1.75))
            },
            recovery_dry_allowed_length: if is_vllm {
                None
            } else {
                Some(env_int(env, "AFI_RECOVERY_DRY_ALLOWED_LENGTH", 2))
            },
            autocompress_percent: env_u32(env, "AFI_AUTOCOMPRESS_PERCENT", 85).min(100),
            read_file_lines: env_int(env, "AFI_READ_FILE_LINES", 400),
            tool_policy: ToolPolicy::from_env(
                env.get("AFI_ALLOWED_TOOLS").map(String::as_str),
                env.get("AFI_DISALLOWED_TOOLS").map(String::as_str),
                env.get("AFI_READ_ONLY").map(String::as_str),
            ),
        }
    }
}

fn env_int(env: &HashMap<String, String>, name: &str, default: i64) -> i64 {
    env.get(name)
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(default)
}

fn env_float(env: &HashMap<String, String>, name: &str, default: f64) -> f64 {
    env.get(name)
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(default)
}

fn env_u32(env: &HashMap<String, String>, name: &str, default: u32) -> u32 {
    env.get(name)
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(default)
}

fn env_u64(env: &HashMap<String, String>, name: &str, default: u64) -> u64 {
    env.get(name)
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_usize(env: &HashMap<String, String>, name: &str, default: usize) -> usize {
    env.get(name)
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(default)
}
