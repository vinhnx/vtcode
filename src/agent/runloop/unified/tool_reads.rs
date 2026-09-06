use serde_json::Value;
use vtcode_core::config::constants::tools as tool_names;
use vtcode_core::tools::tool_intent;

pub(crate) fn read_file_path_arg(args: &Value) -> Option<&str> {
    let obj = args.as_object()?;
    for key in ["path", "file_path", "filepath", "target_path", "file"] {
        if let Some(path) = obj.get(key).and_then(Value::as_str) {
            let trimmed = path.trim();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    None
}

pub(crate) fn is_read_file_style_call(canonical_tool_name: &str, args: &Value) -> bool {
    match canonical_tool_name {
        tool_names::READ_FILE => true,
        tool_names::UNIFIED_FILE => tool_intent::file_operation_action(args)
            .unwrap_or("read")
            .eq_ignore_ascii_case("read"),
        _ => false,
    }
}

fn looks_like_tool_output_spool_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let components: Vec<_> = normalized.split('/').filter(|part| !part.is_empty() && *part != ".").collect();
    if components.contains(&"..") {
        return false;
    }
    components
        .windows(4)
        .any(|parts| parts.starts_with(&[".vtcode", "context", "tool_outputs"]))
}

pub(crate) fn spool_chunk_read_path<'a>(canonical_tool_name: &str, args: &'a Value) -> Option<&'a str> {
    if !is_read_file_style_call(canonical_tool_name, args) {
        return None;
    }

    let path = read_file_path_arg(args)?;
    if looks_like_tool_output_spool_path(path) {
        Some(path)
    } else {
        None
    }
}

/// Read the first 4 KB of a file to detect error payloads written by previous
/// tool calls. Used to short-circuit repeated reads of an error spool.
pub(crate) async fn read_spool_head_for_error_check(workspace: &std::path::Path, path: &str) -> Option<String> {
    let workspace = workspace.to_path_buf();
    let path = std::path::PathBuf::from(path);
    tokio::task::spawn_blocking(move || -> std::io::Result<String> {
        use std::io::Read;
        let relative = if path.is_absolute() {
            path.strip_prefix(&workspace)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::PermissionDenied, error))?
        } else {
            path.as_path()
        };
        if !relative.starts_with(".vtcode/context/tool_outputs") {
            return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "not a workspace spool"));
        }
        let file = vtcode_commons::fs::bound_file::open_file_beneath(&workspace, relative)?;
        let mut buffer = Vec::new();
        file.take(4096).read_to_end(&mut buffer)?;
        Ok(String::from_utf8_lossy(&buffer).into_owned())
    })
    .await
    .ok()?
    .ok()
    .filter(|head| !head.is_empty())
}

/// Returns `true` if the spool file's first bytes look like a tool error
/// response rather than a successful tool output. We check both:
///   - JSON `{"error":...}` envelopes produced by `build_error_content`
///   - Plain-text error fragments commonly written by outline / search tools
pub(crate) fn spool_content_looks_like_error(content: &str) -> bool {
    let trimmed = content.trim_start();
    if trimmed.starts_with("{\"error\"") {
        return true;
    }
    if trimmed.starts_with("{\"output\":\"Outline requires ast-grep") {
        return true;
    }
    if trimmed.starts_with("{\"output\":\"Error") {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("error:")
        || lower.starts_with("tool error:")
        || lower.starts_with("ast-grep")
        || lower.contains("outline requires ast-grep")
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use vtcode_core::config::constants::tools;

    use super::{spool_chunk_read_path, spool_content_looks_like_error};

    #[test]
    fn spool_path_requires_components_not_substrings() {
        for path in [
            "fake.vtcode/context/tool_outputs/file",
            ".vtcode/context/tool_outputs/../secret",
            ".vtcode/context/tool_outputs",
        ] {
            assert_eq!(spool_chunk_read_path(tools::READ_FILE, &json!({"path": path})), None);
        }
    }

    #[test]
    fn spool_chunk_read_path_matches_read_file_spool_reads() {
        let args = json!({
            "path": ".vtcode/context/tool_outputs/command_session_123.txt",
            "offset": 41,
            "limit": 40
        });

        assert_eq!(
            spool_chunk_read_path(tools::READ_FILE, &args),
            Some(".vtcode/context/tool_outputs/command_session_123.txt")
        );
    }

    #[test]
    fn spool_chunk_read_path_matches_file_operation_read_spool_reads() {
        let args = json!({
            "action": "read",
            "path": ".vtcode/context/tool_outputs/command_session_456.txt",
            "offset": 81,
            "limit": 40
        });

        assert_eq!(
            spool_chunk_read_path(tools::UNIFIED_FILE, &args),
            Some(".vtcode/context/tool_outputs/command_session_456.txt")
        );
    }

    #[test]
    fn spool_chunk_read_path_ignores_regular_reads() {
        let args = json!({
            "path": "src/main.rs",
            "offset": 1,
            "limit": 100
        });

        assert_eq!(spool_chunk_read_path(tools::READ_FILE, &args), None);
    }

    #[test]
    fn spool_chunk_read_path_ignores_non_read_file_operation_actions() {
        let args = json!({
            "action": "write",
            "path": ".vtcode/context/tool_outputs/command_session_789.txt",
            "content": "replacement"
        });

        assert_eq!(spool_chunk_read_path(tools::UNIFIED_FILE, &args), None);
    }

    #[test]
    fn spool_content_looks_like_error_detects_json_error_envelope() {
        let payload = r#"{"error":"Spool chunk reads exceeded","failure_kind":"spool_chunk_guard"}"#;
        assert!(spool_content_looks_like_error(payload));
    }

    #[test]
    fn spool_content_looks_like_error_detects_outline_ast_grep_failure() {
        let payload = r#"{"output":"Outline requires ast-grep (`sg`). not installed"}"#;
        assert!(spool_content_looks_like_error(payload));
    }

    #[test]
    fn spool_content_looks_like_error_detects_plain_error_prefix() {
        assert!(spool_content_looks_like_error("Error: file not found"));
        assert!(spool_content_looks_like_error("ast-grep: command not found"));
    }

    #[test]
    fn spool_content_looks_like_error_returns_false_for_normal_output() {
        assert!(!spool_content_looks_like_error("approval_recorder.rs\nassembly.rs\nbuilder.rs"));
        assert!(!spool_content_looks_like_error("=== mod.rs ===\npub struct ToolRegistry"));
    }
}
