//! Tool output processing helpers for ToolRegistry.

use serde_json::{Value, json};
use vtcode_commons::sanitizer::redact_secrets;

use super::ToolRegistry;
use super::spool_processing::{
    limit_output_preview, limit_spooled_preview, preview_budget_bytes, should_force_spool,
    should_keep_inline_pty_output,
};
use crate::tools::output_spooler::{command_preview_content, ensure_spooled_reference_metadata};

fn redact_value_strings(value: &mut Value) {
    match value {
        Value::String(text) => {
            let redacted = redact_secrets(std::mem::take(text));
            *text = redacted;
        }
        Value::Array(values) => {
            for value in values {
                redact_value_strings(value);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                redact_value_strings(value);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

impl ToolRegistry {
    fn sanitize_tool_output(value: Value, is_mcp: bool, max_output_tokens: usize) -> Value {
        let (entry_fuse, depth_fuse, token_fuse, byte_fuse) = Self::fuse_limits();

        let trimmed = Self::clamp_value_recursive(&value, entry_fuse, depth_fuse);

        let serialized = trimmed.to_string();
        let approx_tokens = serialized.len() / 4;
        let max_preview_bytes = max_output_tokens
            .saturating_mul(crate::tools::output_limits::OUTPUT_PREVIEW_CHARS_PER_TOKEN)
            .max(1);
        let byte_fuse = byte_fuse.min(max_preview_bytes);
        let token_fuse = token_fuse.min(max_output_tokens);
        if serialized.len() > byte_fuse || approx_tokens > token_fuse {
            let truncated = serialized.chars().take(byte_fuse).collect::<String>();
            return json!({
                "content": truncated,
                "truncated": true,
                "note": if is_mcp {
                    "MCP tool result truncated to protect context budget"
                } else {
                    "Tool result truncated to protect context budget"
                },
                "approx_tokens": approx_tokens,
                "byte_fuse": byte_fuse
            });
        }
        trimmed
    }

    fn clamp_value_recursive(value: &Value, entry_fuse: usize, depth: usize) -> Value {
        if depth == 0 {
            return value.clone();
        }
        match value {
            Value::Array(arr) => {
                if arr.is_empty() {
                    return Value::Array(Vec::new());
                }
                let overflow = arr.len().saturating_sub(entry_fuse);
                let trimmed: Vec<Value> = arr
                    .iter()
                    .take(entry_fuse)
                    .map(|v| Self::clamp_value_recursive(v, entry_fuse, depth - 1))
                    .collect();
                if overflow > 0 {
                    let approx_tokens = trimmed.iter().map(|v| v.to_string().len() / 4).sum::<usize>();
                    json!({
                        "truncated": true,
                        "note": "Array truncated to protect context budget",
                        "total_entries": arr.len(),
                        "entries": trimmed,
                        "overflow": overflow,
                        "approx_tokens": approx_tokens
                    })
                } else {
                    Value::Array(trimmed)
                }
            }
            Value::Object(map) => {
                if map.is_empty() {
                    return Value::Object(serde_json::Map::new());
                }
                let overflow = map.len().saturating_sub(entry_fuse);
                let mut head = serde_json::Map::new();
                for (k, v) in map.iter().take(entry_fuse) {
                    head.insert(k.clone(), Self::clamp_value_recursive(v, entry_fuse, depth - 1));
                }
                if overflow > 0 {
                    let approx_tokens = head.iter().map(|(k, v)| (k.len() + v.to_string().len()) / 4).sum::<usize>();
                    json!({
                        "truncated": true,
                        "note": "Object truncated to protect context budget",
                        "total_entries": map.len(),
                        "entries": head,
                        "overflow": overflow,
                        "approx_tokens": approx_tokens
                    })
                } else {
                    Value::Object(head)
                }
            }
            _ => value.clone(),
        }
    }

    fn fuse_limits() -> (usize, usize, usize, usize) {
        let entry_fuse = std::env::var("VTCODE_FUSE_ENTRY")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v >= 10)
            .unwrap_or(200);
        let depth_fuse = std::env::var("VTCODE_FUSE_DEPTH")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v >= 1)
            .unwrap_or(3);
        let token_fuse = std::env::var("VTCODE_FUSE_TOKEN")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v >= 1_000)
            .unwrap_or(50_000);
        let byte_fuse = std::env::var("VTCODE_FUSE_BYTES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v >= 10_000)
            .unwrap_or(200_000);
        (entry_fuse, depth_fuse, token_fuse, byte_fuse)
    }

    /// Process tool output with dynamic context discovery.
    ///
    /// This method implements Cursor-style dynamic context discovery:
    /// 1. First checks if output should be spooled to a file (large outputs)
    /// 2. If spooled, returns a file reference instead of truncated content
    /// 3. Otherwise, applies standard sanitization
    ///
    /// This is more token-efficient as agents can inspect spooled files with
    /// shell commands or use `code_search` for code structure on demand.
    pub(super) async fn process_tool_output(
        &self,
        tool_name: &str,
        value: Value,
        is_mcp: bool,
        max_output_tokens: usize,
    ) -> Value {
        let processed = self
            .process_tool_output_inner(tool_name, value, is_mcp, max_output_tokens)
            .await;
        self.enforce_turn_preview_budget(processed)
    }

    /// Enforce the aggregate provider-visible preview budget for the turn.
    ///
    /// Per-result preview limiting (above) bounds one response; this bound
    /// covers the whole turn: once `TURN_PREVIEW_BUDGET_BYTES` worth of
    /// payload bodies has been emitted, later responses keep outcome/control
    /// metadata while payload bodies are truncated to the remaining budget
    /// and then omitted, marked with `preview_budget_exhausted`.
    fn enforce_turn_preview_budget(&self, mut value: Value) -> Value {
        const BUDGET_BYTES: usize = vtcode_config::constants::output_limits::TURN_PREVIEW_BUDGET_BYTES;

        let body_bytes = payload_body_bytes(&value);
        if body_bytes == 0 {
            return value;
        }

        let previous = self.charge_turn_preview_bytes(body_bytes);
        if previous >= BUDGET_BYTES {
            strip_payload_bodies(&mut value);
            return value;
        }
        if previous + body_bytes > BUDGET_BYTES {
            truncate_payload_bodies(&mut value, BUDGET_BYTES - previous);
        }
        value
    }

    async fn process_tool_output_inner(
        &self,
        tool_name: &str,
        mut value: Value,
        is_mcp: bool,
        max_output_tokens: usize,
    ) -> Value {
        if value.get("output_spooled").and_then(Value::as_bool) == Some(true) {
            redact_value_strings(&mut value);
            if value.get("no_spool").and_then(Value::as_bool) == Some(true) {
                if let Some(object) = value.as_object_mut() {
                    for field in [
                        "output_spooled",
                        "spool_path",
                        "spool_complete",
                        "spool_pending",
                        "spooled_bytes",
                        "spool_note",
                    ] {
                        object.remove(field);
                    }
                }
                return limit_output_preview(value, max_output_tokens);
            }

            // A producer may have supplied both a spool marker and inline
            // fields. Sanitize those fields before trusting the marker; the
            // marker only describes storage, not the safety of the payload.
            ensure_spooled_reference_metadata(&mut value);
            if crate::tools::tool_intent::canonical_command_session_tool_name(tool_name).is_some()
                && let Some(content) = value
                    .get("preview")
                    .and_then(Value::as_str)
                    .filter(|preview| !preview.is_empty())
                    .map(str::to_owned)
            {
                let preview =
                    command_preview_content(tool_name, &value, &content, Some(preview_budget_bytes(max_output_tokens)));
                if let Some(object) = value.as_object_mut() {
                    object.remove("raw_output");
                    object.remove("stdout");
                    object.remove("output");
                    object.remove("content");
                    object.insert("preview".to_string(), Value::String(preview));
                }
            }
            if let Some(object) = value.as_object_mut() {
                object.remove("output_spooled");
            }
            return limit_spooled_preview(value, max_output_tokens);
        }

        let spooling_enabled = self.output_spooler.config().enabled;
        let force_spool = should_force_spool(tool_name, &value, is_mcp, spooling_enabled, max_output_tokens);

        // Check if output should be spooled to file
        if force_spool || self.output_spooler.should_spool(&value) {
            match self
                .output_spooler
                .process_output_with_preview_limit(
                    tool_name,
                    value.clone(),
                    is_mcp,
                    force_spool,
                    preview_budget_bytes(max_output_tokens),
                )
                .await
            {
                Ok(spooled) => {
                    if spooled.get("spool_path").and_then(Value::as_str).is_some() {
                        return limit_spooled_preview(spooled, max_output_tokens);
                    }
                    // Spooling was skipped (`no_spool` request or spooling
                    // disabled) while force-spool conditions still fired, so
                    // the oversized payload must not bypass preview limiting
                    // and redaction; fall through to standard sanitization.
                }
                Err(e) => {
                    // Log error but fall back to standard sanitization
                    tracing::warn!(
                        tool = tool_name,
                        error = %e,
                        "Failed to spool tool output to file, falling back to truncation"
                    );
                }
            }
        }

        if should_keep_inline_pty_output(tool_name, &value, max_output_tokens) {
            redact_value_strings(&mut value);
            return limit_output_preview(value, max_output_tokens);
        }

        Self::sanitize_tool_output(value, is_mcp, max_output_tokens)
    }
}

/// Provider-visible payload-body fields counted against the per-turn preview
/// budget. Outcome/control metadata (exit codes, spool paths, truncation
/// flags) is never counted and never stripped.
const PAYLOAD_BODY_FIELDS: [&str; 5] = ["output", "preview", "content", "stdout", "stderr"];

fn payload_body_bytes(value: &Value) -> usize {
    PAYLOAD_BODY_FIELDS
        .iter()
        .filter_map(|field| value.get(*field).and_then(Value::as_str))
        .map(str::len)
        .sum()
}

fn strip_payload_bodies(value: &mut Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    for field in PAYLOAD_BODY_FIELDS {
        object.remove(field);
    }
    object.insert("preview_budget_exhausted".to_string(), Value::Bool(true));
}

fn truncate_payload_bodies(value: &mut Value, remaining_bytes: usize) {
    let mut remaining = remaining_bytes;
    let Some(object) = value.as_object_mut() else {
        return;
    };
    for field in PAYLOAD_BODY_FIELDS {
        let Some(text) = object.get(field).and_then(Value::as_str) else {
            continue;
        };
        if remaining == 0 {
            object.remove(field);
            continue;
        }
        if text.len() <= remaining {
            remaining -= text.len();
            continue;
        }
        let mut end = remaining;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        object.insert(field.to_string(), Value::String(text[..end].to_string()));
        remaining = 0;
    }
    object.insert("preview_budget_exhausted".to_string(), Value::Bool(true));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::constants::tools;

    #[test]
    fn force_spool_for_truncated_pty_output() {
        let value = json!({
            "output": "x",
            "truncated": true
        });
        assert!(should_force_spool(
            "run_pty_cmd",
            &value,
            false,
            true,
            vtcode_utility_tool_specs::DEFAULT_MAX_OUTPUT_TOKENS,
        ));
    }

    #[test]
    fn force_spool_for_large_pty_output() {
        let value = json!({
            "output": "x".repeat(vtcode_utility_tool_specs::DEFAULT_MAX_OUTPUT_TOKENS * 4 + 1)
        });
        assert!(should_force_spool(
            "run_pty_cmd",
            &value,
            false,
            true,
            vtcode_utility_tool_specs::DEFAULT_MAX_OUTPUT_TOKENS,
        ));
    }

    #[test]
    fn skip_force_spool_when_disabled_or_non_pty() {
        let no_spool_value = json!({
            "output": "x".repeat(vtcode_utility_tool_specs::DEFAULT_MAX_OUTPUT_TOKENS * 4 + 1),
            "no_spool": true
        });
        assert!(!should_force_spool(
            "run_pty_cmd",
            &no_spool_value,
            false,
            true,
            vtcode_utility_tool_specs::DEFAULT_MAX_OUTPUT_TOKENS,
        ));

        let non_pty_value = json!({
            "output": "x".repeat(1_000)
        });
        assert!(!should_force_spool(
            tools::GREP_FILE,
            &non_pty_value,
            false,
            true,
            vtcode_utility_tool_specs::DEFAULT_MAX_OUTPUT_TOKENS,
        ));
        assert!(!should_force_spool(
            "run_pty_cmd",
            &non_pty_value,
            false,
            false,
            vtcode_utility_tool_specs::DEFAULT_MAX_OUTPUT_TOKENS,
        ));
    }

    #[test]
    fn custom_preview_cap_forces_spooling_for_any_tool() {
        let value = json!({"content": "output exceeds one token"});
        assert!(should_force_spool(tools::CODE_SEARCH, &value, false, true, 1));
        assert!(should_force_spool("mcp::server::tool", &value, true, true, 1));
        assert!(should_force_spool(
            tools::CODE_SEARCH,
            &json!({"content": "output exceeds one token", "no_spool": true}),
            false,
            true,
            1,
        ));
    }

    #[test]
    fn zero_token_cap_still_forces_spooling_for_nonempty_pty_output() {
        assert!(should_force_spool("run_pty_cmd", &json!({"output": "x"}), false, true, 0,));
    }

    #[test]
    fn small_pty_output_does_not_force_spooling_from_response_metadata() {
        let value = json!({
            "output": "delayed-output\\n",
            "session_id": "run-123",
            "command": "sleep 0.5; printf delayed-output",
            "working_directory": ".",
            "next_continue_args": {"session_id": "run-123"},
            "next_wait_args": {"session_id": "run-123", "action": "wait"},
            "next_action_hint": "wait for the command to finish"
        });

        assert!(!should_force_spool("run_pty_cmd", &value, false, true, 16));
    }

    #[test]
    fn metadata_only_command_session_output_respects_context_cap() {
        let value = json!({
            "sessions": [{
                "session_id": "run-123",
                "metadata": "x".repeat(128)
            }]
        });

        assert!(should_force_spool("list_pty_sessions", &value, false, true, 1));
        assert!(!should_keep_inline_pty_output("list_pty_sessions", &value, 1));
    }

    #[test]
    fn large_secondary_output_field_cannot_hide_behind_empty_raw_output() {
        let value = json!({
            "raw_output": "",
            "output": "x".repeat(128)
        });

        assert!(should_force_spool("run_pty_cmd", &value, false, true, 1));
        assert!(!should_keep_inline_pty_output("run_pty_cmd", &value, 1));
    }

    #[tokio::test]
    async fn small_pty_response_preserves_session_metadata_inline() {
        let temp = tempfile::tempdir().unwrap();
        let registry = ToolRegistry::new(temp.path().to_path_buf()).await;
        let value = json!({
            "success": true,
            "output": "delayed-output\\n",
            "session_id": "run-123",
            "command": "sleep 0.5; printf delayed-output",
            "working_directory": ".",
            "next_continue_args": {"session_id": "run-123"},
            "next_wait_args": {"session_id": "run-123", "action": "wait"},
            "next_action_hint": "wait for the command to finish"
        });

        let result = registry.process_tool_output("run_pty_cmd", value, false, 16).await;

        assert_eq!(result["session_id"], "run-123");
        assert_eq!(result["output"], "delayed-output\\n");
        assert!(result.get("content").is_none());
        assert!(result.get("note").is_none());
    }

    #[tokio::test]
    async fn turn_preview_budget_truncates_then_strips_payload_bodies() {
        let temp = tempfile::tempdir().unwrap();
        let registry = ToolRegistry::new(temp.path().to_path_buf()).await;
        // 8 KiB bodies stay under the spooler threshold while five of them
        // exceed the 32 KiB aggregate turn budget.
        let body = "x".repeat(8_000);

        for _ in 0..4 {
            let result = registry
                .process_tool_output("grep_file", json!({ "success": true, "output": body.clone() }), false, 100_000)
                .await;
            assert_eq!(result["output"].as_str().unwrap().len(), body.len());
            assert!(result.get("preview_budget_exhausted").is_none());
        }

        let fifth = registry
            .process_tool_output("grep_file", json!({ "success": true, "output": body.clone() }), false, 100_000)
            .await;
        let fifth_len = fifth["output"].as_str().unwrap().len();
        assert!(
            fifth_len > 0 && fifth_len < body.len(),
            "fifth response should be truncated to the remaining turn budget, got {fifth_len}"
        );
        assert_eq!(fifth["preview_budget_exhausted"], true);

        let sixth = registry
            .process_tool_output("grep_file", json!({ "success": true, "output": body }), false, 100_000)
            .await;
        assert!(sixth.get("output").is_none(), "payload body should be stripped once exhausted");
        assert_eq!(sixth["preview_budget_exhausted"], true);
        assert_eq!(sixth["success"], true, "outcome metadata must survive the strip");
    }

    #[tokio::test]
    async fn turn_preview_budget_resets_between_turns() {
        let temp = tempfile::tempdir().unwrap();
        let registry = ToolRegistry::new(temp.path().to_path_buf()).await;
        let body = "x".repeat(8_000);

        for _ in 0..5 {
            registry
                .process_tool_output("grep_file", json!({ "success": true, "output": body.clone() }), false, 100_000)
                .await;
        }
        registry.begin_turn_preview_window();

        let after_reset = registry
            .process_tool_output("grep_file", json!({ "success": true, "output": body }), false, 100_000)
            .await;
        assert_eq!(
            after_reset["output"].as_str().unwrap().len(),
            body.len(),
            "a fresh turn window must accept the payload again"
        );
    }
    #[tokio::test]
    async fn small_pty_response_redacts_inline_secrets() {
        let temp = tempfile::tempdir().unwrap();
        let registry = ToolRegistry::new(temp.path().to_path_buf()).await;
        let value = json!({
            "output": "password=supersecretvalue",
            "command": "printf password=supersecretvalue"
        });

        let result = registry.process_tool_output("run_pty_cmd", value, false, 16).await;

        assert_eq!(result["output"], "password=[REDACTED_SECRET]");
        assert_eq!(result["command"], "printf password=[REDACTED_SECRET]");
    }

    #[tokio::test]
    async fn configured_threshold_controls_untruncated_pty_output() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("vtcode.toml"),
            "[context.dynamic]\nenabled = true\ntool_output_threshold = 16384\n\n[workspace]\nuse_root_config = true\n",
        )
        .unwrap();
        let registry = ToolRegistry::new(temp.path().to_path_buf()).await;
        let value = json!({
            "output": "x".repeat(16_000)
        });

        let result = registry
            .process_tool_output(
                "run_pty_cmd",
                value.clone(),
                false,
                vtcode_utility_tool_specs::DEFAULT_MAX_OUTPUT_TOKENS,
            )
            .await;

        assert!(result.get("spool_path").is_none());
        assert_eq!(result["output"], value["output"]);
    }

    #[test]
    fn spooled_preview_keeps_the_full_output_reference() {
        let value = json!({
            "content": "output exceeds one token",
            "spool_path": ".vtcode/context/tool_outputs/result.txt"
        });
        let result = limit_spooled_preview(value, 1);
        assert_eq!(result["spool_path"], ".vtcode/context/tool_outputs/result.txt");
        assert_eq!(result["content"], "outp");
        assert_eq!(result["preview_truncated"], true);
    }

    #[tokio::test]
    async fn spooled_marker_does_not_bypass_inline_secret_redaction() {
        let temp = tempfile::tempdir().unwrap();
        let registry = ToolRegistry::new(temp.path().to_path_buf()).await;
        let value = json!({
            "output_spooled": true,
            "spool_path": ".vtcode/context/tool_outputs/result.txt",
            "output": "password=supersecretvalue"
        });

        let result = registry.process_tool_output("run_pty_cmd", value, false, 100).await;

        assert_eq!(result["preview"], "password=[REDACTED_SECRET]");
        assert!(result.get("output").is_none());
        assert_eq!(result["spool_path"], ".vtcode/context/tool_outputs/result.txt");
        assert!(result.get("output_spooled").is_none());
    }

    #[tokio::test]
    async fn no_spool_marker_still_redacts_and_bounds_inline_output() {
        let temp = tempfile::tempdir().unwrap();
        let registry = ToolRegistry::new(temp.path().to_path_buf()).await;
        let value = json!({
            "output_spooled": true,
            "no_spool": true,
            "spool_path": ".vtcode/context/tool_outputs/result.txt",
            "output": "0123456789abcdef",
            "raw_output": "0123456789abcdef",
            "diagnostic": "password=supersecretvalue"
        });

        let result = registry.process_tool_output("run_pty_cmd", value, false, 2).await;

        assert_eq!(result["diagnostic"], "password=[REDACTED_SECRET]");
        assert!(result["output"].as_str().unwrap_or_default().len() <= 8);
        assert!(result.get("spool_path").is_none());
        assert!(result.get("output_spooled").is_none());
    }

    #[tokio::test]
    async fn spooled_marker_retains_bounded_preview_and_recovery_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let registry = ToolRegistry::new(temp.path().to_path_buf()).await;
        let value = json!({
            "output_spooled": true,
            "spool_path": ".vtcode/context/tool_outputs/result.txt",
            "total_output_bytes": 12_345,
            "output": "0123456789abcdef"
        });

        let result = registry.process_tool_output("run_pty_cmd", value, false, 2).await;

        assert!(result["preview"].as_str().unwrap_or_default().len() <= 8);
        assert!(result.get("output").is_none());
        assert_eq!(result["spooled_bytes"], 12_345);
        assert!(result["spool_note"].as_str().unwrap_or_default().contains("result.txt"));
        assert!(result.get("output_spooled").is_none());
    }

    #[tokio::test]
    async fn synthetic_replay_of_producer_spooled_inspection_keeps_only_six_kib_preview() {
        let temp = tempfile::tempdir().unwrap();
        let registry = ToolRegistry::new(temp.path().to_path_buf()).await;
        let output = format!("REPLAY_HEAD\n{}\nREPLAY_TAIL", "x".repeat(36 * 1024));
        let value = json!({
            "output_spooled": true,
            "spool_path": ".vtcode/context/tool_outputs/replay-36k.txt",
            "spool_complete": true,
            "total_output_bytes": output.len(),
            "output": output,
            "content": format!("REPLAY_CONTENT_LEAK\\n{}", "y".repeat(36 * 1024)),
            "command": "sed -n '1,80p' .vtcode/context/tool_outputs/replay-36k.txt",
            "exit_code": 1,
            "failure_diagnostic": "command completed with status 1"
        });

        let result = registry.process_tool_output("exec_command", value, false, 16_384).await;
        let serialized_model_payload = serde_json::to_string(&result).unwrap();
        let preview = result["preview"].as_str().unwrap();

        assert!(preview.len() <= 6 * 1024);
        assert!(result.get("output").is_none());
        assert!(result.get("content").is_none());
        assert!(preview.contains("REPLAY_HEAD"));
        assert!(preview.contains("REPLAY_TAIL"));
        assert!(!serialized_model_payload.contains("REPLAY_CONTENT_LEAK"));
        assert!(serialized_model_payload.len() < 8 * 1024);
        assert_eq!(result["spool_path"], ".vtcode/context/tool_outputs/replay-36k.txt");
        assert_eq!(result["spooled_bytes"], 36_888);
        assert_eq!(result["spool_complete"], true);
        assert_eq!(result["failure_diagnostic"], "command completed with status 1");
        assert!(result["spool_note"].as_str().unwrap().contains("replay-36k.txt"));
    }

    #[tokio::test]
    async fn process_tool_output_skips_force_spool_when_dynamic_context_is_disabled() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("vtcode.toml"),
            "[context.dynamic]\nenabled = false\n\n[workspace]\nuse_root_config = true\n",
        )
        .unwrap();
        let registry = ToolRegistry::new(temp.path().to_path_buf()).await;
        let value = json!({
            "output": "x".repeat(vtcode_utility_tool_specs::DEFAULT_MAX_OUTPUT_TOKENS * 2 + 1),
            "truncated": true
        });

        let result = registry
            .process_tool_output(
                "run_pty_cmd",
                value.clone(),
                false,
                vtcode_utility_tool_specs::DEFAULT_MAX_OUTPUT_TOKENS,
            )
            .await;

        assert!(result.get("spool_path").is_none());
        assert_eq!(result.get("output"), value.get("output"));
    }

    #[tokio::test]
    async fn large_outputs_from_any_tool_spool_to_path_with_guidance() {
        let temp = tempfile::tempdir().unwrap();
        let registry = ToolRegistry::new(temp.path().to_path_buf()).await;

        // Representative tool types: search, file read, web fetch, exec, and an MCP tool.
        // Every one must land on disk as a `spool_path` the model can post-process
        // (unified_file, code_search, or unified_exec) instead of being inlined.
        let tool_names = [
            tools::CODE_SEARCH,
            tools::UNIFIED_FILE,
            tools::WEB_FETCH,
            tools::UNIFIED_EXEC,
            "mcp_get_tool_details",
        ];
        let big = "a".repeat(20_000);
        for tool in tool_names {
            // PTY/exec tools emit `output`; the rest emit `content`.
            let field = if tool == tools::UNIFIED_EXEC {
                "output"
            } else {
                "content"
            };
            let mut value = json!({});
            value[field] = json!(big);
            let result = registry
                .process_tool_output(
                    tool,
                    value.clone(),
                    tool.starts_with("mcp"),
                    vtcode_utility_tool_specs::DEFAULT_MAX_OUTPUT_TOKENS,
                )
                .await;

            assert!(result.get("spool_path").is_some(), "tool {tool} should spool large output to a file path");
            assert!(
                result.get("spool_note").is_some(),
                "tool {tool} should explain how to post-process the spooled file"
            );
            let inline = result
                .get("content")
                .or_else(|| result.get("output"))
                .and_then(Value::as_str)
                .unwrap_or("");
            assert!(inline.len() < big.len(), "tool {tool} should condense the preview, not inline the full blob");
        }
    }
}
