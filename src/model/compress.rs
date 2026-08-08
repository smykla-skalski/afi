//! Context compression: fold older turns into a summary, keep the last N
//! verbatim. Manual `/compress` keeps 2; auto-compress keeps ~⅓.
//!
//! The fold is in two halves - [`plan`] works out what would change and what to
//! ask the model, [`CompressionPlan::apply`] performs it - because the request
//! for the summary is asynchronous and the surgery around it is not. [`auto`]
//! drives the pair around a live request, which is the only thing that does.

mod auto;
mod plan;
mod summary;
pub(crate) use auto::{AutoCompress, Fold, fold_after_turn};
pub(crate) use plan::plan_compression;
pub(crate) use summary::{Summary, fetch};

/// How many recent turns to leave untouched in a manual `/compress`.
pub const COMPRESS_KEEP: usize = 2;

/// The result of a successful compression: `(kept_n, summarized_n, summary_chars)`.
pub struct CompressResult {
    pub kept_n: usize,
    pub summarized_n: usize,
    pub summary_chars: usize,
}

#[cfg(test)]
mod tests;
