use super::*;

#[test]
fn the_built_in_prompt_is_the_parts_in_order() {
    // The assembled string is the Anthropic cache prefix, so its layout is a
    // wire detail rather than a formatting preference: reordering the parts or
    // losing a blank-line seam misses the cache on every turn of every run.
    let system = system();
    assert!(system.starts_with(AGENT), "the agent line opens the prompt");
    assert!(
        system.ends_with(PROTOCOL_RUN_BASH),
        "the run_bash arg note closes it"
    );
    assert!(
        system.contains(&format!("{AGENT}\n\n{PROTOCOL}\n\n{SHELL}")),
        "the parts run agent, protocol, shell, in that order"
    );
    assert!(
        !system.contains("\n\n\n"),
        "the seams are one blank line, not two"
    );
}

#[test]
fn the_wire_contract_stands_alone() {
    // What a replaced prompt keeps. It has to carry both halves of the text
    // protocol - the call syntax and the argument name run_bash is given wrong
    // most often - and none of the guidance a supplied prompt is replacing.
    let contract = tool_protocol();
    assert!(contract.contains("[afi_tool_call]"));
    assert!(contract.contains("[/afi_tool_call]"));
    assert!(contract.contains("Text-protocol run_bash arg is `command`"));
    assert!(
        !contract.contains("Operating principles"),
        "the shell guidance is the part a replacement drops"
    );
    assert!(
        !contract.contains("You are a terminal coding agent"),
        "the supplied prompt says who the agent is"
    );
}
