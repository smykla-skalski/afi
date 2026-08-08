//! Resolving the context window a run measures its auto-compress threshold
//! against.
//!
//! Beside [`super::effort`] and [`super::tools`] rather than inside `runtime`,
//! for the same reason those are: one setting, gathered from a flag and the
//! environment, resolved once at startup. `Runtime` holds what a session *is*;
//! this decides what one number *starts as*.

use std::collections::HashMap;

use crate::model::context_window::context_window_for;
use crate::summary::{ErrorKind, RunError};

/// The window `--context-window` named, recording a refusal when it named
/// something unreadable.
///
/// Strict where the variables are lenient, because a flag has nowhere to fall
/// through to: it was typed for this run, and a run that shrugged it off would
/// measure its threshold against a different number than the command line asked
/// for. `Input`, like every other flag naming something this run cannot use.
pub(super) fn from_flag(raw: Option<&str>, flag_errors: &mut Vec<RunError>) -> Option<u64> {
    let raw = raw?;
    let window = raw.trim().parse::<u64>().ok();
    if window.is_none() {
        flag_errors.push(RunError::new(
            format!("--context-window wants a whole number of tokens, got {raw:?}"),
            ErrorKind::Input,
        ));
    }
    window
}

/// How much context `model` on source `name` holds. `None` when nothing knows.
///
/// Most specific first: the `--context-window` flag, then the source's own
/// variable, then the built-in's own namespace - `AFI_ANTHROPIC_*` and
/// `AFI_BEDROCK_*`, the same spelling those two take their `MODEL` and `BASE_URL`
/// from - then the run-wide `AFI_CONTEXT_WINDOW`, then the compiled table.
///
/// A config file is not a step of its own: it sets these variables rather than
/// competing with them, and `FileSettings::apply_to` only fills gaps, so "the
/// variable beats the file" needs no rule here.
///
/// `Some(0)` is a real answer and means folding is off for this source. It is
/// distinct from `None`, which means nobody knows the window - the difference is
/// what decides whether the run says anything about it.
pub(super) fn resolve(
    env: &HashMap<String, String>,
    flag: Option<u64>,
    name: &str,
    model: &str,
) -> Option<u64> {
    if flag.is_some() {
        return flag;
    }
    let upper = name.to_uppercase();
    [
        format!("AFI_SOURCE_{upper}_CONTEXT_WINDOW"),
        format!("AFI_{upper}_CONTEXT_WINDOW"),
        "AFI_CONTEXT_WINDOW".to_string(),
    ]
    .iter()
    .find_map(|key| declared(env, key))
    .or_else(|| context_window_for(model))
}

/// A window declared under `key`, or `None` when it is unset, blank, or not a
/// whole number.
///
/// An unreadable value falls through to the next spelling rather than refusing
/// the run, which is how every other `AFI_*` number behaves - see
/// `ModelConfig::from_env`.
fn declared(env: &HashMap<String, String>, key: &str) -> Option<u64> {
    env.get(key)
        .map(|raw| raw.trim())
        .filter(|raw| !raw.is_empty())?
        .parse()
        .ok()
}
