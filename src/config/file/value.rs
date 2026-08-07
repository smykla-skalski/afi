//! One JSON value into the string its environment variable holds.
//!
//! Each function carries a whole key's contract: the shapes it accepts and the
//! sentence it produces when given something else. They sit in the
//! [`super::schema`] tables as function pointers rather than behind a `match` on
//! some kind enum, so a key's type and the message it refuses with cannot drift
//! apart, and adding a type is adding a function rather than editing a
//! dispatcher.
//!
//! Every message completes the sentence "<key> ...", so a caller supplies the
//! path and nothing else.

use serde_json::Value;

use crate::summary::SummaryFormat;

use super::super::effort::{self, Effort};
use super::super::protocol::Protocol;
use super::super::system_prompt::PromptMode;

/// A key's value into the string its variable holds, or why it cannot be one.
pub(super) type Convert = fn(&Value) -> Result<String, String>;

/// Any string, kept exactly as written.
pub(super) fn text(value: &Value) -> Result<String, String> {
    value
        .as_str()
        .map(String::from)
        .ok_or_else(|| expected("a string", value))
}

/// `true` or `false`, written as the `1` or `0` the truthy readers take.
///
/// `false` is written rather than dropped. A project file turning off what the
/// user file turned on has to say something the reader will act on, and an
/// absent key says nothing at all.
pub(super) fn flag(value: &Value) -> Result<String, String> {
    match value.as_bool() {
        Some(true) => Ok("1".to_string()),
        Some(false) => Ok("0".to_string()),
        None => Err(expected("true or false", value)),
    }
}

/// A whole number, zero or more, that fits the `u32` its reader parses.
///
/// `16000.0` is refused rather than rounded, and so is anything past
/// `u32::MAX`. Every reader of these parses an integer of that width and falls
/// back to its default on anything else, so a value that does not fit would be
/// accepted here and then silently replaced by the default - the drop this file
/// exists to end, reached by one extra zero.
pub(super) fn count(value: &Value) -> Result<String, String> {
    value
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .map(|n| n.to_string())
        .ok_or_else(|| expected(&format!("a whole number from 0 to {}", u32::MAX), value))
}

/// A whole number, negative allowed, that fits the `i64` its reader parses.
///
/// Separate from [`count`] because these readers are signed and the sign means
/// something: `-1` is llama.cpp's "the whole context" for `repeat_last_n`, and
/// refusing it here would make the file unable to say what the variable can.
pub(super) fn whole(value: &Value) -> Result<String, String> {
    value
        .as_i64()
        .map(|n| n.to_string())
        .ok_or_else(|| expected("a whole number", value))
}

/// A number, whole or fractional, in the form it was written.
pub(super) fn decimal(value: &Value) -> Result<String, String> {
    match value {
        Value::Number(n) => Ok(n.to_string()),
        other => Err(expected("a number", other)),
    }
}

/// Names, as a list or as one comma-separated string of them.
///
/// The list is what JSON has for a list. The string is what the variable holds,
/// kept so a value can move from a shell export into the file unchanged.
pub(super) fn list(value: &Value) -> Result<String, String> {
    if let Some(one) = value.as_str() {
        return Ok(one.to_string());
    }
    let Some(items) = value.as_array() else {
        return Err(expected("a list of names, or one string of them", value));
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Some(name) = item.as_str() else {
            return Err(expected("a list of names", item));
        };
        out.push(name);
    }
    Ok(out.join(","))
}

/// An allow list, which may not be empty.
///
/// `[]` reads as "every tool" by the time it reaches the policy, because a blank
/// list is how an unset shell variable arrives and that cannot be a lockout. In
/// a file it is unambiguous and means the opposite of what it would do, so it is
/// refused rather than granted. `disallowed_tools` keeps [`list`]: an empty deny
/// list denies nothing, which is what it looks like it means.
pub(super) fn allow_list(value: &Value) -> Result<String, String> {
    let names = list(value)?;
    if names.trim().is_empty() {
        return Err(
            "must name at least one tool; an empty list would permit every tool, \
             which is what leaving the key out does - use read_only or \
             disallowed_tools to take tools away"
                .to_string(),
        );
    }
    Ok(names)
}

/// A JSON object, written back as the compact JSON its variable holds.
///
/// This is the shape the file exists for. `AFI_ANTHROPIC_EXTRA_BODY` is a JSON
/// object squeezed onto one line of shell, quoted so that the shell leaves it
/// alone; here it is an object, and afi does the squeezing.
pub(super) fn object(value: &Value) -> Result<String, String> {
    if !value.is_object() {
        return Err(expected("a JSON object", value));
    }
    serde_json::to_string(value).map_err(|e| format!("cannot be written back as JSON ({e})"))
}

/// One reasoning-effort level.
pub(super) fn effort_level(value: &Value) -> Result<String, String> {
    accepted(value, &effort::LEVELS, |raw| {
        Effort::parse(raw).map(Effort::as_str)
    })
}

/// `json` for the run summary on stdout, `none` to leave it off.
pub(super) fn summary_format(value: &Value) -> Result<String, String> {
    accepted(value, &SummaryFormat::NAMES, |raw| {
        SummaryFormat::parse(raw).map(|_| raw)
    })
}

/// How a supplied system prompt combines with afi's own.
pub(super) fn prompt_mode(value: &Value) -> Result<String, String> {
    accepted(value, &PromptMode::NAMES, |raw| {
        PromptMode::from_value(raw).map(PromptMode::as_str)
    })
}

/// A source's wire protocol.
pub(super) fn protocol_name(value: &Value) -> Result<String, String> {
    accepted(value, &Protocol::NAMES, |raw| {
        Protocol::NAMES
            .into_iter()
            .find(|name| name.eq_ignore_ascii_case(raw))
    })
}

/// A string its own reader accepts, refused here when the reader would not take
/// it.
///
/// `parse` is the reader's, so the file cannot drift into accepting a value the
/// variable rejects or refusing one it takes; `allowed` is only for the message.
/// Both are needed because a reader that cannot fail - `SummaryFormat` maps a
/// typo to "off" on purpose, so a run's output survives one - still has to be
/// asked, and only the file turns its answer into a refusal.
///
/// The message quotes what it was given, where [`expected`] deliberately does
/// not: no member of a closed set is a credential, and a typo is hard to fix
/// without seeing which one it was.
fn accepted<'v>(
    value: &'v Value,
    allowed: &[&str],
    parse: impl Fn(&'v str) -> Option<&'v str>,
) -> Result<String, String> {
    let raw = value.as_str().ok_or_else(|| expected("a string", value))?;
    parse(raw.trim()).map(String::from).ok_or_else(|| {
        format!(
            "must be one of {} (got {:?})",
            allowed.join(", "),
            raw.trim()
        )
    })
}

/// "must be <want> (got <what the file has>)".
///
/// The tail names the JSON type rather than quoting the value: a message about
/// a key that may hold a credential must not print what was there.
pub(super) fn expected(want: &str, value: &Value) -> String {
    format!("must be {want} (got {})", super::super::type_name(value))
}
