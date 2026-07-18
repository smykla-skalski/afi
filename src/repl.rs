//! The REPL: main loop, slash command dispatch, session auto-save, banner,
//! and one-shot mode. This is the top-level orchestrator that ties together
//! all the other modules (config, sessions, tools, model, risk, term).

use std::collections::HashMap;
use std::io::{self, Read};

use serde_json::{json, Value};

use crate::approval::Level;
use crate::config::Runtime;
use crate::log::log_event;
use crate::model::ModelConfig;
use crate::prompt::SYSTEM;
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
pub fn banner(rt: &Runtime) -> String {
    let sep = format!("{DIM} \u{00b7} {RESET}");
    let model = rt.model.clone().unwrap_or_else(|| "auto".to_string());
    let mut parts = vec![
        format!("{BOLD}minion{RESET}"),
        format!("{CYAN}{model}{RESET}"),
    ];
    if rt.sources.len() > 1 {
        if let Some(active) = &rt.active {
            parts.push(format!("{MAGENTA}{active}{RESET}"));
        }
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
pub fn handle_slash_command(
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
    let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");

    match cmd {
        "/quit" | "/exit" => CommandResult::Quit,
        "/reset" | "/clear" | "/new" => {
            *messages = vec![json!({"role": "system", "content": SYSTEM})];
            *session_id = new_session_id();
            println!("{DIM}  ↻ started a fresh session ({}){RESET}", session_id);
            CommandResult::Continue
        }
        "/yolo" => {
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
        "/approval" => {
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
            } else {
                match crate::approval::normalize_approval(arg) {
                    Some(kind) => {
                        crate::approval::apply_approval(&mut rt.approval, kind, true);
                        println!(
                            "{DIM}  approval set to: {}{RESET}",
                            crate::approval::approval_display(&rt.approval)
                        );
                    }
                    None => {
                        println!("{RED}  unknown approval level {arg:?} (want all|low|medium|high|yolo){RESET}");
                    }
                }
            }
            CommandResult::Continue
        }
        "/source" => {
            if arg.is_empty() {
                // List all sources.
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
            } else {
                let source_parts: Vec<&str> = arg.split_whitespace().collect();
                let name = source_parts[0];
                let model_override = source_parts.get(1).map(|s| s.to_string());
                if rt.switch_source(name, model_override.as_deref()) {
                    println!("{DIM}  → switched to {name}{RESET}");
                    println!("{}", banner(rt));
                } else {
                    println!("{RED}  ✗ unknown source {name:?}{RESET}");
                }
            }
            CommandResult::Continue
        }
        "/compress" | "/compact" => {
            let body_len = messages.len().saturating_sub(1); // minus system
            if body_len <= crate::model::compress::COMPRESS_KEEP {
                println!("{DIM}  nothing to compress (too few turns){RESET}");
            } else {
                println!("{DIM}  compressing context...{RESET}");
                if let (Some(source), Some(model)) = (rt.active_source(), &rt.model) {
                    let client = crate::model::client::ReqwestClient::new();
                    let runtime =
                        tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
                    let extra_body = source.extra_request_kwargs();
                    let result = runtime.block_on(async {
                        let prompt =
                            "Summarize the following conversation history for context retention.";
                        use crate::model::client::ChatClient;
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
                            Ok(text) => {
                                // Parse the response.
                                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                                    v.get("choices")
                                        .and_then(|c| c.as_array())
                                        .and_then(|c| c.first())
                                        .and_then(|c| c.get("message"))
                                        .and_then(|m| m.get("content"))
                                        .and_then(|c| c.as_str())
                                        .map(|s| s.to_string())
                                } else {
                                    None
                                }
                            }
                            Err(e) => {
                                eprintln!("{RED}  compress failed: {e}{RESET}");
                                None
                            }
                        }
                    });
                    if let Some(summary) = result {
                        let header = format!("[Compressed context - earlier turns summarized; last {} turns kept verbatim]", crate::model::compress::COMPRESS_KEEP);
                        // Keep system + summary + last COMPRESS_KEEP turns.
                        let keep = crate::model::compress::COMPRESS_KEEP;
                        let has_sys = messages
                            .first()
                            .map(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
                            .unwrap_or(false);
                        let split = messages.len().saturating_sub(keep);
                        let mut new_msgs = Vec::new();
                        if has_sys {
                            new_msgs.push(messages[0].clone());
                        }
                        new_msgs.push(json!({"role": "user", "content": format!("{}\n\n{}", header, summary)}));
                        new_msgs.extend(messages[split..].iter().cloned());
                        *messages = new_msgs;
                        println!("{DIM}  compressed context{RESET}");
                    }
                } else {
                    eprintln!("{RED}  no active source{RESET}");
                }
            }
            CommandResult::Continue
        }
        "/save" => {
            let title = if arg.is_empty() {
                None
            } else {
                Some(arg.to_string())
            };
            let dir = sessions::sessions_dir(env);
            let meta = match &title {
                Some(t) => json!({"title": t}),
                None => json!({}),
            };
            let _ = sessions::write_session(&dir, session_id, messages, Some(&meta));
            println!("{DIM}  saved session {session_id}{RESET}");
            CommandResult::Continue
        }
        "/sessions" => {
            // Delegate to the CLI sessions printer.
            let dir = sessions::sessions_dir(env);
            let sessions_list = sessions::list_sessions(&dir, Some(15), 0, None);
            if sessions_list.is_empty() {
                println!("{DIM}  no saved sessions{RESET}");
            } else {
                for (i, s) in sessions_list.iter().enumerate() {
                    let mark = if &s.id == session_id {
                        format!("{GREEN}●{RESET}")
                    } else {
                        format!("{DIM}{}{RESET}", i + 1)
                    };
                    println!("  {mark} {MAGENTA}{}{RESET} {}", s.id, s.title);
                }
            }
            CommandResult::Continue
        }
        "/delete" => {
            if arg.is_empty() {
                println!("{YELLOW}  usage: /delete <n|id>{RESET}");
            } else {
                let dir = sessions::sessions_dir(env);
                let sessions_list = sessions::list_sessions(&dir, Some(50), 0, None);
                if let Some(sid) = sessions::resolve_session(arg, &sessions_list) {
                    if &sid == session_id {
                        println!("{YELLOW}  cannot delete the current session{RESET}");
                    } else if sessions::delete_session(&dir, &sid) {
                        println!("{DIM}  deleted session {sid}{RESET}");
                    } else {
                        println!("{RED}  ✗ could not delete session {sid}{RESET}");
                    }
                } else {
                    println!("{YELLOW}  no session matching {arg:?}{RESET}");
                }
            }
            CommandResult::Continue
        }
        "/memory" => {
            let sub_parts: Vec<&str> = arg.splitn(2, ' ').collect();
            match sub_parts.first().copied() {
                Some("list") => {
                    let memories = crate::memory::list_memories(env);
                    if memories.is_empty() {
                        println!("{DIM}  no saved memories{RESET}");
                    } else {
                        for (name, title) in &memories {
                            println!("  {MAGENTA}{name}{RESET} {DIM}{title}{RESET}");
                        }
                    }
                }
                Some("remember") => {
                    let query = sub_parts.get(1).copied().unwrap_or("");
                    if query.is_empty() {
                        println!("{YELLOW}  usage: /memory remember <query>{RESET}");
                    } else {
                        let results = crate::memory::remember_memories(env, query);
                        if results.is_empty() {
                            println!("{DIM}  no memories matching {query:?}{RESET}");
                        } else {
                            for (name, title, _) in &results {
                                println!("  {MAGENTA}{name}{RESET} {DIM}{title}{RESET}");
                            }
                        }
                    }
                }
                Some("save") => {
                    println!("{DIM}  (memory save requires a live model connection){RESET}");
                }
                _ => {
                    println!("{DIM}  usage:{RESET}");
                    println!("{DIM}    /memory save [focus...]   save a memory{RESET}");
                    println!("{DIM}    /memory remember <query>   search memories{RESET}");
                    println!("{DIM}    /memory list               list all memories{RESET}");
                }
            }
            CommandResult::Continue
        }
        "/recover" => {
            let note = if arg.is_empty() {
                String::new()
            } else {
                format!(" - {}", arg)
            };
            let nudge = format!("{}{}", crate::model::recovery::MANUAL_RECOVERY_NUDGE, note);
            messages.push(json!({"role": "user", "content": format!("[Runtime note: {}]", nudge)}));
            if let (Some(source), Some(model)) = (rt.active_source(), &rt.model) {
                let config = ModelConfig::from_env(env);
                let client = crate::model::client::ReqwestClient::new();
                let classifier = crate::risk::HighDefaultClassifier;
                let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                let project_root = crate::risk::detect_project_root(Some(&cwd));
                let runtime =
                    tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
                runtime.block_on(crate::model::turn::run_model_turn_loop(
                    messages,
                    crate::model::turn::LoopRequest {
                        config: &config,
                        client: &client,
                        source,
                        model,
                        approval: &rt.approval,
                        classifier: &classifier,
                        cwd: &cwd,
                        project_root: &project_root,
                        env,
                        force_final: true,
                        recovery_sampling: true,
                    },
                ));
            } else {
                eprintln!("{RED}  no active source{RESET}");
            }
            CommandResult::Continue
        }
        "/autocompress" => {
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
        "/provider" => {
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
        "/help" => {
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
        _ => {
            println!("{YELLOW}  unknown command {cmd:?} (try /help){RESET}");
            CommandResult::Continue
        }
    }
}

/// Run one-shot mode: read a prompt from a file (or stdin), run one model
/// turn, and exit. No REPL, no banner, no session save.
pub fn run_one_shot(prompt_file: &str, _rt: &Runtime) {
    let prompt = if prompt_file == "-" {
        let mut input = String::new();
        let _ = io::stdin().read_to_string(&mut input);
        input
    } else {
        match std::fs::read_to_string(prompt_file) {
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

    // Build the initial messages.
    let mut messages = vec![
        json!({"role": "system", "content": SYSTEM}),
        json!({"role": "user", "content": prompt}),
    ];

    log_event("req", &json!({"prompt": prompt, "mode": "one_shot"}));

    // Run the model turn loop.
    if let (Some(source), Some(model)) = (_rt.active_source(), &_rt.model) {
        let config = ModelConfig::from_env(&_rt.env);
        let client = crate::model::client::ReqwestClient::new();
        let classifier = crate::risk::HighDefaultClassifier;
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let project_root = crate::risk::detect_project_root(Some(&cwd));

        let runtime = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
        runtime.block_on(crate::model::turn::run_model_turn_loop(
            &mut messages,
            crate::model::turn::LoopRequest {
                config: &config,
                client: &client,
                source,
                model,
                approval: &_rt.approval,
                classifier: &classifier,
                cwd: &cwd,
                project_root: &project_root,
                env: &_rt.env,
                force_final: false,
                recovery_sampling: false,
            },
        ));
    } else {
        eprintln!("{RED}  no active source - set MINION_BASE_URL and MINION_MODEL{RESET}");
    }
}

/// The main REPL loop. Reads input, dispatches slash commands, sends user
/// messages to the model, and auto-saves sessions.
pub fn run_repl(rt: &mut Runtime, env: HashMap<String, String>) {
    let config = ModelConfig::from_env(&env);

    // Print the banner.
    println!("{}", banner(rt));
    println!();

    // Session state.
    let dir = sessions::sessions_dir(&env);
    let mut session_id = new_session_id();
    let mut messages = vec![json!({"role": "system", "content": SYSTEM})];
    let mut history: Vec<String> = Vec::new();

    // Check for --resume.
    let resume_requested = rt.resume.is_some();
    if resume_requested {
        if let Some(target) = &rt.resume {
            match target {
                Some(t) => {
                    if let Some(sid) =
                        crate::cli::session_id_from_args(&["--resume".to_string(), t.clone()], &env)
                    {
                        if let Some(data) = sessions::load_session(&dir, &sid) {
                            if let Some(msgs) = data.get("messages").and_then(|m| m.as_array()) {
                                messages = msgs
                                    .iter()
                                    .filter(|m| {
                                        m.get("role").and_then(|r| r.as_str()) != Some("system")
                                    })
                                    .cloned()
                                    .collect();
                                messages.insert(0, json!({"role": "system", "content": SYSTEM}));
                                session_id = sid.clone();
                                println!(
                                    "{DIM}  ↻ resumed session {sid} ({ } messages){RESET}",
                                    messages.len() - 1
                                );
                                println!();
                                if let Some(src) = data.get("source").and_then(|s| s.as_str()) {
                                    rt.restore_source(
                                        Some(src),
                                        data.get("model").and_then(|m| m.as_str()),
                                    );
                                }
                            }
                        }
                    }
                }
                None => {
                    // Bare --resume: pick most recent.
                    let sessions_list = sessions::list_sessions(&dir, Some(1), 0, None);
                    if let Some(s) = sessions_list.first() {
                        if let Some(data) = sessions::load_session(&dir, &s.id) {
                            if let Some(msgs) = data.get("messages").and_then(|m| m.as_array()) {
                                messages = msgs
                                    .iter()
                                    .filter(|m| {
                                        m.get("role").and_then(|r| r.as_str()) != Some("system")
                                    })
                                    .cloned()
                                    .collect();
                                messages.insert(0, json!({"role": "system", "content": SYSTEM}));
                                session_id = s.id.clone();
                                println!(
                                    "{DIM}  ↻ resumed session {} ({} messages){RESET}",
                                    s.id,
                                    messages.len() - 1
                                );
                                println!();
                                if let Some(src) = data.get("source").and_then(|s| s.as_str()) {
                                    rt.restore_source(
                                        Some(src),
                                        data.get("model").and_then(|m| m.as_str()),
                                    );
                                }
                            }
                        }
                    } else {
                        println!("{DIM}  no saved sessions to resume - starting fresh{RESET}");
                    }
                }
            }
        }
    }

    // Check for --prompt-file (one-shot mode).
    if let Some(prompt_file) = &rt.prompt_file {
        run_one_shot(prompt_file, rt);
        return;
    }

    // Main REPL loop.
    loop {
        // Read input.
        let input = match term::chatbox::read_multiline("  > ", &mut history) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                // Ctrl+D: exit gracefully.
                println!();
                println!("{DIM}  resume with: minion --resume {session_id}{RESET}");
                break;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                // Ctrl+C: clear and continue.
                println!();
                continue;
            }
            Err(e) => {
                eprintln!("{RED}  input error: {e}{RESET}");
                break;
            }
        };

        let input = input.trim();
        if input.is_empty() {
            continue;
        }

        // Check for slash commands.
        match handle_slash_command(
            input,
            rt,
            &mut messages,
            &mut session_id,
            &mut history,
            &env,
        ) {
            CommandResult::Quit => {
                // Save and exit.
                let meta = json!({});
                let _ = sessions::write_session(&dir, &session_id, &mut messages, Some(&meta));
                println!("{DIM}  resume with: minion --resume {session_id}{RESET}");
                break;
            }
            CommandResult::Continue => {
                continue;
            }
            CommandResult::NotACommand => {
                // It's a user message - add to context.
                messages.push(json!({"role": "user", "content": input}));

                // Run the model turn loop.
                if let (Some(source), Some(model)) = (rt.active_source(), &rt.model) {
                    let client = crate::model::client::ReqwestClient::new();
                    let classifier = crate::risk::HighDefaultClassifier;
                    let cwd =
                        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                    let project_root = crate::risk::detect_project_root(Some(&cwd));

                    let runtime =
                        tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
                    runtime.block_on(crate::model::turn::run_model_turn_loop(
                        &mut messages,
                        crate::model::turn::LoopRequest {
                            config: &config,
                            client: &client,
                            source,
                            model,
                            approval: &rt.approval,
                            classifier: &classifier,
                            cwd: &cwd,
                            project_root: &project_root,
                            env: &env,
                            force_final: false,
                            recovery_sampling: false,
                        },
                    ));
                } else {
                    eprintln!("{RED}  no active source - use /source to select one{RESET}");
                }

                // Auto-save the session.
                let title = safe_title(Some(input), 60);
                let meta = match title {
                    Some(t) => {
                        json!({"title": t, "source": rt.active, "model": rt.model, "cwd": std::env::current_dir().ok().map(|p| p.to_string_lossy().to_string())})
                    }
                    None => json!({"source": rt.active, "model": rt.model}),
                };
                let _ = sessions::write_session(&dir, &session_id, &mut messages, Some(&meta));
                let _ = config.autocompress_percent;
            }
        }
    }

    // Set idle title on exit.
    term::set_idle_title();
}
