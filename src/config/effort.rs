//! Reasoning effort: one named level, translated into whatever each source's
//! wire format calls it.
//!
//! Effort already reachable through `EXTRA_BODY` is provider-specific JSON -
//! `output_config` on Anthropic, `reasoning` on `OpenRouter` - so a caller who
//! changes source rewrites the same intent into another schema, and a typo
//! there is warned about and ignored, leaving a complete, plausible run at an
//! effort nobody asked for. `--effort` and `AFI_EFFORT` name the level once and
//! either take it or refuse to start.
//!
//! `EXTRA_BODY` wins wherever the two would meet, and the object it writes is
//! left exactly as written rather than merged into. It is the escape hatch, and
//! an escape hatch that loses to a flag is not one.
//!
//! Not every endpoint has the same ladder, so a level is capped at the highest
//! one its dialect defines and the difference is reported. Sending a level an
//! endpoint has never heard of would fail the turn outright, which is a worse
//! answer to "think harder" than thinking as hard as it can.

use serde_json::{Map, Value};

use super::{Runtime, Source};

/// The level names, in order, for the flag's error message.
const LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// How hard the model is asked to think, and how freely it may spend tokens
/// getting there.
///
/// The ladder is Anthropic's, because it is the widest one afi speaks to. The
/// other dialects take the same names in their own key up to their own ceiling
/// (see [`wire`]), so the levels are ordered: a source that cannot reach a level
/// carries the highest one it can.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Effort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl Effort {
    /// The wire spelling, which is the same in every dialect afi translates to.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }

    /// Parse one level name, case- and whitespace-insensitively. `None` for
    /// anything else - the caller turns that into a refusal rather than a
    /// fallback.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::XHigh),
            "max" => Some(Self::Max),
            _ => None,
        }
    }
}

/// Resolve the run's effort from `--effort`, then `AFI_EFFORT`.
///
/// An unusable value is an error rather than a warning: a wrong effort produces
/// a finished run that looks exactly like a right one, so there is nothing
/// downstream to notice the fallback. The flag wins over the variable, matching
/// every other setting.
pub(super) fn resolve(flag: Option<&str>, env: Option<&str>) -> Result<Option<Effort>, String> {
    let (name, raw) = match (flag, env.filter(|value| !value.trim().is_empty())) {
        (Some(value), _) => ("--effort", value),
        (None, Some(value)) => ("AFI_EFFORT", value),
        (None, None) => return Ok(None),
    };
    Effort::parse(raw)
        .map(Some)
        .ok_or_else(|| format!("unknown {name} {raw:?} (want {})", LEVELS.join("|")))
}

/// How a source's wire format carries the level: an optional container object,
/// the field inside it, and the highest level its schema defines.
struct Wire {
    container: Option<&'static str>,
    key: &'static str,
    ceiling: Effort,
}

/// Where this source's endpoint carries the effort level, or `None` when afi has
/// nowhere to put one.
///
/// Resolved from the endpoint rather than the source's name, because the wire
/// format belongs to the API being spoken to and a source may be called
/// anything.
///
/// The ceilings are a property of the wire format, which is stable. Individual
/// models are stricter still - Claude Haiku 4.5 takes no effort at all, and
/// older Opus stops at `high` - and afi deliberately keeps no table of that: one
/// nobody notices going stale would send a level with total confidence, where a
/// model that rejects one says so loudly on the first request instead.
fn wire(source: &Source) -> Option<Wire> {
    // The Messages API defines the whole ladder.
    if source.is_anthropic() {
        return Some(Wire {
            container: Some("output_config"),
            key: "effort",
            ceiling: Effort::Max,
        });
    }
    // `reasoning_effort` and `reasoning.effort` are defined up to `high`;
    // anything above it exists on particular models at best, and a level an
    // endpoint does not know is a rejected request rather than a slower one.
    if source.is_openai() {
        return Some(Wire {
            container: None,
            key: "reasoning_effort",
            ceiling: Effort::High,
        });
    }
    let host = source.host();
    if host == "openrouter.ai" || host.ends_with(".openrouter.ai") {
        return Some(Wire {
            container: Some("reasoning"),
            key: "effort",
            ceiling: Effort::High,
        });
    }
    // llama.cpp, vLLM, `SGLang`, and Z.ai either have no such control or spell
    // it per-model, so afi sets nothing and says so rather than guessing a key
    // the endpoint may reject.
    None
}

/// Translate the run's effort into every source, then say so if the source the
/// run starts on ends up carrying something other than what was asked for.
///
/// Only the starting source is reported on: the others are reached through an
/// interactive `/source`, where the caller is present to ask what a request
/// carries.
pub(super) fn apply_to_sources(rt: &mut Runtime, level: Option<Effort>) {
    let Some(level) = level else {
        return;
    };
    for source in rt.sources.values_mut() {
        apply(source, level);
    }
    if let Some(note) = rt.active_source().and_then(|source| note(source, level)) {
        eprintln!("  \u{2717} {note}");
    }
}

/// What became of `asked` on this source, when that is not what was asked for.
///
/// Every case here is a warning rather than a refusal. The level is a
/// preference an endpoint may simply not have, and a run that dies because one
/// of its sources tops out lower would make `--effort` unusable in any script
/// that switches source.
/// The cases are checked in dependency order rather than as a tuple match:
/// `resolved_effort` reads through [`wire`], so a source with no wire has no
/// resolved level either, and half a `(level, wire)` matrix cannot occur.
fn note(source: &Source, asked: Effort) -> Option<String> {
    let name = &source.name;
    let Some(w) = wire(source) else {
        return Some(format!(
            "effort {} did not reach source {name:?} - afi has nowhere to put it on \
             that endpoint, so the run takes whatever the endpoint defaults to",
            asked.as_str(),
        ));
    };
    let Some(sent) = source.resolved_effort() else {
        return Some(format!(
            "source {name:?} sets {:?} in EXTRA_BODY, which afi leaves as written, \
             so effort {} was not added to it",
            w.container.unwrap_or(w.key),
            asked.as_str(),
        ));
    };
    if sent == asked.as_str() {
        return None;
    }
    if asked > w.ceiling && sent == w.ceiling.as_str() {
        return Some(format!(
            "source {name:?} defines no effort above {}, so its requests carry \
             that rather than {}",
            w.ceiling.as_str(),
            asked.as_str(),
        ));
    }
    Some(format!(
        "source {name:?} already sets effort {sent:?}, which wins over --effort {}",
        asked.as_str(),
    ))
}

/// Fold `level` into one source's `extra_body`, in the spelling its endpoint
/// understands and no higher than the level its schema defines.
///
/// Nothing already in `extra_body` is touched - not the key the level would
/// take, and not the object that key lives in. `EXTRA_BODY` is the escape
/// hatch, so anything written there by hand wins.
fn apply(source: &mut Source, level: Effort) {
    let Some(Wire {
        container,
        key,
        ceiling,
    }) = wire(source)
    else {
        return;
    };
    // One guard for both shapes, on the outermost key the level would claim. A
    // container written by hand belongs to whoever wrote it, whatever is in it:
    // merging a level into one would compose a request neither side asked for,
    // since `OpenRouter` documents `reasoning.effort` and `reasoning.max_tokens`
    // as mutually exclusive and afi cannot know which keys any endpoint pairs
    // that way.
    let outer = container.unwrap_or(key);
    let mut body = match &source.extra_body {
        Some(Value::Object(map)) if map.contains_key(outer) => return,
        Some(Value::Object(map)) => map.clone(),
        _ => Map::new(),
    };
    let value = Value::from(level.min(ceiling).as_str());
    let value = match container {
        Some(_) => Value::Object(Map::from_iter([(key.to_string(), value)])),
        None => value,
    };
    body.insert(outer.to_string(), value);
    source.extra_body = Some(Value::Object(body));
}

impl Source {
    /// The effort level this source's requests will carry, or `None` when they
    /// carry none.
    ///
    /// Read back off `extra_body` rather than from the flag, so what is
    /// reported is what the wire gets: a level written by hand in `EXTRA_BODY`,
    /// the one `--effort` resolved to, or nothing at all for an endpoint with
    /// no dialect afi knows.
    #[must_use]
    pub fn resolved_effort(&self) -> Option<&str> {
        let body = self.extra_body.as_ref()?;
        let Wire { container, key, .. } = wire(self)?;
        match container {
            Some(name) => body.get(name)?.get(key)?.as_str(),
            None => body.get(key)?.as_str(),
        }
    }
}

#[cfg(test)]
mod tests;
