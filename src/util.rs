//! Small env-var helpers (`_env_int` / `_env_float` in the Python original),
//! plus the hex encoding two digest call sites share.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::hash::BuildHasher;

use chrono::Local;

/// A trimmed value, or `None` when it says nothing.
///
/// The rule every `AFI_*` variable that names a thing reads by: a blank value is
/// no value, because that is what an exported-but-unset shell variable looks
/// like, and a workflow turns a setting off for one job by leaving its variable
/// empty. The matching flags are stricter - see `config::args::set_required_value`.
#[must_use]
pub fn nonblank(raw: Option<&str>) -> Option<&str> {
    raw.map(str::trim).filter(|value| !value.is_empty())
}

/// Whether a value is an explicit "off".
///
/// The words afi accepts for it, in one place. Three settings read them - the
/// read-only posture, the config file's either-on merge, and the price refresh -
/// and they differ only in what an *absent* value means, which is each caller's
/// own business. Spelling the list three times meant adding a word was three
/// edits, and the first two disagreeing was nothing's fault in particular.
#[must_use]
pub fn is_off(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "no" | "off"
    )
}

/// Current Unix time as fractional seconds. Computed with an integer split and
/// `f64::from(u32)` so there is no lossy `i64 -> f64` cast; exact for every
/// timestamp before year 2106 (`u32` seconds).
#[must_use]
pub(crate) fn now_secs_f64() -> f64 {
    let millis = Local::now().timestamp_millis();
    let secs = u32::try_from(millis / 1000).unwrap_or(0);
    let sub_ms = u32::try_from(millis.rem_euclid(1000)).unwrap_or(0);
    f64::from(secs) + f64::from(sub_ms) / 1000.0
}

#[must_use]
pub fn env_int<S: BuildHasher>(env: &HashMap<String, String, S>, name: &str, default: i64) -> i64 {
    match env.get(name).and_then(|v| v.parse::<i64>().ok()) {
        Some(n) => n,
        None => default,
    }
}

#[must_use]
pub fn env_float<S: BuildHasher>(
    env: &HashMap<String, String, S>,
    name: &str,
    default: f64,
) -> f64 {
    match env.get(name).and_then(|v| v.parse::<f64>().ok()) {
        Some(n) => n,
        None => default,
    }
}

/// Lowercase hex, the encoding both digest call sites need - the `--version`
/// self-digest and `SigV4`'s payload hash and signature.
#[must_use]
pub(crate) fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // Infallible: a String write cannot fail.
        let _ = write!(out, "{byte:02x}");
    }
    out
}
