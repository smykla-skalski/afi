use super::*;
use crate::tools::known_tool_names;
use serde_json::json;

mod refusals;
pub(in crate::summary) mod shape;
mod version;

pub(in crate::summary) fn totals(requests: u64) -> UsageTotals {
    UsageTotals {
        input_tokens: 4085,
        output_tokens: 509,
        cache_read_tokens: 6837,
        cache_write_tokens: 2279,
        reasoning_tokens: 0,
        requests,
    }
}

/// The breakdown a one-source run has: the whole of `usage` under the name that
/// paid, and nothing at all when nothing was billed.
fn billed_by(usage: UsageTotals) -> Vec<SourceSpend<'static>> {
    if usage.is_empty() {
        return Vec::new();
    }
    vec![SourceSpend {
        source: "anthropic".to_string(),
        usage,
        cost_usd: None,
        auth: Some(RunAuth::ApiKey),
    }]
}

pub(in crate::summary) fn summary(ok: bool, answer: &str, usage: UsageTotals) -> RunSummary<'_> {
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
        refused_tool_calls: RefusedToolCalls::default(),
        auth: Some(RunAuth::ApiKey),
        // Derived from `usage` rather than written unconditionally, so a fixture
        // for a run that billed nothing cannot hand a test an entry of zeros -
        // a shape `spend_by_source` has no way to produce, since it maps over
        // ledger entries and every one of those was recorded by a request.
        sources: billed_by(usage),
        system_prompt_mode: Some("builtin"),
        system_prompt_file: None,
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
fn the_prompt_the_run_used_is_named() {
    // The whole point of reporting it: a job told to review under its own
    // instructions and one that quietly used afi's are otherwise identical here.
    let unconfigured = summary(true, "done", totals(1)).to_json();
    assert_eq!(unconfigured["system_prompt"]["mode"], "builtin");
    assert_eq!(unconfigured["system_prompt"]["file"], Value::Null);

    let mut run = summary(true, "done", totals(1));
    run.system_prompt_mode = Some("replace");
    run.system_prompt_file = Some("ci/review.md");
    let configured = run.to_json();
    assert_eq!(configured["system_prompt"]["mode"], "replace");
    assert_eq!(configured["system_prompt"]["file"], "ci/review.md");
}

#[test]
fn a_refused_run_names_no_prompt_at_all() {
    // Null rather than `builtin`, for the reason `tools` is empty and `effort` is
    // null there: the run never started, so it sent no prompt, and reporting the
    // built-in one would read as an ordinary unconfigured run that did.
    let json =
        RunSummary::refused("the system prompt at \"p.md\" is empty", ErrorKind::Input).to_json();
    assert_eq!(json["system_prompt"], Value::Null);
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

// --- what each source spent ---------------------------------------------------

#[test]
fn a_single_source_run_reads_the_same_as_it_always_did() {
    // The breakdown is a key a consumer can ignore. Everything it was already
    // reading has to keep the value it had, or an added field is a breaking one.
    let json = summary(true, "x", totals(3)).to_json();
    assert_eq!(json["source"], "anthropic");
    assert_eq!(json["auth"]["mode"], "api_key");
    assert_eq!(json["usage"]["total_tokens"], 4085 + 509 + 6837 + 2279);
    // And the one entry says what the flat fields do, under the name that paid.
    assert_eq!(json["sources"].as_array().expect("an array").len(), 1);
    assert_eq!(json["sources"][0]["source"], json["source"]);
    assert_eq!(json["sources"][0]["auth"], json["auth"]);
}

/// A run that spent 3 of its 4 requests on `local` and the last on `bedrock`,
/// which is the case the run-level `auth` answers null for.
fn switched_session() -> RunSummary<'static> {
    let mut run = summary(true, "x", totals(3));
    run.auth = None;
    run.sources = vec![
        SourceSpend {
            source: "local".to_string(),
            usage: totals(3),
            cost_usd: Some(0.02),
            auth: Some(RunAuth::ApiKey),
        },
        SourceSpend {
            source: "bedrock".to_string(),
            usage: UsageTotals {
                input_tokens: 100,
                requests: 1,
                ..UsageTotals::default()
            },
            cost_usd: Some(0.000_5),
            auth: Some(RunAuth::SigV4 {
                region: "us-east-1",
                access_key_id: "AKIAEXAMPLE",
            }),
        },
    ];
    run.usage = UsageTotals {
        input_tokens: totals(3).input_tokens + 100,
        requests: 4,
        ..totals(3)
    };
    run
}

#[test]
fn a_switched_session_reports_each_source_and_the_credential_that_paid_it() {
    // Nothing else in the summary says that the personal key bought the first
    // 4085 input tokens and the assumed role the last 100.
    let json = switched_session().to_json();
    assert_eq!(json["auth"], Value::Null, "no single credential paid");
    assert_eq!(json["sources"][0]["source"], "local");
    assert_eq!(json["sources"][0]["auth"]["mode"], "api_key");
    assert_eq!(json["sources"][0]["usage"]["cost_usd"], 0.02);
    assert_eq!(json["sources"][1]["source"], "bedrock");
    assert_eq!(json["sources"][1]["auth"]["access_key_id"], "AKIAEXAMPLE");
}

// The invariant a reader looks for here - that the entries sum to the flat block
// - is not asserted in this file. A fixture writes both sides, so the assertion
// would only prove the fixture author added up. It is proved where the two are
// actually derived from one another: `usage_totals::tests` folds a real ledger,
// `repl::report::tests` derives the entries from one, and
// `tests/summary_sources.rs` sums them off a real run's stdout.

#[test]
fn a_run_that_billed_nothing_breaks_down_into_nothing() {
    // A refusal never reached a source, and a source that was configured and
    // never sent a request has nothing to report. Both are the empty list.
    let refused = RunSummary::refused("--disallowed-tools needs a value", ErrorKind::Policy);
    assert_eq!(refused.to_json()["sources"], json!([]));

    let mut failed = summary(false, "", UsageTotals::default());
    failed.error = Some("HTTP 401: authentication_error");
    failed.error_kind = Some(ErrorKind::Auth);
    let json = failed.to_json();
    assert_eq!(json["sources"], json!([]));
    // The credential it tried is still named, which is what a failed run has to
    // show - the breakdown reports spend, and there was none.
    assert_eq!(json["auth"]["mode"], "api_key");
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
