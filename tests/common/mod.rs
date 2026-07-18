//! Shared test helpers: build a `Runtime` from an explicit env + args, with
//! no leakage from the real `~/.env` or shell env.

use std::collections::HashMap;
use std::path::Path;

use afi::Runtime;

/// Build a runtime with a clean env (only the vars you pass in).
///
/// `args` is argv including argv[0] (typically `"minion"`). `env` is the
/// starting env; no `MINION_*` vars leak from the shell. `env_file` is
/// optional - pass `Some(path)` to exercise the `~/.env` loader.
#[allow(dead_code)]
pub fn build(args: &[&str], env: &[(&str, &str)]) -> Runtime {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let env_map: HashMap<String, String> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Runtime::build(&args, env_map, None)
}

/// Build a runtime with an env file (the `~/.env` path).
#[allow(dead_code)]
pub fn build_with_env_file(
    args: &[&str],
    env: &[(&str, &str)],
    env_file: Option<&Path>,
) -> Runtime {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let env_map: HashMap<String, String> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Runtime::build(&args, env_map, env_file)
}
