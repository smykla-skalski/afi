//! `SigV4` vectors captured from `curl --aws-sigv4`, an implementation with no
//! code in common with this one.
//!
//! Each case was produced by pointing curl at a local listener and reading the
//! request it wrote, so the expected signature is a second implementation's
//! answer to the same input rather than this one's output written down. The
//! credentials are AWS's own documentation placeholders and authorize nothing.
//!
//! ```text
//! nc -l 127.0.0.1 8731 > capture.txt &
//! curl --aws-sigv4 "aws:amz:us-east-1:bedrock" \
//!      --user "AKIDEXAMPLE:wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY" \
//!      -H "Content-Type: application/json" -d '{"model":"zai.glm-5","stream":true}' \
//!      http://127.0.0.1:8731/v1/chat/completions
//! ```

use super::{CanonicalRequest, Credentials, canonical_query, sign, uri_encode};

const SECRET: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";

/// The `Authorization` value `sign` produces, which is the only header curl and
/// this implementation both have to agree on byte for byte.
fn authorization(request: &CanonicalRequest<'_>, credentials: &Credentials<'_>) -> String {
    sign(request, credentials)
        .into_iter()
        .find(|(name, _)| *name == "authorization")
        .map(|(_, value)| value)
        .expect("sign always emits an authorization header")
}

#[test]
fn matches_curl_on_a_streaming_chat_completion() {
    let body = br#"{"model":"zai.glm-5","stream":true}"#;
    let signed = authorization(
        &CanonicalRequest {
            method: "POST",
            host: "127.0.0.1:8731",
            path: "/v1/chat/completions",
            query: "",
            content_type: "application/json",
            body,
            region: "us-east-1",
            service: "bedrock",
            timestamp: "20260807T050217Z",
        },
        &Credentials {
            access_key_id: "AKIDEXAMPLE",
            secret_access_key: SECRET,
            session_token: None,
        },
    );
    assert_eq!(
        signed,
        "AWS4-HMAC-SHA256 \
         Credential=AKIDEXAMPLE/20260807/us-east-1/bedrock/aws4_request, \
         SignedHeaders=content-type;host;x-amz-date, \
         Signature=2ffa18de3976ac7fe83ede76ea567483e0025215f50b3ee18c3dc70d50c99aad"
    );
}

/// A session token is both sent and signed, or AWS rejects the request. This is
/// the case every SSO or assumed-role shell hits, so it is the one that matters
/// most in practice.
#[test]
fn matches_curl_when_the_credentials_are_temporary() {
    let body = br#"{"model":"openai.gpt-oss-20b-1:0"}"#;
    let signed = authorization(
        &CanonicalRequest {
            method: "POST",
            host: "127.0.0.1:8732",
            path: "/openai/v1/chat/completions",
            query: "",
            content_type: "application/json",
            body,
            region: "eu-central-1",
            service: "bedrock",
            timestamp: "20260807T050231Z",
        },
        &Credentials {
            access_key_id: "ASIAEXAMPLE",
            secret_access_key: SECRET,
            session_token: Some("FQoGZXIvYXdzEBY/fake+token=="),
        },
    );
    assert_eq!(
        signed,
        "AWS4-HMAC-SHA256 \
         Credential=ASIAEXAMPLE/20260807/eu-central-1/bedrock/aws4_request, \
         SignedHeaders=content-type;host;x-amz-date;x-amz-security-token, \
         Signature=c1f4245dfd3b96ba478b184c164d5d6c731346a48a03683a35e47437d2f9ce3d"
    );
}

/// A query string arrives already percent-encoded from `Url`, and has to be
/// sorted without being encoded a second time. afi builds no query today; this
/// pins the behaviour so a base url that carries one is not signed wrongly.
#[test]
fn matches_curl_on_an_out_of_order_encoded_query() {
    let signed = authorization(
        &CanonicalRequest {
            method: "POST",
            host: "127.0.0.1:8733",
            path: "/v1/chat/completions",
            query: "zeta=1&alpha=a%20b",
            content_type: "application/json",
            body: b"{}",
            region: "us-west-2",
            service: "bedrock",
            timestamp: "20260807T050243Z",
        },
        &Credentials {
            access_key_id: "AKIDEXAMPLE",
            secret_access_key: SECRET,
            session_token: None,
        },
    );
    assert!(
        signed.ends_with(
            "Signature=4841801b97b30be6e4294cbbad02b6c163a3d1349008306194519ff801b6b962"
        ),
        "got {signed}"
    );
}

/// A plain Bedrock request. The tests below vary one field of it each; the
/// captured-vector tests above keep their literals, since the exact values are
/// what curl was given and the point of those cases.
fn request() -> CanonicalRequest<'static> {
    CanonicalRequest {
        method: "POST",
        host: "bedrock-runtime.us-east-1.amazonaws.com",
        path: "/v1/chat/completions",
        query: "",
        content_type: "application/json",
        body: b"{}",
        region: "us-east-1",
        service: "bedrock",
        timestamp: "20260807T050217Z",
    }
}

fn long_lived() -> Credentials<'static> {
    Credentials {
        access_key_id: "AKIDEXAMPLE",
        secret_access_key: SECRET,
        session_token: None,
    }
}

#[test]
fn the_signature_covers_the_body() {
    let with_body = |body| CanonicalRequest { body, ..request() };
    assert_ne!(
        authorization(&with_body(b"{\"a\":1}"), &long_lived()),
        authorization(&with_body(b"{\"a\":2}"), &long_lived()),
        "a body change must change the signature, or nothing is being covered"
    );
}

#[test]
fn the_signature_is_scoped_to_the_region() {
    let in_region = |region| CanonicalRequest {
        region,
        ..request()
    };
    assert_ne!(
        authorization(&in_region("us-east-1"), &long_lived()),
        authorization(&in_region("us-west-2"), &long_lived())
    );
}

#[test]
fn sign_emits_the_date_and_the_token_it_signed() {
    let headers = sign(
        &request(),
        &Credentials {
            access_key_id: "ASIAEXAMPLE",
            secret_access_key: SECRET,
            session_token: Some("token-value"),
        },
    );
    let names: Vec<&str> = headers.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        names,
        ["x-amz-date", "x-amz-security-token", "authorization"]
    );
    assert_eq!(headers[0].1, "20260807T050217Z");
    assert_eq!(headers[1].1, "token-value");
}

/// A long-lived IAM user has no session token, and sending an empty one would
/// be rejected.
#[test]
fn no_session_token_means_no_token_header() {
    let headers = sign(&request(), &long_lived());
    assert!(
        headers
            .iter()
            .all(|(name, _)| *name != "x-amz-security-token")
    );
}

#[test]
fn uri_encode_escapes_everything_outside_the_unreserved_set() {
    assert_eq!(
        uri_encode("chat.completions-1_0~x"),
        "chat.completions-1_0~x"
    );
    assert_eq!(uri_encode("a b"), "a%20b");
    // `Url` leaves all three of these alone in a path; AWS does not.
    assert_eq!(uri_encode("a+b:c,d"), "a%2Bb%3Ac%2Cd");
}

#[test]
fn uri_encode_keeps_an_escape_that_is_already_there() {
    // Encoding this again would sign `%2520` against a request carrying `%20`.
    assert_eq!(uri_encode("a%20b"), "a%20b");
    assert_eq!(uri_encode("a%2fb"), "a%2Fb", "escapes are uppercased");
    // Not a well-formed escape, so the `%` is a literal to be encoded.
    assert_eq!(uri_encode("100%"), "100%25");
    assert_eq!(uri_encode("%zz"), "%25zz");
}

#[test]
fn canonical_query_sorts_by_name_then_value() {
    assert_eq!(canonical_query(""), "");
    assert_eq!(canonical_query("b=2&a=1"), "a=1&b=2");
    assert_eq!(canonical_query("a=2&a=1"), "a=1&a=2");
    assert_eq!(canonical_query("flag"), "flag=");
}
