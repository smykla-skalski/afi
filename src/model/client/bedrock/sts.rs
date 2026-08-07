//! `sts:AssumeRoleWithWebIdentity`: an OIDC identity token traded for the
//! temporary AWS credentials a request is then signed with.
//!
//! Not a second auth mode on the wire. Anthropic's exchange hands back a bearer
//! token that goes straight into a header; AWS hands back an access key, a
//! secret, and a session token, and those feed [`super::sigv4`] exactly as a
//! long-lived pair would. Every Bedrock request is signed the same way either
//! way - this only decides what it is signed with.
//!
//! The call is deliberately unsigned. The identity token is the credential
//! being presented, and a run that federates has nothing else to sign with; the
//! trust policy on the role is what decides whether the claims are good enough.
//!
//! It is also the one AWS API afi speaks in the Query protocol, so the answer
//! is XML rather than JSON. It is read with a tag scan rather than a parser -
//! see [`element`].

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use reqwest::{Client, StatusCode};

use crate::config::WebIdentity;
use crate::model::client::expiry::{Expiring, deadline};
use crate::model::client::identity::{fetch, refused_credential};
use crate::model::client::{
    ClientError, Credential, Redactor, transport_error, transport_error_at,
};

use super::Signing;
use super::sigv4::form_encode;

/// The Query-protocol API version `AssumeRoleWithWebIdentity` is defined at.
const API_VERSION: &str = "2011-06-15";

/// One assumed-role credential and the deadline it stops working at.
type Assumed = (Signing, Instant);

/// Per-source cache of assumed-role credentials.
///
/// STS credentials are short-lived - an hour by default, and as little as
/// fifteen minutes on a role configured that way - which is fine for a one-shot
/// CI run and would fail a long session partway through a turn. They are
/// re-assumed as they approach expiry, so a run that outlives its credential
/// carries on rather than stopping with a 403 that reads like a broken trust
/// policy. What counts as approaching lives in
/// [`expiry`](crate::model::client::expiry), shared with the Anthropic token
/// exchange, which had to answer the same question.
#[derive(Default)]
pub(crate) struct CredentialCache {
    credentials: Expiring<Signing>,
}

impl CredentialCache {
    /// The credential `source_name` signs with, assuming the role if the cached
    /// one is gone or near expiry.
    pub(super) async fn assumed(
        &self,
        http: &Client,
        source_name: &str,
        region: &str,
        web: &WebIdentity,
    ) -> Result<Signing, ClientError> {
        if let Some(signing) = self.credentials.fresh(source_name).await {
            return Ok(signing);
        }
        // The same accessor `Bedrock::incomplete` refuses the run through, so
        // the token the exchange needs comes from the check rather than from an
        // unwrap beside it. `Auth`, because no retry configures a role.
        let identity = web.assumable().map_err(ClientError::Auth)?;
        let assertion = fetch(http, identity).await?;
        let (signing, expires_at) = assume_role(http, region, web, &assertion).await?;
        self.credentials
            .store(source_name, signing.clone(), expires_at)
            .await;
        Ok(signing)
    }
}

/// The regional STS endpoint. Regional rather than the legacy global host, so
/// the call stays in the Region the credential will be used in.
///
/// The Region reaches this only past [`crate::config::Bedrock::incomplete`],
/// which has already checked it against the Region charset - the same check
/// that keeps the Bedrock host from being moved by an `AWS_REGION` carrying a
/// dot or a slash.
fn endpoint(region: &str) -> String {
    format!("https://sts.{region}.amazonaws.com/")
}

/// The Query-protocol request body: the action, its version, and the three
/// parameters that identify what is being assumed and on whose word.
///
/// Sent as a form body rather than a query string because the identity token is
/// a credential and a url is logged by every proxy between here and AWS.
fn form(web: &WebIdentity, assertion: &str) -> String {
    [
        ("Action", "AssumeRoleWithWebIdentity"),
        ("Version", API_VERSION),
        ("RoleArn", &web.role_arn),
        ("RoleSessionName", &web.session_name),
        ("WebIdentityToken", assertion),
    ]
    .into_iter()
    .map(|(name, value)| format!("{name}={}", form_encode(value)))
    .collect::<Vec<_>>()
    .join("&")
}

/// POST the role assumption and read what comes back.
async fn assume_role(
    http: &Client,
    region: &str,
    web: &WebIdentity,
    assertion: &str,
) -> Result<Assumed, ClientError> {
    let response = http
        .post(endpoint(region))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form(web, assertion))
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| transport_error_at(Some("the AWS role assumption failed"), &e))?;
    let status = response.status();
    let text = response.text().await.map_err(|e| transport_error(&e))?;
    if !status.is_success() {
        return Err(refused(status, &text, assertion));
    }
    parse_assumed(&text, region)
}

/// A refused role assumption, saying which kind it is and quoting AWS.
///
/// The assertion was in the form body that was just posted, so a rejection that
/// echoes the request back carries it. This is the leak the redaction exists
/// for - and it bites harder here than on a static credential, because afi
/// fetched that token from the Actions endpoint itself rather than through the
/// toolkit that would have registered it for masking.
///
/// The code decides retryable, not the status. [`refused_credential`] reads a
/// 429 or a 5xx as worth another attempt and everything else in the 4xx range as
/// a credential to go fix, which is the right rule for an endpoint that spells
/// throttling as a 429. The Query protocol predates that convention and answers
/// a throttled call with a 400 carrying the code, so [`transient`] gets first
/// refusal on the body.
fn refused(status: StatusCode, body: &str, assertion: &str) -> ClientError {
    let redact = Redactor::default().with(assertion, Credential::IdentityToken);
    let code = element(body, "Code").unwrap_or_default();
    let what = describe(code);
    if transient(code) {
        return ClientError::Http {
            status: status.as_u16(),
            body: format!("{what}: {}", redact.clean(body)),
        };
    }
    refused_credential(&what, status, body, &redact)
}

/// Codes that say come back, rather than go fix something.
///
/// Throttling is the case worth naming: a role assumption AWS shed under load
/// reported as an auth failure sends whoever reads the summary auditing a trust
/// policy that was never the problem, and stops a scheduled run that a second
/// attempt would have finished.
///
/// `IDPCommunicationError` is here for the same reason rather than for its
/// status. AWS could not reach the identity provider registered on the role,
/// which is neither the token nor the policy and which nobody holding the run
/// can do anything about.
fn transient(code: &str) -> bool {
    matches!(
        code,
        "Throttling" | "ThrottlingException" | "RequestLimitExceeded" | "IDPCommunicationError"
    )
}

/// What an STS error code means for whoever has to fix it.
///
/// The sentence leads and AWS's own `<Message>` follows in the quoted body, so
/// nothing AWS said is replaced - the classification only says which fix to
/// reach for first.
///
/// **`AccessDenied` names two causes rather than one.** A trust policy whose
/// conditions the token's claims did not match and a role that does not exist
/// are answered by STS identically, on purpose: distinguishing them would let
/// anyone holding a GitHub token enumerate an account's roles. So afi says both
/// rather than guessing, the way it already declines to guess whether a Bedrock
/// `ValidationException` was about tool support.
fn describe(code: &str) -> String {
    match code {
        "AccessDenied" => "AWS refused the role assumption: the role's trust policy did not \
             accept the token's claims, or the role does not exist - STS answers both the \
             same way"
            .to_string(),
        "InvalidIdentityToken" => "AWS would not read the OIDC identity token: no matching \
             identity provider is registered in the account, or the token names an audience \
             other than sts.amazonaws.com"
            .to_string(),
        "ExpiredTokenException" => {
            "the OIDC identity token expired before it was exchanged".to_string()
        }
        "IDPRejectedClaim" => {
            "the identity provider registered on the role rejected the token's claims".to_string()
        }
        "IDPCommunicationError" => {
            "AWS could not reach the identity provider registered on the role".to_string()
        }
        "Throttling" | "ThrottlingException" | "RequestLimitExceeded" => {
            "AWS is rate-limiting role assumptions on this account".to_string()
        }
        "ValidationError" => "AWS refused the role-assumption request itself (check \
             AWS_ROLE_ARN and AWS_ROLE_SESSION_NAME)"
            .to_string(),
        "" => "AWS refused the role assumption".to_string(),
        other => format!("AWS refused the role assumption ({other})"),
    }
}

/// The credential out of a successful response.
///
/// All three parts are required. AWS returns them together and a signature
/// needs all three - an assumed role's request is rejected without its session
/// token - so a response missing one is a wire change rather than a credential
/// with a part afi can do without.
fn parse_assumed(body: &str, region: &str) -> Result<Assumed, ClientError> {
    let credentials = element(body, "Credentials").ok_or_else(|| {
        ClientError::Parse("the AWS role assumption returned no Credentials".to_string())
    })?;
    let field = |name: &str| {
        element(credentials, name)
            .filter(|value| !value.is_empty())
            .map(String::from)
            .ok_or_else(|| {
                ClientError::Parse(format!("the AWS role assumption returned no {name}"))
            })
    };
    Ok((
        Signing {
            region: region.to_string(),
            access_key_id: field("AccessKeyId")?,
            secret_access_key: field("SecretAccessKey")?,
            session_token: Some(field("SessionToken")?),
        },
        deadline(lifetime(credentials)),
    ))
}

/// How much longer the credential lasts, from the absolute instant AWS reported.
///
/// AWS answers on its own wall clock, so it is converted to a duration here and
/// hung off the monotonic clock by [`deadline`] - a system-clock step mid-run
/// then cannot make a live credential look expired or the reverse.
///
/// A missing, unreadable, or already-past `Expiration` is zero, which
/// [`deadline`] turns into a deadline already past: the credential serves this
/// request and is re-assumed for the next one.
fn lifetime(credentials: &str) -> Duration {
    element(credentials, "Expiration")
        .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
        .map(|at| at.signed_duration_since(Utc::now()))
        .and_then(|left| left.to_std().ok())
        .unwrap_or_default()
}

/// The text inside the first `<name>` element of `xml`.
///
/// A tag scan rather than an XML parser, and a dependency not taken. The Query
/// protocol's answer is a fixed shape afi asked for by name, five fields deep,
/// whose values are base64 and a timestamp - so the parts of XML a parser earns
/// its keep on (namespaces, mixed content, entity expansion) do not arise. The
/// one place text AWS did not generate can appear is an error `<Message>`,
/// which is quoted rather than acted on.
///
/// Elements are matched with no attributes, which is what STS emits below the
/// root element; the root is never looked up. Were that to change, the parse
/// fails loudly with "returned no Credentials" rather than reading a field out
/// of the wrong place.
fn element<'x>(xml: &'x str, name: &str) -> Option<&'x str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(&xml[start..end])
}

#[cfg(test)]
mod tests;
