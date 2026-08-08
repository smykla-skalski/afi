//! Amazon Bedrock: the Region and the `SigV4` credentials a source signs with.
//!
//! Bedrock takes no static key header, so there is no credential to hand to
//! `Source::api_key`. The four values below stand in for it, and unlike a key
//! they are all needed at once - a signature is scoped to a Region, and an
//! access key without its secret signs nothing.
//!
//! Those four can also be produced rather than configured. [`WebIdentity`]
//! names a role to assume from an OIDC identity token, which is how a CI job
//! reaches Bedrock with no AWS key stored anywhere; what comes back is the same
//! three-part credential, so everything downstream of the exchange is unchanged.

use std::collections::HashMap;
use std::fmt;
use std::hash::BuildHasher;

use crate::summary::RunAuth;

use super::{AWS_IDENTITY, Identity};

/// Bedrock's `OpenAI`-compatible endpoint, one host per Region.
///
/// `/v1` rather than the older `/openai/v1` on the same host: it is the path
/// AWS documents a `SigV4` request against, and the one whose examples use the
/// unversioned model ids (`zai.glm-5`). Override with `AFI_BEDROCK_BASE_URL`.
const ENDPOINT: &str = "https://bedrock-runtime.{region}.{suffix}/v1";

/// The role session name when `AWS_ROLE_SESSION_NAME` names none. It reaches
/// `CloudTrail` as the tail of the assumed-role identity, so a call afi made is
/// attributable past the role every job in the account shares.
const SESSION_NAME: &str = "afi";

/// Where a Bedrock source's Region and signing credentials come from.
///
/// Every field is optional so an incomplete configuration is still a value
/// rather than a `None` that vanishes. That is what lets [`Bedrock::missing`]
/// name what is absent before the run starts, instead of the run failing
/// partway through a turn.
///
/// Read from the same merged env map as every other setting rather than from
/// the process environment at request time - otherwise a value set in `~/.env`
/// or `AFI_ENV_FILE` would be invisible, since nothing copies those into the
/// process environment.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Bedrock {
    /// `AWS_REGION`, else `AWS_DEFAULT_REGION`. Names both the endpoint host
    /// and the credential scope the signature is computed over.
    pub region: Option<String>,
    /// `AWS_ACCESS_KEY_ID`.
    pub access_key_id: Option<String>,
    /// `AWS_SECRET_ACCESS_KEY`.
    pub secret_access_key: Option<String>,
    /// `AWS_SESSION_TOKEN`. Present for STS, SSO, and instance-role
    /// credentials; absent for a long-lived IAM user, which is why it is not
    /// required.
    pub session_token: Option<String>,
    /// The role to assume when the three fields above hold no usable static
    /// credential. `None` when `AWS_ROLE_ARN` names none.
    pub web_identity: Option<WebIdentity>,
}

/// Assuming an AWS role from an OIDC identity token, in place of a static key.
///
/// AWS federates differently from Anthropic, which is why this is a second type
/// rather than a second [`Federation`](super::Federation). There is no token
/// endpoint handing back a bearer: `sts:AssumeRoleWithWebIdentity` answers with
/// an access key, a secret, and a session token, and those then sign requests
/// exactly as a long-lived pair would. So this configures where the credential
/// comes from, and the three fields above are what it produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebIdentity {
    /// `AWS_ROLE_ARN` - the role to assume.
    pub role_arn: String,
    /// `AWS_ROLE_SESSION_NAME`, else [`SESSION_NAME`].
    pub session_name: String,
    /// Where the identity token to exchange comes from. `None` when nothing is
    /// configured, which [`Bedrock::incomplete`] refuses the run over rather
    /// than letting it reach STS with an empty assertion.
    pub identity: Option<Identity>,
}

impl WebIdentity {
    /// The identity token this role is assumed from, or why it cannot be.
    ///
    /// One accessor rather than a check followed by a later unwrap. The startup
    /// refusal and the exchange itself need to know the same thing, and the
    /// exchange also needs the token - so handing it back is what stops the
    /// exchange from re-deriving a value this already had and then calling the
    /// failure it cannot handle unreachable.
    ///
    /// # Errors
    /// The role ARN is not one, or no identity token is configured.
    pub fn assumable(&self) -> Result<&Identity, String> {
        if !is_role_arn(&self.role_arn) {
            return Err(format!(
                "AWS_ROLE_ARN={:?} is not a role ARN",
                self.role_arn
            ));
        }
        self.identity
            .as_ref()
            .ok_or_else(|| Identity::absent(AWS_IDENTITY))
    }
}

impl Bedrock {
    /// Read the Region and credentials from an env map, using the variable
    /// names every AWS SDK and the `aws` CLI already read, so a shell that can
    /// run `aws bedrock` needs no afi-specific setup.
    #[must_use]
    pub fn from_env<S: BuildHasher>(env: &HashMap<String, String, S>) -> Self {
        let get = |key: &str| {
            env.get(key)
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .map(String::from)
        };
        Self {
            region: get("AWS_REGION").or_else(|| get("AWS_DEFAULT_REGION")),
            access_key_id: get("AWS_ACCESS_KEY_ID"),
            secret_access_key: get("AWS_SECRET_ACCESS_KEY"),
            session_token: get("AWS_SESSION_TOKEN"),
            web_identity: get("AWS_ROLE_ARN").map(|role_arn| WebIdentity {
                role_arn,
                session_name: get("AWS_ROLE_SESSION_NAME")
                    .unwrap_or_else(|| SESSION_NAME.to_string()),
                identity: Identity::from_env(env, AWS_IDENTITY),
            }),
        }
    }

    /// True when the environment carries any part of an AWS credential.
    ///
    /// The built-in source registers on this rather than on a complete set: an
    /// incomplete one has to exist as a source to be refused by name, and a
    /// source that never registered names nothing.
    ///
    /// A role ARN counts. It is the whole of the configuration a federated run
    /// has - the point of that mode is that no key is stored anywhere - so
    /// without it the one shape this feature exists for would register nothing.
    #[must_use]
    pub fn has_any_credential(&self) -> bool {
        self.access_key_id.is_some()
            || self.secret_access_key.is_some()
            || self.web_identity.is_some()
    }

    /// The role to assume for a signing credential, or `None` when this source
    /// signs with the static key it was given.
    ///
    /// A complete static pair wins. Every AWS SDK's default credential chain
    /// resolves environment keys ahead of a web identity, and afi's own
    /// `anthropic` built-in already orders its three modes the same way. Which
    /// one a run actually took is in the summary's `auth` block, so a job that
    /// meant to federate and found a stray key in the environment can see that
    /// it did.
    ///
    /// Half a pair does not win. The SDK chain moves on from an incomplete
    /// environment credential, and so does this: otherwise a misspelled
    /// `AWS_SECRET_ACCESS_KEY` would take down a run that had a perfectly good
    /// role to assume, and the refusal would name the variable that was never
    /// meant to be set.
    #[must_use]
    pub fn federating(&self) -> Option<&WebIdentity> {
        if self.access_key_id.is_some() && self.secret_access_key.is_some() {
            return None;
        }
        self.web_identity.as_ref()
    }

    /// The required variables that are not set, in the order listed here.
    ///
    /// A session token is deliberately absent: a long-lived IAM user has none.
    /// The static key pair is absent too when the source federates, since the
    /// exchange is what produces it.
    #[must_use]
    pub fn missing(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if self.region.is_none() {
            missing.push("AWS_REGION");
        }
        if self.federating().is_some() {
            return missing;
        }
        if self.access_key_id.is_none() {
            missing.push("AWS_ACCESS_KEY_ID");
        }
        if self.secret_access_key.is_none() {
            missing.push("AWS_SECRET_ACCESS_KEY");
        }
        missing
    }

    /// Why a source on this configuration cannot sign, naming the variables
    /// that are absent or unusable. `None` when it can.
    #[must_use]
    pub fn incomplete(&self, source_name: &str) -> Option<String> {
        let missing = self.missing();
        if !missing.is_empty() {
            return Some(format!(
                "source {source_name} signs for Bedrock but {} not set",
                list(&missing)
            ));
        }
        let region = self.region.as_deref()?;
        if !is_region(region) {
            return Some(format!(
                "source {source_name} signs for Bedrock but AWS_REGION={region:?} \
                 is not a Region name"
            ));
        }
        // Checked here rather than on the first request for the reason every
        // other case in this function is: a run refused before it starts costs
        // nothing, and one that discovers a missing token file three turns in
        // has already been paid for.
        self.federating()
            .and_then(|web| web.assumable().err())
            .map(|why| format!("source {source_name} assumes an AWS role but {why}"))
    }

    /// Which credential this source authenticates with, for the run summary.
    ///
    /// Sits here rather than in `Source::run_auth` because the choice between
    /// the two modes is [`Self::federating`]'s, and reporting one while signing
    /// with the other is exactly the mistake the `auth` block exists to make
    /// visible.
    #[must_use]
    pub fn run_auth(&self) -> RunAuth<'_> {
        let region = self.region.as_deref().unwrap_or_default();
        match self.federating() {
            Some(web) => RunAuth::WebIdentity {
                region,
                role_arn: &web.role_arn,
                session_name: &web.session_name,
            },
            None => RunAuth::SigV4 {
                region,
                access_key_id: self.access_key_id.as_deref().unwrap_or_default(),
            },
        }
    }

    /// The endpoint for this Region, or `None` when no Region is configured.
    #[must_use]
    pub fn base_url(&self) -> Option<String> {
        self.region.as_ref().map(|region| {
            ENDPOINT
                .replace("{region}", region)
                .replace("{suffix}", dns_suffix(region))
        })
    }
}

/// The DNS suffix AWS serves a Region's endpoints under.
///
/// One partition differs. China's hosts end in `.amazonaws.com.cn`, and the
/// commercial name for a `cn-` Region resolves to nothing at all - so a
/// hardcoded suffix answers a misconfiguration that afi could name with a DNS
/// failure that it cannot. `GovCloud` shares the commercial suffix, which leaves
/// the Region prefix as the whole rule.
///
/// Shared with the STS host the role assumption posts to rather than written
/// twice: the credential and the request it signs have to land in the same
/// partition, and a role assumed in one to sign for a host in the other is the
/// failure this exists to avoid.
pub(crate) fn dns_suffix(region: &str) -> &'static str {
    if region.starts_with("cn-") {
        "amazonaws.com.cn"
    } else {
        "amazonaws.com"
    }
}

/// The shape of every AWS Region name: lowercase letters, digits, and hyphens.
///
/// Checked because the Region is interpolated straight into the endpoint host,
/// and a value carrying a dot or a slash silently moves the request - and the
/// `AWS_SESSION_TOKEN` riding it - to another host entirely. Nothing untrusted
/// reaches this today, so the value of the check is that a typo is answered
/// with the variable's name instead of an opaque DNS failure.
fn is_region(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// The shape of a role ARN: `arn:<partition>:iam::<account>:role/<name>`.
///
/// Checked for the reason [`is_region`] is - a typo answered with the
/// variable's name beats one answered by AWS, after a network round trip, with
/// a `ValidationError` about a request the operator never wrote. The mistake
/// this actually catches is pasting the role's *name* where its ARN goes.
///
/// Loose on purpose. Partitions differ (`aws`, `aws-us-gov`, `aws-cn`), a role
/// may carry a path, and account ids are not afi's to police, so only the two
/// parts every role ARN has are required.
fn is_role_arn(value: &str) -> bool {
    value.starts_with("arn:") && value.contains(":role/")
}

/// "A is" / "A and B are" / "A, B, and C are" - the tail of the refusal, so it
/// reads as a sentence however many variables are missing.
fn list(names: &[&str]) -> String {
    let verb = if names.len() == 1 { "is" } else { "are" };
    let joined = match names {
        [only] => (*only).to_string(),
        [first, second] => format!("{first} and {second}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
        [] => String::new(),
    };
    format!("{joined} {verb}")
}

/// Redacts the two secret fields, so the `Debug` that `Source` derives cannot
/// carry a live credential into a panic message or a log.
///
/// It does not make every dump safe. `Runtime` holds the whole merged
/// environment in `env` and prints it in the clear, `AWS_SECRET_ACCESS_KEY`
/// included - that predates this protocol and covers `ANTHROPIC_API_KEY` too.
/// Nothing debug-prints either type today.
impl fmt::Debug for Bedrock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hidden = |value: Option<&String>| if value.is_some() { "<set>" } else { "<unset>" };
        f.debug_struct("Bedrock")
            .field("region", &self.region)
            .field("access_key_id", &self.access_key_id)
            .field(
                "secret_access_key",
                &hidden(self.secret_access_key.as_ref()),
            )
            .field("session_token", &hidden(self.session_token.as_ref()))
            // Safe to print whole: the role and the session name are
            // identifiers, and `IdentitySource` redacts the token it holds.
            .field("web_identity", &self.web_identity)
            .finish()
    }
}

#[cfg(test)]
mod tests;
