use super::*;
use crate::model::FINAL_ANSWER_TOOL;
use crate::tools::TOOLS;

/// Assert the exact permitted set, so a test cannot pass by checking only the
/// tools it happened to think of.
fn assert_permits_exactly(policy: &ToolPolicy, expected: &[&str]) {
    let permitted = policy.permitted();
    assert_eq!(permitted, expected, "permitted set");
    for name in known_tool_names() {
        assert_eq!(
            policy.permits(name),
            expected.contains(name),
            "permits({name})"
        );
    }
}

/// The names a filtered schema array advertises.
fn advertised(tools: &Value) -> Vec<String> {
    tools
        .as_array()
        .expect("filter_tools returns an array")
        .iter()
        .filter_map(|entry| schema_name(entry).map(str::to_string))
        .collect()
}

#[test]
fn the_default_policy_changes_nothing() {
    let policy = ToolPolicy::default();
    assert!(policy.is_unrestricted());
    for name in known_tool_names() {
        assert!(policy.permits(name), "{name} should be permitted");
    }
    assert_eq!(policy.filter_tools(&TOOLS), *TOOLS);
    assert_eq!(policy.describe(), "all");
}

#[test]
fn a_blank_list_is_an_unset_list() {
    // `AFI_ALLOWED_TOOLS=""` from an unset shell variable must not lock the run
    // out of every tool.
    for blank in ["", "  ", ",", " , ,"] {
        let policy = ToolPolicy::parse(Some(blank), Some(blank));
        assert!(policy.is_unrestricted(), "{blank:?} should be unrestricted");
        assert!(policy.permits("run_bash"));
    }
}

#[test]
fn an_allow_list_is_exhaustive() {
    let policy = ToolPolicy::parse(Some("read_file,list_dir"), None);
    assert_permits_exactly(&policy, &["read_file", "list_dir"]);
    assert_eq!(policy.describe(), "read_file,list_dir");
}

#[test]
fn a_deny_list_leaves_the_rest_alone() {
    let policy = ToolPolicy::parse(None, Some("write_file,edit_file"));
    assert_permits_exactly(
        &policy,
        &["read_file", "list_dir", "run_bash", "wait_background"],
    );
}

#[test]
fn deny_beats_allow() {
    let policy = ToolPolicy::parse(Some("read_file,run_bash"), Some("run_bash"));
    assert_permits_exactly(&policy, &["read_file"]);
    assert_eq!(policy.describe(), "read_file");
}

#[test]
fn separators_and_case_are_forgiving() {
    let policy = ToolPolicy::parse(Some(" READ_FILE , list_dir\tRUN_BASH "), None);
    assert_permits_exactly(&policy, &["read_file", "list_dir", "run_bash"]);
    assert!(policy.unknown_names().is_empty());
}

#[test]
fn an_unknown_name_fails_closed_rather_than_widening_the_policy() {
    // The dangerous typo: `run_bsah` in a deny list would otherwise match
    // nothing and leave `run_bash` available, so the run must not proceed.
    let policy = ToolPolicy::parse(None, Some("run_bsah"));
    assert_eq!(policy.unknown_names(), ["run_bsah"]);
    assert!(!policy.permits("run_bash"));
    assert!(!policy.permits("read_file"));
    assert!(!policy.is_unrestricted());
    assert_eq!(policy.describe(), "none");
}

#[test]
fn unknown_names_are_collected_from_both_lists_once() {
    let policy = ToolPolicy::parse(Some("read_file,nope"), Some("nope,alsonope"));
    assert_eq!(policy.unknown_names(), ["alsonope", "nope"]);
}

#[test]
fn a_valid_policy_reports_no_unknown_names() {
    let policy = ToolPolicy::parse(Some("read_file"), Some("run_bash"));
    assert!(policy.unknown_names().is_empty());
}

#[test]
fn final_answer_is_never_blockable() {
    // The forced-final path offers `final_answer` alone and reads the answer out
    // of the call, so blocking it would strand the turn instead of limiting it.
    for policy in [
        ToolPolicy::parse(Some("read_file"), None),
        ToolPolicy::parse(None, Some("final_answer")),
        ToolPolicy::parse(Some("run_bash"), Some("run_bash")),
    ] {
        assert!(policy.permits("final_answer"));
    }
}

#[test]
fn final_answer_is_not_a_registered_tool_name() {
    // Rejecting it as unknown is what makes `--disallowed-tools final_answer`
    // fail loudly instead of looking honoured.
    assert!(!known_tool_names().contains(&"final_answer"));
    let policy = ToolPolicy::parse(None, Some("final_answer"));
    assert_eq!(policy.unknown_names(), ["final_answer"]);
}

#[test]
fn filtering_hides_blocked_schemas_from_the_model() {
    let policy = ToolPolicy::parse(Some("read_file,list_dir"), None);
    let filtered = policy.filter_tools(&TOOLS);
    assert_eq!(advertised(&filtered), ["read_file", "list_dir"]);
}

#[test]
fn filtering_preserves_the_registration_order_of_what_survives() {
    let policy = ToolPolicy::parse(None, Some("edit_file"));
    let filtered = policy.filter_tools(&TOOLS);
    assert_eq!(
        advertised(&filtered),
        [
            "read_file",
            "write_file",
            "list_dir",
            "run_bash",
            "wait_background"
        ]
    );
}

#[test]
fn filtering_everything_out_yields_an_empty_array_not_a_null() {
    // The caller distinguishes "no tools" from "unfiltered" by the array being
    // empty, and omits the key rather than sending `[]`.
    let policy = ToolPolicy::parse(None, Some(&known_tool_names().join(",")));
    let filtered = policy.filter_tools(&TOOLS);
    assert_eq!(filtered, Value::Array(vec![]));
    assert_eq!(policy.describe(), "none");
}

#[test]
fn filtering_keeps_the_forced_final_tool() {
    let policy = ToolPolicy::parse(Some("read_file"), None);
    let forced = serde_json::json!([FINAL_ANSWER_TOOL.clone()]);
    assert_eq!(advertised(&policy.filter_tools(&forced)), ["final_answer"]);
}

#[test]
fn a_malformed_schema_entry_is_left_to_the_protocol_layers() {
    let policy = ToolPolicy::parse(Some("read_file"), None);
    let odd = serde_json::json!([{"type": "function"}, {"nonsense": true}]);
    assert_eq!(policy.filter_tools(&odd), odd);
}

#[test]
fn a_non_array_schema_value_passes_through_untouched() {
    let policy = ToolPolicy::parse(Some("read_file"), None);
    let not_an_array = Value::Null;
    assert_eq!(policy.filter_tools(&not_an_array), not_an_array);
}

/// The read-only posture permits exactly the tools that cannot change anything.
#[test]
fn read_only_denies_every_mutating_tool() {
    let policy = ToolPolicy::default().read_only();
    assert_permits_exactly(&policy, &["read_file", "list_dir"]);
    assert!(policy.is_read_only());
}

/// `wait_background` deletes the log it read, so the posture denies it even
/// though the approval gate never asks about it. Two lists, two questions - and
/// a test, because folding them back into one is the obvious tidy-up.
#[test]
fn read_only_denies_the_wait_the_approval_gate_ignores() {
    assert!(!ToolPolicy::default().read_only().permits("wait_background"));
    assert!(!is_mutating("wait_background"));
}

/// The property the posture exists for. An allow list cannot widen it, so a
/// wrapper that sets it cannot be argued out of it by a later argument.
#[test]
fn read_only_outranks_an_allow_list_naming_a_writer() {
    let policy = ToolPolicy::parse(Some("read_file,run_bash,write_file"), None).read_only();
    assert_permits_exactly(&policy, &["read_file"]);
    assert!(policy.is_read_only());
    assert!(!policy.permits("run_bash"));
}

/// Applying it twice is applying it once: the caller may not know whether a
/// wrapper already did.
#[test]
fn read_only_is_idempotent() {
    let once = ToolPolicy::default().read_only();
    let twice = ToolPolicy::default().read_only().read_only();
    assert_eq!(once, twice);
}

/// A read-only run is a restricted run, so the banner and summary show it.
#[test]
fn read_only_is_not_unrestricted_and_describes_its_set() {
    let policy = ToolPolicy::default().read_only();
    assert!(!policy.is_unrestricted());
    assert_eq!(policy.describe(), "read_file,list_dir");
}

/// `is_read_only` reports the effect, not how it was asked for.
#[test]
fn a_deny_list_naming_the_same_tools_reads_as_read_only() {
    let spelled = ToolPolicy::parse(None, Some("write_file,edit_file,run_bash,wait_background"));
    assert!(spelled.is_read_only());
    // One short of the set is not the posture, however close it looks.
    assert!(!ToolPolicy::parse(None, Some("write_file,edit_file,run_bash")).is_read_only());
    assert!(!ToolPolicy::default().is_read_only());
}

/// Every name either list holds has to be a real tool, or the posture would deny
/// a name nothing dispatches and quietly permit the tool it was meant to stop.
#[test]
fn the_policy_lists_name_only_registered_tools() {
    for name in read_only_denied() {
        assert!(
            known_tool_names().contains(&name),
            "{name} is not a registered tool"
        );
    }
    for name in MUTATING_TOOLS {
        assert!(is_mutating(name));
    }
    assert!(!is_mutating("read_file"));
    assert!(!is_mutating("nope"));
}

/// A read-only run never offers a writer's schema, so the model does not learn
/// the tool exists.
#[test]
fn read_only_filters_the_advertised_schemas() {
    let policy = ToolPolicy::default().read_only();
    let filtered = policy.filter_tools(&TOOLS);
    let names = advertised(&filtered);
    assert_eq!(names, ["read_file", "list_dir"]);
}
