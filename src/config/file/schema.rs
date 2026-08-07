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
//! Two columns beyond the pairing. [`Scope`] says whether a project file may set
//! the key, because a file in the working tree is written by whoever wrote the
//! repository rather than by whoever is running afi. [`REFUSED`] names the keys
//! no config file may set at all, with the reason each one gets told, so a
//! credential in a file is answered with where to put it instead of with "unknown
//! key".
//!
//! No credential has a row. A config file is a thing people commit, paste into an
//! issue, and copy between machines, and the one kind of value that must not
//! travel that way is the kind that authenticates. `AFI_SOURCE_<NAME>_API_KEY`
//! and the `ANTHROPIC_*` credentials stay in the environment or the env file,
//! which is where the tooling around secrets already looks.
//!
//! Three more names are absent for their own reasons. `AFI_ENV_FILE` is read
//! before this file is located, so a key naming it could not take effect. The
//! legacy `AFI_BASE_URL`, `AFI_MODEL`, and `AFI_API_KEY` trio is the flat
//! spelling of one source, and [`SOURCE`] is the structured one - a new file has
//! no legacy to keep. `AFI_BUILD_*` are set by the build, not by whoever runs
//! it.

use crate::pricing::RATE_CLASSES;

use super::value::{
    Convert, allow_list, count, decimal, effort_level, flag, list, object, percent, prompt_mode,
    protocol_name, summary_format, text, whole, wide_count,
};

/// Which files may set a key. The other half of the pair is
/// [`super::Origin`], which says which kind of file is being read.
///
/// A repository has a legitimate say in some of this - which model to use, how
/// hard to think, what a token costs - and none at all in where requests go,
/// whose instructions the model follows, or whether the operator is asked before
/// a tool runs. Those are the keys that would turn a checkout into a run nobody
/// chose, so they carry [`Scope::Operator`] and a file in the working tree cannot
/// reach them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Scope {
    /// Any config file, including one in the working tree.
    Anywhere,
    /// Only a file the operator keeps or named themselves.
    Operator,
}

/// How a key combines when two files both set it.
///
/// Replacing is the rule and the rest are the exceptions, each for its own
/// reason. The three that bound what a run may do combine so a project file
/// cannot widen them by replacing - see `super::FileSettings::merge`. The two
/// that carry an object combine key by key, because replacing drops what the
/// other file said about a key this one is silent on: a project file pricing one
/// model would take the rates for every other model with it, and `cost_usd` would
/// go quiet for them without a word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Merge {
    /// A later file's value stands in for an earlier one's.
    Replace,
    /// Objects combine key by key, the later file's winning per key.
    Object,
    /// Every name in either list, so a deny list only ever grows.
    Union,
    /// Only the names both lists carry, so an allow list only ever shrinks.
    Intersection,
    /// On as soon as either file asks for it.
    Either,
}

/// One key and the variable it carries.
pub(super) struct Setting {
    pub key: &'static str,
    /// The variable, or - in [`SOURCE`] - the part after `AFI_SOURCE_<NAME>_`.
    pub env: &'static str,
    pub convert: Convert,
    pub scope: Scope,
    pub merge: Merge,
}

/// Shorthand so a table row reads as one line. A key a project may set, whose
/// value a later file replaces.
const fn row(key: &'static str, env: &'static str, convert: Convert) -> Setting {
    Setting {
        key,
        env,
        convert,
        scope: Scope::Anywhere,
        merge: Merge::Replace,
    }
}

/// A key a project may set that combines rather than replaces. See [`Merge`].
const fn joins(key: &'static str, env: &'static str, convert: Convert, merge: Merge) -> Setting {
    Setting {
        key,
        env,
        convert,
        scope: Scope::Anywhere,
        merge,
    }
}

/// A key only the operator's own file may set, or one they named with
/// `--config`. See [`Scope`].
///
/// The tool policy is deliberately not among them. A repository saying "this
/// project is read-only", or naming fewer tools than the operator allowed, is a
/// thing it should be able to say - and it cannot say the opposite, because those
/// three combine rather than replace when two files set them. See
/// `FileSettings::merge`.
const fn mine(key: &'static str, env: &'static str, convert: Convert) -> Setting {
    Setting {
        key,
        env,
        convert,
        scope: Scope::Operator,
        merge: Merge::Replace,
    }
}

/// Keys no config file may set, and what to tell whoever wrote one.
///
/// Every entry is a credential. They are refused by name rather than reported as
/// unknown, because "unknown key" beside a key that used to work, or that the
/// matching variable still accepts, reads as a bug in afi rather than as a
/// decision about where secrets live.
pub(super) const REFUSED: [(&str, &str); 5] = [
    (
        "api_key",
        "a credential does not go in a config file - set AFI_SOURCE_<NAME>_API_KEY, \
         in the environment or in the env file",
    ),
    (
        "oauth_token",
        "a credential does not go in a config file - set AFI_ANTHROPIC_OAUTH_TOKEN, \
         in the environment or in the env file",
    ),
    (
        "identity_token_file",
        "the federation identity is a credential and does not go in a config file - \
         set ANTHROPIC_IDENTITY_TOKEN_FILE in the environment or in the env file",
    ),
    (
        "together_api_key",
        "a credential does not go in a config file - set AFI_TOGETHER_API_KEY in the \
         environment or in the env file",
    ),
    (
        "openrouter_api_key",
        "a credential does not go in a config file - set AFI_OPENROUTER_API_KEY in \
         the environment or in the env file",
    ),
];

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
    mine("approval", "AFI_APPROVAL", text),
    row("effort", "AFI_EFFORT", effort_level),
    row("backend", "AFI_BACKEND", text),
    // Where afi keeps what it writes.
    mine("home", "AFI_HOME", text),
    mine("sessions_dir", "AFI_SESSIONS_DIR", text),
    // What the run may reach.
    joins("read_only", "AFI_READ_ONLY", flag, Merge::Either),
    joins(
        "allowed_tools",
        "AFI_ALLOWED_TOOLS",
        allow_list,
        Merge::Intersection,
    ),
    joins(
        "disallowed_tools",
        "AFI_DISALLOWED_TOOLS",
        list,
        Merge::Union,
    ),
    // What it is told to do, before the conversation starts.
    mine("system_prompt_file", "AFI_SYSTEM_PROMPT_FILE", text),
    mine("system_prompt_mode", "AFI_SYSTEM_PROMPT_MODE", prompt_mode),
    // How it reports itself.
    row("summary", "AFI_SUMMARY", summary_format),
    mine("summary_file", "AFI_SUMMARY_FILE", text),
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
];

/// A named source's fields. `env` is the part after `AFI_SOURCE_<NAME>_`.
pub(super) const SOURCE: &[Setting] = &[
    mine("base_url", "BASE_URL", text),
    row("model", "MODEL", text),
    mine("protocol", "PROTOCOL", protocol_name),
    row("app_name", "APP_NAME", text),
    row("app_url", "APP_URL", text),
    joins("extra_body", "EXTRA_BODY", object, Merge::Object),
];

/// The built-in `anthropic` source's overrides. Outside the `AFI_SOURCE_*`
/// namespace in the file for the same reason they are outside it in the
/// environment - that namespace belongs to sources you define yourself.
pub(super) const ANTHROPIC: &[Setting] = &[
    mine("base_url", "AFI_ANTHROPIC_BASE_URL", text),
    row("model", "AFI_ANTHROPIC_MODEL", text),
    joins(
        "extra_body",
        "AFI_ANTHROPIC_EXTRA_BODY",
        object,
        Merge::Object,
    ),
];

/// The workload-identity-federation ids, under `anthropic.federation`.
///
/// These keep the un-prefixed variable names the official SDKs use, so a
/// workspace already configured for them needs no second spelling. The four ids
/// are non-secret, but they name whose credential gets exchanged, so a project
/// file does not set them. The identity token and the file holding it are the
/// credential itself and have no key at all - see [`REFUSED`].
pub(super) const FEDERATION: &[Setting] = &[
    mine("rule_id", "ANTHROPIC_FEDERATION_RULE_ID", text),
    mine("organization_id", "ANTHROPIC_ORGANIZATION_ID", text),
    mine("service_account_id", "ANTHROPIC_SERVICE_ACCOUNT_ID", text),
    mine("workspace_id", "ANTHROPIC_WORKSPACE_ID", text),
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
