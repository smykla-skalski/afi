//! Small env-var helpers (`_env_int` / `_env_float` in the Python original).

use std::collections::HashMap;

pub fn env_int(env: &HashMap<String, String>, name: &str, default: i64) -> i64 {
    match env.get(name).and_then(|v| v.parse::<i64>().ok()) {
        Some(n) => n,
        None => default,
    }
}

pub fn env_float(env: &HashMap<String, String>, name: &str, default: f64) -> f64 {
    match env.get(name).and_then(|v| v.parse::<f64>().ok()) {
        Some(n) => n,
        None => default,
    }
}
