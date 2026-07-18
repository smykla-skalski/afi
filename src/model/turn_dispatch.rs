//! Tool-call dispatch for a model turn: accumulator ordering, the per-tool
//! approval gate, and the loops that run structured and text-protocol tool
//! calls, pushing their results back into the message history.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::approval::ApprovalState;
use crate::model::ModelConfig;
use crate::risk::{RiskClassifier, confirm};
use crate::term::{OutputEvent, UserInterface};
use crate::tools;
use crate::tools::protocol::sanitize_tool_result;
use std::path::PathBuf;

/// Accumulates the streamed fragments of a single tool call.
#[derive(Default)]
pub(crate) struct ToolCallAccum {
    pub id: Option<String>,
    pub name: Option<String>,
    pub args: String,
}

/// Return the accumulated tool calls ordered by their stream index.
pub(crate) fn order_tool_calls(tcs: &HashMap<u32, ToolCallAccum>) -> Vec<ToolCallAccum> {
    let mut indices: Vec<u32> = tcs.keys().copied().collect();
    indices.sort_unstable();
    indices
        .into_iter()
        .filter_map(|i| {
            tcs.get(&i).map(|c| ToolCallAccum {
                id: c.id.clone(),
                name: c.name.clone(),
                args: c.args.clone(),
            })
        })
        .collect()
}

pub(crate) enum ToolDispatchResult {
    Ok(String),
    Escaped(String),
}

/// Whether a batch of tool calls ran to completion or was escaped by the user.
pub(crate) enum ToolRunOutcome {
    Ran,
    Escaped(String),
}

/// Bundles the parameters for dispatching a tool call.
pub(crate) struct DispatchArgs<'a> {
    pub approval: &'a ApprovalState,
    pub classifier: &'a dyn RiskClassifier,
    pub cwd: &'a Path,
    pub project_root: &'a Path,
    pub env: &'a HashMap<String, String>,
    pub config: &'a ModelConfig,
    pub cancel: &'a CancellationToken,
    pub ui: &'a mut dyn UserInterface,
}

pub(crate) fn dispatch_tool(
    name: &str,
    args: &Value,
    da: &mut DispatchArgs<'_>,
) -> ToolDispatchResult {
    let action = match name {
        "write_file" => format!(
            "write {} ({} bytes)",
            args.get("path").and_then(|p| p.as_str()).unwrap_or("?"),
            args.get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .len()
        ),
        "edit_file" => format!(
            "edit {}",
            args.get("path").and_then(|p| p.as_str()).unwrap_or("?")
        ),
        "read_file" => format!(
            "read {}",
            args.get("path").and_then(Value::as_str).unwrap_or("?")
        ),
        "list_dir" => format!(
            "list {}",
            args.get("path").and_then(Value::as_str).unwrap_or(".")
        ),
        "run_bash" => format!(
            "run: {}",
            args.get("command").and_then(|c| c.as_str()).unwrap_or("?")
        ),
        "wait_background" => format!(
            "wait for PID {}",
            args.get("pid").and_then(Value::as_u64).unwrap_or(0)
        ),
        _ => name.to_string(),
    };
    da.ui.emit(OutputEvent::ToolStarted {
        name: name.to_string(),
        action: display_action(&action),
    });
    if da.cancel.is_cancelled() {
        emit_tool_finished(da.ui, name, "cancelled");
        return ToolDispatchResult::Escaped(action);
    }

    if matches!(name, "write_file" | "edit_file" | "run_bash") {
        let decision = {
            let ui = RefCell::new(&mut *da.ui);
            let ask = |prompt: &str| ui.borrow_mut().approve(prompt);
            confirm(
                &action,
                da.approval,
                da.classifier,
                da.cwd,
                da.project_root,
                &ask,
            )
        };
        match decision {
            Ok(true) => {}
            Ok(false) => {
                emit_tool_finished(da.ui, name, "denied by user");
                return ToolDispatchResult::Ok("DENIED by user".to_string());
            }
            Err(error) => {
                emit_tool_finished(da.ui, name, "cancelled");
                return ToolDispatchResult::Escaped(error.0);
            }
        }
    }
    if da.cancel.is_cancelled() {
        emit_tool_finished(da.ui, name, "cancelled");
        return ToolDispatchResult::Escaped(action);
    }

    let result = run_tool(name, args, da);
    emit_tool_finished(da.ui, name, tool_summary(name, &result));
    ToolDispatchResult::Ok(result)
}

fn run_tool(name: &str, args: &Value, da: &mut DispatchArgs<'_>) -> String {
    match name {
        "read_file" => tools::read_file(
            args.get("path").and_then(|p| p.as_str()).unwrap_or(""),
            args.get("offset").and_then(Value::as_i64),
            args.get("limit").and_then(Value::as_i64),
            da.config.read_file_lines,
        ),
        "write_file" => tools::write_file(
            args.get("path").and_then(|p| p.as_str()).unwrap_or(""),
            args.get("content").and_then(|c| c.as_str()).unwrap_or(""),
        ),
        "edit_file" => tools::edit_file(
            args.get("path").and_then(|p| p.as_str()).unwrap_or(""),
            args.get("old").and_then(|o| o.as_str()).unwrap_or(""),
            args.get("new").and_then(|n| n.as_str()).unwrap_or(""),
        ),
        "list_dir" => tools::list_dir(args.get("path").and_then(|p| p.as_str()).unwrap_or(".")),
        "run_bash" => {
            let command = args.get("command").and_then(|c| c.as_str()).unwrap_or("");
            if command.is_empty() {
                return "ERROR: run_bash requires 'command'.".to_string();
            }
            let cancel = da.ui.start_activity("running command");
            let result = tools::bash::run_bash(
                command,
                args.get("timeout").and_then(Value::as_i64),
                da.env,
                &|| cancel.is_cancelled(),
            );
            da.ui.stop_activity();
            result
        }
        "wait_background" => {
            let pid = args
                .get("pid")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(0);
            let timeout = args.get("timeout").and_then(Value::as_i64).unwrap_or(0);
            let log_path = args
                .get("log_path")
                .and_then(|l| l.as_str())
                .map(PathBuf::from);
            let cancel = da.ui.start_activity("waiting for command");
            let result =
                tools::bash::wait_background(pid, log_path.as_deref(), timeout, da.env, &|| {
                    cancel.is_cancelled()
                });
            da.ui.stop_activity();
            result
        }
        "final_answer" => args
            .get("answer")
            .and_then(|a| a.as_str())
            .unwrap_or("")
            .to_string(),
        _ => format!("ERROR: unknown tool {name}"),
    }
}

fn display_action(action: &str) -> String {
    let mut display = String::new();
    for ch in action.chars().take(160) {
        display.push(if ch.is_control() { ' ' } else { ch });
    }
    if action.chars().count() > 160 {
        display.push('…');
    }
    display
}

fn tool_summary(name: &str, result: &str) -> &'static str {
    if result.starts_with("ERROR") {
        return "failed";
    }
    if result.contains("interrupted") || result.contains("CANCELLED") {
        return "interrupted";
    }
    match name {
        "read_file" => "read complete",
        "write_file" => "write complete",
        "edit_file" => "edit complete",
        "list_dir" => "listing complete",
        "run_bash" => "command complete",
        "wait_background" => "wait complete",
        "final_answer" => "answer complete",
        _ => "completed",
    }
}

fn emit_tool_finished(ui: &mut dyn UserInterface, name: &str, summary: &str) {
    ui.emit(OutputEvent::ToolFinished {
        name: name.to_string(),
        summary: summary.to_string(),
    });
}

/// Run structured (OpenAI-format) tool calls, pushing a tool message per call
/// and marking later calls SKIPPED once one is escaped.
pub(crate) fn dispatch_structured(
    messages: &mut Vec<Value>,
    ordered: &[ToolCallAccum],
    parsed_args: &[Value],
    da: &mut DispatchArgs<'_>,
    tool_result_chars: usize,
) -> ToolRunOutcome {
    for (idx, (c, args)) in ordered.iter().zip(parsed_args.iter()).enumerate() {
        let name = c.name.as_deref().unwrap_or("tool");
        match dispatch_tool(name, args, da) {
            ToolDispatchResult::Ok(result) => {
                let sanitized = sanitize_tool_result(&result, tool_result_chars);
                messages.push(json!({"role": "tool", "tool_call_id": c.id.clone().unwrap_or_default(), "content": sanitized}));
            }
            ToolDispatchResult::Escaped(action) => {
                messages.push(json!({"role": "tool", "tool_call_id": c.id.clone().unwrap_or_default(), "content": "CANCELLED by user (Esc)"}));
                for c2 in &ordered[idx + 1..] {
                    messages.push(json!({"role": "tool", "tool_call_id": c2.id.clone().unwrap_or_default(), "content": "SKIPPED"}));
                }
                return ToolRunOutcome::Escaped(action);
            }
        }
    }
    ToolRunOutcome::Ran
}

/// Run text-protocol tool calls, collecting observations into a single user
/// message. Stops at the first escaped call.
pub(crate) fn dispatch_text(
    messages: &mut Vec<Value>,
    calls: &[(String, Value)],
    da: &mut DispatchArgs<'_>,
) -> ToolRunOutcome {
    let mut observations: Vec<String> = Vec::new();
    let mut escaped: Option<String> = None;
    for (name, args) in calls {
        match dispatch_tool(name, args, da) {
            ToolDispatchResult::Ok(r) => {
                observations.push(format!("Observation ({name}): {r}"));
            }
            ToolDispatchResult::Escaped(action) => {
                escaped = Some(action);
                observations.push(format!("Observation ({name}): CANCELLED"));
                break;
            }
        }
    }
    messages.push(json!({"role": "user", "content": observations.join("\n")}));
    match escaped {
        Some(action) => ToolRunOutcome::Escaped(action),
        None => ToolRunOutcome::Ran,
    }
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn tool_summary_never_echoes_result_payload() {
        let secret = "TOP_SECRET=file contents";
        assert_eq!(tool_summary("read_file", secret), "read complete");
        assert!(!tool_summary("read_file", secret).contains("TOP_SECRET"));
    }

    #[test]
    fn cancellation_skips_current_and_remaining_writes() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first.txt");
        let second = temp.path().join("second.txt");
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
        let approval = ApprovalState {
            yolo: true,
            ..ApprovalState::default()
        };
        let classifier = HighDefaultClassifier;
        let config = ModelConfig::default();
        let env = HashMap::new();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut ui = TestUi;
        let mut da = DispatchArgs {
            approval: &approval,
            classifier: &classifier,
            cwd: temp.path(),
            project_root: temp.path(),
            env: &env,
            config: &config,
            cancel: &cancel,
            ui: &mut ui,
        };
        let mut messages = Vec::new();

        let outcome = dispatch_structured(&mut messages, &ordered, &parsed, &mut da, 1_000);

        assert!(matches!(outcome, ToolRunOutcome::Escaped(_)));
        assert!(!first.exists());
        assert!(!second.exists());
        assert_eq!(messages[0]["content"], "CANCELLED by user (Esc)");
        assert_eq!(messages[1]["content"], "SKIPPED");
    }
}
