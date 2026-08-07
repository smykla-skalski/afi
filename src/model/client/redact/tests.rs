use super::*;

/// Shaped like a real assertion: three base64url segments, long enough that a
/// prefix of it is still recognisably a credential.
const ASSERTION: &str = "eyJhbGciOiJSUzI1NiIsImtpZCI6ImFmaSJ9.eyJzdWIiOiJyZXBvOmFjbWUvYWZpIn0.c2ln";

fn exchange_echo(assertion: &str) -> String {
    format!(
        r#"{{"type":"error","error":{{"type":"invalid_request_error","message":"the assertion did not satisfy the federation rule"}},"request":{{"grant_type":"urn:ietf:params:oauth:grant-type:jwt-bearer","assertion":"{assertion}"}}}}"#
    )
}

#[test]
fn an_echoed_assertion_is_replaced_by_a_marker() {
    let cleaned = Redactor::default()
        .with(ASSERTION, Credential::IdentityToken)
        .clean(&exchange_echo(ASSERTION));
    assert!(!cleaned.contains(ASSERTION), "{cleaned}");
    assert!(
        cleaned.contains("[redacted OIDC identity token]"),
        "{cleaned}"
    );
}

#[test]
fn the_rest_of_the_body_survives() {
    // A caller has to tell a refused credential from a rate limit, and the type
    // and message are how. Blanking the whole body would lose that.
    let cleaned = Redactor::default()
        .with(ASSERTION, Credential::IdentityToken)
        .clean(&exchange_echo(ASSERTION));
    assert!(cleaned.contains("invalid_request_error"), "{cleaned}");
    assert!(
        cleaned.contains("did not satisfy the federation rule"),
        "{cleaned}"
    );
}

#[test]
fn a_body_carrying_no_credential_is_untouched() {
    let body = r#"{"type":"error","error":{"type":"rate_limit_error"}}"#;
    let cleaned = Redactor::default()
        .with(ASSERTION, Credential::IdentityToken)
        .clean(body);
    assert_eq!(cleaned, body);
}

#[test]
fn every_occurrence_goes_not_just_the_first() {
    let body = format!("assertion={ASSERTION} and again {ASSERTION}");
    let cleaned = Redactor::default()
        .with(ASSERTION, Credential::IdentityToken)
        .clean(&body);
    assert!(!cleaned.contains(ASSERTION), "{cleaned}");
    assert_eq!(cleaned.matches("[redacted OIDC identity token]").count(), 2);
}

#[test]
fn one_pass_covers_a_key_and_a_bearer_at_once() {
    // One reporting path serves every credential mode, so one redactor has to.
    let cleaned = Redactor::default()
        .with("sk-ant-api03-real-key", Credential::ApiKey)
        .with("oat-01-real-bearer", Credential::BearerToken)
        .clean("sent x-api-key=sk-ant-api03-real-key with Bearer oat-01-real-bearer");
    assert!(!cleaned.contains("sk-ant-api03-real-key"), "{cleaned}");
    assert!(!cleaned.contains("oat-01-real-bearer"), "{cleaned}");
    assert!(cleaned.contains("[redacted API key]"), "{cleaned}");
    assert!(cleaned.contains("[redacted bearer token]"), "{cleaned}");
}

#[test]
fn a_credential_cut_part_way_through_still_goes() {
    // The body limit can sever a credential. The half that survived the cut is
    // still the opening of one, so leaving it is the same leak in miniature.
    let severed = &ASSERTION[..40];
    let cleaned = Redactor::default()
        .with(ASSERTION, Credential::IdentityToken)
        .clean(&format!(r#"{{"assertion":"{severed}"#));
    assert!(!cleaned.contains(severed), "{cleaned}");
    assert!(
        cleaned.contains("[redacted OIDC identity token]"),
        "{cleaned}"
    );
}

#[test]
fn a_tail_too_short_to_be_a_credential_is_left_alone() {
    // `eyJ` opens every JWT and would otherwise strike any body ending in it.
    let cleaned = Redactor::default()
        .with(ASSERTION, Credential::IdentityToken)
        .clean("the token began with eyJ");
    assert_eq!(cleaned, "the token began with eyJ");
}

#[test]
fn the_marker_names_what_it_removed() {
    // Truncation marks itself too, and a reader who cannot tell them apart reads
    // a redaction as a body that merely ran long.
    for (credential, marker) in [
        (Credential::ApiKey, "[redacted API key]"),
        (Credential::BearerToken, "[redacted bearer token]"),
        (Credential::IdentityToken, "[redacted OIDC identity token]"),
        (Credential::RequestToken, "[redacted Actions request token]"),
    ] {
        let cleaned = Redactor::default()
            .with("credential-value-long-enough", credential)
            .clean("echo: credential-value-long-enough");
        assert_eq!(cleaned, format!("echo: {marker}"));
    }
}

#[test]
fn the_placeholder_key_is_not_a_credential() {
    // `Source::new` stores it whenever no key was configured. Striking it would
    // rewrite bodies that leaked nothing at all.
    let cleaned = Redactor::default()
        .with(NOOP_KEY, Credential::ApiKey)
        .clean(&format!("no key configured, sent {NOOP_KEY}"));
    assert!(cleaned.contains(NOOP_KEY), "{cleaned}");
}

#[test]
fn blank_and_stubby_values_are_ignored() {
    // The blank is what a mode that uses no such credential passes, so one guard
    // covers both it and a value too short to be worth matching.
    let redactor = Redactor::default()
        .with("", Credential::ApiKey)
        .with("short", Credential::BearerToken);
    assert_eq!(
        redactor.clean("short and empty:  stay"),
        "short and empty:  stay"
    );
}

#[test]
fn a_source_contributes_its_own_key() {
    let source = Source::new(
        "acme",
        "https://api.example.invalid/v1".to_string(),
        Some("sk-source-key-value".to_string()),
        None,
        None,
        None,
    );
    let cleaned = Redactor::for_source(&source).clean("echoed sk-source-key-value back");
    assert_eq!(cleaned, "echoed [redacted API key] back");
}

#[test]
fn a_keyless_source_leaves_bodies_alone() {
    let source = Source::new(
        "local",
        "http://127.0.0.1:8080/v1".to_string(),
        None,
        None,
        None,
        None,
    );
    let body = "llama.cpp says: context shift is disabled";
    assert_eq!(Redactor::for_source(&source).clean(body), body);
}

#[test]
fn a_value_registered_twice_is_named_by_the_first_label() {
    // `AnthropicOAuth` keeps its bearer in `api_key`, so both registrations carry
    // the same string. Order is what decides the name, and the bearer is what it
    // actually is.
    let token = "oat-01-the-same-string-twice";
    let cleaned = Redactor::default()
        .with(token, Credential::BearerToken)
        .with(token, Credential::ApiKey)
        .clean(&format!("echoed {token}"));
    assert_eq!(cleaned, "echoed [redacted bearer token]");
}

// --- credentials a wrap or a byte cap broke apart ------------------------------

#[test]
fn a_credential_split_across_a_wrap_goes_from_both_lines() {
    // A gateway page that hard-wraps puts half the token on each line. Joined,
    // neither half is verbatim and neither ends the preview, so each line is
    // cleaned while its own edge is still the cut.
    let redactor = Redactor::default().with(ASSERTION, Credential::IdentityToken);
    let head = redactor.clean_line(&format!("<pre>Bearer {}", &ASSERTION[..40]));
    let tail = redactor.clean_line(&format!("{}</pre>", &ASSERTION[40..]));
    assert!(!head.contains(&ASSERTION[..40]), "{head}");
    assert!(!tail.contains(&ASSERTION[40..]), "{tail}");
    assert!(head.contains("[redacted OIDC identity token]"), "{head}");
    assert!(tail.contains("[redacted OIDC identity token]"), "{tail}");
    // The join is what the old code fell for: strip the separator and the two
    // halves must not reconstruct the credential.
    let joined = format!("{head} {tail}").replace(' ', "");
    assert!(!joined.contains(&ASSERTION.replace(' ', "")), "{joined}");
}

#[test]
fn the_middle_of_a_credential_broken_into_three_goes_too() {
    // A wrap narrow enough to leave an interior run, which is neither a prefix at
    // the end nor a suffix at the start.
    let redactor = Redactor::default().with(ASSERTION, Credential::IdentityToken);
    let middle = redactor.clean_line(&ASSERTION[20..50]);
    assert_eq!(middle, "[redacted OIDC identity token]");
}

#[test]
fn an_ordinary_line_survives_being_cleaned_as_one() {
    let redactor = Redactor::default().with(ASSERTION, Credential::IdentityToken);
    let line = "<html><body>502 Bad Gateway</body></html>";
    assert_eq!(redactor.clean_line(line), line);
}

#[test]
fn a_credential_severed_mid_character_still_goes() {
    // A non-ASCII key cut by the byte cap inside one of its own characters comes
    // back from `from_utf8_lossy` with a replacement character stuck on the end,
    // and that one character used to stop the severed-tail match dead.
    let secret = "kęy-".repeat(10);
    let bytes = secret.as_bytes();
    let severed = String::from_utf8_lossy(&bytes[..32]);
    assert!(
        severed.ends_with(char::REPLACEMENT_CHARACTER),
        "the cut must land mid-character: {severed:?}"
    );
    let cleaned = Redactor::default()
        .with(&secret, Credential::ApiKey)
        .clean(&severed);
    assert_eq!(cleaned, "[redacted API key]", "{cleaned}");
}
