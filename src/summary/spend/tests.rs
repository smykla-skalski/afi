use super::*;
use crate::summary::tests::shape::sorted_keys;

fn totals(input: u64, output: u64, requests: u64) -> UsageTotals {
    UsageTotals {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: 0,
        requests,
        estimated_tokens: 0,
    }
}

fn spend(source: &str, usage: UsageTotals, auth: RunAuth<'static>) -> SourceSpend<'static> {
    SourceSpend {
        source: source.to_string(),
        usage,
        cost_usd: None,
        auth: Some(auth),
    }
}

/// Two sources that both spent, the case the whole block exists for.
fn switched_session() -> Value {
    SourceSpend::json(&[
        spend("local", totals(1000, 50, 1), RunAuth::ApiKey),
        spend(
            "anthropic",
            totals(400, 20, 2),
            RunAuth::Federated {
                organization_id: "org_acme",
                service_account_id: "svac_ci",
                workspace_id: None,
                federation_rule_id: "fdrl_review",
            },
        ),
    ])
}

#[test]
fn each_entry_names_its_source_and_the_counts_that_source_spent() {
    let json = switched_session();
    assert_eq!(json.as_array().expect("an array").len(), 2);
    assert_eq!(json[0]["source"], "local");
    assert_eq!(json[0]["usage"]["input_tokens"], 1000);
    assert_eq!(json[0]["usage"]["requests"], 1);
    assert_eq!(json[1]["source"], "anthropic");
    assert_eq!(json[1]["usage"]["total_tokens"], 420);
}

#[test]
fn the_credential_travels_in_the_entry_rather_than_beside_it() {
    // The whole point: a switched session attributes its spend without a second
    // lookup, and the run-level `auth` has no answer to give here at all.
    let json = switched_session();
    assert_eq!(json[0]["auth"]["mode"], "api_key");
    assert_eq!(json[1]["auth"]["mode"], "federated");
    assert_eq!(json[1]["auth"]["service_account_id"], "svac_ci");
}

#[test]
fn the_shape_of_an_entry_is_pinned_like_the_object_around_it() {
    // Published keys, so renaming one breaks a consumer that worked and has to
    // move `SCHEMA_VERSION` with it.
    let json = SourceSpend::json(&[spend("local", totals(10, 2, 1), RunAuth::ApiKey)]);
    assert_eq!(sorted_keys(&json[0]), ["auth", "source", "usage"]);
    // The counts an entry reports are the run's own, minus the two that belong
    // to no request - see `entry_json`.
    assert_eq!(
        sorted_keys(&json[0]["usage"]),
        [
            "cache_read_tokens",
            "cache_write_tokens",
            "input_tokens",
            "output_tokens",
            "reasoning_tokens",
            "requests",
            "total_tokens",
        ]
    );
}

#[test]
fn a_refusal_is_not_attributed_to_a_source() {
    // A refused call was never sent, so no source was billed for it and no entry
    // can carry it. Reporting the run's count in every entry would multiply it.
    let json = SourceSpend::json(&[spend("local", totals(10, 2, 1), RunAuth::ApiKey)]);
    for key in [
        "refused_tool_calls",
        "refused_by_policy",
        "refused_by_approval",
    ] {
        assert!(
            json[0]["usage"].get(key).is_none(),
            "{key} belongs to the run, not to a source: {json}"
        );
    }
}

#[test]
fn an_unpriced_entry_carries_no_cost_key_either() {
    // Same rule as the flat figure, for the same reason: a zero or a null reads
    // as "this source was free" to anything summing the field.
    let unpriced = SourceSpend::json(&[spend("local", totals(10, 2, 1), RunAuth::ApiKey)]);
    assert!(unpriced[0]["usage"].get("cost_usd").is_none());

    let mut entry = spend("local", totals(10, 2, 1), RunAuth::ApiKey);
    entry.cost_usd = Some(0.001_25);
    assert_eq!(
        SourceSpend::json(&[entry])[0]["usage"]["cost_usd"],
        0.001_25
    );
}

#[test]
fn nothing_billed_is_an_empty_list() {
    // Not null: there is no zero row to be misread here, so the empty set says
    // what it means and iterates like any other.
    assert_eq!(SourceSpend::json(&[]), json!([]));
}

#[test]
fn a_source_that_left_the_runtime_still_reports_what_it_spent() {
    // `auth` null is afi declining to name the credential, as it is at the top
    // level. The counts are what the run observed and stand on their own.
    let entry = SourceSpend {
        source: "gone".to_string(),
        usage: totals(5, 1, 1),
        cost_usd: None,
        auth: None,
    };
    let json = SourceSpend::json(&[entry]);
    assert_eq!(json[0]["auth"], Value::Null);
    assert_eq!(json[0]["usage"]["input_tokens"], 5);
}
