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
//! The same object can also be written to a path. Capturing stdout to get the
//! JSON costs the readable rendering of the run, and it puts the only machine
//! copy behind a pipe that a wrapper, a tee, or a shell printing one line of its
//! own can corrupt. A path is addressed rather than piped, so a workflow can
//! upload it as a build artifact and leave stdout to the human view.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Number, Value, json};

use crate::atomic;
use crate::model::usage_totals::{RefusedToolCalls, UsageTotals};

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

/// The path the summary is also written to, from `--summary-file` /
/// `AFI_SUMMARY_FILE`.
///
/// Independent of `SummaryFormat`: naming a file does not turn `--summary json`
/// on. Leaving stdout to the rendered run is the reason to ask for a file at
/// all, so implying the stdout copy would take back the readable output the
/// caller kept. Pass both to get both.
///
/// A blank value is no path, matching how the other variables here read a shell
/// variable that is exported but unset. The flag is stricter, because writing it
/// out is a statement that a file is wanted - see `set_required`.
#[must_use]
pub fn summary_path(raw: Option<&str>) -> Option<PathBuf> {
    raw.map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

/// Prove the summary can reach `path` before the run starts.
///
/// Creates and removes the very temp file the real write will use, so a missing
/// directory, one that cannot be written, or a path that is itself a directory
/// is reported in a second rather than after a run has been paid for. Nothing is
/// left behind and the target is untouched, so a summary from a previous run
/// stays readable until this one has a complete object to replace it with.
///
/// # Errors
///
/// Returns why the path cannot be written, naming it.
pub fn writable(path: &Path) -> Result<(), String> {
    if let Some(problem) = directory_problem(path) {
        return Err(format!(
            "can't write the run summary to {}: {problem}",
            path.display()
        ));
    }
    match atomic::create_temp(path) {
        Ok((probe, _)) => {
            let _ = fs::remove_file(&probe);
            Ok(())
        }
        Err(error) => Err(reason(path, &error)),
    }
}

/// Why `path` names a directory rather than a file, if it does.
///
/// Neither case survives to the write, and neither is caught by creating a temp
/// sibling: the sibling of a directory is an ordinary name that opens fine, and
/// only the rename at the end of the run would fail. Checking both here is what
/// moves the failure to before the run is paid for.
///
/// The trailing separator is the case a caller reaches by accident, from
/// `--summary-file "$OUTDIR/$NAME"` with `NAME` unset. `file_name` strips the
/// separator, so the path looks like an ordinary file to everything downstream
/// until `rename` refuses it.
fn directory_problem(path: &Path) -> Option<&'static str> {
    if path.is_dir() {
        return Some("it is a directory");
    }
    if path.as_os_str().as_encoded_bytes().last() == Some(&b'/') {
        return Some("it names a directory, not a file");
    }
    None
}

/// Write `summary` to `path` as one line of JSON.
///
/// Goes through a temp sibling and a rename, so a reader that opens the path
/// sees either nothing or one complete object - never the prefix of one still
/// being written. See `crate::atomic` for why the temp file is opened the way
/// it is.
///
/// # Errors
///
/// Returns why the path could not be written. The caller fails the run on it
/// rather than falling back to stdout, which would be no fallback at all: a
/// caller that asked for a file is not watching stdout for the JSON.
pub fn write_file(path: &Path, summary: &Value) -> Result<(), String> {
    // Trailing newline so the file reads like every other line-oriented artifact
    // a workflow collects, and so `read` in a shell loop terminates.
    let body = format!("{summary}\n");
    atomic::write(path, body.as_bytes()).map_err(|error| reason(path, &error))
}

fn reason(path: &Path, error: &io::Error) -> String {
    format!("can't write the run summary to {}: {error}", path.display())
}

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

/// Which credential a run authenticated with, in identifiers safe to publish.
///
/// Reported for the reason `tools` is: an audit should read a run's posture out
/// of its own output instead of trusting that the workflow passed the flags it
/// claims. Credential mode is the other half of that posture. It matters most
/// once `cost_usd` is on - the next question after what a run cost is whose
/// budget paid, and a job that quietly fell back to a personal key otherwise
/// produces a summary indistinguishable from one that used the intended service
/// account.
///
/// Identifiers only. The minted access token and the OIDC assertion must never
/// land here: a summary gets uploaded as a build artifact, and artifacts carry
/// no masking, so a value redacted in a log is plain text there.
///
/// One enum rather than a mode string beside four optional ids, for the reason
/// [`crate::config::Protocol`] gives for folding auth into itself: the two are
/// not independent. Only federation has identifiers, so only that variant
/// carries them, and a static key with an organization id is a state nothing has
/// to test for because nothing can build it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunAuth<'a> {
    /// A static key out of the environment, whichever header carries it.
    ApiKey,
    /// A bearer token minted elsewhere and handed to afi.
    OAuth,
    /// No credential was configured at all - a local server that wants none.
    /// Distinct from `auth: null`, which is afi declining to attribute the run.
    NoCredential,
    /// A bearer token afi minted itself, through the workload-identity
    /// federation exchange. The only mode with identifiers of its own.
    Federated {
        organization_id: &'a str,
        service_account_id: &'a str,
        /// Only present when the federation rule spans workspaces.
        workspace_id: Option<&'a str>,
        federation_rule_id: &'a str,
    },
}

impl RunAuth<'_> {
    /// The `mode` the summary reports, naming how the credential was obtained.
    #[must_use]
    pub fn mode(&self) -> &'static str {
        match self {
            Self::ApiKey => "api_key",
            Self::OAuth => "oauth",
            Self::NoCredential => "none",
            Self::Federated { .. } => "federated",
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
            // refused before it resolved one.
            auth: None,
        }
    }

    /// Render as JSON.
    ///
    /// `usage` is null rather than a zeroed object when no request reported any,
    /// so a consumer can tell "the provider sent no usage" from "the run used no
    /// tokens" instead of silently charting zeros. A refusal keeps the object
    /// anyway - see `usage_json`.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
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
            "auth": self.auth_json(),
        })
    }

    /// The `auth` block: the mode, plus the identifiers federation carries.
    ///
    /// Only that mode has any, so only that arm adds them. An id the credential
    /// does not carry is left out rather than emitted blank, so
    /// `auth.workspace_id` is either a workspace or nothing - an empty string
    /// would read as one afi failed to capture.
    fn auth_json(&self) -> Value {
        let Some(auth) = self.auth else {
            return Value::Null;
        };
        let mut block = json!({ "mode": auth.mode() });
        if let RunAuth::Federated {
            organization_id,
            service_account_id,
            workspace_id,
            federation_rule_id,
        } = auth
            && let Some(fields) = block.as_object_mut()
        {
            fields.insert("organization_id".to_string(), organization_id.into());
            fields.insert("service_account_id".to_string(), service_account_id.into());
            fields.insert("federation_rule_id".to_string(), federation_rule_id.into());
            if let Some(workspace_id) = workspace_id {
                fields.insert("workspace_id".to_string(), workspace_id.into());
            }
        }
        block
    }

    fn usage_json(&self) -> Value {
        // A refused call is afi's own observation, so it survives a provider that
        // reported no tokens at all: dropping it would hide the one thing this
        // field exists to report. `requests` is still 0 there, which is how a
        // consumer tells the silent provider apart from a run that used nothing.
        if self.usage.is_empty() && self.refused_tool_calls.is_empty() {
            return Value::Null;
        }
        let mut usage = json!({
            "input_tokens": self.usage.input_tokens,
            "output_tokens": self.usage.output_tokens,
            "cache_read_tokens": self.usage.cache_read_tokens,
            "cache_write_tokens": self.usage.cache_write_tokens,
            "reasoning_tokens": self.usage.reasoning_tokens,
            "total_tokens": self.usage.total_tokens(),
            "requests": self.usage.requests,
            "refused_tool_calls": self.refused_tool_calls.total(),
            "refused_by_policy": self.refused_tool_calls.by_policy,
            "refused_by_approval": self.refused_tool_calls.by_approval,
        });
        // Inserted rather than declared in the object above, because an unpriced
        // run must have no key here at all: a null would read as "the run was
        // free" to anything summing the field.
        if let (Some(cost), Some(fields)) = (self.cost_usd, usage.as_object_mut())
            && let Some(number) = Number::from_f64(cost)
        {
            fields.insert("cost_usd".to_string(), Value::Number(number));
        }
        usage
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
