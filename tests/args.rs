//! What argv means: which flags take a value, which refuse one, and what happens
//! to an argument afi does not have.
//!
//! Split from `config_units` because these are about the command line rather than
//! about the types it fills in, and because every one of them covers a way a
//! typed instruction used to be dropped without a word.

use afi::config::{ParsedArgs, parse_args};

/// `parse_args` over a borrowed argv, which is how every caller here spells it.
fn mk(args: &[&str]) -> ParsedArgs {
    parse_args(&args.iter().map(ToString::to_string).collect::<Vec<_>>())
}

#[test]
fn parse_args_resume_bare_vs_target() {
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

#[test]
fn a_flag_written_with_an_equals_sign_is_the_same_flag() {
    // Both spellings are how people write a long flag, and dropping one meant a
    // run configured by something other than what the command line said.
    let p = mk(&["afi", "--source=zai", "--effort=xhigh", "--summary=json"]);
    assert_eq!(p.source.as_deref(), Some("zai"));
    assert_eq!(p.effort.as_deref(), Some("xhigh"));
    assert_eq!(p.summary.as_deref(), Some("json"));
    assert!(p.flag_errors.is_empty(), "{:?}", p.flag_errors);
}

#[test]
fn an_equals_sign_carries_the_value_and_leaves_the_next_word_alone() {
    let p = mk(&["afi", "--source=zai", "--yolo"]);
    assert_eq!(p.source.as_deref(), Some("zai"));
    assert!(p.yolo);
}

#[test]
fn an_empty_equals_sign_goes_without_a_value() {
    // The same mistake as the spaced form, and refused the same way.
    let p = mk(&["afi", "--config="]);
    assert_eq!(p.config, None);
    assert_eq!(p.flag_errors.len(), 1, "{:?}", p.flag_errors);
}

#[test]
fn a_flag_that_takes_no_value_refuses_one() {
    // `--read-only=false` reads as "off". Taking the token as a bare
    // `--read-only` would turn the posture on, which is the opposite.
    let p = mk(&["afi", "--read-only=false"]);
    assert!(
        !p.read_only,
        "the posture was turned on by a value meaning off"
    );
    assert_eq!(p.flag_errors.len(), 1, "{:?}", p.flag_errors);
    assert!(
        p.flag_errors[0]
            .message
            .contains("--read-only takes no value"),
        "{:?}",
        p.flag_errors
    );

    let p = mk(&["afi", "--yolo=0"]);
    assert!(!p.yolo);
    assert_eq!(p.flag_errors.len(), 1, "{:?}", p.flag_errors);
}

#[test]
fn an_argument_afi_does_not_have_refuses_the_run() {
    // Every one of these used to be ignored, so `--red-only` left a run with
    // writes enabled while the command line said otherwise.
    for arg in ["--red-only", "--allowed-tool", "-x", "prompt.txt"] {
        let p = mk(&["afi", arg]);
        assert_eq!(p.flag_errors.len(), 1, "{arg}: {:?}", p.flag_errors);
        assert!(
            p.flag_errors[0].message.contains("unknown argument"),
            "{arg}: {:?}",
            p.flag_errors
        );
    }
    // A flag answered before this one is not reported as unknown.
    for arg in ["--help", "-h", "--version", "-V"] {
        let p = mk(&["afi", arg]);
        assert!(p.flag_errors.is_empty(), "{arg}: {:?}", p.flag_errors);
    }
}

#[test]
fn a_prompt_file_still_reads_stdin_from_a_dash() {
    let p = mk(&["afi", "-f", "-"]);
    assert_eq!(p.prompt_file.as_deref(), Some("-"));
    assert!(p.flag_errors.is_empty(), "{:?}", p.flag_errors);
    let p = mk(&["afi", "--prompt-file", "-"]);
    assert_eq!(p.prompt_file.as_deref(), Some("-"));
    assert!(p.flag_errors.is_empty(), "{:?}", p.flag_errors);
}

#[test]
fn parse_args_never_takes_another_flag_as_a_value() {
    // Two properties, and the second is why the first is not enough on its own:
    // the following flag still applies, and the flag that went without a value
    // refuses the run rather than being dropped. `afi --summary --effort xhigh`
    // asked for a summary; producing none silently is the failure this refuses.
    let p = mk(&["afi", "--summary", "--effort", "xhigh", "-f", "p.txt"]);
    assert_eq!(p.summary, None, "--summary must not eat --effort");
    assert_eq!(p.effort.as_deref(), Some("xhigh"));
    assert_eq!(p.prompt_file.as_deref(), Some("p.txt"));
    assert_eq!(p.flag_errors.len(), 1, "{:?}", p.flag_errors);
    assert!(
        p.flag_errors[0].message.contains("--summary needs a value"),
        "{:?}",
        p.flag_errors
    );
}

#[test]
fn every_value_flag_leaves_the_flag_after_it_standing() {
    for flag in [
        "--source",
        "--session",
        "--approval",
        "--prompt-file",
        "--budget-usd",
    ] {
        let p = mk(&["afi", flag, "--yolo"]);
        assert!(p.yolo, "{flag} must not eat --yolo");
        assert_eq!(p.flag_errors.len(), 1, "{flag}: {:?}", p.flag_errors);
    }
}

#[test]
fn parse_args_still_reads_the_prompt_from_stdin() {
    assert_eq!(mk(&["afi", "-f", "-"]).prompt_file.as_deref(), Some("-"));
    assert_eq!(
        mk(&["afi", "--prompt-file", "-", "--yolo"])
            .prompt_file
            .as_deref(),
        Some("-")
    );
    assert!(mk(&["afi", "-f", "-", "--yolo"]).yolo);
}

#[test]
fn the_budget_flag_takes_its_value_either_way_and_refuses_a_blank() {
    // A cap given wrongly must refuse the run rather than leaving it uncapped:
    // `afi --budget-usd "$CAP"` with `CAP` unset arrives as an empty argument,
    // and that is the form a CI script is written in.
    assert_eq!(
        mk(&["afi", "--budget-usd", "5"]).budget_usd.as_deref(),
        Some("5")
    );
    assert_eq!(
        mk(&["afi", "--budget-usd=2.50"]).budget_usd.as_deref(),
        Some("2.50")
    );
    for args in [
        vec!["afi", "--budget-usd"],
        vec!["afi", "--budget-usd", ""],
        vec!["afi", "--budget-usd="],
    ] {
        let p = mk(&args);
        assert_eq!(p.budget_usd, None, "{args:?}");
        assert_eq!(p.flag_errors.len(), 1, "{args:?}: {:?}", p.flag_errors);
    }
}
