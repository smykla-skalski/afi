use super::*;
use serde_json::json;

fn totals(turns: u64) -> UsageTotals {
    UsageTotals {
        input_tokens: 4085,
        output_tokens: 509,
        cache_read_tokens: 6837,
        reasoning_tokens: 0,
        turns,
    }
}

fn summary(ok: bool, answer: &str, usage: UsageTotals) -> RunSummary<'_> {
    RunSummary {
        ok,
        error: None,
        source: Some("anthropic"),
        model: Some("claude-sonnet-5"),
        answer,
        usage,
        elapsed_secs: 14.2341,
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
fn usage_totals_are_reported_and_add_up() {
    let json = summary(true, "x", totals(3)).to_json();
    let usage = &json["usage"];
    assert_eq!(usage["input_tokens"], 4085);
    assert_eq!(usage["output_tokens"], 509);
    assert_eq!(usage["cache_read_tokens"], 6837);
    assert_eq!(usage["reasoning_tokens"], 0);
    assert_eq!(usage["turns"], 3);
    // The four counts are disjoint, so the total must be their sum.
    assert_eq!(usage["total_tokens"], 4085 + 509 + 6837);
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
    let json = run.to_json();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"], "HTTP 401: authentication_error");
}

#[test]
fn no_cost_field_is_emitted() {
    // Deliberate: no provider returns cost here, so any figure would come from a
    // hard-coded price table that goes stale without anyone noticing.
    let json = summary(true, "x", totals(1)).to_json();
    assert!(json.get("cost_usd").is_none());
    assert!(json.get("total_cost_usd").is_none());
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
