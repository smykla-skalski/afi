//! Context compression: fold older turns into a summary, keep the last N
//! verbatim. Manual `/compress` keeps 2; auto-compress keeps ~⅓.
//!
//! The fold is in two halves - [`plan`] works out what would change and what to
//! ask the model, [`CompressionPlan::apply`] performs it - because the request
//! for the summary is asynchronous and the surgery around it is not. [`compress`]
//! is the synchronous pairing of the two, for a caller that already has the
//! summary or can fetch one without awaiting; [`auto`] drives the same pair
//! around a live request.

use serde_json::Value;

mod auto;
mod plan;
pub(crate) use auto::completion_content;
pub use auto::{AutoCompress, maybe_autocompress, run_autocompress};
pub use plan::{CompressionPlan, plan_compression};

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
/// single user-role summary turn. Returns `Some(CompressResult)` on success, or
/// `None` on failure (in which case `messages` is untouched).
///
/// When `auto` is set the keep count is raised above `COMPRESS_KEEP`, so
/// auto-compression is more conservative than a manual `/compress` - it keeps
/// roughly the last third of the conversation verbatim so in-progress work and
/// recent tool results survive the fold.
///
/// `summarize` takes the summary prompt and returns the model's summary text (or
/// `None` on failure). That closure is what abstracts the HTTP call, which is why
/// this is testable without a live server.
pub fn compress<F>(
    messages: &mut Vec<Value>,
    keep: usize,
    auto: bool,
    summarize: F,
) -> Option<CompressResult>
where
    F: FnOnce(&str) -> Option<String>,
{
    let plan = plan_compression(messages, keep, auto)?;
    let summary = summarize(plan.prompt())?;
    plan.apply(messages, &summary)
}

#[cfg(test)]
mod tests;
