//! What the three refusal counts put in the JSON.
//!
//! Split out from the rest because they answer a different question: everything
//! in `super` is about a run reporting what it did, and these are about a run
//! reporting what it was not allowed to do. The arithmetic behind them - a total
//! derived from its halves rather than tracked beside them - belongs to
//! `RefusedToolCalls` and is proved with it.

use super::{summary, totals};
use crate::model::usage_totals::{RefusedToolCalls, UsageTotals};

#[test]
fn a_run_that_refused_nothing_still_reports_the_counts() {
    // Zeros rather than absent keys, or a caller cannot tell "nothing was refused"
    // from "this afi is too old to say".
    let json = summary(true, "x", totals(3)).to_json();
    assert_eq!(json["usage"]["refused_tool_calls"], 0);
    assert_eq!(json["usage"]["refused_by_policy"], 0);
    assert_eq!(json["usage"]["refused_by_approval"], 0);
}

#[test]
fn refusals_are_split_by_what_refused_them_and_sum_to_the_total() {
    // The split is the point: a policy block means the model reached for a tool the
    // caller ruled out, while an unattended run with no --yolo denies every mutating
    // call at the gate. One number covering both cannot be alerted on.
    let mut run = summary(true, "x", totals(3));
    run.refused_tool_calls = RefusedToolCalls {
        by_policy: 2,
        by_approval: 1,
    };
    let json = run.to_json();
    assert_eq!(json["usage"]["refused_by_policy"], 2);
    assert_eq!(json["usage"]["refused_by_approval"], 1);
    assert_eq!(json["usage"]["refused_tool_calls"], 3);
    // The counts are not a token class and must not move the arithmetic.
    assert_eq!(json["usage"]["total_tokens"], 4085 + 509 + 6837 + 2279);
}

#[test]
fn a_refusal_is_reported_even_when_the_provider_sent_no_usage() {
    // The hole this closes: an endpoint that reports no tokens used to null the
    // whole object, so a run that was attacked and one that was not read alike.
    for refused in [
        RefusedToolCalls {
            by_policy: 1,
            by_approval: 0,
        },
        RefusedToolCalls {
            by_policy: 0,
            by_approval: 1,
        },
    ] {
        let mut run = summary(true, "x", UsageTotals::default());
        run.refused_tool_calls = refused;
        let json = run.to_json();
        assert_eq!(json["usage"]["refused_tool_calls"], 1, "{refused:?}");
        assert_eq!(
            json["usage"]["requests"], 0,
            "the silent provider is still distinguishable"
        );
    }
}
