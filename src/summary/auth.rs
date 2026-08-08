//! Which credential a run authenticated with, and how that reaches the summary.
//!
//! Split from `summary.rs` so the type and the block it renders stay together:
//! a variant added here has one place to be rendered, rather than a field here
//! and an insert three hundred lines away.

use serde_json::{Map, Value};

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
    /// The same signature, over credentials afi assumed a role for rather than
    /// ones it was handed. The role and the session name are what `CloudTrail`
    /// attributes the call by, and the pair a trust policy is written against.
    ///
    /// No access key id, even though the minted one is no more secret than a
    /// static one. It is re-minted as the run outlives it, so a summary that
    /// named one would name whichever happened to be current when the run
    /// ended, and an audit tracing that key back would find a session rather
    /// than the identity that opened it. The role is the stable answer to whose
    /// budget paid. The minted secret and session token are secrets and never
    /// reach this type at all.
    WebIdentity {
        region: &'a str,
        role_arn: &'a str,
        session_name: &'a str,
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
    ///
    /// Two AWS modes rather than one `sigv4` with an extra field, because the
    /// difference is the whole question the block answers: one run signed with
    /// a key somebody stored, the other with a role a workflow's own identity
    /// bought. A consumer that switches on `mode` reads that off the value it
    /// already reads, instead of probing for whichever field the other mode
    /// leaves out.
    #[must_use]
    pub fn mode(&self) -> &'static str {
        match self {
            Self::ApiKey => "api_key",
            Self::OAuth => "oauth",
            Self::NoCredential => "none",
            Self::SigV4 { .. } => "sigv4",
            Self::WebIdentity { .. } => "sigv4_web_identity",
            Self::Federated { .. } => "federated",
        }
    }
}

impl RunAuth<'_> {
    /// The `auth` block: the mode, plus whichever identifiers that credential
    /// carries.
    ///
    /// An id the credential does not carry is left out rather than emitted
    /// blank, so `auth.workspace_id` is either a workspace or nothing - an
    /// empty string would read as one afi failed to capture.
    ///
    /// `None` renders as JSON null: afi declining to attribute the run at all,
    /// which is not the same as the `none` mode.
    #[must_use]
    pub fn json(auth: Option<Self>) -> Value {
        let Some(auth) = auth else {
            return Value::Null;
        };
        let mut block = Map::new();
        block.insert("mode".to_string(), auth.mode().into());
        for (name, value) in auth.identifiers() {
            block.insert(name.to_string(), value.into());
        }
        Value::Object(block)
    }

    /// The non-secret identifiers this credential carries, in the order they
    /// are written. A match rather than a chain of `if let`s so a variant added
    /// to the enum has to say what it publishes.
    fn identifiers(&self) -> Vec<(&'static str, &str)> {
        match self {
            Self::ApiKey | Self::OAuth | Self::NoCredential => Vec::new(),
            Self::SigV4 {
                region,
                access_key_id,
            } => vec![("region", region), ("access_key_id", access_key_id)],
            Self::WebIdentity {
                region,
                role_arn,
                session_name,
            } => vec![
                ("region", region),
                ("role_arn", role_arn),
                ("session_name", session_name),
            ],
            Self::Federated {
                organization_id,
                service_account_id,
                workspace_id,
                federation_rule_id,
            } => {
                let mut ids = vec![
                    ("organization_id", *organization_id),
                    ("service_account_id", *service_account_id),
                    ("federation_rule_id", *federation_rule_id),
                ];
                // Left out of the grant when the rule covers one workspace, and
                // left out of the report for the same reason.
                ids.extend(workspace_id.map(|id| ("workspace_id", id)));
                ids
            }
        }
    }
}

#[cfg(test)]
mod tests;
