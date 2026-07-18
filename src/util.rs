//! Small env-var helpers (`_env_int` / `_env_float` in the Python original).

use std::collections::HashMap;
use std::hash::BuildHasher;

use chrono::Local;

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
