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
use std::time::{Duration, Instant};

use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::json;

use crate::sessions::minion_home;

/// Default poll window for `run_bash` (~3 s). Override with
/// `AFI_BASH_POLL_SECONDS`.
pub const DEFAULT_POLL_SECONDS: i64 = 3;

/// Infer a poll timeout from a `sleep N` in the command. Returns `max(N) + 10`
/// so `sleep 30 && cat …` finishes synchronously.
pub fn infer_timeout_from_sleep(command: &str, default: i64) -> i64 {
    static SLEEP_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?:^|[;&|]\s*)sleep\s+(\d+)").unwrap());
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
pub fn run_detached(
    command: &str,
    env: &std::collections::HashMap<String, String>,
) -> (u32, PathBuf) {
    delete_old_bg_logs(env);

    let home = minion_home(env);
    let bg_dir = home.join("bg-logs");
    let _ = fs::create_dir_all(&bg_dir);

    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let hex: String = (0..3).map(|_| format!("{:02x}", rand_u8())).collect();
    let log_path = bg_dir.join(format!("bg-{}-{}.log", stamp, hex));

    let inner = format!("({}); echo \"[exit: $?]\"", command);

    let log_file = match fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
    {
        Ok(f) => f,
        Err(_) => return (0, log_path),
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
    let map_path = bg_dir.join(format!("bg-pid-{}.map", pid));
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
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0xdeadbeef);
    let tid = std::thread::current().id();
    let tid_hash = format!("{:?}", tid)
        .bytes()
        .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
    now ^ tid_hash
}

/// Poll `pid` up to `timeout` seconds. Returns `"exited"`, `"timeout"`, or
/// `"interrupted"` (only if `check_esc` returns true).
///
/// Uses `waitpid(WNOHANG)` to reap children started by us, and `kill(pid, 0)`
/// (signal-0 probe) as a fallback for reparented processes.
pub fn poll_pid(pid: u32, timeout: i64, check_esc: &dyn Fn() -> bool) -> PollStatus {
    let deadline = Instant::now() + Duration::from_secs(timeout.max(0) as u64);
    let pid_nix = Pid::from_raw(pid as i32);

    loop {
        // Try to reap our own child.
        match waitpid(pid_nix, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(_, _)) | Ok(WaitStatus::Signaled(_, _, _)) => {
                return PollStatus::Exited
            }
            Ok(WaitStatus::StillAlive) => {}
            Ok(_) => return PollStatus::Exited,
            Err(nix::errno::Errno::ECHILD) => {
                // Not our child (reparented to init) — fall through to kill probe.
            }
            Err(_) => return PollStatus::Exited,
        }

        // Signal-0 probe: ESRCH = no such process (exited); EPERM = exists but
        // not ours (alive); success = alive.
        match nix::sys::signal::kill(pid_nix, None) {
            Ok(()) => {}
            Err(nix::errno::Errno::ESRCH) => return PollStatus::Exited,
            Err(nix::errno::Errno::EPERM) => {}
            Err(_) => return PollStatus::Exited,
        }

        if check_esc() {
            return PollStatus::Interrupted;
        }

        if Instant::now() >= deadline {
            return PollStatus::Timeout;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollStatus {
    Exited,
    Timeout,
    Interrupted,
}

/// Read a log file, parse the trailing `[exit: N]` marker. Returns
/// `(content, exit_code)`.
pub fn read_log(log_path: &Path) -> (String, Option<i32>) {
    let bytes = match fs::read(log_path) {
        Ok(b) => b,
        Err(_) => return (String::new(), None),
    };
    let mut out = String::from_utf8_lossy(&bytes).to_string();

    // Parse the trailing [exit: N] marker.
    static EXIT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\[exit:\s*(-?\d+)\]").unwrap());

    let last_nl = out.trim_end_matches('\n').rfind('\n');
    let trailer = match last_nl {
        Some(idx) => out[idx + 1..].trim().to_string(),
        None => out.trim().to_string(),
    };

    if let Some(m) = EXIT_RE.captures(&trailer) {
        let exit_code = m.get(1).and_then(|g| g.as_str().parse::<i32>().ok());
        // Remove the trailer line from the output.
        if let Some(idx) = last_nl {
            out.truncate(idx);
        } else {
            out.clear();
        }
        (out, exit_code)
    } else {
        (out, None)
    }
}

/// Look up the log path for a background PID via the pid→log map files.
pub fn find_bg_log_for_pid(
    pid: u32,
    env: &std::collections::HashMap<String, String>,
) -> Option<PathBuf> {
    let bg_dir = minion_home(env).join("bg-logs");
    let map_path = bg_dir.join(format!("bg-pid-{}.map", pid));
    if let Ok(data) = fs::read_to_string(&map_path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
            if let Some(p) = v.get("log_path").and_then(|v| v.as_str()) {
                return Some(PathBuf::from(p));
            }
        }
    }
    // Fallback: scan logs for one that mentions the PID in the first 1K.
    if let Ok(entries) = fs::read_dir(&bg_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("bg-") || name.contains(".map") {
                continue;
            }
            if let Ok(bytes) = fs::read(entry.path()) {
                let head = String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]);
                if head.contains(&pid.to_string()) {
                    return Some(entry.path());
                }
            }
        }
    }
    None
}

/// Trim background logs so they don't pile up. Removes logs older than
/// `max_age_days` and caps the count at `max_logs` (keeping newest). Also
/// cleans stale `.map` files whose target log no longer exists.
pub fn delete_old_bg_logs(env: &std::collections::HashMap<String, String>) {
    let bg_dir = minion_home(env).join("bg-logs");
    let entries = match fs::read_dir(&bg_dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    let mut log_entries: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    let mut map_entries: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("bg-") {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        if name.ends_with(".map") {
            map_entries.push((mtime, entry.path()));
        } else {
            log_entries.push((mtime, entry.path()));
        }
    }

    let now = std::time::SystemTime::now();
    let max_age = Duration::from_secs(7 * 86400);

    // Remove logs by age.
    for (mtime, p) in &log_entries {
        if now.duration_since(*mtime).unwrap_or(Duration::ZERO) > max_age {
            let _ = fs::remove_file(p);
        }
    }
    // Cap log count (keep newest).
    log_entries.sort_by_key(|(m, _)| std::cmp::Reverse(*m));
    for (_, p) in log_entries.iter().skip(50) {
        let _ = fs::remove_file(p);
    }
    // Clean stale map files.
    for (mtime, p) in &map_entries {
        let should_del = match fs::read_to_string(p) {
            Ok(data) => {
                let target = serde_json::from_str::<serde_json::Value>(&data)
                    .ok()
                    .and_then(|v| {
                        v.get("log_path")
                            .and_then(|v| v.as_str())
                            .map(PathBuf::from)
                    });
                match target {
                    Some(t) => {
                        !t.exists()
                            || now.duration_since(*mtime).unwrap_or(Duration::ZERO) > max_age
                    }
                    None => true,
                }
            }
            Err(_) => true,
        };
        if should_del {
            let _ = fs::remove_file(p);
        }
    }
}

/// Run a shell command. If it finishes within the poll window, output is
/// returned directly and the log deleted. Otherwise it backgrounds and the
/// caller gets a PID + log path.
///
/// `check_esc` is polled during the wait; returning `true` backgrounds the
/// command immediately.
pub fn run_bash(
    command: &str,
    timeout: Option<i64>,
    env: &std::collections::HashMap<String, String>,
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
            std::thread::sleep(Duration::from_millis(200));
            let (out, exit_code) = read_log(&log_path);
            let _ = fs::remove_file(&log_path);
            match exit_code {
                Some(code) => format!("[exit {}]\n{}", code, out),
                None => format!("[exited]\n{}", out),
            }
        }
    }
}

/// Wait for a backgrounded command to finish. Polls `pid` until it exits
/// (default: indefinitely). Esc interrupts the wait (the command keeps
/// running).
pub fn wait_background(
    pid: u32,
    log_path: Option<&Path>,
    timeout: i64,
    env: &std::collections::HashMap<String, String>,
    check_esc: &dyn Fn() -> bool,
) -> String {
    let log_path = match log_path {
        Some(p) => p.to_path_buf(),
        None => match find_bg_log_for_pid(pid, env) {
            Some(p) => p,
            None => {
                return format!(
                    "[error] No background log found for PID {}. The log may have been cleaned up already.",
                    pid
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
            log_path.canonicalize().unwrap_or_else(|_| log_path.clone()).display()
        )
        }
        PollStatus::Exited => {
            std::thread::sleep(Duration::from_millis(200));
            let (out, exit_code) = read_log(&log_path);
            let _ = fs::remove_file(&log_path);
            match exit_code {
                Some(code) => format!("[exit {}]\n{}", code, out),
                None => format!("[exited]\n{}", out),
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
        let mut env = std::collections::HashMap::new();
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
        let mut env = std::collections::HashMap::new();
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
        let mut env = std::collections::HashMap::new();
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
