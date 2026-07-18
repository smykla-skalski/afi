//! Env-file loading (`~/.env`) and `$name` key indirection.
//!
//! Loads `~/.env` (or `MINION_ENV_FILE`) into a map without clobbering vars
//! already set. Lets source config / API keys live in one place instead of
//! being exported in every terminal. Mirrors the Python `_load_env_file` /
//! `_resolve_api_key` helpers.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Parse a `KEY=VALUE` env file into a map.
///
/// Handles `export` prefix, quoted values, comments, and blank lines - the
/// same subset as the Python loader. Unknown lines are silently skipped.
pub fn parse_env_file(path: &Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return out,
    };
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || !line.contains('=') {
            continue;
        }
        let (k, v) = match line.split_once('=') {
            Some(kv) => kv,
            None => continue,
        };
        let mut k = k.trim();
        if let Some(stripped) = k.strip_prefix("export ") {
            k = stripped.trim();
        }
        if k.is_empty() {
            continue;
        }
        let mut v = v.trim().to_string();
        if v.len() >= 2 {
            let first = v.chars().next().unwrap();
            let last = v.chars().last().unwrap();
            if first == last && (first == '\'' || first == '"') {
                v = v[1..v.len() - 1].to_string();
            }
        }
        out.insert(k.to_string(), v);
    }
    out
}

/// Load an env file and merge into `env` without clobbering existing keys.
pub fn load_into(env: &mut HashMap<String, String>, path: &Path) {
    for (k, v) in parse_env_file(path) {
        env.entry(k).or_insert(v);
    }
}

/// Resolve a `$name` API-key indirection.
///
/// `"$FOO"` looks up `FOO` in `env` (returns `""` if missing); any other
/// value is returned as-is. `None` stays `None` (caller applies the
/// `"sk-noop"` default).
pub fn resolve_api_key(env: &HashMap<String, String>, val: Option<&str>) -> Option<String> {
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
        assert_eq!(
            resolve_api_key(&env, Some("$MISSING")),
            Some("".to_string())
        );
        assert_eq!(
            resolve_api_key(&env, Some("literal-key")),
            Some("literal-key".to_string())
        );
        assert_eq!(resolve_api_key(&env, None), None);
    }
}
