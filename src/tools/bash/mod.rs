//! Bash tool: detached command execution in a new process group, background
//! log management, and `wait_background` polling.
//!
//! Commands run via `sh -c` in their own process group (set with the safe
//! `CommandExt::process_group`) so terminal Ctrl+C -- sent only to the
//! foreground group -- can't reach them. Quick commands return output inline
//! (log deleted); longer ones keep running and the caller gets a PID + log.

use std::fs;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use chrono::Local;
use regex::Regex;
use serde_json::json;

use crate::sessions::afi_home;
use std::collections::HashMap;
use std::hash::BuildHasher;
use std::sync::LazyLock;
use std::thread;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

mod logs;
pub use logs::{PollStatus, delete_old_bg_logs, find_bg_log_for_pid, poll_pid, read_log};

/// Default poll window for `run_bash` (~3 s). Override with
/// `AFI_BASH_POLL_SECONDS`.
pub const DEFAULT_POLL_SECONDS: i64 = 3;

/// Infer a poll timeout from a `sleep N` in the command. Returns `max(N) + 10`
/// so `sleep 30 && cat …` finishes synchronously.
pub fn infer_timeout_from_sleep(command: &str, default: i64) -> i64 {
    static SLEEP_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?:^|[;&|]\s*)sleep\s+(\d+)").unwrap());
    let max_sleep = SLEEP_RE
        .captures_iter(command)
        .filter_map(|c| c.get(1).and_then(|m| m.as_str().parse::<i64>().ok()))
        .max()
        .unwrap_or(0);
    if max_sleep > 0 {
        max_sleep + 10
    } else {
        default
    }
}

/// Launch `command` detached in its own process group via
/// `CommandExt::process_group`. Returns `(pid, log_path)`. The command runs in
/// a subshell that echoes its exit status as `[exit: N]` at the end of the log.
///
/// # Panics
/// Panics if the detached child process cannot be spawned.
#[must_use]
pub fn run_detached<S: BuildHasher>(
    command: &str,
    env: &HashMap<String, String, S>,
) -> (u32, PathBuf) {
    delete_old_bg_logs(env);

    let home = afi_home(env);
    let bg_dir = home.join("bg-logs");
    let _ = fs::create_dir_all(&bg_dir);

    let stamp = Local::now().format("%Y%m%d-%H%M%S");
    let hex = format!("{:02x}{:02x}{:02x}", rand_u8(), rand_u8(), rand_u8());
    let log_path = bg_dir.join(format!("bg-{stamp}-{hex}.log"));

    let inner = format!("({command}); echo \"[exit: $?]\"");

    let Ok(log_file) = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
    else {
        return (0, log_path);
    };

    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(&inner);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::from(
        log_file
            .try_clone()
            .unwrap_or_else(|_| log_file.try_clone().unwrap()),
    ));
    cmd.stderr(Stdio::from(log_file));
    // Put the child in its own process group (pgid == child pid) so a terminal
    // Ctrl+C -- which the kernel delivers only to the foreground process group
    // -- can't reach it. `process_group` is the safe `CommandExt` equivalent of
    // a pre_exec `setpgid`: no `unsafe` and no post-fork closure.
    cmd.process_group(0);

    let child = cmd.spawn().expect("failed to spawn detached command");
    let pid = child.id();
    // Intentionally don't wait() on the Child handle — it's detached via
    // setsid() and we poll its exit via waitpid/kill in poll_pid(). Drop
    // the handle without reaping.
    drop(child);

    // Write a pid→logpath map so wait_background can find it later.
    let map_path = bg_dir.join(format!("bg-pid-{pid}.map"));
    if let Ok(mut f) = fs::File::create(&map_path) {
        let _ = writeln!(f, "{}", json!({"log_path": log_path.to_string_lossy()}));
    }

    (pid, log_path)
}

fn rand_u8() -> u8 {
    use std::cell::Cell;
    thread_local! {
        static STATE: Cell<u64> = Cell::new(seed());
    }
    STATE.with(|s| {
        let mut x = s.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.set(x);
        (x & 0xff) as u8
    })
}

fn seed() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0xdead_beef, |d| d.as_secs() ^ u64::from(d.subsec_nanos()));
    let tid = thread::current().id();
    let tid_hash = format!("{tid:?}").bytes().fold(0u64, |acc, b| {
        acc.wrapping_mul(31).wrapping_add(u64::from(b))
    });
    now ^ tid_hash
}

/// Run a shell command. If it finishes within the poll window, output is
/// returned directly and the log deleted. Otherwise it backgrounds and the
/// caller gets a PID + log path.
///
/// `check_esc` is polled during the wait; returning `true` backgrounds the
/// command immediately.
pub fn run_bash<S: BuildHasher>(
    command: &str,
    timeout: Option<i64>,
    env: &HashMap<String, String, S>,
    check_esc: &dyn Fn() -> bool,
) -> String {
    let command = match command {
        "" => {
            return "ERROR: run_bash requires a 'command' argument.".to_string();
        }
        c => c,
    };

    let timeout = match timeout {
        Some(t) => t,
        None => infer_timeout_from_sleep(
            command,
            env.get("AFI_BASH_POLL_SECONDS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_POLL_SECONDS),
        ),
    };
    let timeout = if timeout <= 0 { i64::MAX } else { timeout };

    let (pid, log_path) = run_detached(command, env);
    let status = poll_pid(pid, timeout, check_esc);

    match status {
        PollStatus::Interrupted => {
            format!(
                "[background] PID {} | log: {}\nThe command was interrupted (Esc pressed) and is \
                 now running in the background. Check output with read_file('{}')",
                pid,
                log_path.display(),
                log_path
                    .canonicalize()
                    .unwrap_or_else(|_| log_path.clone())
                    .display()
            )
        }
        PollStatus::Timeout => {
            format!(
                "[background] PID {} | log: {}\nThe command is still running in the background. \
                 Check output with read_file('{}')",
                pid,
                log_path.display(),
                log_path
                    .canonicalize()
                    .unwrap_or_else(|_| log_path.clone())
                    .display()
            )
        }
        PollStatus::Exited => {
            thread::sleep(Duration::from_millis(200));
            let (out, exit_code) = read_log(&log_path);
            let _ = fs::remove_file(&log_path);
            match exit_code {
                Some(code) => format!("[exit {code}]\n{out}"),
                None => format!("[exited]\n{out}"),
            }
        }
    }
}

/// Wait for a backgrounded command to finish. Polls `pid` until it exits
/// (default: indefinitely). Esc interrupts the wait (the command keeps
/// running).
pub fn wait_background<S: BuildHasher>(
    pid: u32,
    log_path: Option<&Path>,
    timeout: i64,
    env: &HashMap<String, String, S>,
    check_esc: &dyn Fn() -> bool,
) -> String {
    let log_path = match log_path {
        Some(p) => p.to_path_buf(),
        None => match find_bg_log_for_pid(pid, env) {
            Some(p) => p,
            None => {
                return format!(
                    "[error] No background log found for PID {pid}. The log may have been cleaned up already."
                );
            }
        },
    };

    let timeout = if timeout <= 0 { i64::MAX } else { timeout };
    let status = poll_pid(pid, timeout, check_esc);

    match status {
        PollStatus::Interrupted => format!(
            "[interrupted] PID {} still running. Check later with read_file('{}')",
            pid,
            log_path
                .canonicalize()
                .unwrap_or_else(|_| log_path.clone())
                .display()
        ),
        PollStatus::Timeout => {
            format!(
                "[timeout after {}s] PID {} still running. Check output so far with read_file('{}')",
                timeout,
                pid,
                log_path
                    .canonicalize()
                    .unwrap_or_else(|_| log_path.clone())
                    .display()
            )
        }
        PollStatus::Exited => {
            thread::sleep(Duration::from_millis(200));
            let (out, exit_code) = read_log(&log_path);
            let _ = fs::remove_file(&log_path);
            match exit_code {
                Some(code) => format!("[exit {code}]\n{out}"),
                None => format!("[exited]\n{out}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_timeout_from_sleep_extracts_max() {
        assert_eq!(infer_timeout_from_sleep("sleep 30 && cat file", 3), 40);
        assert_eq!(infer_timeout_from_sleep("sleep 5; sleep 20", 3), 30);
        assert_eq!(infer_timeout_from_sleep("echo hello", 3), 3);
    }

    #[test]
    fn run_bash_quick_command() {
        let tmp = tempfile::tempdir().unwrap();
        let mut env = HashMap::new();
        env.insert(
            "AFI_HOME".to_string(),
            tmp.path().to_string_lossy().to_string(),
        );
        let out = run_bash("echo hello", None, &env, &|| false);
        assert!(out.contains("[exit 0]"));
        assert!(out.contains("hello"));
    }

    #[test]
    fn run_bash_backgrounds_long_command() {
        let tmp = tempfile::tempdir().unwrap();
        let mut env = HashMap::new();
        env.insert(
            "AFI_HOME".to_string(),
            tmp.path().to_string_lossy().to_string(),
        );
        let out = run_bash("sleep 10", Some(1), &env, &|| false);
        assert!(out.contains("[background]"));
        assert!(out.contains("PID"));
    }

    #[test]
    fn run_bash_esc_interrupts() {
        let tmp = tempfile::tempdir().unwrap();
        let mut env = HashMap::new();
        env.insert(
            "AFI_HOME".to_string(),
            tmp.path().to_string_lossy().to_string(),
        );
        let out = run_bash("sleep 10", Some(5), &env, &|| true);
        assert!(out.contains("[background]"));
        assert!(out.contains("interrupted"));
    }

    #[test]
    fn read_log_parses_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("test.log");
        fs::write(&log, "hello world\n[exit: 42]\n").unwrap();
        let (out, code) = read_log(&log);
        assert_eq!(out, "hello world");
        assert_eq!(code, Some(42));
    }

    #[test]
    fn read_log_no_exit_marker() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("test.log");
        fs::write(&log, "no marker here\n").unwrap();
        let (out, code) = read_log(&log);
        assert_eq!(out, "no marker here\n");
        assert_eq!(code, None);
    }
}
