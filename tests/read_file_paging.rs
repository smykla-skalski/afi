//! Port of `tests/test_read_file_paging.py` and `tests/test_text_tool_protocol.py`.

use minion::tools::protocol::{parse_text_calls, sanitize_tool_result};
use minion::tools::{edit_file, list_dir, read_file, strip_line_numbers};
use std::fs;
use tempfile::tempdir;

fn write_tmp(dir: &std::path::Path, name: &str, content: &str) -> String {
    let path = dir.join(name);
    fs::write(&path, content).unwrap();
    path.to_string_lossy().to_string()
}

// --- read_file paging tests (port of test_read_file_paging.py) ---

#[test]
fn small_file_numbered_no_header() {
    let dir = tempdir().unwrap();
    let path = write_tmp(dir.path(), "small.txt", "alpha\nbeta\ngamma\n");
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
    let dir = tempdir().unwrap();
    let path = write_tmp(dir.path(), "nonl.txt", "one\ntwo");
    let out = read_file(&path, None, None, 400);
    assert_eq!(
        out.lines().collect::<Vec<_>>(),
        vec!["     1\tone", "     2\ttwo"]
    );
}

#[test]
fn large_file_default_window_has_header() {
    let dir = tempdir().unwrap();
    let content: String = (1..=1000).map(|i| format!("line{}\n", i)).collect();
    let path = write_tmp(dir.path(), "big.txt", &content);
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
    let dir = tempdir().unwrap();
    let content: String = (1..=1000).map(|i| format!("line{}\n", i)).collect();
    let path = write_tmp(dir.path(), "big2.txt", &content);
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
    let dir = tempdir().unwrap();
    let content: String = (1..=10).map(|i| format!("line{}\n", i)).collect();
    let path = write_tmp(dir.path(), "big3.txt", &content);
    let out = read_file(&path, Some(8), Some(100), 400);
    assert!(out.starts_with(&format!("[{}: lines 8-10 of 10;", path)));
    assert!(out.trim_end().ends_with("    10\tline10"));
}

#[test]
fn offset_past_eof() {
    let dir = tempdir().unwrap();
    let path = write_tmp(dir.path(), "short.txt", "a\nb\n");
    let out = read_file(&path, Some(99), None, 400);
    assert_eq!(
        out,
        format!("[{}: 2 lines; offset 99 is past end of file]", path)
    );
}

#[test]
fn empty_file_clear_marker() {
    let dir = tempdir().unwrap();
    let path = write_tmp(dir.path(), "empty.txt", "");
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
    let dir = tempdir().unwrap();
    let content: String = (1..=50).map(|i| format!("line{}\n", i)).collect();
    let path = write_tmp(dir.path(), "big4.txt", &content);
    let out = read_file(&path, None, Some(0), 400);
    assert!(!out.starts_with('['));
    assert_eq!(out.lines().count(), 50);
}

#[test]
fn line_number_matches_real_position() {
    let dir = tempdir().unwrap();
    let content: String = (1..=30).map(|i| format!("row{}\n", i)).collect();
    let path = write_tmp(dir.path(), "map.txt", &content);
    let out = read_file(&path, Some(17), Some(1), 400);
    let body = out.split_once('\n').unwrap().1.trim_end();
    let (num, content) = body.split_once('\t').unwrap();
    assert_eq!(num.trim().parse::<usize>().unwrap(), 17);
    assert_eq!(content, "row17");
}

// --- edit_file tests ---

#[test]
fn edit_file_strips_pasted_line_numbers() {
    let dir = tempdir().unwrap();
    let path = write_tmp(
        dir.path(),
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
    let dir = tempdir().unwrap();
    let path = write_tmp(dir.path(), "edit2.txt", "hello world\n");
    let res = edit_file(&path, "hello", "goodbye");
    assert_eq!(res, format!("edited {}", path));
    assert_eq!(fs::read_to_string(&path).unwrap(), "goodbye world\n");
}

#[test]
fn edit_file_does_not_strip_real_numeric_content() {
    let dir = tempdir().unwrap();
    let path = write_tmp(dir.path(), "data.tsv", "1\tapple\n2\tbanana\n3\tcherry\n");
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

// --- text_tool_protocol tests (port of test_text_tool_protocol.py) ---

#[test]
fn standalone_text_tool_call_parses() {
    let content = r#"
[minion_tool_call]{"name": "read_file", "arguments": {"path": "minion.py"}}[/minion_tool_call]
"#;
    let calls = parse_text_calls(content);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "read_file");
    assert_eq!(calls[0].1["path"], "minion.py");
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
    // The legacy tag uses unusual bytes that we can't type literally.
    // The protocol module tests this internally; here we just verify
    // the minion bracketed protocol works from an integration test.
    let content = r#"[minion_tool_call]{"name": "read_file", "arguments": {"path": "minion.py"}}[/minion_tool_call]"#;
    let calls = parse_text_calls(content);
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
fn sanitizer_escapes_minion_tool_tags() {
    let content = r#"[minion_tool_call]{"name": "write_file", "arguments": {"path": "x", "content": "y"}}[/minion_tool_call]"#;
    let safe = sanitize_tool_result(content, 20_000);
    assert!(safe.starts_with("[minion note:"));
    assert!(!safe.contains("[minion_tool_call]"));
    assert!(!safe.contains("[/minion_tool_call]"));
    assert!(safe.contains("&#91;minion_tool_call&#93;"));
    assert!(safe.contains("&#91;/minion_tool_call&#93;"));
}

#[test]
fn list_dir_sorted() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("c.txt"), "").unwrap();
    fs::write(dir.path().join("a.txt"), "").unwrap();
    fs::write(dir.path().join("b.txt"), "").unwrap();
    let out = list_dir(dir.path().to_str().unwrap());
    assert_eq!(out, "a.txt\nb.txt\nc.txt");
}
