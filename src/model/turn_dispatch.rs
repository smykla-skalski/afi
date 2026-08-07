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
use crate::model::usage_totals;
use crate::risk::{RiskClassifier, confirm};
use crate::term::{OutputEvent, UserInterface};
use crate::tools;
use crate::tools::policy::ToolPolicy;
use crate::tools::policy::is_mutating;
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

    // Second enforcement point. The request already withholds blocked schemas,
    // but the text protocol parses calls out of prose, so a model can still name
    // a tool it was never offered. Runs before the approval gate and before any
    // side effect.
    if let Some(refusal) = policy_refusal(name, &da.config.tool_policy) {
        emit_tool_finished(da.ui, name, "blocked by policy");
        // Counted for the run summary. Announcing it in the terminal is no use to
        // a job that reads the JSON, and a refused write is exactly what a caller
        // reviewing untrusted input wants to know happened.
        usage_totals::record_policy_refusal();
        return ToolDispatchResult::Ok(refusal);
    }

    if is_mutating(name) {
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
                // A refusal too: the call never ran and the model was told no. The
                // reason differs from a policy block, so the count does too.
                usage_totals::record_approval_denial();
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

/// The tool result to hand back when policy blocks `name`, or `None` when it is
/// permitted.
///
/// Phrased as a permanent refusal listing the alternatives, because a model told
/// only "denied" reasonably retries - and a retry loop against a fixed policy
/// burns the whole turn budget.
fn policy_refusal(name: &str, policy: &ToolPolicy) -> Option<String> {
    if policy.permits(name) {
        return None;
    }
    let permitted = policy.permitted();
    let available = if permitted.is_empty() {
        "none".to_string()
    } else {
        permitted.join(", ")
    };
    Some(format!(
        "ERROR: tool '{name}' is blocked by this run's tool policy and will stay \
         blocked. Do not retry it. Permitted tools: {available}."
    ))
}

/// Count the policy refusals in a batch of tool calls that was thrown away before
/// dispatch could rule on it.
///
/// Two paths drop a batch: arguments that would not parse, and a forced-final turn
/// that answered with a tool. Neither reaches the gate in `dispatch_tool`, so a
/// `--read-only` run whose model asked to write reported a clean zero exactly when
/// the request was malformed enough to sidestep dispatch. The policy reads names
/// only, so its answer is already known here without the arguments that failed to
/// parse. The names are resolved the way the dispatcher resolves them, so the two
/// agree on what an unnamed call is, and `final_answer` is always permitted and so
/// never counted.
///
/// Counts every blocked call in the batch, including one whose own arguments were
/// fine, because the whole batch is gone. A retried batch is a fresh model output
/// and counts again: the alternative is a stream that keeps arriving malformed
/// ending on a clean zero, which is the failure this exists to prevent.
pub(crate) fn count_discarded_refusals(ordered: &[ToolCallAccum], policy: &ToolPolicy) {
    for call in ordered {
        // `permits` rather than `policy_refusal`: the same verdict, without building
        // the sentence that would have gone back to a model this batch never reaches.
        if !policy.permits(call.name.as_deref().unwrap_or("tool")) {
            usage_totals::record_policy_refusal();
        }
    }
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
mod tests;
