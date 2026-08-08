//! End-to-end: what the run summary says each source spent.
//!
//! A session that `/source`-switches is the only run this block exists for, and
//! it is the one run a unit test cannot stage: the split comes out of a
//! process-wide ledger that a real switch writes to, and the credentials come
//! out of a runtime that a real environment builds. This drives the binary
//! through the switch and reads the breakdown off stdout.

mod common;

use std::io::Write;
use std::process::{Command, Output, Stdio};

use serde_json::Value;
use tempfile::TempDir;

use common::{billing_server, summary_of};

/// What the fake endpoint reports for every completion. A round million of each
/// so the money below is checkable by eye.
const PROMPT_TOKENS: u64 = 1_000_000;
const COMPLETION_TOKENS: u64 = 1_000_000;

/// The two sources run different models, which is what makes the per-source
/// figures more than a division of one bill: each is priced at its own rates.
const RATES: &str = r#"{"model-one": {"input": 1, "output": 2},
                        "model-two": {"input": 10, "output": 20}}"#;

/// 1M x $1 + 1M x $2, and 1M x $10 + 1M x $20.
const FIRST_USD: f64 = 3.0;
const SECOND_USD: f64 = 30.0;

fn billed_usage() -> String {
    format!(r#"{{"prompt_tokens":{PROMPT_TOKENS},"completion_tokens":{COMPLETION_TOKENS}}}"#)
}

/// A piped session that spends on `first`, switches, and spends on `second`.
///
/// `third` is configured and never switched to, which is the case the breakdown
/// has to leave out: a source that was available is not a source that was billed.
fn run_switched_session(prices: Option<&str>) -> Output {
    let addr = billing_server(&billed_usage(), 12);
    let base_url = format!("http://{addr}/v1");
    let home = TempDir::new().expect("a temporary home");
    let mut command = Command::new(env!("CARGO_BIN_EXE_afi"));
    command
        .args(["--summary", "json"])
        .env_clear()
        .env("AFI_HOME", home.path())
        .env("HOME", home.path())
        .env("AFI_ACTIVE", "first")
        .env("AFI_SOURCE_FIRST_BASE_URL", &base_url)
        .env("AFI_SOURCE_FIRST_MODEL", "model-one")
        .env("AFI_SOURCE_FIRST_API_KEY", "sk-first")
        .env("AFI_SOURCE_SECOND_BASE_URL", &base_url)
        .env("AFI_SOURCE_SECOND_MODEL", "model-two")
        .env("AFI_SOURCE_SECOND_API_KEY", "sk-second")
        .env("AFI_SOURCE_THIRD_BASE_URL", &base_url)
        .env("AFI_SOURCE_THIRD_MODEL", "model-three")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(prices) = prices {
        command.env("AFI_PRICES", prices);
    }
    let mut child = command.spawn().expect("afi must start");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(b"hi\n/source second\nhi again\n/quit\n")
        .expect("the input must write");
    child.wait_with_output().expect("afi must exit")
}

/// Every entry's value at `field`, in the order the summary reported them.
fn column<'a>(sources: &'a [Value], field: &str) -> Vec<&'a Value> {
    sources.iter().map(|entry| &entry["usage"][field]).collect()
}

/// What one entry attributes: the source, what it spent, what that cost, and
/// the credential that paid - read as a tuple so the whole breakdown is one
/// assertion rather than a column of them.
fn attributed(entry: &Value) -> (&str, u64, f64, &str) {
    (
        entry["source"].as_str().expect("a source name"),
        entry["usage"]["input_tokens"].as_u64().expect("a count"),
        entry["usage"]["cost_usd"].as_f64().expect("a figure"),
        entry["auth"]["mode"].as_str().expect("a credential mode"),
    )
}

#[test]
fn each_source_reports_its_own_counts_its_own_cost_and_its_own_credential() {
    let summary = summary_of(&run_switched_session(Some(RATES)));
    assert_eq!(
        summary["auth"],
        Value::Null,
        "two credentials paid, so none is the run's: {summary}"
    );

    // The attribution the flat block cannot make: this budget bought these
    // tokens, at this model's rates - in the order the run first spent on them.
    let sources = summary["sources"].as_array().expect("an array");
    let reported: Vec<(&str, u64, f64, &str)> = sources.iter().map(attributed).collect();
    assert_eq!(
        reported,
        vec![
            ("first", PROMPT_TOKENS, FIRST_USD, "api_key"),
            ("second", PROMPT_TOKENS, SECOND_USD, "api_key"),
        ],
        "{summary}"
    );
}

#[test]
fn the_breakdown_accounts_for_the_run_and_nothing_more() {
    let output = run_switched_session(Some(RATES));
    let summary = summary_of(&output);
    let sources = summary["sources"].as_array().expect("an array");

    // A breakdown that does not add up to the totals beside it is worse than no
    // breakdown: both figures get charted and one of them is wrong.
    let summed: u64 = column(sources, "total_tokens")
        .iter()
        .map(|count| count.as_u64().expect("a number"))
        .sum();
    assert_eq!(summed, summary["usage"]["total_tokens"]);
    assert_eq!(
        summary["usage"]["cost_usd"],
        FIRST_USD + SECOND_USD,
        "the flat figure is what the entries add to: {summary}"
    );

    // `third` was configured and never billed. An entry of zeros for it would
    // read as a source that ran for free.
    assert!(
        sources.len() == 2,
        "a source that spent nothing has nothing to report: {summary}"
    );

    // The block sits one field away from two API keys, and this JSON is what CI
    // uploads as an unmasked artifact.
    let stdout = String::from_utf8(output.stdout).expect("stdout must be utf-8");
    for key in ["sk-first", "sk-second"] {
        assert!(!stdout.contains(key), "the summary leaked {key}: {stdout}");
    }
}

#[test]
fn an_unpriced_run_still_says_who_spent_what() {
    // The counts and the credential do not depend on a rate table, so a run with
    // no prices still attributes its tokens - it just reports no money, the same
    // way the flat block does.
    let summary = summary_of(&run_switched_session(None));
    let sources = summary["sources"].as_array().expect("an array");
    assert_eq!(sources.len(), 2);
    for entry in sources {
        assert_eq!(entry["usage"]["input_tokens"], PROMPT_TOKENS);
        assert!(
            entry["usage"].get("cost_usd").is_none(),
            "an unpriced entry carries no cost key at all: {entry}"
        );
    }
}
