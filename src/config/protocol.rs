//! How to reach a source: its wire protocol, the credential shape that
//! protocol implies, and the workload-identity-federation parameters.

use std::collections::HashMap;
use std::fmt;
use std::hash::BuildHasher;
use std::path::PathBuf;

use super::Bedrock;

/// Placeholder stored when a source has no API key. It must never reach the
/// wire as an Anthropic `x-api-key`: the Messages API validates that header in
/// preference to a bearer token, so a non-empty wrong value fails the request
/// outright even when a valid bearer is present.
pub const NOOP_KEY: &str = "sk-noop";

/// Whether a stored credential is really no credential at all.
///
/// `Source::new` substitutes [`NOOP_KEY`] when nothing was configured, so every
/// reader has to know that a key can be present and still be nothing. One
/// definition, beside the constant it tests for, keeps the wire path and the run
/// summary from drifting on what counts as unconfigured - a source afi refuses
/// to authenticate must not be reported as having a key.
#[must_use]
pub fn is_placeholder(credential: &str) -> bool {
    credential.is_empty() || credential == NOOP_KEY
}

/// The variables one protocol reads its OIDC identity token from, and the
/// audience that protocol's exchange requires the token to carry.
///
/// Two paths federate now - Anthropic's token exchange and AWS's role
/// assumption - and they agree on the shape of the thing (a JWT, from a
/// variable, a file, or minted by GitHub Actions) while agreeing on none of the
/// three names. Held as one value rather than passed as three arguments so a
/// token read from `AWS_WEB_IDENTITY_TOKEN_FILE` cannot be paired with an
/// audience minted for Anthropic: the variable, the file, and the audience
/// travel together or not at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentityVars {
    /// The variable carrying the token itself.
    pub literal: &'static str,
    /// The variable carrying a path to read it from.
    pub file: &'static str,
    /// The `aud` claim the exchange requires, and so the audience afi asks the
    /// GitHub Actions endpoint to mint for.
    pub audience: &'static str,
}

/// Anthropic's names, and the audience its token exchange accepts.
pub const ANTHROPIC_IDENTITY: IdentityVars = IdentityVars {
    literal: "ANTHROPIC_IDENTITY_TOKEN",
    file: "ANTHROPIC_IDENTITY_TOKEN_FILE",
    audience: "https://api.anthropic.com",
};

/// AWS's names, and the audience `sts:AssumeRoleWithWebIdentity` expects.
///
/// `AWS_WEB_IDENTITY_TOKEN_FILE` is the variable every AWS SDK already reads,
/// so a job that has run `configure-aws-credentials`, or a pod given an EKS
/// service-account identity, needs no afi-specific setup.
/// `AWS_WEB_IDENTITY_TOKEN` is afi's own counterpart for a caller holding the
/// token rather than a file, mirroring the pair the Anthropic SDKs define.
///
/// `sts.amazonaws.com` is the audience AWS's own GitHub Actions documentation
/// registers on the IAM identity provider. An identity provider created with a
/// different one is reached by minting the token elsewhere and passing it in
/// the two variables above, which skips the Actions endpoint entirely.
pub const AWS_IDENTITY: IdentityVars = IdentityVars {
    literal: "AWS_WEB_IDENTITY_TOKEN",
    file: "AWS_WEB_IDENTITY_TOKEN_FILE",
    audience: "sts.amazonaws.com",
};

/// An OIDC identity token to exchange, together with the names it was resolved
/// under.
///
/// The names ride along because they are what a refusal has to say. A token
/// that turns out to be blank is a misconfiguration of one particular variable,
/// and "the identity token is empty" without that variable's name sends the
/// reader looking through both protocols' spellings of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub vars: IdentityVars,
    pub source: IdentitySource,
}

impl Identity {
    /// Resolve an identity token under `vars`, or `None` when none is
    /// configured.
    #[must_use]
    pub fn from_env<S: BuildHasher>(
        env: &HashMap<String, String, S>,
        vars: IdentityVars,
    ) -> Option<Self> {
        IdentitySource::from_env(env, vars).map(|source| Self { vars, source })
    }

    /// Names the configured identity source for an error message. Never
    /// includes the token itself: it is a bearer credential in its own right.
    #[must_use]
    pub fn label(&self) -> String {
        match &self.source {
            IdentitySource::Literal(_) => self.vars.literal.to_string(),
            IdentitySource::File(path) => format!("{} {}", self.vars.file, path.display()),
            IdentitySource::GithubActions { .. } => "the GitHub Actions OIDC endpoint".to_string(),
        }
    }

    /// What to tell whoever configured a federating source with no identity
    /// token at all, naming both variables and the workflow permission that
    /// makes a third one unnecessary.
    #[must_use]
    pub fn absent(vars: IdentityVars) -> String {
        format!(
            "no OIDC identity token is available. Set {} or {}, or run inside \
             GitHub Actions with `permissions: id-token: write`",
            vars.literal, vars.file
        )
    }
}

/// Where the OIDC identity token for a federation exchange comes from.
///
/// Resolved at source discovery, from the same merged env map as every other
/// setting, rather than from the process env at request time - otherwise a
/// value set in `~/.env` or `AFI_ENV_FILE` would be invisible, since nothing
/// copies those into the process environment.
#[derive(Clone, PartialEq, Eq)]
pub enum IdentitySource {
    /// [`IdentityVars::literal`] - the token itself.
    Literal(String),
    /// [`IdentityVars::file`] - a path to read it from.
    File(PathBuf),
    /// GitHub Actions' OIDC endpoint, which is what `core.getIDToken` calls.
    GithubActions { url: String, request_token: String },
}

impl IdentitySource {
    /// Resolve an identity-token source, in the same precedence order the
    /// official SDKs use, with GitHub Actions as an afi-specific fallback so a
    /// workflow needs no explicit token-minting step.
    #[must_use]
    pub fn from_env<S: BuildHasher>(
        env: &HashMap<String, String, S>,
        vars: IdentityVars,
    ) -> Option<Self> {
        let get = |key: &str| env.get(key).map(String::as_str).filter(|v| !v.is_empty());
        if let Some(token) = get(vars.literal) {
            return Some(Self::Literal(token.to_string()));
        }
        if let Some(path) = get(vars.file) {
            return Some(Self::File(PathBuf::from(path)));
        }
        let url = get("ACTIONS_ID_TOKEN_REQUEST_URL")?;
        let request_token = get("ACTIONS_ID_TOKEN_REQUEST_TOKEN")?;
        Some(Self::GithubActions {
            url: url.to_string(),
            request_token: request_token.to_string(),
        })
    }
}

/// Redacts the two token-bearing variants, so the `Debug` that `Source`,
/// `Runtime`, and [`Protocol`] all derive cannot carry a live credential into a
/// panic message or a log. The same reason [`Bedrock`]'s is hand-written.
///
/// The path a token is read from stays readable, and so does the Actions url:
/// neither authenticates anything, and a dump with both struck out says nothing
/// about which of the three variants was even in play.
impl fmt::Debug for IdentitySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Literal(_) => f.debug_tuple("Literal").field(&"<set>").finish(),
            Self::File(path) => f.debug_tuple("File").field(path).finish(),
            Self::GithubActions { url, .. } => f
                .debug_struct("GithubActions")
                .field("url", url)
                .field("request_token", &"<set>")
                .finish(),
        }
    }
}

/// Workload-identity-federation parameters for minting a short-lived Anthropic
/// access token from an OIDC identity token. The four ids are non-secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Federation {
    pub rule_id: String,
    pub organization_id: String,
    pub service_account_id: String,
    /// Only required when the federation rule spans multiple workspaces.
    pub workspace_id: Option<String>,
    /// Where to get the identity token to exchange. `None` when none is
    /// configured, which surfaces as an actionable error on the first request
    /// rather than hiding the source entirely.
    pub identity: Option<Identity>,
}

impl Federation {
    /// Read the federation ids from an env map, using the same variable names as
    /// the official SDKs so a workspace already configured for them needs no
    /// afi-specific plumbing. Returns `None` unless all three required ids are
    /// present and non-empty.
    #[must_use]
    pub fn from_env<S: BuildHasher>(env: &HashMap<String, String, S>) -> Option<Self> {
        let get = |key: &str| {
            env.get(key)
                .map(String::as_str)
                .filter(|v| !v.is_empty())
                .map(String::from)
        };
        Some(Self {
            rule_id: get("ANTHROPIC_FEDERATION_RULE_ID")?,
            organization_id: get("ANTHROPIC_ORGANIZATION_ID")?,
            service_account_id: get("ANTHROPIC_SERVICE_ACCOUNT_ID")?,
            workspace_id: get("ANTHROPIC_WORKSPACE_ID"),
            identity: Identity::from_env(env, ANTHROPIC_IDENTITY),
        })
    }
}

/// How to reach a source: its wire protocol together with the credential shape
/// that protocol implies.
///
/// Protocol and auth are one enum rather than two fields because they are not
/// independent - there is no such thing as an `OpenAI`-compatible endpoint
/// authenticated with an Anthropic OAuth bearer. Folding them removes the
/// nonsense states entirely.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Protocol {
    /// `OpenAI`-compatible `/chat/completions`, `Authorization: Bearer <key>`.
    #[default]
    OpenAiCompat,
    /// Anthropic Messages API, `x-api-key` plus `anthropic-version`.
    AnthropicApiKey,
    /// Anthropic Messages API with a pre-minted OAuth bearer token.
    /// `x-api-key` is never sent in this mode.
    AnthropicOAuth,
    /// Anthropic Messages API, minting a bearer token via workload identity
    /// federation before each expiry. `x-api-key` is never sent.
    AnthropicFederated(Box<Federation>),
    /// Amazon Bedrock's `OpenAI`-compatible `/chat/completions`, with each
    /// request signed with AWS `SigV4`. The same wire shape as
    /// [`OpenAiCompat`](Self::OpenAiCompat), so the SSE decoder is shared;
    /// only the credential differs, and it is computed per request rather
    /// than sent as a static header.
    Bedrock(Box<Bedrock>),
}

impl Protocol {
    /// True when this source speaks Anthropic's Messages API rather than
    /// `OpenAI`-compatible chat completions.
    #[must_use]
    pub fn is_anthropic(&self) -> bool {
        matches!(
            self,
            Self::AnthropicApiKey | Self::AnthropicOAuth | Self::AnthropicFederated(_)
        )
    }

    /// True when requests to this source are signed with AWS `SigV4` instead of
    /// carrying a credential header.
    #[must_use]
    pub fn is_bedrock(&self) -> bool {
        matches!(self, Self::Bedrock(_))
    }

    /// Why a source on this protocol cannot be used as configured, naming what
    /// is absent. `None` when it can.
    ///
    /// Checked before the run starts. Bedrock is the only protocol with more
    /// than one thing to get wrong: the others carry a single credential, and
    /// a source with none of it is never registered in the first place.
    #[must_use]
    pub fn config_error(&self, source_name: &str) -> Option<String> {
        match self {
            Self::Bedrock(bedrock) => bedrock.incomplete(source_name),
            Self::OpenAiCompat
            | Self::AnthropicApiKey
            | Self::AnthropicOAuth
            | Self::AnthropicFederated(_) => None,
        }
    }

    /// The endpoint a source on this protocol derives for itself, for a source
    /// that configured none. Only Bedrock does: its Region names the host.
    ///
    /// `Some` means "this protocol supplies its own endpoint", and nothing
    /// weaker - a Bedrock source with no Region still answers `Some`, with the
    /// empty string. That source has to exist for `Runtime::refusals` to name
    /// `AWS_REGION` against it, and the refusal stops the run before anything
    /// reads the url. Were the two folded into one `None`, the caller would
    /// have to ask `is_bedrock()` to tell them apart, and this would only look
    /// like a general mechanism.
    #[must_use]
    pub fn default_base_url(&self) -> Option<String> {
        match self {
            Self::Bedrock(bedrock) => Some(bedrock.base_url().unwrap_or_default()),
            Self::OpenAiCompat
            | Self::AnthropicApiKey
            | Self::AnthropicOAuth
            | Self::AnthropicFederated(_) => None,
        }
    }

    /// True when auth rides on `Authorization: Bearer` and `x-api-key` must be
    /// omitted entirely.
    #[must_use]
    pub fn is_bearer(&self) -> bool {
        matches!(self, Self::AnthropicOAuth | Self::AnthropicFederated(_))
    }

    /// The values [`Self::from_env_value`] understands, so a caller that has to
    /// refuse anything else - the config file - and the warning below both read
    /// the same list.
    pub const NAMES: [&str; 6] = [
        "openai",
        "openai-compat",
        "anthropic",
        "anthropic-api-key",
        "anthropic-oauth",
        "aws-bedrock-openai",
    ];

    /// Parse an `AFI_SOURCE_<NAME>_PROTOCOL` value. Unknown values warn to
    /// stderr and fall back to `OpenAiCompat` so a typo never silently
    /// reroutes a source. Federated auth is not reachable from this knob - it
    /// needs the federation ids, which only the built-in source resolves.
    ///
    /// `env` is the merged env map, which `aws-bedrock-openai` reads its Region
    /// and credentials from; every other value ignores it.
    #[must_use]
    pub fn from_env_value<S: BuildHasher>(raw: &str, env: &HashMap<String, String, S>) -> Self {
        match raw.trim().to_lowercase().as_str() {
            "" | "openai" | "openai-compat" => Self::OpenAiCompat,
            "anthropic" | "anthropic-api-key" => Self::AnthropicApiKey,
            "anthropic-oauth" => Self::AnthropicOAuth,
            "aws-bedrock-openai" => Self::Bedrock(Box::new(Bedrock::from_env(env))),
            other => {
                eprintln!(
                    "afi: unknown AFI_SOURCE_*_PROTOCOL {other:?}, using openai; \
                     expected one of {}",
                    Self::NAMES.join(", ")
                );
                Self::OpenAiCompat
            }
        }
    }
}
