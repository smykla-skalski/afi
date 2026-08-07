use super::*;
use crate::tools::known_tool_names;
use serde_json::json;
use tempfile::TempDir;

fn totals(requests: u64) -> UsageTotals {
    UsageTotals {
        input_tokens: 4085,
        output_tokens: 509,
        cache_read_tokens: 6837,
        cache_write_tokens: 2279,
        reasoning_tokens: 0,
        requests,
    }
}

fn summary(ok: bool, answer: &str, usage: UsageTotals) -> RunSummary<'_> {
    RunSummary {
        ok,
        error: None,
        error_kind: None,
        source: Some("anthropic"),
        model: Some("claude-sonnet-5"),
        answer,
        usage,
        cost_usd: None,
        elapsed_secs: 14.2341,
        tools: known_tool_names().to_vec(),
        effort: None,
    }
}

// --- format selection ---------------------------------------------------------

#[test]
fn only_an_explicit_json_value_turns_it_on() {
    assert_eq!(SummaryFormat::from_value(Some("json")), SummaryFormat::Json);
    assert_eq!(
        SummaryFormat::from_value(Some(" JSON ")),
        SummaryFormat::Json
    );
    for off in [None, Some(""), Some("text"), Some("yaml"), Some("true")] {
        assert_eq!(
            SummaryFormat::from_value(off),
            SummaryFormat::None,
            "{off:?} must not enable the summary"
        );
    }
}

#[test]
fn the_default_is_off_so_existing_runs_are_unchanged() {
    assert_eq!(SummaryFormat::default(), SummaryFormat::None);
    assert!(!SummaryFormat::default().is_json());
}

// --- payload ------------------------------------------------------------------

#[test]
fn a_successful_run_reports_every_field() {
    let json = summary(true, "APPROVE_WITH_COMMENTS", totals(3)).to_json();
    assert_eq!(json["ok"], true);
    assert_eq!(json["error"], Value::Null);
    assert_eq!(json["source"], "anthropic");
    assert_eq!(json["model"], "claude-sonnet-5");
    assert_eq!(json["answer"], "APPROVE_WITH_COMMENTS");
    assert_eq!(json["elapsed_secs"], 14.234);
}

#[test]
fn the_effort_the_requests_carried_is_reported_beside_the_tools() {
    // Both are settings a CI job depends on and cannot confirm from the answer,
    // so both are auditable from the summary alone.
    let mut run = summary(true, "x", totals(1));
    run.effort = Some("xhigh");
    assert_eq!(run.to_json()["effort"], "xhigh");

    // Null covers both "nobody asked" and "the endpoint has no such control":
    // either way the run took the endpoint's own default.
    assert_eq!(
        summary(true, "x", totals(1)).to_json()["effort"],
        Value::Null
    );
}

#[test]
fn usage_totals_are_reported_and_add_up() {
    let json = summary(true, "x", totals(3)).to_json();
    let usage = &json["usage"];
    assert_eq!(usage["input_tokens"], 4085);
    assert_eq!(usage["output_tokens"], 509);
    assert_eq!(usage["cache_read_tokens"], 6837);
    // Reported on its own, because Anthropic bills a write above plain input
    // and a consumer computing cost needs to price it separately.
    assert_eq!(usage["cache_write_tokens"], 2279);
    assert_eq!(usage["reasoning_tokens"], 0);
    assert_eq!(usage["requests"], 3);
}

#[test]
fn the_reported_counts_are_disjoint_and_sum_to_the_total() {
    let json = summary(true, "x", totals(3)).to_json();
    assert_eq!(json["usage"]["total_tokens"], 4085 + 509 + 6837 + 2279);
}

#[test]
fn missing_usage_is_null_not_a_row_of_zeros() {
    // A consumer charting this must be able to tell "the provider sent no usage"
    // from "the run genuinely used nothing", or it silently records zeros.
    let json = summary(true, "x", UsageTotals::default()).to_json();
    assert_eq!(json["usage"], Value::Null);
}

#[test]
fn a_failed_run_says_so_and_carries_the_reason() {
    let mut run = summary(false, "", UsageTotals::default());
    run.error = Some("HTTP 401: authentication_error");
    run.error_kind = Some(ErrorKind::Auth);
    let json = run.to_json();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"], "HTTP 401: authentication_error");
    // The free text is for a human; this is what a workflow branches on.
    assert_eq!(json["error_kind"], "auth");
}

#[test]
fn the_reported_reason_is_the_pair_a_failure_carries() {
    // The two travel together, so a caller reporting the failure and a caller
    // deciding what to do about it read the same object.
    let error = RunError::new("HTTP 429: rate_limit_error", ErrorKind::ProviderHttp);
    assert_eq!(error.message, "HTTP 429: rate_limit_error");
    assert_eq!(error.kind.as_str(), "provider_http");
}

#[test]
fn a_successful_run_has_no_error_kind() {
    let json = summary(true, "done", totals(1)).to_json();
    assert_eq!(json["error_kind"], Value::Null);
}

#[test]
fn every_kind_has_a_stable_wire_value() {
    // Callers branch on these strings, so renaming one silently breaks a
    // workflow's retry rule - the failure this field exists to prevent.
    let pairs = [
        (ErrorKind::Auth, "auth"),
        (ErrorKind::Policy, "policy"),
        (ErrorKind::Input, "input"),
        (ErrorKind::ProviderHttp, "provider_http"),
        (ErrorKind::ProviderStream, "provider_stream"),
        (ErrorKind::Timeout, "timeout"),
        (ErrorKind::NoAnswer, "no_answer"),
        (ErrorKind::Internal, "internal"),
    ];
    for (kind, wire) in pairs {
        assert_eq!(kind.as_str(), wire);
    }
    // The set is closed, so a kind added without a row here would go undocumented
    // and unasserted - which is how a caller's `case` arm silently stops matching.
    assert_eq!(pairs.len(), 8, "every kind needs a pinned wire value");
}

#[test]
fn a_refused_policy_reports_itself_without_naming_a_tool() {
    // The run never started, so the wide set the mistyped policy resolved to must
    // not be reported as what it was permitted to call - publishing that set is
    // exactly what refusing to start avoids.
    let json = RunSummary::refused("--disallowed-tools needs a value", ErrorKind::Policy).to_json();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"], "--disallowed-tools needs a value");
    assert_eq!(json["error_kind"], "policy");
    assert_eq!(json["tools"], json!([]));
    assert_eq!(json["usage"], Value::Null);
    assert_eq!(json["answer"], "");
}

#[test]
fn a_refusal_reports_the_kind_it_was_given() {
    // The two refusals are not the same failure: a policy that cannot be honoured
    // would have run wider than asked, and an unwritable summary file is a path
    // the invocation named that this machine has no answer for.
    let json = RunSummary::refused(
        "can't write the run summary to /nope/run.json",
        ErrorKind::Input,
    )
    .to_json();
    assert_eq!(json["error_kind"], "input");
}

#[test]
fn an_unpriced_run_has_no_cost_key_at_all() {
    // Not a null and not a zero: either one reads as "this run was free" to
    // anything summing the field, and no provider here reports a cost afi could
    // fall back on.
    let json = summary(true, "x", totals(1)).to_json();
    assert!(json["usage"].get("cost_usd").is_none());
    assert!(json.get("cost_usd").is_none());
    assert!(json.get("total_cost_usd").is_none());
}

#[test]
fn a_priced_run_reports_the_figure_beside_the_counts() {
    let mut run = summary(true, "x", totals(3));
    run.cost_usd = Some(0.041_823);
    let json = run.to_json();
    assert_eq!(json["usage"]["cost_usd"], 0.041_823);
    // The counts it was derived from stay put, so the figure is auditable.
    assert_eq!(json["usage"]["total_tokens"], 4085 + 509 + 6837 + 2279);
}

#[test]
fn a_cost_without_usage_to_back_it_is_not_invented() {
    // `usage` is null when no request reported any, and a cost hanging off
    // nothing would be a number with no derivation.
    let mut run = summary(true, "x", UsageTotals::default());
    run.cost_usd = Some(1.0);
    assert_eq!(run.to_json()["usage"], Value::Null);
}

// --- the summary file ---------------------------------------------------------

#[test]
fn a_path_is_taken_only_when_one_was_actually_given() {
    assert_eq!(
        summary_path(Some("  /tmp/run.json  ")),
        Some(PathBuf::from("/tmp/run.json")),
        "surrounding space in an env file must not become part of the path"
    );
    for absent in [None, Some(""), Some("   ")] {
        assert_eq!(
            summary_path(absent),
            None,
            "{absent:?} must not name a file"
        );
    }
}

#[test]
fn naming_a_file_does_not_claim_stdout() {
    // The two are asked for separately. Implying `--summary json` would divert
    // human output to stderr, taking away the readable rendering that wanting a
    // file instead of a pipe was about keeping.
    assert!(!SummaryFormat::from_value(None).is_json());
    assert!(summary_path(Some("/tmp/run.json")).is_some());
}

#[test]
fn the_written_file_is_the_object_and_a_newline() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("run.json");
    let summary = summary(true, "done", totals(2)).to_json();

    write_file(&path, &summary).expect("a fresh path must be writable");

    let body = fs::read_to_string(&path).unwrap();
    assert!(body.ends_with('\n'), "no trailing newline: {body:?}");
    let parsed: Value = serde_json::from_str(&body).expect("the file must parse whole");
    assert_eq!(parsed, summary);
}

#[test]
fn a_rerun_replaces_the_previous_summary_rather_than_appending() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("run.json");

    write_file(&path, &summary(true, "first", totals(1)).to_json()).unwrap();
    write_file(&path, &summary(true, "second", totals(1)).to_json()).unwrap();

    let parsed: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(parsed["answer"], "second");
}

#[test]
fn no_temp_copy_is_left_beside_the_summary() {
    // A workflow collecting `*.json` from the directory should find one file.
    // The temp file's placement and its refusal to follow a planted name are
    // `crate::atomic`'s to prove.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("run.json");

    write_file(&path, &summary(true, "done", totals(1)).to_json()).unwrap();

    let left: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(left.len(), 1, "expected only the summary, got {left:?}");
}

#[test]
fn a_missing_directory_is_refused_before_the_run_and_names_the_path() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("no-such-dir/run.json");

    let error = writable(&path).expect_err("a missing directory must be refused");

    assert!(error.contains("no-such-dir/run.json"), "{error}");
    // And the write agrees, so the check is not a second opinion about the path.
    assert!(write_file(&path, &json!({})).is_err());
}

#[test]
fn a_directory_in_place_of_the_file_is_refused_before_the_run() {
    // The probe writes a sibling, which succeeds beside a directory - only the
    // rename at the end of the run would fail, long after the tokens are spent.
    let dir = TempDir::new().unwrap();
    let error = writable(dir.path()).expect_err("a directory must be refused");
    assert!(error.contains("is a directory"), "{error}");
}

#[test]
fn a_trailing_slash_is_refused_before_the_run_even_where_nothing_exists() {
    // `--summary-file "$OUTDIR/$NAME"` with `NAME` unset. `file_name` strips the
    // separator, so the probe writes an ordinary sibling of the parent and
    // passes; the rename then fails at the end of a run already paid for.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("missing/");

    let error = writable(&path).expect_err("a trailing separator must be refused");

    assert!(error.contains("names a directory"), "{error}");
    // And the write agrees, so the check is not a second opinion about the path.
    assert!(write_file(&path, &json!({})).is_err());
}

#[test]
fn a_writable_path_passes_the_check_without_creating_it() {
    // A summary from a previous run has to stay readable until this one has a
    // whole object to put in its place, so the check must not truncate anything.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("run.json");
    fs::write(&path, "previous\n").unwrap();

    writable(&path).expect("an existing file must be writable");

    assert_eq!(fs::read_to_string(&path).unwrap(), "previous\n");
}

// --- final answer extraction --------------------------------------------------

#[test]
fn the_last_assistant_text_wins() {
    let messages = vec![
        json!({"role": "system", "content": "sys"}),
        json!({"role": "user", "content": "hi"}),
        json!({"role": "assistant", "content": "first"}),
        json!({"role": "user", "content": "more"}),
        json!({"role": "assistant", "content": "final"}),
    ];
    assert_eq!(final_answer(&messages), "final");
}

#[test]
fn tool_call_turns_are_skipped() {
    // turn_finalize writes null content for a tool-call-only turn, and a run ends
    // on one whenever the last thing the model did was call a tool.
    let messages = vec![
        json!({"role": "assistant", "content": "the answer"}),
        json!({"role": "assistant", "content": Value::Null, "tool_calls": [{"id": "1"}]}),
        json!({"role": "tool", "tool_call_id": "1", "content": "result"}),
        json!({"role": "assistant", "content": "   "}),
    ];
    assert_eq!(final_answer(&messages), "the answer");
}

#[test]
fn no_assistant_message_yields_an_empty_answer() {
    let messages = vec![json!({"role": "user", "content": "hi"})];
    assert_eq!(final_answer(&messages), "");
    assert_eq!(final_answer(&[]), "");
}

#[test]
fn array_content_is_not_mistaken_for_text() {
    // compress.rs can leave content as an array of parts; treating that as the
    // answer would put a JSON blob in the report.
    let messages = vec![
        json!({"role": "assistant", "content": "real text"}),
        json!({"role": "assistant", "content": [{"type": "text", "text": "parts"}]}),
    ];
    assert_eq!(final_answer(&messages), "real text");
}
