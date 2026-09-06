//! Helper functions for the execute module.
//!
//! Contains standalone functions used by the main execute_task loop that
//! don't need access to `&mut self`.

use crate::core::agent::events::ExecEventRecorder;
use crate::core::agent::task::TaskOutcome;
use crate::llm::provider::{FinishReason, Message, ResponsesContinuationState};
use vtcode_exec_events::Usage;

/// Record the terminal turn event (TurnCompleted or TurnFailed) based on outcome.
///
/// Routes through `turn_completed`/`turn_failed` (not a raw `record_thread_event`)
/// so the recorder's turn lifecycle — in particular `finish_turn()` clearing the
/// in-flight-turn flag — is always driven to completion. Skipping this left the
/// thread's `turn_in_flight` stuck true, breaking `begin_turn` on every retry.
pub(super) fn record_terminal_turn_event(event_recorder: &mut ExecEventRecorder, outcome: &TaskOutcome, usage: Usage) {
    if outcome.is_success() {
        event_recorder.turn_completed(usage);
    } else {
        event_recorder.turn_failed(&outcome.description(), Some(usage));
    }
}

/// Emit blocked handoff events for both current and archive paths.
///
/// Also publishes the first-class `turn.blocked` contract event so UI and
/// external subscribers see the same signal the unified runloop emits; the
/// legacy runner does not track the blocked-tool fuse counters, so they keep
/// their defaults.
pub(super) fn emit_blocked_handoff_events(
    event_recorder: &mut ExecEventRecorder,
    blocked_message: &str,
    current_path: &std::path::Path,
    archive_path: &std::path::Path,
) {
    event_recorder.turn_blocked(vtcode_exec_events::TurnBlockedEvent {
        message: blocked_message.to_string(),
        last_tool: None,
        blocked_streak: 0,
        blocked_total: 0,
        consecutive_cap: 0,
        total_cap: 0,
        recovery_active: false,
        usage: None,
    });
    event_recorder.harness_event(
        crate::exec::events::HarnessEventKind::TurnBlocked,
        Some(blocked_message.to_string()),
        None,
        None,
        None,
        None,
        None,
    );
    for path in [current_path, archive_path] {
        event_recorder.harness_event(
            crate::exec::events::HarnessEventKind::BlockedHandoffWritten,
            Some("Blocked handoff written".to_string()),
            None,
            Some(path.display().to_string()),
            None,
            None,
            None,
        );
    }
}

/// Summarize verification output from a tool result JSON.
pub(super) fn summarize_verification_output(result: &serde_json::Value) -> String {
    result
        .get("output")
        .and_then(serde_json::Value::as_str)
        .or_else(|| result.get("stderr").and_then(serde_json::Value::as_str))
        .or_else(|| result.get("stdout").and_then(serde_json::Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| {
            let truncated = text.lines().take(20).collect::<Vec<_>>().join("\n").trim().to_string();
            if truncated.len() < text.len() {
                format!("{truncated}\n...")
            } else {
                truncated
            }
        })
        .unwrap_or_default()
}

/// Map a FinishReason to a stop_reason string for the turn event.
pub(super) fn stop_reason_from_finish_reason(finish_reason: &FinishReason) -> String {
    match finish_reason {
        FinishReason::Stop => "end_turn".to_string(),
        FinishReason::Length => "max_tokens".to_string(),
        FinishReason::ToolCalls => "tool_calls".to_string(),
        FinishReason::ContentFilter => "content_filter".to_string(),
        FinishReason::Pause => "pause_turn".to_string(),
        FinishReason::Refusal => "refusal".to_string(),
        FinishReason::Error(message) => message.clone(),
    }
}

/// Prepare request messages for responses-style continuation.
pub(super) fn prepare_responses_request_messages<'a>(
    previous_chains: &mut hashbrown::HashMap<(String, String), ResponsesContinuationState>,
    provider_name: &str,
    provider_supports_responses_compaction: bool,
    model: &str,
    messages: &'a [Message],
) -> (std::borrow::Cow<'a, [Message]>, Option<String>) {
    let key = crate::llm::provider::responses_continuation_key(provider_name, model);
    let continuation = key.as_ref().and_then(|k| previous_chains.get(k));
    let prepared = crate::llm::provider::prepare_responses_continuation_request(
        provider_name,
        provider_supports_responses_compaction,
        messages,
        continuation,
    );
    if prepared.clear_stale_chain
        && let Some(key) = key
    {
        previous_chains.remove(&key);
    }

    (prepared.messages, prepared.previous_response_id)
}
