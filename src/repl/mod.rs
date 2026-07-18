//! The REPL: main loop, slash command dispatch, session auto-save, banner,
//! and one-shot mode. This is the top-level orchestrator that ties together
//! all the other modules (config, sessions, tools, model, risk, term).

mod commands;

pub use commands::CommandResult;
use commands::handle_slash_command;

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tokio::runtime::Runtime as TokioRuntime;

use crate::approval::{ApprovalState, Level};
use crate::cli::session_id_from_args;
use crate::config::{Runtime, Source};
use crate::log::log_event;
use crate::model::ModelConfig;
use crate::model::client::ReqwestClient;
use crate::model::turn::{LoopRequest, run_model_turn_loop};
use crate::prompt::SYSTEM;
use crate::risk::{HighDefaultClassifier, detect_project_root};
use crate::sessions::{self, new_session_id, safe_title};
use crate::term;

// ANSI codes for banner output.
pub const DIM: &str = "\x1b[2m";
pub const CYAN: &str = "\x1b[36m";
pub const GREEN: &str = "\x1b[32m";
pub const YELLOW: &str = "\x1b[33m";
pub const RED: &str = "\x1b[31m";
pub const MAGENTA: &str = "\x1b[35m";
pub const BOLD: &str = "\x1b[1m";
pub const RESET: &str = "\x1b[0m";

/// The startup / post-switch banner: model name, active source (when more
/// than one is configured), approval mode, and endpoint.
#[must_use]
pub fn banner(rt: &Runtime) -> String {
    let sep = format!("{DIM} \u{00b7} {RESET}");
    let model = rt.model.clone().unwrap_or_else(|| "auto".to_string());
    let mut parts = vec![format!("{BOLD}afi{RESET}"), format!("{CYAN}{model}{RESET}")];
    if rt.sources.len() > 1
        && let Some(active) = &rt.active
    {
        parts.push(format!("{MAGENTA}{active}{RESET}"));
    }
    if rt.approval.yolo {
        parts.push(format!("{GREEN}yolo{RESET}"));
    } else {
        match rt.approval.approve_level {
            None => parts.push(format!("{YELLOW}prompt:all{RESET}")),
            Some(Level::High) => parts.push(format!("{GREEN}auto:high{RESET}")),
            Some(Level::Medium) => parts.push(format!("{YELLOW}auto:medium{RESET}")),
            Some(Level::Low) => parts.push(format!("{DIM}auto:low{RESET}")),
        }
    }
    if let Some(src) = rt.active_source() {
        parts.push(format!("{DIM}{}{RESET}", src.base_url));
    }
    parts.join(&sep)
}

/// The turn inputs that vary between the main loop, one-shot mode, and
/// `/recover`. `run_turn_loop` supplies the client, classifier, and cwd.
pub(crate) struct TurnParams<'a> {
    pub config: &'a ModelConfig,
    pub source: &'a Source,
    pub model: &'a str,
    pub approval: &'a ApprovalState,
    pub env: &'a HashMap<String, String>,
    pub force_final: bool,
    pub recovery_sampling: bool,
}

/// Run one model turn loop to completion on a fresh blocking Tokio runtime.
/// Shared by the main loop, one-shot mode, and `/recover`.
pub(crate) fn run_turn_loop(messages: &mut Vec<Value>, params: &TurnParams) {
    let client = ReqwestClient::new();
    let classifier = HighDefaultClassifier;
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let project_root = detect_project_root(Some(&cwd));
    let runtime = TokioRuntime::new().expect("failed to create tokio runtime");
    runtime.block_on(run_model_turn_loop(
        messages,
        LoopRequest {
            config: params.config,
            client: &client,
            source: params.source,
            model: params.model,
            approval: params.approval,
            classifier: &classifier,
            cwd: &cwd,
            project_root: &project_root,
            env: params.env,
            force_final: params.force_final,
            recovery_sampling: params.recovery_sampling,
        },
    ));
}

/// Load a session's messages (dropping stored system turns, re-inserting the
/// current `SYSTEM`) and restore its source. `None` on missing / unparseable.
fn load_session_messages(dir: &Path, sid: &str, rt: &mut Runtime) -> Option<Vec<Value>> {
    let data = sessions::load_session(dir, sid)?;
    let stored = data.get("messages").and_then(|m| m.as_array())?;
    let mut messages: Vec<Value> = stored
        .iter()
        .filter(|m| m.get("role").and_then(|r| r.as_str()) != Some("system"))
        .cloned()
        .collect();
    messages.insert(0, json!({"role": "system", "content": SYSTEM}));
    if let Some(src) = data.get("source").and_then(|s| s.as_str()) {
        rt.restore_source(Some(src), data.get("model").and_then(|m| m.as_str()));
    }
    Some(messages)
}

/// Apply `--resume`: returns `(messages, session_id)` for the resumed session,
/// or `None` when there is nothing to resume (fresh start).
fn resume_session(
    rt: &mut Runtime,
    dir: &Path,
    env: &HashMap<String, String>,
) -> Option<(Vec<Value>, String)> {
    let target = rt.resume.clone()?;
    let sid = if let Some(t) = target {
        session_id_from_args(&["--resume".to_string(), t], env)?
    } else {
        let recent = sessions::list_sessions(dir, Some(1), 0, None);
        let Some(s) = recent.first() else {
            println!("{DIM}  no saved sessions to resume - starting fresh{RESET}");
            return None;
        };
        s.id.clone()
    };
    let messages = load_session_messages(dir, &sid, rt)?;
    println!(
        "{DIM}  ↻ resumed session {sid} ({} messages){RESET}",
        messages.len() - 1
    );
    println!();
    Some((messages, sid))
}

/// Run one-shot mode: read a prompt from a file (or stdin), run one model
/// turn, and exit. No REPL, no banner, no session save.
pub fn run_one_shot(prompt_file: &str, rt: &Runtime) {
    let prompt = if prompt_file == "-" {
        let mut input = String::new();
        let _ = io::stdin().read_to_string(&mut input);
        input
    } else {
        match fs::read_to_string(prompt_file) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{RED}  ✗ couldn't read prompt file {prompt_file:?}: {e}{RESET}");
                return;
            }
        }
    };

    let prompt = prompt.trim();
    if prompt.is_empty() {
        eprintln!("{RED}  ✗ prompt file is empty{RESET}");
        return;
    }

    let mut messages = vec![
        json!({"role": "system", "content": SYSTEM}),
        json!({"role": "user", "content": prompt}),
    ];
    log_event("req", &json!({"prompt": prompt, "mode": "one_shot"}));

    if let (Some(source), Some(model)) = (rt.active_source(), &rt.model) {
        let config = ModelConfig::from_env(&rt.env);
        run_turn_loop(
            &mut messages,
            &TurnParams {
                config: &config,
                source,
                model,
                approval: &rt.approval,
                env: &rt.env,
                force_final: false,
                recovery_sampling: false,
            },
        );
    } else {
        eprintln!("{RED}  no active source - set AFI_BASE_URL and AFI_MODEL{RESET}");
    }
}

/// One prompt read: a line to handle, a retry (Ctrl+C), or a quit (Ctrl+D/err).
enum Prompt {
    Line(String),
    Retry,
    Quit,
}

/// Read one multi-line prompt, mapping the terminal control signals.
fn read_prompt(history: &mut Vec<String>, session_id: &str) -> Prompt {
    match term::chatbox::read_multiline("  > ", history) {
        Ok(text) => Prompt::Line(text),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
            println!();
            println!("{DIM}  resume with: afi --resume {session_id}{RESET}");
            Prompt::Quit
        }
        Err(e) if e.kind() == io::ErrorKind::Interrupted => {
            println!();
            Prompt::Retry
        }
        Err(e) => {
            eprintln!("{RED}  input error: {e}{RESET}");
            Prompt::Quit
        }
    }
}

/// The main REPL loop. Reads input, dispatches slash commands, sends user
/// messages to the model, and auto-saves sessions.
pub fn run_repl(rt: &mut Runtime) {
    let env = rt.env.clone();
    let config = ModelConfig::from_env(&env);

    println!("{}", banner(rt));
    println!();

    let dir = sessions::sessions_dir(&env);
    let mut session_id = new_session_id();
    let mut messages = vec![json!({"role": "system", "content": SYSTEM})];
    let mut history: Vec<String> = Vec::new();

    if let Some((resumed, sid)) = resume_session(rt, &dir, &env) {
        messages = resumed;
        session_id = sid;
    }

    if let Some(prompt_file) = &rt.prompt_file {
        run_one_shot(prompt_file, rt);
        return;
    }

    loop {
        let input = match read_prompt(&mut history, &session_id) {
            Prompt::Line(text) => text,
            Prompt::Retry => continue,
            Prompt::Quit => break,
        };

        let input = input.trim();
        if input.is_empty() {
            continue;
        }

        match handle_slash_command(
            input,
            rt,
            &mut messages,
            &mut session_id,
            &mut history,
            &env,
        ) {
            CommandResult::Quit => {
                let meta = json!({});
                let _ = sessions::write_session(&dir, &session_id, &mut messages, Some(&meta));
                println!("{DIM}  resume with: afi --resume {session_id}{RESET}");
                break;
            }
            CommandResult::Continue => {}
            CommandResult::NotACommand => {
                messages.push(json!({"role": "user", "content": input}));
                run_user_turn(&mut messages, &config, rt, &env);
                auto_save(&dir, &session_id, &mut messages, rt, input);
            }
        }
    }

    term::set_idle_title();
}

/// Send the accumulated `messages` to the model for a user turn, or warn when
/// no source is configured.
fn run_user_turn(
    messages: &mut Vec<Value>,
    config: &ModelConfig,
    rt: &Runtime,
    env: &HashMap<String, String>,
) {
    if let (Some(source), Some(model)) = (rt.active_source(), &rt.model) {
        run_turn_loop(
            messages,
            &TurnParams {
                config,
                source,
                model,
                approval: &rt.approval,
                env,
                force_final: false,
                recovery_sampling: false,
            },
        );
    } else {
        eprintln!("{RED}  no active source - use /source to select one{RESET}");
    }
}

/// Auto-save the session after a model turn, tagging it with a title/source.
fn auto_save(dir: &Path, session_id: &str, messages: &mut Vec<Value>, rt: &Runtime, input: &str) {
    let meta = match safe_title(Some(input), 60) {
        Some(t) => {
            let cwd = env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().to_string());
            json!({"title": t, "source": rt.active, "model": rt.model, "cwd": cwd})
        }
        None => json!({"source": rt.active, "model": rt.model}),
    };
    let _ = sessions::write_session(dir, session_id, messages, Some(&meta));
}
