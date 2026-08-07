//! Removing afi's own credentials from the bodies it reports on a failed
//! request.
//!
//! A provider that echoes the request back when it refuses one returns the
//! credential afi just sent, inside the body afi then quotes. The federated path
//! is the sharp edge: the token exchange posts the OIDC assertion, and afi
//! fetches that assertion from the Actions endpoint itself rather than through
//! the toolkit that would register it for masking, so nothing downstream hides
//! it. Whoever reads the job log can mint an access token with it until it
//! expires.
//!
//! Cleaning happens at the client boundary, where the credentials are still in
//! scope and before a [`ClientError`](super::ClientError) carries the body
//! anywhere - so stderr, the run summary, and the summary file a CI job uploads
//! as a build artifact all read what is left, rather than each having to
//! remember to strip it.
//!
//! Removal is targeted. A body that carries no credential is reported whole, so
//! a rejected key stays distinguishable from a rate limit.

use crate::config::{NOOP_KEY, Source};

/// Below this a value is not worth matching: it would collide with ordinary
/// error text more often than it would catch a credential.
const MIN_SECRET_LEN: usize = 8;
/// A body cut by [`MAX_ERROR_BODY_BYTES`](super::MAX_ERROR_BODY_BYTES) can sever
/// a credential and leave its opening behind, so a trailing prefix of one is
/// removed too. Below this length that tail is noise rather than a leak, and
/// matching it would fire on any body ending in a common few characters.
const MIN_TAIL_LEN: usize = 16;

/// Which credential a marker stands in for.
///
/// The marker names it so a reader tells a credential that was removed from a
/// body that was merely cut short - the two are otherwise the same ellipsis.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Credential {
    /// `x-api-key`, or the `OpenAI`-compatible bearer taken from the same field.
    ApiKey,
    /// An OAuth access token, minted or configured.
    BearerToken,
    /// The OIDC assertion posted to the token exchange.
    IdentityToken,
    /// The Actions runtime token that buys the assertion.
    RequestToken,
}

impl Credential {
    fn marker(self) -> &'static str {
        match self {
            Self::ApiKey => "[redacted API key]",
            Self::BearerToken => "[redacted bearer token]",
            Self::IdentityToken => "[redacted OIDC identity token]",
            Self::RequestToken => "[redacted Actions request token]",
        }
    }
}

/// The credentials one request carried, ready to be struck from its error body.
#[derive(Debug, Default, Clone)]
pub(crate) struct Redactor {
    secrets: Vec<(String, Credential)>,
}

impl Redactor {
    /// The credentials any request to `source` carries in its own right.
    ///
    /// Bearer modes add their token separately, since it is resolved per request
    /// rather than stored on the source.
    pub(crate) fn for_source(source: &Source) -> Self {
        Self::default().with(&source.api_key, Credential::ApiKey)
    }

    /// Register `value`, ignoring blanks, the [`NOOP_KEY`] placeholder, and
    /// anything too short to be a credential. `Source::new` stores the
    /// placeholder whenever no key was configured, and striking it would rewrite
    /// bodies that leaked nothing.
    ///
    /// A mode that uses no such credential passes `""`, which the same guard
    /// drops - so a caller holding an `Option` needs nothing more than
    /// `unwrap_or_default`.
    ///
    /// The [`NOOP_KEY`] clause is redundant at its current 7 bytes, which the
    /// length check already rejects. It is kept against the placeholder growing
    /// past [`MIN_SECRET_LEN`], where striking it would start rewriting bodies
    /// that leaked nothing.
    #[must_use]
    pub(crate) fn with(mut self, value: &str, credential: Credential) -> Self {
        if value.len() >= MIN_SECRET_LEN && value != NOOP_KEY {
            self.secrets.push((value.to_string(), credential));
        }
        self
    }

    /// `body` with every registered credential replaced by its marker.
    ///
    /// Call this before any length limit is applied. Cleaning first means a cut
    /// can only ever trim a marker, whereas cutting first would leave whatever
    /// half of a credential fell inside the window.
    pub(crate) fn clean(&self, body: &str) -> String {
        let mut cleaned = body.to_string();
        for (value, credential) in &self.secrets {
            cleaned = cleaned.replace(value.as_str(), credential.marker());
            // A body the byte cap severed mid-character ends in the replacement
            // character `from_utf8_lossy` left there, and that one character
            // would stop every `ends_with` below from matching. It goes with the
            // tail it is stuck to.
            let searchable = cleaned.trim_end_matches(char::REPLACEMENT_CHARACTER);
            if let Some(tail) = severed_tail(searchable, value) {
                cleaned.truncate(searchable.len() - tail);
                cleaned.push_str(credential.marker());
            }
        }
        cleaned
    }

    /// One line of a body being assembled from several.
    ///
    /// A hard-wrapped echo splits a credential across lines, and the assembled
    /// preview hides it from [`Self::clean`] twice over: joined by a separator
    /// the halves are no longer verbatim, and neither half sits at the end of
    /// the finished string. Each line is cleaned while its own edges are still
    /// the cut, so a wrap is caught where a join would have buried it.
    pub(crate) fn clean_line(&self, line: &str) -> String {
        for (value, credential) in &self.secrets {
            if is_fragment(line, value) {
                return credential.marker().to_string();
            }
        }
        let mut cleaned = self.clean(line);
        for (value, credential) in &self.secrets {
            if let Some(head) = severed_head(&cleaned, value) {
                cleaned.replace_range(..head, credential.marker());
            }
        }
        cleaned
    }
}

/// Whether the whole of `line` is a long enough run of `value` to strike - the
/// middle of a credential a wrap broke into three, which is neither a prefix at
/// the end nor a suffix at the start.
fn is_fragment(line: &str, value: &str) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= MIN_TAIL_LEN && value.contains(trimmed)
}

/// The length of the longest prefix of `value` that `body` ends with, once whole
/// occurrences are already gone - that is, a credential the body was truncated
/// part-way through.
///
/// Bounded below by [`MIN_TAIL_LEN`], so a credential of that length or shorter
/// has no severed tail struck at all: the range is empty rather than merely
/// strict. Every credential afi sends is far longer, and lowering the bound to
/// cover a short one would start matching bodies by coincidence.
fn severed_tail(body: &str, value: &str) -> Option<usize> {
    let longest = body.len().min(value.len().saturating_sub(1));
    (MIN_TAIL_LEN..=longest)
        .rev()
        .find(|&len| value.is_char_boundary(len) && body.ends_with(&value[..len]))
}

/// The mirror of [`severed_tail`]: the longest suffix of `value` that `body`
/// starts with. This is the half a line break leaves at the start of the next
/// line.
fn severed_head(body: &str, value: &str) -> Option<usize> {
    let longest = body.len().min(value.len().saturating_sub(1));
    (MIN_TAIL_LEN..=longest).rev().find(|&len| {
        let at = value.len() - len;
        value.is_char_boundary(at) && body.starts_with(&value[at..])
    })
}

#[cfg(test)]
mod tests;
