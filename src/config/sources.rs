//! Building the sources map from the environment.
//!
//! Split out of `runtime` because it answers a different question: `Runtime` is
//! the state a session carries, and this is how the `AFI_SOURCE_*` variables
//! become the endpoints it carries. Nothing here touches session state.

use std::collections::HashMap;
use std::hash::BuildHasher;

use crate::envfile;

use super::builtins::add_builtin_sources;
use super::{Protocol, Source, build_http_headers, parse_extra_body};

/// Build the sources map + ordered list from `AFI_SOURCE_*` env vars,
/// falling back to a single `local` source from the legacy `AFI_*` vars.
#[must_use]
pub fn discover_sources<S: BuildHasher>(
    env: &HashMap<String, String, S>,
) -> (HashMap<String, Source>, Vec<String>) {
    let mut sources: HashMap<String, Source> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for name in source_names(env) {
        if let Some(src) = configured_source(env, &name) {
            sources.insert(name.clone(), src);
            order.push(name);
        }
    }
    if sources.is_empty() {
        add_legacy_source(env, &mut sources, &mut order);
    }
    add_builtin_sources(env, &mut sources, &mut order);

    (sources, order)
}

/// The configured source names: an explicit `AFI_SOURCES` list, else
/// auto-discovered from `AFI_SOURCE_*_BASE_URL` keys (sorted).
fn source_names<S: BuildHasher>(env: &HashMap<String, String, S>) -> Vec<String> {
    if let Some(raw) = env.get("AFI_SOURCES").filter(|r| !r.trim().is_empty()) {
        return raw
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    let mut found: Vec<String> = env
        .keys()
        .filter_map(|k| {
            k.strip_prefix("AFI_SOURCE_")
                .and_then(|rest| rest.strip_suffix("_BASE_URL"))
                .map(str::to_lowercase)
        })
        .collect();
    found.sort();
    found
}

/// Build a `Source` for `name` from its `AFI_SOURCE_<NAME>_*` vars, or `None`
/// when no `BASE_URL` is set.
fn configured_source<S: BuildHasher>(
    env: &HashMap<String, String, S>,
    name: &str,
) -> Option<Source> {
    let p = format!("AFI_SOURCE_{}_", name.to_uppercase());
    let base_url = env.get(&format!("{p}BASE_URL"))?.clone();
    let api_key =
        envfile::resolve_api_key(env, env.get(&format!("{p}API_KEY")).map(String::as_str));
    let model = env.get(&format!("{p}MODEL")).cloned();
    let extra_body = parse_extra_body(env.get(&format!("{p}EXTRA_BODY")).map(String::as_str));
    let http_headers = build_http_headers(
        env.get(&format!("{p}APP_NAME")).map(String::as_str),
        env.get(&format!("{p}APP_URL")).map(String::as_str),
    );
    let protocol = env
        .get(&format!("{p}PROTOCOL"))
        .map_or(Protocol::default(), |raw| Protocol::from_env_value(raw));
    Some(
        Source::new(name, base_url, api_key, model, extra_body, http_headers)
            .with_protocol(protocol),
    )
}

/// Legacy single-source fallback from the bare `AFI_*` vars.
fn add_legacy_source<S: BuildHasher>(
    env: &HashMap<String, String, S>,
    sources: &mut HashMap<String, Source>,
    order: &mut Vec<String>,
) {
    let src = Source::new(
        "local",
        env.get("AFI_BASE_URL")
            .cloned()
            .unwrap_or_else(|| "http://localhost:8080/v1".to_string()),
        envfile::resolve_api_key(env, env.get("AFI_API_KEY").map(String::as_str)),
        env.get("AFI_MODEL").cloned(),
        None,
        None,
    );
    sources.insert("local".to_string(), src);
    order.push("local".to_string());
}
