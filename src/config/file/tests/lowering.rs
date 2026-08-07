//! Reading one file: which variables a key becomes, and what is refused.

use std::collections::HashMap;
use std::path::Path;

use super::super::lower;

/// Lower `body` and return its pairs as a map, asserting nothing was refused.
fn pairs(body: &str) -> HashMap<String, String> {
    let read = lower::read(Path::new("config.json"), body);
    assert_eq!(read.refusals, Vec::<String>::new(), "unexpected refusals");
    read.pairs.into_iter().collect()
}

/// Lower `body` and return the refusals, asserting there was at least one.
fn refusals(body: &str) -> Vec<String> {
    let read = lower::read(Path::new("config.json"), body);
    assert!(!read.refusals.is_empty(), "expected a refusal");
    read.refusals
}

/// The one refusal `body` produces.
fn refusal(body: &str) -> String {
    let mut all = refusals(body);
    assert_eq!(all.len(), 1, "expected exactly one refusal: {all:?}");
    all.remove(0)
}

#[test]
fn scalars_become_their_variables() {
    let out = pairs(
        r#"{"active": "zai", "max_tokens": 8000, "read_only": true,
             "effort": "high", "recovery_top_p": 0.9}"#,
    );
    assert_eq!(out.get("AFI_ACTIVE").unwrap(), "zai");
    assert_eq!(out.get("AFI_MAX_TOKENS").unwrap(), "8000");
    assert_eq!(out.get("AFI_READ_ONLY").unwrap(), "1");
    assert_eq!(out.get("AFI_EFFORT").unwrap(), "high");
    assert_eq!(out.get("AFI_RECOVERY_TOP_P").unwrap(), "0.9");
}

#[test]
fn a_false_flag_is_written_rather_than_dropped() {
    // A project file turning off what the user file turned on has to say
    // something `read_only_requested` will act on.
    let out = pairs(r#"{"read_only": false}"#);
    assert_eq!(out.get("AFI_READ_ONLY").unwrap(), "0");
}

#[test]
fn tool_lists_take_either_shape() {
    let listed = pairs(r#"{"allowed_tools": ["read_file", "list_dir"]}"#);
    assert_eq!(
        listed.get("AFI_ALLOWED_TOOLS").unwrap(),
        "read_file,list_dir"
    );
    let inline = pairs(r#"{"disallowed_tools": "run_bash, write_file"}"#);
    assert_eq!(
        inline.get("AFI_DISALLOWED_TOOLS").unwrap(),
        "run_bash, write_file"
    );
}

#[test]
fn sources_become_their_prefixed_variables() {
    let out = pairs(
        r#"{"sources": {"zai": {
             "base_url": "https://api.z.ai/api/paas/v4",
             "api_key": "$ZAI_API_KEY",
             "model": "glm-4.6",
             "protocol": "openai",
             "extra_body": {"provider": {"order": ["z-ai"]}}
           }}}"#,
    );
    assert_eq!(
        out.get("AFI_SOURCE_ZAI_BASE_URL").unwrap(),
        "https://api.z.ai/api/paas/v4"
    );
    assert_eq!(out.get("AFI_SOURCE_ZAI_API_KEY").unwrap(), "$ZAI_API_KEY");
    assert_eq!(out.get("AFI_SOURCE_ZAI_MODEL").unwrap(), "glm-4.6");
    assert_eq!(out.get("AFI_SOURCE_ZAI_PROTOCOL").unwrap(), "openai");
    // The one shape the file exists to give: an object stays an object here and
    // becomes the single-line JSON the variable holds.
    assert_eq!(
        out.get("AFI_SOURCE_ZAI_EXTRA_BODY").unwrap(),
        r#"{"provider":{"order":["z-ai"]}}"#
    );
}

#[test]
fn source_order_becomes_the_sources_list() {
    let out = pairs(r#"{"source_order": ["zai", "anthropic"]}"#);
    assert_eq!(out.get("AFI_SOURCES").unwrap(), "zai,anthropic");
}

#[test]
fn prices_are_written_back_whole() {
    let out = pairs(r#"{"prices": {"glm-4.6": {"input": 0.6, "output": 2.2}}}"#);
    assert_eq!(
        out.get("AFI_PRICES").unwrap(),
        r#"{"glm-4.6":{"input":0.6,"output":2.2}}"#
    );
}

#[test]
fn the_anthropic_block_keeps_its_own_variables() {
    let out = pairs(
        r#"{"anthropic": {
             "model": "claude-opus-5",
             "extra_body": {"thinking": {"type": "adaptive"}},
             "federation": {
               "rule_id": "rule_1", "organization_id": "org_1",
               "service_account_id": "sa_1", "workspace_id": "ws_1"
             }
           }}"#,
    );
    assert_eq!(out.get("AFI_ANTHROPIC_MODEL").unwrap(), "claude-opus-5");
    assert_eq!(
        out.get("AFI_ANTHROPIC_EXTRA_BODY").unwrap(),
        r#"{"thinking":{"type":"adaptive"}}"#
    );
    // The un-prefixed names the official SDKs use, so a workspace already
    // configured for them needs no second spelling.
    assert_eq!(out.get("ANTHROPIC_FEDERATION_RULE_ID").unwrap(), "rule_1");
    assert_eq!(out.get("ANTHROPIC_ORGANIZATION_ID").unwrap(), "org_1");
    assert_eq!(out.get("ANTHROPIC_SERVICE_ACCOUNT_ID").unwrap(), "sa_1");
    assert_eq!(out.get("ANTHROPIC_WORKSPACE_ID").unwrap(), "ws_1");
}

#[test]
fn a_blank_file_sets_nothing_and_is_not_an_error() {
    let read = lower::read(Path::new("config.json"), "  \n\t\n");
    assert!(read.pairs.is_empty());
    assert!(read.refusals.is_empty());
}

#[test]
fn an_unknown_key_names_the_file_the_key_and_the_nearest_one() {
    let message = refusal(r#"{"activ": "zai"}"#);
    assert_eq!(
        message,
        "config.json: unknown key \"activ\" (did you mean \"active\"?)"
    );
}

#[test]
fn an_unknown_key_with_nothing_close_says_so_without_guessing() {
    let message = refusal(r#"{"telemetry": true}"#);
    assert_eq!(message, "config.json: unknown key \"telemetry\"");
}

#[test]
fn an_unknown_nested_key_names_its_path() {
    assert_eq!(
        refusal(r#"{"sources": {"zai": {"base_urls": "http://x"}}}"#),
        "config.json: unknown key \"sources.zai.base_urls\" (did you mean \"base_url\"?)"
    );
    assert_eq!(
        refusal(r#"{"anthropic": {"federation": {"rule": "r"}}}"#),
        "config.json: unknown key \"anthropic.federation.rule\" (did you mean \"rule_id\"?)"
    );
}

#[test]
fn the_block_that_carries_structure_is_suggested_too() {
    assert_eq!(
        refusal(r#"{"source": {}}"#),
        "config.json: unknown key \"source\" (did you mean \"sources\"?)"
    );
    assert_eq!(
        refusal(r#"{"anthropic": {"federatio": {}}}"#),
        "config.json: unknown key \"anthropic.federatio\" (did you mean \"federation\"?)"
    );
}

#[test]
fn a_value_of_the_wrong_shape_says_what_was_wanted() {
    assert_eq!(
        refusal(r#"{"max_tokens": "8000"}"#),
        "config.json: max_tokens must be a whole number from 0 to 4294967295 (got string)"
    );
    assert_eq!(
        refusal(r#"{"read_only": "yes"}"#),
        "config.json: read_only must be true or false (got string)"
    );
    assert_eq!(
        refusal(r#"{"active": null}"#),
        "config.json: active must be a string (got null)"
    );
    assert_eq!(
        refusal(r#"{"sources": {"zai": {"extra_body": "{}"}}}"#),
        "config.json: sources.zai.extra_body must be a JSON object (got string)"
    );
}

#[test]
fn a_setting_added_after_this_layer_still_has_a_key() {
    // `system_prompt_file` and `system_prompt_mode` arrived with `--system-prompt-*`.
    // The rule is that every AFI_* setting has a key, so a new variable that stops
    // at the flag is the rule quietly breaking.
    let out = pairs(r#"{"system_prompt_file": "p.md", "system_prompt_mode": "append"}"#);
    assert_eq!(out.get("AFI_SYSTEM_PROMPT_FILE").unwrap(), "p.md");
    assert_eq!(out.get("AFI_SYSTEM_PROMPT_MODE").unwrap(), "append");
    // The mode is checked against its own parser, not a copy of its list.
    assert_eq!(
        refusal(r#"{"system_prompt_mode": "prepend"}"#),
        "config.json: system_prompt_mode must be one of replace, append (got \"prepend\")"
    );
}

#[test]
fn an_empty_allow_list_is_refused_rather_than_granting_everything() {
    // `[]` reaches the policy as a blank list, which means "every tool" - the
    // opposite of what writing it says. A deny list has no such inversion.
    let message = refusal(r#"{"allowed_tools": []}"#);
    assert!(message.contains("must name at least one tool"), "{message}");
    let empty_deny = pairs(r#"{"disallowed_tools": []}"#);
    assert_eq!(empty_deny.get("AFI_DISALLOWED_TOOLS").unwrap(), "");
}

#[test]
fn a_count_past_what_its_reader_holds_is_refused() {
    // `ModelConfig` parses these as `u32` and keeps its default on anything else,
    // so accepting a wider number would take the setting silently.
    assert_eq!(
        refusal(r#"{"max_tokens": 5000000000}"#),
        "config.json: max_tokens must be a whole number from 0 to 4294967295 (got number)"
    );
    let fits = pairs(r#"{"max_tokens": 4294967295}"#);
    assert_eq!(fits.get("AFI_MAX_TOKENS").unwrap(), "4294967295");
}

#[test]
fn a_signed_setting_can_say_what_its_variable_can() {
    // `-1` is llama.cpp's "the whole context" and `AFI_RECOVERY_REPEAT_LAST_N`
    // takes it, so the file has to as well.
    let out = pairs(r#"{"recovery_repeat_last_n": -1, "read_file_lines": 400}"#);
    assert_eq!(out.get("AFI_RECOVERY_REPEAT_LAST_N").unwrap(), "-1");
    assert_eq!(out.get("AFI_READ_FILE_LINES").unwrap(), "400");
    // A count that its reader holds unsigned still refuses one.
    assert_eq!(
        refusal(r#"{"max_tokens": -1}"#),
        "config.json: max_tokens must be a whole number from 0 to 4294967295 (got number)"
    );
}

#[test]
fn a_fractional_count_is_refused_rather_than_rounded() {
    // Every reader of these parses an integer and silently keeps its default on
    // anything else, which is the drop the file exists to end.
    assert_eq!(
        refusal(r#"{"max_tokens": 8000.5}"#),
        "config.json: max_tokens must be a whole number from 0 to 4294967295 (got number)"
    );
}

#[test]
fn a_level_outside_the_closed_set_is_refused_with_the_set() {
    assert_eq!(
        refusal(r#"{"effort": "hihg"}"#),
        "config.json: effort must be one of low, medium, high, xhigh, max (got \"hihg\")"
    );
    assert_eq!(
        refusal(r#"{"summary": "yaml"}"#),
        "config.json: summary must be one of json, none (got \"yaml\")"
    );
    assert_eq!(
        refusal(r#"{"sources": {"zai": {"protocol": "anthropic-oath"}}}"#),
        "config.json: sources.zai.protocol must be one of openai, openai-compat, \
         anthropic, anthropic-api-key, anthropic-oauth (got \"anthropic-oath\")"
    );
}

#[test]
fn a_misspelled_price_class_is_refused() {
    assert_eq!(
        refusal(r#"{"prices": {"glm-4.6": {"inputs": 1}}}"#),
        "config.json: unknown key \"prices.glm-4.6.inputs\" (did you mean \"input\"?)"
    );
}

#[test]
fn a_rate_that_is_not_a_number_is_refused() {
    assert_eq!(
        refusal(r#"{"prices": {"glm-4.6": {"input": "0.6"}}}"#),
        "config.json: prices.glm-4.6.input must be a number (got string)"
    );
}

#[test]
fn a_source_name_with_a_capital_in_it_is_refused() {
    // It would register lowercased, so `"active": "Zai"` would name a source that
    // is sitting right there and match nothing.
    let message = refusal(r#"{"sources": {"Zai": {"model": "a"}}}"#);
    assert!(message.contains("is not a usable source name"), "{message}");
}

#[test]
fn a_source_name_that_cannot_become_a_variable_is_refused() {
    let message = refusal(r#"{"sources": {"my source": {"model": "a"}}}"#);
    assert!(message.contains("is not a usable source name"), "{message}");
    let empty = refusal(r#"{"sources": {"": {"model": "a"}}}"#);
    assert!(empty.contains("is not a usable source name"), "{empty}");
}

#[test]
fn the_names_a_source_may_have_all_work() {
    let out = pairs(r#"{"sources": {"my-source_2": {"model": "a"}}}"#);
    assert_eq!(out.get("AFI_SOURCE_MY-SOURCE_2_MODEL").unwrap(), "a");
}

#[test]
fn a_file_that_is_not_json_names_itself_and_the_spot() {
    let message = refusal("{\"active\": \"zai\",}");
    assert!(
        message.starts_with("config.json: is not readable JSON (") && message.contains("line 1"),
        "{message}"
    );
}

#[test]
fn a_file_that_is_not_an_object_is_refused() {
    assert_eq!(
        refusal(r#"["active"]"#),
        "config.json: the file must be a JSON object (got array)"
    );
}

#[test]
fn every_bad_key_is_reported_rather_than_only_the_first() {
    // Fixing a config file one failed run at a time is its own punishment.
    let all = refusals(r#"{"activ": "zai", "max_tokens": "8000", "telemetry": 1}"#);
    assert_eq!(all.len(), 3, "{all:?}");
}
