//! AWS Signature Version 4, the credential Bedrock takes instead of a header.
//!
//! Pure and synchronous: [`sign`] turns a request description and a set of
//! credentials into the headers to add. Nothing here reaches the network or the
//! clock - the timestamp is an argument - so the whole algorithm is checked in
//! `tests` against signatures a second implementation produced for the same
//! input.
//!
//! Only the header-authorization form is implemented, which is what a POST with
//! a body needs; presigned query-string URLs have no caller here.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

use crate::util::hex;
use std::fmt::Write as _;

type HmacSha256 = Hmac<Sha256>;

/// The fixed algorithm token, and the terminator of the credential scope.
const ALGORITHM: &str = "AWS4-HMAC-SHA256";
const TERMINATOR: &str = "aws4_request";

/// A signing credential. Borrowed rather than owned so the secret is never
/// copied into a longer-lived value than the request it signs.
pub(crate) struct Credentials<'a> {
    pub access_key_id: &'a str,
    pub secret_access_key: &'a str,
    /// Set for STS, SSO, and instance-role credentials. When present it is both
    /// sent as `x-amz-security-token` and covered by the signature; AWS rejects
    /// the request otherwise.
    pub session_token: Option<&'a str>,
}

/// The parts of an HTTP request the signature is computed over.
pub(crate) struct CanonicalRequest<'a> {
    pub method: &'a str,
    /// Exactly the `Host` header that will be sent, port included when the URL
    /// carries a non-default one. A signature over a different host is invalid.
    pub host: &'a str,
    /// The URL path, unencoded, with a leading slash.
    pub path: &'a str,
    /// The raw query string without the `?`, or `""`.
    pub query: &'a str,
    /// The `Content-Type` that will be sent. Signed because afi sets it
    /// explicitly, so it is stable.
    pub content_type: &'a str,
    /// The exact body bytes that will be sent.
    pub body: &'a [u8],
    pub region: &'a str,
    pub service: &'a str,
    /// ISO8601 basic format, `20150830T123600Z`.
    pub timestamp: &'a str,
}

/// Sign a request and return the headers to add to it, in the order
/// `x-amz-date`, `x-amz-security-token` (when temporary), `authorization`.
pub(crate) fn sign(
    request: &CanonicalRequest<'_>,
    credentials: &Credentials<'_>,
) -> Vec<(&'static str, String)> {
    let signed = signed_headers(request, credentials);
    let names = signed
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(";");
    let canonical = canonical_request(request, &signed, &names);
    let scope = format!(
        "{}/{}/{}/{TERMINATOR}",
        date_of(request.timestamp),
        request.region,
        request.service
    );
    let to_sign = format!(
        "{ALGORITHM}\n{}\n{scope}\n{}",
        request.timestamp,
        hex(&sha256(canonical.as_bytes()))
    );
    let signature = hex(&hmac(
        &signing_key(credentials.secret_access_key, request),
        to_sign.as_bytes(),
    ));

    let mut headers = vec![("x-amz-date", request.timestamp.to_string())];
    if let Some(token) = credentials.session_token {
        headers.push(("x-amz-security-token", token.to_string()));
    }
    headers.push((
        "authorization",
        format!(
            "{ALGORITHM} Credential={}/{scope}, SignedHeaders={names}, Signature={signature}",
            credentials.access_key_id,
        ),
    ));
    headers
}

/// The headers covered by the signature, with their values, lowercase and
/// already in sorted order.
///
/// Names and values come from here together so neither can drift from the
/// other - a signature over a header value that was never sent is rejected,
/// and nothing downstream would catch it.
///
/// A minimal set on purpose: every one of these is written by afi itself, so
/// none can be rewritten between signing and sending. Whatever reqwest adds of
/// its own - `accept`, `user-agent`, `accept-encoding` - stays out, which is
/// allowed and keeps the signature independent of the HTTP client's defaults.
fn signed_headers<'a>(
    request: &CanonicalRequest<'a>,
    credentials: &Credentials<'a>,
) -> Vec<(&'static str, &'a str)> {
    let mut headers = vec![
        ("content-type", request.content_type),
        ("host", request.host),
        ("x-amz-date", request.timestamp),
    ];
    if let Some(token) = credentials.session_token {
        headers.push(("x-amz-security-token", token));
    }
    headers
}

fn canonical_request(
    request: &CanonicalRequest<'_>,
    signed: &[(&str, &str)],
    names: &str,
) -> String {
    let mut headers = String::new();
    for (name, value) in signed {
        let _ = writeln!(headers, "{name}:{}", value.trim());
    }
    format!(
        "{}\n{}\n{}\n{headers}\n{names}\n{}",
        request.method,
        canonical_path(request.path),
        canonical_query(request.query),
        hex(&sha256(request.body)),
    )
}

/// The path, URI-encoded per segment.
fn canonical_path(path: &str) -> String {
    if path.is_empty() {
        return "/".to_string();
    }
    path.split('/')
        .map(uri_encode)
        .collect::<Vec<_>>()
        .join("/")
}

/// Query parameters sorted by name, then by value, each side URI-encoded.
///
/// afi sends none today; implemented so a base url that carries one signs
/// correctly rather than silently producing a signature over a different
/// request than the one sent.
fn canonical_query(query: &str) -> String {
    if query.is_empty() {
        return String::new();
    }
    let mut pairs: Vec<(String, String)> = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((name, value)) => (uri_encode(name), uri_encode(value)),
            None => (uri_encode(pair), String::new()),
        })
        .collect();
    pairs.sort();
    pairs
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// Percent-encode everything outside AWS's unreserved set (`A-Za-z0-9-_.~`),
/// leaving any escape already present intact.
///
/// The path and query arrive from `Url`, which has already encoded whatever the
/// URL syntax demanded, so encoding them again wholesale would turn one `%20`
/// into `%2520` and sign a request nobody sent. Re-encoding the rest closes the
/// gap the other way: `Url` leaves `+`, `:`, and `,` alone in a path, and AWS
/// counts all three as reserved.
fn uri_encode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
            index += 1;
        } else if is_escape(bytes, index) {
            // Uppercased, as AWS requires. The two digits were just checked, so
            // they are ASCII by construction.
            out.push('%');
            out.push(char::from(bytes[index + 1].to_ascii_uppercase()));
            out.push(char::from(bytes[index + 2].to_ascii_uppercase()));
            index += 3;
        } else {
            let _ = write!(out, "%{byte:02X}");
            index += 1;
        }
    }
    out
}

/// Whether a well-formed `%XX` escape starts at `index`.
fn is_escape(bytes: &[u8], index: usize) -> bool {
    bytes[index] == b'%'
        && bytes
            .get(index + 1..index + 3)
            .is_some_and(|digits| digits.iter().all(u8::is_ascii_hexdigit))
}

/// The four-step HMAC chain that derives a key scoped to date, Region, and
/// service, so a leaked signing key is useless outside that day and endpoint.
fn signing_key(secret: &str, request: &CanonicalRequest<'_>) -> Vec<u8> {
    let mut key = hmac(
        format!("AWS4{secret}").as_bytes(),
        date_of(request.timestamp).as_bytes(),
    );
    key = hmac(&key, request.region.as_bytes());
    key = hmac(&key, request.service.as_bytes());
    hmac(&key, TERMINATOR.as_bytes())
}

/// The `YYYYMMDD` prefix of an ISO8601 basic timestamp.
fn date_of(timestamp: &str) -> String {
    timestamp.chars().take(8).collect()
}

fn sha256(data: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().to_vec()
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC-SHA256 accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

#[cfg(test)]
mod tests;
