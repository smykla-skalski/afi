//! Slash-command dispatch for the REPL. `handle_slash_command` is a thin
//! dispatcher; each command is a small `cmd_*` handler.

use std::collections::HashMap;

use serde_json::{json, Value};
use tokio::runtime::Runtime as TokioRuntime;

use super::{banner, run_turn_loop, TurnParams, CYAN, DIM, GREEN, MAGENTA, RED, RESET, YELLOW};
use crate::approval::{apply_approval, approval_display, normalize_approval};
use crate::config::{Runtime, Source};
use crate::memory::{list_memories, remember_memories};
use crate::model::client::{ChatClient, ReqwestClient};
use crate::model::compress::COMPRESS_KEEP;
use crate::model::recovery::MANUAL_RECOVERY_NUDGE;
use crate::model::ModelConfig;
use crate::prompt::SYSTEM;
use crate::sessions::{self, new_session_id, resolve_session};

/// The result of evaluating a slash command.
pub enum CommandResult {
    /// The command was handled; the REPL should continue.
    Continue,
    /// The command was handled; the REPL should exit.
    Quit,
    /// The input was not a slash command; it should be sent to the model.
    NotACommand,
}

/// Dispatch a slash command. Returns `CommandResult::Quit` for `/quit`,
/// `CommandResult::Continue` for handled commands, `CommandResult::NotACommand`
/// for non-slash input.
pub(crate) fn handle_slash_command(
    input: &str,
    rt: &mut Runtime,
    messages: &mut Vec<Value>,
    session_id: &mut String,
    _history: &mut [String],
    env: &HashMap<String, String>,
) -> CommandResult {
    let input = input.trim();
    if !input.starts_with('/') {
        return CommandResult::NotACommand;
    }

    let parts: Vec<&str> = input.splitn(2, ' ').collect();
    let cmd = parts[0];
    let arg = parts.get(1).map_or("", |s| s.trim());

    match cmd {
        "/quit" | "/exit" => CommandResult::Quit,
        "/reset" | "/clear" | "/new" => cmd_reset(messages, session_id),
        "/yolo" => cmd_yolo(rt),
        "/approval" => cmd_approval(rt, arg),
        "/source" => cmd_source(rt, arg),
        "/compress" | "/compact" => cmd_compress(rt, messages),
        "/save" => cmd_save(messages, session_id, arg, env),
        "/sessions" => cmd_sessions(session_id, env),
        "/delete" => cmd_delete(session_id, arg, env),
        "/memory" => cmd_memory(arg, env),
        "/recover" => cmd_recover(rt, messages, arg, env),
        "/autocompress" => cmd_autocompress(arg, env),
        "/provider" => cmd_provider(rt, arg),
        "/help" => print_help(),
        _ => {
            println!("{YELLOW}  unknown command {cmd:?} (try /help){RESET}");
            CommandResult::Continue
        }
    }
}

fn cmd_reset(messages: &mut Vec<Value>, session_id: &mut String) -> CommandResult {
    *messages = vec![json!({"role": "system", "content": SYSTEM})];
    *session_id = new_session_id();
    println!("{DIM}  ↻ started a fresh session ({session_id}){RESET}");
    CommandResult::Continue
}

fn cmd_yolo(rt: &mut Runtime) -> CommandResult {
    rt.approval.yolo = !rt.approval.yolo;
    if rt.approval.yolo {
        rt.approval.approve_level = None;
        println!("{GREEN}  yolo mode on{RESET}");
    } else {
        println!("{DIM}  yolo mode off{RESET}");
    }
    println!("{}", banner(rt));
    CommandResult::Continue
}

fn cmd_approval(rt: &mut Runtime, arg: &str) -> CommandResult {
    if arg.is_empty() {
        let display = if rt.approval.yolo {
            "off (yolo)".to_string()
        } else {
            match rt.approval.approve_level {
                None => "all".to_string(),
                Some(l) => l.to_string(),
            }
        };
        println!("{DIM}  approval: {display}{RESET}");
    } else if let Some(kind) = normalize_approval(arg) {
        apply_approval(&mut rt.approval, kind, true);
        println!(
            "{DIM}  approval set to: {}{RESET}",
            approval_display(&rt.approval)
        );
    } else {
        println!("{RED}  unknown approval level {arg:?} (want all|low|medium|high|yolo){RESET}");
    }
    CommandResult::Continue
}

fn cmd_source(rt: &mut Runtime, arg: &str) -> CommandResult {
    if arg.is_empty() {
        for name in &rt.source_order {
            let src = &rt.sources[name];
            let active_mark = if rt.active.as_deref() == Some(name.as_str()) {
                format!("{GREEN}●{RESET} ")
            } else {
                format!("{DIM}○{RESET} ")
            };
            let model = src.display_model();
            println!(
                "  {active_mark}{MAGENTA}{name}{RESET} {CYAN}{model}{RESET} {DIM}{}{RESET}",
                src.base_url
            );
        }
        return CommandResult::Continue;
    }
    let source_parts: Vec<&str> = arg.split_whitespace().collect();
    let name = source_parts[0];
    let model_override = source_parts.get(1).map(ToString::to_string);
    if rt.switch_source(name, model_override.as_deref()) {
        println!("{DIM}  → switched to {name}{RESET}");
        println!("{}", banner(rt));
    } else {
        println!("{RED}  ✗ unknown source {name:?}{RESET}");
    }
    CommandResult::Continue
}

fn cmd_compress(rt: &Runtime, messages: &mut Vec<Value>) -> CommandResult {
    let body_len = messages.len().saturating_sub(1);
    if body_len <= COMPRESS_KEEP {
        println!("{DIM}  nothing to compress (too few turns){RESET}");
        return CommandResult::Continue;
    }
    println!("{DIM}  compressing context...{RESET}");
    let (Some(source), Some(model)) = (rt.active_source(), rt.model.as_ref()) else {
        eprintln!("{RED}  no active source{RESET}");
        return CommandResult::Continue;
    };
    if let Some(summary) = request_compression(source, model) {
        apply_compression(messages, &summary);
        println!("{DIM}  compressed context{RESET}");
    }
    CommandResult::Continue
}

/// Ask the model for a one-shot summary of the conversation so far.
fn request_compression(source: &Source, model: &str) -> Option<String> {
    let client = ReqwestClient::new();
    let runtime = TokioRuntime::new().expect("failed to create tokio runtime");
    let extra_body = source.extra_request_kwargs();
    runtime.block_on(async {
        let prompt = "Summarize the following conversation history for context retention.";
        match client
            .chat_completions(
                source,
                model,
                &[json!({"role": "user", "content": prompt})],
                30,
                extra_body.as_ref(),
            )
            .await
        {
            Ok(text) => parse_completion_content(&text),
            Err(e) => {
                eprintln!("{RED}  compress failed: {e}{RESET}");
                None
            }
        }
    })
}

/// Pull `choices[0].message.content` out of a chat-completions JSON response.
fn parse_completion_content(text: &str) -> Option<String> {
    let v: Value = serde_json::from_str(text).ok()?;
    v.get("choices")
        .and_then(|c| c.as_array())
        .and_then(|c| c.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(ToString::to_string)
}

/// Replace all but the last `COMPRESS_KEEP` turns with a summary user turn.
fn apply_compression(messages: &mut Vec<Value>, summary: &str) {
    let header = format!(
        "[Compressed context - earlier turns summarized; last {COMPRESS_KEEP} turns kept verbatim]"
    );
    let has_sys = messages
        .first()
        .is_some_and(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"));
    let split = messages.len().saturating_sub(COMPRESS_KEEP);
    let mut new_msgs = Vec::new();
    if has_sys {
        new_msgs.push(messages[0].clone());
    }
    new_msgs.push(json!({"role": "user", "content": format!("{header}\n\n{summary}")}));
    new_msgs.extend(messages[split..].iter().cloned());
    *messages = new_msgs;
}

fn cmd_save(
    messages: &mut Vec<Value>,
    session_id: &str,
    arg: &str,
    env: &HashMap<String, String>,
) -> CommandResult {
    let dir = sessions::sessions_dir(env);
    let meta = if arg.is_empty() {
        json!({})
    } else {
        json!({ "title": arg })
    };
    let _ = sessions::write_session(&dir, session_id, messages, Some(&meta));
    println!("{DIM}  saved session {session_id}{RESET}");
    CommandResult::Continue
}

fn cmd_sessions(session_id: &str, env: &HashMap<String, String>) -> CommandResult {
    let dir = sessions::sessions_dir(env);
    let sessions_list = sessions::list_sessions(&dir, Some(15), 0, None);
    if sessions_list.is_empty() {
        println!("{DIM}  no saved sessions{RESET}");
        return CommandResult::Continue;
    }
    for (i, s) in sessions_list.iter().enumerate() {
        let mark = if s.id == *session_id {
            format!("{GREEN}●{RESET}")
        } else {
            format!("{DIM}{}{RESET}", i + 1)
        };
        println!("  {mark} {MAGENTA}{}{RESET} {}", s.id, s.title);
    }
    CommandResult::Continue
}

fn cmd_delete(session_id: &str, arg: &str, env: &HashMap<String, String>) -> CommandResult {
    if arg.is_empty() {
        println!("{YELLOW}  usage: /delete <n|id>{RESET}");
        return CommandResult::Continue;
    }
    let dir = sessions::sessions_dir(env);
    let sessions_list = sessions::list_sessions(&dir, Some(50), 0, None);
    let Some(sid) = resolve_session(arg, &sessions_list) else {
        println!("{YELLOW}  no session matching {arg:?}{RESET}");
        return CommandResult::Continue;
    };
    if sid == *session_id {
        println!("{YELLOW}  cannot delete the current session{RESET}");
    } else if sessions::delete_session(&dir, &sid) {
        println!("{DIM}  deleted session {sid}{RESET}");
    } else {
        println!("{RED}  ✗ could not delete session {sid}{RESET}");
    }
    CommandResult::Continue
}

fn cmd_memory(arg: &str, env: &HashMap<String, String>) -> CommandResult {
    let sub_parts: Vec<&str> = arg.splitn(2, ' ').collect();
    match sub_parts.first().copied() {
        Some("list") => memory_list(env),
        Some("remember") => memory_remember(sub_parts.get(1).copied().unwrap_or(""), env),
        Some("save") => println!("{DIM}  (memory save requires a live model connection){RESET}"),
        _ => print_memory_usage(),
    }
    CommandResult::Continue
}

fn memory_list(env: &HashMap<String, String>) {
    let memories = list_memories(env);
    if memories.is_empty() {
        println!("{DIM}  no saved memories{RESET}");
        return;
    }
    for (name, title) in &memories {
        println!("  {MAGENTA}{name}{RESET} {DIM}{title}{RESET}");
    }
}

fn memory_remember(query: &str, env: &HashMap<String, String>) {
    if query.is_empty() {
        println!("{YELLOW}  usage: /memory remember <query>{RESET}");
        return;
    }
    let results = remember_memories(env, query);
    if results.is_empty() {
        println!("{DIM}  no memories matching {query:?}{RESET}");
        return;
    }
    for (name, title, _) in &results {
        println!("  {MAGENTA}{name}{RESET} {DIM}{title}{RESET}");
    }
}

fn print_memory_usage() {
    println!("{DIM}  usage:{RESET}");
    println!("{DIM}    /memory save [focus...]   save a memory{RESET}");
    println!("{DIM}    /memory remember <query>   search memories{RESET}");
    println!("{DIM}    /memory list               list all memories{RESET}");
}

fn cmd_recover(
    rt: &Runtime,
    messages: &mut Vec<Value>,
    arg: &str,
    env: &HashMap<String, String>,
) -> CommandResult {
    let note = if arg.is_empty() {
        String::new()
    } else {
        format!(" - {arg}")
    };
    let nudge = format!("{MANUAL_RECOVERY_NUDGE}{note}");
    messages.push(json!({"role": "user", "content": format!("[Runtime note: {nudge}]")}));
    let (Some(source), Some(model)) = (rt.active_source(), rt.model.as_ref()) else {
        eprintln!("{RED}  no active source{RESET}");
        return CommandResult::Continue;
    };
    let config = ModelConfig::from_env(env);
    run_turn_loop(
        messages,
        &TurnParams {
            config: &config,
            source,
            model,
            approval: &rt.approval,
            env,
            force_final: true,
            recovery_sampling: true,
        },
    );
    CommandResult::Continue
}

fn cmd_autocompress(arg: &str, env: &HashMap<String, String>) -> CommandResult {
    let config = ModelConfig::from_env(env);
    if arg.is_empty() {
        let pct = config.autocompress_percent;
        if pct == 0 {
            println!("{DIM}  auto-compress: off{RESET}");
        } else {
            println!("{DIM}  auto-compress: {pct}%{RESET}");
        }
    } else {
        println!("{DIM}  (autocompress setting updated){RESET}");
    }
    CommandResult::Continue
}

fn cmd_provider(rt: &Runtime, arg: &str) -> CommandResult {
    if arg.is_empty() {
        if let Some(src) = rt.active_source() {
            let order = src.provider_order();
            if order.is_empty() {
                println!("{DIM}  no provider routing set{RESET}");
            } else {
                println!("{DIM}  provider order: {}{RESET}", order.join(", "));
            }
        }
    } else {
        println!("{DIM}  (provider routing updated){RESET}");
    }
    CommandResult::Continue
}

fn print_help() -> CommandResult {
    println!("{DIM}  commands:{RESET}");
    println!("{DIM}    /source [name] [model]  list/switch sources{RESET}");
    println!("{DIM}    /yolo                   toggle auto-approve{RESET}");
    println!("{DIM}    /approval [level]       show/set approval mode{RESET}");
    println!("{DIM}    /sessions               list saved sessions{RESET}");
    println!("{DIM}    /save [title]           save current session{RESET}");
    println!("{DIM}    /delete [target]        delete a session{RESET}");
    println!("{DIM}    /compress               compress context{RESET}");
    println!("{DIM}    /reset                  start fresh session{RESET}");
    println!("{DIM}    /memory save|remember|list  manage memories{RESET}");
    println!("{DIM}    /quit                   exit{RESET}");
    CommandResult::Continue
}
