//! Which rate card a source is billed against, in afi's own vocabulary.
//!
//! A model id is not enough to price a request. The same id is served by
//! several providers at different rates - `google/gemma-4-31b-it` is $0.10 per
//! million input tokens on `OpenRouter` and $0.39 on Together - so a table keyed
//! on the id alone would bill whichever entry happened to win, which is the one
//! failure [`super::Pricing`] refuses a duplicate id to prevent.
//!
//! The host is what decides, rather than the protocol. A source speaking the
//! Messages API to a proxy is not billed at Anthropic's rates just because it
//! speaks Anthropic's protocol, and a Bedrock-shaped request sent somewhere
//! other than AWS is not billed at AWS's. An address afi does not recognise is
//! priced by nothing, which is the right answer for a llama.cpp on localhost and
//! the honest one for a gateway whose rates afi was never told.
//!
//! [`Provider`] is deliberately afi's own enum with afi's own names, not the
//! catalogue's strings. Whoever publishes the rates gets to call Bedrock
//! whatever they like; afi calls it `bedrock`, the same as the source it
//! registers, and [`super::catalog`] is the one place the two vocabularies meet.

use crate::config::Source;

/// A provider afi can bill a request against.
///
/// The stored table, the vendored file, and every message use [`Self::key`],
/// which is afi's name rather than any catalogue's - see the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Provider {
    Anthropic,
    Bedrock,
    Cerebras,
    DeepSeek,
    Fireworks,
    Google,
    Groq,
    Mistral,
    OpenAi,
    OpenRouter,
    Together,
    XAi,
    ZAi,
    Zhipu,
}

/// Every provider afi can bill against, in `key` order.
///
/// The catalogue is narrowed by this, so what afi caches is what afi can read
/// back. A provider absent here is one no source resolves to, and rates for it
/// would be weight in the binary nothing could ever use.
pub const ALL: [Provider; 14] = [
    Provider::Anthropic,
    Provider::Bedrock,
    Provider::Cerebras,
    Provider::DeepSeek,
    Provider::Fireworks,
    Provider::Google,
    Provider::Groq,
    Provider::Mistral,
    Provider::OpenAi,
    Provider::OpenRouter,
    Provider::Together,
    Provider::XAi,
    Provider::ZAi,
    Provider::Zhipu,
];

impl Provider {
    /// afi's name for this provider: the key in `assets/prices.json`, in the
    /// cache, and in anything a person reads.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Bedrock => "bedrock",
            Self::Cerebras => "cerebras",
            Self::DeepSeek => "deepseek",
            Self::Fireworks => "fireworks",
            Self::Google => "google",
            Self::Groq => "groq",
            Self::Mistral => "mistral",
            Self::OpenAi => "openai",
            Self::OpenRouter => "openrouter",
            Self::Together => "together",
            Self::XAi => "xai",
            Self::ZAi => "zai",
            Self::Zhipu => "zhipu",
        }
    }

    /// The provider a stored key names, or `None` for one afi no longer knows.
    ///
    /// A cache written by a newer afi can carry a provider this one has never
    /// heard of. That is a row to skip, not a file to refuse.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        ALL.into_iter().find(|provider| provider.key() == key)
    }
}

/// Host, then the provider whose rates apply. Matched exactly - a lookalike
/// registered by somebody else must not be billed as the real one.
///
/// Bedrock has no row: its host carries the Region, so it is matched by
/// [`is_bedrock`] instead.
const HOSTS: [(&str, Provider); 13] = [
    ("api.anthropic.com", Provider::Anthropic),
    ("api.cerebras.ai", Provider::Cerebras),
    ("api.deepseek.com", Provider::DeepSeek),
    ("api.fireworks.ai", Provider::Fireworks),
    ("api.groq.com", Provider::Groq),
    ("api.mistral.ai", Provider::Mistral),
    ("api.openai.com", Provider::OpenAi),
    ("api.together.xyz", Provider::Together),
    ("api.x.ai", Provider::XAi),
    ("api.z.ai", Provider::ZAi),
    ("generativelanguage.googleapis.com", Provider::Google),
    ("open.bigmodel.cn", Provider::Zhipu),
    ("openrouter.ai", Provider::OpenRouter),
];

/// The provider whose rates price requests sent to this address, or `None` when
/// afi has no rates for it.
#[must_use]
pub fn of_url(base_url: &str) -> Option<Provider> {
    let host = host_of(base_url)?;
    if is_bedrock(&host) {
        return Some(Provider::Bedrock);
    }
    HOSTS
        .iter()
        .find_map(|(pattern, provider)| (host == *pattern).then_some(*provider))
}

/// Whether a host is an AWS Bedrock runtime endpoint.
///
/// Both ends are checked. `bedrock::ENDPOINT` builds
/// `bedrock-runtime.{region}.{suffix}`, so the Region in the middle is the part
/// that varies and the two ends are the part that identifies AWS - matching the
/// prefix alone would bill `bedrock-runtime.someone-else.com` at AWS's rates.
fn is_bedrock(host: &str) -> bool {
    host.starts_with("bedrock-runtime.")
        && (host.ends_with(".amazonaws.com") || host.ends_with(".amazonaws.com.cn"))
}

/// The host part of a base url, lowercased, without credentials or port.
///
/// Hand-rolled rather than parsed: afi has no url crate, and everything past
/// the authority is exactly what this must ignore.
fn host_of(base_url: &str) -> Option<String> {
    let rest = base_url
        .split_once("://")
        .map_or(base_url, |(_, rest)| rest)
        .trim_start_matches('/');
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    // A bracketed IPv6 literal is never one of ours, so splitting on the last
    // colon is safe: it either finds a port or finds nothing.
    let host = host.rsplit_once(':').map_or(host, |(h, port)| {
        if port.bytes().all(|b| b.is_ascii_digit()) {
            h
        } else {
            host
        }
    });
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

impl Source {
    /// The provider whose rates price this source's requests.
    ///
    /// `None` for an address afi carries no rates for, which leaves the source
    /// unpriced rather than priced wrongly - see the module doc.
    #[must_use]
    pub fn price_provider(&self) -> Option<Provider> {
        of_url(&self.base_url)
    }
}

#[cfg(test)]
mod tests;
