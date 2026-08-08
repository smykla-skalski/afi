//! Why a run must not start, gathered in the order it is reported.
//!
//! Every case here is a setting whose quiet fallback leaves a finished run that
//! differs from the one the command line asked for, with nothing downstream to
//! notice: a wider tool grant than was asked for, an effort nobody chose, afi's
//! own prompt in place of the instructions the run was handed, or a config file
//! whose settings are all absent.
//!
//! Split out of `runtime` because the list grows with every setting that can be
//! given wrongly, while `Runtime` itself does not. Keeping them together put an
//! ever-lengthening `match`-by-another-name in the middle of the struct that
//! holds the session.

use crate::summary::{ErrorKind, RunError, writable};

use super::Source;
use super::runtime::Runtime;

/// Why this run must not start, if it must not.
///
/// The summary-file case is checked here, by touching the path, rather than
/// left to the write at the end of the run: a caller that asked for a file is
/// not watching stdout for the JSON, and a run that has already been paid for is
/// a poor moment to learn the directory does not exist.
///
/// Each refusal carries the kind the summary reports, decided here where the
/// reason is known. Deriving it afterwards from which field was non-empty would
/// put the caller's classification back at one remove from the thing being
/// classified, which is what `error_kind` exists to end.
pub(super) fn of(rt: &Runtime) -> Vec<RunError> {
    let mut out = rt.flag_errors.clone();
    out.extend(
        rt.tool_policy
            .unknown_names_message()
            .map(|m| RunError::new(m, ErrorKind::Policy)),
    );
    out.extend(
        rt.summary_file
            .as_deref()
            .and_then(|p| writable(p).err())
            .map(|m| RunError::new(m, ErrorKind::Input)),
    );
    // `Input`: the invocation named a prompt this run cannot use, and retrying
    // it lands in the same place.
    out.extend(
        rt.system_prompt
            .as_ref()
            .err()
            .map(|m| RunError::new(m.clone(), ErrorKind::Input)),
    );
    // `Auth`: the source the run starts on cannot assemble a credential, and no
    // retry assembles one either.
    //
    // Only the *active* source is checked. A half-configured source nobody
    // switches to costs the run nothing, and refusing to start over one would
    // make an unused `AWS_ACCESS_KEY_ID` in the shell enough to block every run.
    out.extend(
        rt.active_source()
            .and_then(Source::config_error)
            .map(|m| RunError::new(m, ErrorKind::Auth)),
    );
    // Last, after the credential check: a source that cannot assemble one spends
    // nothing, so complaining about its unpriced model first would be
    // second-order.
    out.extend(unenforceable_budget(rt).map(|m| RunError::new(m, ErrorKind::Input)));
    out
}

/// Why a budget this run was given could never fire, if it could not.
///
/// afi caps what a run spends by pricing what it used, so a cap it cannot
/// measure is not a cap. Every other setting that fails this way degrades into
/// something plausible; this one degrades into a run carrying a cap it will
/// never enforce, spending real money while the invocation says it is capped.
///
/// Only the *active* model is checked, for the same reason only the active
/// source is: a `/source` switch afterwards is an interactive path with a human
/// present, and the turn loop stops that run at its next checkpoint.
fn unenforceable_budget(rt: &Runtime) -> Option<String> {
    let budget = rt.budget?;
    let model = rt.model.as_deref()?;
    let named = budget.named();
    let Some(pricing) = rt.pricing.as_ref() else {
        return Some(format!(
            "{named} cannot be enforced: no rate table could be read, and afi caps what \
             a run spends by pricing what it used - fix AFI_PRICES, or drop the budget"
        ));
    };
    let why = pricing.unpriceable(rt.active_source().and_then(Source::price_provider), model)?;
    Some(format!(
        "{named} cannot be enforced: {why} - afi caps what a run spends by pricing what \
         it used, so price it in AFI_PRICES or drop the budget"
    ))
}
