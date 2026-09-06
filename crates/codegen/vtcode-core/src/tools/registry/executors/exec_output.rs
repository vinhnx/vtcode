//! Output building for the exec tool family.
//!
//! Turns captured command output into the response seen by the model: ANSI/PTY
//! normalization, bounded previews, `filter_lines` query matching, head/tail
//! excerpts, and the final response JSON (including cargo failure diagnostics
//! and recovery guidance). Split out of `exec_support.rs` so response shaping
//! stays separate from command construction and session book-keeping.

use super::cargo_failure_diagnostics::{
    attach_exec_recovery_guidance, attach_failure_diagnostics_metadata, cargo_test_failure_diagnostics,
};
use super::exec_command::suggest_max_tokens_for_command;
use super::exec_support::{
    ExecOutputPreview, ExecRunOutputConfig, PtyEphemeralCapture, attach_exec_response_context,
    attach_long_command_wait_steering, attach_pty_continuation, floor_exec_char_boundary,
    max_output_tokens_from_payload,
};
use crate::tools::types::VTCodeExecSession;
use anyhow::{Context, Result};
use regex::Regex;
use serde_json::{Value, json};
use std::{fmt::Write, sync::Mutex};
use vtcode_commons::preview::excerpt_text_lines;

const DEFAULT_INSPECT_MAX_MATCHES: usize = 200;
const EXEC_OUTPUT_TRUNCATED_SENTINEL: &str = "\n[Output truncated]";

pub(super) fn build_exec_output_preview(raw_output: &str, max_tokens: usize) -> (String, bool) {
    if max_tokens == 0 {
        // A zero budget means "no inline output"; returning the full capture
        // here would leak unbounded bytes into the response.
        return (String::new(), true);
    }

    let max_output_len = max_tokens.saturating_mul(4);
    if raw_output.len() <= max_output_len {
        return (String::new(), false);
    }

    let preview_end = floor_exec_char_boundary(raw_output, max_output_len);
    let mut output = raw_output[..preview_end].to_string();
    output.push_str(EXEC_OUTPUT_TRUNCATED_SENTINEL);

    (output, true)
}

pub(super) fn build_exec_response(
    session: &VTCodeExecSession,
    command: &str,
    capture: &PtyEphemeralCapture,
    output_preview: ExecOutputPreview,
    matched_count: Option<usize>,
    query_truncated: bool,
    running_process_id: Option<&str>,
) -> Value {
    let ExecOutputPreview { raw_output, output, truncated } = output_preview;
    let cargo_test_diagnostics = cargo_test_failure_diagnostics(command, &raw_output, capture.exit_code);
    let mut response = json!({
        "success": true,
        "output": output,
        "wall_time": capture.duration.as_secs_f64(),
    });
    if let Some(count) = matched_count {
        response["matched_count"] = json!(count);
        response["query_truncated"] = json!(query_truncated);
    }

    attach_exec_response_context(&mut response, session, command, capture.exit_code.is_some());

    if let Some(code) = capture.exit_code {
        response["exit_code"] = json!(code);
    } else if let Some(process_id) = running_process_id {
        response["process_id"] = json!(process_id);
    }

    if truncated {
        response["truncated"] = json!(true);
    }
    if capture.exit_code.is_none() {
        attach_pty_continuation(&mut response, session.id.as_str());
        attach_long_command_wait_steering(&mut response, session.id.as_str(), capture.duration);
    }

    attach_exec_recovery_guidance(&mut response, command, capture.exit_code);
    if let Some(diagnostics) = cargo_test_diagnostics {
        attach_failure_diagnostics_metadata(&mut response, &diagnostics);
    }
    response
}

pub(super) fn exec_run_output_config(
    payload: &serde_json::Map<String, Value>,
    display_command: &str,
) -> ExecRunOutputConfig {
    ExecRunOutputConfig {
        max_tokens: max_output_tokens_from_payload(payload)
            .or_else(|| suggest_max_tokens_for_command(display_command))
            .unwrap_or(crate::config::constants::defaults::DEFAULT_PTY_OUTPUT_MAX_TOKENS),
        inspect_query: payload
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        inspect_literal: payload.get("literal").and_then(Value::as_bool).unwrap_or(false),
        inspect_max_matches: clamp_max_matches(payload.get("max_matches").and_then(Value::as_u64)),
    }
}

pub(super) fn build_exec_filtered_response(
    session_metadata: &VTCodeExecSession,
    command_display: &str,
    capture: &PtyEphemeralCapture,
    output_config: &ExecRunOutputConfig,
    running_process_id: Option<&str>,
) -> Result<Value> {
    let raw_output = filter_pty_output(&strip_ansi(&capture.output));
    let mut matched_count = None;
    let mut query_truncated = false;
    let filtered_output = if let Some(query) = output_config.inspect_query.as_deref() {
        let (filtered, count, truncated_matches) =
            filter_lines(&raw_output, query, output_config.inspect_literal, output_config.inspect_max_matches)?;
        matched_count = Some(count);
        query_truncated = truncated_matches;
        filtered
    } else {
        raw_output.clone()
    };
    let (preview_output, preview_truncated) = build_exec_output_preview(&filtered_output, output_config.max_tokens);

    Ok(build_exec_response(
        session_metadata,
        command_display,
        capture,
        ExecOutputPreview {
            raw_output,
            // When not truncated the preview is the filtered output itself;
            // move it instead of cloning a second full copy.
            output: if preview_truncated {
                preview_output
            } else {
                filtered_output
            },
            truncated: preview_truncated,
        },
        matched_count,
        query_truncated,
        running_process_id,
    ))
}

pub(super) fn build_exec_passthrough_response(
    session_metadata: &VTCodeExecSession,
    command_display: &str,
    capture: &PtyEphemeralCapture,
    max_tokens: Option<usize>,
) -> Value {
    let raw_output = filter_pty_output(&strip_ansi(&capture.output));
    let output_preview = if let Some(limit) = max_tokens {
        let (preview, truncated) = build_exec_output_preview(&raw_output, limit);
        ExecOutputPreview {
            output: if truncated { preview } else { raw_output.clone() },
            raw_output,
            truncated,
        }
    } else {
        ExecOutputPreview {
            raw_output: raw_output.clone(),
            output: raw_output,
            truncated: false,
        }
    };

    build_exec_response(session_metadata, command_display, capture, output_preview, None, false, None)
}

pub(super) fn clamp_inspect_lines(value: Option<u64>, default: usize) -> usize {
    value.map(|v| v as usize).unwrap_or(default).min(5_000)
}

pub(super) fn clamp_max_matches(value: Option<u64>) -> usize {
    value
        .map(|v| v as usize)
        .unwrap_or(DEFAULT_INSPECT_MAX_MATCHES)
        .clamp(1, 10_000)
}

pub(super) fn build_head_tail_preview(content: &str, head_lines: usize, tail_lines: usize) -> (String, bool) {
    let preview = excerpt_text_lines(content, head_lines.max(1), tail_lines.max(1));
    if preview.total == 0 {
        return (String::new(), false);
    }

    if preview.hidden_count == 0 {
        return (preview.head.join("\n"), false);
    }

    let mut lines = Vec::with_capacity(preview.head.len() + preview.tail.len() + 1);
    lines.extend(preview.head.into_iter().map(String::from));
    lines.push(format!("[... omitted {} lines ...]", preview.hidden_count));
    lines.extend(preview.tail.into_iter().map(String::from));
    (lines.join("\n"), true)
}

/// Single-slot memo for the `filter_lines` regex: the same filter query is
/// reused across many renders, so we avoid recompiling `Regex::new` per call.
static FILTER_LINES_REGEX: Mutex<Option<(String, Regex)>> = Mutex::new(None);

pub(super) fn filter_lines(
    content: &str,
    query: &str,
    literal: bool,
    max_matches: usize,
) -> Result<(String, usize, bool)> {
    const MAX_LINE_BYTES: usize = 16 * 1024;

    let matcher = if literal {
        None
    } else {
        // Reuse the previously compiled regex when the query is unchanged.
        let compiled = {
            let guard = FILTER_LINES_REGEX.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            match &*guard {
                Some((q, re)) if q == query => re.clone(),
                _ => {
                    drop(guard);
                    let re = Regex::new(query).with_context(|| format!("Invalid regex query: {query}"))?;
                    *FILTER_LINES_REGEX.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
                        Some((query.to_string(), re.clone()));
                    re
                }
            }
        };
        Some(compiled)
    };

    let mut matches = Vec::new();
    let mut total_matches = 0usize;
    let mut buf = String::new();

    for (idx, line) in content.lines().enumerate() {
        // Cap a single matching line so one enormous line (e.g. a minified
        // bundle) cannot be copied whole into the filtered output.
        let line = &line[..line.floor_char_boundary(line.len().min(MAX_LINE_BYTES))];
        let is_match = if literal {
            line.contains(query)
        } else {
            matcher.as_ref().map(|regex| regex.is_match(line)).unwrap_or(false)
        };
        if !is_match {
            continue;
        }

        total_matches = total_matches.saturating_add(1);
        if matches.len() < max_matches {
            buf.clear();
            let _ = write!(buf, "{}: {}", idx + 1, line);
            matches.push(std::mem::take(&mut buf));
        }
    }

    let truncated = total_matches > max_matches;
    Ok((matches.join("\n"), total_matches, truncated))
}

pub(super) fn strip_ansi(text: &str) -> String {
    crate::utils::ansi_parser::strip_ansi(text)
}

pub(super) fn filter_pty_output(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}
