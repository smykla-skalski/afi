//! Unit-level ports from `tests/test_sources.py`: `parse_extra_body`,
//! `Source::clean_model_id`, `Source::is_local`, and `parse_args`.

use afi::config::{ParsedArgs, parse_args, parse_extra_body};
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
