//! Which credential a run authenticated with, and how that reaches the summary.
//!
//! Split from `summary.rs` so the type and the block it renders stay together:
//! a variant added here has one place to be rendered, rather than a field here
//! and an insert three hundred lines away.

use serde_json::{Value, json};

/// Which credential a run authenticated with, in identifiers safe to publish.
///
/// Reported for the reason `tools` is: an audit should read a run's posture out
/// of its own output instead of trusting that the workflow passed the flags it
/// claims. Credential mode is the other half of that posture. It matters most
/// once `cost_usd` is on - the next question after what a run cost is whose
/// budget paid, and a job that quietly fell back to a personal key otherwise
/// produces a summary indistinguishable from one that used the intended service
/// account.
///
/// Identifiers only. The minted access token and the OIDC assertion must never
/// land here: a summary gets uploaded as a build artifact, and artifacts carry
/// no masking, so a value redacted in a log is plain text there.
///
/// One enum rather than a mode string beside a bag of optional ids, for the
/// reason [`crate::config::Protocol`] gives for folding auth into itself: the
/// two are not independent. Each variant carries only the identifiers its own
/// credential has, so a static key with an organization id is a state nothing
/// has to test for because nothing can build it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunAuth<'a> {
    /// A static key out of the environment, whichever header carries it.
    ApiKey,
    /// A bearer token minted elsewhere and handed to afi.
    OAuth,
    /// No credential was configured at all - a local server that wants none.
    /// Distinct from `auth: null`, which is afi declining to attribute the run.
    NoCredential,
    /// An AWS `SigV4` signature, computed per request rather than sent as a
    /// header. Carries the two non-secret halves of the credential, for the
    /// reason `Federated` carries its ids: they are what says whose budget paid.
    ///
    /// The access key id is safe to publish - it travels in cleartext in the
    /// `Authorization` header of every signed request, and is the identifier
    /// `CloudTrail` attributes a call by. The secret access key and the session
    /// token are the secrets, and neither reaches this type.
    SigV4 {
        region: &'a str,
        access_key_id: &'a str,
    },
    /// A bearer token afi minted itself, through the workload-identity
    /// federation exchange. The only mode with identifiers of its own.
    Federated {
        organization_id: &'a str,
        service_account_id: &'a str,
        /// Only present when the federation rule spans workspaces.
        workspace_id: Option<&'a str>,
        federation_rule_id: &'a str,
    },
}

impl RunAuth<'_> {
    /// The `mode` the summary reports, naming how the credential was obtained.
    #[must_use]
    pub fn mode(&self) -> &'static str {
        match self {
            Self::ApiKey => "api_key",
            Self::OAuth => "oauth",
            Self::NoCredential => "none",
            Self::SigV4 { .. } => "sigv4",
            Self::Federated { .. } => "federated",
        }
    }
}

impl RunAuth<'_> {
    /// The `auth` block: the mode, plus the identifiers federation carries.
    ///
    /// Only that mode has any, so only that arm adds them. An id the credential
    /// does not carry is left out rather than emitted blank, so
    /// `auth.workspace_id` is either a workspace or nothing - an empty string
    /// would read as one afi failed to capture.
    ///
    /// `None` renders as JSON null: afi declining to attribute the run at all,
    /// which is not the same as the `none` mode.
    #[must_use]
    pub fn json(auth: Option<Self>) -> Value {
        let Some(auth) = auth else {
            return Value::Null;
        };
        let mut block = json!({ "mode": auth.mode() });
        if let RunAuth::SigV4 {
            region,
            access_key_id,
        } = auth
            && let Some(fields) = block.as_object_mut()
        {
            fields.insert("region".to_string(), region.into());
            fields.insert("access_key_id".to_string(), access_key_id.into());
        }
        if let RunAuth::Federated {
            organization_id,
            service_account_id,
            workspace_id,
            federation_rule_id,
        } = auth
            && let Some(fields) = block.as_object_mut()
        {
            fields.insert("organization_id".to_string(), organization_id.into());
            fields.insert("service_account_id".to_string(), service_account_id.into());
            fields.insert("federation_rule_id".to_string(), federation_rule_id.into());
            if let Some(workspace_id) = workspace_id {
                fields.insert("workspace_id".to_string(), workspace_id.into());
            }
        }
        block
    }
}

#[cfg(test)]
mod tests;
