//! Machine-readable run summary, for CI that needs the result rather than the
//! rendered transcript.
//!
//! A review run in a workflow needs two things out of `afi`: the text the model
//! finally produced, so it can be posted, and the run's real token accounting,
//! so it can be reported. Both are printed as one JSON object on stdout after
//! the run, leaving the workflow to decide what to do with it.
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
    /// What the run cost, when the caller configured rates for every model and
    /// token class it used. `None` leaves the field out entirely rather than
    /// reporting a zero a consumer would chart as free.
    pub cost_usd: Option<f64>,
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
        let mut usage = json!({
            "input_tokens": self.usage.input_tokens,
            "output_tokens": self.usage.output_tokens,
            "cache_read_tokens": self.usage.cache_read_tokens,
            "cache_write_tokens": self.usage.cache_write_tokens,
            "reasoning_tokens": self.usage.reasoning_tokens,
            "total_tokens": self.usage.total_tokens(),
            "requests": self.usage.requests,
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
