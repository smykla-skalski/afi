//! Which key in the file carries which environment variable.
//!
//! The file is a second spelling of the settings the `AFI_*` variables already
//! name, so every key lowers to one of those names and nothing downstream learns
//! a file was read. The rule is mechanical - a key is its variable minus the
//! `AFI_` prefix, lowercased - which is why this is a table rather than a
//! `match`: the pairing is data, the rule is checkable by reading one column
//! against the other, and a variable that has no row is a variable the file
//! cannot set, reported as an unknown key rather than accepted and dropped.
//!
//! Four names are absent on purpose. `AFI_ENV_FILE` is read before this file is
//! located, so a key naming it could not take effect. The legacy
//! `AFI_BASE_URL`, `AFI_MODEL`, and `AFI_API_KEY` trio is the flat spelling of
//! one source, and [`SOURCE`] is the structured one - a new file has no legacy
//! to keep. `ANTHROPIC_IDENTITY_TOKEN` is the identity token itself rather than
//! a name for one, and nothing that resolves `$NAME` indirection reads it, so a
//! file holding one would hold the secret. `AFI_BUILD_*` are set by the build,
//! not by whoever runs it.

use crate::pricing::RATE_CLASSES;

use super::value::{
    Convert, allow_list, count, decimal, effort_level, flag, list, object, percent, prompt_mode,
    protocol_name, summary_format, text, whole, wide_count,
};

/// One key and the variable it carries.
pub(super) struct Setting {
    pub key: &'static str,
    /// The variable, or - in [`SOURCE`] - the part after `AFI_SOURCE_<NAME>_`.
    pub env: &'static str,
    pub convert: Convert,
}

/// Shorthand so a table row reads as one line.
const fn row(key: &'static str, env: &'static str, convert: Convert) -> Setting {
    Setting { key, env, convert }
}

/// The blocks at the root that carry structure rather than one value. Named
/// here so a misspelling of one is answered with the right suggestion.
pub(super) const BLOCKS: [&str; 3] = ["sources", "prices", "anthropic"];

/// The token classes a price entry may set, from the type that deserializes
/// them, so the file and `AFI_PRICES` cannot disagree about the set.
pub(super) const PRICE_CLASSES: [&str; 5] = RATE_CLASSES;

/// Root-level keys that hold one value.
pub(super) const TOP: &[Setting] = &[
    // Where a run starts and how hard it thinks.
    row("active", "AFI_ACTIVE", text),
    row("approval", "AFI_APPROVAL", text),
    row("effort", "AFI_EFFORT", effort_level),
    row("backend", "AFI_BACKEND", text),
    // Where afi keeps what it writes.
    row("home", "AFI_HOME", text),
    row("sessions_dir", "AFI_SESSIONS_DIR", text),
    // What the run may reach.
    row("read_only", "AFI_READ_ONLY", flag),
    row("allowed_tools", "AFI_ALLOWED_TOOLS", allow_list),
    row("disallowed_tools", "AFI_DISALLOWED_TOOLS", list),
    // What it is told to do, before the conversation starts.
    row("system_prompt_file", "AFI_SYSTEM_PROMPT_FILE", text),
    row("system_prompt_mode", "AFI_SYSTEM_PROMPT_MODE", prompt_mode),
    // How it reports itself.
    row("summary", "AFI_SUMMARY", summary_format),
    row("summary_file", "AFI_SUMMARY_FILE", text),
    // The one key whose variable is not its own name uppercased. It lives here
    // rather than beside the blocks because it carries one value like the rest of
    // this table - the `env` column is what the exception needs.
    row("source_order", "AFI_SOURCES", list),
    // Sizes and caps.
    row("max_tokens", "AFI_MAX_TOKENS", count),
    row(
        "forced_final_max_tokens",
        "AFI_FORCED_FINAL_MAX_TOKENS",
        count,
    ),
    row("autocompress_percent", "AFI_AUTOCOMPRESS_PERCENT", percent),
    row("read_file_lines", "AFI_READ_FILE_LINES", whole),
    row("tool_result_chars", "AFI_TOOL_RESULT_CHARS", wide_count),
    row("max_model_turns", "AFI_MAX_MODEL_TURNS", count),
    row("bash_poll_seconds", "AFI_BASH_POLL_SECONDS", whole),
    row("session_desc_refresh", "AFI_SESSION_DESC_REFRESH", count),
    // When to give up on a turn.
    row(
        "reasoning_only_chars",
        "AFI_REASONING_ONLY_CHARS",
        wide_count,
    ),
    row("reasoning_only_time", "AFI_REASONING_ONLY_TIME", wide_count),
    row(
        "reasoning_only_retries",
        "AFI_REASONING_ONLY_RETRIES",
        count,
    ),
    row(
        "malformed_stream_retries",
        "AFI_MALFORMED_STREAM_RETRIES",
        count,
    ),
    row("empty_turn_retries", "AFI_EMPTY_TURN_RETRIES", count),
    // Recovery samplers, which llama.cpp takes and vLLM does not.
    row("recovery_temperature", "AFI_RECOVERY_TEMPERATURE", decimal),
    row("recovery_top_p", "AFI_RECOVERY_TOP_P", decimal),
    row("recovery_min_p", "AFI_RECOVERY_MIN_P", decimal),
    row(
        "recovery_repeat_penalty",
        "AFI_RECOVERY_REPEAT_PENALTY",
        decimal,
    ),
    row(
        "recovery_repeat_last_n",
        "AFI_RECOVERY_REPEAT_LAST_N",
        whole,
    ),
    row(
        "recovery_dry_multiplier",
        "AFI_RECOVERY_DRY_MULTIPLIER",
        decimal,
    ),
    row("recovery_dry_base", "AFI_RECOVERY_DRY_BASE", decimal),
    row(
        "recovery_dry_allowed_length",
        "AFI_RECOVERY_DRY_ALLOWED_LENGTH",
        whole,
    ),
    // Credentials that register a built-in source by existing.
    row("together_api_key", "AFI_TOGETHER_API_KEY", text),
    row("openrouter_api_key", "AFI_OPENROUTER_API_KEY", text),
];

/// A named source's fields. `env` is the part after `AFI_SOURCE_<NAME>_`.
pub(super) const SOURCE: &[Setting] = &[
    row("base_url", "BASE_URL", text),
    row("api_key", "API_KEY", text),
    row("model", "MODEL", text),
    row("protocol", "PROTOCOL", protocol_name),
    row("app_name", "APP_NAME", text),
    row("app_url", "APP_URL", text),
    row("extra_body", "EXTRA_BODY", object),
];

/// The built-in `anthropic` source's overrides. Outside the `AFI_SOURCE_*`
/// namespace in the file for the same reason they are outside it in the
/// environment - that namespace belongs to sources you define yourself.
pub(super) const ANTHROPIC: &[Setting] = &[
    row("api_key", "AFI_ANTHROPIC_API_KEY", text),
    row("oauth_token", "AFI_ANTHROPIC_OAUTH_TOKEN", text),
    row("base_url", "AFI_ANTHROPIC_BASE_URL", text),
    row("model", "AFI_ANTHROPIC_MODEL", text),
    row("extra_body", "AFI_ANTHROPIC_EXTRA_BODY", object),
];

/// The workload-identity-federation ids, under `anthropic.federation`.
///
/// These keep the un-prefixed variable names the official SDKs use, so a
/// workspace already configured for them needs no second spelling. All five are
/// non-secret: four ids and a path.
pub(super) const FEDERATION: &[Setting] = &[
    row("rule_id", "ANTHROPIC_FEDERATION_RULE_ID", text),
    row("organization_id", "ANTHROPIC_ORGANIZATION_ID", text),
    row("service_account_id", "ANTHROPIC_SERVICE_ACCOUNT_ID", text),
    row("workspace_id", "ANTHROPIC_WORKSPACE_ID", text),
    row("identity_token_file", "ANTHROPIC_IDENTITY_TOKEN_FILE", text),
];

/// The row for `key`, or `None` when the table has none.
pub(super) fn find(table: &'static [Setting], key: &str) -> Option<&'static Setting> {
    table.iter().find(|s| s.key == key)
}

/// A table's keys plus the blocks nested beside them, for the suggestion an
/// unknown key gets. `extra` is empty for a table that nests nothing.
pub(super) fn keys(table: &'static [Setting], extra: &[&'static str]) -> Vec<&'static str> {
    table
        .iter()
        .map(|s| s.key)
        .chain(extra.iter().copied())
        .collect()
}
