//! Machine-readable run summary, for CI that needs the result rather than the
//! rendered transcript.
//!
//! A review run in a workflow needs two things out of `afi`: the text the model
//! finally produced, so it can be posted, and the run's real token accounting,
//! so it can be reported. Both are printed as one JSON object on stdout after
//! the run, leaving the workflow to decide what to do with it.
//!
//! A failed run needs a third thing: which kind of failure it was, so the
//! workflow can tell a retry worth making from a dead end without reading the
//! log - see [`ErrorKind`].
//!
//! Cost is reported only when the caller supplied rates in `AFI_PRICES`. No
//! provider here returns a cost figure, so a price table compiled into afi would
//! be the only source of one, and a table nobody notices going stale reports a
//! wrong number with total confidence. Unpriced runs therefore carry no
//! `cost_usd` key at all - see `crate::pricing`.
//!
//! The same object can also be written to a path rather than piped - see
//! [`file`] for why that is worth offering and what makes the write safe to
//! read.

use serde_json::{Number, Value, json};

use crate::model::usage_totals::{RefusedToolCalls, UsageTotals};

mod auth;
mod file;
mod format;
mod schema;
mod spend;
pub use auth::RunAuth;
pub use file::{summary_path, writable, write_file};
pub use format::SummaryFormat;
pub use schema::SCHEMA_VERSION;
pub use spend::SourceSpend;

/// Why a run failed, as a closed set a caller can branch on.
///
/// Retry policy is the reason this exists. A timeout, a rate limit, or a cut
/// stream is worth another attempt; a rejected credential never is, and retrying
/// that one burns the schedule to arrive at the same answer. Telling the two
/// apart from `error` alone means substring-matching a sentence that changes
/// wording, and failing silently when it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// A credential was missing, unusable, or refused. A federated identity
    /// exchange turned down by its own rule lands here too, even though it
    /// arrives as an HTTP status: retrying it cannot change the answer.
    Auth,
    /// The run refused to start because its tool policy could not be honoured.
    Policy,
    /// The invocation itself was wrong - no prompt to read, or no source
    /// configured to send it to.
    Input,
    /// The provider answered with a failing status, or never answered at all. A
    /// rate limit is here rather than in its own kind, since what a caller does
    /// about one is what it does about capacity generally.
    ProviderHttp,
    /// The response opened and then broke: a stream cut mid-answer, or bytes afi
    /// could not decode as one.
    ProviderStream,
    /// A request outlived its deadline.
    Timeout,
    /// The model was reached, and billed for, but never produced an answer: it
    /// looped in its own reasoning, or the forced final came back empty. afi has
    /// already spent its own retries by the time this is reported.
    NoAnswer,
    /// A bug in afi. Nothing a caller can do about it but report it.
    Internal,
}

impl ErrorKind {
    /// The wire value. Stable: callers branch on these strings.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::Policy => "policy",
            Self::Input => "input",
            Self::ProviderHttp => "provider_http",
            Self::ProviderStream => "provider_stream",
            Self::Timeout => "timeout",
            Self::NoAnswer => "no_answer",
            Self::Internal => "internal",
        }
    }
}

/// What a failed run reports: the sentence whoever reads the log needs, and the
/// closed-set kind a caller branches on.
///
/// The two travel together because they describe one failure. Reporting a kind
/// without the sentence would leave a workflow able to decide but unable to say
/// why, and the sentence without the kind is where this started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunError {
    pub message: String,
    pub kind: ErrorKind,
}

impl RunError {
    #[must_use]
    pub fn new(message: impl Into<String>, kind: ErrorKind) -> Self {
        Self {
            message: message.into(),
            kind,
        }
    }
}

/// Everything the summary reports on.
#[derive(Debug, Clone)]
pub struct RunSummary<'a> {
    /// False when the run failed, so a consumer never reads a partial answer as
    /// a finished one.
    pub ok: bool,
    /// Present only on failure, and the same sentence the run printed to stderr,
    /// so a log line and the JSON never disagree about what happened.
    pub error: Option<&'a str>,
    /// Which kind of failure it was. Set whenever `error` is, so a caller that
    /// branches on the kind never has to fall back to reading the sentence.
    pub error_kind: Option<ErrorKind>,
    pub source: Option<&'a str>,
    pub model: Option<&'a str>,
    /// The last assistant text of the run - the answer a review flow posts.
    pub answer: &'a str,
    pub usage: UsageTotals,
    /// What the run cost, when the caller configured rates for every model and
    /// token class it used. `None` leaves the field out entirely rather than
    /// reporting a zero a consumer would chart as free.
    pub cost_usd: Option<f64>,
    pub elapsed_secs: f64,
    /// The tools the run was permitted to call. Reported so an audit of a CI run
    /// can confirm the restriction from the output alone, without trusting that
    /// the workflow passed the flag it claims to.
    pub tools: Vec<&'static str>,
    /// The reasoning effort the requests carried, for the same reason as
    /// `tools`. `None` when nothing set one, or when the source's endpoint has
    /// no effort control afi knows - in both cases the run used the endpoint's
    /// own default, which is what a null here means.
    pub effort: Option<&'a str>,
    /// Tool calls the run asked for and did not get. `tools` is what the run was
    /// permitted; this is what it tried anyway.
    ///
    /// Reported as a total and split by what refused it, because the two halves
    /// mean different things: a policy block is the model reaching for a tool the
    /// caller ruled out, while a run with no terminal and no `--yolo` denies every
    /// mutating call at the gate by default. Always reported, zeros included, so a
    /// caller can tell "nothing was refused" from "this afi does not report
    /// refusals". A tool that ran and failed is not counted - that is an error, and
    /// folding the two together would lose the signal.
    pub refused_tool_calls: RefusedToolCalls,
    /// The credential the run billed. `None` when no single one can be named:
    /// a run with no source at all, or a session that spent on two of them.
    pub auth: Option<RunAuth<'a>>,
    /// What each source spent, for the session that `auth` cannot answer for.
    /// Empty when nothing was billed - see [`SourceSpend`].
    pub sources: Vec<SourceSpend<'a>>,
    /// How the run's system prompt was built: `builtin`, `replace`, or
    /// `append`. Reported for the same reason as `tools` - a job told to review
    /// under its own instructions and a job that fell back to afi's produce
    /// otherwise identical output.
    ///
    /// `None` only for a run that refused to start, which sent no prompt at all.
    pub system_prompt_mode: Option<&'static str>,
    /// The file that prompt came from, absent for `builtin`. The path, not the
    /// text: the prompt can be long, and a workflow that wants to know what was
    /// sent has the file.
    pub system_prompt_file: Option<&'a str>,
}

impl<'a> RunSummary<'a> {
    /// The summary for a run that refused to start.
    ///
    /// Nothing ran, so there is nothing to report but the reason: no answer, no
    /// usage, and an empty `tools` list rather than the wide set an unhonourable
    /// policy resolved to - publishing that set is exactly what refusing avoids.
    ///
    /// The kind comes from the caller because the two refusals differ: a policy
    /// that cannot be honoured is `Policy`, and a summary file that cannot be
    /// written is `Input`.
    #[must_use]
    pub fn refused(error: &'a str, kind: ErrorKind) -> Self {
        Self {
            ok: false,
            error: Some(error),
            error_kind: Some(kind),
            source: None,
            model: None,
            answer: "",
            usage: UsageTotals::default(),
            cost_usd: None,
            elapsed_secs: 0.0,
            tools: Vec::new(),
            // No request was ever sent, so no effort was carried. Reporting the
            // resolved level here would describe a run that did not happen.
            effort: None,
            // A literal rather than the live counters, which would read the same:
            // this refusal lands before any turn, so nothing has been dispatched to
            // refuse. Zeros are also what keeps `usage` null here, which is how a
            // run that never started stays distinguishable from one that ran and was
            // refused nothing.
            refused_tool_calls: RefusedToolCalls::default(),
            // No credential to name, for the reason `source` is none: the run was
            // refused before it resolved one, and nothing was billed to break
            // down either.
            auth: None,
            sources: Vec::new(),
            // Nothing was sent, so there is no prompt to name - including when the
            // prompt itself is what the run was refused over.
            system_prompt_mode: None,
            system_prompt_file: None,
        }
    }

    /// Render as JSON.
    ///
    /// Every object names its own shape - see [`SCHEMA_VERSION`] - so a field a
    /// consumer cannot find is a question about the run, not about the build.
    ///
    /// `usage` is null rather than a zeroed object when no request reported any,
    /// so a consumer can tell "the provider sent no usage" from "the run used no
    /// tokens" instead of silently charting zeros. A refusal keeps the object
    /// anyway - see `usage_json`.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "schema_version": SCHEMA_VERSION,
            "ok": self.ok,
            "error": self.error,
            "error_kind": self.error_kind.map(ErrorKind::as_str),
            "source": self.source,
            "model": self.model,
            "answer": self.answer,
            "usage": self.usage_json(),
            "elapsed_secs": round_millis(self.elapsed_secs),
            "tools": self.tools,
            "effort": self.effort,
            "auth": RunAuth::json(self.auth),
            "sources": SourceSpend::json(&self.sources),
            "system_prompt": self.system_prompt_json(),
        })
    }

    /// Null rather than an object of nulls when the run never started, matching
    /// `usage`: a consumer can tell "no prompt was sent" from "the built-in one
    /// was" instead of reading a refusal as an ordinary unconfigured run.
    fn system_prompt_json(&self) -> Value {
        let Some(mode) = self.system_prompt_mode else {
            return Value::Null;
        };
        json!({"mode": mode, "file": self.system_prompt_file})
    }

    fn usage_json(&self) -> Value {
        // A refused call is afi's own observation, so it survives a provider that
        // reported no tokens at all: dropping it would hide the one thing this
        // field exists to report. `requests` is still 0 there, which is how a
        // consumer tells the silent provider apart from a run that used nothing.
        if self.usage.is_empty() && self.refused_tool_calls.is_empty() {
            return Value::Null;
        }
        let mut usage = counts_json(&self.usage, self.cost_usd);
        // Only the run has these. A refusal is counted where it was refused,
        // which is a dispatch that knows of no request and therefore of no
        // source to bill it to - see `crate::model::usage_totals`.
        if let Some(fields) = usage.as_object_mut() {
            let refused = self.refused_tool_calls;
            fields.insert("refused_tool_calls".to_string(), refused.total().into());
            fields.insert("refused_by_policy".to_string(), refused.by_policy.into());
            fields.insert(
                "refused_by_approval".to_string(),
                refused.by_approval.into(),
            );
        }
        usage
    }
}

/// The counts every usage block reports, and the money when the caller priced
/// them.
///
/// Shared by the run's flat block and by each source's share of it, so the two
/// cannot come to report the same tokens under different names.
fn counts_json(usage: &UsageTotals, cost_usd: Option<f64>) -> Value {
    let mut counts = json!({
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "cache_read_tokens": usage.cache_read_tokens,
        "cache_write_tokens": usage.cache_write_tokens,
        "reasoning_tokens": usage.reasoning_tokens,
        "total_tokens": usage.total_tokens(),
        "requests": usage.requests,
    });
    // Inserted rather than declared in the object above, because an unpriced run
    // must have no key here at all: a null would read as "the run was free" to
    // anything summing the field.
    if let (Some(cost), Some(fields)) = (cost_usd, counts.as_object_mut())
        && let Some(number) = Number::from_f64(cost)
    {
        fields.insert("cost_usd".to_string(), Value::Number(number));
    }
    counts
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
