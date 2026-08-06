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
    #[must_use]
    pub fn parse(allowed: Option<&str>, denied: Option<&str>) -> Self {
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
