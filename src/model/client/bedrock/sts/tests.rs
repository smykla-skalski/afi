//! Reading what STS answered with, and saying which refusal it was.
//!
//! No live endpoint. What the exchange has to get right is the request it
//! writes and the answer it reads, and both are pure functions here for the same
//! reason the Anthropic exchange's are: a canned XML document exercises every
//! branch, where a mock STS would only ever exercise the happy one.

use super::*;
use crate::config::{AWS_IDENTITY, Identity, IdentitySource};
use crate::summary::ErrorKind;

const ROLE: &str = "arn:aws:iam::123456789012:role/afi-ci";
/// Stands in for the GitHub OIDC token posted as the assertion.
const ASSERTION: &str =
    "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJyZXBvOmFjbWUvYWZpOnJlZjpyZWZzL2hlYWRzL21haW4ifQ.signature";

fn web() -> WebIdentity {
    WebIdentity {
        role_arn: ROLE.to_string(),
        session_name: "afi".to_string(),
        identity: Some(Identity {
            vars: AWS_IDENTITY,
            source: IdentitySource::Literal(ASSERTION.to_string()),
        }),
    }
}

/// What AWS answers a successful assumption with, trimmed to the elements afi
/// reads plus enough of the envelope to prove the scan is not fooled by it.
fn assumed(expiration: &str) -> String {
    format!(
        r#"<AssumeRoleWithWebIdentityResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/">
  <AssumeRoleWithWebIdentityResult>
    <SubjectFromWebIdentityToken>repo:acme/afi:ref:refs/heads/main</SubjectFromWebIdentityToken>
    <Audience>sts.amazonaws.com</Audience>
    <AssumedRoleUser>
      <Arn>arn:aws:sts::123456789012:assumed-role/afi-ci/afi</Arn>
      <AssumedRoleId>AROAEXAMPLE:afi</AssumedRoleId>
    </AssumedRoleUser>
    <Credentials>
      <AccessKeyId>ASIAEXAMPLE</AccessKeyId>
      <SecretAccessKey>wJalrXUtnFEMI/K7MDENG</SecretAccessKey>
      <SessionToken>FwoGZXIvYXdzEExample//////</SessionToken>
      <Expiration>{expiration}</Expiration>
    </Credentials>
    <Provider>arn:aws:iam::123456789012:oidc-provider/token.actions.githubusercontent.com</Provider>
  </AssumeRoleWithWebIdentityResult>
  <ResponseMetadata>
    <RequestId>00000000-0000-0000-0000-000000000000</RequestId>
  </ResponseMetadata>
</AssumeRoleWithWebIdentityResponse>"#
    )
}

/// What AWS answers a refused one with.
fn error(code: &str, message: &str) -> String {
    format!(
        r#"<ErrorResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/">
  <Error><Type>Sender</Type><Code>{code}</Code><Message>{message}</Message></Error>
  <RequestId>00000000-0000-0000-0000-000000000000</RequestId>
</ErrorResponse>"#
    )
}

/// An hour out, in the format STS writes.
fn an_hour_out() -> String {
    (Utc::now() + chrono::Duration::hours(1)).to_rfc3339()
}

// --- the request ---------------------------------------------------------------

#[test]
fn the_form_carries_the_action_the_role_and_the_assertion() {
    let body = form(&web(), ASSERTION);
    assert!(body.contains("Action=AssumeRoleWithWebIdentity"), "{body}");
    assert!(body.contains(&format!("Version={API_VERSION}")), "{body}");
    assert!(body.contains("RoleSessionName=afi"), "{body}");
    // The ARN's colons and slash are reserved, so they have to be escaped or
    // the parameter ends early and AWS sees a different role.
    assert!(
        body.contains("RoleArn=arn%3Aaws%3Aiam%3A%3A123456789012%3Arole%2Fafi-ci"),
        "{body}"
    );
    assert!(body.contains(&form_encode(ASSERTION)), "{body}");
}

/// A url is logged by every proxy between here and AWS, and the assertion is a
/// bearer credential for as long as it lives.
#[test]
fn the_assertion_never_reaches_the_url() {
    assert_eq!(
        endpoint("us-east-1"),
        "https://sts.us-east-1.amazonaws.com/"
    );
    assert!(!endpoint("us-east-1").contains(ASSERTION));
}

// --- the answer ----------------------------------------------------------------

#[test]
fn a_successful_assumption_yields_all_three_parts_and_the_region() {
    let (signing, expires_at) = parse_assumed(&assumed(&an_hour_out()), "eu-west-1").unwrap();
    assert_eq!(signing.region, "eu-west-1");
    assert_eq!(signing.access_key_id, "ASIAEXAMPLE");
    assert_eq!(signing.secret_access_key, "wJalrXUtnFEMI/K7MDENG");
    assert_eq!(
        signing.session_token.as_deref(),
        Some("FwoGZXIvYXdzEExample//////")
    );
    assert!(expires_at > Instant::now(), "should not be expired");
}

/// An assumed role's request is rejected without its session token, so a
/// credential missing one is not a credential afi can sign with.
#[test]
fn every_part_of_the_credential_is_required() {
    for absent in ["AccessKeyId", "SecretAccessKey", "SessionToken"] {
        let body = assumed(&an_hour_out()).replace(absent, "Renamed");
        let err =
            parse_assumed(&body, "us-east-1").expect_err("a credential missing a part cannot sign");
        assert!(err.to_string().contains(absent), "{err}");
    }
}

#[test]
fn a_response_that_is_not_an_assumption_is_a_parse_error() {
    let err = parse_assumed("<html>gateway timeout</html>", "us-east-1").unwrap_err();
    assert!(matches!(err, ClientError::Parse(_)), "got {err:?}");
    assert!(err.to_string().contains("no Credentials"), "{err}");
}

/// The `AssumedRoleUser` block above carries an `Arn` too, so a scan that
/// ignored the enclosing element would read the wrong field.
#[test]
fn the_credential_is_read_from_inside_the_credentials_element() {
    let body = assumed(&an_hour_out());
    let credentials = element(&body, "Credentials").unwrap();
    assert!(credentials.contains("ASIAEXAMPLE"));
    assert!(
        !credentials.contains("assumed-role"),
        "the scan must stop at </Credentials>"
    );
}

// --- expiry --------------------------------------------------------------------

/// The whole point of caching these: a run that outlives its credential
/// re-assumes rather than failing partway through a turn.
#[test]
fn a_credential_near_its_expiry_counts_as_stale() {
    // Inside the skew, so it is treated as gone even though AWS would still
    // take it - a request minted at the boundary would race the deadline.
    let soon = (Utc::now() + chrono::Duration::seconds(30)).to_rfc3339();
    let (_, expires_at) = parse_assumed(&assumed(&soon), "us-east-1").unwrap();
    assert!(expires_at <= Instant::now());
}

#[test]
fn an_expiry_already_past_does_not_underflow() {
    let past = (Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
    let (_, expires_at) = parse_assumed(&assumed(&past), "us-east-1").unwrap();
    assert!(expires_at <= Instant::now());
}

#[test]
fn an_unreadable_expiry_re_assumes_next_request_rather_than_failing() {
    // The credential is good; only its lifetime is unknown. Refusing it would
    // fail a run over a field afi does not need to use the credential once.
    let (signing, expires_at) = parse_assumed(&assumed("not a timestamp"), "us-east-1")
        .expect("the credential is still usable");
    assert_eq!(signing.access_key_id, "ASIAEXAMPLE");
    assert!(expires_at <= Instant::now());
}

// --- refusals ------------------------------------------------------------------

/// The classification, and the fact that AWS's own sentence always survives it.
#[test]
fn each_refusal_says_which_kind_it_is_and_quotes_aws() {
    let cases = [
        (
            "AccessDenied",
            "Not authorized to perform sts:AssumeRoleWithWebIdentity",
            "trust policy",
        ),
        (
            "InvalidIdentityToken",
            "No OpenIDConnect provider found in your account",
            "identity provider is registered",
        ),
        (
            "ExpiredTokenException",
            "Token expired",
            "expired before it was exchanged",
        ),
        (
            "IDPRejectedClaim",
            "Claim rejected",
            "rejected the token's claims",
        ),
        ("ValidationError", "Request ARN is invalid", "AWS_ROLE_ARN"),
    ];
    for (code, message, expected) in cases {
        let text = refused(StatusCode::FORBIDDEN, &error(code, message), ASSERTION).to_string();
        assert!(text.contains(expected), "{code}: {text}");
        assert!(
            text.contains(message),
            "AWS's own words must survive: {text}"
        );
    }
}

/// AWS answers a trust policy that did not match and a role that is not there
/// identically, on purpose. Naming one would send the operator editing a policy
/// on a role that does not exist.
#[test]
fn access_denied_names_both_causes_rather_than_guessing() {
    let text = refused(
        StatusCode::FORBIDDEN,
        &error("AccessDenied", "Not authorized"),
        ASSERTION,
    )
    .to_string();
    assert!(text.contains("trust policy"), "{text}");
    assert!(text.contains("does not exist"), "{text}");
}

/// A body with no `<Code>` is not from the STS API layer - a proxy or a VPC
/// endpoint refusing on the way - so nothing is claimed about it beyond that
/// the assumption was refused, and the body speaks for itself.
#[test]
fn a_refusal_from_outside_aws_is_left_unclassified() {
    let text = refused(
        StatusCode::FORBIDDEN,
        "<html>403 Forbidden</html>",
        ASSERTION,
    )
    .to_string();
    assert!(text.contains("AWS refused the role assumption"), "{text}");
    assert!(text.contains("403 Forbidden"), "{text}");
}

#[test]
fn an_unknown_code_is_reported_by_name() {
    let text = refused(
        StatusCode::BAD_REQUEST,
        &error("PackedPolicyTooLarge", "too large"),
        ASSERTION,
    )
    .to_string();
    assert!(text.contains("PackedPolicyTooLarge"), "{text}");
}

/// No retry assembles a trust policy, so a refused assumption must not be one.
#[test]
fn a_refused_assumption_is_an_auth_failure() {
    let error = refused(
        StatusCode::FORBIDDEN,
        &error("AccessDenied", "Not authorized"),
        ASSERTION,
    );
    assert!(matches!(error, ClientError::Auth(_)), "got {error:?}");
    assert_eq!(error.kind(), ErrorKind::Auth);
}

#[test]
fn a_throttled_exchange_stays_retryable() {
    let error = refused(
        StatusCode::TOO_MANY_REQUESTS,
        &error("Throttling", "Rate exceeded"),
        ASSERTION,
    );
    assert_eq!(error.kind(), ErrorKind::ProviderHttp);
}

/// The assertion was in the form body that was just posted, so a rejection
/// echoing the request back carries it - and afi fetched that token from the
/// Actions endpoint itself, outside the toolkit that would have masked it.
#[test]
fn a_refused_assumption_does_not_report_the_assertion_it_posted() {
    let echoed = format!(
        "<ErrorResponse><Error><Code>AccessDenied</Code>\
         <Message>Not authorized: {ASSERTION}</Message></Error></ErrorResponse>"
    );
    let text = refused(StatusCode::FORBIDDEN, &echoed, ASSERTION).to_string();
    assert!(!text.contains(ASSERTION), "{text}");
    assert!(text.contains("[redacted OIDC identity token]"), "{text}");
    assert!(
        text.contains("Not authorized"),
        "the reason survives: {text}"
    );
}

// --- the cache -----------------------------------------------------------------

/// `Bedrock::incomplete` refuses this before the run, and `super::signing`
/// re-checks it on a mid-session switch, so this guards the type itself.
#[tokio::test]
async fn a_role_with_no_identity_token_is_refused_before_anything_is_sent() {
    let mut web = web();
    web.identity = None;
    let error = CredentialCache::default()
        .assumed(&Client::new(), "bedrock", "us-east-1", &web)
        .await
        .expect_err("there is nothing to exchange");
    let ClientError::Auth(message) = error else {
        panic!("a missing identity token is an auth failure, not a transport one");
    };
    assert!(message.contains("AWS_WEB_IDENTITY_TOKEN_FILE"), "{message}");
}
