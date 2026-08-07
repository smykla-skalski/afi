//! Amazon Bedrock: the Region and the `SigV4` credentials a source signs with.
//!
//! Bedrock takes no static key header, so there is no credential to hand to
//! `Source::api_key`. The four values below stand in for it, and unlike a key
//! they are all needed at once - a signature is scoped to a Region, and an
//! access key without its secret signs nothing.

use std::collections::HashMap;
use std::fmt;
use std::hash::BuildHasher;

/// Bedrock's `OpenAI`-compatible endpoint, one host per Region.
///
/// `/v1` rather than the older `/openai/v1` on the same host: it is the path
/// AWS documents a `SigV4` request against, and the one whose examples use the
/// unversioned model ids (`zai.glm-5`). Override with `AFI_BEDROCK_BASE_URL`.
const ENDPOINT: &str = "https://bedrock-runtime.{region}.amazonaws.com/v1";

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
        }
    }

    /// True when the environment carries any part of an AWS credential.
    ///
    /// The built-in source registers on this rather than on a complete set: an
    /// incomplete one has to exist as a source to be refused by name, and a
    /// source that never registered names nothing.
    #[must_use]
    pub fn has_any_credential(&self) -> bool {
        self.access_key_id.is_some() || self.secret_access_key.is_some()
    }

    /// The required variables that are not set, in the order listed here. A
    /// session token is deliberately absent: a long-lived IAM user has none.
    #[must_use]
    pub fn missing(&self) -> Vec<&'static str> {
        [
            ("AWS_REGION", &self.region),
            ("AWS_ACCESS_KEY_ID", &self.access_key_id),
            ("AWS_SECRET_ACCESS_KEY", &self.secret_access_key),
        ]
        .into_iter()
        .filter(|(_, value)| value.is_none())
        .map(|(name, _)| name)
        .collect()
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
        if is_region(region) {
            return None;
        }
        Some(format!(
            "source {source_name} signs for Bedrock but AWS_REGION={region:?} \
             is not a Region name"
        ))
    }

    /// The endpoint for this Region, or `None` when no Region is configured.
    #[must_use]
    pub fn base_url(&self) -> Option<String> {
        self.region
            .as_ref()
            .map(|region| ENDPOINT.replace("{region}", region))
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
            .finish()
    }
}

#[cfg(test)]
mod tests;
