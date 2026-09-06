//! Session book-keeping and shared helpers for the exec tool family.
//!
//! Keeps the session/response-context glue, path/security checks, and PTY
//! policy. Command construction lives in [`super::exec_command`], response
//! shaping lives in [`super::exec_output`]; both are re-exported here so the
//! historical `exec_support::*` import surface stays stable for callers.

use crate::tools::continuation::PtyContinuationArgs;
use crate::tools::types::VTCodeExecSession;
use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

pub(super) use super::exec_command::*;
pub(super) use super::exec_output::*;

pub(super) const DEFAULT_INSPECT_HEAD_LINES: usize = 30;
pub(super) const DEFAULT_INSPECT_TAIL_LINES: usize = 30;
const MIN_EXEC_YIELD_MS: u64 = 250;
const MAX_EXEC_YIELD_MS: u64 = 30_000;
const EXEC_CAPTURE_WINDOW_BYTES: usize = 64 * 1024;
/// Byte cap for `inspect` reads of agent-supplied `spool_path` files. The file
/// is streamed up to this limit so a multi-GB spool file cannot OOM the
/// process; `read_bounded_spool_file` reports whether the cap was hit.
pub(super) const EXEC_INSPECT_SPOOL_MAX_BYTES: usize = 8 * 1024 * 1024;
/// `wait_timeout_seconds` pre-filled into `next_wait_args` for still-running
/// commands. Generous enough that a typical build completes in one wait call,
/// while a deadline-expired in-progress session can simply be waited on again.
/// The hard ceiling is `long_running_command_ceiling_seconds` (default 3600);
/// this hint stays well below it so a single wait never blocks excessively.
const DEFAULT_LONG_COMMAND_WAIT_HINT_SECONDS: u64 = 600;

// Conservative PTY command policy inspired by bash allow/deny defaults.
const PTY_DENY_PREFIXES: &[&str] = &[
    "bash -i",
    "sh -i",
    "zsh -i",
    "fish -i",
    "python -i",
    "python3 -i",
    "ipython",
    "nano",
    "vim",
    "vi",
    "emacs",
    "top",
    "htop",
    "less",
    "more",
    "screen",
    "tmux",
];

const PTY_DENY_STANDALONE: &[&str] = &["python", "python3", "bash", "sh", "zsh", "fish"];

pub(super) struct ExecOutputPreview {
    pub(super) raw_output: String,
    pub(super) output: String,
    pub(super) truncated: bool,
}

pub(super) struct ExecRunOutputConfig {
    pub(super) max_tokens: usize,
    pub(super) inspect_query: Option<String>,
    pub(super) inspect_literal: bool,
    pub(super) inspect_max_matches: usize,
}

pub(super) struct PtyEphemeralCapture {
    pub(super) output: String,
    pub(super) exit_code: Option<i32>,
    pub(super) duration: Duration,
}

pub(super) fn append_bounded_capture(target: &mut String, chunk: &str) {
    target.push_str(chunk);
    if target.len() <= EXEC_CAPTURE_WINDOW_BYTES {
        return;
    }

    let half = EXEC_CAPTURE_WINDOW_BYTES / 2;
    let head_end = floor_exec_char_boundary(target, half);
    let tail_start = target.len().saturating_sub(half);
    let tail_start = target.ceil_char_boundary(tail_start);
    let head = target[..head_end].to_owned();
    let tail = target[tail_start..].to_owned();
    *target = format!("{head}\n[output preview truncated]\n{tail}");
}

pub(super) fn summarized_arg_keys(args: &Value) -> String {
    match args.as_object() {
        Some(map) => {
            if map.is_empty() {
                return "<none>".to_string();
            }
            let mut keys: Vec<&str> = map.keys().map(|k| k.as_str()).collect();
            keys.sort_unstable();
            let mut preview = keys.into_iter().take(10).collect::<Vec<_>>().join(", ");
            if map.len() > 10 {
                preview.push_str(", ...");
            }
            preview
        }
        None => match args {
            Value::Null => "<null>".to_string(),
            Value::Array(_) => "<array>".to_string(),
            Value::String(_) => "<string>".to_string(),
            Value::Bool(_) => "<bool>".to_string(),
            Value::Number(_) => "<number>".to_string(),
            Value::Object(_) => "<object>".to_string(),
        },
    }
}

pub(super) fn serialized_payload_size_bytes(args: &Value) -> usize {
    serde_json::to_vec(args)
        .map(|bytes| bytes.len())
        .unwrap_or_else(|_| args.to_string().len())
}

#[cold]
pub(super) fn missing_command_session_action_error(args: &Value) -> anyhow::Error {
    anyhow!(
        "Missing command session action. Use `action` or fields: \
         `command|cmd|raw_command` (run), `session_id`+`input|chars|text` (write), \
         `session_id` (poll), `action:\"continue\"` with `session_id` and optional `input|chars|text`, \
         `spool_path|query|head_lines|tail_lines|max_matches|literal` (inspect), \
         or `action:\"list\"|\"close\"`. Keys: {}",
        summarized_arg_keys(args)
    )
}

fn is_valid_pty_session_id(session_id: &str) -> bool {
    !session_id.trim().is_empty()
        && session_id.len() <= 128
        && session_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub(super) fn validate_exec_session_id<'a>(raw_session_id: &'a str, context: &str) -> Result<&'a str> {
    let session_id = raw_session_id.trim();
    if is_valid_pty_session_id(session_id) {
        Ok(session_id)
    } else {
        Err(anyhow!(
            "Invalid session_id for {context}: '{raw_session_id}'. Expected an ASCII token (letters, digits, '-', '_')."
        ))
    }
}

fn build_session_command_display_parts(command: &str, args: &[String]) -> String {
    if let Some(flag_index) = args.iter().position(|arg| matches!(arg.as_str(), "-c" | "/C" | "-Command"))
        && let Some(command) = args.get(flag_index + 1)
        && !command.trim().is_empty()
    {
        return command.clone();
    }

    let mut parts = Vec::with_capacity(1 + args.len());
    if !command.trim().is_empty() {
        parts.push(command);
    }
    for arg in args {
        if !arg.trim().is_empty() {
            parts.push(arg.as_str());
        }
    }

    if parts.is_empty() {
        "unknown".to_string()
    } else {
        shell_words::join(parts)
    }
}

pub(super) fn build_exec_session_command_display(session: &VTCodeExecSession) -> String {
    build_session_command_display_parts(&session.command, &session.args)
}

pub(super) fn is_pty_exec_session(session: &VTCodeExecSession) -> bool {
    session.backend == "pty"
}

pub(super) fn attach_exec_response_context(
    response: &mut Value,
    session: &VTCodeExecSession,
    command: &str,
    is_exited: bool,
) {
    response["session_id"] = json!(session.id.as_str());
    response["command"] = json!(command);
    if let Some(value) = session.working_dir.as_deref() {
        response["working_directory"] = json!(value);
    }
    response["backend"] = json!(session.backend);
    if let Some(rows) = session.rows {
        response["rows"] = json!(rows);
    }
    if let Some(cols) = session.cols {
        response["cols"] = json!(cols);
    }
    response["is_exited"] = json!(is_exited);
}

#[cfg(test)]
pub(super) fn extract_run_session_id_from_tool_output_path(path: &str) -> Option<String> {
    let file_name = Path::new(path).file_name()?.to_str()?;
    let session_id = file_name.strip_suffix(".txt")?;
    if session_id.starts_with("run-") && is_valid_pty_session_id(session_id) {
        Some(session_id.to_string())
    } else {
        None
    }
}

#[cfg(test)]
pub(super) fn extract_run_session_id_from_read_file_error(error_message: &str) -> Option<String> {
    let marker = "session_id=\"";
    let start = error_message.find(marker)? + marker.len();
    let rest = &error_message[start..];
    let end = rest.find('"')?;
    let session_id = &rest[..end];
    if session_id.starts_with("run-") && is_valid_pty_session_id(session_id) {
        Some(session_id.to_string())
    } else {
        None
    }
}

pub(super) fn attach_pty_continuation(response: &mut Value, session_id: &str) {
    response["next_continue_args"] = PtyContinuationArgs::new(session_id).to_value();
}

/// Attach no-burn wait steering to a still-running command response.
///
/// When a run yields with no exit code, the default `next_continue_args` nudges
/// the model toward short polls — each poll costs a full model round-trip that
/// merely re-asserts "still running" (the codex $20/h-on-a-long-build failure
/// mode). `write_stdin` with `action: "wait"` blocks in the harness until exit
/// or the deadline with *no* model round-trips while waiting, so we surface a
/// ready-to-use `next_wait_args` and a hint that ranks it ahead of polling.
pub(super) fn attach_long_command_wait_steering(response: &mut Value, session_id: &str, elapsed: Duration) {
    response["next_wait_args"] = json!({
        "session_id": session_id,
        "action": "wait",
        "wait_timeout_seconds": DEFAULT_LONG_COMMAND_WAIT_HINT_SECONDS,
    });
    let elapsed_secs = elapsed.as_secs();
    let hint = format!(
        "Command still running after {elapsed_secs}s. To avoid burning tokens on short polls, \
         call `write_stdin` with `next_wait_args` (action:\"wait\") — it blocks until the command \
         exits or the deadline elapses with no model round-trips while waiting. If the deadline \
         returns an in-progress session, call `wait` again. Use `next_continue_args` only when \
         you need to peek at incremental output mid-run."
    );
    response["next_action_hint"] = json!(hint);
}

pub(super) fn clamp_exec_yield_ms(value: Option<u64>, default: u64) -> u64 {
    value.unwrap_or(default).clamp(MIN_EXEC_YIELD_MS, MAX_EXEC_YIELD_MS)
}

pub(super) fn clamp_peek_yield_ms(value: Option<u64>) -> u64 {
    value.unwrap_or(0).min(MAX_EXEC_YIELD_MS)
}

pub(super) fn max_output_tokens_from_payload(payload: &serde_json::Map<String, Value>) -> Option<usize> {
    payload
        .get("max_output_tokens")
        .or_else(|| payload.get("max_tokens"))
        .and_then(Value::as_u64)
        .map(|value| value as usize)
}

pub(super) fn floor_exec_char_boundary(text: &str, index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }

    let mut boundary = index;
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    boundary
}

/// Resolve a candidate path inside the workspace, rejecting paths that escape
/// via `..` traversal or a symlink pointing outside the workspace root.
///
/// Delegates to `vtcode_commons::paths::ensure_path_within_workspace_resolved`
/// so a symlink inside the workspace that points outside cannot smuggle an
/// out-of-workspace file past the `inspect`/spool read path.
pub(super) async fn resolve_workspace_scoped_path_resolved(workspace_root: &Path, raw_path: &str) -> Result<PathBuf> {
    let path = Path::new(raw_path.trim());
    if path.as_os_str().is_empty() {
        return Err(anyhow!("spool_path cannot be empty"));
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    };
    vtcode_commons::paths::ensure_path_within_workspace_resolved(&absolute, workspace_root)
        .await
        .map_err(|err| anyhow!("spool_path must stay within workspace: {raw_path}: {err}"))
}

/// Read a spool file up to a bounded byte limit, streaming the first `cap`
/// bytes only. Prevents an agent-supplied `spool_path` (which may point at a
/// multi-GB generated/minified file) from materializing entirely in memory.
/// Returns `(content, truncated)`.
pub(super) async fn read_bounded_spool_file(workspace_root: &Path, path: &Path, cap: usize) -> Result<(String, bool)> {
    use tokio::io::AsyncReadExt;

    let relative = path
        .strip_prefix(workspace_root)
        .context("spool path must remain beneath workspace")?
        .to_path_buf();
    anyhow::ensure!(
        relative.starts_with(".vtcode/context/tool_outputs"),
        "inspect requires a workspace tool-output spool"
    );
    let root = workspace_root.to_path_buf();
    let file = tokio::task::spawn_blocking(move || vtcode_commons::fs::bound_file::open_file_beneath(&root, &relative))
        .await
        .context("spool open task failed")?
        .with_context(|| format!("failed to securely open spool: {}", path.display()))?;

    let mut bytes = Vec::with_capacity(cap.min(64 * 1024));
    tokio::fs::File::from_std(file)
        .take((cap as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .await
        .with_context(|| format!("Failed to read spool path: {}", path.display()))?;

    let truncated = bytes.len() > cap;
    bytes.truncate(cap);
    Ok((String::from_utf8_lossy(&bytes).into_owned(), truncated))
}

fn path_is_tool_accessible_from_workspace(workspace_root: &Path, raw_path: &str) -> bool {
    let path = Path::new(raw_path.trim());
    if path.as_os_str().is_empty() {
        return false;
    }

    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    };
    let normalized = crate::utils::path::normalize_path(&absolute);
    let normalized_workspace = crate::utils::path::normalize_path(workspace_root);
    normalized.starts_with(&normalized_workspace)
}

pub(super) fn sanitize_subagent_tool_output_paths(workspace_root: &Path, value: &mut Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };

    if let Some(raw_path) = object.get("transcript_path").and_then(Value::as_str)
        && !path_is_tool_accessible_from_workspace(workspace_root, raw_path)
    {
        object.remove("transcript_path");
    }

    if let Some(entry) = object.get_mut("entry") {
        sanitize_subagent_tool_output_paths(workspace_root, entry);
    }
}

/// Re-check submitted session input against the PTY deny policy.
///
/// `enforce_pty_command_policy` only guards session *creation*; without this
/// re-check the deny list is defeated by typing the denied program into an
/// already-running session (e.g. starting `echo`, then sending `bash\n`).
pub(super) fn enforce_exec_input_line_policy(input: &str) -> Result<()> {
    for line in input.lines() {
        let trimmed = line.trim().to_ascii_lowercase();
        if trimmed.is_empty() {
            continue;
        }
        let is_standalone = trimmed.split_whitespace().count() == 1;
        let deny_match = PTY_DENY_PREFIXES.iter().any(|prefix| trimmed.starts_with(prefix));
        let standalone_denied = is_standalone && PTY_DENY_STANDALONE.contains(&trimmed.as_str());
        if deny_match || standalone_denied {
            return Err(anyhow!(
                "Session input '{line}' would launch a program blocked by the PTY safety policy. Start a fresh session through exec_command with operator approval instead."
            ));
        }
    }
    Ok(())
}

pub(super) fn enforce_pty_command_policy(display_command: &str, confirm: bool) -> Result<()> {
    let lower = display_command.to_ascii_lowercase();
    let trimmed = lower.trim();
    let is_standalone = trimmed.split_whitespace().count() == 1;

    let deny_match = PTY_DENY_PREFIXES.iter().any(|prefix| trimmed.starts_with(prefix));
    let standalone_denied = is_standalone && PTY_DENY_STANDALONE.contains(&trimmed);

    if deny_match || standalone_denied {
        if confirm {
            return Ok(());
        }
        return Err(anyhow!(
            "Command '{display_command}' is blocked by PTY safety policy. Do not self-approve: surface it to the operator, and only retry with confirm=true after the operator explicitly approves this exact command."
        ));
    }

    Ok(())
}

#[cfg(test)]
mod input_policy_tests {
    use super::enforce_exec_input_line_policy;

    #[test]
    fn blocks_interactive_launchers_submitted_into_sessions() {
        assert!(enforce_exec_input_line_policy("bash\n").is_err());
        assert!(enforce_exec_input_line_policy("  zsh  \n").is_err());
        assert!(enforce_exec_input_line_policy("echo ok\nbash\n").is_err());
        assert!(enforce_exec_input_line_policy("vim\n").is_err());
    }

    #[test]
    fn allows_plain_session_input() {
        assert!(enforce_exec_input_line_policy("echo ok\n").is_ok());
        assert!(enforce_exec_input_line_policy("\n\n").is_ok());
        assert!(enforce_exec_input_line_policy("printf '%s\\n' hello\n").is_ok());
        // Prefix matches must land on the program token, not substrings.
        assert!(enforce_exec_input_line_policy("bashism\n").is_ok());
    }
}
