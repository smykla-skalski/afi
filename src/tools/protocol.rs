//! Text-protocol tool-call parsing and tool-result sanitization.
//!
//! When the server doesn't support native tool-calling, minion falls back to
//! parsing `[minion_tool_call]...[/minion_tool_call]` tags out of the model's
//! text. The legacy tag form is also recognized for backwards compat. Tags
//! inside fenced code blocks are literal text and never executed.

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;

use super::known_tool_names;

// Regexes are non-greedy on the JSON body so multiple calls per line parse.
static MINION_TOOL_TAG: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\[minion_tool_call\]\s*(\{.*?\})\s*\[/minion_tool_call\]").unwrap());

static TOOL_PROTOCOL_TAG_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"</?tool_call\b[^>]*>").unwrap());

const TOOL_RESULT_PROTOCOL_NOTE: &str =
    "[afi note: escaped tool-call protocol delimiters from this tool result \
     before sending it back to the model]\n";

/// The byte sequences for the legacy tool-call tag. The opener is three
/// unusual bytes and the closer is another three - the same convention as
/// the Python original. Kept as plain strings so the unusual bytes don't
/// appear literally in source.
const LEGACY_OPEN: &str = "\u{0001}\u{0001}\u{0001}";
const LEGACY_CLOSE: &str = "\u{0002}\u{0002}\u{0002}";

/// A parsed text-protocol tool call: `(name, arguments)`.
pub type TextCall = (String, Value);

/// Pull text-protocol tool-call messages from assistant content. Returns
/// `(name, arguments)` tuples for each tag whose name is a known tool and
/// whose JSON parses. Tags inside fenced code blocks are literal text.
pub fn parse_text_calls(content: &str) -> Vec<TextCall> {
    if !content.contains("[minion_tool_call]") && !content.contains(LEGACY_OPEN) {
        return vec![];
    }
    let mut calls: Vec<TextCall> = Vec::new();
    let mut in_fence = false;
    for line in content.split('\n') {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        scan_line(line, &MINION_TOOL_TAG, &mut calls);
        scan_line(line, legacy_tool_tag_regex(), &mut calls);
    }
    calls
}

fn scan_line(line: &str, re: &Regex, calls: &mut Vec<TextCall>) {
    let known: std::collections::HashSet<&str> = known_tool_names().iter().copied().collect();
    for m in re.captures_iter(line) {
        let json_str = match m.get(1) {
            Some(s) => s.as_str(),
            None => continue,
        };
        let obj: Value = match serde_json::from_str(json_str) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let name = match obj.get("name").and_then(|n| n.as_str()) {
            Some(n) => n,
            None => continue,
        };
        if !known.contains(name) {
            continue;
        }
        let args = obj.get("arguments").cloned().unwrap_or(Value::Null);
        calls.push((name.to_string(), args));
    }
}

/// Build the legacy tag regex at runtime so the unusual byte sequences don't
/// need to appear literally in source.
fn legacy_tool_tag_regex() -> &'static Regex {
    static LEGACY_RE: Lazy<Regex> = Lazy::new(|| {
        let pattern = format!(
            "{}\\s*(\\{{.*?\\}})\\s*{}",
            regex::escape(LEGACY_OPEN),
            regex::escape(LEGACY_CLOSE)
        );
        Regex::new(&pattern).unwrap()
    });
    &LEGACY_RE
}

/// Neutralize active tool-call delimiters in untrusted tool output.
///
/// Replaces the protocol delimiters with HTML-escaped forms so file contents
/// that happen to contain a tool-call-looking tag can't be echoed back as a
/// real call.
pub fn escape_tool_protocol_delimiters(text: &str) -> String {
    let lt = "\x26lt;";
    let gt = "\x26gt;";
    let safe = TOOL_PROTOCOL_TAG_RE.replace_all(text, |c: &regex::Captures<'_>| {
        c[0].replace('<', lt).replace('>', gt)
    });
    let safe = safe
        .replace("[minion_tool_call]", "\x26#91;minion_tool_call\x26#93;")
        .replace("[/minion_tool_call]", "\x26#91;/minion_tool_call\x26#93;")
        .replace(LEGACY_OPEN, "\x26lt;tool_call\x26gt;")
        .replace(LEGACY_CLOSE, "\x26lt;/tool_call\x26gt;");
    if safe != text {
        format!("{}{}", TOOL_RESULT_PROTOCOL_NOTE, safe)
    } else {
        safe
    }
}

/// Default per-tool-result char cap (`MINION_TOOL_RESULT_CHARS`, default 20000).
pub const TOOL_RESULT_CHARS_DEFAULT: usize = 20_000;

/// Neutralize active protocol delimiters, dedup runs of >=3 identical
/// consecutive lines, then head/tail-cap to `budget` chars with a visible
/// marker. Short, non-repetitive results pass through except for
/// protocol-delimiter escaping.
pub fn sanitize_tool_result(text: &str, budget: usize) -> String {
    let text = escape_tool_protocol_delimiters(text);
    if text.len() <= 1000 {
        return text;
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    let n = lines.len();
    while i < n {
        let mut j = i + 1;
        while j < n && lines[j] == lines[i] {
            j += 1;
        }
        let run = j - i;
        if run >= 3 {
            out.push(lines[i].to_string());
            out.push(format!("... [+{} identical lines elided]", run - 1));
        } else {
            out.extend(lines[i..j].iter().map(|l| l.to_string()));
        }
        i = j;
    }
    let mut result = out.join("\n");
    if budget > 0 && result.len() > budget {
        let head = budget * 2 / 3;
        let tail = budget - head;
        let elided = result.len() - head - tail;
        let (head_part, tail_part) = result.split_at(head);
        let tail_start = tail_part.len() - tail;
        let tail_part = &tail_part[tail_start..];
        result = format!(
            "{}\n... [{} chars elided to bound context; re-run more narrowly if you need the rest]\n{}",
            head_part, elided, tail_part
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn standalone_text_tool_call_parses() {
        let content = r#"
[minion_tool_call]{"name": "read_file", "arguments": {"path": "minion.py"}}[/minion_tool_call]
"#;
        let calls = parse_text_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "read_file");
        assert_eq!(calls[0].1, json!({"path": "minion.py"}));
    }

    #[test]
    fn multiple_standalone_text_tool_calls_parse() {
        let content = r#"[minion_tool_call]{"name": "list_dir", "arguments": {"path": "."}}[/minion_tool_call]
[minion_tool_call]{"name": "read_file", "arguments": {"path": "README.md"}}[/minion_tool_call]"#;
        let calls = parse_text_calls(content);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "list_dir");
        assert_eq!(calls[1].0, "read_file");
    }

    #[test]
    fn legacy_tool_call_tag_still_parses() {
        let content = format!(
            r#"{}{{"name": "read_file", "arguments": {{"path": "minion.py"}}}}{}"#,
            LEGACY_OPEN, LEGACY_CLOSE
        );
        let calls = parse_text_calls(&content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "read_file");
    }

    #[test]
    fn tool_call_inside_code_block_is_plain_text() {
        let content = r#"I found the system prompt:
```python
SYSTEM = """If your runtime does NOT support native tool calls, emit:
[minion_tool_call]{"name": "read_file", "arguments": {"path": "foo.py"}}[/minion_tool_call]
"""
```
"#;
        let calls = parse_text_calls(content);
        assert!(calls.is_empty());
    }

    #[test]
    fn tool_call_with_surrounding_prose_executes() {
        let content = r#"I'll start by exploring the current directory and checking the orca-cli tool.
[minion_tool_call]{"name": "read_file", "arguments": {"path": "foo.py"}}[/minion_tool_call]"#;
        let calls = parse_text_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "read_file");
    }

    #[test]
    fn tool_call_with_unknown_name_is_skipped() {
        let content = r#"Here is the literal protocol string: [minion_tool_call]{"name": "read_file", "arguments": {"path": "foo.py"}}[/minion_tool_call] [minion_tool_call]{"name": "not_a_real_tool", "arguments": {}}[/minion_tool_call]"#;
        let calls = parse_text_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "read_file");
    }

    #[test]
    fn multiline_prose_then_tool_call_executes() {
        let content = r#"I'll start by exploring the current directory to understand the context, and check what orca-cli is
[minion_tool_call]{"name": "run_bash", "arguments": {"command": "pwd && ls -la"}}[/minion_tool_call]"#;
        let calls = parse_text_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "run_bash");
    }

    #[test]
    fn sanitizer_escapes_legacy_tool_tags() {
        let content = format!(
            r#"line 1
{}{{"name": "write_file", "arguments": {{"path": "x", "content": "y"}}}}{}"#,
            LEGACY_OPEN, LEGACY_CLOSE
        );
        let safe = sanitize_tool_result(&content, 20_000);
        assert!(safe.starts_with("[afi note:"));
        assert!(!safe.contains(LEGACY_OPEN));
        assert!(!safe.contains(LEGACY_CLOSE));
        assert!(safe.contains("\x26lt;tool_call\x26gt;"));
        assert!(safe.contains("\x26lt;/tool_call\x26gt;"));
    }

    #[test]
    fn sanitizer_escapes_minion_tool_tags() {
        let content = r#"[minion_tool_call]{"name": "write_file", "arguments": {"path": "x", "content": "y"}}[/minion_tool_call]"#;
        let safe = sanitize_tool_result(content, 20_000);
        assert!(safe.starts_with("[afi note:"));
        assert!(!safe.contains("[minion_tool_call]"));
        assert!(!safe.contains("[/minion_tool_call]"));
        assert!(safe.contains("&#91;minion_tool_call&#93;"));
        assert!(safe.contains("&#91;/minion_tool_call&#93;"));
    }

    #[test]
    fn sanitizer_dedups_identical_runs() {
        // Input must exceed 1000 chars to reach the dedup path.
        let content = "x\n".repeat(501);
        let safe = sanitize_tool_result(&content, 20_000);
        assert!(safe.contains("... [+500 identical lines elided]"));
    }
}
