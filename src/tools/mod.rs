//! Tools the agent can call: read_file, write_file, edit_file, list_dir,
//! run_bash, wait_background.
//!
//! Phase 3 implements the file tools + the text-protocol parser. Bash (with
//! detached setsid + background polling) lands in `bash.rs`. The risk
//! classifier and approval prompt (phase 4) wrap the dispatch layer.

pub mod bash;
pub mod protocol;

use std::fs;

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Value};

/// The names of all registered tools, in dispatch order.
pub fn known_tool_names() -> &'static [&'static str] {
    &[
        "read_file",
        "write_file",
        "edit_file",
        "list_dir",
        "run_bash",
        "wait_background",
    ]
}

/// The OpenAI tool schemas sent to the model. Mirrors `TOOLS` in the Python.
pub static TOOLS: Lazy<Value> = Lazy::new(|| {
    json!([
        {"type": "function", "function": {"name": "read_file", "description": "Read a file's contents. Returns lines numbered (1-based, like `cat -n`: a right-aligned number, a tab, then the line). Large files return only a window — pass `offset` (1-based start line) and `limit` (max lines, default 400) to page through the rest; a header shows the visible range and total line count.",
            "parameters": {"type": "object", "properties": {"path": {"type": "string"}, "offset": {"type": "integer", "description": "1-based line to start from (default 1)"}, "limit": {"type": "integer", "description": "max lines to return (default 400; <=0 reads to end)"}}, "required": ["path"]}}},
        {"type": "function", "function": {"name": "write_file", "description": "Write (overwrite) a file",
            "parameters": {"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}}, "required": ["path", "content"]}}},
        {"type": "function", "function": {"name": "edit_file", "description": "Replace one exact occurrence of `old` with `new` in a file. Prefer raw file text; if you paste read_file's numbered lines, the `<n>\\t` line-number prefixes are stripped automatically.",
            "parameters": {"type": "object", "properties": {"path": {"type": "string"}, "old": {"type": "string"}, "new": {"type": "string"}}, "required": ["path", "old", "new"]}}},
        {"type": "function", "function": {"name": "list_dir", "description": "List a directory",
            "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}}},
        {"type": "function", "function": {"name": "run_bash", "description": "Run a shell command. Commands are launched detached (new session via setsid(2)) and never block the agent loop. If the command finishes within a short poll window (~3 s by default), output is returned directly; otherwise it continues in the background and you get a PID + log path to check later with read_file() or wait_background().",
            "parameters": {"type": "object", "properties": {"command": {"type": "string"}, "timeout": {"type": "integer", "description": "Seconds to wait for the command to finish synchronously (default unset = ~3 s poll; 0 = wait indefinitely)."}}, "required": ["command"]}}},
        {"type": "function", "function": {"name": "wait_background", "description": "Wait for a backgrounded command (previously started by run_bash) to finish and return its output.",
            "parameters": {"type": "object", "properties": {"pid": {"type": "integer", "description": "PID of the background command to wait for"}, "log_path": {"type": "string", "description": "Log file path from the background message (optional — auto-located by PID if omitted)"}, "timeout": {"type": "integer", "description": "Max seconds to wait (default 0 = wait indefinitely; almost never set this)"}}, "required": ["pid"]}}}
    ])
});

// --- read_file ---------------------------------------------------------------

/// Read a file as numbered lines (1-based, `cat -n` style). Large files
/// return only a window; pass `offset` / `limit` to page through the rest.
/// A header announces the visible range + total line count.
pub fn read_file(
    path: &str,
    offset: Option<i64>,
    limit: Option<i64>,
    env_read_lines: i64,
) -> String {
    let lines = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => return format!("ERROR reading {}: {}", path, e),
    };
    let all: Vec<&str> = lines.split('\n').collect();
    // split('\n') on "a\nb\n" gives ["a", "b", ""] — drop the trailing empty
    // from a final newline so line counts match `cat -n`. An empty file ("")
    // gives [""] which is 0 lines.
    let total = if all.len() == 1 && all[0].is_empty() {
        0
    } else if all.last() == Some(&"") && all.len() > 1 {
        all.len() - 1
    } else {
        all.len()
    };

    let start = match offset {
        Some(o) => o.max(1) as usize - 1,
        None => 0,
    };

    if total == 0 {
        return format!("[{}: empty file]", path);
    }
    if start >= total {
        return format!(
            "[{}: {} lines; offset {} is past end of file]",
            path,
            total,
            start + 1
        );
    }

    let lim = match limit {
        Some(l) => l,
        None => env_read_lines,
    };
    let end = if lim <= 0 {
        total
    } else {
        (start + lim as usize).min(total)
    };

    let mut body = String::new();
    for (idx, line) in all.iter().enumerate().skip(start).take(end - start) {
        body.push_str(&format!("{:>6}\t{}\n", idx + 1, line));
    }

    if start > 0 || end < total {
        format!(
            "[{}: lines {}-{} of {}; call read_file with offset/limit to page]\n{}",
            path,
            start + 1,
            end,
            total,
            body
        )
    } else {
        body
    }
}

// --- write_file --------------------------------------------------------------

/// Write (overwrite) a file. Returns a status string. Phase 4 adds the
/// approval prompt; for now the caller decides whether to call this.
pub fn write_file(path: &str, content: &str) -> String {
    match fs::write(path, content) {
        Ok(_) => format!("wrote {} bytes to {}", content.len(), path),
        Err(e) => format!("ERROR writing {}: {}", path, e),
    }
}

// --- edit_file ---------------------------------------------------------------

static LINE_NUM_PREFIX: Lazy<Regex> = Lazy::new(|| Regex::new(r"^ *\d+\t").unwrap());

/// Remove read_file's `<n>\t` line-number prefixes from a pasted block. Only
/// strips when EVERY non-empty line carries one — so ordinary edits, and code
/// that merely starts a line with digits, are never altered.
pub fn strip_line_numbers(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let nonempty: Vec<&&str> = lines.iter().filter(|l| !l.trim().is_empty()).collect();
    if nonempty.is_empty() || !nonempty.iter().all(|l| LINE_NUM_PREFIX.is_match(l)) {
        return text.to_string();
    }
    lines
        .iter()
        .map(|l| LINE_NUM_PREFIX.replace_all(l, "").to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Replace one exact occurrence of `old` with `new` in a file. If the exact
/// match fails, retries with line-number prefixes stripped (the model may have
/// pasted read_file's numbered output).
pub fn edit_file(path: &str, old: &str, new: &str) -> String {
    let src = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => return format!("ERROR reading {}: {}", path, e),
    };

    // Try exact match first.
    let mut old_str = old.to_string();
    let mut new_str = new.to_string();
    let count = src.matches(&old_str).count();
    if count != 1 {
        let stripped_old = strip_line_numbers(old);
        if stripped_old != old && src.matches(&stripped_old).count() == 1 {
            old_str = stripped_old;
            new_str = strip_line_numbers(new);
        }
    }
    let count = src.matches(&old_str).count();
    if count != 1 {
        return format!("ERROR: `old` matched {} times (need exactly 1)", count);
    }
    let result = src.replacen(&old_str, &new_str, 1);
    match fs::write(path, &result) {
        Ok(_) => format!("edited {}", path),
        Err(e) => format!("ERROR writing {}: {}", path, e),
    }
}

// --- list_dir ----------------------------------------------------------------

/// List a directory, sorted.
pub fn list_dir(path: &str) -> String {
    match fs::read_dir(path) {
        Ok(entries) => {
            let mut names: Vec<String> = entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect();
            names.sort();
            names.join("\n")
        }
        Err(e) => format!("ERROR listing {}: {}", path, e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tmp(name: &str, content: &str) -> String {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        fs::write(&path, content).unwrap();
        // Leak the temp dir so the file survives the test — return the path.
        // (tempfile::TempDir drops on scope exit; we keep the dir alive by
        // forgetting it.)
        std::mem::forget(dir);
        path.to_string_lossy().to_string()
    }

    #[test]
    fn small_file_numbered_no_header() {
        let path = write_tmp("small.txt", "alpha\nbeta\ngamma\n");
        let out = read_file(&path, None, None, 400);
        assert!(!out.starts_with('['));
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines,
            vec!["     1\talpha", "     2\tbeta", "     3\tgamma"]
        );
    }

    #[test]
    fn no_trailing_newline_preserved() {
        let path = write_tmp("nonl.txt", "one\ntwo");
        let out = read_file(&path, None, None, 400);
        assert_eq!(
            out.lines().collect::<Vec<_>>(),
            vec!["     1\tone", "     2\ttwo"]
        );
    }

    #[test]
    fn large_file_default_window_has_header() {
        let content: String = (1..=1000).map(|i| format!("line{}\n", i)).collect();
        let path = write_tmp("big.txt", &content);
        let out = read_file(&path, None, None, 400);
        assert!(out.starts_with(&format!("[{}: lines 1-400 of 1000;", path)));
        let body = out.split_once('\n').unwrap().1;
        let body_lines: Vec<&str> = body.lines().collect();
        assert_eq!(body_lines.len(), 400);
        assert_eq!(body_lines[0], "     1\tline1");
        assert_eq!(body_lines[399], "   400\tline400");
    }

    #[test]
    fn offset_and_limit_window() {
        let content: String = (1..=1000).map(|i| format!("line{}\n", i)).collect();
        let path = write_tmp("big2.txt", &content);
        let out = read_file(&path, Some(500), Some(3), 400);
        assert!(out.starts_with(&format!("[{}: lines 500-502 of 1000;", path)));
        let body_lines: Vec<&str> = out.split_once('\n').unwrap().1.lines().collect();
        assert_eq!(
            body_lines,
            vec!["   500\tline500", "   501\tline501", "   502\tline502"]
        );
    }

    #[test]
    fn limit_clamps_at_eof() {
        let content: String = (1..=10).map(|i| format!("line{}\n", i)).collect();
        let path = write_tmp("big3.txt", &content);
        let out = read_file(&path, Some(8), Some(100), 400);
        assert!(out.starts_with(&format!("[{}: lines 8-10 of 10;", path)));
        assert!(out.trim_end().ends_with("    10\tline10"));
    }

    #[test]
    fn offset_past_eof() {
        let path = write_tmp("short.txt", "a\nb\n");
        let out = read_file(&path, Some(99), None, 400);
        assert_eq!(
            out,
            format!("[{}: 2 lines; offset 99 is past end of file]", path)
        );
    }

    #[test]
    fn empty_file_clear_marker() {
        let path = write_tmp("empty.txt", "");
        assert_eq!(
            read_file(&path, None, None, 400),
            format!("[{}: empty file]", path)
        );
        assert_eq!(
            read_file(&path, Some(5), None, 400),
            format!("[{}: empty file]", path)
        );
    }

    #[test]
    fn limit_zero_reads_to_end() {
        let content: String = (1..=50).map(|i| format!("line{}\n", i)).collect();
        let path = write_tmp("big4.txt", &content);
        let out = read_file(&path, None, Some(0), 400);
        assert!(!out.starts_with('['));
        assert_eq!(out.lines().count(), 50);
    }

    #[test]
    fn line_number_matches_real_position() {
        let content: String = (1..=30).map(|i| format!("row{}\n", i)).collect();
        let path = write_tmp("map.txt", &content);
        let out = read_file(&path, Some(17), Some(1), 400);
        let body = out.split_once('\n').unwrap().1.trim_end();
        let (num, content) = body.split_once('\t').unwrap();
        assert_eq!(num.trim().parse::<usize>().unwrap(), 17);
        assert_eq!(content, "row17");
    }

    #[test]
    fn edit_file_strips_pasted_line_numbers() {
        let path = write_tmp(
            "edit1.py",
            "def foo():\n    return 1\n\n\ndef bar():\n    return 2\n",
        );
        let win = read_file(&path, Some(1), Some(2), 400);
        let old_block = win.split_once('\n').unwrap().1.trim_end();
        assert!(old_block.contains('\t') && old_block.trim_start().starts_with("1\t"));
        let new_block = "     1\tdef foo():\n     2\t    return 10";
        let res = edit_file(&path, old_block, new_block);
        assert_eq!(res, format!("edited {}", path));
        let src = fs::read_to_string(&path).unwrap();
        assert!(src.contains("return 10"));
        assert!(!src.contains('\t'), "line-number prefixes must not leak");
        assert!(src.contains("return 2"));
    }

    #[test]
    fn edit_file_plain_old_still_works() {
        let path = write_tmp("edit2.txt", "hello world\n");
        let res = edit_file(&path, "hello", "goodbye");
        assert_eq!(res, format!("edited {}", path));
        assert_eq!(fs::read_to_string(&path).unwrap(), "goodbye world\n");
    }

    #[test]
    fn edit_file_does_not_strip_real_numeric_content() {
        let path = write_tmp("data.tsv", "1\tapple\n2\tbanana\n3\tcherry\n");
        let res = edit_file(&path, "2\tbanana", "2\tBANANA");
        assert_eq!(res, format!("edited {}", path));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "1\tapple\n2\tBANANA\n3\tcherry\n"
        );
    }

    #[test]
    fn strip_only_when_whole_block_numbered() {
        assert_eq!(strip_line_numbers("     1\tfoo\nbar"), "     1\tfoo\nbar");
        assert_eq!(strip_line_numbers("     1\tfoo\n    22\tbar"), "foo\nbar");
        assert_eq!(strip_line_numbers("foo\nbar"), "foo\nbar");
    }

    #[test]
    fn write_file_roundtrip() {
        let path = write_tmp("w.txt", "");
        let res = write_file(&path, "hello\n");
        assert!(res.starts_with("wrote"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello\n");
    }

    #[test]
    fn list_dir_sorted() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("c.txt"), "").unwrap();
        fs::write(dir.path().join("a.txt"), "").unwrap();
        fs::write(dir.path().join("b.txt"), "").unwrap();
        let out = list_dir(dir.path().to_str().unwrap());
        assert_eq!(out, "a.txt\nb.txt\nc.txt");
    }
}
