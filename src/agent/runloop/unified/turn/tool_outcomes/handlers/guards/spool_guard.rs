//! Guard for spool chunk reads.
//!
//! Tracks sequential reads of spool files (tool output files). When the cap is
//! reached, recovery is activated to prevent the model from paginating through
//! large spool files indefinitely.
//!
//! The guard also short-circuits when a spool file contains an error payload
//! from a previous turn, injecting the error inline instead of reading.

use std::path::Path;

use serde_json::{Value, json};
use vtcode_core::config::constants::defaults::DEFAULT_MAX_SEQUENTIAL_SPOOL_CHUNK_READS_PER_TURN;
use vtcode_core::config::constants::tools as tool_names;
use vtcode_core::tools::registry::labels::tool_action_label;

use super::super::ValidationResult;
use super::super::looping::spool_chunk_read_path;
use super::common::push_guard_failure_messages;
use crate::agent::runloop::unified::tool_reads::{read_spool_head_for_error_check, spool_content_looks_like_error};
use crate::agent::runloop::unified::turn::context::TurnProcessingContext;

const SPOOL_CHUNK_GREP_PATTERN: &str = "warning|error|TODO";
const SPOOL_CHUNK_INLINE_MAX_BYTES: usize = 32 * 1024;
const SPOOL_CHUNK_INLINE_HEAD_BYTES: usize = 8 * 1024;
const SPOOL_CHUNK_INLINE_TAIL_BYTES: usize = 8 * 1024;

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn public_spool_grep_args(path: &str, pattern: &str) -> Value {
    json!({
        "cmd": format!(
            "rg --line-number --column --color=never {} {}",
            shell_single_quote(pattern),
            shell_single_quote(path)
        ),
        "max_output_tokens": 4000
    })
}

/// Read a spool file's content for inline embedding when the spool-chunk
/// guard trips. Returns `None` if the file is missing, empty, or unreadable.
///
/// The preview is opened through the workspace-bound no-follow helper just
/// like ordinary spool reads. This keeps a concurrent symlink retarget from
/// exposing an unrelated file while the guard is constructing its response.
async fn read_spool_preview_for_guard(workspace: &Path, path: &str) -> Option<String> {
    let workspace = workspace.to_path_buf();
    let path = Path::new(path).to_path_buf();
    tokio::task::spawn_blocking(move || -> std::io::Result<Option<String>> {
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

        let mut file = vtcode_commons::fs::bound_file::open_file_beneath(&workspace, relative)?;
        let len = file.metadata()?.len() as usize;
        if len == 0 {
            return Ok(None);
        }

        let total_cap = SPOOL_CHUNK_INLINE_MAX_BYTES.min(len);
        let mut buffer = vec![0u8; total_cap];
        let bytes_read = file.read(&mut buffer)?;
        buffer.truncate(bytes_read);
        let content = String::from_utf8_lossy(&buffer).into_owned();
        if content.len() <= SPOOL_CHUNK_INLINE_HEAD_BYTES + SPOOL_CHUNK_INLINE_TAIL_BYTES {
            return Ok(Some(content));
        }
        let head: String = content.chars().take(SPOOL_CHUNK_INLINE_HEAD_BYTES).collect();
        let tail: String = content
            .chars()
            .rev()
            .take(SPOOL_CHUNK_INLINE_TAIL_BYTES)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        Ok(Some(format!("{head}\n\n... [spool content truncated; full file is {len} bytes] ...\n\n{tail}")))
    })
    .await
    .ok()?
    .ok()
    .flatten()
}

/// Inspect the spool filename and try to derive a useful fallback tool call
/// that resumes the original workflow rather than the generic
/// `grep warning|error|TODO` placeholder.
fn derive_spool_fallback(path: &str) -> Option<(String, Value)> {
    let file_name = Path::new(path).file_name()?.to_str()?.to_string();

    if let Some(sid) = file_name.strip_suffix(".txt").and_then(|stem| stem.strip_prefix("run-")) {
        return Some((
            tool_names::WRITE_STDIN.to_string(),
            json!({
                "session_id": sid,
                "chars": "",
                "yield_time_ms": 1000,
            }),
        ));
    }

    let stem = file_name.strip_suffix(".txt")?;
    let prefix = stem.split('_').next()?;
    match prefix {
        "unified" | "outline" | "search" => {
            Some((tool_names::EXEC_COMMAND.to_string(), public_spool_grep_args(path, ".")))
        }
        _ => None,
    }
}

/// Get the max sequential spool chunk reads per turn from config.
fn max_sequential_spool_chunk_reads_per_turn(ctx: &TurnProcessingContext<'_>) -> usize {
    ctx.vt_cfg
        .map(|cfg| cfg.tools.max_sequential_spool_chunk_reads)
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MAX_SEQUENTIAL_SPOOL_CHUNK_READS_PER_TURN)
}

/// Build fallback args for spool chunk guard.
fn spool_chunk_guard_fallback_args(path: &str) -> Value {
    derive_spool_fallback(path)
        .map(|(_, args)| args)
        .unwrap_or_else(|| public_spool_grep_args(path, SPOOL_CHUNK_GREP_PATTERN))
}

/// Build fallback tool for spool chunk guard.
fn spool_chunk_guard_fallback_tool(path: &str) -> Option<String> {
    if let Some((tool, _)) = derive_spool_fallback(path) {
        Some(tool)
    } else {
        Some(tool_names::EXEC_COMMAND.to_string())
    }
}

/// Build the error content for a spool chunk guard trip.
#[cold]
async fn build_spool_chunk_guard_error_content(workspace: &Path, path: &str, max_reads_per_turn: usize) -> String {
    let inline_content = read_spool_preview_for_guard(workspace, path).await;
    let fallback_tool = spool_chunk_guard_fallback_tool(path);
    let fallback_args = spool_chunk_guard_fallback_args(path);

    let mut payload = super::super::super::execution_result::build_error_content(
        format!(
            "Spool chunk reads exceeded per-turn cap ({max_reads_per_turn}). Use targeted extraction before reading more from '{path}'."
        ),
        fallback_tool.clone(),
        Some(fallback_args),
        "spool_chunk_guard",
    );

    if let Some(obj) = payload.as_object_mut() {
        obj.insert("loop_detected".to_string(), Value::Bool(true));
        obj.insert("recovery_required".to_string(), Value::Bool(true));
        obj.insert(
            "next_action".to_string(),
            Value::String(
                "STOP requesting this spool. Use the `inline_content` below \
                 and the existing conversation history to synthesise your final \
                 answer. If additional inspection is still required, use \
                 `exec_command` for targeted shell inspection, `write_stdin` \
                 for session continuation, or `apply_patch` for edits."
                    .to_string(),
            ),
        );
        if let Some(content) = inline_content {
            obj.insert("inline_content".to_string(), Value::String(content));
            obj.insert(
                "inline_content_note".to_string(),
                Value::String(
                    "Full spool content embedded inline. Do NOT re-read this \
                     spool file — the per-turn cap will continue to block you."
                        .to_string(),
                ),
            );
        }
        if let Some(tool) = fallback_tool {
            obj.insert("fallback_tool".to_string(), Value::String(tool));
        }
    }

    payload.to_string()
}

/// Build the tool response payload for a short-circuited spool read of an
/// error payload from a previous turn.
fn build_previous_turn_error_spool_content(spool_path: &str, head: &str) -> String {
    let preview = if head.len() > 1024 {
        // Use char-aware truncation to avoid panicking on multi-byte UTF-8.
        let truncated: String = head.chars().take(1024).collect();
        format!("{truncated}... [truncated]")
    } else {
        head.to_string()
    };
    serde_json::json!({
        "spool_path": spool_path,
        "loop_detected": true,
        "is_recoverable": true,
        "error_class": "previous_turn_error_spool",
        "next_action": "STOP re-reading this spool file. The original error payload is already in your conversation history from the previous turn. Choose a different tool or approach.",
        "hint": "This spool contains an error response from an earlier turn; you do not need to re-read it. Use the error message already in your history and try a different approach.",
        "inline_content": preview,
    })
    .to_string()
}

/// Enforce the spool chunk read guard.
///
/// Returns `Some(ValidationResult)` when the guard trips,
/// or `None` when the guard passes.
pub(crate) async fn enforce_spool_chunk_read_guard(
    ctx: &mut TurnProcessingContext<'_>,
    tool_call_id: &str,
    canonical_tool_name: &str,
    args: &Value,
) -> Option<ValidationResult> {
    let Some(spool_path) = spool_chunk_read_path(canonical_tool_name, args) else {
        ctx.harness_state.reset_spool_chunk_read_streak();
        return None;
    };

    // Short-circuit: if the spool file's first bytes look like an error
    // payload, the agent already has the error in its conversation history
    // from the previous turn. Inject the error inline as a tool response and
    // tell the model to use it instead of paginating the spool.
    if let Some(head) = read_spool_head_for_error_check(&ctx.config.workspace, spool_path).await {
        if spool_content_looks_like_error(&head) {
            ctx.push_rejected_tool_response(
                tool_call_id,
                Some(canonical_tool_name),
                Some(args),
                build_previous_turn_error_spool_content(spool_path, &head),
            );
            ctx.push_system_message(format!(
                "Spool file '{spool_path}' contains a tool error from an earlier turn. Use the error payload already in your conversation history instead of re-reading the spool."
            ));
            // Do not increment the streak for this short-circuit — the model
            // is being told to stop reading the spool, not to try again.
            ctx.harness_state.reset_spool_chunk_read_streak();
            return Some(ValidationResult::Handled);
        }
    }

    let max_reads_per_turn = max_sequential_spool_chunk_reads_per_turn(ctx);
    let streak = ctx.harness_state.record_spool_chunk_read();
    if streak <= max_reads_per_turn {
        return None;
    }

    // Once the cap trips, do not increment the streak again for this path so
    // subsequent attempts don't double-count and the recovery pass can
    // synthesize a final answer without re-entering this guard.
    ctx.harness_state.reset_spool_chunk_read_streak();

    let display_tool = tool_action_label(canonical_tool_name, args);
    let block_reason = format!(
        "Spool chunk guard stopped repeated '{display_tool}' calls for this turn. Scheduling a final recovery pass without more tools."
    );

    ctx.activate_recovery(block_reason.clone());
    push_guard_failure_messages(
        ctx,
        tool_call_id,
        canonical_tool_name,
        build_spool_chunk_guard_error_content(&ctx.config.workspace, spool_path, max_reads_per_turn).await,
        &block_reason,
    );

    Some(ValidationResult::Blocked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spool_chunk_guard_error_resolves_to_pty_poll_for_run_prefix() {
        let payload =
            build_spool_chunk_guard_error_content(Path::new("/"), ".vtcode/context/tool_outputs/run-1.txt", 3).await;
        let parsed: Value = serde_json::from_str(&payload).expect("spool chunk guard payload should be json");

        assert_eq!(parsed.get("fallback_tool").and_then(Value::as_str), Some(tool_names::WRITE_STDIN));
        assert_eq!(parsed["fallback_tool_args"]["session_id"], "1");
        assert_eq!(parsed["fallback_tool_args"]["chars"], "");
        assert!(parsed.get("next_action").and_then(Value::as_str).is_some());
        assert_eq!(parsed.get("loop_detected").and_then(Value::as_bool), Some(true));
        assert_eq!(parsed.get("recovery_required").and_then(Value::as_bool), Some(true));
    }

    #[tokio::test]
    async fn spool_chunk_guard_error_resolves_to_search_grep_for_search_prefix() {
        let payload = build_spool_chunk_guard_error_content(
            Path::new("/"),
            ".vtcode/context/tool_outputs/unified_search_1782625284532136.txt",
            3,
        )
        .await;
        let parsed: Value = serde_json::from_str(&payload).expect("spool chunk guard payload should be json");

        assert_eq!(parsed.get("fallback_tool").and_then(Value::as_str), Some(tool_names::EXEC_COMMAND));
        assert!(
            parsed["fallback_tool_args"]["cmd"]
                .as_str()
                .is_some_and(|cmd| cmd.contains("unified_search_1782625284532136.txt"))
        );
        assert_eq!(parsed.get("loop_detected").and_then(Value::as_bool), Some(true));
        assert_eq!(parsed.get("recovery_required").and_then(Value::as_bool), Some(true));
    }

    #[tokio::test]
    async fn spool_chunk_guard_error_falls_back_to_warning_error_todo_for_unknown_prefix() {
        let payload =
            build_spool_chunk_guard_error_content(Path::new("/"), ".vtcode/context/tool_outputs/custom_tool_42.txt", 3)
                .await;
        let parsed: Value = serde_json::from_str(&payload).expect("spool chunk guard payload should be json");

        assert_eq!(parsed.get("fallback_tool").and_then(Value::as_str), Some(tool_names::EXEC_COMMAND));
        assert!(
            parsed["fallback_tool_args"]["cmd"]
                .as_str()
                .is_some_and(|cmd| cmd.contains(SPOOL_CHUNK_GREP_PATTERN))
        );
    }

    #[test]
    fn derive_spool_fallback_recognizes_pty_session_id() {
        let (tool, args) = derive_spool_fallback(".vtcode/context/tool_outputs/run-abc123.txt")
            .expect("pty session spool should resolve to a fallback");
        assert_eq!(tool, tool_names::WRITE_STDIN);
        assert_eq!(args["session_id"], "abc123");
        assert_eq!(args["chars"], "");
    }
}
