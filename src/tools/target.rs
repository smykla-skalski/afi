//! The path a tool call acts on.
//!
//! Which tools carry one is a fact about the tool registry, so it lives here rather
//! than in each caller that needs it. `policy` states the rule this follows: a second
//! hard-coded copy of a tool-name list "would eventually disagree with this one", and
//! the way this one would disagree is by going quiet. A caller that guesses wrong gets
//! `None` and simply stops working, with nothing red to show for it - so the list is
//! pinned to the schemas by a test rather than by care.

use serde_json::Value;

/// The argument every tool below names its path under.
const PATH_KEY: &str = "path";

/// The tools whose calls name a path.
///
/// `run_bash` is deliberately absent, and that is a decision the schema does not state:
/// its path, if it has one, is somewhere inside a shell command, and guessing would read
/// a directory off a substring that happened to look like one. So this is a list rather
/// than a derivation, and the test below is what keeps it honest.
const PATH_TOOLS: [&str; 4] = ["read_file", "write_file", "edit_file", "list_dir"];

/// The path `args` names for tool `name`, or `None` when that tool acts on none.
#[must_use]
pub fn path_arg<'a>(name: &str, args: &'a Value) -> Option<&'a str> {
    if !PATH_TOOLS.contains(&name) {
        return None;
    }
    args.get(PATH_KEY)?.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::TOOLS;
    use serde_json::json;

    #[test]
    fn every_tool_named_here_declares_the_argument_this_reads() {
        // Both halves, because only one of them fails loudly on its own. A tool that is
        // renamed breaks the name lookup; a tool whose schema renames `path` leaves this
        // answering `None` forever, the subtree instruction loader never fires, and
        // nothing else notices.
        let schemas = TOOLS.as_array().expect("the registry is an array");
        for tool in PATH_TOOLS {
            let schema = schemas
                .iter()
                .find(|entry| entry["function"]["name"] == tool)
                .unwrap_or_else(|| panic!("{tool} is not a registered tool"));
            assert!(
                !schema["function"]["parameters"]["properties"][PATH_KEY].is_null(),
                "{tool} does not declare a {PATH_KEY:?} argument"
            );
        }
    }

    #[test]
    fn every_tool_that_declares_the_argument_is_named_here() {
        // The other direction, which is the one a removal takes. The approval prompt
        // reads its path through here, and `risk::extract_action_path` reads that string
        // back to fill `path_scope` - so dropping a name from the list renders `write ?`
        // and hands the classifier `cwd/?` as the target, in project scope, for a write
        // that may land anywhere. No exclusion list is needed: `run_bash` takes a
        // `command` and `wait_background` a `log_path`, so declaring `path` is exactly
        // the property that means this module owns the tool.
        for schema in TOOLS.as_array().expect("the registry is an array") {
            let function = &schema["function"];
            if function["parameters"]["properties"][PATH_KEY].is_null() {
                continue;
            }
            let name = function["name"].as_str().expect("a tool has a name");
            assert!(
                PATH_TOOLS.contains(&name),
                "{name} declares {PATH_KEY:?} but is missing from PATH_TOOLS, so its \
                 approval prompt would name no target"
            );
        }
    }

    #[test]
    fn a_tool_that_acts_on_no_path_answers_none() {
        assert_eq!(path_arg("run_bash", &json!({"command": "ls /etc"})), None);
        assert_eq!(path_arg("wait_background", &json!({"pid": 7})), None);
        assert_eq!(path_arg("final_answer", &json!({})), None);
    }

    #[test]
    fn a_path_tool_answers_with_its_argument() {
        for name in PATH_TOOLS {
            assert_eq!(
                path_arg(name, &json!({"path": "src/main.rs"})),
                Some("src/main.rs"),
                "{name}"
            );
            assert_eq!(path_arg(name, &json!({})), None, "{name} with no path");
        }
    }
}
