//! Token-efficient tool result reducers.
//!
//! When tool results are too large for the context window, reducers truncate
//! them to keep only high-signal information. This follows the context
//! engineering principle: "return only summaries or a small number of results
//! to the model."

use std::borrow::Cow;

use serde_json::Value;

use crate::config::constants::tools;
use crate::tools::tool_intent;

/// Reduce a tool result to be more token-efficient.
///
/// Dispatches to the appropriate reducer based on the tool name.
pub fn reduce_tool_result(tool_name: &str, result: Value) -> Value {
    let canonical_tool_name = tool_intent::canonical_command_session_tool_name(tool_name).unwrap_or(tool_name);
    match canonical_tool_name {
        tools::READ_FILE => reduce_read_file_result(result),
        tools::UNIFIED_EXEC => reduce_command_result(result),
        _ => result,
    }
}

/// Strip TUI-only display fields from a tool result before it enters model
/// context.
///
/// The `view` field produced by `task_tracker` (and its planning-workflow
/// variant) is rendered only by the TUI — branch symbols, status icons, and
/// per-item display lines that duplicate the structured `checklist` already
/// present in the same payload. Sending it to the model wastes tokens on every
/// tracker call, and the waste grows as items accumulate `outcome`/`verify`
/// metadata (observed growing from ~5 KB to ~12 KB per call in session logs,
/// with `view` accounting for ~3 KB of each).
///
/// The TUI reads `view` from the original tool-output `Value` via the
/// pipeline-output / event path, not from the model-facing string this is
/// applied to, so removing it here is display-safe.
///
/// Returns a borrowed value when no stripping is needed (non-tracker tools or
/// results without a `view` field) so the common path pays zero allocation.
pub fn strip_tui_display_fields<'a>(tool_name: &str, value: &'a Value) -> Cow<'a, Value> {
    let canonical = tool_intent::canonical_command_session_tool_name(tool_name).unwrap_or(tool_name);
    if canonical != tools::TASK_TRACKER {
        return Cow::Borrowed(value);
    }
    let Some(obj) = value.as_object() else {
        return Cow::Borrowed(value);
    };
    if !obj.contains_key("view") {
        return Cow::Borrowed(value);
    }
    let mut stripped = obj.clone();
    stripped.remove("view");
    Cow::Owned(Value::Object(stripped))
}

/// Hard byte cap on model-visible content. Line truncation alone does not
/// bound output with very long lines (minified bundles, generated data), so
/// the provider-visible preview contract needs a byte budget as well.
const MAX_RESULT_BYTES: usize = 32 * 1024;

fn truncate_utf8_bytes(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_string(), false);
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

fn reduce_read_file_result(result: Value) -> Value {
    const MAX_FILE_LINES: usize = 2000;

    let Some(obj) = result.as_object() else {
        return result;
    };
    let Some(content) = obj.get("content").and_then(Value::as_str) else {
        return result;
    };

    let (line_truncated, content) = match truncate_lines(content, MAX_FILE_LINES) {
        Some((truncated, _)) => (true, truncated),
        None => (false, content.to_string()),
    };
    let (content, byte_truncated) = truncate_utf8_bytes(&content, MAX_RESULT_BYTES);
    let is_truncated = line_truncated || byte_truncated;

    let mut reduced = serde_json::Map::new();
    reduced.insert("success".to_string(), Value::Bool(true));
    reduced.insert(
        "status".to_string(),
        obj.get("status")
            .cloned()
            .unwrap_or_else(|| Value::String("success".to_string())),
    );
    if let Some(message) = obj.get("message") {
        reduced.insert("message".to_string(), message.clone());
    }
    reduced.insert("content".to_string(), Value::String(content));
    if let Some(path) = obj.get("path").or_else(|| obj.get("file")) {
        reduced.insert("path".to_string(), path.clone());
    }
    if let Some(metadata) = obj.get("metadata") {
        reduced.insert("metadata".to_string(), metadata.clone());
    }
    if is_truncated {
        reduced.insert("is_truncated".to_string(), Value::Bool(true));
    }

    Value::Object(reduced)
}

fn reduce_command_result(result: Value) -> Value {
    const MAX_FILE_LINES: usize = 2000;

    let Some(obj) = result.as_object() else {
        return result;
    };
    let stream_key = if obj.get("stdout").and_then(Value::as_str).is_some() {
        "stdout"
    } else {
        "output"
    };
    let Some(stream) = obj.get(stream_key).and_then(Value::as_str) else {
        return result;
    };

    let (line_truncated, lines_count, stream) = match truncate_lines(stream, MAX_FILE_LINES) {
        Some((truncated, lines_count)) => (true, lines_count, truncated),
        None => (false, 0, stream.to_string()),
    };
    let (stream, byte_truncated) = truncate_utf8_bytes(&stream, MAX_RESULT_BYTES);
    if !line_truncated && !byte_truncated {
        return result;
    }

    let mut reduced = obj.clone();
    reduced.insert(stream_key.to_string(), Value::String(stream));
    reduced.insert("is_truncated".to_string(), Value::Bool(true));
    if line_truncated {
        reduced.insert("original_lines".to_string(), Value::Number(serde_json::Number::from(lines_count as u64)));
    }
    reduced.insert("note".to_string(), Value::String("Command output truncated for context economy.".to_string()));
    Value::Object(reduced)
}

pub fn truncate_lines(text: &str, max_lines: usize) -> Option<(String, usize)> {
    if max_lines == 0 {
        return Some((String::new(), text.lines().count()));
    }

    let mut lines = text.lines();
    let mut total = 0usize;
    let mut out = String::new();
    while let Some(line) = lines.next() {
        total += 1;
        if total <= max_lines {
            if total > 1 {
                out.push('\n');
            }
            out.push_str(line);
            continue;
        }
        total += lines.count();
        return Some((out, total));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strip_view_removes_view_from_task_tracker_result() {
        let result = json!({
            "status": "updated",
            "message": "Item 1 status changed: pending → completed",
            "checklist": { "title": "Demo", "total": 2, "completed": 1, "items": [] },
            "view": { "title": "Demo", "lines": [{ "display": "└ [x] step one" }] },
        });
        let stripped = strip_tui_display_fields("task_tracker", &result);
        assert!(stripped.get("view").is_none(), "view should be removed");
        assert!(stripped.get("checklist").is_some(), "checklist must remain for the model");
        assert_eq!(stripped["status"], "updated");
    }

    #[test]
    fn strip_view_borrows_non_tracker_tools_unchanged() {
        let result = json!({ "status": "ok", "output": "hello" });
        let stripped = strip_tui_display_fields("exec_command", &result);
        assert!(matches!(stripped, Cow::Borrowed(_)), "non-tracker tools should borrow without allocation");
        assert_eq!(stripped.as_ref(), &result);
    }

    #[test]
    fn strip_view_borrows_tracker_result_without_view_field() {
        let result = json!({ "status": "empty", "message": "No active checklist." });
        let stripped = strip_tui_display_fields("task_tracker", &result);
        assert!(
            matches!(stripped, Cow::Borrowed(_)),
            "tracker results without `view` should borrow without allocation"
        );
        assert_eq!(stripped.as_ref(), &result);
    }

    #[test]
    fn strip_view_preserves_checklist_items_and_metadata() {
        let result = json!({
            "status": "ok",
            "checklist": {
                "title": "Plan",
                "total": 2,
                "completed": 0,
                "items": [
                    { "index": 1, "description": "Step A", "status": "pending", "files": ["a.rs"], "outcome": null, "verify": ["cargo check"] },
                    { "index": 2, "description": "Step B", "status": "pending" },
                ],
            },
            "view": { "title": "Plan", "lines": [{ "display": "├ [ ] Step A" }] },
        });
        let stripped = strip_tui_display_fields("task_tracker", &result);
        let items = stripped["checklist"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["verify"][0], "cargo check");
        assert_eq!(items[1]["description"], "Step B");
        assert!(stripped.get("view").is_none());
    }

    #[test]
    fn strip_view_handles_non_object_result() {
        let result = json!("not an object");
        let stripped = strip_tui_display_fields("task_tracker", &result);
        assert_eq!(stripped.as_ref(), &result);
    }
}
