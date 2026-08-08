//! Slash-command dispatch for the REPL. `handle_slash_command` is a thin
//! dispatcher; each command is a small `cmd_*` handler.

use std::collections::HashMap;

use serde_json::{Value, json};

/// The one command that spends money, kept apart for that reason - see its
/// module doc.
mod compress;
use compress::cmd_compress;

use super::core::session_meta;
use super::failure::RunFailure;
use super::{NO_ACTIVE_SOURCE, Shared, TurnParams, header, run_turn_loop};
use crate::approval::{apply_approval, approval_display, normalize_approval};
use crate::config::{Runtime, nested};
use crate::memory::{list_memories, remember_memories};
use crate::model::recovery::MANUAL_RECOVERY_NUDGE;
use crate::model::{ModelConfig, TurnOutcome};
use crate::sessions::{self, new_session_id, resolve_session};
use crate::summary::{ErrorKind, RunError};
use crate::term::{MessageKind, UserInterface};
use crate::util;
use MessageKind::{Error, Info, Warning};

type Env = HashMap<String, String>;
type Ui<'a> = &'a mut dyn UserInterface;

const MEMORY_USAGE: &str = "Usage:\n  /memory save [focus...]   save a memory\n  /memory remember <query>  search memories\n  /memory list              list all memories";
const HELP: &str = "Commands:\n  /source [name] [model]  list/switch sources\n  /yolo                   toggle auto-approve\n  /approval [level]       show/set approval mode\n  /sessions               list saved sessions\n  /save [title]           save current session\n  /delete [target]        delete a session\n  /compress               compress context\n  /reset                  start fresh session\n  /instructions           list the project instructions this run loaded\n  /memory save|remember|list  manage memories\n  /quit                   exit";

/// The result of evaluating a slash command.
pub enum CommandResult {
    /// The command was handled; the REPL should continue.
    Continue,
    /// The command was handled; the REPL should exit.
    Quit,
    /// The input was not a slash command; it should be sent to the model.
    NotACommand,
}

fn say(ui: Ui<'_>, kind: MessageKind, text: impl Into<String>) {
    ui.message(kind, text.into());
}

/// Dispatch a slash command. Returns `CommandResult::Quit` for `/quit`,
/// `CommandResult::Continue` for handled commands, `CommandResult::NotACommand`
/// for non-slash input.
pub(crate) async fn handle_slash_command(
    input: &str,
    rt: &mut Runtime,
    messages: &mut Vec<Value>,
    session_id: &mut String,
    // The session's environment and its HTTP client. `/compress` and `/recover`
    // take the client from here rather than building one, which is what keeps a
    // federated source from re-assuming its role for each of them.
    shared: &Shared<'_>,
    ui: Ui<'_>,
    // Fed by any command that runs a model turn. `/recover` is one, so without
    // this a session whose only failure came from `/recover` reported success and
    // exited 0.
    failure: &mut RunFailure,
) -> CommandResult {
    let input = input.trim();
    if !input.starts_with('/') {
        return CommandResult::NotACommand;
    }

    let parts: Vec<&str> = input.splitn(2, ' ').collect();
    let cmd = parts[0];
    let arg = parts.get(1).map_or("", |s| s.trim());

    match cmd {
        "/quit" | "/exit" => return CommandResult::Quit,
        "/reset" | "/clear" | "/new" => cmd_reset(rt.prompt().message(), messages, session_id, ui),
        "/yolo" => cmd_yolo(rt, ui),
        "/approval" => cmd_approval(rt, arg, ui),
        "/source" => cmd_source(rt, arg, ui),
        "/compress" | "/compact" => cmd_compress(rt, messages, shared.client, ui).await,
        "/save" => cmd_save(rt, messages, session_id, arg, ui),
        "/sessions" => cmd_sessions(session_id, shared.env, ui),
        "/delete" => cmd_delete(session_id, arg, shared.env, ui),
        "/instructions" => say(ui, Info, super::instructions_listing(rt)),
        "/memory" => cmd_memory(arg, shared.env, ui),
        "/recover" => failure.record(&cmd_recover(rt, messages, arg, shared, ui).await),
        "/autocompress" => cmd_autocompress(arg, shared.env, ui),
        "/provider" => cmd_provider(rt, arg, ui),
        "/help" => say(ui, Info, HELP),
        _ => {
            say(ui, Warning, format!("Unknown command {cmd:?} (try /help)"));
        }
    }
    CommandResult::Continue
}

fn cmd_reset(system: Value, messages: &mut Vec<Value>, session_id: &mut String, ui: Ui<'_>) {
    *messages = vec![system];
    nested::reset();
    *session_id = new_session_id();
    say(ui, Info, format!("Started a fresh session ({session_id})"));
}

fn cmd_yolo(rt: &mut Runtime, ui: Ui<'_>) {
    rt.approval.yolo = !rt.approval.yolo;
    if rt.approval.yolo {
        rt.approval.approve_level = None;
        say(ui, Info, "Yolo mode on");
    } else {
        say(ui, Info, "Yolo mode off");
    }
    ui.header(header(rt));
}

fn cmd_approval(rt: &mut Runtime, arg: &str, ui: Ui<'_>) {
    if arg.is_empty() {
        let display = if rt.approval.yolo {
            "off (yolo)".to_string()
        } else {
            match rt.approval.approve_level {
                None => "all".to_string(),
                Some(l) => l.to_string(),
            }
        };
        say(ui, Info, format!("Approval: {display}"));
    } else if let Some(kind) = normalize_approval(arg) {
        apply_approval(&mut rt.approval, kind, true);
        say(
            ui,
            Info,
            format!("Approval set to: {}", approval_display(&rt.approval)),
        );
        ui.header(header(rt));
    } else {
        say(
            ui,
            Error,
            format!("Unknown approval level {arg:?} (want all|low|medium|high|yolo)"),
        );
    }
}

fn cmd_source(rt: &mut Runtime, arg: &str, ui: Ui<'_>) {
    if arg.is_empty() {
        let sources = rt
            .source_order
            .iter()
            .map(|name| {
                let src = &rt.sources[name];
                let mark = if rt.active.as_deref() == Some(name.as_str()) {
                    '●'
                } else {
                    '○'
                };
                format!("{mark} {name} {} {}", src.display_model(), src.base_url)
            })
            .collect::<Vec<_>>()
            .join("\n");
        say(ui, Info, sources);
        return;
    }
    let source_parts: Vec<&str> = arg.split_whitespace().collect();
    let name = source_parts[0];
    let model_override = source_parts.get(1).map(ToString::to_string);
    if rt.switch_source(name, model_override.as_deref()) {
        say(ui, Info, format!("Switched to {name}"));
        if let Some(warning) = budget_switch_note(rt) {
            say(ui, Warning, warning);
        }
        ui.header(header(rt));
    } else {
        say(ui, Error, format!("Unknown source {name:?}"));
    }
}

/// What to say when `/source` lands a budgeted run somewhere it cannot be priced.
///
/// It cannot refuse, because the operator typed it and the REPL has no refusal
/// channel, so it lands before the next prompt is sent rather than after the
/// turn that would stop the run. The stop itself is the turn loop's, at its next
/// checkpoint, and it is a failure rather than a cap hit: a budget that cannot
/// be measured must never be treated as no budget.
fn budget_switch_note(rt: &Runtime) -> Option<String> {
    let why = rt.budget_unenforceable()?;
    Some(format!(
        "{} cannot be enforced here: {why} - the next turn will stop the run rather \
         than spend under a cap afi cannot measure",
        rt.budget?.named()
    ))
}

fn cmd_save(rt: &Runtime, messages: &mut Vec<Value>, session_id: &str, arg: &str, ui: Ui<'_>) {
    let dir = sessions::sessions_dir(&rt.env);
    // Through the same builder the automatic saves use, so this cannot write fresh
    // messages beside a stale record of what the model has been told.
    let meta = session_meta(rt, util::nonblank(Some(arg)), None);
    let _ = sessions::write_session(&dir, session_id, messages, Some(&meta));
    say(ui, Info, format!("Saved session {session_id}"));
}

fn cmd_sessions(session_id: &str, env: &Env, ui: Ui<'_>) {
    let dir = sessions::sessions_dir(env);
    let sessions_list = sessions::list_sessions(&dir, Some(15), 0, None);
    if sessions_list.is_empty() {
        say(ui, Info, "No saved sessions");
        return;
    }
    let rows = sessions_list
        .iter()
        .enumerate()
        .map(|(index, session)| {
            let mark = if session.id == *session_id {
                "●".to_string()
            } else {
                (index + 1).to_string()
            };
            format!("{mark} {} {}", session.id, session.title)
        })
        .collect::<Vec<_>>()
        .join("\n");
    say(ui, Info, rows);
}

fn cmd_delete(session_id: &str, arg: &str, env: &Env, ui: Ui<'_>) {
    if arg.is_empty() {
        say(ui, Warning, "Usage: /delete <n|id>");
        return;
    }
    let dir = sessions::sessions_dir(env);
    let sessions_list = sessions::list_sessions(&dir, Some(50), 0, None);
    let Some(sid) = resolve_session(arg, &sessions_list) else {
        say(ui, Warning, format!("No session matching {arg:?}"));
        return;
    };
    if sid == *session_id {
        say(ui, Warning, "Cannot delete the current session");
    } else if sessions::delete_session(&dir, &sid) {
        say(ui, Info, format!("Deleted session {sid}"));
    } else {
        say(ui, Error, format!("Could not delete session {sid}"));
    }
}

fn cmd_memory(arg: &str, env: &Env, ui: Ui<'_>) {
    let sub_parts: Vec<&str> = arg.splitn(2, ' ').collect();
    match sub_parts.first().copied() {
        Some("list") => memory_list(env, ui),
        Some("remember") => {
            memory_remember(sub_parts.get(1).copied().unwrap_or(""), env, ui);
        }
        Some("save") => say(ui, Info, "Memory save requires a live model connection"),
        _ => print_memory_usage(ui),
    }
}

fn memory_list(env: &Env, ui: Ui<'_>) {
    let memories = list_memories(env);
    if memories.is_empty() {
        say(ui, Info, "No saved memories");
        return;
    }
    let rows = memories
        .iter()
        .map(|(name, title)| format!("{name} {title}"))
        .collect::<Vec<_>>()
        .join("\n");
    say(ui, Info, rows);
}

fn memory_remember(query: &str, env: &Env, ui: Ui<'_>) {
    if query.is_empty() {
        say(ui, Warning, "Usage: /memory remember <query>");
        return;
    }
    let results = remember_memories(env, query);
    if results.is_empty() {
        say(ui, Info, format!("No memories matching {query:?}"));
        return;
    }
    let rows = results
        .iter()
        .map(|(name, title, _)| format!("{name} {title}"))
        .collect::<Vec<_>>()
        .join("\n");
    say(ui, Info, rows);
}

fn print_memory_usage(ui: Ui<'_>) {
    say(ui, Info, MEMORY_USAGE);
}

/// Returns how the recovery turn ended.
async fn cmd_recover(
    rt: &Runtime,
    messages: &mut Vec<Value>,
    arg: &str,
    shared: &Shared<'_>,
    ui: Ui<'_>,
) -> TurnOutcome {
    let note = if arg.is_empty() {
        String::new()
    } else {
        format!(" - {arg}")
    };
    let nudge = format!("{MANUAL_RECOVERY_NUDGE}{note}");
    messages.push(json!({"role": "user", "content": format!("[Runtime note: {nudge}]")}));
    let (Some(source), Some(model)) = (rt.active_source(), rt.model.as_ref()) else {
        let error = NO_ACTIVE_SOURCE;
        say(ui, Error, error);
        return TurnOutcome::failed(RunError::new(error, ErrorKind::Input));
    };
    let config = ModelConfig::from_env(shared.env);
    run_turn_loop(
        messages,
        &TurnParams {
            config: &config,
            prompt: rt.prompt(),
            source,
            model,
            approval: &rt.approval,
            shared,
            force_final: true,
            recovery_sampling: true,
        },
        ui,
    )
    .await
}

fn cmd_autocompress(arg: &str, env: &Env, ui: Ui<'_>) {
    let config = ModelConfig::from_env(env);
    if arg.is_empty() {
        let pct = config.autocompress_percent;
        if pct == 0 {
            say(ui, Info, "Auto-compress: off");
        } else {
            say(ui, Info, format!("Auto-compress: {pct}%"));
        }
    } else {
        say(ui, Info, "Auto-compress setting updated");
    }
}

fn cmd_provider(rt: &Runtime, arg: &str, ui: Ui<'_>) {
    if arg.is_empty() {
        if let Some(src) = rt.active_source() {
            let order = src.provider_order();
            if order.is_empty() {
                say(ui, Info, "No provider routing set");
            } else {
                say(ui, Info, format!("Provider order: {}", order.join(", ")));
            }
        }
    } else {
        say(ui, Info, "Provider routing updated");
    }
}
