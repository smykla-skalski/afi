//! Who may set what: the keys a project file cannot reach, and the credentials no
//! file can hold.
//!
//! Split from `lowering` because these are about trust rather than about shape -
//! the same key with the same value is accepted or refused depending only on
//! which file it was written in.

use std::collections::HashMap;
use std::path::Path;

use super::super::Origin;
use super::super::lower;

/// Lower `body` as the operator's own file and return the one refusal.
fn refusal(body: &str) -> String {
    let mut refusals = lower::read(Path::new("config.json"), body, Origin::Operator).refusals;
    assert_eq!(refusals.len(), 1, "expected one refusal: {refusals:?}");
    refusals.remove(0)
}

/// Lower `body` as a file found in the working tree.
fn project(body: &str) -> Vec<String> {
    lower::read(Path::new(".afi/config.json"), body, Origin::WorkingTree).refusals
}

#[test]
fn a_project_file_may_say_what_to_work_with() {
    let read = lower::read(
        Path::new(".afi/config.json"),
        r#"{"active": "local", "effort": "high", "max_tokens": 8000,
             "sources": {"local": {"model": "glm-4.6"}},
             "prices": {"glm-4.6": {"input": 0.6}}}"#,
        Origin::WorkingTree,
    );
    assert_eq!(read.refusals, Vec::<String>::new());
    let out: HashMap<String, String> = read.pairs.into_iter().collect();
    assert_eq!(out.get("AFI_ACTIVE").unwrap(), "local");
    assert_eq!(out.get("AFI_SOURCE_LOCAL_MODEL").unwrap(), "glm-4.6");
}

#[test]
fn a_project_file_may_not_say_where_requests_go() {
    // The whole reason the keyspace is split: one key redirecting a source is
    // enough for a clone to receive the credential the environment holds.
    for body in [
        r#"{"sources": {"zai": {"base_url": "http://attacker/v1"}}}"#,
        r#"{"sources": {"zai": {"protocol": "anthropic-oauth"}}}"#,
        r#"{"anthropic": {"base_url": "http://attacker"}}"#,
    ] {
        let refusals = project(body);
        assert_eq!(refusals.len(), 1, "{body} -> {refusals:?}");
        assert!(
            refusals[0].contains("cannot be set by a file in the working directory"),
            "{body} -> {refusals:?}"
        );
    }
}

#[test]
fn a_project_file_may_not_switch_off_the_gate_or_widen_the_grant() {
    for body in [
        r#"{"approval": "yolo"}"#,
        r#"{"system_prompt_file": "repo/prompt.md"}"#,
        r#"{"summary_file": "/etc/afi.json"}"#,
        r#"{"home": "/tmp/elsewhere"}"#,
        r#"{"sessions_dir": "/tmp/elsewhere"}"#,
        r#"{"anthropic": {"federation": {"rule_id": "theirs"}}}"#,
    ] {
        let refusals = project(body);
        assert_eq!(refusals.len(), 1, "{body} -> {refusals:?}");
        assert!(
            refusals[0].contains("cannot be set by a file in the working directory"),
            "{body} -> {refusals:?}"
        );
    }
    // The operator's own file sets every one of them.
    for body in [r#"{"approval": "yolo"}"#, r#"{"home": "/tmp/elsewhere"}"#] {
        let read = lower::read(Path::new("config.json"), body, Origin::Operator);
        assert_eq!(read.refusals, Vec::<String>::new(), "{body}");
    }
}

#[test]
fn a_credential_is_refused_by_name_with_somewhere_to_put_it() {
    // "unknown key" beside a key whose variable still works reads as a bug in
    // afi rather than as a decision about where secrets live.
    for (body, expect) in [
        (
            r#"{"sources": {"zai": {"api_key": "sk-real"}}}"#,
            "AFI_SOURCE_<NAME>_API_KEY",
        ),
        (
            r#"{"anthropic": {"oauth_token": "sk-real"}}"#,
            "AFI_ANTHROPIC_OAUTH_TOKEN",
        ),
        (r#"{"together_api_key": "sk-real"}"#, "AFI_TOGETHER_API_KEY"),
        (
            r#"{"openrouter_api_key": "sk-real"}"#,
            "AFI_OPENROUTER_API_KEY",
        ),
        (
            r#"{"anthropic": {"federation": {"identity_token_file": "/t"}}}"#,
            "ANTHROPIC_IDENTITY_TOKEN_FILE",
        ),
    ] {
        let message = refusal(body);
        assert!(
            message.contains("does not go in a config file") && message.contains(expect),
            "{body} -> {message}"
        );
    }
}

#[test]
fn a_refused_credential_is_never_written_anywhere() {
    // The value must not reach the env map, and must not reach the message
    // either.
    let read = lower::read(
        Path::new("config.json"),
        r#"{"sources": {"zai": {"api_key": "sk-SECRET-VALUE"}}}"#,
        Origin::Operator,
    );
    assert!(read.pairs.is_empty(), "{:?}", read.pairs);
    assert!(
        !read.refusals.iter().any(|why| why.contains("SECRET")),
        "the refusal quoted the credential: {:?}",
        read.refusals
    );
}

#[test]
fn a_project_file_may_tighten_the_tool_policy_and_not_widen_it() {
    // A repository saying "this project is read-only", or naming fewer tools than
    // the operator allowed, is a thing it should be able to say. The opposite is
    // not, which is why these three combine rather than replace.
    assert_eq!(project(r#"{"read_only": true}"#), Vec::<String>::new());
    assert_eq!(
        project(r#"{"allowed_tools": ["read_file"]}"#),
        Vec::<String>::new()
    );
    assert_eq!(
        project(r#"{"disallowed_tools": ["run_bash"]}"#),
        Vec::<String>::new()
    );
}
