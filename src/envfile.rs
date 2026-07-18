//! Env-file loading (`~/.env`) and `$name` key indirection.
//!
//! Loads `~/.env` (or `AFI_ENV_FILE`) into a map without clobbering vars
//! already set. Lets source config / API keys live in one place instead of
//! being exported in every terminal. Mirrors the Python `_load_env_file` /
//! `_resolve_api_key` helpers.

use std::collections::HashMap;
use std::fs;
use std::hash::BuildHasher;
use std::path::Path;

/// Parse a `KEY=VALUE` env file into a map.
///
/// Handles `export` prefix, quoted values, comments, and blank lines - the
/// same subset as the Python loader. Unknown lines are silently skipped.
#[must_use]
pub fn parse_env_file(path: &Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(text) = fs::read_to_string(path) else {
        return out;
    };
    for raw in text.lines() {
        if let Some((k, v)) = parse_env_line(raw) {
            out.insert(k, v);
        }
    }
    out
}

/// Parse one `KEY=VALUE` line, honoring an `export` prefix and matched quotes.
/// Returns `None` for blank lines, comments, and lines without a key.
fn parse_env_line(raw: &str) -> Option<(String, String)> {
    let line = raw.trim();
    if line.is_empty() || line.starts_with('#') || !line.contains('=') {
        return None;
    }
    let (k, v) = line.split_once('=')?;
    let k = k.trim();
    let k = k.strip_prefix("export ").map_or(k, str::trim);
    if k.is_empty() {
        return None;
    }
    Some((k.to_string(), strip_matching_quotes(v.trim()).to_string()))
}

/// Strip a single pair of matching surrounding `'` or `"` quotes, if present.
fn strip_matching_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if first == last && (first == b'\'' || first == b'"') {
            return &s[1..s.len() - 1];
        }
    }
    s
}

/// Load an env file and merge into `env` without clobbering existing keys.
pub fn load_into<S: BuildHasher>(env: &mut HashMap<String, String, S>, path: &Path) {
    for (k, v) in parse_env_file(path) {
        env.entry(k).or_insert(v);
    }
}

/// Resolve a `$name` API-key indirection.
///
/// `"$FOO"` looks up `FOO` in `env` (returns `""` if missing); any other
/// value is returned as-is. `None` stays `None` (caller applies the
/// `"sk-noop"` default).
#[must_use]
pub fn resolve_api_key<S: BuildHasher>(
    env: &HashMap<String, String, S>,
    val: Option<&str>,
) -> Option<String> {
    match val {
        Some(v) if v.starts_with('$') => Some(env.get(&v[1..]).cloned().unwrap_or_default()),
        Some(v) => Some(v.to_string()),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_plain_pairs() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "FOO=bar\nBAZ=qux\n# comment\n\nexport EXPORTED=yes").unwrap();
        let m = parse_env_file(f.path());
        assert_eq!(m.get("FOO").unwrap(), "bar");
        assert_eq!(m.get("BAZ").unwrap(), "qux");
        assert_eq!(m.get("EXPORTED").unwrap(), "yes");
    }

    #[test]
    fn strips_quotes() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "A='hello'\nB=\"world\"\nC=bare").unwrap();
        let m = parse_env_file(f.path());
        assert_eq!(m.get("A").unwrap(), "hello");
        assert_eq!(m.get("B").unwrap(), "world");
        assert_eq!(m.get("C").unwrap(), "bare");
    }

    #[test]
    fn resolve_indirection() {
        let mut env = HashMap::new();
        env.insert("REAL_KEY".to_string(), "secret-123".to_string());
        assert_eq!(
            resolve_api_key(&env, Some("$REAL_KEY")),
            Some("secret-123".to_string())
        );
        assert_eq!(resolve_api_key(&env, Some("$MISSING")), Some(String::new()));
        assert_eq!(
            resolve_api_key(&env, Some("literal-key")),
            Some("literal-key".to_string())
        );
        assert_eq!(resolve_api_key(&env, None), None);
    }
}
