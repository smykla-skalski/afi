//! End-to-end: what the run summary says each source spent.
//!
//! A session that `/source`-switches is the only run this block exists for, and
//! it is the one run a unit test cannot stage: the split comes out of a
//! process-wide ledger that a real switch writes to, and the credentials come
//! out of a runtime that a real environment builds. This drives the binary
//! through the switch and reads the breakdown off stdout.

mod common;

use std::process::Output;

use serde_json::Value;
use tempfile::TempDir;

use common::{billing_server, run_afi, summary_of};

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

/// The AWS secret behind `second`'s signature. Never in the summary - the
/// access key id beside it is, and the pair is what makes the difference
/// testable.
const AWS_SECRET: &str = "aws-secret-never-published";
const AWS_KEY_ID: &str = "AKIAEXAMPLESECOND";

/// A piped session against the two sources, driven by `input`.
///
/// The two authenticate differently on purpose. Both on a static key would make
/// every assertion about a credential pass whichever source it was read off,
/// which is the misattribution the breakdown exists to prevent: a stored key and
/// an AWS signature are what tell the two entries apart.
///
/// `third` is configured and never switched to, which is the case the breakdown
/// has to leave out: a source that was available is not a source that was
/// billed. The AWS credentials register a built-in `bedrock` source nobody asked
/// for, which is a second one of those and rides along for free.
fn run_session(prices: Option<&str>, input: &str) -> Output {
    let addr = billing_server(&billed_usage(), 16);
    let base_url = format!("http://{addr}/v1");
    let home = TempDir::new().expect("a temporary home");
    let mut env = vec![
        ("AFI_ACTIVE", "first"),
        ("AFI_SOURCE_FIRST_BASE_URL", base_url.as_str()),
        ("AFI_SOURCE_FIRST_MODEL", "model-one"),
        ("AFI_SOURCE_FIRST_API_KEY", "sk-first"),
        ("AFI_SOURCE_SECOND_BASE_URL", base_url.as_str()),
        ("AFI_SOURCE_SECOND_MODEL", "model-two"),
        ("AFI_SOURCE_SECOND_PROTOCOL", "aws-bedrock-openai"),
        ("AWS_REGION", "us-east-1"),
        ("AWS_ACCESS_KEY_ID", AWS_KEY_ID),
        ("AWS_SECRET_ACCESS_KEY", AWS_SECRET),
        ("AFI_SOURCE_THIRD_BASE_URL", base_url.as_str()),
        ("AFI_SOURCE_THIRD_MODEL", "model-three"),
    ];
    env.extend(prices.map(|prices| ("AFI_PRICES", prices)));
    run_afi(home.path(), &["--summary", "json"], &env, input)
}

/// A session that spends on `first`, switches, and spends on `second`.
fn run_switched_session(prices: Option<&str>) -> Output {
    run_session(prices, "hi\n/source second\nhi again\n/quit\n")
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
    // Read as tuples so the whole breakdown is one assertion rather than a
    // column of them.
    let sources = summary["sources"].as_array().expect("an array");
    let reported: Vec<(&str, u64, f64, &str)> = sources
        .iter()
        .map(|entry| {
            (
                entry["source"].as_str().expect("a source name"),
                entry["usage"]["input_tokens"].as_u64().expect("a count"),
                entry["usage"]["cost_usd"].as_f64().expect("a figure"),
                entry["auth"]["mode"].as_str().expect("a credential mode"),
            )
        })
        .collect();
    assert_eq!(
        reported,
        vec![
            ("first", PROMPT_TOKENS, FIRST_USD, "api_key"),
            ("second", PROMPT_TOKENS, SECOND_USD, "sigv4"),
        ],
        "{summary}"
    );
    // The identifiers travel with the entry too, not just the mode. Reading them
    // off the source that ended the session would name this one for both.
    assert_eq!(sources[1]["auth"]["access_key_id"], AWS_KEY_ID);
    assert_eq!(sources[1]["auth"]["region"], "us-east-1");
    assert!(
        sources[0]["auth"].get("access_key_id").is_none(),
        "a static key identifies nothing: {summary}"
    );
}

#[test]
fn the_breakdown_accounts_for_the_run_and_nothing_more() {
    let output = run_switched_session(Some(RATES));
    let summary = summary_of(&output);
    let sources = summary["sources"].as_array().expect("an array");

    // A breakdown that does not add up to the totals beside it is worse than no
    // breakdown: both figures get charted and one of them is wrong.
    let summed: u64 = sources
        .iter()
        .map(|entry| entry["usage"]["total_tokens"].as_u64().expect("a count"))
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

    // The block sits one field away from two credentials, and this JSON is what
    // CI uploads as an unmasked artifact. The AWS access key id is the one
    // identifier here that is safe to publish; its secret half is not.
    let stdout = String::from_utf8(output.stdout).expect("stdout must be utf-8");
    for secret in ["sk-first", AWS_SECRET] {
        assert!(
            !stdout.contains(secret),
            "the summary leaked {secret}: {stdout}"
        );
    }
}

#[test]
fn a_source_returned_to_accumulates_rather_than_appearing_twice() {
    // The one sequence that could silently produce a second entry for one
    // source: `record` finds the source it already has or appends a new one, and
    // a name that came back through the turn loop spelled differently would
    // append. Two `first` entries would still sum to the flat totals, so nothing
    // else in the summary would look wrong - the breakdown would just report a
    // budget twice, at half its spend each.
    let summary = summary_of(&run_session(
        Some(RATES),
        "hi\n/source second\nhi again\n/source first\nhi once more\n/quit\n",
    ));
    let sources = summary["sources"].as_array().expect("an array");
    let reported: Vec<(&str, u64, u64)> = sources
        .iter()
        .map(|entry| {
            (
                entry["source"].as_str().expect("a source name"),
                entry["usage"]["input_tokens"].as_u64().expect("a count"),
                entry["usage"]["requests"].as_u64().expect("a count"),
            )
        })
        .collect();
    assert_eq!(
        reported,
        vec![
            ("first", 2 * PROMPT_TOKENS, 2),
            ("second", PROMPT_TOKENS, 1),
        ],
        "the source it came back to keeps its place and its running total: {summary}"
    );
    // Both turns on `first` were billed at `first`'s rates, not at whichever
    // source the run happened to be on when the ledger was read.
    assert_eq!(sources[0]["usage"]["cost_usd"], 2.0 * FIRST_USD);
    assert_eq!(
        summary["source"], "first",
        "the session ends where it began"
    );
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
