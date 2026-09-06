use std::time::Duration;

use anyhow::Result;
use vtcode_core::core::agent::snapshots::SnapshotTurnContext;
use vtcode_core::exec::events::HarnessEventKind;
use vtcode_core::llm::provider as uni;
use vtcode_core::types::CompactStr;
use vtcode_core::utils::ansi::MessageStyle;
use vtcode_core::utils::session_archive::SessionMessage;

use crate::agent::runloop::unified::display::reset_inline_input;
use crate::agent::runloop::unified::turn::context::{TurnLoopResult, TurnOutcomeContext};
use crate::agent::runloop::unified::turn::turn_loop::TurnLoopOutcome;

fn format_turn_elapsed_label(duration: Duration) -> String {
    let total_seconds = duration.as_secs();
    if total_seconds < 60 {
        return format!("{total_seconds}s");
    }

    if total_seconds < 3600 {
        let minutes = total_seconds / 60;
        let seconds = total_seconds % 60;
        return format!("{minutes}m {seconds}s");
    }

    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    format!("{hours}h {minutes}m")
}

pub(crate) async fn apply_turn_outcome(outcome: TurnLoopOutcome, ctx: TurnOutcomeContext<'_>) -> Result<()> {
    match outcome.result {
        TurnLoopResult::Cancelled => {
            if ctx.ctrl_c_state.is_exit_requested() {
                *ctx.session_end_reason = vtcode_core::hooks::SessionEndReason::Exit;
                return Ok(());
            }
            ctx.renderer.line_if_not_empty(MessageStyle::Output)?;
            ctx.renderer
                .line(MessageStyle::Info, "Interrupted current task. Press Esc, Ctrl+C, or /stop again to exit.")?;
            reset_inline_input(
                ctx.handle,
                Some(vtcode_config::constants::ui::CHAT_INPUT_PLACEHOLDER_INTERRUPTED.to_owned()),
            );
            ctx.ctrl_c_state.mark_cancel_handled();
            *ctx.session_end_reason = vtcode_core::hooks::SessionEndReason::Cancelled;
            Ok(())
        }
        TurnLoopResult::Exit => {
            *ctx.session_end_reason = vtcode_core::hooks::SessionEndReason::Exit;
            Ok(())
        }
        TurnLoopResult::Aborted => {
            if let Some(last) = ctx.conversation_history.last() {
                match last.role {
                    uni::MessageRole::Assistant | uni::MessageRole::Tool => {
                        let _ = ctx.conversation_history.pop();
                    }
                    _ => {}
                }
            }
            ctx.ctrl_c_state.reset();
            Ok(())
        }
        TurnLoopResult::Blocked { reason } => {
            if let Some(reason) = reason.as_deref() {
                let _ = ctx.renderer.line(MessageStyle::Info, reason);
            }
            reset_inline_input(ctx.handle, ctx.default_placeholder.clone());
            ctx.ctrl_c_state.reset();
            Ok(())
        }
        TurnLoopResult::Completed { .. } => {
            if let Some(manager) = ctx.checkpoint_manager {
                let conversation_snapshot: Vec<SessionMessage> =
                    ctx.conversation_history.iter().map(SessionMessage::from).collect();
                let turn_number = *ctx.next_checkpoint_turn;
                let mut turn_diagnostics = outcome.turn_diagnostics.clone();
                turn_diagnostics.elapsed_ms = ctx.turn_elapsed.as_millis().min(u128::from(u64::MAX)) as u64;
                let turn_context = SnapshotTurnContext {
                    session_id: Some(CompactStr::from(ctx.session_id)),
                    runtime_turn_id: ctx.runtime_turn_id.map(CompactStr::from),
                    session_turn_number: Some(turn_number),
                    turn_diagnostics: Some(turn_diagnostics),
                    touched_files: outcome
                        .turn_touched_files
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect(),
                };
                match manager
                    .create_snapshot(
                        turn_number,
                        ctx.completed_turn_prompt.unwrap_or_default(),
                        &conversation_snapshot,
                        &outcome.turn_modified_files,
                        ctx.completed_turn_prompt,
                        ctx.completed_turn_prompt_message_index,
                        Some(turn_context),
                    )
                    .await
                {
                    Ok(Some(meta)) => {
                        *ctx.next_checkpoint_turn = meta.turn_number.saturating_add(1);
                        if let Some(emitter) = ctx.harness_emitter {
                            let _ =
                                emitter.emit(crate::agent::runloop::unified::inline_events::harness::harness_event(
                                    HarnessEventKind::SnapshotCreated,
                                    Some(format!("Turn {turn_number} snapshot saved")),
                                    None,
                                    None,
                                    None,
                                ));
                        }
                    }
                    Ok(None) => {}
                    Err(err) => tracing::warn!("Failed to create checkpoint for turn {}: {}", turn_number, err),
                }
            }
            if ctx.show_turn_timer {
                ctx.renderer
                    .line(MessageStyle::Info, &format!("Worked for {}", format_turn_elapsed_label(ctx.turn_elapsed)))?;
            }
            ctx.ctrl_c_state.reset();
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
    use vtcode_core::utils::ansi::AnsiRenderer;
    use vtcode_ui::tui::app::{InlineCommand, InlineHandle};

    use super::*;
    use crate::agent::runloop::unified::state::CtrlCState;

    fn renderer_with_channel() -> (InlineHandle, AnsiRenderer, UnboundedReceiver<InlineCommand>) {
        let (tx, rx) = unbounded_channel();
        let handle = InlineHandle::new_for_tests(tx);
        let renderer = AnsiRenderer::with_inline_ui(handle.clone(), Default::default());
        (handle, renderer, rx)
    }

    fn drain_appended_lines(receiver: &mut UnboundedReceiver<InlineCommand>) -> Vec<String> {
        let mut lines = Vec::new();
        while let Ok(command) = receiver.try_recv() {
            if let InlineCommand::AppendLine { segments, .. } = command {
                let line = segments.into_iter().map(|segment| segment.text).collect::<String>();
                if !line.trim().is_empty() {
                    lines.push(line);
                }
            }
        }
        lines
    }

    #[test]
    fn format_turn_elapsed_label_mixed_adaptive() {
        assert_eq!(format_turn_elapsed_label(Duration::from_secs(59)), "59s");
        assert_eq!(format_turn_elapsed_label(Duration::from_secs(90)), "1m 30s");
        assert_eq!(format_turn_elapsed_label(Duration::from_secs(3600)), "1h 0m");
    }

    #[tokio::test]
    async fn blocked_turn_resets_interactive_input_without_ending_session() {
        let (handle, mut renderer, mut receiver) = renderer_with_channel();
        let ctrl_c_state = Arc::new(CtrlCState::new());
        let default_placeholder = None;
        let mut session_end_reason = vtcode_core::hooks::SessionEndReason::Completed;
        let mut next_checkpoint_turn = 1usize;
        let mut conversation_history = Vec::new();
        let outcome = TurnLoopOutcome {
            result: TurnLoopResult::Blocked { reason: Some("blocked for test".to_owned()) },
            turn_modified_files: BTreeSet::new(),
            turn_touched_files: BTreeSet::new(),
            turn_diagnostics: Default::default(),
            pending_primary_agent: None,
            pending_plan_auto_accept: false,
            pending_plan_execution_context:
                crate::agent::runloop::unified::planning_workflow::PlanExecutionContext::Current,
            plan_approved_execution_pending: false,
            final_response_was_fallback: false,
        };

        apply_turn_outcome(
            outcome,
            TurnOutcomeContext {
                conversation_history: &mut conversation_history,
                completed_turn_prompt: None,
                completed_turn_prompt_message_index: None,
                renderer: &mut renderer,
                handle: &handle,
                ctrl_c_state: &ctrl_c_state,
                default_placeholder: &default_placeholder,
                checkpoint_manager: None,
                next_checkpoint_turn: &mut next_checkpoint_turn,
                session_end_reason: &mut session_end_reason,
                turn_elapsed: Duration::from_secs(1),
                show_turn_timer: true,
                session_id: "session-test",
                runtime_turn_id: None,
                harness_emitter: None,
            },
        )
        .await
        .expect("apply blocked outcome");

        assert!(matches!(session_end_reason, vtcode_core::hooks::SessionEndReason::Completed));
        let mut saw_clear_input = false;
        let mut saw_default_placeholder = false;
        while let Ok(command) = receiver.try_recv() {
            match command {
                InlineCommand::ClearInput => saw_clear_input = true,
                InlineCommand::SetPlaceholder { hint: None, .. } => saw_default_placeholder = true,
                _ => {}
            }
        }
        assert!(saw_clear_input);
        assert!(saw_default_placeholder);
    }

    #[tokio::test]
    async fn completed_turn_emits_worked_for_divider() {
        let (handle, mut renderer, mut receiver) = renderer_with_channel();
        let ctrl_c_state = Arc::new(CtrlCState::new());
        let default_placeholder = None;
        let mut session_end_reason = vtcode_core::hooks::SessionEndReason::Completed;
        let mut next_checkpoint_turn = 1usize;
        let mut conversation_history = Vec::new();
        let outcome = TurnLoopOutcome {
            result: TurnLoopResult::Completed { plan_approved_execution_pending: false },
            turn_modified_files: BTreeSet::new(),
            turn_touched_files: BTreeSet::new(),
            turn_diagnostics: Default::default(),
            pending_primary_agent: None,
            pending_plan_auto_accept: false,
            pending_plan_execution_context:
                crate::agent::runloop::unified::planning_workflow::PlanExecutionContext::Current,
            plan_approved_execution_pending: false,
            final_response_was_fallback: false,
        };
        conversation_history.push(uni::Message::assistant("done".to_string()));

        apply_turn_outcome(
            outcome,
            TurnOutcomeContext {
                conversation_history: &mut conversation_history,
                completed_turn_prompt: None,
                completed_turn_prompt_message_index: None,
                renderer: &mut renderer,
                handle: &handle,
                ctrl_c_state: &ctrl_c_state,
                default_placeholder: &default_placeholder,
                checkpoint_manager: None,
                next_checkpoint_turn: &mut next_checkpoint_turn,
                session_end_reason: &mut session_end_reason,
                turn_elapsed: Duration::from_secs(90),
                show_turn_timer: true,
                session_id: "session-test",
                runtime_turn_id: None,
                harness_emitter: None,
            },
        )
        .await
        .expect("apply completed outcome");

        let lines = drain_appended_lines(&mut receiver);
        assert!(lines.iter().any(|line| line == "Worked for 1m 30s"));
    }

    #[tokio::test]
    async fn cancelled_turn_does_not_emit_worked_for_divider() {
        let (handle, mut renderer, mut receiver) = renderer_with_channel();
        let ctrl_c_state = Arc::new(CtrlCState::new());
        let default_placeholder = None;
        let mut session_end_reason = vtcode_core::hooks::SessionEndReason::Completed;
        let mut next_checkpoint_turn = 1usize;
        let mut conversation_history = Vec::new();
        let outcome = TurnLoopOutcome {
            result: TurnLoopResult::Cancelled,
            turn_modified_files: BTreeSet::new(),
            turn_touched_files: BTreeSet::new(),
            turn_diagnostics: Default::default(),
            pending_primary_agent: None,
            pending_plan_auto_accept: false,
            pending_plan_execution_context:
                crate::agent::runloop::unified::planning_workflow::PlanExecutionContext::Current,
            plan_approved_execution_pending: false,
            final_response_was_fallback: false,
        };
        conversation_history.push(uni::Message::assistant("done".to_string()));

        apply_turn_outcome(
            outcome,
            TurnOutcomeContext {
                conversation_history: &mut conversation_history,
                completed_turn_prompt: None,
                completed_turn_prompt_message_index: None,
                renderer: &mut renderer,
                handle: &handle,
                ctrl_c_state: &ctrl_c_state,
                default_placeholder: &default_placeholder,
                checkpoint_manager: None,
                next_checkpoint_turn: &mut next_checkpoint_turn,
                session_end_reason: &mut session_end_reason,
                turn_elapsed: Duration::from_secs(90),
                show_turn_timer: true,
                session_id: "session-test",
                runtime_turn_id: None,
                harness_emitter: None,
            },
        )
        .await
        .expect("apply cancelled outcome");

        let lines = drain_appended_lines(&mut receiver);
        assert!(!lines.iter().any(|line| line.contains("Worked for")));
    }

    #[tokio::test]
    async fn completed_turn_skips_timer_when_disabled() {
        let (handle, mut renderer, mut receiver) = renderer_with_channel();
        let ctrl_c_state = Arc::new(CtrlCState::new());
        let default_placeholder = None;
        let mut session_end_reason = vtcode_core::hooks::SessionEndReason::Completed;
        let mut next_checkpoint_turn = 1usize;
        let mut conversation_history = Vec::new();
        let outcome = TurnLoopOutcome {
            result: TurnLoopResult::Completed { plan_approved_execution_pending: false },
            turn_modified_files: BTreeSet::new(),
            turn_touched_files: BTreeSet::new(),
            turn_diagnostics: Default::default(),
            pending_primary_agent: None,
            pending_plan_auto_accept: false,
            pending_plan_execution_context:
                crate::agent::runloop::unified::planning_workflow::PlanExecutionContext::Current,
            plan_approved_execution_pending: false,
            final_response_was_fallback: false,
        };
        conversation_history.push(uni::Message::assistant("done".to_string()));

        apply_turn_outcome(
            outcome,
            TurnOutcomeContext {
                conversation_history: &mut conversation_history,
                completed_turn_prompt: None,
                completed_turn_prompt_message_index: None,
                renderer: &mut renderer,
                handle: &handle,
                ctrl_c_state: &ctrl_c_state,
                default_placeholder: &default_placeholder,
                checkpoint_manager: None,
                next_checkpoint_turn: &mut next_checkpoint_turn,
                session_end_reason: &mut session_end_reason,
                turn_elapsed: Duration::from_secs(90),
                show_turn_timer: false,
                session_id: "session-test",
                runtime_turn_id: None,
                harness_emitter: None,
            },
        )
        .await
        .expect("apply completed outcome with timer disabled");

        let lines = drain_appended_lines(&mut receiver);
        assert!(!lines.iter().any(|line| line.contains("Worked for")));
    }
}
