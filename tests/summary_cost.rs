//! End-to-end: a real `afi` process, a real HTTP endpoint, and the cost figure
//! the run summary prints.
//!
//! Every piece of this is unit tested on its own - the rate table, the per-model
//! accumulator, the JSON assembly - and the wiring between them is what a unit
//! test cannot see. This runs the binary against a server that reports known
//! token counts and checks the money that comes out of stdout.

mod common;

use std::fs;
use std::net::SocketAddr;
use std::path::Path;
use std::process::Output;

use serde_json::Value;
use tempfile::TempDir;

use common::{billing_server, run_afi, summary_of};

/// Usage the fake endpoint reports, chosen so the arithmetic is checkable by
/// eye: a million tokens of each kind bills at exactly the per-million rate.
const PROMPT_TOKENS: u64 = 3_000_000;
const CACHED_TOKENS: u64 = 2_000_000;
const COMPLETION_TOKENS: u64 = 1_000_000;

/// `input` is `prompt_tokens` minus the cached subset, so this run spends 1M
/// input, 2M cache read, and 1M output.
const RATES: &str = r#"{"test-model": {"input": 3, "output": 15, "cache_read": 0.3}}"#;

/// 1M x $3 + 1M x $15 + 2M x $0.30.
const EXPECTED_USD: f64 = 18.6;

/// Usage the fake endpoint reports for every completion.
fn billed_usage() -> String {
    format!(
        r#"{{"prompt_tokens":{PROMPT_TOKENS},"completion_tokens":{COMPLETION_TOKENS},"prompt_tokens_details":{{"cached_tokens":{CACHED_TOKENS}}}}}"#
    )
}

/// A one-shot run against the canned endpoint, with the rate table and the
/// summary path the case is about.
fn run_one_shot(
    home: &TempDir,
    addr: SocketAddr,
    prices: Option<&str>,
    summary_file: Option<&Path>,
) -> Output {
    let base_url = format!("http://{addr}/v1");
    let mut env = vec![
        ("AFI_BASE_URL", base_url.as_str()),
        ("AFI_MODEL", "test-model"),
    ];
    env.extend(prices.map(|prices| ("AFI_PRICES", prices)));
    let mut args = vec!["--yolo", "--summary", "json", "-f", "-"];
    if let Some(path) = summary_file {
        args.extend(["--summary-file", path.to_str().expect("a utf-8 path")]);
    }
    run_afi(home.path(), &args, &env, "say done\n")
}

#[test]
fn a_priced_run_reports_what_it_cost() {
    let addr = billing_server(&billed_usage(), 8);
    let home = TempDir::new().unwrap();

    let priced = summary_of(&run_one_shot(&home, addr, Some(RATES), None));
    let usage = &priced["usage"];
    assert_eq!(usage["input_tokens"], PROMPT_TOKENS - CACHED_TOKENS);
    assert_eq!(usage["cache_read_tokens"], CACHED_TOKENS);
    assert_eq!(usage["output_tokens"], COMPLETION_TOKENS);
    assert_eq!(
        usage["cost_usd"], EXPECTED_USD,
        "each class must be billed at its own rate"
    );

    // Same run, no rate table: the counts stay, the money goes.
    let unpriced = summary_of(&run_one_shot(&home, addr, None, None));
    assert_eq!(unpriced["usage"]["total_tokens"], usage["total_tokens"]);
    assert!(
        unpriced["usage"].get("cost_usd").is_none(),
        "an unpriced run must carry no cost key at all"
    );
}

#[test]
fn one_source_spending_everything_breaks_down_into_the_flat_block() {
    // The compatibility half of the breakdown: a run that never switched has one
    // entry saying what the flat fields say, under the name that paid, so a
    // consumer reading those needs no change at all.
    let addr = billing_server(&billed_usage(), 4);
    let home = TempDir::new().unwrap();
    let summary = summary_of(&run_one_shot(&home, addr, Some(RATES), None));

    let sources = summary["sources"].as_array().expect("an array");
    assert_eq!(sources.len(), 1, "one source was billed: {summary}");
    assert_eq!(sources[0]["source"], "local");
    assert_eq!(sources[0]["auth"], summary["auth"]);
    // The entry is the flat block itself, minus the three refusal counts - those
    // belong to calls that were never sent and so to no source.
    let mut flat = summary["usage"].as_object().expect("an object").clone();
    flat.retain(|name, _| !name.starts_with("refused"));
    assert_eq!(sources[0]["usage"], Value::Object(flat));
}

#[test]
fn a_run_that_worked_writes_the_same_object_to_the_summary_file() {
    // The `--summary-file` tests elsewhere all run against a closed port, which
    // leaves the case a workflow actually collects unproven: a run that answered,
    // its answer on the path, and the flag not turning a good run into a bad
    // exit code.
    let addr = billing_server(&billed_usage(), 4);
    let home = TempDir::new().unwrap();
    let path = home.path().join("run.json");

    let output = run_one_shot(&home, addr, Some(RATES), Some(&path));

    assert!(
        output.status.success(),
        "a written summary must not fail the run: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body = fs::read_to_string(&path).expect("the summary file must exist");
    let written: Value = serde_json::from_str(&body).expect("the file must parse whole");
    assert_eq!(written["ok"], true);
    assert_eq!(written["answer"], "done");
    assert_eq!(written["usage"]["cost_usd"], EXPECTED_USD);
    // One object, two channels: they are rendered from a single build.
    assert_eq!(written, summary_of(&output));
}
