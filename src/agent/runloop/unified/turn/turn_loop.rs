//! Agent Legibility:
//! - Entrypoint: `run_turn_loop` coordinates the per-turn request, recovery, tool execution, and completion flow.
//! - Common changes:
//!   - Main loop policy and break/continue rules stay in this root.
//!   - Post-tool recovery, usage accounting, and completion notification helpers live in `turn_loop/` support modules.
//! - Constraints: Preserve turn-phase transitions and recovery semantics when moving helpers out of the root.
//! - Verify: `cargo check -p vtcode && cargo test -p vtcode --bin vtcode turn_loop`

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::RwLock;
use vtcode_core::acp::ToolPermissionCache;
use vtcode_core::config::loader::VTCodeConfig;
use vtcode_core::core::agent::events::{tool_invocation_completed_event, tool_output_completed_event};
use vtcode_core::core::agent::runtime::RuntimeSteering;
use vtcode_core::core::decision_tracker::DecisionTracker;
use vtcode_core::core::trajectory::TrajectoryLogger;
use vtcode_core::exec::events::{ToolCallStatus, Usage as HarnessUsage, tool_outcome_from_status};
use vtcode_core::hooks::LifecycleHookEngine;
use vtcode_core::llm::provider as uni;
use vtcode_core::tools::{ApprovalRecorder, ToolRegistry, ToolResultCache};
use vtcode_core::utils::ansi::{AnsiRenderer, MessageStyle};
use vtcode_ui::tui::app::{InlineHandle, InlineSession};

use crate::agent::runloop::unified::inline_events::harness::{
    HarnessEventEmitter, harness_event, turn_blocked_event, turn_completed_event, turn_failed_event, turn_started_event,
};
use crate::agent::runloop::unified::planning_workflow::maybe_handle_planning_exit_trigger;
use crate::agent::runloop::unified::planning_workflow_state::PlanningWorkflowSessionState;
use crate::agent::runloop::unified::run_loop_context::{HarnessTurnState, RunLoopContext, TurnPhase};
use crate::agent::runloop::unified::tool_call_safety::ToolCallSafetyValidator;
use crate::agent::runloop::unified::turn::context::TurnLoopResult;
use crate::agent::runloop::unified::turn::turn_loop_helpers::{
    ToolLoopLimitAction, extract_turn_config, handle_steering_messages, initial_tool_loop_limit,
    is_stale_approved_plan_pause_response, maybe_handle_planning_enter_trigger, maybe_handle_tool_loop_limit,
    resolve_safety_tool_call_limits,
};

#[path = "turn_loop/notifications.rs"]
mod notifications;
#[path = "turn_loop/post_tool_recovery.rs"]
mod post_tool_recovery;
#[path = "turn_loop/recovery_compaction.rs"]
mod recovery_compaction;
#[path = "turn_loop/usage_accounting.rs"]
mod usage_accounting;

// Using `tool_output_handler::handle_pipeline_output_from_turn_ctx` adapter where needed

use notifications::emit_turn_outcome_notification;
#[cfg(test)]
use post_tool_recovery::PostToolFailureRecovery;
#[cfg(test)]
use post_tool_recovery::maybe_recover_after_post_tool_llm_failure;
#[cfg(test)]
use post_tool_recovery::normalize_tool_free_recovery_break_outcome;
use post_tool_recovery::{
    POST_TOOL_CONTEXT_COMPACTION_FAILED_REASON, PlanRecoveryEventContext, PostToolFailureAction,
    PostToolRecoveryContext, dispatch_post_tool_failure, ensure_post_tool_resume_directive,
    normalize_tool_free_recovery_break_outcome_with_events,
};
#[cfg(test)]
use recovery_compaction::current_turn_preserve_index;
use recovery_compaction::{RecoveryCompactionRequest, compact_before_tool_enabled_retry};
use usage_accounting::{accumulate_turn_usage, estimate_session_costs, has_turn_usage, stop_reason_from_finish_reason};
use vtcode_core::config::types::AgentConfig;
use vtcode_core::core::agent::error_recovery::ErrorType;
use vtcode_core::primary_agent::ActivePrimaryAgentState;

use crate::agent::runloop::mcp_events;
use crate::agent::runloop::unified::turn::tool_outcomes::helpers::{
    ANTI_BLIND_EDITING_DIRECTIVE, FAILED_VERIFICATION_FIX_ALLOWANCE, LoopTracker,
};
use crate::agent::runloop::unified::turn::turn_helpers::{display_error, error_message_for_user};

/// Max completion tokens for the tool-free recovery synthesis pass.
///
/// Raised from 1024 → 4096: recovery synthesis must summarize the entire
/// turn's tool outputs (often dozens of file reads and searches). 1024 tokens
/// truncated substantive answers — observed in checkpoint turn_621 where a
/// launch-time analysis over ~60 messages could not fit and the pass
/// destabilized into emitting tool-call markup instead of prose.
const RECOVERY_SYNTHESIS_MAX_TOKENS: u32 = 4096;
/// Maximum number of times the recovery pass is retried when the model
/// returns tool calls (discarded) instead of text during tool-free recovery.
///
/// Raised from 2 → 3: a single transient post-tool follow-up failure (e.g. an
/// LLM stream timeout on a large context) no longer terminates the turn and
/// forces the user to nudge "continue". The extra pass only fires when the
/// model keeps emitting tool calls during a tool-free recovery window.
const MAX_RECOVERY_RETRIES: u8 = 3;
/// Hard cap on consecutive assistant text-only responses. Without this, the
/// recovery / continuation logic can loop forever when the model has already
/// produced a substantive final answer but the system keeps re-prompting it
/// (e.g. tool-free recovery activates on a transient LLM stream timeout, the
/// model re-summarizes the same outline 4+ times, wasting context and tokens).
/// Admitted tool execution resets the streak. Blocked and malformed attempts
/// retain it because they are not productive progress; their dedicated
/// safeguards still bound those failure paths.
///
/// Observed in checkpoint turn_594: a simple "what functions/structs are
/// defined in crates/codegen/vtcode-core/src/tools/registry?" task produced 4 identical
/// 6500-token outline responses and burned ~90 seconds.  Capping at 2
/// consecutive responses terminate the runaway loop while still allowing one
/// retry for genuine recovery scenarios.
pub(crate) const MAX_ASSISTANT_TEXT_RESPONSES_PER_TURN: u32 = 2;
/// Closure text for streamed tool-call items whose calls never reached the
/// pipeline (rejected pre-flight, dropped from a batch, or interrupted when
/// the turn ended). Teardown emits it so the session log carries a terminal
/// `tool_output` instead of a dangling `item.started`.
const UNDISPATCHED_TOOL_CALL_CLOSURE_TEXT: &str = "Tool call ended when the turn finished before it could execute.";
pub(crate) const ASSISTANT_TEXT_RESPONSE_CAP_REASON: &str =
    "Turn blocked after repeated assistant responses reached the safety cap; the latest response was preserved.";
pub(crate) const PENDING_VERIFICATION_BLOCK_REASON: &str =
    "Turn blocked after repeated unverified assistant responses; verification is still pending.";
const PENDING_VERIFICATION_FINAL_RESPONSE_PREFIX: &str = "The turn is blocked because verification is still pending. \
    Inspection-only checks do not clear the verification gate; run `cargo check --locked`, \
    `cargo fmt --all -- --check`, or the relevant `cargo nextest run` command (standalone or as a pure `&&` chain, \
    no `| head` pipes and no `;`/`||`/`|` joins) to exit 0, then resume the request. \
    A failed verifier grants ";
const PENDING_VERIFICATION_FINAL_RESPONSE_SUFFIX: &str = " fix-up edits before re-verify is required.";

fn pending_verification_final_response() -> String {
    format!(
        "{PENDING_VERIFICATION_FINAL_RESPONSE_PREFIX}{FAILED_VERIFICATION_FIX_ALLOWANCE}{PENDING_VERIFICATION_FINAL_RESPONSE_SUFFIX}"
    )
}
const CONTEXT_CAPACITY_FINAL_RESPONSE: &str = "The turn is blocked because context capacity or compaction failed. \
    The retained tool outputs and progress are preserved; resume the request or switch \
    models and try again.";
const GENERIC_BLOCKED_FINAL_RESPONSE: &str = "The turn is blocked before success could be confirmed. \
    The available history and outputs are retained; resume the request to continue.";
/// Maximum number of times the post-tool follow-up failure path may schedule
/// a tool-free recovery pass within a single turn. This is a defense-in-depth
/// backstop: the recovery pass itself is terminal (a text response ends the
/// turn), so under normal operation this cap never trips. It only fires if a
/// future regression re-enables tools after recovery or otherwise re-triggers
/// the post-tool failure path cyclically.
const MAX_POST_TOOL_RECOVERY_CYCLES: u8 = 2;
/// Bound retries for stale planning-recovery status text after an approved
/// plan handoff. The response is discarded and the model is reminded that the
/// new build turn has tools; after two attempts normal failure handling wins.
const MAX_APPROVED_PLAN_STALE_PAUSE_RETRIES: u8 = 2;
pub(crate) const POST_TOOL_RECOVERY_REASON: &str =
    "Tool follow-up failed. Tools disabled; respond with text using context and recent tool outputs.";
const RECOVERY_SYNTHESIS_FALLBACK_FINAL_ANSWER: &str = "Recovery synthesis failed; no tool call applied. The tool outputs gathered above contain the information needed. Re-state your request and the next turn will reuse the gathered context from this conversation history.";
/// Plan-mode variant of [`RECOVERY_SYNTHESIS_FALLBACK_FINAL_ANSWER`]. In plan
/// mode the turn must not dead-end: planning research is preserved and the
/// interview is re-forced on the next turn (the planning session state is left
/// `interview_pending`), so this message tells the user to continue planning
/// rather than re-state a generic request.
const PLANNING_RECOVERY_SYNTHESIS_FALLBACK: &str = "Planning research completed, but final synthesis failed after one bounded automatic retry. The gathered context is preserved, no implementation has been approved, and the next planning turn will reuse this research.";
const PLANNING_RECOVERY_SYNTHESIS_FALLBACK_NO_RETRY: &str = "Planning research completed, but final synthesis failed to produce an approval-ready plan. The gathered context is preserved, no implementation has been approved, and the next planning turn will reuse this research.";
/// Variant of [`PLANNING_RECOVERY_SYNTHESIS_FALLBACK`] used once
/// `request_user_input` has been permanently denied by policy this session
/// (`PlanningWorkflowSessionState::is_interview_denied`). The transient-error
/// variant above assumes the interview can be re-forced on the next turn — a
/// denial recurs on every attempt, so no interview will ever be shown again.
/// This presents a clean yes/no/edit HITL prompt to the
/// user instead of a confusing "synthesis failed" message (checkpoint
/// turn_655/turn_660/turn_725). The plan draft from gathered research is
/// preserved in the session plan file; the user picks one of three paths:
///   - `yes`/`implement` → exit plan mode and start implementation
///   - `no` → abandon the plan
///   - `edit`/`keep planning` → refine the plan (user re-states what to revise)
///
/// Use this ONLY when an approval-ready draft was actually persisted
/// (`persisted_plan_ready`). The message promises "Review the plan below" and
/// offers `yes`/`implement`/`no`/`edit` choices that dead-end without a
/// persisted plan. When synthesis produced no draft, use
/// [`PLANNING_INTERVIEW_DENIED_NO_DRAFT_NOTICE`] instead (checkpoint turn_902).
const PLANNING_RECOVERY_SYNTHESIS_FALLBACK_NO_INTERVIEW: &str = "Plan draft ready (interactive questions are unavailable in this runtime, so the plan was finalized from the research already gathered). Review the plan below. For long research sessions, choose `Yes, clear context and implement` to preserve the plan while starting execution with a fresh context and tool budget. Choose `Yes, implement this plan` when the recent planning details are still useful. Type `no` to abandon or `edit` to revise.";
/// Variant of [`PLANNING_RECOVERY_SYNTHESIS_FALLBACK_NO_INTERVIEW`] for the
/// case where the bounded plan synthesis did NOT produce an approval-ready
/// draft (the model emitted research prose or pseudo-tool-call markup instead
/// of a `<proposed_plan>`). The ready-draft variant above promises "Review the
/// plan below" and offers `yes`/`implement`/`no`/`edit` — choices that
/// dead-end without a persisted plan, and which the appended
/// `PLANNING_WORKFLOW_NO_APPROVAL_READY_PLAN_HINT` immediately contradicts with
/// "no approval-ready plan was produced". This variant makes the primary
/// message consistent with that hint: it tells the user no draft was produced
/// and asks them to re-state so the next turn can synthesize from the research
/// already gathered (checkpoint turn_902).
const PLANNING_INTERVIEW_DENIED_NO_DRAFT_NOTICE: &str = "Planning synthesis did not produce an approval-ready plan, and interactive questions are unavailable in this runtime. The research gathered this turn is preserved in this conversation. Re-state what you'd like to plan; the next turn will reuse the evidence already gathered instead of re-exploring.";
/// User-facing final answer for the budget-exhausted plan-mode dead end. The
/// the planning session stays alive so `implement` / `keep planning` still
/// work. Use this variant when an approval-ready draft was persisted.
const PLANNING_BUDGET_EXHAUSTED_USER_NOTICE: &str = "Planning research reached its safe budget before synthesis completed. The evidence and current plan draft are preserved in the session plan file (.vtcode/plans/); retry approval or revision from the preserved plan.";
/// No-draft variant of [`PLANNING_BUDGET_EXHAUSTED_USER_NOTICE`]. Used when
/// budget was exhausted but no approval-ready plan was produced. Does NOT
/// mention a plan draft or plan file, consistent with the appended
/// `PLANNING_WORKFLOW_NO_APPROVAL_READY_PLAN_HINT`.
const PLANNING_BUDGET_EXHAUSTED_NO_DRAFT_NOTICE: &str = "Planning research reached its safe budget before synthesis completed. The evidence gathered this turn is preserved in this conversation. Re-state what you'd like to plan; the next turn will reuse the evidence already gathered instead of re-exploring.";
/// User-facing final answer for the recovery-exhausted plan-mode dead end.
/// See [`PLANNING_BUDGET_EXHAUSTED_USER_NOTICE`]. Use this variant when an
/// approval-ready draft was persisted.
const PLANNING_RECOVERY_EXHAUSTED_USER_NOTICE: &str = "Plan synthesis failed after repeated recovery attempts (provider errors or saturated context). The research gathered and the current plan draft are preserved in the session plan file (.vtcode/plans/).";
/// No-draft variant of [`PLANNING_RECOVERY_EXHAUSTED_USER_NOTICE`]. Used when
/// recovery is exhausted but no approval-ready plan was produced.
const PLANNING_RECOVERY_EXHAUSTED_NO_DRAFT_NOTICE: &str = "Plan synthesis failed after repeated recovery attempts (provider errors or saturated context). The research gathered this turn is preserved in this conversation. Re-state what you'd like to plan; the next turn will reuse the evidence already gathered instead of re-exploring.";
/// Reason set on `TurnLoopResult::Blocked` when the model emits tool calls or
/// textual tool-call markup during a tool-free recovery pass.  Shared between
/// `result_handler` (producer) and `post_tool_recovery` (consumer).
pub(super) const RECOVERY_CONTRACT_VIOLATION_REASON: &str =
    "Recovery mode requested a final tool-free synthesis pass, but the model attempted more tool calls.";
pub(crate) const COMPLETED_TURN_FALLBACK_RESPONSE: &str = "The turn stopped before a final assistant response was produced. No final outcome was confirmed; please retry the request.";
const COMPLETED_TURN_FALLBACK_REASON: &str = "Turn ended with a recovery fallback; the requested work was not confirmed. The current plan and task state were retained.";
const COMPLETED_TURN_NO_RESPONSE_REASON: &str =
    "Turn ended without a harness-visible final assistant response, so successful completion could not be confirmed.";
const PLAN_RECOVERY_EXHAUSTED_REASON: &str = "Approved-plan execution stopped after recovery was exhausted. The approved plan and task checklist were retained; retry from the pending step.";
/// System message injected before retrying a tool-free recovery pass when the model
/// produced tool calls or textual tool-call markup (which are discarded) instead of
/// plain text. It names the failure mode explicitly so the model can self-correct on
/// the retry rather than repeating the same violation (observed in checkpoint turn_621,
/// where a single `<tool_call>` block terminated the turn instead of being retried).
const RECOVERY_TOOL_CALL_RETRY_DIRECTIVE: &str = "Recovery: tools are disabled, so respond with plain text only. Your previous \
     attempt included tool-call or function-call markup; summarize the findings from \
     the tool outputs already in history as a final answer, without any <tool_call>, \
     <function=...>, or other tool-call syntax.";
/// Plan-mode variant of [`POST_TOOL_RECOVERY_REASON`]. In plan mode the agent
/// must emit the `<proposed_plan>` from the research already gathered, not start
/// more research — so the recovery reason names the plan format explicitly.
/// Without this, the model treats the tool-free recovery pass as another
/// research step and emits `<invoke>`/`<tool_call>` markup instead of a plan
/// (observed in checkpoints turn_648 and turn_650).
const POST_TOOL_RECOVERY_REASON_PLAN_MODE: &str = "Planning research completed, but final plan synthesis needs recovery. Tools are disabled. Produce the `<proposed_plan>` NOW from the context and tool outputs already in this conversation: keep each step to a single line (`Action -> files: [path] -> verify: [command]`), prefer file:symbol references, and do NOT emit any tool calls or tool-call markup.";
/// Plan-mode variant of [`RECOVERY_TOOL_CALL_RETRY_DIRECTIVE`]. The generic
/// directive only says \"respond with plain text\"; in plan mode the agent must
/// instead finalize the `<proposed_plan>` from gathered research, otherwise it
/// loops emitting `<invoke>` research calls during the tool-free recovery pass.
const RECOVERY_TOOL_CALL_RETRY_DIRECTIVE_PLAN_MODE: &str = "Recovery: in plan mode, tools are disabled and you must finalize the plan. Emit ONLY the `<proposed_plan>` now from the research already gathered in this conversation — each step on a single line (`Action -> files: [path] -> verify: [command]`), no prose, no tool calls, and no `<tool_call>`/`<invoke>`/`<function=...>` markup.";
const APPROVED_PLAN_STALE_PAUSE_RECOVERY_DIRECTIVE: &str = "Approved-plan execution recovery: the previous response incorrectly claimed that tools were disabled or implementation was paused. The planning approval is complete and the write-capable build agent is active. Continue with the next concrete implementation action now; use task_tracker and execute an edit or verification command. Do not respond with a pause/status message and do not ask for another confirmation.";

fn latest_final_assistant_response(history: &[uni::Message], turn_history_start_len: usize) -> Option<String> {
    history
        .get(turn_history_start_len..)
        .unwrap_or_default()
        .iter()
        .rev()
        .find(|message| {
            message.role == uni::MessageRole::Assistant
                && message.tool_calls.is_none()
                && message.phase != Some(uni::AssistantPhase::Commentary)
                && !message.content.as_text().trim().is_empty()
        })
        .map(|message| message.content.as_text().trim().to_string())
}

/// Promote the latest substantive commentary response when the anti-runaway
/// cap ends a turn without a final answer. The response was already rendered
/// as commentary; changing its phase preserves that content for history and
/// lets the normal blocked-turn publisher emit the canonical final item.
fn promote_latest_commentary_to_final(history: &mut [uni::Message], turn_history_start_len: usize) -> bool {
    if latest_final_assistant_response(history, turn_history_start_len).is_some() {
        return false;
    }

    let Some(turn_history) = history.get_mut(turn_history_start_len..) else {
        return false;
    };
    let Some(message) = turn_history.iter_mut().rev().find(|message| {
        message.role == uni::MessageRole::Assistant
            && message.tool_calls.is_none()
            && message.phase == Some(uni::AssistantPhase::Commentary)
            && !message.content.as_text().trim().is_empty()
    }) else {
        return false;
    };

    message.phase = Some(uni::AssistantPhase::FinalAnswer);
    true
}

fn publish_final_assistant_response(ctx: &mut TurnLoopContext<'_>, text: &str) -> Result<bool> {
    if text.trim().is_empty() {
        return Ok(false);
    }

    if !ctx.harness_state.final_response_rendered() {
        ctx.renderer.line(MessageStyle::Response, text)?;
        ctx.harness_state.mark_final_response_rendered();
    }

    if ctx.harness_emitter.is_none() {
        ctx.harness_state.mark_final_response_event_emitted();
        return Ok(true);
    }

    if !ctx.harness_state.final_response_event_emitted()
        && let Some(emitter) = ctx.harness_emitter
    {
        match emitter.emit_assistant_message(&ctx.harness_state.turn_id.0, text) {
            Ok(()) => ctx.harness_state.mark_final_response_event_emitted(),
            Err(err) => tracing::warn!(error = %err, "final assistant message harness emission failed"),
        }
    }

    Ok(ctx.harness_state.final_response_event_emitted())
}

pub(crate) fn format_blocked_turn_final_response(reason: &str) -> String {
    if reason.contains(PENDING_VERIFICATION_BLOCK_REASON) {
        pending_verification_final_response()
    } else if reason.contains(POST_TOOL_CONTEXT_COMPACTION_FAILED_REASON) {
        CONTEXT_CAPACITY_FINAL_RESPONSE.to_string()
    } else if reason.contains("tool-call limit")
        || reason.contains("Recovery tool-call limit")
        || reason.contains("Blocked tool-call limit")
    {
        format!(
            "The turn is blocked because repeated tool calls were rejected: {reason}. The available history and outputs are retained. You can resume the request with specific guidance, or adjust permissions/tools to continue."
        )
    } else if reason.contains("Repeated shell command") {
        "The turn is blocked because repeated identical shell commands were detected. The available history and outputs are retained. Please provide alternative instructions or adjust the command.".to_string()
    } else if !reason.trim().is_empty() && reason != "blocked" {
        format!(
            "The turn is blocked before success could be confirmed: {reason}. The available history and outputs are retained; resume the request or provide updated instructions to continue."
        )
    } else {
        GENERIC_BLOCKED_FINAL_RESPONSE.to_string()
    }
}

#[cfg(test)]
pub(crate) fn blocked_turn_final_response(reason: &str) -> String {
    format_blocked_turn_final_response(reason)
}

fn ensure_blocked_turn_response(
    ctx: &mut TurnLoopContext<'_>,
    working_history: &mut Vec<uni::Message>,
    turn_history_start_len: usize,
    reason: &str,
) -> Result<()> {
    let existing_final = latest_final_assistant_response(working_history, turn_history_start_len);
    let generated_fallback = existing_final.is_none();
    let final_text = existing_final.unwrap_or_else(|| format_blocked_turn_final_response(reason));
    if generated_fallback {
        ctx.harness_state.mark_final_response_fallback();
        working_history
            .push(uni::Message::assistant(final_text.clone()).with_phase(Some(uni::AssistantPhase::FinalAnswer)));
    }
    if generated_fallback && ctx.harness_state.final_response_event_emitted() {
        // The harness already has the one allowed final assistant item. The
        // retained history may have been compacted after that emission, so
        // restore the deterministic handoff locally without duplicating the
        // canonical AgentMessage event.
        if !ctx.harness_state.final_response_rendered() {
            ctx.renderer.line(MessageStyle::Response, &final_text)?;
            ctx.harness_state.mark_final_response_rendered();
        }
    } else {
        let _ = publish_final_assistant_response(ctx, &final_text)?;
    }
    Ok(())
}

/// Ensure a completed turn has crossed both user-visible response surfaces.
/// Recovery helpers may already have appended a fallback to history; this
/// publishes that existing text instead of appending a second answer.
fn ensure_completed_turn_response(
    ctx: &mut TurnLoopContext<'_>,
    working_history: &mut Vec<uni::Message>,
    turn_history_start_len: usize,
) -> Result<bool> {
    let mut response_was_fallback = ctx.harness_state.final_response_was_fallback();
    let final_text = latest_final_assistant_response(working_history, turn_history_start_len);

    let final_text = if let Some(final_text) = final_text {
        final_text
    } else {
        response_was_fallback = true;
        let fallback = COMPLETED_TURN_FALLBACK_RESPONSE.to_string();
        working_history
            .push(uni::Message::assistant(fallback.clone()).with_phase(Some(uni::AssistantPhase::FinalAnswer)));
        fallback
    };

    if !ctx.harness_state.final_response_rendered() {
        response_was_fallback = true;
    }
    let _ = publish_final_assistant_response(ctx, &final_text)?;

    if response_was_fallback {
        ctx.harness_state.mark_final_response_fallback();
    }
    Ok(response_was_fallback)
}

fn completed_turn_requires_final_response(result: &TurnLoopResult) -> bool {
    matches!(result, TurnLoopResult::Completed { plan_approved_execution_pending: false })
}

pub(crate) struct TurnLoopOutcome {
    pub result: TurnLoopResult,
    pub turn_modified_files: BTreeSet<PathBuf>,
    pub turn_diagnostics: vtcode_core::core::agent::snapshots::SnapshotTurnDiagnostics,
    /// When set, the interaction loop should switch the active primary agent
    /// to this name after the turn completes.
    pub pending_primary_agent: Option<String>,
    /// Explicit auto-accept state from plan approval. This must survive an
    /// agent fallback from `plan` to `build`; inferring it from the agent name
    /// loses the user's confirmation choice.
    pub pending_plan_auto_accept: bool,
    pub pending_plan_execution_context: crate::agent::runloop::unified::planning_workflow::PlanExecutionContext,
    /// When true, the plan was approved via the inline confirmation overlay
    /// inside the turn loop and the session loop must push an execution
    /// directive before starting the next turn so the model begins
    /// implementation immediately.
    pub plan_approved_execution_pending: bool,
    /// Whether the turn's final response came from deterministic recovery
    /// fallback rather than a confirmed model synthesis.
    pub final_response_was_fallback: bool,
}

pub(crate) struct TurnLoopContext<'a> {
    pub renderer: &'a mut AnsiRenderer,
    pub handle: &'a InlineHandle,
    pub session: &'a mut InlineSession,
    pub session_stats: &'a mut crate::agent::runloop::unified::state::SessionStats,
    pub plan_session: &'a mut PlanningWorkflowSessionState,
    pub auto_finish_planning_attempted: &'a mut bool,
    pub mcp_panel_state: &'a mut mcp_events::McpPanelState,
    pub tool_result_cache: &'a Arc<RwLock<ToolResultCache>>,
    pub approval_recorder: &'a Arc<ApprovalRecorder>,
    pub decision_ledger: &'a Arc<RwLock<DecisionTracker>>,
    pub tool_registry: &'a mut ToolRegistry,
    pub tools: &'a Arc<RwLock<Vec<uni::ToolDefinition>>>,
    pub tool_catalog: &'a Arc<crate::agent::runloop::unified::tool_catalog::ToolCatalogState>,
    pub ctrl_c_state: &'a Arc<crate::agent::runloop::unified::state::CtrlCState>,
    pub ctrl_c_notify: &'a Arc<tokio::sync::Notify>,
    pub context_manager: &'a mut crate::agent::runloop::unified::context_manager::ContextManager,
    pub last_forced_redraw: &'a mut Instant,
    pub input_status_state: &'a mut crate::agent::runloop::unified::status_line::InputStatusState,
    pub lifecycle_hooks: Option<&'a LifecycleHookEngine>,
    pub default_placeholder: &'a Option<String>,
    pub tool_permission_cache: &'a Arc<RwLock<ToolPermissionCache>>,
    pub permissions_state: &'a Arc<RwLock<vtcode_core::config::PermissionsConfig>>,
    pub safety_validator: &'a Arc<ToolCallSafetyValidator>,
    pub circuit_breaker: &'a Arc<vtcode_core::tools::circuit_breaker::CircuitBreaker>,
    pub tool_health_tracker: &'a Arc<vtcode_core::tools::health::ToolHealthTracker>,
    pub rate_limiter: &'a Arc<vtcode_core::tools::adaptive_rate_limiter::AdaptiveRateLimiter>,
    pub telemetry: &'a Arc<vtcode_core::core::telemetry::TelemetryManager>,
    pub autonomous_executor: &'a Arc<vtcode_core::tools::autonomous_executor::AutonomousExecutor>,
    pub error_recovery: &'a Arc<RwLock<vtcode_core::core::agent::error_recovery::ErrorRecoveryState>>,
    pub harness_state: &'a mut HarnessTurnState,
    pub harness_emitter: Option<&'a HarnessEventEmitter>,
    pub config: &'a mut AgentConfig,
    pub vt_cfg: Option<&'a VTCodeConfig>,
    pub turn_metadata_cache: &'a mut Option<Option<serde_json::Value>>,
    pub provider_client: &'a mut Box<dyn uni::LLMProvider>,
    pub traj: &'a TrajectoryLogger,
    pub active_primary_agent: &'a ActivePrimaryAgentState,
    pub skip_confirmations: bool,
    pub full_auto: bool,
    pub runtime_steering: &'a mut RuntimeSteering,
}

impl<'a> TurnLoopContext<'a> {
    #[expect(
        clippy::too_many_arguments,
        reason = "Intentional compatibility, platform, test, or API-shape suppression."
    )]
    pub(crate) fn new(
        renderer: &'a mut AnsiRenderer,
        handle: &'a InlineHandle,
        session: &'a mut InlineSession,
        session_stats: &'a mut crate::agent::runloop::unified::state::SessionStats,
        plan_session: &'a mut PlanningWorkflowSessionState,
        auto_finish_planning_attempted: &'a mut bool,
        mcp_panel_state: &'a mut mcp_events::McpPanelState,
        tool_result_cache: &'a Arc<RwLock<ToolResultCache>>,
        approval_recorder: &'a Arc<ApprovalRecorder>,
        decision_ledger: &'a Arc<RwLock<DecisionTracker>>,
        tool_registry: &'a mut ToolRegistry,
        tools: &'a Arc<RwLock<Vec<uni::ToolDefinition>>>,
        tool_catalog: &'a Arc<crate::agent::runloop::unified::tool_catalog::ToolCatalogState>,
        ctrl_c_state: &'a Arc<crate::agent::runloop::unified::state::CtrlCState>,
        ctrl_c_notify: &'a Arc<tokio::sync::Notify>,
        context_manager: &'a mut crate::agent::runloop::unified::context_manager::ContextManager,
        last_forced_redraw: &'a mut Instant,
        input_status_state: &'a mut crate::agent::runloop::unified::status_line::InputStatusState,
        lifecycle_hooks: Option<&'a LifecycleHookEngine>,
        default_placeholder: &'a Option<String>,
        tool_permission_cache: &'a Arc<RwLock<ToolPermissionCache>>,
        permissions_state: &'a Arc<RwLock<vtcode_core::config::PermissionsConfig>>,
        safety_validator: &'a Arc<ToolCallSafetyValidator>,
        circuit_breaker: &'a Arc<vtcode_core::tools::circuit_breaker::CircuitBreaker>,
        tool_health_tracker: &'a Arc<vtcode_core::tools::health::ToolHealthTracker>,
        rate_limiter: &'a Arc<vtcode_core::tools::adaptive_rate_limiter::AdaptiveRateLimiter>,
        telemetry: &'a Arc<vtcode_core::core::telemetry::TelemetryManager>,
        autonomous_executor: &'a Arc<vtcode_core::tools::autonomous_executor::AutonomousExecutor>,
        error_recovery: &'a Arc<RwLock<vtcode_core::core::agent::error_recovery::ErrorRecoveryState>>,
        harness_state: &'a mut HarnessTurnState,
        harness_emitter: Option<&'a HarnessEventEmitter>,
        config: &'a mut AgentConfig,
        vt_cfg: Option<&'a VTCodeConfig>,
        turn_metadata_cache: &'a mut Option<Option<serde_json::Value>>,
        provider_client: &'a mut Box<dyn uni::LLMProvider>,
        traj: &'a TrajectoryLogger,
        active_primary_agent: &'a ActivePrimaryAgentState,
        skip_confirmations: bool,
        full_auto: bool,
        runtime_steering: &'a mut RuntimeSteering,
    ) -> Self {
        Self {
            renderer,
            handle,
            session,
            session_stats,
            plan_session,
            auto_finish_planning_attempted,
            mcp_panel_state,
            tool_result_cache,
            approval_recorder,
            decision_ledger,
            tool_registry,
            tools,
            tool_catalog,
            ctrl_c_state,
            ctrl_c_notify,
            context_manager,
            last_forced_redraw,
            input_status_state,
            lifecycle_hooks,
            default_placeholder,
            tool_permission_cache,
            permissions_state,
            safety_validator,
            circuit_breaker,
            tool_health_tracker,
            rate_limiter,
            telemetry,
            autonomous_executor,
            error_recovery,
            harness_state,
            harness_emitter,
            config,
            vt_cfg,
            turn_metadata_cache,
            provider_client,
            traj,
            active_primary_agent,
            skip_confirmations,
            full_auto,
            runtime_steering,
        }
    }

    pub(crate) fn as_run_loop_context(&mut self) -> RunLoopContext<'_> {
        let auto_permission = Some(crate::agent::runloop::unified::run_loop_context::AutoPermissionRuntimeContext {
            config: self.config,
            vt_cfg: self.vt_cfg,
            provider_client: self.provider_client.as_mut(),
            working_history: &[],
        });

        let mut ctx = RunLoopContext::new_with_auto_permission_context(
            self.renderer,
            self.handle,
            self.tool_registry,
            self.tools,
            self.tool_result_cache,
            self.tool_permission_cache,
            self.permissions_state,
            self.decision_ledger,
            self.session_stats,
            self.plan_session,
            self.mcp_panel_state,
            self.approval_recorder,
            self.session,
            Some(self.safety_validator),
            self.traj,
            self.harness_state,
            self.harness_emitter,
            auto_permission,
            self.skip_confirmations,
            self.full_auto,
        );
        ctx.active_agent_permissions = self
            .vt_cfg
            .and_then(|cfg| cfg.runtime_agent_permissions.as_ref())
            .or(Some(&self.active_primary_agent.active().permissions));
        ctx.agent_name = Some(self.active_primary_agent.active().identity.name.clone());
        ctx.default_primary_agent = self.vt_cfg.map(|cfg| cfg.default_primary_agent.clone());
        // The primary agent loop is always for the primary agent, not a subagent
        ctx.is_subagent = false;
        ctx
    }

    pub(crate) fn as_turn_processing_context<'b>(
        &'b mut self,
        working_history: &'b mut Vec<uni::Message>,
    ) -> crate::agent::runloop::unified::turn::context::TurnProcessingContext<'b> {
        let tool = crate::agent::runloop::unified::turn::context::ToolContext {
            tool_result_cache: self.tool_result_cache,
            approval_recorder: self.approval_recorder,
            tool_registry: self.tool_registry,
            tools: self.tools,
            tool_catalog: self.tool_catalog,
            tool_permission_cache: self.tool_permission_cache,
            permissions_state: self.permissions_state,
            safety_validator: self.safety_validator,
            circuit_breaker: self.circuit_breaker,
            tool_health_tracker: self.tool_health_tracker,
            rate_limiter: self.rate_limiter,
            telemetry: self.telemetry,
            autonomous_executor: self.autonomous_executor,
            error_recovery: self.error_recovery,
        };
        let llm = crate::agent::runloop::unified::turn::context::LLMContext {
            provider_client: self.provider_client,
            config: self.config,
            vt_cfg: self.vt_cfg,
            context_manager: self.context_manager,
            active_primary_agent: self.active_primary_agent,
            decision_ledger: self.decision_ledger,
            traj: self.traj,
        };
        let ui = crate::agent::runloop::unified::turn::context::UIContext {
            renderer: self.renderer,
            handle: self.handle,
            session: self.session,
            active_thread_label: "main",
            ctrl_c_state: self.ctrl_c_state,
            ctrl_c_notify: self.ctrl_c_notify,
            lifecycle_hooks: self.lifecycle_hooks,
            default_placeholder: self.default_placeholder,
            last_forced_redraw: self.last_forced_redraw,
            input_status_state: self.input_status_state,
        };
        let state = crate::agent::runloop::unified::turn::context::TurnProcessingState {
            session_stats: self.session_stats,
            plan_session: self.plan_session,
            auto_finish_planning_attempted: self.auto_finish_planning_attempted,
            mcp_panel_state: self.mcp_panel_state,
            working_history,
            turn_metadata_cache: self.turn_metadata_cache,
            skip_confirmations: self.skip_confirmations,
            full_auto: self.full_auto,
            harness_state: self.harness_state,
            harness_emitter: self.harness_emitter,
            runtime_steering: self.runtime_steering,
        };

        crate::agent::runloop::unified::turn::context::TurnProcessingContext::from_parts(
            crate::agent::runloop::unified::turn::context::TurnProcessingContextParts { tool, llm, ui, state },
        )
    }

    pub(crate) fn is_planning_active(&self) -> bool {
        self.tool_registry.is_planning_active()
    }

    pub(crate) fn set_phase(&mut self, phase: TurnPhase) {
        self.harness_state.set_phase(phase);
    }
}

pub(crate) const POST_TOOL_RESUME_DIRECTIVE: &str = "Previous turn already completed tool execution. Reuse the latest tool outputs in history instead of rerunning the same exploration. If those tool outputs include `critical_note`, `hint`, `next_action`, `fallback_tool`, `fallback_tool_args`, or `rerun_hint`, follow that guidance first. Do NOT re-read files that were already read in the previous turn — their content is in the conversation history above. Synthesize a plan or answer from what is already gathered.";
pub(crate) const POST_TOOL_TOOL_ENABLED_RETRY_DIRECTIVE: &str = "The previous model follow-up failed after tool execution. The older context will be compacted before this retry. Reuse the completed tool outputs above, do not repeat read-only exploration, and continue the user's request with any required write or verification tools. Only finish after the requested work is confirmed; do not claim success from an unverified plan.";

// For `TurnLoopContext`, we will reuse the generic `handle_pipeline_output` via an adapter below.

pub(crate) async fn run_turn_loop(
    working_history: &mut Vec<uni::Message>,
    mut ctx: TurnLoopContext<'_>,
) -> Result<TurnLoopOutcome> {
    use crate::agent::runloop::unified::turn::context::{TurnHandlerOutcome, TurnProcessingResult};
    use crate::agent::runloop::unified::turn::guards::run_proactive_guards;
    use crate::agent::runloop::unified::turn::turn_processing::{
        HandleTurnProcessingResultParams, execute_llm_request, handle_turn_processing_result,
        maybe_force_planning_workflow_interview, process_llm_response, resolve_effective_request_model,
    };

    // Initialize the outcome result
    let mut result = TurnLoopResult::Completed { plan_approved_execution_pending: false };
    let mut turn_modified_files = BTreeSet::new();
    let mut pending_primary_agent: Option<String> = None;
    let mut pending_plan_auto_accept = false;
    let mut pending_plan_execution_context =
        crate::agent::runloop::unified::planning_workflow::PlanExecutionContext::Current;
    *ctx.auto_finish_planning_attempted = false;

    // Compact command rows are a presentation-only active tail. A new turn
    // must start a fresh contiguous group even when the renderer is reused by
    // the session loop.
    ctx.renderer.flush_compact_command_group();
    ctx.set_phase(TurnPhase::Preparing);
    ctx.tool_registry.begin_turn_preview_window();
    if let Some(Err(e)) = ctx.harness_emitter.map(|e| e.emit(turn_started_event())) {
        tracing::debug!(error = %e, "harness turn_started event emission failed");
    }

    // Optimization: Extract all frequently accessed config values once
    let mut turn_config = extract_turn_config(ctx.vt_cfg, ctx.is_planning_active(), ctx.renderer.supports_inline_ui());
    if ctx.is_planning_active() {
        ctx.plan_session.start_turn();
    }
    // After a permanent `request_user_input` denial, suppress the tool for the
    // rest of the session so the model stops retrying it across turns.
    if ctx.plan_session.is_interview_denied() {
        turn_config.request_user_input_enabled = false;
    }

    let mut step_count = 0;
    let mut current_max_tool_loops =
        initial_tool_loop_limit(turn_config.max_tool_loops, ctx.harness_state.is_approved_plan_execution());
    let mut turn_history_start_len = working_history.len();
    // Bounded condense-retry counter for truncated planning syntheses (see
    // the re-prompt block after generation). Prevents an oversized plan from
    // looping the turn.
    let mut plan_condense_attempts: u8 = 0;
    let mut turn_usage = HarnessUsage::default();
    // Optimization: Interned signatures with exponential backoff for loop detection
    let mut repeated_tool_attempts = LoopTracker::with_verification_snapshot(ctx.session_stats.verification_snapshot());
    if repeated_tool_attempts.verification_is_pending() {
        repeated_tool_attempts.verification_warning_emitted = true;
        let has_recent_verification_directive = working_history.iter().rev().take(8).any(|message| {
            message.role == uni::MessageRole::System && message.content.as_text().contains(ANTI_BLIND_EDITING_DIRECTIVE)
        });
        if !has_recent_verification_directive {
            working_history.push(uni::Message::system(ANTI_BLIND_EDITING_DIRECTIVE.to_string()));
        }
    }

    // Reset per-turn counters while preserving only the explicit session
    // headroom granted by the user. The validator reapplies that fuse inside
    // `set_limits`, while ordinary runtime config changes (including a lower
    // configured limit) remain authoritative when no grant exists.
    {
        let (max_per_turn, max_per_session) = resolve_safety_tool_call_limits(
            ctx.harness_state.max_tool_calls,
            turn_config.max_session_turns,
            ctx.is_planning_active(),
        );
        ctx.safety_validator.set_limits(max_per_turn, max_per_session);
        ctx.safety_validator.start_turn();
    }
    // Tracks whether planning-aware budgets are already in effect. When
    // planning is entered mid-turn (via the enter trigger below), the limits
    // computed above used `planning_active = false` and must be re-applied so
    // the planning research floor (120 calls/turn) takes effect immediately
    // instead of exhausting the smaller build-mode budget (checkpoint turn_804).
    let mut planning_limits_applied = ctx.is_planning_active();

    loop {
        if handle_steering_messages(&mut ctx, working_history, &mut result).await? {
            break;
        }

        // A permanent interview denial can happen after this turn's initial
        // config snapshot (for example when the model's plan response
        // triggers the inline interview). Keep the parser and request builder
        // on the same capability set immediately; otherwise this turn can
        // still inject another `request_user_input` call even though the
        // catalog has already removed it.
        if ctx.plan_session.is_interview_denied() {
            turn_config.request_user_input_enabled = false;
        }

        step_count += 1;
        ctx.telemetry.record_turn();

        if maybe_handle_planning_enter_trigger(&mut ctx, working_history, step_count, &mut result).await? {
            break;
        }

        // Planning entered mid-turn: re-derive turn config and budgets with
        // `planning_active = true` so research isn't capped by the smaller
        // build-mode limits that were computed at turn start.
        if !planning_limits_applied && ctx.is_planning_active() {
            planning_limits_applied = true;
            ctx.plan_session.start_turn();
            turn_config = extract_turn_config(ctx.vt_cfg, true, ctx.renderer.supports_inline_ui());
            if ctx.plan_session.is_interview_denied() {
                turn_config.request_user_input_enabled = false;
            }
            current_max_tool_loops = current_max_tool_loops.max(turn_config.max_tool_loops);
            ctx.harness_state.max_tool_calls =
                super::turn_loop_helpers::effective_max_tool_calls_for_turn(ctx.harness_state.max_tool_calls, true);
            let (max_per_turn, max_per_session) =
                resolve_safety_tool_call_limits(ctx.harness_state.max_tool_calls, turn_config.max_session_turns, true);
            ctx.safety_validator.set_limits(max_per_turn, max_per_session);
        }

        let transition = maybe_handle_planning_exit_trigger(
            ctx.renderer,
            ctx.tool_registry,
            ctx.plan_session,
            ctx.handle,
            working_history,
            ctx.auto_finish_planning_attempted,
            crate::agent::runloop::unified::planning_workflow::PlanningExitContext {
                active_agent_name: ctx.active_primary_agent.active().name(),
                session: ctx.session,
                ctrl_c_state: ctx.ctrl_c_state,
                ctrl_c_notify: ctx.ctrl_c_notify,
                vt_cfg: ctx.vt_cfg,
                skip_confirmations: ctx.skip_confirmations,
                full_auto: ctx.full_auto,
                context_usage_percent: ctx.context_manager.context_usage_percent(
                    vtcode_core::compaction::effective_context_budget(
                        ctx.vt_cfg,
                        ctx.provider_client.as_ref(),
                        &resolve_effective_request_model(&ctx.config.model, ctx.active_primary_agent.active()),
                    ),
                ),
                telemetry: crate::agent::runloop::unified::planning_workflow::PlanApprovalTelemetryContext {
                    emitter: ctx.harness_emitter,
                    thread_id: &ctx.harness_state.run_id.0,
                    turn_id: &ctx.harness_state.turn_id.0,
                },
            },
        )
        .await?;

        if transition.should_break() {
            let (loop_result, agent, auto_accept, execution_context) = transition.into_result_and_agent();
            result = loop_result;
            pending_primary_agent = agent;
            pending_plan_auto_accept = auto_accept;
            pending_plan_execution_context = execution_context;
            break;
        }

        match maybe_handle_tool_loop_limit(&mut ctx, step_count, &mut current_max_tool_loops).await? {
            ToolLoopLimitAction::Proceed => {}
            ToolLoopLimitAction::ContinueLoop => continue,
            ToolLoopLimitAction::BreakLoop => {
                break;
            }
        }

        let active_model = resolve_effective_request_model(&ctx.config.model, ctx.active_primary_agent.active());
        let harness_snapshot = ctx.tool_registry.harness_context_snapshot();
        let steering_update = vtcode_core::compaction::memory_envelope::SessionMemoryEnvelopeUpdate {
            pending_intents: Some(ctx.runtime_steering.pending_follow_up_intents_snapshot()),
            applied_intent_ids: ctx.runtime_steering.applied_follow_up_intent_ids().iter().cloned().collect(),
            ..Default::default()
        };
        let recovery_compaction_requested = ctx.harness_state.take_post_tool_compaction_pending();
        if recovery_compaction_requested {
            let context_capacity_failure = ctx.harness_state.post_tool_context_capacity_failure();
            match compact_before_tool_enabled_retry(RecoveryCompactionRequest {
                history: working_history,
                turn_history_start_len: &mut turn_history_start_len,
                compaction_context: crate::agent::runloop::unified::turn::compaction::CompactionContext::new(
                    ctx.provider_client.as_ref(),
                    &active_model,
                    &harness_snapshot.session_id,
                    &ctx.harness_state.run_id.0,
                    &ctx.config.workspace,
                    ctx.vt_cfg,
                    ctx.lifecycle_hooks,
                    ctx.harness_emitter,
                ),
                session_stats: ctx.session_stats,
                context_manager: ctx.context_manager,
                steering_update,
            })
            .await
            {
                Ok(true) => {}
                Ok(false) if context_capacity_failure => {
                    ctx.harness_state.mark_post_tool_context_compaction_failed();
                    ensure_post_tool_resume_directive(working_history);
                    result = TurnLoopResult::Blocked {
                        reason: Some(POST_TOOL_CONTEXT_COMPACTION_FAILED_REASON.to_string()),
                    };
                    break;
                }
                Ok(false) => {}
                Err(err) => {
                    if context_capacity_failure {
                        ctx.harness_state.mark_post_tool_context_compaction_failed();
                        ensure_post_tool_resume_directive(working_history);
                        result = TurnLoopResult::Blocked {
                            reason: Some(POST_TOOL_CONTEXT_COMPACTION_FAILED_REASON.to_string()),
                        };
                        break;
                    }
                    tracing::warn!(
                        error = %err,
                        "Post-tool recovery compaction failed; preserving the existing history for the bounded retry"
                    );
                }
            }
        } else {
            match crate::agent::runloop::unified::turn::compaction::maybe_auto_compact_history(
                crate::agent::runloop::unified::turn::compaction::CompactionContext::new(
                    ctx.provider_client.as_ref(),
                    &active_model,
                    &harness_snapshot.session_id,
                    &ctx.harness_state.run_id.0,
                    &ctx.config.workspace,
                    ctx.vt_cfg,
                    ctx.lifecycle_hooks,
                    ctx.harness_emitter,
                ),
                crate::agent::runloop::unified::turn::compaction::CompactionState::new(
                    working_history,
                    ctx.session_stats,
                    ctx.context_manager,
                )
                .with_steering_update(steering_update),
            )
            .await
            {
                Ok(Some(outcome)) => {
                    turn_history_start_len = outcome.compacted_len;
                    tracing::info!(
                        original_len = outcome.original_len,
                        compacted_len = outcome.compacted_len,
                        turn_history_start_len,
                        "Applied local fallback compaction before the next turn request"
                    );
                }
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!(error = %err, "Local fallback compaction failed");
                }
            }
        }

        // Clone validation cache arc to avoid borrow conflict
        let validation_cache = ctx.session_stats.validation_cache.clone();

        // Capture input status state for potential restoration after LLM response
        // (needed because turn_processing_ctx will mutably borrow input_status_state)
        let restore_status_left = ctx.input_status_state.left.clone();
        let restore_status_right = ctx.input_status_state.right.clone();

        // Anti-runaway guard: if the model has already emitted
        // MAX_ASSISTANT_TEXT_RESPONSES_PER_TURN consecutive text-only
        // responses without admitted tool progress, the recovery / continuation
        // loop is regenerating the same answer. Conclude the turn with the
        // last response stored in working_history and emit a warning so users
        // can see what happened. See MAX_ASSISTANT_TEXT_RESPONSES_PER_TURN for
        // the observed failure mode (checkpoint turn_594: 4 identical
        // 6500-token outline responses, ~90s of wasted context).
        // Harness state is authoritative. History cannot represent Copilot's
        // inline VTCode tool calls, while compaction does not discard this
        // turn-scoped counter.
        let text_response_streak = ctx.harness_state.consecutive_assistant_text_responses;
        // A pending validation repair is a deliberate extra candidate, so
        // allow the next request through the generic text-response cap. This
        // allowance is queued independently of the response count because a
        // repair can be scheduled after an ordinary planning response.
        let plan_validation_repair_follow_up =
            ctx.is_planning_active() && ctx.plan_session.plan_validation_repair_follow_up_allowed();
        if text_response_streak >= MAX_ASSISTANT_TEXT_RESPONSES_PER_TURN && !plan_validation_repair_follow_up {
            tracing::warn!(
                text_response_streak,
                cap = MAX_ASSISTANT_TEXT_RESPONSES_PER_TURN,
                "Assistant text-response cap reached; ending turn to prevent runaway regeneration loop"
            );
            let _ = ctx.renderer.line(
                MessageStyle::Warning,
                "Recovery loop detected: capping repeated assistant responses to avoid wasted context.",
            );
            if promote_latest_commentary_to_final(working_history, turn_history_start_len) {
                // Commentary has already crossed the renderer surface. Avoid
                // rendering it a second time while still allowing the normal
                // blocked-turn finalizer to publish the canonical event.
                ctx.harness_state.mark_final_response_rendered();
                if ctx.harness_state.streamed_response_event_emitted() {
                    // The streaming lifecycle already emitted the assistant
                    // item; publishing another AgentMessage would duplicate it.
                    ctx.harness_state.mark_final_response_event_emitted();
                }
            }
            result = TurnLoopResult::Blocked {
                reason: Some(ASSISTANT_TEXT_RESPONSE_CAP_REASON.to_string()),
            };
            break;
        }
        if plan_validation_repair_follow_up {
            ctx.plan_session.consume_plan_validation_repair_follow_up();
        }

        // Prepare turn processing context
        let mut turn_processing_ctx = ctx.as_turn_processing_context(working_history);

        // === PROACTIVE GUARDS (HP-2: Pre-request checks) ===
        run_proactive_guards(&mut turn_processing_ctx, step_count).await?;

        // Execute the LLM request
        turn_processing_ctx.set_phase(TurnPhase::Requesting);
        let active_model = resolve_effective_request_model(
            &turn_processing_ctx.config.model,
            turn_processing_ctx.active_primary_agent.active(),
        );
        let recovery_pass = turn_processing_ctx.consume_recovery_pass();

        let tool_free_recovery = recovery_pass && turn_processing_ctx.recovery_is_tool_free();

        // Cache-gap advisory (Phase E1): warn once per gap when the user
        // paused long enough for the provider prompt cache to have expired,
        // so this request may unexpectedly re-pay full input cost.
        let cache_gap_provider_name = turn_processing_ctx.config.provider.clone();
        if let Some(threshold) = turn_processing_ctx
            .vt_cfg
            .and_then(|cfg| cfg.prompt_cache.gap_threshold_secs(&cache_gap_provider_name))
        {
            let threshold = Duration::from_secs(threshold);
            if turn_processing_ctx.session_stats.total_usage().cached_input_tokens > 0
                && let Some(elapsed) = turn_processing_ctx.session_stats.cache_gap_exceeds(threshold)
            {
                let _ = turn_processing_ctx.renderer.line(
                    MessageStyle::Info,
                    &format!(
                        "~{} since the last request; the provider prompt cache has likely expired, so this request may re-pay full input cost.",
                        vtcode_core::llm::request_gap::format_gap(elapsed)
                    ),
                );
            }
        }
        turn_processing_ctx.session_stats.note_request_sent();

        let suppress_unverified_output = repeated_tool_attempts.verification_is_pending();
        let request_result = if suppress_unverified_output
            || turn_processing_ctx
                .config
                .provider
                .eq_ignore_ascii_case(vtcode_core::copilot::COPILOT_PROVIDER_KEY)
        {
            turn_processing_ctx
                .execute_llm_request_with_options(
                    step_count,
                    &active_model,
                    tool_free_recovery.then_some(RECOVERY_SYNTHESIS_MAX_TOKENS),
                    tool_free_recovery,
                    None, // parallel_cfg_opt
                    suppress_unverified_output,
                    Some(&mut repeated_tool_attempts),
                )
                .await
        } else {
            execute_llm_request(
                &mut turn_processing_ctx,
                step_count,
                &active_model,
                tool_free_recovery.then_some(RECOVERY_SYNTHESIS_MAX_TOKENS),
                tool_free_recovery,
                None, // parallel_cfg_opt
            )
            .await
        };
        let (response, response_streamed) = match request_result {
            Ok(val) => val,
            Err(err) => {
                // Record the error in the recovery state for diagnostics
                turn_processing_ctx
                    .record_recovery_error("llm_request", &err, ErrorType::Other)
                    .await;

                // execute_llm_request already performs retry/backoff for retryable provider errors.
                // Avoid a second retry layer here, which can consume turn budget and cause timeouts.
                // Restore input status on request failure to clear loading/shimmer state.
                turn_processing_ctx.restore_input_status(restore_status_left.clone(), restore_status_right.clone());

                let planning = turn_processing_ctx.is_planning_active();
                match dispatch_post_tool_failure(PostToolRecoveryContext {
                    renderer: &mut *turn_processing_ctx.renderer,
                    working_history: &mut *turn_processing_ctx.working_history,
                    harness_state: &mut *turn_processing_ctx.harness_state,
                    harness_emitter: turn_processing_ctx.harness_emitter,
                    plan_session: planning.then_some(&mut *turn_processing_ctx.plan_session),
                    plan_state: planning.then_some(&turn_processing_ctx.tool_registry.planning_workflow_state()),
                    err: &err,
                    step_count,
                    turn_history_start_len,
                    stage: "execute_llm_request",
                    tool_free_recovery,
                })
                .await?
                {
                    PostToolFailureAction::Continue => continue,
                    PostToolFailureAction::Break(r) => {
                        result = r;
                        break;
                    }
                    PostToolFailureAction::Fallthrough => {}
                }

                display_error(turn_processing_ctx.renderer, "LLM request failed", &err)?;
                // Show recovery hints derived from the canonical error category
                {
                    let err_cat = vtcode_commons::classify_anyhow_error(&err);
                    if matches!(err_cat, vtcode_commons::ErrorCategory::Authentication) {
                        // For auth errors, show actionable provider-specific guidance
                        // that distinguishes "no key stored" from "stored key rejected".
                        let (provider_label, provider_key, is_managed_auth, has_stored_credential) =
                            turn_processing_ctx
                                .config
                                .provider
                                .parse::<vtcode_core::config::models::Provider>()
                                .map(|p| {
                                    let storage_mode = turn_processing_ctx
                                        .vt_cfg
                                        .map(|cfg| cfg.agent.credential_storage_mode)
                                        .unwrap_or_default();
                                    let has =
                                        vtcode_config::api_keys::provider_credential_detail_with_mode(p, storage_mode)
                                            .is_some();
                                    (p.label().to_string(), p.as_ref().to_string(), p.uses_managed_auth(), has)
                                })
                                .unwrap_or_else(|_| {
                                    (
                                        turn_processing_ctx.config.provider.clone(),
                                        turn_processing_ctx.config.provider.clone(),
                                        false,
                                        false,
                                    )
                                });
                        let guidance = err_cat.auth_recovery_guidance(
                            &provider_label,
                            &provider_key,
                            is_managed_auth,
                            has_stored_credential,
                        );
                        for line in &guidance {
                            turn_processing_ctx.renderer.line(MessageStyle::Info, line)?;
                        }
                    } else {
                        let suggestions = err_cat.recovery_suggestions();
                        if !suggestions.is_empty() {
                            let hint = suggestions.join("; ");
                            turn_processing_ctx
                                .renderer
                                .line(MessageStyle::Info, &format!("Hint: {hint}"))?;
                        }
                    }
                }
                // Log error via tracing instead of polluting conversation history
                // Adding error messages as assistant content can poison future turns
                let error_message = error_message_for_user(&err);
                tracing::error!(error = %error_message, step = step_count, "LLM request failed");
                // Do NOT add error message to working_history - this prevents the model
                // from learning spurious error patterns and keeps the conversation clean
                result = TurnLoopResult::Aborted;
                break;
            }
        };

        // Track turn usage and context pressure before later processing borrows `response`.
        let response_usage = response.usage.clone();
        let provider_name = turn_processing_ctx.config.provider.clone();
        accumulate_turn_usage(&provider_name, &mut turn_usage, &response_usage);
        turn_processing_ctx.session_stats.record_usage(&provider_name, &response_usage);
        turn_processing_ctx
            .session_stats
            .set_stop_reason(Some(stop_reason_from_finish_reason(&response.finish_reason)));
        let max_budget_usd = turn_processing_ctx.vt_cfg.and_then(|cfg| cfg.agent.harness.max_budget_usd);
        let total_usage = turn_processing_ctx.session_stats.total_usage();
        match estimate_session_costs(&provider_name, &active_model, &total_usage) {
            Some(estimate) => {
                turn_processing_ctx.session_stats.set_total_cost_usd(Some(estimate.raw_usd));
                let threshold = turn_processing_ctx
                    .vt_cfg
                    .map(|cfg| cfg.agent.harness.budget_warning_threshold)
                    .unwrap_or(vtcode_core::llm::usage_cost::DEFAULT_BUDGET_WARNING_RATIO);
                match vtcode_core::llm::usage_cost::BudgetStatus::classify(estimate.raw_usd, max_budget_usd, threshold)
                {
                    vtcode_core::llm::usage_cost::BudgetStatus::Exceeded { max, .. } => {
                        turn_processing_ctx
                            .session_stats
                            .mark_budget_limit_reached(max, estimate.raw_usd);
                        turn_processing_ctx.context_manager.update_token_usage(&response_usage);
                        #[cfg(debug_assertions)]
                        turn_processing_ctx.context_manager.validate_token_tracking(&response_usage);
                        // In planning mode, preserve the session and use the same
                        // user-facing recovery fallback as post-tool failures.
                        // Returning a model-only directive here would leave no
                        // synthesis call to consume it and would dead-end the turn.
                        if turn_processing_ctx.is_planning_active() {
                            turn_processing_ctx.plan_session.mark_budget_exhausted();
                            let plan_state = turn_processing_ctx.tool_registry.planning_workflow_state();
                            let event_thread_id = turn_processing_ctx.harness_state.run_id.0.clone();
                            let event_turn_id = turn_processing_ctx.harness_state.turn_id.0.clone();
                            let event_context = PlanRecoveryEventContext {
                                emitter: turn_processing_ctx.harness_emitter,
                                thread_id: &event_thread_id,
                                turn_id: &event_turn_id,
                            };
                            result = post_tool_recovery::complete_turn_after_failed_tool_free_recovery_with_events(
                                turn_processing_ctx.working_history,
                                "turn_loop.budget_limit_planning",
                                None,
                                None,
                                Some(turn_processing_ctx.plan_session),
                                Some(&plan_state),
                                Some(event_context),
                            )
                            .await;
                            let _ = turn_processing_ctx.renderer.line(
                                MessageStyle::Warning,
                                "Budget exhausted during planning workflow. The plan draft is preserved for approval or revision.",
                            );
                        } else {
                            result = TurnLoopResult::Blocked {
                                reason: Some(format!(
                                    "Stopped after reaching budget limit (max: ${max:.4}, spent: ${:.4}, cache-adjusted: ${:.4}).",
                                    estimate.raw_usd, estimate.effective_usd
                                )),
                            };
                        }
                        break;
                    }
                    vtcode_core::llm::usage_cost::BudgetStatus::Warning { max, .. }
                        if !turn_processing_ctx.session_stats.budget_warning_emitted() =>
                    {
                        turn_processing_ctx.session_stats.mark_budget_warning_emitted();
                        let _ = turn_processing_ctx.renderer.line(
                            MessageStyle::Info,
                            &format!(
                                "Session cost ${:.4} has reached {:.0}% of the ${max:.2} budget. {}",
                                estimate.raw_usd,
                                threshold * 100.0,
                                total_usage.cache_summary(),
                            ),
                        );
                    }
                    _ => {}
                }
            }
            None => {
                turn_processing_ctx.session_stats.set_total_cost_usd(None);
                if max_budget_usd.is_some() && !turn_processing_ctx.session_stats.cost_warning_emitted() {
                    turn_processing_ctx.session_stats.mark_cost_warning_emitted();
                    tracing::warn!(
                        provider = %provider_name,
                        model = %active_model,
                        "Budget enforcement disabled because pricing metadata is unavailable"
                    );
                    let _ = turn_processing_ctx.renderer.line(
                        MessageStyle::Info,
                        "Budget limit is not enforced for this model because pricing metadata is unavailable.",
                    );
                }
            }
        }
        if !response.tool_references.is_empty() {
            turn_processing_ctx
                .tool_catalog
                .note_tool_references(turn_processing_ctx.tools, &response.tool_references)
                .await;
        }

        {
            if turn_processing_ctx.is_planning_active() {
                turn_processing_ctx.plan_session.increment_turns();
            }
        }

        // Plan-mode robustness: if the planning synthesis was truncated at the
        // model's output token limit, the emitted plan is incomplete ("cut off
        // mid-flight"). Rather than accept a partial plan or re-enter the
        // recovery path (which previously looped forever), ask for a tighter
        // spec and retry once. Bounded so a genuinely oversized plan cannot
        // loop. The policy lives in the planning-workflow facade so this loop
        // stays free of plan-mode specifics.
        let planning_active = turn_processing_ctx.is_planning_active();
        if crate::agent::runloop::unified::planning_workflow::maybe_condense_truncated_plan(
            &mut *turn_processing_ctx.working_history,
            &mut *turn_processing_ctx.renderer,
            planning_active,
            tool_free_recovery,
            &mut plan_condense_attempts,
            &response,
        ) {
            continue;
        }

        // Process the LLM response
        let processing_result_outcome = {
            let allow_plan_interview = turn_processing_ctx.is_planning_active()
                && turn_config.request_user_input_enabled
                && crate::agent::runloop::unified::turn::turn_processing::planning_workflow_interview_ready(
                    turn_processing_ctx.session_stats,
                    turn_processing_ctx.plan_session,
                );
            process_llm_response(
                &response,
                turn_processing_ctx.renderer,
                turn_processing_ctx.working_history.len(),
                turn_processing_ctx.is_planning_active(),
                allow_plan_interview,
                turn_config.request_user_input_enabled,
                !tool_free_recovery,
                Some(&validation_cache),
                Some(turn_processing_ctx.tool_registry),
            )
        };
        let mut processing_result = match processing_result_outcome {
            Ok(result) => result,
            Err(err) => {
                let err_cat = vtcode_commons::classify_anyhow_error(&err);
                if err_cat.is_retryable() {
                    tracing::warn!(
                        error = %err,
                        step = step_count,
                        category = ?err_cat,
                        "Response parse failed with transient error; skipping extra request retry"
                    );
                }

                {
                    let mut recovery = turn_processing_ctx.error_recovery.write().await;
                    recovery.record_error("llm_response_parse", format!("{err:#}"), ErrorType::Other);
                }
                let tool_free_recovery =
                    turn_processing_ctx.recovery_pass_used() && turn_processing_ctx.recovery_is_tool_free();
                let planning = turn_processing_ctx.is_planning_active();

                // Restore the input status/UI before dispatching recovery handling
                // so the bottom-line is not left in a loading state if recovery
                // ultimately ends the turn or returns an error.
                turn_processing_ctx.restore_input_status(restore_status_left.clone(), restore_status_right.clone());

                match dispatch_post_tool_failure(PostToolRecoveryContext {
                    renderer: &mut *turn_processing_ctx.renderer,
                    working_history: &mut *turn_processing_ctx.working_history,
                    harness_state: &mut *turn_processing_ctx.harness_state,
                    harness_emitter: turn_processing_ctx.harness_emitter,
                    plan_session: planning.then_some(&mut *turn_processing_ctx.plan_session),
                    plan_state: planning.then_some(&turn_processing_ctx.tool_registry.planning_workflow_state()),
                    err: &err,
                    step_count,
                    turn_history_start_len,
                    stage: "process_llm_response",
                    tool_free_recovery,
                })
                .await?
                {
                    PostToolFailureAction::Continue => continue,
                    PostToolFailureAction::Break(r) => {
                        result = r;
                        break;
                    }
                    PostToolFailureAction::Fallthrough => {}
                }
                return Err(err);
            }
        };
        // When in tool-free recovery and the model returns no text (e.g. producing
        // tool calls that get discarded), retry with a more explicit directive
        // rather than immediately falling back to the deterministic final answer.
        if tool_free_recovery
            && !turn_processing_ctx.is_planning_active()
            && matches!(processing_result, TurnProcessingResult::Empty)
            && response.tool_calls.as_ref().is_some_and(|tc| !tc.is_empty())
            && turn_processing_ctx.recovery_retry_count() < MAX_RECOVERY_RETRIES
        {
            let directive = if turn_processing_ctx.is_planning_active() {
                RECOVERY_TOOL_CALL_RETRY_DIRECTIVE_PLAN_MODE
            } else {
                RECOVERY_TOOL_CALL_RETRY_DIRECTIVE
            };
            turn_processing_ctx
                .working_history
                .push(uni::Message::system(directive.to_string()));
            turn_processing_ctx.retry_recovery_pass();
            continue;
        }
        // During the tool-free recovery pass, tools are disabled at the API
        // level, so an injected `request_user_input` interview call can never
        // be executed — it only trips the recovery contract guard and collapses
        // the turn to a dead-end fallback. Skip interview synthesis/forcing
        // here; the recovery synthesis can still produce a valid text answer.
        if turn_config.request_user_input_enabled && !tool_free_recovery && turn_processing_ctx.is_planning_active() {
            processing_result = maybe_force_planning_workflow_interview(
                processing_result,
                response.content.as_deref(),
                turn_processing_ctx.session_stats,
                turn_processing_ctx.plan_session,
                turn_processing_ctx.working_history.len(),
            );
        }

        // A recovery directive from the planning turn can leak into the first
        // approved-plan build request. If the model echoes that stale state as
        // its answer, do not commit it to the transcript or end execution;
        // give the fresh write-capable turn a bounded, explicit retry instead.
        let stale_approved_plan_pause = matches!(
            &processing_result,
            TurnProcessingResult::TextResponse { text, .. }
                if turn_processing_ctx.is_approved_plan_execution()
                    && is_stale_approved_plan_pause_response(text)
        );
        if stale_approved_plan_pause && repeated_tool_attempts.verification_is_pending() {
            if let Some(blocked_result) =
                turn_processing_ctx.handle_pending_verification_text_response(&mut repeated_tool_attempts)?
            {
                result = blocked_result;
                break;
            }
            continue;
        }
        if stale_approved_plan_pause {
            let response_count = turn_processing_ctx.harness_state.record_assistant_text_response();
            if response_count >= MAX_ASSISTANT_TEXT_RESPONSES_PER_TURN {
                result = TurnLoopResult::Blocked {
                    reason: Some(PENDING_VERIFICATION_BLOCK_REASON.to_string()),
                };
                break;
            }

            if turn_processing_ctx.harness_state.approved_plan_recovery_retries()
                < MAX_APPROVED_PLAN_STALE_PAUSE_RETRIES
            {
                turn_processing_ctx.harness_state.record_approved_plan_recovery_retry();
                turn_processing_ctx
                    .working_history
                    .push(uni::Message::system(APPROVED_PLAN_STALE_PAUSE_RECOVERY_DIRECTIVE.to_string()));
                let _ = turn_processing_ctx
                    .renderer
                    .line(MessageStyle::Info, "Approved-plan execution resumed after clearing stale recovery state.");
                continue;
            }
        }

        // Restore input status if there are no tool calls (turn is completing)
        // This handles the case where defer_restore was set but no tool spinners will take over
        let has_tool_calls = matches!(processing_result, TurnProcessingResult::ToolCalls { .. });
        if !has_tool_calls {
            turn_processing_ctx.restore_input_status(restore_status_left, restore_status_right);
        }

        if has_tool_calls {
            turn_processing_ctx.set_phase(TurnPhase::ExecutingTools);
        } else {
            turn_processing_ctx.set_phase(TurnPhase::Finalizing);
        }

        // Handle the turn processing result (dispatch tool calls or finish turn)
        let turn_outcome_result = handle_turn_processing_result(HandleTurnProcessingResultParams {
            ctx: &mut turn_processing_ctx,
            processing_result,
            response_streamed,
            step_count,
            repeated_tool_attempts: &mut repeated_tool_attempts,
            turn_modified_files: &mut turn_modified_files,
            max_tool_loops: current_max_tool_loops,
            tool_repeat_limit: turn_config.tool_repeat_limit,
        })
        .await;
        let turn_outcome = match turn_outcome_result {
            Ok(outcome) => outcome,
            Err(err) => {
                // Record result-handler errors for diagnostics (mirrors llm_request recording)
                ctx.error_recovery.write().await.record_error(
                    "turn_result_handler",
                    format!("{err:#}"),
                    ErrorType::ToolExecution,
                );
                let tool_free_recovery =
                    ctx.harness_state.recovery_pass_used() && ctx.harness_state.recovery_is_tool_free();
                let planning = ctx.is_planning_active();

                // Ensure the inline input and status line are not left showing a
                // loading or shimmer state when the result handler fails and the
                // turn may exit early. Keep the terminal state synchronized.
                crate::agent::runloop::unified::display::reset_inline_input(
                    ctx.handle,
                    ctx.default_placeholder.clone(),
                );
                crate::agent::runloop::unified::status_line::clear_input_status(ctx.handle, ctx.input_status_state);

                match dispatch_post_tool_failure(PostToolRecoveryContext {
                    renderer: &mut *ctx.renderer,
                    working_history: &mut *working_history,
                    harness_state: &mut *ctx.harness_state,
                    harness_emitter: ctx.harness_emitter,
                    plan_session: planning.then_some(&mut *ctx.plan_session),
                    plan_state: planning.then_some(&ctx.tool_registry.planning_workflow_state()),
                    err: &err,
                    step_count,
                    turn_history_start_len,
                    stage: "handle_turn_processing_result",
                    tool_free_recovery,
                })
                .await?
                {
                    PostToolFailureAction::Continue => continue,
                    PostToolFailureAction::Break(r) => {
                        result = r;
                        break;
                    }
                    PostToolFailureAction::Fallthrough => {}
                }
                return Err(err);
            }
        };
        // Record token usage before continuing or breaking
        ctx.context_manager.update_token_usage(&response_usage);
        #[cfg(debug_assertions)]
        ctx.context_manager.validate_token_tracking(&response_usage);

        match turn_outcome {
            TurnHandlerOutcome::Continue => continue,
            TurnHandlerOutcome::SwitchPrimaryAgent(agent) => {
                // Plan-mode "switch to build/auto agent" decision: end the turn
                // normally and let the interaction loop perform the handoff.
                pending_primary_agent = Some(agent);
                result = TurnLoopResult::Completed { plan_approved_execution_pending: true };
                break;
            }
            TurnHandlerOutcome::SwitchPrimaryAgentWithPolicy { agent, skip_confirmations, execution_context } => {
                pending_primary_agent = Some(agent);
                pending_plan_auto_accept = skip_confirmations;
                pending_plan_execution_context = execution_context;
                result = TurnLoopResult::Completed { plan_approved_execution_pending: true };
                break;
            }
            TurnHandlerOutcome::BreakWithPolicy {
                result: outcome_result,
                skip_confirmations,
                execution_context,
            } => {
                pending_plan_auto_accept = skip_confirmations;
                pending_plan_execution_context = execution_context;
                result = outcome_result;
                break;
            }
            TurnHandlerOutcome::Break(outcome_result) => {
                // When the model violates the tool-free recovery contract
                // (emits tool calls or textual tool-call markup instead of a
                // final answer), retry the synthesis pass with a corrective
                // directive instead of immediately concluding with the
                // deterministic fallback answer. Mirrors the Empty+tool_calls
                // retry path above. Observed in checkpoint turn_621: a single
                // textual `<tool_call>` block in the recovery response
                // terminated the turn and discarded ~60 messages of gathered
                // context.
                let contract_violation = matches!(
                    outcome_result,
                    TurnLoopResult::Blocked {
                        reason: Some(ref reason)
                    } if reason == RECOVERY_CONTRACT_VIOLATION_REASON
                );
                if tool_free_recovery
                    && !ctx.is_planning_active()
                    && contract_violation
                    && ctx.harness_state.recovery_retry_count() < MAX_RECOVERY_RETRIES
                    && ctx.harness_state.retry_recovery_pass()
                {
                    tracing::warn!(
                        retry = ctx.harness_state.recovery_retry_count(),
                        max = MAX_RECOVERY_RETRIES,
                        "Recovery contract violation; retrying tool-free synthesis pass"
                    );
                    let directive = if ctx.is_planning_active() {
                        RECOVERY_TOOL_CALL_RETRY_DIRECTIVE_PLAN_MODE
                    } else {
                        RECOVERY_TOOL_CALL_RETRY_DIRECTIVE
                    };
                    working_history.push(uni::Message::system(directive.to_string()));
                    continue;
                }
                // Wall-clock-exhausted planning turns must finalize instead of
                // re-forcing the interview (see `dispatch_post_tool_failure`).
                if tool_free_recovery
                    && (ctx.harness_state.wall_clock_exhausted_emitted
                        || ctx.harness_state.wall_clock_exhausted()
                        || ctx.harness_state.tool_budget_exhausted_emitted)
                    && ctx.is_planning_active()
                {
                    ctx.plan_session.mark_recovery_exhausted();
                }
                let salvaged = ctx.harness_state.take_recovery_rejected_synthesis();
                let planning = ctx.is_planning_active();
                let plan_session_opt = planning.then_some(&mut *ctx.plan_session);
                let plan_state = ctx.tool_registry.planning_workflow_state();
                let plan_state_opt = planning.then_some(&plan_state);
                let event_thread_id = ctx.harness_state.run_id.0.clone();
                let event_turn_id = ctx.harness_state.turn_id.0.clone();
                let event_context = PlanRecoveryEventContext {
                    emitter: ctx.harness_emitter,
                    thread_id: &event_thread_id,
                    turn_id: &event_turn_id,
                };
                result = normalize_tool_free_recovery_break_outcome_with_events(
                    working_history,
                    outcome_result,
                    tool_free_recovery,
                    salvaged,
                    plan_session_opt,
                    plan_state_opt,
                    Some(event_context),
                )
                .await;
                break;
            }
        }
    }

    if let TurnLoopResult::Blocked { reason } = &result {
        ensure_blocked_turn_response(
            &mut ctx,
            working_history,
            turn_history_start_len,
            reason.as_deref().unwrap_or("blocked"),
        )?;
    }

    // An approved-plan handoff is a completed control-flow turn, not a user-
    // visible assistant turn. Its implementation request is constructed by
    // the outer session loop, so requiring a final assistant response here
    // would convert the handoff into `Blocked` before execution can start.
    let final_response_was_fallback = if completed_turn_requires_final_response(&result) {
        ensure_completed_turn_response(&mut ctx, working_history, turn_history_start_len)?
    } else {
        ctx.harness_state.final_response_was_fallback()
    };
    if completed_turn_requires_final_response(&result) {
        if final_response_was_fallback {
            result = TurnLoopResult::Blocked {
                reason: Some(COMPLETED_TURN_FALLBACK_REASON.to_string()),
            };
        } else if !ctx.harness_state.final_response_rendered() || !ctx.harness_state.final_response_event_emitted() {
            result = TurnLoopResult::Blocked {
                reason: Some(COMPLETED_TURN_NO_RESPONSE_REASON.to_string()),
            };
        } else if ctx.plan_session.is_recovery_exhausted() {
            result = TurnLoopResult::Blocked {
                reason: Some(PLAN_RECOVERY_EXHAUSTED_REASON.to_string()),
            };
        }
    }

    ctx.renderer.flush_compact_command_group();
    ctx.set_phase(TurnPhase::Finalizing);
    finalize_turn(&mut ctx, working_history, &result, &turn_usage).await;

    // Final outcome with the correct result status
    ctx.session_stats.record_turn_completed();
    let plan_approved_execution_pending =
        matches!(&result, TurnLoopResult::Completed { plan_approved_execution_pending: true });
    ctx.session_stats
        .set_verification_snapshot(repeated_tool_attempts.verification_snapshot());
    let turn_diagnostics = ctx
        .harness_state
        .snapshot_turn_diagnostics(turn_usage.clone(), repeated_tool_attempts.low_signal_tool_calls);
    Ok(TurnLoopOutcome {
        result,
        turn_modified_files,
        turn_diagnostics,
        pending_primary_agent,
        pending_plan_auto_accept,
        pending_plan_execution_context,
        plan_approved_execution_pending,
        final_response_was_fallback,
    })
}

/// Finalize the turn: terminate sessions if needed, emit outcome events,
/// and send notifications.
async fn finalize_turn(
    ctx: &mut TurnLoopContext<'_>,
    working_history: &[uni::Message],
    result: &TurnLoopResult,
    turn_usage: &HarnessUsage,
) {
    if matches!(result, TurnLoopResult::Cancelled | TurnLoopResult::Exit)
        && let Err(err) = ctx.tool_registry.terminate_all_exec_sessions_async().await
    {
        tracing::warn!(error = %err, "Failed to terminate all exec sessions after turn stop");
    }
    if let Some(emitter) = ctx.harness_emitter {
        // Exit is a graceful user-initiated action, not a failure
        let event = match result {
            TurnLoopResult::Completed { .. } | TurnLoopResult::Exit => turn_completed_event(turn_usage.clone()),
            TurnLoopResult::Aborted => {
                turn_failed_event("turn aborted", has_turn_usage(turn_usage).then_some(turn_usage.clone()))
            }
            TurnLoopResult::Cancelled => {
                turn_failed_event("turn cancelled", has_turn_usage(turn_usage).then_some(turn_usage.clone()))
            }
            TurnLoopResult::Blocked { .. } => {
                turn_failed_event("turn blocked", has_turn_usage(turn_usage).then_some(turn_usage.clone()))
            }
        };
        if let Err(e) = emitter.emit(event) {
            tracing::debug!(error = %e, "harness turn outcome event emission failed");
        }
        if let TurnLoopResult::Blocked { reason } = result {
            let message = reason.clone().unwrap_or_else(|| "turn blocked".to_string());
            // Blocked-call fuse telemetry (last tool, enforced caps) captured
            // at trip time; blocks that did not trip the fuse keep the
            // previous `None`/zero fields.
            let fuse_telemetry = ctx.harness_state.take_blocked_tool_recovery_telemetry();
            let (last_tool, consecutive_cap, total_cap, blocked_streak, blocked_total) = fuse_telemetry
                .map(|telemetry| {
                    (
                        Some(telemetry.last_tool),
                        telemetry.consecutive_cap,
                        telemetry.total_cap,
                        telemetry.blocked_streak,
                        telemetry.blocked_total,
                    )
                })
                .unwrap_or((
                    None,
                    0,
                    0,
                    ctx.harness_state.consecutive_blocked_tool_calls,
                    ctx.harness_state.blocked_tool_calls,
                ));
            let blocked_event = vtcode_core::exec::events::TurnBlockedEvent {
                message: message.clone(),
                last_tool,
                blocked_streak,
                blocked_total,
                consecutive_cap,
                total_cap,
                recovery_active: ctx.harness_state.is_recovery_active() || ctx.harness_state.recovery_pass_used(),
                usage: has_turn_usage(turn_usage).then(|| turn_usage.clone()),
            };
            if let Err(e) = emitter.emit(turn_blocked_event(blocked_event)) {
                tracing::debug!(error = %e, "harness turn.blocked event emission failed");
            }
            if let Err(e) = emitter.emit(harness_event(
                vtcode_core::exec::events::HarnessEventKind::TurnBlocked,
                Some(message),
                None,
                None,
                None,
            )) {
                tracing::debug!(error = %e, "harness TurnBlocked event emission failed");
            }
        }

        // Close streamed tool-call items whose calls never reached the
        // pipeline. Entries only survive here when the call was rejected
        // pre-flight, dropped from a batch, or interrupted by the turn ending;
        // dispatched calls remove themselves on execution or rejection.
        for (tool_call_id, streamed) in ctx.harness_state.take_all_streamed_tool_call_item_ids() {
            let failed = ToolCallStatus::Failed;
            let raw_id = (!tool_call_id.trim().is_empty()).then_some(tool_call_id.as_str());
            let _ = emitter.emit(tool_invocation_completed_event(
                streamed.item_id.clone(),
                &streamed.tool_name,
                None,
                raw_id,
                failed.clone(),
                tool_outcome_from_status(&failed),
            ));
            let _ = emitter.emit(tool_output_completed_event(
                streamed.item_id,
                raw_id,
                failed,
                None,
                None,
                UNDISPATCHED_TOOL_CALL_CLOSURE_TEXT,
            ));
        }
    }
    emit_turn_outcome_notification(
        ctx.vt_cfg,
        working_history,
        ctx.config.workspace.as_path(),
        ctx.harness_state,
        result,
    )
    .await;
}

#[cfg(test)]
mod tests;
