//! Unit-level ports from `tests/test_sources.py`: `parse_extra_body`,
//! `Source::clean_model_id`, `Source::is_local`, and `parse_args`.

use std::collections::HashMap;
use std::path::PathBuf;

use afi::config::{
    Federation, IdentitySource, NOOP_KEY, ParsedArgs, Protocol, parse_args, parse_extra_body,
};
use afi::{ApprovalKind, Source};

// parse_extra_body edge cases (empty object, non-object, empty string)
#[test]
fn parse_extra_body_empty_object_is_none() {
    assert_eq!(parse_extra_body(Some("{}")), None);
}

#[test]
fn parse_extra_body_non_object_is_none() {
    assert_eq!(parse_extra_body(Some("[1,2,3]")), None);
    assert_eq!(parse_extra_body(Some("\"hi\"")), None);
}

#[test]
fn parse_extra_body_blank_is_none() {
    assert_eq!(parse_extra_body(Some("")), None);
    assert_eq!(parse_extra_body(Some("   ")), None);
    assert_eq!(parse_extra_body(None), None);
}

// clean_model_id
#[test]
fn clean_model_id_strips_gguf_path() {
    assert_eq!(
        Source::clean_model_id(
            "/media/h/.../GLM-5.2-GGUF/UD-IQ4_NL/GLM-5.2-UD-IQ4_NL-00001-of-00009.gguf"
        ),
        "GLM-5.2-UD-IQ4_NL"
    );
    assert_eq!(
        Source::clean_model_id("/models/Meta-Llama-3-8B-Instruct-Q4_K_M.gguf"),
        "Meta-Llama-3-8B-Instruct-Q4_K_M"
    );
    // org/model form is returned unchanged.
    assert_eq!(Source::clean_model_id("zai-org/GLM-5.2"), "zai-org/GLM-5.2");
    assert_eq!(Source::clean_model_id(""), "");
}

fn mk_source(url: &str) -> Source {
    Source::new("x", url.to_string(), None, None, None, None)
}

// --- Protocol -----------------------------------------------------------------

#[test]
fn protocol_defaults_to_openai_compat() {
    let src = mk_source("https://api.together.xyz/v1");
    assert_eq!(src.protocol, Protocol::OpenAiCompat);
    assert!(!src.is_anthropic());
    assert!(!src.protocol.is_bearer());
}

#[test]
fn protocol_from_env_value_parses_known_names() {
    assert_eq!(Protocol::from_env_value(""), Protocol::OpenAiCompat);
    assert_eq!(Protocol::from_env_value("openai"), Protocol::OpenAiCompat);
    assert_eq!(
        Protocol::from_env_value("openai-compat"),
        Protocol::OpenAiCompat
    );
    assert_eq!(
        Protocol::from_env_value("anthropic"),
        Protocol::AnthropicApiKey
    );
    assert_eq!(
        Protocol::from_env_value("anthropic-api-key"),
        Protocol::AnthropicApiKey
    );
    assert_eq!(
        Protocol::from_env_value("anthropic-oauth"),
        Protocol::AnthropicOAuth
    );
}

#[test]
fn protocol_from_env_value_is_case_and_space_insensitive() {
    assert_eq!(
        Protocol::from_env_value("  Anthropic-OAuth "),
        Protocol::AnthropicOAuth
    );
}

#[test]
fn protocol_from_env_value_falls_back_on_typo() {
    // A typo must never silently reroute a source to a different wire protocol.
    assert_eq!(
        Protocol::from_env_value("anthropik"),
        Protocol::OpenAiCompat
    );
}

#[test]
fn with_protocol_round_trips_and_classifies() {
    let key = mk_source("https://api.anthropic.com").with_protocol(Protocol::AnthropicApiKey);
    assert!(key.is_anthropic());
    assert!(!key.protocol.is_bearer(), "api-key mode must not be bearer");

    let oauth = mk_source("https://api.anthropic.com").with_protocol(Protocol::AnthropicOAuth);
    assert!(oauth.is_anthropic());
    assert!(oauth.protocol.is_bearer());

    let fed = mk_source("https://api.anthropic.com").with_protocol(Protocol::AnthropicFederated(
        Box::new(Federation {
            rule_id: "fdrl_x".to_string(),
            organization_id: "org".to_string(),
            service_account_id: "svac".to_string(),
            workspace_id: None,
            identity: None,
        }),
    ));
    assert!(fed.is_anthropic());
    assert!(fed.protocol.is_bearer());
}

// --- Federation::from_env -----------------------------------------------------

fn env_map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[test]
fn federation_reads_all_ids_with_optional_workspace() {
    let env = env_map(&[
        ("ANTHROPIC_FEDERATION_RULE_ID", "fdrl_1"),
        ("ANTHROPIC_ORGANIZATION_ID", "org-uuid"),
        ("ANTHROPIC_SERVICE_ACCOUNT_ID", "svac_1"),
    ]);
    let federation = Federation::from_env(&env).expect("resolves without a workspace");
    assert_eq!(federation.rule_id, "fdrl_1");
    assert_eq!(federation.organization_id, "org-uuid");
    assert_eq!(federation.service_account_id, "svac_1");
    assert!(federation.workspace_id.is_none());
}

#[test]
fn federation_picks_up_the_workspace_when_present() {
    let env = env_map(&[
        ("ANTHROPIC_FEDERATION_RULE_ID", "fdrl_1"),
        ("ANTHROPIC_ORGANIZATION_ID", "org"),
        ("ANTHROPIC_SERVICE_ACCOUNT_ID", "svac"),
        ("ANTHROPIC_WORKSPACE_ID", "wrkspc_1"),
    ]);
    let federation = Federation::from_env(&env).unwrap();
    assert_eq!(federation.workspace_id.as_deref(), Some("wrkspc_1"));
}

#[test]
fn federation_needs_every_required_id() {
    for omit in [
        "ANTHROPIC_FEDERATION_RULE_ID",
        "ANTHROPIC_ORGANIZATION_ID",
        "ANTHROPIC_SERVICE_ACCOUNT_ID",
    ] {
        let mut env = env_map(&[
            ("ANTHROPIC_FEDERATION_RULE_ID", "fdrl_1"),
            ("ANTHROPIC_ORGANIZATION_ID", "org"),
            ("ANTHROPIC_SERVICE_ACCOUNT_ID", "svac"),
        ]);
        env.remove(omit);
        assert!(
            Federation::from_env(&env).is_none(),
            "{omit} should be required"
        );
    }
}

#[test]
fn blank_federation_values_count_as_unset() {
    let env = env_map(&[
        ("ANTHROPIC_FEDERATION_RULE_ID", "fdrl_1"),
        ("ANTHROPIC_ORGANIZATION_ID", ""),
        ("ANTHROPIC_SERVICE_ACCOUNT_ID", "svac"),
    ]);
    assert!(Federation::from_env(&env).is_none());
}

// --- IdentitySource::from_env -------------------------------------------------

#[test]
fn literal_identity_token_wins() {
    let env = env_map(&[
        ("ANTHROPIC_IDENTITY_TOKEN", "jwt-literal"),
        ("ANTHROPIC_IDENTITY_TOKEN_FILE", "/tmp/token"),
        ("ACTIONS_ID_TOKEN_REQUEST_URL", "https://actions"),
        ("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "req"),
    ]);
    assert_eq!(
        IdentitySource::from_env(&env),
        Some(IdentitySource::Literal("jwt-literal".to_string()))
    );
}

#[test]
fn token_file_beats_github_actions() {
    let env = env_map(&[
        ("ANTHROPIC_IDENTITY_TOKEN_FILE", "/tmp/token"),
        ("ACTIONS_ID_TOKEN_REQUEST_URL", "https://actions"),
        ("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "req"),
    ]);
    assert_eq!(
        IdentitySource::from_env(&env),
        Some(IdentitySource::File(PathBuf::from("/tmp/token")))
    );
}

#[test]
fn github_actions_is_the_fallback() {
    let env = env_map(&[
        ("ACTIONS_ID_TOKEN_REQUEST_URL", "https://actions/token"),
        ("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "req-token"),
    ]);
    assert_eq!(
        IdentitySource::from_env(&env),
        Some(IdentitySource::GithubActions {
            url: "https://actions/token".to_string(),
            request_token: "req-token".to_string(),
        })
    );
}

#[test]
fn github_actions_needs_both_variables() {
    let url_only = env_map(&[("ACTIONS_ID_TOKEN_REQUEST_URL", "https://actions")]);
    assert!(IdentitySource::from_env(&url_only).is_none());
    let token_only = env_map(&[("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "req")]);
    assert!(IdentitySource::from_env(&token_only).is_none());
}

#[test]
fn no_identity_configuration_is_none() {
    assert!(IdentitySource::from_env(&env_map(&[])).is_none());
}

#[test]
fn empty_identity_values_count_as_unset() {
    let env = env_map(&[
        ("ANTHROPIC_IDENTITY_TOKEN", ""),
        ("ANTHROPIC_IDENTITY_TOKEN_FILE", ""),
    ]);
    assert!(IdentitySource::from_env(&env).is_none());
}

#[test]
fn federation_carries_its_identity_source() {
    // Resolved alongside the ids so the token-mint path never has to consult the
    // process env, which would miss anything set in an env file.
    let env = env_map(&[
        ("ANTHROPIC_FEDERATION_RULE_ID", "fdrl_1"),
        ("ANTHROPIC_ORGANIZATION_ID", "org"),
        ("ANTHROPIC_SERVICE_ACCOUNT_ID", "svac"),
        ("ANTHROPIC_IDENTITY_TOKEN_FILE", "/run/secrets/token"),
    ]);
    let federation = Federation::from_env(&env).unwrap();
    assert_eq!(
        federation.identity,
        Some(IdentitySource::File(PathBuf::from("/run/secrets/token")))
    );
}

#[test]
fn noop_key_is_the_placeholder_source_new_stores() {
    // The Anthropic header builder must special-case this value, so pin the
    // link between the constant and what `Source::new` actually stores.
    assert_eq!(mk_source("https://example.com").api_key, NOOP_KEY);
}

// is_local: loopback and RFC-1918 private ranges classify as local.
#[test]
fn is_local_loopback_and_private() {
    assert!(mk_source("http://localhost:8080/v1").is_local());
    assert!(mk_source("http://127.0.0.1:8080/v1").is_local());
    assert!(mk_source("http://10.0.0.5/v1").is_local());
    assert!(mk_source("http://192.168.1.5/v1").is_local());
}

// is_local: link-local, the 172.16/12 edges, and a blank URL are local.
#[test]
fn is_local_link_local_and_edges() {
    assert!(mk_source("http://169.254.1.5/v1").is_local());
    assert!(mk_source("http://172.16.0.5/v1").is_local());
    assert!(mk_source("http://172.31.255.255/v1").is_local());
    assert!(mk_source("").is_local()); // blank -> local
}

// is_local: just past the private ranges, and public hosts, are not local.
#[test]
fn is_local_public_is_not() {
    assert!(!mk_source("http://172.32.0.5/v1").is_local());
    assert!(!mk_source("https://api.z.ai/api/paas/v4").is_local());
    assert!(!mk_source("https://openrouter.ai/api/v1").is_local());
}

// parse_args handles --resume with and without a target
#[test]
fn parse_args_resume_bare_vs_target() {
    let mk = |args: &[&str]| -> ParsedArgs {
        parse_args(&args.iter().map(ToString::to_string).collect::<Vec<_>>())
    };
    assert_eq!(mk(&["afi", "--resume"]).resume, Some(None));
    assert_eq!(
        mk(&["afi", "--resume", "deadbe"]).resume,
        Some(Some("deadbe".to_string()))
    );
    // --resume --yolo does NOT swallow --yolo as the target.
    let p = mk(&["afi", "--resume", "--yolo"]);
    assert_eq!(p.resume, Some(None));
    assert!(p.yolo);
}

// ApprovalKind surfaces from a source-built runtime
#[test]
fn approval_kind_import_works() {
    let _ = ApprovalKind::Yolo;
}

// A value-less flag must not swallow the next flag. Losing `--effort` this way
// produced a finished run at an effort nobody asked for, with no refusal.
#[test]
fn parse_args_never_takes_another_flag_as_a_value() {
    let mk = |args: &[&str]| -> ParsedArgs {
        parse_args(&args.iter().map(ToString::to_string).collect::<Vec<_>>())
    };
    let p = mk(&["afi", "--summary", "--effort", "xhigh", "-f", "p.txt"]);
    assert_eq!(p.summary, None, "--summary must not eat --effort");
    assert_eq!(p.effort.as_deref(), Some("xhigh"));
    assert_eq!(p.prompt_file.as_deref(), Some("p.txt"));
    assert!(p.flag_errors.is_empty(), "{:?}", p.flag_errors);

    for flag in ["--source", "--session", "--approval", "--prompt-file"] {
        let p = mk(&["afi", flag, "--yolo"]);
        assert!(p.yolo, "{flag} must not eat --yolo");
    }
}

// `-` is the documented stdin form and is a value, not a flag.
#[test]
fn parse_args_still_reads_the_prompt_from_stdin() {
    let mk = |args: &[&str]| -> ParsedArgs {
        parse_args(&args.iter().map(ToString::to_string).collect::<Vec<_>>())
    };
    assert_eq!(mk(&["afi", "-f", "-"]).prompt_file.as_deref(), Some("-"));
    assert_eq!(
        mk(&["afi", "--prompt-file", "-", "--yolo"])
            .prompt_file
            .as_deref(),
        Some("-")
    );
    assert!(mk(&["afi", "-f", "-", "--yolo"]).yolo);
}
