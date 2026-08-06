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

/// An env map pointing `AFI_HOME` at `home`, with the `bg-logs` directory
/// created so containment has something to canonicalize against.
fn bg_env(home: &Path) -> HashMap<String, String> {
    fs::create_dir_all(home.join("bg-logs")).unwrap();
    let mut env = HashMap::new();
    env.insert("AFI_HOME".to_string(), home.to_string_lossy().to_string());
    env
}

#[test]
fn wait_background_refuses_a_path_outside_the_log_directory() {
    // The exit path unlinks the file it read, and `wait_background` is not
    // approval-gated, so an uncontained `log_path` is an unapproved
    // arbitrary read plus delete - reachable under a policy whose whole
    // point is that the run cannot write.
    let tmp = tempfile::tempdir().unwrap();
    let env = bg_env(tmp.path());
    let victim = tmp.path().join("victim-notes.md");
    fs::write(&victim, "SECRET private notes\n").unwrap();

    let out = wait_background(999_999, Some(&victim), 1, &env, &|| false);

    assert!(out.contains("is not a background log"), "{out}");
    assert!(!out.contains("SECRET"), "contents leaked: {out}");
    assert!(victim.exists(), "the refused path was deleted anyway");
}

#[test]
fn wait_background_refuses_a_traversal_back_out_of_the_log_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let env = bg_env(tmp.path());
    let victim = tmp.path().join("victim-notes.md");
    fs::write(&victim, "SECRET\n").unwrap();
    let traversal = tmp.path().join("bg-logs").join("..").join("bg-notes.md");
    fs::write(tmp.path().join("bg-notes.md"), "SECRET\n").unwrap();

    let out = wait_background(999_999, Some(&traversal), 1, &env, &|| false);

    assert!(out.contains("is not a background log"), "{out}");
    assert!(tmp.path().join("bg-notes.md").exists());
}

#[test]
fn wait_background_refuses_a_non_log_file_inside_the_log_directory() {
    // The `bg-` prefix is what `run_detached` writes, so a file that merely
    // landed in the directory is still not ours to read and unlink.
    let tmp = tempfile::tempdir().unwrap();
    let env = bg_env(tmp.path());
    let stray = tmp.path().join("bg-logs").join("notes.md");
    fs::write(&stray, "SECRET\n").unwrap();

    let out = wait_background(999_999, Some(&stray), 1, &env, &|| false);

    assert!(out.contains("is not a background log"), "{out}");
    assert!(stray.exists());
}

#[test]
fn wait_background_still_reads_a_real_background_log() {
    let tmp = tempfile::tempdir().unwrap();
    let env = bg_env(tmp.path());
    let log = tmp
        .path()
        .join("bg-logs")
        .join("bg-20260806-000000-aabbcc.log");
    fs::write(&log, "command output\n[exit: 0]\n").unwrap();

    let out = wait_background(999_999, Some(&log), 1, &env, &|| false);

    assert!(out.contains("command output"), "{out}");
    assert!(out.contains("[exit 0]"), "{out}");
    // Reading a finished log still cleans it up, as it did before.
    assert!(!log.exists(), "a real log should be consumed");
}
