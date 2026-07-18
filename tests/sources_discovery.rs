//! Port of `tests/test_sources.py` (discovery half). Exercises in-memory
//! source discovery, `$indirection`, `--source`, switching, and the banner.

mod common;

use std::collections::HashSet;

// Test 1: legacy fallback (no AFI_SOURCE_* vars)
#[test]
fn test_1_legacy_fallback() {
    let rt = common::build(&["afi"], &[]);
    let names: Vec<&str> = rt.source_order.iter().map(String::as_str).collect();
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
    let old_model = rt.model.clone();
    assert!(rt.switch_source("zai", None));
    assert_eq!(rt.active.as_deref(), Some("zai"));
    assert_eq!(rt.model.as_deref(), Some("glm-x-preview"));
    assert_ne!(rt.model, old_model);
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
    let set: HashSet<&str> = rt.source_order.iter().map(String::as_str).collect();
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
