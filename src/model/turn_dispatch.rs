//! Tool-call dispatch for a model turn: accumulator ordering, the per-tool
//! approval gate, and the loops that run structured and text-protocol tool
//! calls, pushing their results back into the message history.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{json, Value};

use crate::term::approve::prompt_choice;

use crate::approval::ApprovalState;
use crate::model::ModelConfig;
use crate::risk::{confirm, RiskClassifier};
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
}

pub(crate) fn dispatch_tool(name: &str, args: &Value, da: &DispatchArgs<'_>) -> ToolDispatchResult {
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
        "run_bash" => format!(
            "run: {}",
            args.get("command").and_then(|c| c.as_str()).unwrap_or("?")
        ),
        _ => name.to_string(),
    };

    if matches!(name, "write_file" | "edit_file" | "run_bash") {
        let ask = prompt_choice;
        match confirm(
            &action,
            da.approval,
            da.classifier,
            da.cwd,
            da.project_root,
            &ask,
        ) {
            Ok(true) => {}
            Ok(false) => return ToolDispatchResult::Ok("DENIED by user".to_string()),
            Err(e) => return ToolDispatchResult::Escaped(e.0),
        }
    }

    let result = match name {
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
                return ToolDispatchResult::Ok("ERROR: run_bash requires 'command'.".to_string());
            }
            tools::bash::run_bash(
                command,
                args.get("timeout").and_then(Value::as_i64),
                da.env,
                &|| false,
            )
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
            tools::bash::wait_background(pid, log_path.as_deref(), timeout, da.env, &|| false)
        }
        "final_answer" => {
            return ToolDispatchResult::Ok(
                args.get("answer")
                    .and_then(|a| a.as_str())
                    .unwrap_or("")
                    .to_string(),
            )
        }
        _ => format!("ERROR: unknown tool {name}"),
    };
    ToolDispatchResult::Ok(result)
}

/// Run structured (OpenAI-format) tool calls, pushing a tool message per call
/// and marking later calls SKIPPED once one is escaped.
pub(crate) fn dispatch_structured(
    messages: &mut Vec<Value>,
    ordered: &[ToolCallAccum],
    parsed_args: &[Value],
    da: &DispatchArgs<'_>,
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
    da: &DispatchArgs<'_>,
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
