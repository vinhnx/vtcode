use super::AgentRunner;
use super::continuation::{CompletionAssessment, VerificationResult};
use super::escalation::{EscalationDecision, EscalationGate};
use super::execute_helpers::{
    emit_blocked_handoff_events, prepare_responses_request_messages, record_terminal_turn_event,
    stop_reason_from_finish_reason, summarize_verification_output,
};
use super::helpers::detect_textual_exec_tool_call;
use super::orchestration::EvaluatorGateOutcome;
use super::prompt_alignment;
use crate::config::build_openai_prompt_cache_key;
use crate::config::constants::tools;
use crate::config::models::{ModelId, Provider as ModelProvider};
use crate::config::tool_loop_limit_reached;
use crate::config::types::{ReasoningEffortLevel, SystemPromptMode, VerbosityLevel};
use crate::core::agent::blocked_handoff::{BlockedHandoffResume, write_blocked_handoff_with_resume};
use crate::core::agent::completion::{check_completion_candidate, check_for_response_loop};
use crate::core::agent::events::ExecEventRecorder;
use crate::core::agent::harness_artifacts::existing_harness_artifact_paths;
use crate::core::agent::harness_kernel::{
    HarnessRequestPlanInput, SessionToolCatalogSnapshot, build_harness_request_plan,
};
use crate::core::agent::hash_utils::stable_system_prefix_hash;
use crate::core::agent::runtime::{AgentRuntime, RuntimeControl};
use crate::core::agent::session::AgentSessionState;
use crate::core::agent::task::{ContextItem, Task, TaskOutcome, TaskResults};
use crate::exec::events::HarnessEventKind;
use crate::llm::provider::{Message, ToolCall, ToolChoice, ToolDefinition, supports_responses_chaining};
use crate::llm::providers::gemini::wire::Part;
use crate::prompts::{
    PromptContext, RuntimePromptContract, append_runtime_mode_sections,
    append_runtime_tool_prompt_sections_for_profile, upsert_harness_limits_section,
};
use crate::utils::colors::style;
use anyhow::{Context, Result};
use serde_json::json;
use std::sync::Arc;
use tracing::{debug, warn};

pub(super) struct RuntimePromptBundle {
    request_envelope: crate::core::agent::request_envelope::SessionRequestEnvelope,
    tool_snapshot: SessionToolCatalogSnapshot,
    /// Estimated token overhead of `request_tools`, computed once per
    /// snapshot (see [`crate::llm::usage_cost::estimate_tool_definition_tokens`]).
    tool_def_tokens: u64,
    /// Token-budget report for the composed system instruction sections
    /// (measured before the runtime-mode/tool-guideline/harness-limits
    /// sections appended in `build_runtime_prompt_bundle`, which are runtime
    /// scaffolding rather than trimmable prompt layers). Drives the
    /// preflight token check and the over-budget user warning.
    pub(super) system_prompt_report: crate::prompts::system::SystemPromptReport,
}

/// Outcome of [`AgentRunner::resolve_completion_assessment`].
///
/// Collapses the duplicated `CompletionAssessment` handling (pre- and
/// post-verification) into a single signal the turn loop reacts to.
enum AssessmentResolution {
    /// Assessment resolved to acceptance or skip — the turn loop should break.
    Break,
    /// Assessment requested more work — the turn loop should force a continuation.
    ForceContinue,
    /// Assessment is `Verify`; the caller runs verification and re-dispatches the
    /// `after_verification` result through the same helper.
    VerifyNotHandled,
}

impl AgentRunner {
    async fn compose_task_system_prompt(
        &self,
        prompt_tools: Arc<Vec<ToolDefinition>>,
        is_simple_task: bool,
    ) -> Result<(String, crate::prompts::system::SystemPromptReport)> {
        if !is_simple_task {
            return Ok((self.system_prompt.clone(), self.system_prompt_report.clone()));
        }

        let mut config = self.config().clone();
        config.agent.system_prompt_mode = SystemPromptMode::Minimal;
        let mut prompt_context = PromptContext::from_workspace_tools(
            self._workspace.as_path(),
            prompt_tools.iter().map(|tool| tool.function_name().to_string()),
        );
        prompt_context.load_available_skills_async().await;

        let (prompt, report) =
            super::helpers::compose_system_prompt_with_appendix(self._workspace.as_path(), &config, &prompt_context)
                .await?;

        Ok((prompt, report))
    }

    async fn build_runtime_prompt_bundle(&self, is_simple_task: bool) -> Result<RuntimePromptBundle> {
        let tool_snapshot = self.build_universal_tool_snapshot().await?;
        let request_tools = tool_snapshot.snapshot.clone();
        let prompt_tools = request_tools.clone().unwrap_or_else(|| Arc::new(Vec::new()));
        let (mut system_prompt, system_prompt_report) =
            self.compose_task_system_prompt(prompt_tools, is_simple_task).await?;
        self.append_active_primary_agent_context(&mut system_prompt);

        let planning_active = self.tool_registry.is_planning_active();
        let request_user_input_enabled = self.features().request_user_input_enabled(planning_active, false);
        let full_auto_active = self.tool_registry.current_full_auto_allowlist().await.is_some();

        append_runtime_mode_sections(
            &mut system_prompt,
            RuntimePromptContract {
                full_auto: full_auto_active,
                planning_active,
                request_user_input_enabled,
            },
        );
        upsert_harness_limits_section(
            &mut system_prompt,
            self.config().agent.harness.max_tool_calls_per_turn,
            self.config().agent.harness.max_tool_wall_clock_secs,
            self.config().agent.harness.max_tool_retries,
        );
        let shell_profile = self.config().agent.shell_prompt_profile.resolve_for_current_platform();
        append_runtime_tool_prompt_sections_for_profile(&mut system_prompt, &tool_snapshot, true, shell_profile);

        let tool_def_tokens = request_tools
            .as_deref()
            .map(|tools| crate::llm::usage_cost::estimate_tool_definition_tokens(tools))
            .unwrap_or(0);
        let tool_count = request_tools.as_deref().map_or(0, Vec::len);
        self.tool_registry
            .metrics_collector()
            .record_sdk_tool_definition_tokens(tool_def_tokens);
        debug!(tool_def_tokens, tool_count, "tool definition overhead");

        let system_instruction_prefix_hash = stable_system_prefix_hash(&system_prompt);
        let request_envelope = crate::core::agent::request_envelope::SessionRequestEnvelope::new(
            format!("{}-catalog-{}-{}", self.session_id, tool_snapshot.version, tool_snapshot.epoch),
            system_prompt,
            request_tools.as_deref().map_or_else(Vec::new, |tools| tools.clone()),
            system_instruction_prefix_hash,
        );
        Ok(RuntimePromptBundle {
            request_envelope,
            tool_snapshot,
            tool_def_tokens,
            system_prompt_report,
        })
    }

    fn append_active_primary_agent_context(&self, system_prompt: &mut String) {
        let Some(active_primary_agent) = self.active_primary_agent.as_ref() else {
            return;
        };

        system_prompt.push_str("\n\n## Active Primary Agent Runtime State\n");
        system_prompt.push_str("- Active agent: ");
        system_prompt.push_str(&active_primary_agent.display_name);
        system_prompt.push_str("\n- Spec name: ");
        system_prompt.push_str(&active_primary_agent.identity.name);
        if let Some(model) = active_primary_agent
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty() && !model.eq_ignore_ascii_case("inherit"))
        {
            system_prompt.push_str("\n- Agent model: ");
            system_prompt.push_str(model);
        }
        if let Some(reasoning_effort) = active_primary_agent.reasoning_effort.as_ref().map(|e| e.as_str()) {
            system_prompt.push_str("\n- Agent reasoning effort: ");
            system_prompt.push_str(reasoning_effort);
        }
        system_prompt.push_str("\n\n## Active Primary Agent Instructions\n");
        system_prompt.push_str(active_primary_agent.instructions.trim());
    }

    pub(super) async fn build_validated_runtime_prompt_bundle(
        &self,
        is_simple_task: bool,
    ) -> Result<RuntimePromptBundle> {
        let mut runner = self;
        let bundle = runner.build_runtime_prompt_bundle(is_simple_task).await?;
        prompt_alignment::rebuild_once_on_alignment_mismatch(
            &mut runner,
            bundle,
            |runner| Box::pin((*runner).build_runtime_prompt_bundle(is_simple_task)),
            |runner, bundle| {
                let planning_active = runner.tool_registry.is_planning_active();
                let request_user_input_enabled = runner.features().request_user_input_enabled(planning_active, false);
                prompt_alignment::validate_prompt_catalog_alignment(
                    bundle.request_envelope.system_prompt().as_ref(),
                    &bundle.tool_snapshot,
                    planning_active,
                    request_user_input_enabled,
                )
            },
            "prompt/catalog alignment mismatch; rebuilding runtime prompt bundle",
            "prompt/catalog alignment mismatch persisted after rebuild",
        )
        .await
    }

    async fn refresh_runtime_prompt_bundle_if_catalog_changed(
        &self,
        bundle: &mut RuntimePromptBundle,
        is_simple_task: bool,
    ) -> Result<bool> {
        let current_version = self.tool_registry.tool_catalog_state().current_version();
        if current_version == bundle.tool_snapshot.version {
            return Ok(false);
        }

        debug!(
            old_version = bundle.tool_snapshot.version,
            new_version = current_version,
            "Tool catalog changed mid-task; refreshing runtime prompt bundle"
        );
        *bundle = self.build_validated_runtime_prompt_bundle(is_simple_task).await?;
        Ok(true)
    }

    async fn resolve_completion_acceptance(
        &mut self,
        effective_task: &Task,
        session_state: &mut AgentSessionState,
        event_recorder: &mut ExecEventRecorder,
        orchestration_enabled: bool,
        verification_results: &[VerificationResult],
        revision_rounds_used: &mut usize,
        max_revision_rounds: usize,
        should_write_blocked_handoff: &mut bool,
    ) -> Result<bool> {
        if !orchestration_enabled {
            session_state.is_completed = true;
            session_state.outcome = TaskOutcome::Success;
            return Ok(true);
        }

        match self
            .apply_evaluator_gate(
                effective_task,
                session_state,
                event_recorder,
                verification_results,
                revision_rounds_used,
                max_revision_rounds,
            )
            .await?
        {
            EvaluatorGateOutcome::Accept => {
                session_state.is_completed = true;
                session_state.outcome = TaskOutcome::Success;
                Ok(true)
            }
            EvaluatorGateOutcome::Continue { prompt } => {
                session_state.add_user_message(prompt);
                Ok(false)
            }
            EvaluatorGateOutcome::Exhausted { reason } => {
                session_state.outcome = TaskOutcome::failed(reason, vec![], None, None);
                *should_write_blocked_handoff = true;
                Ok(true)
            }
        }
    }

    /// Resolve a [`CompletionAssessment`] produced by either
    /// [`super::continuation::ContinuationController::assess_completion`]
    /// (pre-verification) or
    /// [`super::continuation::ContinuationController::after_verification`]
    /// (post-verification) into a
    /// single [`AssessmentResolution`] the turn loop reacts to.
    ///
    /// This consolidates the previously duplicated match arms for `Accept`,
    /// `SkipAccept`, and `Continue`. `Verify` is returned as
    /// [`AssessmentResolution::VerifyNotHandled`] so the caller can run
    /// verification commands and re-dispatch the `after_verification` result
    /// through this same helper.
    ///
    /// Note: `SkipAccept` intentionally bypasses the evaluator gate (mirroring
    /// the original pre-verification path) regardless of which controller call
    /// produced it.
    #[allow(
        clippy::too_many_arguments,
        reason = "Intentional compatibility, platform, or test-only suppression."
    )]
    async fn resolve_completion_assessment(
        &mut self,
        assessment: CompletionAssessment,
        verification_results: &[VerificationResult],
        effective_task: &Task,
        runtime: &mut AgentRuntime,
        event_recorder: &mut ExecEventRecorder,
        orchestration_enabled: bool,
        revision_rounds_used: &mut usize,
        max_revision_rounds: usize,
        should_write_blocked_handoff: &mut bool,
    ) -> Result<AssessmentResolution> {
        match assessment {
            CompletionAssessment::Accept => {
                if self
                    .resolve_completion_acceptance(
                        effective_task,
                        &mut runtime.state,
                        event_recorder,
                        orchestration_enabled,
                        verification_results,
                        revision_rounds_used,
                        max_revision_rounds,
                        should_write_blocked_handoff,
                    )
                    .await?
                {
                    return Ok(AssessmentResolution::Break);
                }
                Ok(AssessmentResolution::ForceContinue)
            }
            CompletionAssessment::SkipAccept { reason } => {
                event_recorder.harness_event(
                    HarnessEventKind::ContinuationSkipped,
                    Some(reason),
                    None,
                    None,
                    None,
                    None,
                    None,
                );
                runtime.state.is_completed = true;
                runtime.state.outcome = TaskOutcome::Success;
                Ok(AssessmentResolution::Break)
            }
            CompletionAssessment::Continue { reason, prompt } => {
                self.emit_continuation_started(event_recorder, reason);
                runtime.state.add_user_message(prompt);
                Ok(AssessmentResolution::ForceContinue)
            }
            // Handled by the caller, which has access to the continuation
            // controller and runs verification before re-dispatching.
            CompletionAssessment::Verify { .. } => Ok(AssessmentResolution::VerifyNotHandled),
        }
    }

    async fn run_verification_commands(
        &self,
        commands: &[String],
        event_recorder: &mut ExecEventRecorder,
    ) -> Result<Vec<VerificationResult>> {
        let mut results = Vec::with_capacity(commands.len());
        for command in commands {
            let command_event = event_recorder.command_started(command);
            let payload = json!({
                "action": "run",
                "command": command,
                "workdir": self._workspace.display().to_string(),
                "yield_time_ms": 1000,
            });
            let result = self.tool_registry.execute_harness_command_session(payload).await?;
            let exit_code = result
                .get("exit_code")
                .and_then(serde_json::Value::as_i64)
                .map(|value| value as i32);
            let success = exit_code.unwrap_or(0) == 0;
            let output = summarize_verification_output(&result);
            event_recorder.command_finished(
                &command_event,
                if success {
                    crate::exec::events::CommandExecutionStatus::Completed
                } else {
                    crate::exec::events::CommandExecutionStatus::Failed
                },
                exit_code,
                &output,
            );
            results.push(VerificationResult {
                command: command.clone(),
                success,
                exit_code,
                output,
            });
            if !success {
                break;
            }
        }
        Ok(results)
    }

    /// Emit the `ContinuationStarted` harness event for a forced-continuation
    /// assessment. Centralizes the (reason-only) event shape used by both the
    /// pre- and post-verification continuation paths.
    fn emit_continuation_started(&self, event_recorder: &mut ExecEventRecorder, reason: String) {
        event_recorder.harness_event(HarnessEventKind::ContinuationStarted, Some(reason), None, None, None, None, None);
    }

    /// Emit `VerificationFailed` (for the first failing command) or
    /// `VerificationPassed` based on the verification run. Reuses
    /// [`super::continuation::build_verification_failure_payload`] so the failure
    /// headline stays in sync with the continuation prompt builder.
    fn emit_verification_outcome(
        &self,
        event_recorder: &mut ExecEventRecorder,
        commands: &[String],
        results: &[VerificationResult],
    ) {
        if let Some(failure) = results.iter().find(|result| !result.success) {
            event_recorder.harness_event(
                HarnessEventKind::VerificationFailed,
                Some(super::continuation::build_verification_failure_payload(failure)),
                Some(failure.command.clone()),
                None,
                failure.exit_code,
                None,
                None,
            );
        } else {
            event_recorder.harness_event(
                HarnessEventKind::VerificationPassed,
                Some(format!("Verification passed: {}", commands.join(", "))),
                commands.last().cloned(),
                None,
                Some(0),
                None,
                None,
            );
        }
    }

    /// Execute a task with this agent
    pub async fn execute_task(&mut self, task: &Task, contexts: &[ContextItem]) -> Result<TaskResults> {
        // Phase 1: Setup — harness alignment, conversation building, session init,
        // orchestration planning. Extracted to `prepare_task_execution` for testability.
        let setup = self.prepare_task_execution(task, contexts).await?;

        let agent_prefix = setup.agent_prefix;
        let session_store_handle = setup.session_store_handle;
        let mut event_recorder = setup.event_recorder;
        let run_started_at = setup.run_started_at;
        let is_simple_task = setup.is_simple_task;
        let mut prompt_bundle = setup.prompt_bundle;
        let preserve_recent_turns = setup.preserve_recent_turns;
        let max_tool_loops = setup.max_tool_loops;
        let max_context_tokens = setup.max_context_tokens;
        let mut runtime = setup.runtime;
        runtime.state.stats.provider_name = self.config().agent.provider.clone();
        let mut continuation_controller = setup.continuation_controller;
        let effective_task = setup.effective_task;
        let orchestration_enabled = setup.orchestration_enabled;
        let mut cost_warning_emitted = false;
        let mut budget_warning_emitted = false;
        let max_budget_usd = setup.max_budget_usd;
        let max_revision_rounds = setup.max_revision_rounds;
        let mut revision_rounds_used = 0usize;
        let mut should_write_blocked_handoff = false;

        let result: Result<_> = {
            for turn in 0..self.max_turns {
                self.tool_registry.begin_turn_preview_window();
                if matches!(runtime.poll_turn_control().await, RuntimeControl::StopRequested) {
                    self.runner_println(format_args!(
                        "{} {}",
                        agent_prefix,
                        style("Stopped by steering signal.").red().bold()
                    ));
                    runtime.state.outcome = TaskOutcome::Cancelled;
                    break;
                }

                if let Some(input) = runtime.run_until_idle() {
                    self.runner_println(format_args!(
                        "{} {}: {}",
                        agent_prefix,
                        style("Follow-up Input").cyan().bold(),
                        input
                    ));
                }

                if runtime.state.is_completed {
                    runtime.state.outcome = TaskOutcome::Success;
                    break;
                }

                self.runner_println(format_args!(
                    "{} {} is processing turn {}...",
                    agent_prefix,
                    style("(PROC)").cyan().bold(),
                    turn + 1
                ));

                let turn_model = self.get_selected_model();
                let provider_name = self.provider_client.name().to_string();
                if std::env::var_os("VTCODE_DEBUG_PROVIDER").is_some() {
                    tracing::debug!(
                        provider_client = self.provider_client.name(),
                        turn_model,
                        "Provider debug turn selection"
                    );
                }
                let sampling_overrides = self.provider_client.sampling_overrides(&turn_model);
                let turn_reasoning = if is_simple_task {
                    Some(ReasoningEffortLevel::Minimal)
                } else {
                    sampling_overrides.reasoning_effort.or(self.reasoning_effort)
                };
                let turn_verbosity = if is_simple_task {
                    Some(VerbosityLevel::Low)
                } else {
                    self.verbosity
                };
                let max_tokens = sampling_overrides
                    .max_tokens
                    .or(if is_simple_task { Some(800) } else { Some(2000) });

                let context_budget = crate::compaction::effective_context_budget(
                    Some(self.config()),
                    self.provider_client.as_ref(),
                    &turn_model,
                );
                runtime.state.constraints.max_context_tokens = context_budget;
                let reserved_output_tokens = max_tokens
                    .map_or(crate::compaction::memory_envelope::DEFAULT_OUTPUT_RESERVE_TOKENS, |value| value as usize);
                let prompt_overhead_tokens = (prompt_bundle.system_prompt_report.token_estimate as usize)
                    .saturating_add(prompt_bundle.tool_def_tokens as usize);
                tracing::info!(
                    turn, model = %turn_model, context_budget,
                    reserved_output_tokens, prompt_overhead_tokens,
                    "Resolved per-turn context budget denominator"
                );
                self.maybe_auto_compact(
                    &mut runtime.state,
                    &mut event_recorder,
                    &turn_model,
                    preserve_recent_turns,
                    prompt_overhead_tokens,
                    reserved_output_tokens,
                )
                .await;
                let (fits, estimated, budget) = runtime.state.preflight_token_check(
                    prompt_bundle.system_prompt_report.token_estimate as usize,
                    prompt_bundle.tool_def_tokens as usize,
                    reserved_output_tokens,
                );
                if !fits {
                    // Advisory only: the estimate can overshoot (un-droppable
                    // messages, suppressed compaction), and the provider owns
                    // the authoritative context limit. Aborting here turned a
                    // recoverable situation into a dead `exec`/auto run.
                    tracing::warn!(
                        estimated,
                        budget,
                        "Pre-flight token check failed: prompt exceeds context budget after compaction"
                    );
                    #[allow(
                        clippy::cast_sign_loss,
                        reason = "Intentional compatibility, platform, or test-only suppression."
                    )]
                    let pct = (estimated as f64 / budget.max(1) as f64 * 100.0) as u32;
                    runtime
                        .state
                        .warnings
                        .push(format!("Pre-flight check: {pct}% of context budget used before LLM call"));
                }

                let parallel_tool_config = if self.model.len() < 20 {
                    None
                } else if self.provider_client.supports_parallel_tool_config(&turn_model) {
                    Some(Box::new(crate::llm::provider::ParallelToolConfig::anthropic_optimized()))
                } else {
                    None
                };

                let provider_kind = turn_model
                    .parse::<ModelId>()
                    .map(|model| model.provider())
                    .unwrap_or(ModelProvider::Gemini);

                if matches!(provider_kind, ModelProvider::Gemini)
                    && runtime.state.conversation.len() > runtime.state.last_processed_message_idx
                {
                    // Collect new messages first to avoid borrow conflict:
                    // conversation is immutably borrowed in the loop, but
                    // adjust_token_count/push need &mut state.
                    let new_messages: Vec<Message> = runtime.state.conversation
                        [runtime.state.last_processed_message_idx..]
                        .iter()
                        .map(|content| {
                            let mut text = String::new();
                            for part in &content.parts {
                                if let Part::Text { text: part_text, .. } = part {
                                    if !text.is_empty() {
                                        text.push('\n');
                                    }
                                    text.push_str(part_text);
                                }
                            }
                            match content.role.as_str() {
                                "model" => Message::assistant(text),
                                _ => Message::user(text),
                            }
                        })
                        .collect();
                    let batch_tokens: usize = new_messages.iter().map(|m| m.estimate_tokens()).sum();
                    runtime.state.adjust_token_count(batch_tokens as isize);
                    runtime.state.messages_mut().extend(new_messages);
                    runtime.state.last_processed_message_idx = runtime.state.conversation.len();
                }

                let reasoning_effort = turn_reasoning
                    .and_then(|requested| {
                        crate::llm::reasoning_effort::ReasoningEffortMapper::resolve_or_omit(
                            self.provider_client.as_ref(),
                            &turn_model,
                            requested,
                            self.config().agent.allow_reasoning_effort_downgrade,
                        )
                    })
                    .map(|mapping| {
                        if mapping.degraded() {
                            tracing::warn!(requested = %mapping.requested, effective = %mapping.effective,
                            model = %turn_model, "Harness reasoning effort explicitly downgraded");
                        }
                        mapping.effective
                    });
                let reasoning_active = reasoning_effort.is_some_and(|effort| {
                    !matches!(effort, ReasoningEffortLevel::None | ReasoningEffortLevel::Unknown)
                });
                let capability_prefix_hash = crate::core::agent::hash_utils::PromptCapabilityIdentity::resolve(
                    self.provider_client.as_ref(),
                    &turn_model,
                    reasoning_effort,
                    prompt_bundle.tool_snapshot.epoch,
                )
                .prefix_hash(prompt_bundle.request_envelope.prefix_hash());

                // Reasoning-effort-change advisory (Phase E4): a mid-task
                // change to the reasoning effort alters the request prefix,
                // which invalidates the provider prompt cache for the next
                // request.
                if runtime.state.note_reasoning_effort_change(reasoning_effort) {
                    let message = "Reasoning effort changed mid-task; provider prompt cache \
                                    will be invalidated and the next request re-pays full \
                                    input cost."
                        .to_string();
                    tracing::warn!("{message}");
                    runtime.state.push_warning(message);
                }

                let anthropic_shaped = matches!(provider_kind, ModelProvider::Anthropic | ModelProvider::Minimax);
                let temperature = if sampling_overrides.suppresses_sampling(anthropic_shaped, reasoning_active) {
                    None
                } else {
                    Some(sampling_overrides.temperature.unwrap_or(self.config().agent.temperature))
                };
                let mut top_p_override = sampling_overrides.top_p;
                let mut top_k_override = sampling_overrides.top_k;
                if sampling_overrides.suppresses_sampling(anthropic_shaped, reasoning_active) {
                    // Keep the payload inside what our Anthropic reasoning
                    // validator accepts: `validate_reasoning_constraints`
                    // rejects any top_k and requires top_p in [0.95, 1.0]
                    // while extended thinking is active.
                    top_k_override = None;
                    top_p_override = top_p_override.filter(|value| *value >= 0.95);
                }

                let (request_messages, previous_response_id) = prepare_responses_request_messages(
                    &mut runtime.state.previous_response_chains,
                    &provider_name,
                    self.provider_client.supports_responses_compaction(&turn_model),
                    &turn_model,
                    &runtime.state.messages,
                );
                let request_messages = match request_messages {
                    std::borrow::Cow::Borrowed(_) => Arc::clone(&runtime.state.messages),
                    std::borrow::Cow::Owned(messages) => Arc::new(messages),
                };
                let request = build_harness_request_plan(HarnessRequestPlanInput {
                    messages: request_messages,
                    system_prompt: prompt_bundle.request_envelope.system_prompt(),
                    tools: (!prompt_bundle.request_envelope.ordered_tools().is_empty())
                        .then(|| prompt_bundle.request_envelope.ordered_tools()),
                    model: turn_model.clone(),
                    max_tokens,
                    temperature,
                    top_p: top_p_override,
                    top_k: top_k_override,
                    presence_penalty: if sampling_overrides.suppresses_sampling(anthropic_shaped, reasoning_active) {
                        None
                    } else {
                        sampling_overrides.presence_penalty
                    },
                    frequency_penalty: if sampling_overrides.suppresses_sampling(anthropic_shaped, reasoning_active) {
                        None
                    } else {
                        sampling_overrides.frequency_penalty
                    },
                    stream: self.provider_client.supports_streaming(),
                    tool_choice: (provider_name.eq_ignore_ascii_case("openai")
                        && !prompt_bundle.tool_snapshot.active_tool_names.is_empty())
                    .then(|| {
                        ToolChoice::allowed_tools_auto(prompt_bundle.tool_snapshot.active_tool_names.as_ref().clone())
                    }),
                    parallel_tool_config,
                    reasoning_effort,
                    verbosity: turn_verbosity,
                    metadata: None,
                    context_management: None,
                    previous_response_id,
                    prompt_cache_key: build_openai_prompt_cache_key(
                        provider_name.eq_ignore_ascii_case("openai")
                            && self.config().prompt_cache.enabled
                            && self.config().prompt_cache.providers.openai.enabled,
                        &self.config().prompt_cache.providers.openai.prompt_cache_key_mode,
                        Some(&self.session_id),
                    )
                    .map(|key| format!("{key}-{capability_prefix_hash:016x}")),
                    prompt_cache_profile: None,
                    tool_catalog_hash: prompt_bundle.request_envelope.catalog_hash(),
                    system_prompt_prefix_hash: Some(capability_prefix_hash),
                })
                .request;
                // Cheap pre-flight: catch malformed requests (empty system
                // prompt, no messages, duplicate tool names, missing required
                // properties) before paying for an API round-trip.
                self.validate_llm_request(&request)?;
                let previous_response_chain_present = request.previous_response_id.is_some();
                // O(1) Arc bump: keeps the sent history available for
                // set_previous_response_chain while the request still holds it
                // for providers that validate messages (e.g. MiMo) during
                // stream().
                let sent_messages = Arc::clone(&request.messages);
                // Compute timeout before the call to avoid simultaneous mutable/immutable
                // borrows of `self` (provider_client vs config).
                let streaming_timeout = self
                    .config()
                    .timeouts
                    .ceiling_duration(self.config().timeouts.streaming_ceiling_seconds);

                // Cache-gap advisory (Phase E1): warn once per gap when the
                // provider prompt cache has likely expired since the last
                // request, so this request may unexpectedly re-pay full
                // input cost.
                if let Some(threshold) = self
                    .config()
                    .prompt_cache
                    .gap_threshold_secs(self.config().agent.provider.as_str())
                {
                    let threshold = std::time::Duration::from_secs(threshold);
                    if runtime.state.stats.total_usage.cached_input_tokens > 0
                        && let Some(elapsed) = runtime.state.cache_gap_exceeds(threshold)
                    {
                        let gap = crate::llm::request_gap::format_gap(elapsed);
                        let message = format!(
                            "~{gap} since the last request; the provider prompt cache has likely expired, so this request may re-pay full input cost."
                        );
                        tracing::warn!("{message}");
                        runtime.state.push_warning(message);
                    }
                }
                runtime.state.note_request_sent();

                let turn_output = runtime
                    .run_turn_once(&mut self.provider_client, request, streaming_timeout)
                    .await?;
                super::tool_dispatch_common::drain_and_record_runtime_events(&mut runtime, &mut event_recorder);
                let response = turn_output.response;
                runtime.state.stop_reason = Some(stop_reason_from_finish_reason(&response.finish_reason));

                // --- Progress stagnation detection ---
                // If the assistant produces near-identical responses across consecutive
                // turns (no tool calls, no progress), inject a nudge to break the loop.
                if !runtime.state.is_completed && runtime.state.record_progress_hash_and_check_stagnation() {
                    let nudge = "It looks like you're repeating the same response. \
                                 If you're stuck, try a different approach: break the \
                                 problem into smaller steps, use different tools, or \
                                 declare the task complete if you have enough information.";
                    self.runner_println(format_args!(
                        "{} {}",
                        agent_prefix,
                        style("[STAGNATION WARNING]").yellow().bold(),
                    ));
                    runtime.state.add_user_message(nudge.into());
                }
                if supports_responses_chaining(
                    &provider_name,
                    self.provider_client.supports_responses_compaction(&turn_model),
                ) {
                    runtime.state.set_previous_response_chain_shared(
                        &provider_name,
                        &turn_model,
                        response.request_id.as_deref(),
                        Arc::clone(&sent_messages),
                    );
                }
                match crate::llm::usage_cost::estimate_session_costs(
                    self.config().agent.provider.as_str(),
                    &turn_model,
                    &runtime.state.stats.total_usage,
                ) {
                    Some(estimate) => {
                        runtime.state.total_cost_usd = Some(estimate.raw_usd);
                        let threshold = self.config().agent.harness.budget_warning_threshold;
                        match crate::llm::usage_cost::BudgetStatus::classify(
                            estimate.raw_usd,
                            max_budget_usd,
                            threshold,
                        ) {
                            crate::llm::usage_cost::BudgetStatus::Exceeded { max, .. } => {
                                runtime.state.outcome = TaskOutcome::budget_limit_reached(max, estimate.raw_usd);
                                break;
                            }
                            crate::llm::usage_cost::BudgetStatus::Warning { max, .. } if !budget_warning_emitted => {
                                budget_warning_emitted = true;
                                warn!(
                                    provider = %self.config().agent.provider,
                                    model = %turn_model,
                                    cost_usd = estimate.raw_usd,
                                    max_budget_usd = max,
                                    "Session cost approaching budget limit"
                                );
                                runtime.state.push_warning(format!(
                                    "Session cost ${:.4} has reached {:.0}% of the ${max:.2} budget. {}",
                                    estimate.raw_usd,
                                    threshold * 100.0,
                                    runtime.state.stats.total_usage.cache_summary()
                                ));
                            }
                            _ => {}
                        }
                    }
                    None => {
                        runtime.state.total_cost_usd = None;
                        if max_budget_usd.is_some() && !cost_warning_emitted {
                            cost_warning_emitted = true;
                            let warning_message = format!(
                                "Budget enforcement disabled for model `{turn_model}` because pricing metadata is unavailable"
                            );
                            warn!(
                                provider = %self.config().agent.provider,
                                model = %turn_model,
                                "Budget enforcement disabled because pricing metadata is unavailable"
                            );
                            runtime.state.push_warning(warning_message);
                        }
                    }
                }
                self.runner_println(format_args!(
                    "{} {} {} received response, processing...",
                    agent_prefix,
                    self.agent_type,
                    style("(RECV)").green().bold()
                ));

                self.warn_on_empty_response(
                    &agent_prefix,
                    response.content.as_deref().unwrap_or(""),
                    response.tool_calls.as_ref().is_some_and(|tool_calls| !tool_calls.is_empty()),
                );

                let response_text = response.content_string();
                if !response_text.trim().is_empty() {
                    self.emit_final_assistant_message(&self.agent_type, &response_text);
                }

                let mut effective_tool_calls = response.tool_calls.clone();
                let mut forced_continuation = false;

                if effective_tool_calls.is_none()
                    && response.content_text().len() < 150
                    && let Some(args_value) = detect_textual_exec_tool_call(response.content_text())
                {
                    effective_tool_calls = Some(vec![ToolCall::function(
                        format!("call_text_{turn}"),
                        tools::EXEC_COMMAND.to_string(),
                        args_value.to_string(),
                    )]);
                }

                let is_gemini = matches!(provider_kind, ModelProvider::Gemini);

                // --- Confidence-based escalation gate + chain ---
                // Evaluate tool calls before dispatching them.  Implements the
                // escalation chain: re-plan → prompt user → abort with partial
                // results.  (Steps 1–2 already exist at the tool-exec level:
                // auto-retry and alternative-tool fallback are handled in
                // tool_exec.rs and execution_facade.rs.)
                #[allow(
                    unused_assignments,
                    reason = "Intentional compatibility, platform, or test-only suppression."
                )] // compiler keeps flags but this is clear
                if self.config().agent.harness.confidence_escalation.enabled
                    && !runtime.state.is_completed
                    && let Some(tool_calls) = effective_tool_calls.as_ref()
                    && !tool_calls.is_empty()
                {
                    let escalation_config = &self.config().agent.harness.confidence_escalation;
                    let orchestration_mode = if orchestration_enabled {
                        "plan_build_evaluate"
                    } else {
                        "single"
                    };

                    // Reuse the per-turn cost estimate from the budget check above.
                    let cost_estimate = runtime.state.total_cost_usd;

                    let result = EscalationGate::decide(
                        tool_calls,
                        escalation_config,
                        &runtime.state.error_recovery.lock(),
                        cost_estimate,
                        orchestration_mode,
                    );

                    if result.any_escalated {
                        let reasons: Vec<String> = result
                            .decisions
                            .iter()
                            .filter_map(|d| match d {
                                EscalationDecision::Escalate { reason, .. } => Some(reason.clone()),
                                EscalationDecision::Proceed => None,
                            })
                            .collect();
                        let summary = reasons.join("; ");

                        self.runner_println(format_args!(
                            "{} {}: {}",
                            agent_prefix,
                            style("[ESCALATION REQUIRED]").red().bold(),
                            summary
                        ));

                        event_recorder.harness_event(
                            HarnessEventKind::EscalationTriggered,
                            Some(summary.clone()),
                            None,
                            None,
                            None,
                            None,
                            None,
                        );

                        let esc_count = runtime.state.consecutive_escalations;

                        // --- Step 3: Re-plan via conversation injection ---
                        if esc_count < escalation_config.max_replan_attempts {
                            runtime.state.consecutive_escalations += 1;
                            let replan_msg = format!(
                                "The following tool calls were blocked by the safety \
                                 escalation gate:\n\n{summary}\n\n\
                                 Please try a different approach. You can use alternative \
                                 tools, break the task into smaller steps, or provide a \
                                 direct answer based on what you have already learned."
                            );
                            runtime.state.add_user_message(replan_msg);
                            // Skip dispatching blocked tool calls — let the LLM re-plan
                            effective_tool_calls = None;
                            forced_continuation = true;
                        }
                        // --- Step 4: Prompt user for guidance ---
                        else if esc_count < escalation_config.max_total_escalations
                            && escalation_config.prompt_user_on_exhaust
                        {
                            runtime.state.consecutive_escalations += 1;
                            let user_msg = format!(
                                "I've tried multiple approaches but the safety escalation \
                                 gate keeps blocking my tool calls:\n\n{summary}\n\n\
                                 Could you provide guidance on how to proceed? \
                                 What approach should I use instead?"
                            );
                            runtime.state.add_user_message(user_msg);
                            should_write_blocked_handoff = true;
                            runtime.state.outcome = TaskOutcome::escalated(summary, "multi_tool".to_string());
                            break;
                        }
                        // --- Step 5: Abort with partial results ---
                        else {
                            runtime.state.consecutive_escalations += 1;
                            should_write_blocked_handoff = true;
                            runtime.state.outcome = TaskOutcome::failed(
                                format!("Escalation chain exhausted: {summary}"),
                                Vec::new(),
                                Some(
                                    "The safety escalation gate repeatedly blocked tool calls. \
                                     Consider disabling the gate or adjusting the confidence \
                                     threshold if this action should be permitted."
                                        .into(),
                                ),
                                None,
                            );
                            break;
                        }
                    } else {
                        // Tool calls passed escalation gate — reset chain counter
                        runtime.state.consecutive_escalations = 0;

                        event_recorder.harness_event(
                            HarnessEventKind::EscalationBypassed,
                            Some("All tool calls passed escalation gate".into()),
                            None,
                            None,
                            None,
                            None,
                            None,
                        );
                    }
                }

                if !runtime.state.is_completed
                    && effective_tool_calls.as_ref().is_none_or(|tool_calls| tool_calls.is_empty())
                    && !response.content_text().is_empty()
                {
                    if check_for_response_loop(response.content_text(), &mut runtime.state) {
                        self.runner_println(format_args!(
                            "[{}] {}",
                            self.agent_type,
                            style("Repetitive assistant response detected. Breaking potential loop.")
                                .red()
                                .bold()
                        ));
                        runtime.state.outcome = TaskOutcome::LoopDetected;
                        break;
                    }

                    if check_completion_candidate(response.content_text()) {
                        self.runner_println(format_args!(
                            "[{}] {}",
                            self.agent_type,
                            style("Completion candidate detected; checking tracker and verification state.")
                                .green()
                                .bold()
                        ));
                        let assessment = continuation_controller
                            .assess_completion(&effective_task, &runtime.state)
                            .await?;

                        // Verify requires running verification commands before
                        // re-dispatching the after_verification result through the
                        // same helper. All other variants are resolved directly.
                        if let CompletionAssessment::Verify { commands } = &assessment {
                            event_recorder.harness_event(
                                HarnessEventKind::VerificationStarted,
                                Some(format!("Running verification: {}", commands.join(", "))),
                                commands.first().cloned(),
                                None,
                                None,
                                None,
                                None,
                            );
                            let verification_results =
                                self.run_verification_commands(commands, &mut event_recorder).await?;
                            self.emit_verification_outcome(&mut event_recorder, commands, &verification_results);

                            let post_verification =
                                continuation_controller.after_verification(&verification_results).await?;
                            match self
                                .resolve_completion_assessment(
                                    post_verification,
                                    &verification_results,
                                    &effective_task,
                                    &mut runtime,
                                    &mut event_recorder,
                                    orchestration_enabled,
                                    &mut revision_rounds_used,
                                    max_revision_rounds,
                                    &mut should_write_blocked_handoff,
                                )
                                .await?
                            {
                                AssessmentResolution::Break => break,
                                AssessmentResolution::ForceContinue => {
                                    forced_continuation = true;
                                }
                                // `after_verification` never yields `Verify`.
                                AssessmentResolution::VerifyNotHandled => {}
                            }
                        } else {
                            match self
                                .resolve_completion_assessment(
                                    assessment,
                                    &[],
                                    &effective_task,
                                    &mut runtime,
                                    &mut event_recorder,
                                    orchestration_enabled,
                                    &mut revision_rounds_used,
                                    max_revision_rounds,
                                    &mut should_write_blocked_handoff,
                                )
                                .await?
                            {
                                AssessmentResolution::Break => break,
                                AssessmentResolution::ForceContinue => {
                                    forced_continuation = true;
                                }
                                AssessmentResolution::VerifyNotHandled => {
                                    // Verify is handled in the if-branch above;
                                    // the helper only returns this for Verify.
                                    return Err(anyhow::anyhow!("unexpected VerifyNotHandled from assess_completion"));
                                }
                            }
                        }
                    }
                }

                if let Some(tool_calls) = effective_tool_calls
                    .as_ref()
                    .filter(|tool_calls| !tool_calls.is_empty())
                    .cloned()
                {
                    self.execute_tool_call_batches(
                        tool_calls,
                        &mut runtime,
                        &mut event_recorder,
                        &agent_prefix,
                        is_gemini,
                        previous_response_chain_present,
                    )
                    .await?;
                    super::tool_dispatch_common::drain_and_record_runtime_events(&mut runtime, &mut event_recorder);
                }

                // Refresh tool definitions if the catalog was mutated during tool
                // execution (e.g. tools.load / tools.unload / skill activation).
                if self
                    .refresh_runtime_prompt_bundle_if_catalog_changed(&mut prompt_bundle, is_simple_task)
                    .await?
                {
                    tracing::debug!("runtime prompt bundle refreshed after tool catalog mutation");
                }

                // --- Emit tool latency events ---
                if !runtime.state.turn_tool_latencies.is_empty() {
                    let latencies = std::mem::take(&mut runtime.state.turn_tool_latencies);
                    for (tool_name, duration_ms) in &latencies {
                        event_recorder.record_tool_latency(tool_name, *duration_ms);
                    }
                }

                let had_effective_shell_tool_call = effective_tool_calls.as_ref().is_some_and(|calls| {
                    calls.iter().any(|call| {
                        call.function.as_ref().map(|function| function.name.as_str()) == Some(tools::UNIFIED_EXEC)
                    })
                });
                let had_tool_call = response.tool_calls.as_ref().is_some_and(|tool_calls| !tool_calls.is_empty())
                    || had_effective_shell_tool_call;

                if had_tool_call {
                    let loops = runtime.state.register_tool_loop();
                    if tool_loop_limit_reached(loops, runtime.state.constraints.max_tool_loops) {
                        let warning_message = format!(
                            "You have reached the tool-call iteration limit of {}. \
                             This typically means you are in a loop — repeatedly calling tools \
                             without making progress toward the task goal.\n\n\
                             To proceed:\n\
                             1. Review what you have already learned from previous tool outputs.\n\
                             2. Synthesize your findings into a concrete answer or implementation.\n\
                             3. If you need more information, use a different approach or tool.\n\
                             4. If you are truly stuck, explain what you have accomplished so far \
                             and what is blocking you.",
                            runtime.state.constraints.max_tool_loops
                        );
                        self.record_warning(
                            &agent_prefix,
                            &mut runtime.state,
                            &mut event_recorder,
                            warning_message.clone(),
                        );
                        runtime.state.add_user_message(warning_message);
                        runtime.state.mark_tool_loop_limit_hit();
                        break;
                    }
                    runtime.state.consecutive_idle_turns = 0;
                } else {
                    runtime.state.reset_tool_loop_guard();
                    if forced_continuation {
                        runtime.state.consecutive_idle_turns = 0;
                    } else if !runtime.state.is_completed {
                        runtime.state.consecutive_idle_turns = runtime.state.consecutive_idle_turns.saturating_add(1);
                        let idle_turn_limit = self.config().agent.idle_turn_limit;
                        if runtime.state.consecutive_idle_turns >= idle_turn_limit {
                            let warning_message = format!(
                                "No tool calls or completion for {} consecutive turns. \
                                 The agent appears to be idle — it is responding without \
                                 taking actions or declaring the task complete.\n\n\
                                 To proceed:\n\
                                 1. Take concrete action using the available tools.\n\
                                 2. If you have enough information, present your solution.\n\
                                 3. If you are waiting for something, explain the situation.",
                                runtime.state.consecutive_idle_turns
                            );
                            self.record_warning(
                                &agent_prefix,
                                &mut runtime.state,
                                &mut event_recorder,
                                warning_message.clone(),
                            );
                            runtime.state.add_user_message(warning_message);
                            runtime.state.outcome = TaskOutcome::StoppedNoAction;
                            break;
                        }
                    }
                }

                let should_continue = forced_continuation
                    || had_tool_call
                    || runtime.has_pending_follow_up_inputs()
                    || (!runtime.state.is_completed && (turn + 1) < self.max_turns);

                if !should_continue {
                    if runtime.state.is_completed {
                        runtime.state.outcome = TaskOutcome::Success;
                    } else if (turn + 1) >= self.max_turns {
                        runtime.state.outcome = TaskOutcome::turn_limit_reached(self.max_turns, turn + 1);
                    } else {
                        runtime.state.outcome = TaskOutcome::StoppedNoAction;
                    }
                    break;
                }
            }

            runtime.state.finalize_outcome(self.max_turns);

            let total_duration_ms = run_started_at.elapsed().as_millis();

            // Agent execution completed
            self.runner_println(format_args!("{agent_prefix} Done"));

            // Generate meaningful summary based on agent actions
            let average_turn_duration_ms = if runtime.state.turn_count > 0 {
                Some(runtime.state.turn_total_ms as f64 / runtime.state.turn_count as f64)
            } else {
                None
            };

            let max_turn_duration_ms = if runtime.state.turn_count > 0 {
                Some(runtime.state.turn_max_ms)
            } else {
                None
            };

            let outcome = runtime.state.outcome.clone();
            self.thread_handle.replace_messages((*runtime.state.messages).clone());
            let summary = self.generate_task_summary(
                &effective_task,
                &runtime.state.modified_files,
                &runtime.state.executed_commands,
                &runtime.state.warnings,
                &runtime.state.messages,
                runtime.state.stats.turns_executed,
                runtime.state.max_tool_loop_streak,
                max_tool_loops,
                outcome,
                total_duration_ms,
                average_turn_duration_ms,
                max_turn_duration_ms,
                &runtime.state.stats.total_usage,
            );

            if !summary.trim().is_empty() {
                // Record summary as agent message for event stream
                event_recorder.agent_message(&summary);
                // Also display summary prominently for immediate visibility in TUI transcript
                self.runner_println(format_args!(
                    "\n{} Agent Task Summary\n{}",
                    style("[Task]").cyan().bold(),
                    summary
                ));
            }

            let runtime_agent_config = self.core_agent_config();
            if let Err(err) = crate::persistent_memory::finalize_persistent_memory(
                &runtime_agent_config,
                Some(self.config()),
                &runtime.state.messages,
            )
            .await
            {
                warn!(
                    error = %err,
                    session_id = %self.session_id,
                    "Failed to update persistent memory"
                );
            }

            if runtime.state.outcome.is_hard_block() || should_write_blocked_handoff {
                let relevant_paths = existing_harness_artifact_paths(&self._workspace);
                match write_blocked_handoff_with_resume(
                    &self._workspace,
                    &self.session_id,
                    runtime.state.outcome.code(),
                    &runtime.state.outcome.description(),
                    &relevant_paths,
                    BlockedHandoffResume::Unavailable(
                        "Resume unavailable because the legacy agent runner does not create a session archive.",
                    ),
                ) {
                    Ok(artifacts) => emit_blocked_handoff_events(
                        &mut event_recorder,
                        &runtime.state.outcome.description(),
                        &artifacts.current_path,
                        &artifacts.archive_path,
                    ),
                    Err(err) => warn!(
                        error = %err,
                        session_id = %self.session_id,
                        "Failed to persist blocked handoff"
                    ),
                }
            }

            let total_usage = runtime.state.stats.total_usage.clone();
            record_terminal_turn_event(&mut event_recorder, &runtime.state.outcome, total_usage.clone());
            event_recorder.thread_completed(
                &self.session_id,
                runtime.state.outcome.thread_completion_subtype(),
                runtime.state.outcome.code(),
                runtime.state.outcome.is_success().then_some(summary.as_str()),
                runtime.state.stop_reason.as_deref(),
                total_usage,
                runtime.state.total_cost_usd.and_then(serde_json::Number::from_f64),
                runtime.state.stats.turns_executed,
            );
            let thread_events = event_recorder.take_events();
            let steering_receiver = runtime.take_steering_receiver();
            let state = std::mem::replace(
                &mut runtime.state,
                AgentSessionState::new(self.session_id.clone(), self.max_turns, max_tool_loops, max_context_tokens),
            );

            Ok((state.into_results(summary, thread_events, total_duration_ms), steering_receiver))
        };

        let result = match result {
            Ok((task_results, steering_receiver)) => {
                *self.steering_receiver.lock() = steering_receiver;
                Ok(task_results)
            }
            Err(err) => {
                event_recorder.thread_failed(
                    &self.session_id,
                    &err.to_string(),
                    runtime.state.stats.turns_executed.max(1),
                );
                *self.steering_receiver.lock() = runtime.take_steering_receiver();
                Err(err)
            }
        };

        let persistence_result = if let Some(session_store_handle) = session_store_handle {
            session_store_handle
                .close()
                .await
                .context("authoritative session event persistence failed")
        } else {
            Ok(())
        };

        self.tool_registry.set_harness_task(None);
        persistence_result?;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::{prepare_responses_request_messages, record_terminal_turn_event, tool_loop_limit_reached};
    use crate::core::agent::events::ExecEventRecorder;
    use crate::core::agent::session::AgentSessionState;
    use crate::core::agent::task::TaskOutcome;
    use crate::exec::events::ThreadEvent;
    use crate::llm::provider::{Message, records_responses_continuation_state};

    #[test]
    fn failed_outcome_emits_only_turn_failed() {
        let mut recorder = ExecEventRecorder::new("thread", None, None);
        recorder.turn_started();

        record_terminal_turn_event(
            &mut recorder,
            &TaskOutcome::failed("boom".to_string(), vec![], None, None),
            Default::default(),
        );

        let events = recorder.into_events();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ThreadEvent::TurnFailed(_)))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ThreadEvent::TurnCompleted(_)))
                .count(),
            0
        );
    }

    #[test]
    fn successful_outcome_emits_only_turn_completed() {
        let mut recorder = ExecEventRecorder::new("thread", None, None);
        recorder.turn_started();

        record_terminal_turn_event(&mut recorder, &TaskOutcome::Success, Default::default());

        let events = recorder.into_events();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ThreadEvent::TurnCompleted(_)))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ThreadEvent::TurnFailed(_)))
                .count(),
            0
        );
    }

    #[test]
    fn disabled_tool_loop_limit_never_trips() {
        assert!(!tool_loop_limit_reached(1, 0));
        assert!(!tool_loop_limit_reached(32, 0));
    }

    #[test]
    fn openai_prepare_responses_request_messages_keeps_full_history_without_previous_response_id() {
        let mut state = AgentSessionState::new("session".to_string(), 4, 4, 16_000);
        let prior_messages = vec![Message::user("hello".to_string())];
        let current_messages = vec![
            Message::user("hello".to_string()),
            Message::user("continue".to_string()),
        ];
        state.set_previous_response_chain("openai", "gpt-5.6-sol", Some("resp_123"), prior_messages);

        let (request_messages, previous_response_id) = prepare_responses_request_messages(
            &mut state.previous_response_chains,
            "openai",
            false,
            "gpt-5.6-sol",
            &current_messages,
        );

        assert_eq!(previous_response_id, None);
        assert_eq!(request_messages.as_ref(), current_messages.as_slice());
    }

    #[test]
    fn openai_runner_success_path_does_not_record_previous_response_chain() {
        let mut state = AgentSessionState::new("session".to_string(), 4, 4, 16_000);
        let messages = vec![Message::user("hello".to_string())];

        if records_responses_continuation_state("openai", true) {
            state.set_previous_response_chain("openai", "gpt-5.6-sol", Some("resp_123"), messages);
        }

        assert_eq!(state.previous_response_chain_for("openai", "gpt-5.6-sol"), None);
    }

    #[test]
    fn compatible_runner_success_path_does_not_record_previous_response_chain() {
        let mut state = AgentSessionState::new("session".to_string(), 4, 4, 16_000);
        let messages = vec![Message::user("hello".to_string())];

        if records_responses_continuation_state("mycorp", true) {
            state.set_previous_response_chain("mycorp", "gpt-5.6-sol", Some("resp_123"), messages);
        }

        assert_eq!(state.previous_response_chain_for("mycorp", "gpt-5.6-sol"), None);
    }

    #[test]
    fn gemini_prepare_responses_request_messages_keeps_full_history() {
        let mut state = AgentSessionState::new("session".to_string(), 4, 4, 16_000);
        let prior_messages = vec![Message::user("hello".to_string())];
        let current_messages = vec![
            Message::user("hello".to_string()),
            Message::user("continue".to_string()),
        ];
        state.set_previous_response_chain("gemini", "gemini-2.5-pro", Some("resp_123"), prior_messages);

        let (request_messages, previous_response_id) = prepare_responses_request_messages(
            &mut state.previous_response_chains,
            "gemini",
            false,
            "gemini-2.5-pro",
            &current_messages,
        );

        assert_eq!(previous_response_id.as_deref(), Some("resp_123"));
        assert_eq!(request_messages.as_ref(), current_messages.as_slice());
    }

    #[test]
    fn compatible_prepare_responses_request_messages_keeps_custom_provider_stateless() {
        let mut state = AgentSessionState::new("session".to_string(), 4, 4, 16_000);
        let prior_messages = vec![Message::user("hello".to_string())];
        let current_messages = vec![
            Message::user("hello".to_string()),
            Message::user("continue".to_string()),
        ];
        state.set_previous_response_chain("mycorp", "gpt-5.6-sol", Some("resp_123"), prior_messages);

        let (request_messages, previous_response_id) = prepare_responses_request_messages(
            &mut state.previous_response_chains,
            "mycorp",
            true,
            "gpt-5.6-sol",
            &current_messages,
        );

        assert_eq!(previous_response_id, None);
        assert_eq!(request_messages.as_ref(), current_messages.as_slice());
    }
}
