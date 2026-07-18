//! Regression proof that `run_bash` launches commands in their own process
//! group. That isolation is what lets a terminal Ctrl+C -- which the kernel
//! delivers only to the foreground process group -- miss the detached child.
//! It guards the safe `CommandExt::process_group(0)` swap that replaced the
//! old `unsafe { pre_exec(setsid) }` in `src/tools/bash.rs`.

use std::collections::HashMap;
use std::process::{Command, id};

use afi::tools::bash::run_bash;

/// Process-group id of `pid` via `ps` (portable across macOS and Linux; `=`
/// suppresses the column header on both).
fn pgid_of(pid: u32) -> i64 {
    let out = Command::new("ps")
        .args(["-o", "pgid=", "-p", &pid.to_string()])
        .output()
        .expect("ps invocation failed");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .expect("ps returned a non-numeric pgid")
}

#[test]
fn detached_command_leads_its_own_process_group() {
    let tmp = tempfile::tempdir().unwrap();
    let mut env = HashMap::new();
    env.insert(
        "AFI_HOME".to_string(),
        tmp.path().to_string_lossy().to_string(),
    );

    // Inside `sh -c`, `$$` is the shell afi spawns -- POSIX keeps `$$` stable
    // inside both `( )` and `$( )`, so this reports that shell's own pid and
    // its process-group id. The `\n` is a printf escape, kept literal by the
    // Rust raw string.
    let out = run_bash(
        r#"printf '%s %s\n' "$$" "$(ps -o pgid= -p $$ | tr -d ' ')""#,
        Some(10),
        &env,
        &|| false,
    );

    let line = out.lines().last().unwrap_or_default();
    let mut nums = line.split_whitespace();
    let child_pid: i64 = nums
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("no child pid in run_bash output: {out:?}"));
    let child_group: i64 = nums
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("no child pgid in run_bash output: {out:?}"));

    // process_group(0) makes the child a group leader: pgid == pid.
    assert_eq!(
        child_pid, child_group,
        "detached child must lead its own process group (out={out:?})"
    );

    // And that group must differ from the test runner's own group -- proof the
    // child is isolated from the foreground group a Ctrl+C would target.
    let own_pgid = pgid_of(id());
    assert_ne!(
        child_group, own_pgid,
        "detached group must differ from the test process group"
    );
}
