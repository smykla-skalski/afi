use super::*;

fn federation() -> Federation {
    Federation {
        rule_id: "fdrl_1".to_string(),
        organization_id: "org".to_string(),
        service_account_id: "svac".to_string(),
        workspace_id: None,
        identity: None,
    }
}

fn federated_protocol() -> Protocol {
    Protocol::AnthropicFederated(Box::new(federation()))
}

/// A source on `protocol` holding a real credential.
fn keyed_source(protocol: Protocol) -> Source {
    source(protocol, Some("sk-real-credential"))
}

/// A source on `protocol` with nothing configured, so `Source::new` stores the
/// placeholder.
fn keyless_source(protocol: Protocol) -> Source {
    source(protocol, None)
}

fn source(protocol: Protocol, api_key: Option<&str>) -> Source {
    Source::new(
        "test",
        "https://api.anthropic.com".to_string(),
        api_key.map(ToString::to_string),
        None,
        None,
        None,
    )
    .with_protocol(protocol)
}

// --- what the run summary reports ---------------------------------------------

#[test]
fn a_federated_run_reports_the_ids_the_grant_sent() {
    // The fixture rule covers one workspace, so the exchange sends no
    // `workspace_id` and there is none to report.
    assert_eq!(
        keyed_source(federated_protocol()).run_auth(),
        RunAuth::Federated {
            organization_id: "org",
            service_account_id: "svac",
            workspace_id: None,
            federation_rule_id: "fdrl_1",
        }
    );
}

#[test]
fn a_workspace_scoped_rule_reports_the_workspace_it_billed() {
    let mut federation = federation();
    federation.workspace_id = Some("wrkspc_ci".to_string());
    let source = keyed_source(Protocol::AnthropicFederated(Box::new(federation)));
    assert!(matches!(
        source.run_auth(),
        RunAuth::Federated {
            workspace_id: Some("wrkspc_ci"),
            ..
        }
    ));
}

/// The mode is minted, not stored, so the placeholder in `api_key` says nothing
/// about it. Reporting `none` here would deny a credential the run does have.
#[test]
fn a_federated_source_reports_federated_despite_holding_the_placeholder() {
    let source = keyless_source(federated_protocol());
    assert_eq!(source.api_key, NOOP_KEY, "the fixture must hold no key");
    assert_eq!(source.run_auth().mode(), "federated");
}

#[test]
fn each_mode_is_named_by_how_the_credential_was_obtained() {
    // `OpenAiCompat` is an api key too: a static value out of the environment,
    // differing from `AnthropicApiKey` only in which header carries it.
    for (protocol, mode) in [
        (Protocol::AnthropicApiKey, "api_key"),
        (Protocol::OpenAiCompat, "api_key"),
        (Protocol::AnthropicOAuth, "oauth"),
    ] {
        let source = keyed_source(protocol.clone());
        let auth = source.run_auth();
        assert_eq!(auth.mode(), mode, "{protocol:?}");
        assert!(
            !matches!(auth, RunAuth::Federated { .. }),
            "a static credential has no ids to report: {protocol:?}"
        );
    }
}

/// A keyless llama.cpp source must not claim a credential. `Source::new` stores
/// the placeholder, and `auth_headers` refuses to send it, so reporting
/// `api_key` would attest to something afi would not authenticate with.
#[test]
fn a_source_holding_the_placeholder_reports_no_credential() {
    for protocol in [Protocol::OpenAiCompat, Protocol::AnthropicApiKey] {
        let source = keyless_source(protocol.clone());
        assert_eq!(source.run_auth(), RunAuth::NoCredential, "{protocol:?}");
    }
}

/// A Bedrock source signing with the keys it was given.
fn static_bedrock() -> Bedrock {
    Bedrock {
        region: Some("us-east-1".to_string()),
        access_key_id: Some("AKIDEXAMPLE".to_string()),
        secret_access_key: Some("wJalrXUtnFEMI".to_string()),
        session_token: Some("session".to_string()),
        web_identity: None,
    }
}

/// The same source with no keys at all, assuming a role instead.
fn federated_bedrock() -> Bedrock {
    Bedrock {
        region: Some("us-east-1".to_string()),
        access_key_id: None,
        secret_access_key: None,
        session_token: None,
        web_identity: Some(WebIdentity {
            role_arn: "arn:aws:iam::123456789012:role/afi-ci".to_string(),
            session_name: "afi".to_string(),
            identity: None,
        }),
    }
}

/// Bedrock keeps no static key, so `api_key` holds the placeholder while the run
/// is fully credentialed. Reporting `none` for it would attribute a billed run to
/// nobody.
#[test]
fn a_bedrock_source_reports_the_signature_it_billed() {
    let source = keyless_source(Protocol::Bedrock(Box::new(static_bedrock())));
    assert_eq!(
        source.run_auth(),
        RunAuth::SigV4 {
            region: "us-east-1",
            access_key_id: "AKIDEXAMPLE",
        }
    );
    // The two secrets are not identifiers and never reach the summary.
    let rendered = RunAuth::json(Some(source.run_auth())).to_string();
    assert!(!rendered.contains("wJalrXUtnFEMI"), "{rendered}");
    assert!(!rendered.contains("session"), "{rendered}");
    assert!(rendered.contains("\"mode\":\"sigv4\""), "{rendered}");
}

/// A run that assumed a role has no stored key to name, and the key it minted
/// is re-minted as the run outlives it. The role is the stable answer to whose
/// budget paid, and the mode is what says which of the two questions to ask.
#[test]
fn a_federated_bedrock_source_reports_the_role_it_assumed() {
    let source = keyless_source(Protocol::Bedrock(Box::new(federated_bedrock())));
    assert_eq!(
        source.run_auth(),
        RunAuth::WebIdentity {
            region: "us-east-1",
            role_arn: "arn:aws:iam::123456789012:role/afi-ci",
            session_name: "afi",
        }
    );
    let rendered = RunAuth::json(Some(source.run_auth())).to_string();
    assert!(
        rendered.contains("\"mode\":\"sigv4_web_identity\""),
        "{rendered}"
    );
    assert!(
        !rendered.contains("access_key_id"),
        "a minted key names a session, not the identity that opened it: {rendered}"
    );
}

#[test]
fn the_placeholder_and_a_blank_are_both_no_credential() {
    assert!(is_placeholder(NOOP_KEY));
    assert!(is_placeholder(""));
    assert!(!is_placeholder("sk-real-credential"));
}
