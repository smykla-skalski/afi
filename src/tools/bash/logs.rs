//! Background-command polling and log-file management for the bash tool.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::fs;
use std::hash::BuildHasher;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nix::errno::Errno;
use nix::sys::signal;
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;
use regex::Regex;

use crate::sessions::afi_home;

/// A background file (log or map) paired with its modification time.
type TimedPath = (SystemTime, PathBuf);

/// Poll `pid` up to `timeout` seconds. Returns `"exited"`, `"timeout"`, or
/// `"interrupted"` (only if `check_esc` returns true).
///
/// Uses `waitpid(WNOHANG)` to reap children started by us, and `kill(pid, 0)`
/// (signal-0 probe) as a fallback for reparented processes.
pub fn poll_pid(pid: u32, timeout: i64, check_esc: &dyn Fn() -> bool) -> PollStatus {
    let deadline =
        Instant::now() + Duration::from_secs(u64::try_from(timeout.max(0)).unwrap_or(u64::MAX));
    let pid_nix = Pid::from_raw(i32::try_from(pid).unwrap_or(i32::MAX));

    loop {
        // Try to reap our own child.
        match waitpid(pid_nix, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(_, _) | WaitStatus::Signaled(_, _, _)) => {
                return PollStatus::Exited
            }
            // Still running, or reparented to init (ECHILD) — fall through to
            // the kill probe.
            Ok(WaitStatus::StillAlive) | Err(Errno::ECHILD) => {}
            Ok(_) | Err(_) => return PollStatus::Exited,
        }

        // Signal-0 probe: ESRCH = no such process (exited); EPERM = exists but
        // not ours (alive); success = alive.
        match signal::kill(pid_nix, None) {
            Ok(()) | Err(Errno::EPERM) => {}
            Err(_) => return PollStatus::Exited,
        }

        if check_esc() {
            return PollStatus::Interrupted;
        }

        if Instant::now() >= deadline {
            return PollStatus::Timeout;
        }
        thread::sleep(Duration::from_millis(100));
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
    // Parse the trailing [exit: N] marker.
    static EXIT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[exit:\s*(-?\d+)\]").unwrap());

    let Ok(bytes) = fs::read(log_path) else {
        return (String::new(), None);
    };
    let mut out = String::from_utf8_lossy(&bytes).to_string();

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

/// Read the pid→log map file at `map_path` and return its `log_path`, if any.
fn log_from_map(map_path: &Path) -> Option<PathBuf> {
    let data = fs::read_to_string(map_path).ok()?;
    let v = serde_json::from_str::<serde_json::Value>(&data).ok()?;
    v.get("log_path")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
}

/// Fallback lookup: scan `bg_dir` for a log mentioning `pid` in its first 1 KiB.
fn scan_logs_for_pid(bg_dir: &Path, pid: u32) -> Option<PathBuf> {
    let entries = fs::read_dir(bg_dir).ok()?;
    let needle = pid.to_string();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("bg-") || name.contains(".map") {
            continue;
        }
        let Ok(bytes) = fs::read(entry.path()) else {
            continue;
        };
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]);
        if head.contains(&needle) {
            return Some(entry.path());
        }
    }
    None
}

/// Look up the log path for a background PID via the pid→log map files.
#[must_use]
pub fn find_bg_log_for_pid<S: BuildHasher>(
    pid: u32,
    env: &HashMap<String, String, S>,
) -> Option<PathBuf> {
    let bg_dir = afi_home(env).join("bg-logs");
    let map_path = bg_dir.join(format!("bg-pid-{pid}.map"));
    log_from_map(&map_path).or_else(|| scan_logs_for_pid(&bg_dir, pid))
}

/// Partition the `bg-*` entries in `bg_dir` into (logs, maps), each with its
/// mtime. Returns empty vectors when the directory can't be read.
fn collect_bg_entries(bg_dir: &Path) -> (Vec<TimedPath>, Vec<TimedPath>) {
    let mut logs: Vec<TimedPath> = Vec::new();
    let mut maps: Vec<TimedPath> = Vec::new();
    let Ok(entries) = fs::read_dir(bg_dir) else {
        return (logs, maps);
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("bg-") {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(UNIX_EPOCH);
        if Path::new(&name)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("map"))
        {
            maps.push((mtime, entry.path()));
        } else {
            logs.push((mtime, entry.path()));
        }
    }
    (logs, maps)
}

/// Remove logs older than `max_age`, then cap the count at 50 (newest kept).
fn prune_logs(mut logs: Vec<TimedPath>, now: SystemTime, max_age: Duration) {
    for (mtime, p) in &logs {
        if now.duration_since(*mtime).unwrap_or(Duration::ZERO) > max_age {
            let _ = fs::remove_file(p);
        }
    }
    logs.sort_by_key(|(m, _)| Reverse(*m));
    for (_, p) in logs.iter().skip(50) {
        let _ = fs::remove_file(p);
    }
}

/// True if a `.map` file is unreadable, malformed, points at a missing log, or
/// is itself older than `max_age`.
fn map_is_stale(p: &Path, mtime: SystemTime, now: SystemTime, max_age: Duration) -> bool {
    let Ok(data) = fs::read_to_string(p) else {
        return true;
    };
    let target = serde_json::from_str::<serde_json::Value>(&data)
        .ok()
        .and_then(|v| {
            v.get("log_path")
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
        });
    match target {
        Some(t) => !t.exists() || now.duration_since(mtime).unwrap_or(Duration::ZERO) > max_age,
        None => true,
    }
}

/// Delete stale `.map` files (missing target, malformed, or aged out).
fn prune_maps(maps: &[TimedPath], now: SystemTime, max_age: Duration) {
    for (mtime, p) in maps {
        if map_is_stale(p, *mtime, now, max_age) {
            let _ = fs::remove_file(p);
        }
    }
}

/// Trim background logs so they don't pile up. Removes logs older than one week
/// and caps the count at 50 (keeping newest). Also cleans stale `.map` files
/// whose target log no longer exists.
pub fn delete_old_bg_logs<S: BuildHasher>(env: &HashMap<String, String, S>) {
    let bg_dir = afi_home(env).join("bg-logs");
    let (logs, maps) = collect_bg_entries(&bg_dir);
    let now = SystemTime::now();
    let max_age = Duration::from_hours(168);
    prune_logs(logs, now, max_age);
    prune_maps(&maps, now, max_age);
}
