use std::fs;

use super::*;
use crate::risk::{ApprovalChoice, HighDefaultClassifier};

struct TestUi;

impl UserInterface for TestUi {
    fn emit(&mut self, _event: OutputEvent) {}

    fn start_activity(&mut self, _label: &str) -> CancellationToken {
        CancellationToken::new()
    }

    fn stop_activity(&mut self) {}

    fn approve(&mut self, _prompt: &str) -> ApprovalChoice {
        ApprovalChoice::Yes
    }
}

/// A dispatch context that approves everything, so a test that expects a call to
/// be refused can only be seeing the tool policy.
struct Harness {
    approval: ApprovalState,
    classifier: HighDefaultClassifier,
    config: ModelConfig,
    env: HashMap<String, String>,
    cancel: CancellationToken,
    ui: TestUi,
    temp: tempfile::TempDir,
}

impl Harness {
    fn new(config: ModelConfig) -> Self {
        Self {
            approval: ApprovalState {
                yolo: true,
                ..ApprovalState::default()
            },
            classifier: HighDefaultClassifier,
            config,
            env: HashMap::new(),
            cancel: CancellationToken::new(),
            ui: TestUi,
            temp: tempfile::tempdir().expect("tempdir"),
        }
    }

    /// A config whose policy comes from the two env vars, exercising the same
    /// resolution path a real run uses.
    fn with_policy(allowed: Option<&str>, denied: Option<&str>) -> Self {
        let mut env = HashMap::new();
        if let Some(value) = allowed {
            env.insert("AFI_ALLOWED_TOOLS".to_string(), value.to_string());
        }
        if let Some(value) = denied {
            env.insert("AFI_DISALLOWED_TOOLS".to_string(), value.to_string());
        }
        Self::new(ModelConfig::from_env(&env))
    }

    fn args(&mut self) -> DispatchArgs<'_> {
        DispatchArgs {
            approval: &self.approval,
            classifier: &self.classifier,
            cwd: self.temp.path(),
            project_root: self.temp.path(),
            env: &self.env,
            config: &self.config,
            cancel: &self.cancel,
            ui: &mut self.ui,
        }
    }
}

#[test]
fn tool_summary_never_echoes_result_payload() {
    let secret = "TOP_SECRET=file contents";
    assert_eq!(tool_summary("read_file", secret), "read complete");
    assert!(!tool_summary("read_file", secret).contains("TOP_SECRET"));
}

#[test]
fn cancellation_skips_current_and_remaining_writes() {
    let mut h = Harness::new(ModelConfig::default());
    let first = h.temp.path().join("first.txt");
    let second = h.temp.path().join("second.txt");
    let ordered = vec![
        ToolCallAccum {
            id: Some("one".to_string()),
            name: Some("write_file".to_string()),
            args: String::new(),
        },
        ToolCallAccum {
            id: Some("two".to_string()),
            name: Some("write_file".to_string()),
            args: String::new(),
        },
    ];
    let parsed = vec![
        json!({"path": first, "content": "one"}),
        json!({"path": second, "content": "two"}),
    ];
    h.cancel.cancel();
    let mut messages = Vec::new();

    let outcome = dispatch_structured(&mut messages, &ordered, &parsed, &mut h.args(), 1_000);

    assert!(matches!(outcome, ToolRunOutcome::Escaped(_)));
    assert!(!first.exists());
    assert!(!second.exists());
    assert_eq!(messages[0]["content"], "CANCELLED by user (Esc)");
    assert_eq!(messages[1]["content"], "SKIPPED");
}

// --- tool policy at dispatch ---------------------------------------------------

#[test]
fn a_blocked_write_never_touches_the_filesystem() {
    // The assertion that matters. Withholding the schema only discourages the
    // model; this is what stops a call that arrives anyway, and approval is on
    // yolo so nothing else could be refusing it.
    let mut h = Harness::with_policy(Some("read_file,list_dir"), None);
    let target = h.temp.path().join("should-not-exist.txt");

    let result = dispatch_tool(
        "write_file",
        &json!({"path": &target, "content": "written"}),
        &mut h.args(),
    );

    let ToolDispatchResult::Ok(message) = result else {
        panic!("a blocked tool reports back, it does not escape the turn");
    };
    assert!(message.starts_with("ERROR:"), "{message}");
    assert!(message.contains("write_file"), "{message}");
    assert!(
        !target.exists(),
        "the blocked write created the file anyway"
    );
}

#[test]
fn a_blocked_command_never_runs() {
    let mut h = Harness::with_policy(None, Some("run_bash"));
    let marker = h.temp.path().join("ran.txt");

    let result = dispatch_tool(
        "run_bash",
        &json!({"command": format!("touch {}", marker.display()), "timeout": 5}),
        &mut h.args(),
    );

    assert!(matches!(result, ToolDispatchResult::Ok(_)));
    assert!(!marker.exists(), "the blocked command executed anyway");
}

#[test]
fn a_permitted_tool_still_runs_under_a_policy() {
    let mut h = Harness::with_policy(Some("write_file"), None);
    let target = h.temp.path().join("allowed.txt");

    let result = dispatch_tool(
        "write_file",
        &json!({"path": &target, "content": "written"}),
        &mut h.args(),
    );

    assert!(matches!(result, ToolDispatchResult::Ok(_)));
    assert_eq!(fs::read_to_string(&target).expect("written"), "written");
}

#[test]
fn the_default_policy_leaves_dispatch_alone() {
    let mut h = Harness::new(ModelConfig::default());
    let target = h.temp.path().join("default.txt");

    dispatch_tool(
        "write_file",
        &json!({"path": &target, "content": "written"}),
        &mut h.args(),
    );

    assert!(target.exists(), "an unrestricted run must still write");
}

#[test]
fn a_policy_naming_an_unknown_tool_blocks_everything() {
    // Fail-closed backstop for a caller that skipped the startup check: a
    // mistyped deny entry must not leave the real tool reachable.
    let mut h = Harness::with_policy(None, Some("run_bsah"));
    let target = h.temp.path().join("typo.txt");

    dispatch_tool(
        "write_file",
        &json!({"path": &target, "content": "written"}),
        &mut h.args(),
    );

    assert!(!target.exists());
    assert!(!h.config.tool_policy.unknown_names().is_empty());
}

#[test]
fn the_refusal_names_the_alternatives_so_the_model_stops_retrying() {
    let mut h = Harness::with_policy(Some("read_file,list_dir"), None);

    let result = dispatch_tool("run_bash", &json!({"command": "true"}), &mut h.args());

    let ToolDispatchResult::Ok(message) = result else {
        panic!("blocked calls report back");
    };
    assert!(message.contains("read_file"), "{message}");
    assert!(message.contains("list_dir"), "{message}");
    assert!(message.contains("Do not retry"), "{message}");
}

#[test]
fn a_blocked_call_reports_back_so_the_turn_continues() {
    // A blocked call must land in history as a tool result, or the next request
    // carries a tool_use with no matching tool_result and Anthropic rejects it.
    let mut h = Harness::with_policy(Some("read_file"), None);
    let ordered = vec![ToolCallAccum {
        id: Some("call-1".to_string()),
        name: Some("write_file".to_string()),
        args: String::new(),
    }];
    let parsed = vec![json!({"path": h.temp.path().join("x"), "content": "x"})];
    let mut messages = Vec::new();

    let outcome = dispatch_structured(&mut messages, &ordered, &parsed, &mut h.args(), 1_000);

    assert!(matches!(outcome, ToolRunOutcome::Ran));
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], "tool");
    assert_eq!(messages[0]["tool_call_id"], "call-1");
    assert!(
        messages[0]["content"]
            .as_str()
            .expect("string content")
            .starts_with("ERROR:")
    );
}

#[test]
fn final_answer_survives_the_narrowest_policy() {
    let mut h = Harness::with_policy(Some("read_file"), None);

    let result = dispatch_tool("final_answer", &json!({"answer": "done"}), &mut h.args());

    let ToolDispatchResult::Ok(message) = result else {
        panic!("final_answer dispatches");
    };
    assert_eq!(message, "done");
}
