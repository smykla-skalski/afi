//! Turning the tool-policy flags into the env values the policy is read from.
//!
//! The env map is where the policy lives because `ModelConfig::from_env` is
//! built in four places from a map alone, and all four have to agree on what the
//! run may call. Note this map is afi's own view, not the child process
//! environment: `run_detached` does not export it, so a `run_bash` child does not
//! inherit a flag-supplied policy.

use std::collections::HashMap;

use super::runtime::ParsedArgs;

/// Materialize the tool-policy flags into the env map, so a flag wins over its
/// variable the way every other setting does.
///
/// `--read-only` is the exception: it only ever turns the posture on. A wrapper
/// that passes it should not be defeated by an `AFI_READ_ONLY=0` further out in
/// the environment, and nothing a wrapped command appends can undo it.
pub(super) fn apply_tool_flags(env: &mut HashMap<String, String>, parsed: &ParsedArgs) {
    for (flag, var) in [
        (&parsed.allowed_tools, "AFI_ALLOWED_TOOLS"),
        (&parsed.disallowed_tools, "AFI_DISALLOWED_TOOLS"),
    ] {
        if let Some(value) = flag {
            env.insert(var.to_string(), value.clone());
        }
    }
    if parsed.read_only {
        env.insert("AFI_READ_ONLY".to_string(), "1".to_string());
    }
}
