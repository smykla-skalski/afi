//! Memory: save, remember, and list developer memories stored as markdown
//! files under `~/.afi/memories/`.
//!
//! `/memory save [focus...]`  - distill the session into a memory (model call,
//!   phase 5 wires the actual API; for now it's a stub that returns an error)
//! `/memory remember <query>` - keyword search through saved memories
//! `/memory list`             - list all saved memories

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;

use crate::sessions::memories_dir;
use std::cmp::Reverse;
use std::hash::BuildHasher;
use std::sync::LazyLock;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

/// Turn a title into a filesystem-safe slug.
pub fn slugify(text: &str, maxlen: usize) -> String {
    static NON_ALNUM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^a-z0-9]+").unwrap());
    let s = text.to_lowercase().trim().to_string();
    let s = NON_ALNUM.replace_all(&s, "-");
    let s = s.trim_matches('-').to_string();
    let s = if s.len() > maxlen {
        s[..maxlen].trim_end_matches('-').to_string()
    } else {
        s
    };
    if s.is_empty() {
        "untitled".to_string()
    } else {
        s
    }
}

/// List all saved memories. Returns `(filename, title)` pairs sorted newest-first.
#[must_use]
pub fn list_memories<S: BuildHasher>(env: &HashMap<String, String, S>) -> Vec<(String, String)> {
    let dir = memories_dir(env);
    let Ok(entries) = fs::read_dir(&dir) else {
        return vec![];
    };
    let mut memories: Vec<(SystemTime, String, String)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !Path::new(&name)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("md"))
        {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(UNIX_EPOCH);
        let content = fs::read_to_string(entry.path()).unwrap_or_default();
        let title = content.lines().find(|l| l.starts_with("# ")).map_or_else(
            || name.trim_end_matches(".md").to_string(),
            |l| l.trim_start_matches("# ").to_string(),
        );
        memories.push((mtime, name, title));
    }
    memories.sort_by_key(|(m, _, _)| Reverse(*m));
    memories.into_iter().map(|(_, n, t)| (n, t)).collect()
}

/// Search saved memories by keyword (case-insensitive). Returns matching
/// `(filename, title, first_line)` tuples.
#[must_use]
pub fn remember_memories<S: BuildHasher>(
    env: &HashMap<String, String, S>,
    query: &str,
) -> Vec<(String, String, String)> {
    let dir = memories_dir(env);
    let q = query.to_lowercase();
    let Ok(entries) = fs::read_dir(&dir) else {
        return vec![];
    };
    let mut results: Vec<(SystemTime, String, String, String)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if !Path::new(&name)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("md"))
        {
            continue;
        }
        let content = fs::read_to_string(&path).unwrap_or_default();
        if !content.to_lowercase().contains(&q) {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(UNIX_EPOCH);
        let title = content.lines().find(|l| l.starts_with("# ")).map_or_else(
            || name.trim_end_matches(".md").to_string(),
            |l| l.trim_start_matches("# ").to_string(),
        );
        let first_line = content.lines().take(3).collect::<Vec<_>>().join("\n");
        results.push((mtime, name, title, first_line));
    }
    results.sort_by_key(|(m, _, _, _)| Reverse(*m));
    results.into_iter().map(|(_, n, t, f)| (n, t, f)).collect()
}

/// Save a memory markdown file to `~/.afi/memories/<slug>.md`.
/// Returns the path of the saved file.
#[must_use]
pub fn save_memory_file<S: BuildHasher>(
    env: &HashMap<String, String, S>,
    slug: &str,
    content: &str,
) -> PathBuf {
    let dir = memories_dir(env);
    let _ = fs::create_dir_all(&dir);
    let path = dir.join(format!("{slug}.md"));
    let _ = fs::write(&path, content);
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::Path;

    fn env_for(dir: &Path) -> HashMap<String, String> {
        let mut env = HashMap::new();
        env.insert("AFI_HOME".to_string(), dir.to_string_lossy().to_string());
        env
    }

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Hello World!", 60), "hello-world");
        assert_eq!(slugify("  Some  Title  ", 60), "some-title");
    }

    #[test]
    fn slugify_clamps() {
        let long = "a".repeat(200);
        let s = slugify(&long, 10);
        assert!(s.len() <= 10);
    }

    #[test]
    fn slugify_empty_falls_back() {
        assert_eq!(slugify("", 60), "untitled");
        assert_eq!(slugify("---", 60), "untitled");
    }

    #[test]
    fn save_and_list_memory() {
        let tmp = tempfile::tempdir().unwrap();
        let env = env_for(tmp.path());
        let _ = save_memory_file(&env, "test-memory", "# Test Memory\n\nSome content here.\n");
        let memories = list_memories(&env);
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].0, "test-memory.md");
        assert_eq!(memories[0].1, "Test Memory");
    }

    #[test]
    fn remember_searches_content() {
        let tmp = tempfile::tempdir().unwrap();
        let env = env_for(tmp.path());
        let _ = save_memory_file(
            &env,
            "rust-tips",
            "# Rust Tips\n\nAlways use cargo clippy.\n",
        );
        let _ = save_memory_file(&env, "python-tips", "# Python Tips\n\nUse type hints.\n");
        let results = remember_memories(&env, "cargo");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "rust-tips.md");
    }
}
