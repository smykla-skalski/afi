//! The block these render into is the artifact half of the summary, so the key
//! set is asserted exhaustively rather than field by field.

use serde_json::Value;

use super::*;
use crate::summary::tests::shape::sorted_keys;

fn federated() -> RunAuth<'static> {
    federated_in(Some("wrkspc_reviews"))
}

/// The same credential on a rule that may or may not span workspaces.
fn federated_in(workspace_id: Option<&'static str>) -> RunAuth<'static> {
    RunAuth::Federated {
        organization_id: "org_abc",
        service_account_id: "svac_ci",
        workspace_id,
        federation_rule_id: "fdrl_pr",
    }
}

#[test]
fn a_federated_run_names_the_credential_and_the_workspace_that_paid() {
    // Without this block a job that fell back to a personal key is byte-for-byte
    // the same summary as one that used the service account it was meant to.
    let auth = RunAuth::json(Some(federated()));
    assert_eq!(auth["mode"], "federated");
    assert_eq!(auth["organization_id"], "org_abc");
    assert_eq!(auth["service_account_id"], "svac_ci");
    assert_eq!(auth["workspace_id"], "wrkspc_reviews");
    assert_eq!(auth["federation_rule_id"], "fdrl_pr");
}

#[test]
fn a_static_key_run_names_the_mode_and_stops() {
    // Empty strings here would read as identifiers afi failed to capture rather
    // than as a credential that has none.
    let auth = RunAuth::json(Some(RunAuth::ApiKey));
    assert_eq!(auth["mode"], "api_key");
    for absent in [
        "organization_id",
        "service_account_id",
        "workspace_id",
        "federation_rule_id",
    ] {
        assert!(auth.get(absent).is_none(), "{absent} must be left out");
    }
}

#[test]
fn a_rule_covering_one_workspace_omits_the_workspace_rather_than_blanking_it() {
    let auth = RunAuth::json(Some(federated_in(None)));
    assert!(auth.get("workspace_id").is_none());
    assert_eq!(auth["service_account_id"], "svac_ci");
}

#[test]
fn a_run_with_no_source_reports_no_credential() {
    assert_eq!(RunAuth::json(None), Value::Null);
}

#[test]
fn the_block_carries_nothing_but_identifiers() {
    // The whole summary is uploaded as a build artifact, where nothing is
    // masked, so the key set is asserted exhaustively: a field added to
    // `RunAuth` has to come through this test rather than through an artifact.
    let json = RunAuth::json(Some(federated()));
    assert_eq!(
        sorted_keys(&json),
        [
            "federation_rule_id",
            "mode",
            "organization_id",
            "service_account_id",
            "workspace_id"
        ]
    );
}
