//! Machine-readable run summary, for CI that needs the result rather than the
//! rendered transcript.
//!
//! A review run in a workflow needs two things out of `afi`: the text the model
//! finally produced, so it can be posted, and the run's real token accounting,
//! so it can be reported. Both are printed as one JSON object on stdout after
//! the run, leaving the workflow to decide what to do with it.
//!
//! Cost is deliberately absent. Anthropic returns no cost figure, so any number
//! here would come from a hard-coded price table that silently goes stale and
//! reports a wrong figure with total confidence. Token counts are exact; a
//! caller that wants money multiplies them by rates it controls.

use serde_json::{Value, json};

use crate::model::usage_totals::UsageTotals;

/// How to report the run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SummaryFormat {
    /// Print nothing extra. The default, so existing behaviour is unchanged.
    #[default]
    None,
    /// One JSON object on stdout.
    Json,
}

impl SummaryFormat {
    /// Parse `--summary` / `AFI_SUMMARY`. An unrecognized value is `None` rather
    /// than an error: a typo must not lose a completed run's output.
    #[must_use]
    pub fn from_value(raw: Option<&str>) -> Self {
        match raw.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("json") => Self::Json,
            _ => Self::None,
        }
    }

    #[must_use]
    pub fn is_json(self) -> bool {
        self == Self::Json
    }
}

/// Everything the summary reports on.
#[derive(Debug, Clone)]
pub struct RunSummary<'a> {
    /// False when the run failed, so a consumer never reads a partial answer as
    /// a finished one.
    pub ok: bool,
    /// Present only on failure.
    pub error: Option<&'a str>,
    pub source: Option<&'a str>,
    pub model: Option<&'a str>,
    /// The last assistant text of the run - the answer a review flow posts.
    pub answer: &'a str,
    pub usage: UsageTotals,
    pub elapsed_secs: f64,
    /// The tools the run was permitted to call. Reported so an audit of a CI run
    /// can confirm the restriction from the output alone, without trusting that
    /// the workflow passed the flag it claims to.
    pub tools: Vec<&'static str>,
}

impl RunSummary<'_> {
    /// Render as JSON.
    ///
    /// `usage` is null rather than a zeroed object when no request reported any,
    /// so a consumer can tell "the provider sent no usage" from "the run used no
    /// tokens" instead of silently charting zeros.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "ok": self.ok,
            "error": self.error,
            "source": self.source,
            "model": self.model,
            "answer": self.answer,
            "usage": self.usage_json(),
            "elapsed_secs": round_millis(self.elapsed_secs),
            "tools": self.tools,
        })
    }

    fn usage_json(&self) -> Value {
        if self.usage.is_empty() {
            return Value::Null;
        }
        json!({
            "input_tokens": self.usage.input_tokens,
            "output_tokens": self.usage.output_tokens,
            "cache_read_tokens": self.usage.cache_read_tokens,
            "cache_write_tokens": self.usage.cache_write_tokens,
            "reasoning_tokens": self.usage.reasoning_tokens,
            "total_tokens": self.usage.total_tokens(),
            "requests": self.usage.requests,
        })
    }
}

/// Trim float noise so the field is stable enough to assert on.
fn round_millis(secs: f64) -> f64 {
    (secs * 1000.0).round() / 1000.0
}

/// The last assistant message with non-blank string content.
///
/// Walks backwards because a run ends with the final answer, and skips
/// tool-call-only turns, whose content is `null` or empty.
#[must_use]
pub fn final_answer(messages: &[Value]) -> &str {
    messages
        .iter()
        .rev()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
        .filter_map(|m| m.get("content").and_then(Value::as_str))
        .find(|text| !text.trim().is_empty())
        .unwrap_or("")
}

#[cfg(test)]
mod tests;
