//! Port of `tests/test_sources.py`. No live server needed - exercises the
//! in-memory source discovery, switching, provider routing, and built-in
//! `together` / `openrouter` auto-registration.

mod common;

use afi::config::{parse_extra_body, ParsedArgs};
use afi::{ApprovalKind, Source};
use serde_json::json;

// Test 1: legacy fallback (no AFI_SOURCE_* vars)
#[test]
fn test_1_legacy_fallback() {
    let rt = common::build(&["afi"], &[]);
    let names: Vec<&str> = rt.source_order.iter().map(|s| s.as_str()).collect();
    assert_eq!(names, vec!["local"]);
    assert_eq!(rt.active.as_deref(), Some("local"));
    assert_eq!(rt.sources["local"].base_url, "http://localhost:8080/v1");
}

// Test 2: multi-source discovery + $indirection
#[test]
fn test_2_multi_source_discovery() {
    let rt = common::build(
        &["afi"],
        &[
            ("AFI_SOURCES", "local,zai"),
            ("AFI_SOURCE_LOCAL_BASE_URL", "http://localhost:8080/v1"),
            ("AFI_SOURCE_LOCAL_API_KEY", "sk-noop"),
            ("ZAI_TEST_KEY", "fake-zai-key-12345"),
            ("AFI_SOURCE_ZAI_BASE_URL", "https://api.z.ai/api/paas/v4"),
            ("AFI_SOURCE_ZAI_API_KEY", "$ZAI_TEST_KEY"),
            ("AFI_SOURCE_ZAI_MODEL", "glm-x-preview"),
        ],
    );
    assert_eq!(rt.source_order, vec!["local", "zai"]);
    assert_eq!(rt.active.as_deref(), Some("local"));
    assert_eq!(rt.sources["zai"].api_key, "fake-zai-key-12345");
    assert_eq!(rt.sources["zai"].model.as_deref(), Some("glm-x-preview"));
    assert!(rt.sources["local"].model.is_none());
}

// Test 3: switch_source changes the active source and model
#[test]
fn test_3_switch_source() {
    let mut rt = common::build(
        &["afi"],
        &[
            ("AFI_SOURCES", "local,zai"),
            ("AFI_SOURCE_LOCAL_BASE_URL", "http://localhost:8080/v1"),
            ("AFI_SOURCE_ZAI_BASE_URL", "https://api.z.ai/api/paas/v4"),
            ("AFI_SOURCE_ZAI_MODEL", "glm-x-preview"),
        ],
    );
    let _old_model = rt.model.clone();
    assert!(rt.switch_source("zai", None));
    assert_eq!(rt.active.as_deref(), Some("zai"));
    assert_eq!(rt.model.as_deref(), Some("glm-x-preview"));
    assert_ne!(rt.model, _old_model);
}

// Test 4: switch back
#[test]
fn test_4_switch_back() {
    let mut rt = common::build(
        &["afi"],
        &[
            ("AFI_SOURCES", "local,zai"),
            ("AFI_SOURCE_LOCAL_BASE_URL", "http://localhost:8080/v1"),
            ("AFI_SOURCE_ZAI_BASE_URL", "https://api.z.ai/api/paas/v4"),
            ("AFI_SOURCE_ZAI_MODEL", "glm-x-preview"),
        ],
    );
    assert!(rt.switch_source("local", None));
    assert_eq!(rt.active.as_deref(), Some("local"));
}

// Test 5: unknown source returns false
#[test]
fn test_5_unknown_source() {
    let mut rt = common::build(
        &["afi"],
        &[("AFI_SOURCE_LOCAL_BASE_URL", "http://localhost:8080/v1")],
    );
    assert!(!rt.switch_source("nonexistent", None));
}

// Test 6: --source flag picks the starting source
#[test]
fn test_6_source_flag() {
    let rt = common::build(
        &["afi", "--source", "zai"],
        &[
            ("AFI_SOURCES", "local,zai"),
            ("AFI_SOURCE_LOCAL_BASE_URL", "http://localhost:8080/v1"),
            ("AFI_SOURCE_ZAI_BASE_URL", "https://api.z.ai/api/paas/v4"),
            ("AFI_SOURCE_ZAI_MODEL", "glm-x-preview"),
        ],
    );
    assert_eq!(rt.active.as_deref(), Some("zai"));
}

// Test 7: auto-discover from AFI_SOURCE_*_BASE_URL without AFI_SOURCES
#[test]
fn test_7_auto_discover() {
    let rt = common::build(
        &["afi"],
        &[
            ("AFI_SOURCE_LOCAL_BASE_URL", "http://localhost:8080/v1"),
            ("AFI_SOURCE_ZAI_BASE_URL", "https://api.z.ai/api/paas/v4"),
        ],
    );
    let set: std::collections::HashSet<&str> = rt.source_order.iter().map(|s| s.as_str()).collect();
    assert_eq!(set, ["local", "zai"].into_iter().collect());
}

// Test 8: _banner() reflects the active source
#[test]
fn test_8_banner_reflects_source() {
    let mut rt = common::build(
        &["afi"],
        &[
            ("AFI_SOURCES", "local,zai"),
            ("AFI_SOURCE_LOCAL_BASE_URL", "http://localhost:8080/v1"),
            ("AFI_SOURCE_ZAI_BASE_URL", "https://api.z.ai/api/paas/v4"),
        ],
    );
    rt.switch_source("zai", None);
    let banner = afi::banner(&rt);
    assert!(banner.contains("zai"));
}

// Test 9: built-in `together` source auto-registers with a key
#[test]
fn test_9_builtin_together() {
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
    // Registered last, so local stays the startup default.
    assert_eq!(rt.source_order.first().map(|s| s.as_str()), Some("local"));
    assert_eq!(rt.source_order.last().map(|s| s.as_str()), Some("together"));
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

// Test 13: built-in `openrouter` source auto-registers with a key
#[test]
fn test_13_builtin_openrouter() {
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
    assert_eq!(rt.source_order.first().map(|s| s.as_str()), Some("local"));
    assert_eq!(
        rt.source_order.last().map(|s| s.as_str()),
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

// Test 18: extra_request_kwargs plumbing
#[test]
fn test_18_extra_request_kwargs_plumbing() {
    let mut rt = common::build(
        &["afi"],
        &[
            ("AFI_SOURCE_LOCAL_BASE_URL", "http://localhost:8080/v1"),
            ("AFI_OPENROUTER_API_KEY", "k"),
        ],
    );
    let src = rt.sources.get("openrouter").unwrap().clone();
    let kw = src.extra_request_kwargs();
    assert_eq!(
        kw,
        Some(
            json!({"extra_body": {"provider": {"order": ["parasail/fp8"], "allow_fallbacks": false}}})
        )
    );
    // Clearing routing yields None.
    let src_mut = rt.sources.get_mut("openrouter").unwrap();
    src_mut.set_provider_order(&[], None);
    assert!(src_mut.extra_request_kwargs().is_none());
    // Re-pin and confirm the active source's kwarg matches.
    src_mut.set_provider_order(&["parasail/fp8".to_string()], None);
    rt.switch_source("openrouter", None);
    let active = rt.active_source().unwrap();
    assert_eq!(
        active.extra_request_kwargs(),
        Some(
            json!({"extra_body": {"provider": {"order": ["parasail/fp8"], "allow_fallbacks": false}}})
        )
    );
}

// Extra: parse_extra_body edge cases (empty object, non-object, empty string)
#[test]
fn parse_extra_body_empty_object_is_none() {
    assert_eq!(parse_extra_body(Some("{}")), None);
}

#[test]
fn parse_extra_body_non_object_is_none() {
    assert_eq!(parse_extra_body(Some("[1,2,3]")), None);
    assert_eq!(parse_extra_body(Some("\"hi\"")), None);
}

#[test]
fn parse_extra_body_blank_is_none() {
    assert_eq!(parse_extra_body(Some("")), None);
    assert_eq!(parse_extra_body(Some("   ")), None);
    assert_eq!(parse_extra_body(None), None);
}

// Extra: clean_model_id
#[test]
fn clean_model_id_strips_gguf_path() {
    assert_eq!(
        Source::clean_model_id(
            "/media/h/.../GLM-5.2-GGUF/UD-IQ4_NL/GLM-5.2-UD-IQ4_NL-00001-of-00009.gguf"
        ),
        "GLM-5.2-UD-IQ4_NL"
    );
    assert_eq!(
        Source::clean_model_id("/models/Meta-Llama-3-8B-Instruct-Q4_K_M.gguf"),
        "Meta-Llama-3-8B-Instruct-Q4_K_M"
    );
    // org/model form is returned unchanged.
    assert_eq!(Source::clean_model_id("zai-org/GLM-5.2"), "zai-org/GLM-5.2");
    assert_eq!(Source::clean_model_id(""), "");
}

// Extra: is_local
#[test]
fn is_local_classification() {
    let mk = |url: &str| Source::new("x", url.to_string(), None, None, None, None);
    assert!(mk("http://localhost:8080/v1").is_local());
    assert!(mk("http://127.0.0.1:8080/v1").is_local());
    assert!(mk("http://10.0.0.5/v1").is_local());
    assert!(mk("http://192.168.1.5/v1").is_local());
    assert!(mk("http://169.254.1.5/v1").is_local());
    assert!(mk("http://172.16.0.5/v1").is_local());
    assert!(mk("http://172.31.255.255/v1").is_local());
    assert!(!mk("http://172.32.0.5/v1").is_local());
    assert!(!mk("https://api.z.ai/api/paas/v4").is_local());
    assert!(!mk("https://openrouter.ai/api/v1").is_local());
    assert!(mk("").is_local()); // blank -> local
}

// Extra: parse_args handles --resume with and without a target
#[test]
fn parse_args_resume_bare_vs_target() {
    let mk = |args: &[&str]| -> ParsedArgs {
        afi::config::parse_args(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    };
    assert_eq!(mk(&["afi", "--resume"]).resume, Some(None));
    assert_eq!(
        mk(&["afi", "--resume", "deadbe"]).resume,
        Some(Some("deadbe".to_string()))
    );
    // --resume --yolo does NOT swallow --yolo as the target.
    let p = mk(&["afi", "--resume", "--yolo"]);
    assert_eq!(p.resume, Some(None));
    assert!(p.yolo);
}

// Extra: ApprovalKind surfaces from a source-built runtime
#[test]
fn approval_kind_import_works() {
    let _ = ApprovalKind::Yolo;
}
