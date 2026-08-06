//! Tool-schema translation.
//!
//! `OpenAI` wraps each tool in a `function` object; Anthropic takes the same
//! fields flat with `parameters` renamed to `input_schema`. The JSON Schema
//! bodies themselves are reused verbatim, so `tools::TOOLS` stays the single
//! source of truth for what afi can do.

use serde_json::{Value, json};

/// `[{"type":"function","function":{name,description,parameters}}]`
/// -> `[{name,description,input_schema}]`.
///
/// Returns `None` when nothing translatable is left, so the caller can omit the
/// `tools` key entirely rather than sending an empty array.
pub(super) fn translate_tools(tools: &Value) -> Option<Value> {
    let translated: Vec<Value> = tools
        .as_array()?
        .iter()
        .filter_map(translate_tool)
        .collect();
    if translated.is_empty() {
        return None;
    }
    Some(Value::Array(translated))
}

/// A single tool. Entries without a `function.name` are dropped rather than
/// sent malformed - Anthropic rejects the whole request for one bad tool.
fn translate_tool(entry: &Value) -> Option<Value> {
    let function = entry.get("function")?;
    let name = function.get("name").and_then(Value::as_str)?;
    let mut out = json!({
        "name": name,
        // A tool with no parameters still needs a schema object.
        "input_schema": function
            .get("parameters")
            .cloned()
            .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
    });
    if let Some(description) = function.get("description").and_then(Value::as_str) {
        out["description"] = Value::from(description);
    }
    Some(out)
}

/// `{"type":"function","function":{"name":N}}` -> `{"type":"tool","name":N}`.
pub(super) fn translate_tool_choice(choice: &Value) -> Option<Value> {
    let name = choice.pointer("/function/name").and_then(Value::as_str)?;
    Some(json!({"type": "tool", "name": name}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FINAL_ANSWER_TOOL, FINAL_ANSWER_TOOL_CHOICE};
    use crate::tools::TOOLS;

    /// The translated form of one tool, as a comparable value.
    fn flattened(function: &Value) -> Value {
        json!({
            "name": function["name"],
            "description": function["description"],
            // The schema is reused byte-for-byte under the new key.
            "input_schema": function["parameters"],
        })
    }

    #[test]
    fn real_tool_table_translates_every_entry() {
        let openai = TOOLS.as_array().expect("TOOLS is an array");
        let translated = translate_tools(&TOOLS).expect("some tools translate");
        let out = translated.as_array().expect("array");
        assert_eq!(out.len(), openai.len(), "no tool may be silently dropped");

        let expected: Vec<Value> = openai
            .iter()
            .map(|src| flattened(&src["function"]))
            .collect();
        // Equality over the whole entry also proves the `function` wrapper and
        // the `parameters` key are gone, with no extra keys added.
        assert_eq!(out, &expected);
    }

    #[test]
    fn translated_schemas_are_objects() {
        let translated = translate_tools(&TOOLS).unwrap();
        for tool in translated.as_array().unwrap() {
            assert_eq!(
                tool["input_schema"]["type"], "object",
                "Anthropic requires an object schema"
            );
        }
    }

    #[test]
    fn final_answer_tool_translates() {
        let translated = translate_tools(&json!([FINAL_ANSWER_TOOL.clone()])).unwrap();
        assert_eq!(translated[0]["name"], "final_answer");
        assert!(translated[0]["input_schema"].is_object());
    }

    #[test]
    fn missing_parameters_gets_an_empty_object_schema() {
        let tools = json!([{"type": "function", "function": {"name": "noargs"}}]);
        let translated = translate_tools(&tools).unwrap();
        assert_eq!(
            translated[0]["input_schema"],
            json!({"type": "object", "properties": {}})
        );
        assert!(translated[0].get("description").is_none());
    }

    #[test]
    fn entries_without_a_name_are_dropped() {
        let tools = json!([
            {"type": "function", "function": {"description": "nameless"}},
            {"type": "function", "function": {"name": "keep"}},
            {"not_a_function": true},
        ]);
        let translated = translate_tools(&tools).unwrap();
        assert_eq!(translated.as_array().unwrap().len(), 1);
        assert_eq!(translated[0]["name"], "keep");
    }

    #[test]
    fn empty_or_non_array_input_is_none() {
        assert!(translate_tools(&json!([])).is_none());
        assert!(translate_tools(&json!({})).is_none());
        assert!(translate_tools(&json!([{"function": {}}])).is_none());
    }

    #[test]
    fn real_tool_choice_translates() {
        let choice = translate_tool_choice(&FINAL_ANSWER_TOOL_CHOICE).expect("translates");
        assert_eq!(choice, json!({"type": "tool", "name": "final_answer"}));
    }

    #[test]
    fn unrecognized_tool_choice_is_none() {
        assert!(translate_tool_choice(&json!("auto")).is_none());
        assert!(translate_tool_choice(&json!({"type": "any"})).is_none());
    }
}
