//! Port of `tests/test_sources.py` (built-in providers half). Covers the
//! `together` / `openrouter` auto-registration, explicit overrides, provider
//! ordering, and `extra_body` plumbing.

mod common;

use serde_json::json;

// Test 9a: built-in `together` source auto-registers its config with a key.
#[test]
fn test_9_builtin_together_config() {
    let rt = common::build(
        &["afi"],
        &[
            ("AFI_SOURCE_LOCAL_BASE_URL", "http://localhost:8080/v1"),
            ("AFI_TOGETHER_API_KEY", "fake-together-key"),
        ],
    );
    assert!(rt.sources.contains_key("together"));
    assert_eq!(
        rt.sources["together"].base_url,
        "https://api.together.xyz/v1"
    );
    assert_eq!(
        rt.sources["together"].model.as_deref(),
        Some("zai-org/GLM-5.2")
    );
    assert_eq!(rt.sources["together"].api_key, "fake-together-key");
}

// Test 9b: `together` registers last, so local stays the startup default.
#[test]
fn test_9_builtin_together_ordering() {
    let rt = common::build(
        &["afi"],
        &[
            ("AFI_SOURCE_LOCAL_BASE_URL", "http://localhost:8080/v1"),
            ("AFI_TOGETHER_API_KEY", "fake-together-key"),
        ],
    );
    assert_eq!(rt.source_order.first().map(String::as_str), Some("local"));
    assert_eq!(rt.source_order.last().map(String::as_str), Some("together"));
    assert_eq!(rt.active.as_deref(), Some("local"));
}

// Test 10: no together source without a key
#[test]
fn test_10_no_together_without_key() {
    let rt = common::build(
        &["afi"],
        &[("AFI_SOURCE_LOCAL_BASE_URL", "http://localhost:8080/v1")],
    );
    assert!(!rt.sources.contains_key("together"));
}

// Test 11: explicit together config wins over the built-in
#[test]
fn test_11_explicit_together_overrides_builtin() {
    let rt = common::build(
        &["afi"],
        &[
            ("AFI_SOURCES", "together"),
            (
                "AFI_SOURCE_TOGETHER_BASE_URL",
                "https://my-proxy.example/v1",
            ),
            ("AFI_SOURCE_TOGETHER_API_KEY", "custom-key"),
            ("AFI_SOURCE_TOGETHER_MODEL", "my-org/my-model"),
            ("AFI_TOGETHER_API_KEY", "fake-together-key"),
        ],
    );
    assert_eq!(
        rt.sources["together"].base_url,
        "https://my-proxy.example/v1"
    );
    assert_eq!(
        rt.sources["together"].model.as_deref(),
        Some("my-org/my-model")
    );
    assert_eq!(rt.sources["together"].api_key, "custom-key");
}

// Test 12: switch_source model override (per-switch, non-sticky)
#[test]
fn test_12_switch_source_model_override() {
    let mut rt = common::build(
        &["afi"],
        &[
            ("AFI_SOURCES", "together"),
            (
                "AFI_SOURCE_TOGETHER_BASE_URL",
                "https://api.together.xyz/v1",
            ),
            ("AFI_SOURCE_TOGETHER_API_KEY", "k"),
            ("AFI_SOURCE_TOGETHER_MODEL", "zai-org/GLM-5.2"),
        ],
    );
    rt.switch_source("together", None);
    assert_eq!(rt.model.as_deref(), Some("zai-org/GLM-5.2"));
    rt.switch_source("together", Some("zai-org/GLM-4.6"));
    assert_eq!(rt.model.as_deref(), Some("zai-org/GLM-4.6"));
    rt.switch_source("together", None);
    assert_eq!(rt.model.as_deref(), Some("zai-org/GLM-5.2"));
}

// Test 13a: built-in `openrouter` source auto-registers its config with a key.
#[test]
fn test_13_builtin_openrouter_config() {
    let rt = common::build(
        &["afi"],
        &[
            ("AFI_SOURCE_LOCAL_BASE_URL", "http://localhost:8080/v1"),
            ("AFI_OPENROUTER_API_KEY", "fake-or-key"),
        ],
    );
    let or_src = rt
        .sources
        .get("openrouter")
        .expect("openrouter should auto-register");
    assert_eq!(or_src.base_url, "https://openrouter.ai/api/v1");
    assert_eq!(or_src.model.as_deref(), Some("z-ai/glm-5.2"));
    assert_eq!(or_src.api_key, "fake-or-key");
    assert_eq!(
        or_src.extra_body,
        Some(json!({"provider": {"order": ["parasail/fp8"], "allow_fallbacks": false}}))
    );
}

// Test 13b: `openrouter` registers last, so local stays the startup default.
#[test]
fn test_13_builtin_openrouter_ordering() {
    let rt = common::build(
        &["afi"],
        &[
            ("AFI_SOURCE_LOCAL_BASE_URL", "http://localhost:8080/v1"),
            ("AFI_OPENROUTER_API_KEY", "fake-or-key"),
        ],
    );
    assert_eq!(rt.source_order.first().map(String::as_str), Some("local"));
    assert_eq!(
        rt.source_order.last().map(String::as_str),
        Some("openrouter")
    );
    assert_eq!(rt.active.as_deref(), Some("local"));
}

// Test 14: no openrouter source without a key
#[test]
fn test_14_no_openrouter_without_key() {
    let rt = common::build(
        &["afi"],
        &[("AFI_SOURCE_LOCAL_BASE_URL", "http://localhost:8080/v1")],
    );
    assert!(!rt.sources.contains_key("openrouter"));
}

// Test 15: explicit openrouter config overrides the built-in
#[test]
fn test_15_explicit_openrouter_overrides_builtin() {
    let rt = common::build(
        &["afi"],
        &[
            ("AFI_SOURCES", "openrouter"),
            (
                "AFI_SOURCE_OPENROUTER_BASE_URL",
                "https://my-or-proxy.example/v1",
            ),
            ("AFI_SOURCE_OPENROUTER_API_KEY", "custom-or-key"),
            ("AFI_SOURCE_OPENROUTER_MODEL", "my-org/my-model"),
            (
                "AFI_SOURCE_OPENROUTER_EXTRA_BODY",
                r#"{"provider":{"order":["Together"],"allow_fallbacks":true}}"#,
            ),
            ("AFI_OPENROUTER_API_KEY", "fake-or-key"),
        ],
    );
    assert_eq!(
        rt.sources["openrouter"].base_url,
        "https://my-or-proxy.example/v1"
    );
    assert_eq!(
        rt.sources["openrouter"].model.as_deref(),
        Some("my-org/my-model")
    );
    assert_eq!(rt.sources["openrouter"].api_key, "custom-or-key");
    assert_eq!(
        rt.sources["openrouter"].extra_body,
        Some(json!({"provider": {"order": ["Together"], "allow_fallbacks": true}}))
    );
}

// Test 16: bad EXTRA_BODY JSON is ignored loudly (no silent routing)
#[test]
fn test_16_bad_extra_body_ignored() {
    let rt = common::build(
        &["afi"],
        &[
            ("AFI_SOURCES", "openrouter"),
            (
                "AFI_SOURCE_OPENROUTER_BASE_URL",
                "https://openrouter.ai/api/v1",
            ),
            ("AFI_SOURCE_OPENROUTER_API_KEY", "k"),
            ("AFI_SOURCE_OPENROUTER_MODEL", "z-ai/glm-5.2"),
            ("AFI_SOURCE_OPENROUTER_EXTRA_BODY", "not json{"),
        ],
    );
    // A malformed explicit config must NOT fall back to the built-in default -
    // extra_body stays None so the typo fails loudly.
    assert!(rt.sources["openrouter"].extra_body.is_none());
}

// Test 17: _set_provider_order / _provider_order helpers
#[test]
fn test_17_provider_order_helpers() {
    let mut rt = common::build(
        &["afi"],
        &[
            ("AFI_SOURCE_LOCAL_BASE_URL", "http://localhost:8080/v1"),
            ("AFI_OPENROUTER_API_KEY", "k"),
        ],
    );
    let src = rt.sources.get_mut("openrouter").unwrap();
    // Starts with the built-in default routing.
    assert_eq!(src.provider_order(), vec!["parasail/fp8".to_string()]);
    // Pin a new ordered list - defaults to no fallback (deliberate choice).
    src.set_provider_order(&["Together".to_string(), "DeepInfra".to_string()], None);
    assert_eq!(src.provider_order(), vec!["Together", "DeepInfra"]);
    assert_eq!(
        src.extra_body.as_ref().unwrap()["provider"]["allow_fallbacks"],
        false
    );
    // Explicit allow_fallbacks=true is honored.
    src.set_provider_order(&["Together".to_string()], Some(true));
    assert_eq!(
        src.extra_body.as_ref().unwrap()["provider"]["allow_fallbacks"],
        true
    );
    // Empty order clears routing entirely.
    src.set_provider_order(&[], None);
    assert!(src.provider_order().is_empty());
    assert!(src.extra_body.is_none());
}

// Test 18: extra_body plumbing.
//
// The body keys are carried unwrapped. An earlier `extra_request_kwargs()`
// helper wrapped them as `{"extra_body": {...}}` - the shape the Python OpenAI
// SDK unwrapped as a kwarg - but the Rust client builds the request body by
// hand, so nothing ever unwrapped it. Its one caller (`/compress`) therefore
// silently dropped provider routing on every source.
#[test]
fn test_18_extra_body_plumbing() {
    let mut rt = common::build(
        &["afi"],
        &[
            ("AFI_SOURCE_LOCAL_BASE_URL", "http://localhost:8080/v1"),
            ("AFI_OPENROUTER_API_KEY", "k"),
        ],
    );
    let routing = json!({"provider": {"order": ["parasail/fp8"], "allow_fallbacks": false}});

    let src = rt.sources.get("openrouter").unwrap().clone();
    assert_eq!(src.extra_body, Some(routing.clone()));

    // Clearing routing yields None.
    let src_mut = rt.sources.get_mut("openrouter").unwrap();
    src_mut.set_provider_order(&[], None);
    assert!(src_mut.extra_body.is_none());

    // Re-pin and confirm the active source carries it.
    src_mut.set_provider_order(&["parasail/fp8".to_string()], None);
    rt.switch_source("openrouter", None);
    let active = rt.active_source().unwrap();
    assert_eq!(active.extra_body, Some(routing));
}
