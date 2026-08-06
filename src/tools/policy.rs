//! Which tools a run may call at all.
//!
//! CI is the reason this exists. Approval alone cannot express "read but do not
//! write": unattended, anything above the threshold is denied with nobody to
//! answer the prompt, so a job reaches for `--yolo`, and `--yolo` hands over
//! `write_file`, `edit_file`, and `run_bash` in full. That is fine for a human
//! at a prompt and wrong for a job whose input is an untrusted pull-request
//! diff. These two lists bound the run's reach independently of approval.
//!
//! An absent or blank allow list means every registered tool; a non-empty one
//! is exhaustive. Deny always wins, so
//! `--allowed-tools read_file,run_bash --disallowed-tools run_bash` leaves only
//! `read_file`.
//!
//! Enforced twice, on purpose. [`ToolPolicy::filter_tools`] drops blocked
//! schemas from the request so the model never learns the tool exists, which is
//! what actually stops it trying. [`ToolPolicy::permits`] then gates dispatch,
//! because the text protocol parses calls out of prose and a model can name a
//! tool it was never offered.

use std::collections::BTreeSet;

use serde_json::Value;

use super::known_tool_names;

/// The tools the approval gate confirms before dispatch: the two writers, and
/// the shell, which can do anything at all.
///
/// One list, so the gate and [`read_only_denied`] cannot drift. A second
/// hard-coded copy in either place would eventually disagree with this one, and
/// the way it would disagree is by leaving a mutating tool ungated.
pub const MUTATING_TOOLS: [&str; 3] = ["write_file", "edit_file", "run_bash"];

/// Denied by the read-only posture on top of [`MUTATING_TOOLS`], and by nothing
/// else.
///
/// `wait_background` unlinks the log once the command has exited, so a posture
/// promising that nothing changes has to deny it - but the approval gate must
/// still not ask about it, because it only waits on a command whose own approval
/// was already settled. Two questions, two lists.
///
/// Nothing useful is lost. Read-only denies `run_bash`, so a read-only run
/// cannot start a background command, and the only logs left to wait on belong
/// to some other run.
const READ_ONLY_ONLY: [&str; 1] = ["wait_background"];

/// Every tool the read-only posture denies.
pub fn read_only_denied() -> impl Iterator<Item = &'static str> {
    MUTATING_TOOLS.iter().chain(READ_ONLY_ONLY.iter()).copied()
}

/// Whether `name` can change anything outside afi.
///
/// Unknown names answer `false`: the policy refuses to start on a name it does
/// not recognize, so nothing unregistered reaches dispatch to be classified.
#[must_use]
pub fn is_mutating(name: &str) -> bool {
    MUTATING_TOOLS.contains(&name)
}

/// `final_answer` is turn plumbing, not a capability: the forced-final path
/// offers it alone and reads the answer back out of the call. Blocking it would
/// break the run rather than restrict it, so no policy controls it.
const ALWAYS_PERMITTED: &str = "final_answer";

/// The tools a run may call. The default permits everything, so a run that sets
/// neither list behaves exactly as it did before this existed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolPolicy {
    /// `None` means "every registered tool". `Some` is exhaustive.
    allowed: Option<BTreeSet<String>>,
    denied: BTreeSet<String>,
    /// Names in either list matching no registered tool. Held rather than
    /// dropped: a mistyped deny entry would otherwise leave the tool quietly
    /// available, which is the one failure a security control must not have.
    unknown: Vec<String>,
}

impl ToolPolicy {
    /// Parse both lists. Accepts commas or whitespace as separators and is
    /// case-insensitive.
    ///
    /// A blank value counts as unset, matching how `Source::new` treats a blank
    /// credential - `AFI_ALLOWED_TOOLS=""` must not mean "no tools at all".
    ///
    /// Private, so [`Self::from_env`] is the only way to build a policy from
    /// outside. A caller reaching for the two lists alone silently drops the
    /// read-only posture, which is exactly the bug that shipped: the enforcing
    /// policy called this, the reported one called `from_env`, and `--read-only`
    /// went from a restriction to a banner line.
    #[must_use]
    fn parse(allowed: Option<&str>, denied: Option<&str>) -> Self {
        let mut unknown = Vec::new();
        let allowed = split_names(allowed, &mut unknown);
        let denied = split_names(denied, &mut unknown).unwrap_or_default();
        unknown.sort_unstable();
        unknown.dedup();
        Self {
            allowed,
            denied,
            unknown,
        }
    }

    /// Build from the three environment values that carry a policy.
    ///
    /// The env map is the carrier because `ModelConfig::from_env` is built in
    /// four places from a map alone, and all four have to agree on what the run
    /// may call. Reading the read-only decision here rather than at the call site
    /// keeps that decision in one place with the rest of the policy.
    #[must_use]
    pub fn from_env(allowed: Option<&str>, denied: Option<&str>, read_only: Option<&str>) -> Self {
        let policy = Self::parse(allowed, denied);
        if read_only_requested(read_only) {
            policy.read_only()
        } else {
            policy
        }
    }

    /// Deny everything that can change anything, whatever the allow list says.
    ///
    /// Expressed as a denial rather than an allow list of the readers, because
    /// deny wins: `--read-only --allowed-tools run_bash` still blocks
    /// `run_bash`. An allow list would have to argue with the user's own, and the
    /// posture that loses an argument is not a protection. It also spares the
    /// caller spelling tool names, which is where `--disallowed-tools run_bsah`
    /// came from.
    ///
    /// Nothing here touches approval. Every remaining tool is one the approval
    /// gate never asks about, so a read-only run needs no `--yolo` to go
    /// unattended - that pairing only ever granted writes nobody wanted.
    #[must_use]
    pub fn read_only(mut self) -> Self {
        self.denied.extend(read_only_denied().map(str::to_string));
        self
    }

    /// Whether this policy denies everything that can change anything, for the
    /// banner and the summary. True whether it came from `--read-only` or from a
    /// deny list that happens to name the same tools.
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        read_only_denied().all(|name| !self.permits(name))
    }

    /// Whether `name` may be dispatched.
    #[must_use]
    pub fn permits(&self, name: &str) -> bool {
        if name == ALWAYS_PERMITTED {
            return true;
        }
        // Fail closed on an unusable policy. A caller that skipped
        // `unknown_names` gets a run where nothing dispatches, which is loud.
        if !self.unknown.is_empty() {
            return false;
        }
        if self.denied.contains(name) {
            return false;
        }
        self.allowed.as_ref().is_none_or(|a| a.contains(name))
    }

    /// Whether every registered tool is permitted, letting callers skip the
    /// filtering work and the banner line entirely.
    #[must_use]
    pub fn is_unrestricted(&self) -> bool {
        self.allowed.is_none() && self.denied.is_empty() && self.unknown.is_empty()
    }

    /// Names in either list matching no registered tool. Non-empty means the
    /// policy cannot be honoured and the run must not start.
    #[must_use]
    pub fn unknown_names(&self) -> &[String] {
        &self.unknown
    }

    /// The unknown-name refusal as a caller-printable line, or nothing when the
    /// policy is usable.
    ///
    /// Names both sources because the policy cannot tell them apart - the flags
    /// are materialized into the env vars before parsing - and a CI failure where
    /// only the variables were set should not send the reader hunting for a flag.
    #[must_use]
    pub fn unknown_names_message(&self) -> Option<String> {
        if self.unknown.is_empty() {
            return None;
        }
        Some(format!(
            "unknown tool(s) in --allowed-tools/--disallowed-tools or \
             AFI_ALLOWED_TOOLS/AFI_DISALLOWED_TOOLS: {}",
            self.unknown.join(", ")
        ))
    }

    /// The permitted tools in registration order. `final_answer` is left out:
    /// it is never offered alongside the others, and listing plumbing under a
    /// capability heading reads as a capability.
    #[must_use]
    pub fn permitted(&self) -> Vec<&'static str> {
        known_tool_names()
            .iter()
            .copied()
            .filter(|name| self.permits(name))
            .collect()
    }

    /// One line for the banner and the run summary, so a log proves which
    /// restriction was in force. Renders the effective set rather than the two
    /// input lists, which spares the reader the precedence rule.
    #[must_use]
    pub fn describe(&self) -> String {
        if self.is_unrestricted() {
            return "all".to_string();
        }
        let permitted = self.permitted();
        if permitted.is_empty() {
            return "none".to_string();
        }
        permitted.join(",")
    }

    /// Drop the schemas of blocked tools so the model is never offered them.
    ///
    /// Returns an array. An empty one means every tool was blocked; the caller
    /// omits the `tools` key rather than sending `[]`, which not every endpoint
    /// accepts.
    #[must_use]
    pub fn filter_tools(&self, tools: &Value) -> Value {
        if self.is_unrestricted() {
            return tools.clone();
        }
        let Some(entries) = tools.as_array() else {
            return tools.clone();
        };
        Value::Array(
            entries
                .iter()
                .filter(|entry| schema_name(entry).is_none_or(|name| self.permits(name)))
                .cloned()
                .collect(),
        )
    }
}

/// The advertised name of one `OpenAI` tool schema entry. `None` for a
/// malformed entry, which the protocol layers already drop on their own - the
/// policy has no opinion on it.
fn schema_name(entry: &Value) -> Option<&str> {
    entry.pointer("/function/name").and_then(Value::as_str)
}

/// Whether an `AFI_READ_ONLY` value asks for the read-only posture.
///
/// Blank counts as unset, like every other value here, so an unset shell variable
/// expanded into the environment is not a silent lockout. Anything else present
/// and not an explicit off counts as on: a variable someone bothered to set
/// should not be ignored because they wrote `on` rather than `1`.
fn read_only_requested(raw: Option<&str>) -> bool {
    match raw.map(str::trim) {
        None | Some("") => false,
        Some(value) => !matches!(
            value.to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
    }
}

/// Split one list into canonical names, appending anything unregistered to
/// `unknown`. `None` when the value is absent or holds no names.
fn split_names(raw: Option<&str>, unknown: &mut Vec<String>) -> Option<BTreeSet<String>> {
    let names: BTreeSet<String> = raw?
        .split([',', ' ', '\t', '\n'])
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_lowercase)
        .collect();
    if names.is_empty() {
        return None;
    }
    unknown.extend(
        names
            .iter()
            .filter(|name| !known_tool_names().contains(&name.as_str()))
            .cloned(),
    );
    Some(names)
}

#[cfg(test)]
mod tests;
