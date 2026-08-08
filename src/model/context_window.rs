//! Context window probing: best-effort max context (tokens) for the active
//! model on a source. Cached per source; invalidated on source/model switch.
//!
//! Probe order depends on host locality:
//! - Local (llama.cpp etc.): /v1/models meta -> /props -> overrun probe
//! - Remote (Together, Z.ai, ...): overrun probe -> /v1/models -> /props
//!
//! None of that is on the request path. What a run actually resolves its window
//! from is [`table`], a compiled model-to-window table an operator can override
//! per source - a static fact about a model, learned without a round trip. The
//! probes here answer the same question over the network and stay for a caller
//! that wants to ask a server directly.

use regex::Regex;
use serde_json::Value;

use crate::config::Source;
use std::sync::LazyLock;

pub mod table;
pub use table::context_window_for;

/// Pull `n_ctx` / `context_length` from GET /v1/models. llama.cpp stashes it
/// under `data[0].meta.n_ctx`; some hosts expose `context_length` as a
/// top-level model field.
#[must_use]
pub fn ctx_from_models(models_data: &Value, mid: &str) -> Option<u64> {
    let data = models_data.get("data")?.as_array()?;
    let mtail = mid.rsplit('/').next()?.to_lowercase();
    let pick = data
        .iter()
        .find(|m| model_id_matches(m, mid, &mtail))
        .or_else(|| data.first())?;

    // Prefer the llama.cpp-style `meta` dict, then top-level OpenAI-compat keys.
    if let Some(meta) = pick.get("meta")
        && let Some(n) = first_positive(
            meta,
            &[
                "n_ctx",
                "context_length",
                "context_window",
                "max_context_length",
                "max_input_tokens",
            ],
        )
    {
        return Some(n);
    }
    first_positive(
        pick,
        &[
            "context_length",
            "context_window",
            "max_context_length",
            "max_input_tokens",
            "max_tokens",
            "max_model_len",
        ],
    )
}

/// True when model entry `m`'s id equals `mid` or shares its `/`-tail.
fn model_id_matches(m: &Value, mid: &str, mtail: &str) -> bool {
    m.get("id").and_then(|v| v.as_str()).is_some_and(|id| {
        id.to_lowercase() == mid.to_lowercase()
            || id.rsplit('/').next().map(str::to_lowercase).as_deref() == Some(mtail)
    })
}

/// The first `> 0` `u64` value among `keys` on the object-valued `obj`.
fn first_positive(obj: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|k| obj.get(*k).and_then(Value::as_u64).filter(|v| *v > 0))
}

/// llama.cpp /props fallback: `default_generation_settings.n_ctx`.
#[must_use]
pub fn ctx_from_props(props: &Value) -> Option<u64> {
    if let Some(dgs) = props
        .get("default_generation_settings")
        .and_then(|v| v.as_object())
    {
        for key in &["n_ctx", "context_length", "context_window"] {
            if let Some(v) = dgs.get(*key).and_then(Value::as_u64)
                && v > 0
            {
                return Some(v);
            }
        }
    }
    for key in &["n_ctx", "context_length", "context_window"] {
        if let Some(v) = props.get(*key).and_then(Value::as_u64)
            && v > 0
        {
            return Some(v);
        }
    }
    None
}

/// Parse the "maximum context length is N tokens" error message from an
/// over-max_tokens chat request.
pub fn ctx_from_error_message(msg: &str) -> Option<u64> {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)maximum context length is (\d+) tokens").unwrap());
    RE.captures(msg)
        .and_then(|c| c.get(1).and_then(|m| m.as_str().parse::<u64>().ok()))
}

/// Resolve the context window for `source` by trying the probes in the right
/// order for the host's locality. Returns `None` on a total miss.
///
/// This is a pure function that takes the pre-fetched data; the actual HTTP
/// fetching is done by the caller (the `Client` in `client.rs`).
#[must_use]
pub fn resolve_context_window(
    source: &Source,
    mid: &str,
    models_data: Option<&Value>,
    props_data: Option<&Value>,
    error_msg: Option<&str>,
) -> Option<u64> {
    let from_models = || models_data.and_then(|d| ctx_from_models(d, mid));
    let from_props = || props_data.and_then(ctx_from_props);
    let from_error = || error_msg.and_then(ctx_from_error_message);
    if source.is_local() {
        // Local: /v1/models -> /props -> overrun probe
        from_models().or_else(from_props).or_else(from_error)
    } else {
        // Remote: overrun probe -> /v1/models -> /props
        from_error().or_else(from_models).or_else(from_props)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ctx_from_models_meta_n_ctx() {
        let data = json!({
            "data": [{"id": "test-model", "meta": {"n_ctx": 8192}}]
        });
        assert_eq!(ctx_from_models(&data, "test-model"), Some(8192));
    }

    #[test]
    fn ctx_from_models_context_length() {
        let data = json!({
            "data": [{"id": "model", "context_length": 32768}]
        });
        assert_eq!(ctx_from_models(&data, "model"), Some(32768));
    }

    #[test]
    fn ctx_from_models_empty_data() {
        let data = json!({"data": []});
        assert_eq!(ctx_from_models(&data, "model"), None);
    }

    #[test]
    fn ctx_from_props_dgs() {
        let props = json!({"default_generation_settings": {"n_ctx": 4096}});
        assert_eq!(ctx_from_props(&props), Some(4096));
    }

    #[test]
    fn ctx_from_props_top_level() {
        let props = json!({"n_ctx": 2048});
        assert_eq!(ctx_from_props(&props), Some(2048));
    }

    #[test]
    fn ctx_from_error_message_parses() {
        assert_eq!(
            ctx_from_error_message("This model's maximum context length is 32768 tokens"),
            Some(32768)
        );
    }

    #[test]
    fn resolve_local_prefers_models() {
        let source = Source::new(
            "test",
            "http://localhost:8080/v1".to_string(),
            None,
            None,
            None,
            None,
        );
        let data = json!({"data": [{"id": "m", "meta": {"n_ctx": 4096}}]});
        let props = json!({"default_generation_settings": {"n_ctx": 8192}});
        assert_eq!(
            resolve_context_window(&source, "m", Some(&data), Some(&props), None),
            Some(4096)
        );
    }

    #[test]
    fn resolve_remote_prefers_error() {
        let source = Source::new(
            "test",
            "https://api.together.xyz/v1".to_string(),
            None,
            None,
            None,
            None,
        );
        let data = json!({"data": [{"id": "m", "context_length": 4096}]});
        assert_eq!(
            resolve_context_window(
                &source,
                "m",
                Some(&data),
                None,
                Some("maximum context length is 32768 tokens")
            ),
            Some(32768)
        );
    }
}
