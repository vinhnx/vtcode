use serde_json::Value;
use vtcode_core::config::constants::tools;

use crate::agent::runloop::text_tools::parse_args::normalize_command_string;

/// Schema metadata for a textual tool.
///
/// Kept separate from the parser so parsers do not need to know which
/// parameters are required for which tool.
pub(crate) struct TextualToolSchema {
    /// The named parameter to which the first positional argument should be
    /// mapped, if any.
    pub positional_param: Option<&'static str>,
    /// Parameters that must be present after parsing and positional mapping.
    pub required_params: &'static [&'static str],
}

/// Returns the schema for a canonical textual tool name.
pub(crate) fn textual_tool_schema(name: &str) -> Option<TextualToolSchema> {
    match name {
        tools::EXEC_COMMAND | tools::RUN_PTY_CMD => Some(TextualToolSchema {
            positional_param: Some("command"),
            required_params: &["command"],
        }),
        tools::GREP_FILE => Some(TextualToolSchema {
            positional_param: Some("pattern"),
            required_params: &["pattern"],
        }),
        tools::READ_FILE | tools::WRITE_FILE | tools::EDIT_FILE => Some(TextualToolSchema {
            positional_param: Some("path"),
            required_params: &["path"],
        }),
        _ => None,
    }
}

/// Applies tool-schema normalization and validation to parsed arguments.
///
/// - Maps positional arguments to the tool's primary named parameter.
/// - Normalizes a string `command` value into an array when possible.
/// - Ensures all required parameters are present.
/// - Rejects positional arguments for tools that do not declare a positional
///   mapping, which prevents arbitrary function-call-shaped text from being
///   accepted as a tool call.
///
/// Returns `true` if the arguments pass validation, `false` otherwise.
pub(crate) fn normalize_and_validate_tool_args(name: &str, args: &mut Value, positional: Vec<Value>) -> bool {
    let Some(schema) = textual_tool_schema(name) else {
        // Tools without an explicit schema cannot accept positional arguments;
        // otherwise any function call like `printf!("hi")` would look like a
        // tool call.
        return positional.is_empty();
    };
    let Some(map) = args.as_object_mut() else {
        return false;
    };

    let positional_param = schema.positional_param;
    if let Some(param) = positional_param {
        if !positional.is_empty() && !map.contains_key(param) {
            let Some(mapped) = map_positional_to_param(param, &positional) else {
                return false;
            };
            map.insert(param.to_string(), mapped);
        }
    } else if !positional.is_empty() {
        // Tool has no defined positional mapping; reject rather than guessing.
        return false;
    }

    if positional_param == Some("command")
        && let Some(Value::String(command)) = map.get("command").cloned()
        && let Some(array) = normalize_command_string(&command)
    {
        map.insert("command".to_string(), Value::Array(array));
    }

    for required in schema.required_params {
        if !map.contains_key(*required) {
            return false;
        }
    }

    true
}

fn map_positional_to_param(param: &str, positional: &[Value]) -> Option<Value> {
    match param {
        "command" => map_positional_command(positional),
        _ => {
            // For non-command positional params, use the first positional string.
            positional.first().and_then(|value| match value {
                Value::String(_) => Some(value.clone()),
                _ => None,
            })
        }
    }
}

fn map_positional_command(positional: &[Value]) -> Option<Value> {
    let mut parts = Vec::new();
    let mut all_strings = true;
    for value in positional {
        if let Value::String(part) = value {
            parts.push(part.clone());
        } else {
            all_strings = false;
            break;
        }
    }

    if !all_strings {
        return positional.first().cloned();
    }

    if parts.is_empty() {
        return None;
    }

    if parts.len() == 1 {
        let command = &parts[0];
        if let Some(array) = normalize_command_string(command) {
            Some(Value::Array(array))
        } else {
            Some(Value::String(command.clone()))
        }
    } else {
        Some(Value::Array(parts.into_iter().map(Value::String).collect()))
    }
}
