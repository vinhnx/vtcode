use std::collections::VecDeque;
use std::time::Instant;

use anyhow::Result;
use tokio::sync::mpsc;
use tokio::time::{Duration, sleep, timeout};
use tokio_util::sync::CancellationToken;
use vtcode_commons::ui_protocol::ActivityState;
use vtcode_config::loader::SimpleConfigWatcher;
use vtcode_core::config::loader::VTCodeConfig;
use vtcode_core::config::types::AgentConfig as CoreAgentConfig;
use vtcode_core::core::agent::runtime::AgentRuntime;
use vtcode_core::core::agent::session::AgentSessionState;
use vtcode_core::core::agent::steering::SteeringMessage;
use vtcode_core::core::interfaces::session::PlanningEntrySource;
use vtcode_core::exec::events::ThreadCompletionSubtype;
use vtcode_core::hooks::{SessionEndReason, SessionStartTrigger};
use vtcode_core::llm::provider::MessageRole;
use vtcode_core::session::SessionId;
use vtcode_core::utils::ansi::MessageStyle;
use vtcode_core::utils::session_archive;
use vtcode_core::utils::session_archive::{SessionMessage, SessionProgressArgs};
use vtcode_ui::tui::app::ArchivedPromptEntry;

use super::super::{CancelGuard, RECENT_MESSAGE_LIMIT, TerminalCleanupGuard, extract_idle_config};
use super::archive::{create_session_archive, refresh_runtime_debug_context_for_next_session, workspace_archive_label};
use super::blocked_handoff::{
    SessionCheckpointOutcome, persist_session_checkpoint, write_blocked_handoff_after_checkpoint,
};
use super::handoff::{
    append_approved_plan_execution_input, apply_primary_agent_tool_policy_overrides,
    build_approved_plan_execution_prompt, select_approved_plan_execution_agent,
};
use super::metrics::{
    TurnExecutionMetrics, capture_code_change_snapshot, emit_turn_execution_metrics, estimate_history_bytes,
};
use super::plan_seed::load_active_plan_seed;
use super::support::{
    ExecutionSummaryStatus, append_transient_turn_notes, approved_plan_execution_summary,
    build_unrelated_dirty_worktree_note, checkpoint_session_archive_start, force_reload_workspace_config_for_execution,
    format_workspace_relative_paths, latest_assistant_result_text, prepare_resume_bootstrap_without_archive,
    prompt_startup_planning_workflow, remove_transient_system_notes, take_pending_resumed_user_prompt,
};
use crate::agent::runloop::ResumeSession;
use crate::agent::runloop::git::{compute_session_code_change_delta, normalize_workspace_path};
use crate::agent::runloop::model_picker::ModelPickerState;
use crate::agent::runloop::unified::inline_events::harness::harness_event;
use crate::agent::runloop::unified::palettes::ActivePalette;
use crate::agent::runloop::unified::planning_workflow_state::{
    render_planning_workflow_next_step_hint, transition_to_planning_workflow,
};
use crate::agent::runloop::unified::postamble::{ExitData, print_exit_summary};
use crate::agent::runloop::unified::run_loop_context::{HarnessTurnState, TurnId, TurnRunId};
use crate::agent::runloop::unified::session_setup::{
    SessionState, initialize_session, initialize_session_ui, spawn_signal_handler,
};
use crate::agent::runloop::unified::state::SessionStats;
use crate::agent::runloop::unified::status_line::InputStatusState;
use crate::agent::runloop::unified::turn::context::TurnLoopResult as RunLoopTurnLoopResult;
use crate::agent::runloop::unified::turn::finalization::finalize_session;
use crate::agent::runloop::unified::turn::primary_agent_runtime::{
    PrimaryAgentRuntimeSyncContext, sync_primary_agent_permissions, sync_primary_agent_runtime,
};
use crate::agent::runloop::unified::turn::turn_loop::TurnLoopOutcome;
use crate::agent::runloop::unified::turn::turn_loop_helpers::{
    effective_max_tool_calls_for_approved_plan_execution, effective_max_tool_calls_for_turn,
    resolve_safety_tool_call_limits,
};
use crate::agent::runloop::unified::workspace_links::LinkedDirectory;
use crate::updater::{InlineUpdateOutcome, display_update_notice, run_inline_update_prompt};

fn persist_primary_agent(
    session_archive: &mut Option<session_archive::SessionArchive>,
    active_primary_agent: &vtcode_core::primary_agent::ActivePrimaryAgentState,
) {
    if let Some(archive) = session_archive.as_mut() {
        archive.set_primary_agent(active_primary_agent.active().name());
    }
}

pub(super) fn resolve_thread_completion_status(
    session_end_reason: &SessionEndReason,
    budget_limit_reached: bool,
    last_approved_plan_summary_status: Option<ExecutionSummaryStatus>,
    last_turn_result: Option<&RunLoopTurnLoopResult>,
    last_turn_response_was_fallback: bool,
) -> (&'static str, ThreadCompletionSubtype) {
    if matches!(session_end_reason, SessionEndReason::Completed) {
        return match (last_approved_plan_summary_status, last_turn_result) {
            (Some(ExecutionSummaryStatus::Blocked), _)
            | (_, Some(RunLoopTurnLoopResult::Blocked { .. }))
            | (_, Some(RunLoopTurnLoopResult::Aborted)) => ("blocked", ThreadCompletionSubtype::ErrorDuringExecution),
            (Some(ExecutionSummaryStatus::Failed), _)
            | (_, Some(RunLoopTurnLoopResult::Completed { .. }))
            | (_, Some(RunLoopTurnLoopResult::Cancelled))
                if last_turn_response_was_fallback =>
            {
                ("failed", ThreadCompletionSubtype::ErrorDuringExecution)
            }
            (_, Some(RunLoopTurnLoopResult::Cancelled)) => ("cancelled", ThreadCompletionSubtype::Cancelled),
            (_, Some(RunLoopTurnLoopResult::Exit)) => ("exit", ThreadCompletionSubtype::Cancelled),
            _ => session_end_reason.thread_completion_status(budget_limit_reached),
        };
    }

    if matches!(session_end_reason, SessionEndReason::Exit)
        && !last_turn_response_was_fallback
        && !matches!(
            last_approved_plan_summary_status,
            Some(ExecutionSummaryStatus::Blocked | ExecutionSummaryStatus::Failed)
        )
        && matches!(last_turn_result, Some(RunLoopTurnLoopResult::Completed { .. }))
    {
        return ("exit", ThreadCompletionSubtype::Success);
    }

    session_end_reason.thread_completion_status(budget_limit_reached)
}

#[cfg_attr(feature = "profiling", hotpath::measure)]
pub(crate) async fn run_single_agent_loop_unified_impl(
    config: &CoreAgentConfig,
    initial_vt_cfg: Option<VTCodeConfig>,
    skip_confirmations: bool,
    full_auto: bool,
    primary_agent_explicitly_configured: bool,
    planning_entry_source: PlanningEntrySource,
    resume: Option<ResumeSession>,
    steering_receiver: &mut Option<mpsc::UnboundedReceiver<SteeringMessage>>,
) -> Result<()> {
    let _terminal_cleanup_guard = TerminalCleanupGuard::new();

    let mut config = config.clone();
    let mut session_skip_confirmations = skip_confirmations;
    let mut resume_state = resume;
    let mut _consecutive_idle_cycles = 0;
    let mut last_activity_time: Option<Instant> = None;
    let mut config_watcher = SimpleConfigWatcher::new_with_user_config_paths(config.workspace.clone());
    config_watcher.set_check_interval(15);
    config_watcher.set_debounce_duration(500);
    if let Some(initial_config) = initial_vt_cfg.as_ref() {
        config_watcher.set_last_known_config(initial_config.clone());
    }
    let mut vt_cfg = initial_vt_cfg.or_else(|| config_watcher.load_config());
    let mut idle_config = extract_idle_config(vt_cfg.as_ref());
    let mut pending_session_start_trigger = None;
    let mut next_session_primary_agent: Option<String> = None;

    loop {
        let session_started_at = Instant::now();
        let start_code_changes = capture_code_change_snapshot(&config.workspace, "start").await;
        let resume_request = resume_state.take();
        let resume_ref = resume_request.as_ref();
        let session_trigger = pending_session_start_trigger.take().unwrap_or_else(|| {
            if resume_ref.is_some() {
                SessionStartTrigger::Resume
            } else {
                SessionStartTrigger::Startup
            }
        });
        let active_thread_label = resume_ref.map_or("main", ResumeSession::thread_label);
        let thread_manager = vtcode_core::core::threads::ThreadManager::new();
        let archive_metadata = vtcode_core::core::threads::build_thread_archive_metadata(
            &config.workspace,
            &config.model,
            &config.provider,
            &config.theme,
            config.reasoning_effort.as_str(),
        )
        .with_debug_log_path(
            crate::main_helpers::runtime_debug_log_path().map(|path| path.to_string_lossy().to_string()),
        );
        let reserved_archive_id = crate::main_helpers::runtime_archive_session_id();
        let history_enabled = session_archive::history_persistence_enabled();
        let summarized_fork_provider = if resume_ref.is_some_and(|resume| resume.summarize_fork()) {
            Some(crate::agent::runloop::unified::session_setup::create_provider_client(&config, vt_cfg.as_ref())?)
        } else {
            None
        };
        let (thread_handle, mut session_archive) = if let Some(resume) = resume_ref {
            if history_enabled {
                let mut prepared = vtcode_core::core::threads::prepare_archived_session(
                    resume.listing().clone(),
                    config.workspace.clone(),
                    archive_metadata.clone(),
                    resume.intent().clone(),
                    if resume.is_fork() {
                        reserved_archive_id.clone()
                    } else {
                        None
                    },
                )
                .await?;
                if let Some(provider) = summarized_fork_provider.as_deref() {
                    prepared.bootstrap.messages =
                        crate::agent::runloop::unified::turn::compaction::build_summarized_fork_history(
                            provider,
                            &config.model,
                            &resume.identifier(),
                            &prepared.thread_id,
                            &config.workspace,
                            vt_cfg.as_ref(),
                            resume.history(),
                            resume.budget_limit_continuation().is_some(),
                        )
                        .await?;
                }
                (
                    thread_manager.start_thread_with_identifier(prepared.thread_id.clone(), prepared.bootstrap),
                    Some(prepared.archive),
                )
            } else {
                let (mut bootstrap, thread_id) = prepare_resume_bootstrap_without_archive(
                    resume,
                    archive_metadata.clone(),
                    reserved_archive_id.clone(),
                );
                if let Some(provider) = summarized_fork_provider.as_deref() {
                    bootstrap.messages =
                        crate::agent::runloop::unified::turn::compaction::build_summarized_fork_history(
                            provider,
                            &config.model,
                            &resume.identifier(),
                            &thread_id,
                            &config.workspace,
                            vt_cfg.as_ref(),
                            resume.history(),
                            resume.budget_limit_continuation().is_some(),
                        )
                        .await?;
                }
                (thread_manager.start_thread_with_identifier(thread_id, bootstrap), None)
            }
        } else {
            let thread_id = if let Some(identifier) = reserved_archive_id.clone() {
                identifier
            } else if history_enabled {
                session_archive::reserve_session_archive_identifier(&workspace_archive_label(&config.workspace), None)
                    .await?
            } else {
                session_archive::generate_session_archive_identifier(&workspace_archive_label(&config.workspace), None)
            };
            let bootstrap = vtcode_core::core::threads::ThreadBootstrap::new(Some(archive_metadata.clone()));
            let archive = if history_enabled {
                Some(create_session_archive(archive_metadata.clone(), Some(thread_id.clone())).await?)
            } else {
                None
            };
            (thread_manager.start_thread_with_identifier(thread_id, bootstrap), archive)
        };
        crate::main_helpers::set_runtime_archive_session_id(Some(thread_handle.thread_id().to_string()));
        if let Some(archive) = session_archive.as_ref()
            && let Err(err) = checkpoint_session_archive_start(archive, &thread_handle).await
        {
            tracing::warn!("Failed to checkpoint session archive at startup: {}", err);
        }
        let session_setup_phase = vtcode_commons::startup_trace::phase_started();
        let session_primary_agent_override = next_session_primary_agent.take();
        let mut session_state = initialize_session(
            &config,
            vt_cfg.as_ref(),
            full_auto,
            primary_agent_explicitly_configured,
            resume_ref,
            thread_handle.thread_id().as_str(),
            session_primary_agent_override.as_deref(),
        )
        .await?;
        // Persist the active primary agent ("mode") so a future resume restores
        // it instead of falling back to the config default.
        persist_primary_agent(&mut session_archive, &session_state.active_primary_agent);
        let harness_config = vt_cfg.as_ref().map(|cfg| cfg.agent.harness.clone()).unwrap_or_default();
        let turn_run_id = TurnRunId(thread_handle.thread_id().to_string());
        let harness_emitter =
            super::harness::initialize_harness(&config.workspace, vt_cfg.as_ref(), &config.model, &turn_run_id).await?;

        // Once canonical persistence is open, every fallible operation must
        // finalize it before leaving this session iteration. The normal path
        // emits `thread.completed`; this macro emits an error terminal event
        // and drains the canonical sink for early error exits.
        macro_rules! harness_try {
            ($expression:expr) => {{
                match $expression {
                    Ok(value) => value,
                    Err(error) => {
                        let error: anyhow::Error = error.into();
                        if let Some(emitter) = harness_emitter.as_ref() {
                            emitter.finish_after_unexpected_exit().await;
                        }
                        return Err(error);
                    }
                }
            }};
        }

        let steering_sender = if steering_receiver.is_none() {
            let (sender, receiver) = mpsc::unbounded_channel();
            *steering_receiver = Some(receiver);
            Some(sender)
        } else {
            None
        };
        let ui_setup = initialize_session_ui(
            &config,
            vt_cfg.as_ref(),
            thread_handle.thread_id().as_str(),
            &mut session_state,
            session_trigger,
            resume_ref,
            crate::agent::runloop::unified::session_setup::SessionUiLaunchOptions {
                session_archive,
                full_auto,
                skip_confirmations,
                steering_sender,
            },
        )
        .await;
        let ui_setup = harness_try!(ui_setup);
        vtcode_commons::startup_trace::record_phase("session_setup", session_setup_phase);
        let mut renderer = ui_setup.renderer;
        let mut session = ui_setup.session;
        let handle = ui_setup.handle;

        // Load archived prompts from recent sessions into the history picker.
        // Runs in the background so the TUI starts immediately.
        {
            let handle = handle.clone();
            tokio::spawn(async move {
                load_archived_prompts_for_history(&handle).await;
            });
        }

        let mut header_context = ui_setup.header_context;
        let mut ide_context_bridge = ui_setup.ide_context_bridge;
        let ctrl_c_state = ui_setup.ctrl_c_state;
        let ctrl_c_notify = ui_setup.ctrl_c_notify;
        let input_activity_counter = ui_setup.input_activity_counter;
        let checkpoint_manager = ui_setup.checkpoint_manager;
        let mut session_archive = ui_setup.session_archive;
        let mut lifecycle_hooks = ui_setup.lifecycle_hooks;
        let mut context_manager = ui_setup.context_manager;
        let mut default_placeholder = ui_setup.default_placeholder;
        let mut follow_up_placeholder = ui_setup.follow_up_placeholder;
        let mut next_checkpoint_turn = ui_setup.next_checkpoint_turn;
        let mut session_end_reason = ui_setup.session_end_reason;
        let mut turn_id = turn_run_id.0.clone();
        let _file_palette_task_guard = ui_setup.file_palette_task_guard;
        let _background_subprocess_task_guard = ui_setup.background_subprocess_task_guard;
        let _startup_update_task_guard = ui_setup.startup_update_task_guard;
        let _editor_open_coordinator_task_guard = ui_setup.editor_open_coordinator_task_guard;
        let editor_open_sender = ui_setup.editor_open_sender;
        let startup_update_cached_notice = ui_setup.startup_update_cached_notice;
        let mut startup_update_notice_rx = ui_setup.startup_update_notice_rx;
        let SessionState {
            session_bootstrap,
            mut provider_client,
            mut tool_registry,
            tools,
            tool_catalog,
            conversation_history,
            execution,
            metadata,
            async_mcp_manager,
            mut mcp_panel_state,
            loaded_skills,
            mut active_primary_agent,
            ..
        } = session_state;
        // `initialize_session_ui` may move the archive through setup. Persist
        // again after extracting the live state so every subsequent switch is
        // anchored to the same archive metadata instance.
        persist_primary_agent(&mut session_archive, &active_primary_agent);
        let decision_ledger = metadata.decision_ledger;
        let traj = metadata.trajectory;
        let telemetry = metadata.telemetry;
        let error_recovery = metadata.error_recovery;
        let max_tool_loops = vt_cfg
            .as_ref()
            .map(|cfg| cfg.tools.max_tool_loops)
            .unwrap_or(vtcode_config::constants::tool_limits::DEFAULT_MAX_TOOL_LOOPS);
        let effective_model = crate::agent::runloop::unified::turn::turn_processing::resolve_effective_request_model(
            &config.model,
            active_primary_agent.active(),
        );
        let max_context_tokens = vtcode_core::compaction::effective_context_budget(
            vt_cfg.as_ref(),
            provider_client.as_ref(),
            &effective_model,
        );
        let mut runtime = AgentRuntime::new(
            AgentSessionState::new(
                SessionId::generate().into_inner(),
                config.max_conversation_turns,
                max_tool_loops,
                max_context_tokens,
            ),
            None,
            steering_receiver.take(),
        );
        runtime.state.messages = conversation_history.into();
        let durable_session_id = tool_registry.harness_context_snapshot().session_id;
        if let Some(envelope) = vtcode_core::compaction::memory_envelope::load_latest_memory_envelope_async(
            config.workspace.as_path(),
            &durable_session_id,
        )
        .await
        {
            if let Err(error) = runtime.restore_follow_up_state(envelope.pending_intents, envelope.applied_intent_ids) {
                tracing::warn!(%error, "durable steering queue is full; pending intents were not replayed");
            }
        }
        if resume_ref.is_some()
            && let Some(pending_prompt) = take_pending_resumed_user_prompt(runtime.state.messages_mut())
        {
            let (_, runtime_steering) = runtime.split_mut();
            if let Err(error) = runtime_steering.try_queue_follow_up_input(pending_prompt) {
                tracing::warn!(%error, "Unable to queue resumed user prompt");
            }
        }
        let tool_result_cache = execution.tool_result_cache;
        let tool_permission_cache = execution.tool_permission_cache;
        let permissions_state = execution.permissions_state;
        let approval_recorder = execution.approval_recorder;
        let safety_validator = execution.safety_validator;
        let circuit_breaker = execution.circuit_breaker;
        let tool_health_tracker = execution.tool_health_tracker;
        let rate_limiter = execution.rate_limiter;
        let validation_cache = execution.validation_cache;
        let autonomous_executor = execution.autonomous_executor;
        let cancel_token = CancellationToken::new();
        let _cancel_guard = CancelGuard(cancel_token.clone());
        let _signal_handler = spawn_signal_handler(
            ctrl_c_state.clone(),
            ctrl_c_notify.clone(),
            async_mcp_manager.clone(),
            cancel_token.clone(),
        );
        let mut session_stats = SessionStats::default();
        session_stats.circuit_breaker = circuit_breaker.clone();
        session_stats.tool_health_tracker = tool_health_tracker.clone();
        session_stats.rate_limiter = rate_limiter.clone();
        session_stats.validation_cache = validation_cache.clone();
        session_stats.set_prompt_cache_lineage_id(
            thread_handle
                .snapshot()
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.prompt_cache_lineage_id.clone()),
        );
        session_stats.set_prompt_cache_profile(
            thread_handle
                .snapshot()
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.budget_limit_continuation())
                .map(|_| vtcode_core::llm::provider::PromptCacheProfile::BudgetContinuation),
        );
        session_stats.vim_mode_enabled = vt_cfg.as_ref().is_some_and(|cfg| cfg.ui.vim_mode);
        let mut plan_session =
            crate::agent::runloop::unified::planning_workflow_state::PlanningWorkflowSessionState::default();
        if planning_entry_source.should_auto_enter() {
            transition_to_planning_workflow(
                &tool_registry,
                &mut session_stats,
                &mut plan_session,
                &handle,
                planning_entry_source,
                Some(active_primary_agent.active().name().to_string()),
                vt_cfg.as_ref().map(|cfg| cfg.default_primary_agent.clone()),
                true,
                true,
            )
            .await;
            harness_try!(render_planning_workflow_next_step_hint(&mut renderer));
        } else if planning_entry_source.requires_startup_prompt() && resume_ref.is_none() {
            let should_enter = harness_try!(
                prompt_startup_planning_workflow(&handle, &mut session, &ctrl_c_state, &ctrl_c_notify).await
            );
            if should_enter {
                transition_to_planning_workflow(
                    &tool_registry,
                    &mut session_stats,
                    &mut plan_session,
                    &handle,
                    planning_entry_source,
                    Some(active_primary_agent.active().name().to_string()),
                    vt_cfg.as_ref().map(|cfg| cfg.default_primary_agent.clone()),
                    true,
                    true,
                )
                .await;
                harness_try!(render_planning_workflow_next_step_hint(&mut renderer));
            }
        }
        let mut linked_directories: Vec<LinkedDirectory> = Vec::with_capacity(4);
        let mut model_picker_state: Option<ModelPickerState> = None;
        let mut palette_state: Option<ActivePalette> = None;
        let mut last_forced_redraw = Instant::now();
        let mut input_status_state = InputStatusState::default();
        let mut dismissed_memory_cleanup_fingerprint: Option<(usize, usize)> = None;
        let mut prefer_latest_queued_input_once = false;
        let mut queued_inputs: VecDeque<crate::agent::runloop::unified::inline_events::QueuedInput> =
            VecDeque::with_capacity(8);
        let (webmcp_prompt_sender, webmcp_prompt_receiver) = crate::agent::runloop::unified::webmcp::prompt_channel();
        let mut webmcp_prompt_receiver = Some(webmcp_prompt_receiver);
        let mut webmcp_bridge = None;
        let mut agent_touched_paths = std::collections::BTreeSet::new();
        let mut ctrl_c_notice_displayed = false;
        let mut inline_prompt_cost_notice_shown = false;
        let mut mcp_catalog_initialized = tool_registry.mcp_client().is_some();
        let mut last_known_mcp_tools: Vec<String> = Vec::with_capacity(16);
        let mut pending_mcp_refresh = false;
        let mut last_mcp_refresh = Instant::now();
        let startup_update_requested_restart = if let Some(notice) = startup_update_cached_notice.as_ref() {
            display_update_notice(&handle, &mut header_context, renderer.should_use_unicode_formatting(), notice);
            let update_outcome = harness_try!(
                run_inline_update_prompt(
                    &mut renderer,
                    &handle,
                    &mut session,
                    &ctrl_c_state,
                    &ctrl_c_notify,
                    config.workspace.as_path(),
                    notice,
                )
                .await
            );
            matches!(update_outcome, InlineUpdateOutcome::RestartRequested)
        } else {
            false
        };

        if startup_update_requested_restart {
            session_end_reason = SessionEndReason::Completed;
        }

        // Show release notes on first launch after update
        if !startup_update_requested_restart
            && let Some((ref version, ref highlights)) = session_bootstrap.release_highlights
        {
            crate::updater::display_release_notes(&handle, version, highlights);
            crate::updater::record_current_version_seen();
        }

        let mut cross_turn_tracker = crate::agent::runloop::unified::run_loop_context::CrossTurnTracker::new();
        let mut approved_plan_execution_turn = false;
        let mut pending_approved_plan_execution_input = false;
        let mut last_approved_plan_summary_status: Option<ExecutionSummaryStatus> = None;
        let mut last_turn_result: Option<RunLoopTurnLoopResult> = None;
        let mut last_turn_response_was_fallback = false;
        let mut last_turn_diagnostics = None;

        if !startup_update_requested_restart {
            loop {
                let mut executing_approved_plan = approved_plan_execution_turn;
                approved_plan_execution_turn = false;
                use crate::agent::runloop::unified::turn::session::interaction_loop::InteractionOutcome;

                // The approval turn may have ended before the handoff state
                // was fully applied (for example after recovery or a stale
                // primary-agent catalog). Re-establish the execution boundary
                // before the queued implementation turn can build its request.
                // This prevents a read-only `plan` agent from surviving the
                // approval and denying the first shell/edit call.
                if executing_approved_plan {
                    if tool_registry.is_planning_active() {
                        let plan = match crate::agent::runloop::unified::planning_workflow::load_plan_text_for_approval(
                            &tool_registry,
                        )
                        .await
                        {
                            Ok(plan) => plan,
                            Err(error) => {
                                pending_approved_plan_execution_input = false;
                                harness_try!(renderer.line(
                                    MessageStyle::Error,
                                    &format!("Approved-plan execution is blocked: {error}"),
                                ));
                                continue;
                            }
                        };
                        if let Err(error) =
                            crate::agent::runloop::unified::planning_workflow::complete_approved_plan_handoff(
                                &tool_registry,
                                &mut plan_session,
                                &handle,
                                plan,
                                active_primary_agent.active().name(),
                                session_skip_confirmations,
                                crate::agent::runloop::unified::planning_workflow::PlanExecutionContext::Current,
                            )
                            .await
                        {
                            harness_try!(
                                renderer
                                    .line(MessageStyle::Error, &format!("Approved-plan execution is blocked: {error}"))
                            );
                            pending_approved_plan_execution_input = false;
                            continue;
                        }
                    }
                    let configured_default = vt_cfg
                        .as_ref()
                        .map(|cfg| cfg.default_primary_agent.as_str())
                        .filter(|name| !name.trim().is_empty());
                    let current_agent = active_primary_agent.active().name().to_string();
                    let execution_agent = select_approved_plan_execution_agent(
                        &mut active_primary_agent,
                        &tool_registry,
                        &config.workspace,
                        Some(current_agent.as_str()),
                        configured_default,
                    )
                    .await;
                    let execution_agent = harness_try!(execution_agent);
                    if current_agent != execution_agent {
                        harness_try!(renderer.line(
                            MessageStyle::Info,
                            &format!("Approved plan requires a write-capable agent; switching to {execution_agent}."),
                        ));
                    }
                    if let Err(err) = force_reload_workspace_config_for_execution(
                        config.workspace.as_path(),
                        &config,
                        &mut vt_cfg,
                        &mut tool_registry,
                        async_mcp_manager.as_deref(),
                    )
                    .await
                    {
                        tracing::warn!(error = %err, "Failed to reload workspace configuration before approved-plan execution");
                        harness_try!(
                            renderer.line(MessageStyle::Error, &format!("Failed to reload configuration: {err}"))
                        );
                    }
                    sync_primary_agent_permissions(&mut vt_cfg, active_primary_agent.active());
                    apply_primary_agent_tool_policy_overrides(&tool_registry, active_primary_agent.active()).await;
                    let mut runtime_sync = PrimaryAgentRuntimeSyncContext {
                        config: &config,
                        vt_cfg: vt_cfg.as_ref(),
                        thread_id: &turn_run_id.0,
                        active_primary_agent: active_primary_agent.active(),
                        lifecycle_hooks: &mut lifecycle_hooks,
                        async_mcp_manager: async_mcp_manager.as_ref(),
                        tool_registry: &mut tool_registry,
                        tools: &tools,
                        tool_catalog: &tool_catalog,
                        mcp_catalog_initialized: &mut mcp_catalog_initialized,
                        pending_mcp_refresh: &mut pending_mcp_refresh,
                        provider_client: &*provider_client,
                    };
                    harness_try!(sync_primary_agent_runtime(&mut runtime_sync).await);
                    let display = active_primary_agent.active().display_name.clone();
                    let color = active_primary_agent.active().color.clone().filter(|c| !c.trim().is_empty());
                    handle.set_primary_agent(Some(display), color);
                }

                if let Some(controller) = tool_registry.subagent_controller() {
                    controller.set_parent_messages(&runtime.state.messages).await;
                }

                let interaction_outcome = if pending_approved_plan_execution_input {
                    // An approved-plan handoff is an internal state transition,
                    // not ordinary user steering. Consume it directly so a
                    // full or reordered steering FIFO cannot leave the newly
                    // selected build agent waiting for another `continue`.
                    pending_approved_plan_execution_input = false;
                    let (input, prompt_message_index) = append_approved_plan_execution_input(&mut runtime);
                    let turn_id = SessionId::generate().into_inner();
                    InteractionOutcome::Continue {
                        input,
                        prompt_message_index: Some(prompt_message_index),
                        turn_id,
                    }
                } else if let Some(input) = runtime.run_until_idle() {
                    let turn_id = SessionId::generate().into_inner();
                    InteractionOutcome::Continue { input, prompt_message_index: None, turn_id }
                } else {
                    let mut interaction_turn_metadata_cache = None;
                    let (session_state, runtime_steering) = runtime.split_mut();
                    let mut interaction_ctx =
                        crate::agent::runloop::unified::turn::session::interaction_loop::InteractionLoopContext {
                            thread_id: &turn_run_id.0,
                            active_thread_label,
                            thread_handle: &thread_handle,
                            renderer: &mut renderer,
                            session: &mut session,
                            handle: &handle,
                            header_context: &mut header_context,
                            ide_context_bridge: &mut ide_context_bridge,
                            ctrl_c_state: &ctrl_c_state,
                            ctrl_c_notify: &ctrl_c_notify,
                            input_activity_counter: &input_activity_counter,
                            config: &mut config,
                            vt_cfg: &mut vt_cfg,
                            provider_client: &mut provider_client,
                            session_bootstrap: &session_bootstrap,
                            async_mcp_manager: &async_mcp_manager,
                            tool_registry: &mut tool_registry,
                            tools: &tools,
                            tool_catalog: &tool_catalog,
                            conversation_history: std::sync::Arc::make_mut(&mut session_state.messages),
                            agent_touched_paths: &mut agent_touched_paths,
                            decision_ledger: &decision_ledger,
                            context_manager: &mut context_manager,
                            active_primary_agent: &mut active_primary_agent,
                            session_stats: &mut session_stats,
                            plan_session: &mut plan_session,
                            mcp_panel_state: &mut mcp_panel_state,
                            linked_directories: &mut linked_directories,
                            lifecycle_hooks: &mut lifecycle_hooks,
                            full_auto,
                            skip_confirmations: session_skip_confirmations,
                            approval_recorder: &approval_recorder,
                            tool_permission_cache: &tool_permission_cache,
                            permissions_state: &permissions_state,
                            loaded_skills: &loaded_skills,
                            default_placeholder: &mut default_placeholder,
                            follow_up_placeholder: &mut follow_up_placeholder,
                            checkpoint_manager: checkpoint_manager.as_ref(),
                            tool_result_cache: &tool_result_cache,
                            traj: &traj,
                            harness_emitter: harness_emitter.as_ref(),
                            safety_validator: &safety_validator,
                            circuit_breaker: &circuit_breaker,
                            tool_health_tracker: &tool_health_tracker,
                            rate_limiter: &rate_limiter,
                            telemetry: &telemetry,
                            autonomous_executor: &autonomous_executor,
                            error_recovery: &error_recovery,
                            last_forced_redraw: &mut last_forced_redraw,
                            turn_metadata_cache: &mut interaction_turn_metadata_cache,
                            harness_config: harness_config.clone(),
                            runtime_steering,
                            webmcp_prompt_receiver: &mut webmcp_prompt_receiver,
                            webmcp_prompt_sender: &webmcp_prompt_sender,
                            webmcp_bridge: &mut webmcp_bridge,
                            startup_update_notice_rx: &mut startup_update_notice_rx,
                            editor_open_sender: &editor_open_sender,
                        };

                    let mut interaction_state =
                        crate::agent::runloop::unified::turn::session::interaction_loop::InteractionState {
                            input_status_state: &mut input_status_state,
                            dismissed_memory_cleanup_fingerprint: &mut dismissed_memory_cleanup_fingerprint,
                            queued_inputs: &mut queued_inputs,
                            prefer_latest_queued_input_once: &mut prefer_latest_queued_input_once,
                            model_picker_state: &mut model_picker_state,
                            palette_state: &mut palette_state,
                            last_known_mcp_tools: &mut last_known_mcp_tools,
                            pending_mcp_refresh: &mut pending_mcp_refresh,
                            mcp_catalog_initialized: &mut mcp_catalog_initialized,
                            last_mcp_refresh: &mut last_mcp_refresh,
                            ctrl_c_notice_displayed: &mut ctrl_c_notice_displayed,
                            inline_prompt_cost_notice_shown: &mut inline_prompt_cost_notice_shown,
                        };

                    harness_try!(
                        crate::agent::runloop::unified::turn::session::interaction_loop::run_interaction_loop(
                            &mut interaction_ctx,
                            &mut interaction_state,
                        )
                        .await
                    )
                };
                // User-driven mode switches are applied inside the interaction
                // loop. Capture them before any turn outcome can block or
                // hand off so an archive resume never falls back to Duck/Plan.
                persist_primary_agent(&mut session_archive, &active_primary_agent);
                if input_status_state.is_blocked {
                    input_status_state.is_blocked = false;
                    handle.set_placeholder(default_placeholder.clone());
                    handle.set_activity_state(ActivityState::Idle);
                }
                let (next_turn_input, completed_turn_prompt_message_index) = match interaction_outcome {
                    InteractionOutcome::Exit { reason } => {
                        session_end_reason = reason;
                        break;
                    }
                    InteractionOutcome::Resume { resume_session } => {
                        resume_state = Some(*resume_session);
                        session_end_reason = SessionEndReason::Completed;
                        break;
                    }
                    InteractionOutcome::DirectToolHandled => {
                        // Explicit `run ...` / `!cmd` interactions are direct command mode:
                        // render the tool output and wait for the next user input instead of
                        // fabricating an autonomous follow-up turn.
                        continue;
                    }
                    InteractionOutcome::Continue { input, prompt_message_index, turn_id: next_turn_id } => {
                        turn_id = next_turn_id;
                        (input, prompt_message_index)
                    }
                    InteractionOutcome::PlanApproved {
                        execution_context,
                        skip_confirmations,
                        execution_agent,
                    } => {
                        // This approval path starts the implementation turn in
                        // the same outer iteration, so mark it before the
                        // HarnessTurnState is constructed below. The queued
                        // approval path sets the equivalent flag on the next
                        // iteration.
                        executing_approved_plan = true;
                        let fresh_context = matches!(
                            execution_context,
                            crate::agent::runloop::unified::planning_workflow::PlanExecutionContext::Fresh
                        );
                        if fresh_context {
                            handle.set_activity_state(ActivityState::PreparingFreshExecutionThread);
                        }
                        let plan_seed = load_active_plan_seed(&tool_registry).await;
                        if fresh_context && plan_seed.is_none() {
                            handle.set_activity_state(ActivityState::Idle);
                            harness_try!(renderer.line(
                                MessageStyle::Error,
                                "Fresh execution could not start because the approved plan was not found. The plan was retained; please retry approval.",
                            ));
                            transition_to_planning_workflow(
                                &tool_registry,
                                &mut session_stats,
                                &mut plan_session,
                                &handle,
                                PlanningEntrySource::AgentSelection,
                                Some(active_primary_agent.active().name().to_string()),
                                vt_cfg.as_ref().map(|cfg| cfg.default_primary_agent.clone()),
                                false,
                                false,
                            )
                            .await;
                            continue;
                        }
                        let plan_seed = if fresh_context {
                            load_active_plan_seed(&tool_registry).await.or(plan_seed)
                        } else {
                            plan_seed
                        };
                        let current_context_budget = vtcode_core::compaction::effective_context_budget(
                            vt_cfg.as_ref(),
                            provider_client.as_ref(),
                            &crate::agent::runloop::unified::turn::turn_processing::resolve_effective_request_model(
                                &config.model,
                                active_primary_agent.active(),
                            ),
                        );
                        let previous_context_usage_percent =
                            context_manager.context_usage_percent(current_context_budget);
                        if fresh_context {
                            handle.set_activity_state(ActivityState::RestoringApprovedPlan);
                            runtime.clear_pending_follow_up_inputs();
                            runtime.state.clear_conversation_history();
                            context_manager.reset_for_fresh_execution();
                            session_stats.reset_for_fresh_execution();
                            let build_tool_limit = effective_max_tool_calls_for_approved_plan_execution(
                                harness_config.max_tool_calls_per_turn,
                            );
                            let max_session_turns = vt_cfg
                                .as_ref()
                                .map(|cfg| cfg.agent.max_conversation_turns)
                                .unwrap_or(vtcode_config::constants::defaults::DEFAULT_MAX_CONVERSATION_TURNS);
                            let (max_per_turn, max_per_session) =
                                resolve_safety_tool_call_limits(build_tool_limit, max_session_turns, false);
                            safety_validator.reset_for_fresh_execution(max_per_turn, max_per_session);
                            crate::agent::runloop::unified::planning_workflow::emit_context_reset(
                                harness_emitter.as_ref(),
                                turn_run_id.0.clone(),
                                turn_id.clone(),
                                previous_context_usage_percent,
                            );
                        }
                        let configured_default = vt_cfg
                            .as_ref()
                            .map(|cfg| cfg.default_primary_agent.as_str())
                            .filter(|name| !name.trim().is_empty());
                        let requested_agent = execution_agent.as_deref();
                        let resolved_execution_agent = match select_approved_plan_execution_agent(
                            &mut active_primary_agent,
                            &tool_registry,
                            &config.workspace,
                            requested_agent,
                            configured_default,
                        )
                        .await
                        {
                            Ok(agent) => agent,
                            Err(err) if fresh_context => {
                                handle.set_activity_state(ActivityState::Idle);
                                harness_try!(renderer.line(
                                    MessageStyle::Error,
                                    &format!("Fresh execution could not select a build agent: {err}"),
                                ));
                                transition_to_planning_workflow(
                                    &tool_registry,
                                    &mut session_stats,
                                    &mut plan_session,
                                    &handle,
                                    PlanningEntrySource::AgentSelection,
                                    Some(active_primary_agent.active().name().to_string()),
                                    vt_cfg.as_ref().map(|cfg| cfg.default_primary_agent.clone()),
                                    false,
                                    false,
                                )
                                .await;
                                continue;
                            }
                            Err(err) => {
                                tracing::error!(error = %err, "approved-plan execution agent selection failed");
                                session_end_reason = SessionEndReason::Error;
                                break;
                            }
                        };
                        if requested_agent != Some(resolved_execution_agent.as_str()) {
                            tracing::warn!(
                                requested_agent = ?requested_agent,
                                resolved_agent = ?resolved_execution_agent,
                                "Approved plan requested a non-executable primary agent; using a write-capable agent"
                            );
                            harness_try!(renderer.line(
                                MessageStyle::Info,
                                &format!(
                                    "Approved plan requires a write-capable agent; switching to {}.",
                                    resolved_execution_agent
                                ),
                            ));
                        }
                        if let Err(err) = force_reload_workspace_config_for_execution(
                            config.workspace.as_path(),
                            &config,
                            &mut vt_cfg,
                            &mut tool_registry,
                            async_mcp_manager.as_deref(),
                        )
                        .await
                        {
                            tracing::warn!("Failed to reload workspace configuration at plan approval: {}", err);
                            harness_try!(
                                renderer.line(MessageStyle::Error, &format!("Failed to reload configuration: {err}"))
                            );
                        }

                        sync_primary_agent_permissions(&mut vt_cfg, active_primary_agent.active());
                        apply_primary_agent_tool_policy_overrides(&tool_registry, active_primary_agent.active()).await;
                        let mut runtime_sync = PrimaryAgentRuntimeSyncContext {
                            config: &config,
                            vt_cfg: vt_cfg.as_ref(),
                            thread_id: &turn_run_id.0,
                            active_primary_agent: active_primary_agent.active(),
                            lifecycle_hooks: &mut lifecycle_hooks,
                            async_mcp_manager: async_mcp_manager.as_ref(),
                            tool_registry: &mut tool_registry,
                            tools: &tools,
                            tool_catalog: &tool_catalog,
                            mcp_catalog_initialized: &mut mcp_catalog_initialized,
                            pending_mcp_refresh: &mut pending_mcp_refresh,
                            provider_client: &*provider_client,
                        };
                        if let Err(err) = sync_primary_agent_runtime(&mut runtime_sync).await {
                            if fresh_context {
                                handle.set_activity_state(ActivityState::Idle);
                                harness_try!(renderer.line(
                                    MessageStyle::Error,
                                    &format!("Fresh execution could not restore the build runtime: {err}"),
                                ));
                                transition_to_planning_workflow(
                                    &tool_registry,
                                    &mut session_stats,
                                    &mut plan_session,
                                    &handle,
                                    PlanningEntrySource::AgentSelection,
                                    Some(active_primary_agent.active().name().to_string()),
                                    vt_cfg.as_ref().map(|cfg| cfg.default_primary_agent.clone()),
                                    false,
                                    false,
                                )
                                .await;
                                continue;
                            }
                            tracing::error!(error = %err, "approved-plan runtime restoration failed");
                            session_end_reason = SessionEndReason::Error;
                            break;
                        }
                        let execution_display = active_primary_agent.active().display_name.clone();
                        let execution_color =
                            active_primary_agent.active().color.clone().filter(|c| !c.trim().is_empty());
                        handle.set_primary_agent(Some(execution_display), execution_color);
                        session_skip_confirmations = skip_confirmations;
                        handle.set_skip_confirmations(skip_confirmations);
                        if fresh_context {
                            handle.set_activity_state(ActivityState::StartingBuild);
                        }
                        harness_try!(renderer.line(MessageStyle::Info, "Executing approved plan..."));

                        let execution_directive =
                            build_approved_plan_execution_prompt(execution_context, plan_seed.as_deref());
                        runtime
                            .state
                            .messages_mut()
                            .push(vtcode_core::llm::provider::Message::system(execution_directive));
                        handle.set_activity_state(ActivityState::Building);
                        let (input, prompt_message_index) = append_approved_plan_execution_input(&mut runtime);
                        (input, Some(prompt_message_index))
                    }
                };
                if next_turn_input.trim().is_empty() {
                    continue;
                }
                let (session_state, runtime_steering) = runtime.split_mut();
                let working_history = std::sync::Arc::make_mut(&mut session_state.messages);
                // Pre-fetch the unrelated dirty worktree note off the async
                // executor. `build_unrelated_dirty_worktree_note` spawns
                // blocking `git` subprocesses — see the `# Blocking` docs in
                // `git.rs`. The note is passed into `append_transient_turn_notes`
                // so the sync helper never blocks the runtime.
                let workspace_buf = config.workspace.clone();
                let touched_clone = agent_touched_paths.clone();
                let unrelated_dirty_note = match tokio::task::spawn_blocking(move || {
                    build_unrelated_dirty_worktree_note(&workspace_buf, &touched_clone)
                })
                .await
                {
                    Ok(Ok(Some(note))) => Some(note),
                    Ok(Err(err)) => {
                        tracing::warn!(
                            error = %err,
                            "Failed to inspect unrelated dirty worktree entries before turn"
                        );
                        None
                    }
                    _ => None,
                };
                let transient_system_notes = append_transient_turn_notes(
                    working_history,
                    config.workspace.as_path(),
                    &tool_registry,
                    unrelated_dirty_note,
                );
                let turn_started_at = Instant::now();
                let history_snapshot_bytes = estimate_history_bytes(working_history);
                let mut turn_metadata_cache = None;
                let planning_active = tool_registry.is_planning_active();
                // Cross-turn tracking data and aborted diagnostics are
                // extracted before harness_state goes out of scope.
                let (
                    turn_result,
                    cross_turn_read_sigs,
                    cross_turn_written,
                    cross_turn_shell_cmd,
                    cross_turn_out_of_band_progress,
                    session_limit_granted,
                    aborted_turn_diagnostics,
                ) = {
                    let mut auto_finish_planning_attempted = false;
                    let max_tool_calls_per_turn = if executing_approved_plan {
                        effective_max_tool_calls_for_approved_plan_execution(harness_config.max_tool_calls_per_turn)
                    } else {
                        effective_max_tool_calls_for_turn(harness_config.max_tool_calls_per_turn, planning_active)
                    };
                    let mut harness_state = HarnessTurnState::new(
                        TurnRunId(turn_run_id.0.clone()),
                        TurnId(turn_id.clone()),
                        max_tool_calls_per_turn,
                        harness_config.max_tool_wall_clock_secs,
                        harness_config.max_tool_retries,
                    );
                    harness_state.set_approved_plan_execution(executing_approved_plan);
                    let turn_loop_ctx = crate::agent::runloop::unified::turn::TurnLoopContext::new(
                        &mut renderer,
                        &handle,
                        &mut session,
                        &mut session_stats,
                        &mut plan_session,
                        &mut auto_finish_planning_attempted,
                        &mut mcp_panel_state,
                        &tool_result_cache,
                        &approval_recorder,
                        &decision_ledger,
                        &mut tool_registry,
                        &tools,
                        &tool_catalog,
                        &ctrl_c_state,
                        &ctrl_c_notify,
                        &mut context_manager,
                        &mut last_forced_redraw,
                        &mut input_status_state,
                        lifecycle_hooks.as_ref(),
                        &default_placeholder,
                        &tool_permission_cache,
                        &permissions_state,
                        &safety_validator,
                        &circuit_breaker,
                        &tool_health_tracker,
                        &rate_limiter,
                        &telemetry,
                        &autonomous_executor,
                        &error_recovery,
                        &mut harness_state,
                        harness_emitter.as_ref(),
                        &mut config,
                        vt_cfg.as_ref(),
                        &mut turn_metadata_cache,
                        &mut provider_client,
                        &traj,
                        &active_primary_agent,
                        session_skip_confirmations,
                        full_auto,
                        runtime_steering,
                    );

                    let primary_agent_snapshot = active_primary_agent.active().clone();
                    let result =
                        crate::agent::runloop::unified::turn::run_turn_loop(working_history, turn_loop_ctx).await;

                    // Prompt overlays can receive queued mode events while the
                    // turn owns the input surface. Restore the write-capable
                    // agent selected at prompt time before handling the turn
                    // outcome; legitimate handoffs are applied below from the
                    // explicit outcome, not through leaked UI input.
                    if active_primary_agent.active() != &primary_agent_snapshot {
                        active_primary_agent.restore_snapshot(primary_agent_snapshot);
                    }

                    // Preserve authoritative turn state even when the turn
                    // loop fails before it can construct a normal outcome.
                    let aborted_turn_diagnostics = result
                        .is_err()
                        .then(|| harness_state.snapshot_turn_diagnostics(Default::default(), 0));
                    (
                        result,
                        harness_state
                            .seen_successful_readonly_signatures
                            .iter()
                            .cloned()
                            .collect::<Vec<_>>(),
                        harness_state.recently_written_files.clone(),
                        harness_state.last_admitted_shell_command_signature.clone(),
                        harness_state.has_out_of_band_tool_progress(),
                        harness_state.has_session_limit_grant(),
                        aborted_turn_diagnostics,
                    )
                };
                let outcome = match turn_result {
                    Ok(outcome) => outcome,
                    Err(err) => {
                        crate::agent::runloop::unified::status_line::clear_input_status(
                            &handle,
                            &mut input_status_state,
                        );
                        handle.set_activity_state(ActivityState::Idle);
                        let _ = renderer.line_if_not_empty(MessageStyle::Output);
                        tracing::error!("Turn execution error: {}", err);
                        let _ = renderer.line(MessageStyle::Error, &format!("Error: {err}"));
                        TurnLoopOutcome {
                            result: RunLoopTurnLoopResult::Aborted,
                            turn_modified_files: std::collections::BTreeSet::new(),
                            turn_touched_files: std::collections::BTreeSet::new(),
                            turn_diagnostics: aborted_turn_diagnostics.unwrap_or_default(),
                            pending_primary_agent: None,
                            pending_plan_auto_accept: false,
                            pending_plan_execution_context:
                                crate::agent::runloop::unified::planning_workflow::PlanExecutionContext::Current,
                            plan_approved_execution_pending: false,
                            final_response_was_fallback: false,
                        }
                    }
                };
                if session_limit_granted && !matches!(&outcome.result, RunLoopTurnLoopResult::Blocked { .. }) {
                    // A successful grant resumes the pending Build turn. Do
                    // not leave the previous blocked placeholder/state visible
                    // or let a grant-in-flight turn be finalized as no response.
                    input_status_state.is_blocked = false;
                    handle.set_placeholder(default_placeholder.clone());
                    handle.set_activity_state(ActivityState::Idle);
                }
                remove_transient_system_notes(working_history, &transient_system_notes);

                // Cross-turn loop detection: fingerprint this turn's actions and
                // inject a warning if a loop or stuck pattern is detected.
                if let Some(cross_turn_warning) = cross_turn_tracker.seal_turn_with_progress(
                    &cross_turn_read_sigs,
                    &cross_turn_written,
                    cross_turn_shell_cmd.as_deref(),
                    cross_turn_out_of_band_progress,
                    planning_active,
                ) {
                    tracing::warn!(warning = %cross_turn_warning, "Cross-turn loop detector triggered");
                    working_history.push(vtcode_core::llm::provider::Message::system(cross_turn_warning));
                }

                agent_touched_paths.extend(
                    outcome
                        .turn_modified_files
                        .iter()
                        .map(|path| normalize_workspace_path(config.workspace.as_path(), path)),
                );
                agent_touched_paths.extend(context_manager.tracked_instruction_activity_paths());
                let outcome_result = outcome.result.clone();
                let execution_modified_files = outcome.turn_modified_files.clone();
                let switch_primary_agent = outcome.pending_primary_agent.clone();
                let has_primary_agent_switch = switch_primary_agent.is_some();
                let plan_auto_accept = outcome.pending_plan_auto_accept;
                let plan_execution_context = outcome.pending_plan_execution_context;
                let plan_approved_execution_pending = outcome.plan_approved_execution_pending;
                let final_response_was_fallback = outcome.final_response_was_fallback;
                last_turn_result = Some(outcome_result.clone());
                last_turn_response_was_fallback = final_response_was_fallback;
                let turn_elapsed = turn_started_at.elapsed();
                let mut turn_diagnostics = outcome.turn_diagnostics.clone();
                turn_diagnostics.elapsed_ms = turn_elapsed.as_millis().min(u128::from(u64::MAX)) as u64;
                last_turn_diagnostics = Some(turn_diagnostics.clone());
                let show_turn_timer = vt_cfg.as_ref().map(|cfg| cfg.ui.show_turn_timer).unwrap_or(true);
                let harness_snapshot = tool_registry.harness_context_snapshot();
                if let Err(err) = crate::agent::runloop::unified::turn::apply_turn_outcome(
                    outcome,
                    crate::agent::runloop::unified::turn::TurnOutcomeContext {
                        conversation_history: std::sync::Arc::make_mut(&mut runtime.state.messages),
                        completed_turn_prompt: Some(next_turn_input.as_str()),
                        completed_turn_prompt_message_index,
                        renderer: &mut renderer,
                        handle: &handle,
                        ctrl_c_state: &ctrl_c_state,
                        default_placeholder: &default_placeholder,
                        checkpoint_manager: checkpoint_manager.as_ref(),
                        next_checkpoint_turn: &mut next_checkpoint_turn,
                        session_end_reason: &mut session_end_reason,
                        turn_elapsed,
                        show_turn_timer,
                        session_id: &harness_snapshot.session_id,
                        runtime_turn_id: Some(&turn_id),
                        harness_emitter: harness_emitter.as_ref(),
                    },
                )
                .await
                {
                    tracing::error!("Failed to apply turn outcome: {}", err);
                    renderer
                        .line(MessageStyle::Error, &format!("Failed to finalize turn: {err}"))
                        .ok();
                }
                if executing_approved_plan {
                    handle.set_activity_state(ActivityState::Idle);
                }
                // Plan-mode "switch to build/auto agent" handoff: perform the
                // primary-agent switch now so the chosen agent executes the plan.
                // This mirrors the `PlanApproved` handoff in the interaction loop
                // (session.rs): mutate `active_primary_agent` and refresh the TUI
                // handle display. The full `handle_select_primary_agent` requires
                // `InteractionLoopContext`, which is unavailable here because the
                // plan-confirmation popup is rendered inside the turn loop rather
                // than the inline interaction loop.
                if let Some(requested_agent) = switch_primary_agent {
                    let configured_default = vt_cfg
                        .as_ref()
                        .map(|cfg| cfg.default_primary_agent.as_str())
                        .filter(|name| !name.trim().is_empty());
                    let execution_agent = select_approved_plan_execution_agent(
                        &mut active_primary_agent,
                        &tool_registry,
                        &config.workspace,
                        Some(requested_agent.as_str()),
                        configured_default,
                    )
                    .await;
                    let execution_agent = harness_try!(execution_agent);
                    if execution_agent != requested_agent {
                        tracing::warn!(
                            requested_agent = %requested_agent,
                            resolved_agent = %execution_agent,
                            "Approved plan requested a non-executable primary agent; using a write-capable agent"
                        );
                        harness_try!(renderer.line(
                            MessageStyle::Info,
                            &format!("Approved plan requires a write-capable agent; switching to {}.", execution_agent),
                        ));
                    }
                    // The approval choice, rather than the destination agent
                    // name, owns confirmation policy. This keeps a manual
                    // Execute/Switch Build handoff prompting even if an
                    // earlier agent or fallback happens to be named `auto`.
                    session_skip_confirmations = plan_auto_accept;
                    handle.set_skip_confirmations(session_skip_confirmations);
                    sync_primary_agent_permissions(&mut vt_cfg, active_primary_agent.active());
                    apply_primary_agent_tool_policy_overrides(&tool_registry, active_primary_agent.active()).await;
                    let mut runtime_sync = PrimaryAgentRuntimeSyncContext {
                        config: &config,
                        vt_cfg: vt_cfg.as_ref(),
                        thread_id: &turn_run_id.0,
                        active_primary_agent: active_primary_agent.active(),
                        lifecycle_hooks: &mut lifecycle_hooks,
                        async_mcp_manager: async_mcp_manager.as_ref(),
                        tool_registry: &mut tool_registry,
                        tools: &tools,
                        tool_catalog: &tool_catalog,
                        mcp_catalog_initialized: &mut mcp_catalog_initialized,
                        pending_mcp_refresh: &mut pending_mcp_refresh,
                        provider_client: &*provider_client,
                    };
                    harness_try!(sync_primary_agent_runtime(&mut runtime_sync).await);
                    let display = active_primary_agent.active().display_name.clone();
                    let color = active_primary_agent.active().color.clone().filter(|c| !c.trim().is_empty());
                    handle.set_primary_agent(Some(display), color);
                    tracing::info!(
                        target: "vtcode.planning_workflow",
                        agent = %execution_agent,
                        "Switched primary agent after plan approval"
                    );
                    persist_primary_agent(&mut session_archive, &active_primary_agent);
                }
                if plan_approved_execution_pending && !has_primary_agent_switch {
                    session_skip_confirmations = plan_auto_accept;
                    handle.set_skip_confirmations(session_skip_confirmations);
                }
                if plan_approved_execution_pending {
                    let fresh_context = matches!(
                        plan_execution_context,
                        crate::agent::runloop::unified::planning_workflow::PlanExecutionContext::Fresh
                    );
                    if fresh_context {
                        handle.set_activity_state(ActivityState::PreparingFreshExecutionThread);
                    }
                    let mut plan_seed = load_active_plan_seed(&tool_registry).await;
                    if fresh_context && plan_seed.is_none() {
                        handle.set_activity_state(ActivityState::Idle);
                        harness_try!(renderer.line(
                            MessageStyle::Error,
                            "Fresh execution could not start because the approved plan was not found. The plan was retained; please retry approval.",
                        ));
                        continue;
                    }
                    if fresh_context {
                        let current_context_budget = vtcode_core::compaction::effective_context_budget(
                            vt_cfg.as_ref(),
                            provider_client.as_ref(),
                            &crate::agent::runloop::unified::turn::turn_processing::resolve_effective_request_model(
                                &config.model,
                                active_primary_agent.active(),
                            ),
                        );
                        let previous_context_usage_percent =
                            context_manager.context_usage_percent(current_context_budget);
                        plan_seed = load_active_plan_seed(&tool_registry).await.or(plan_seed.take());
                        handle.set_activity_state(ActivityState::RestoringApprovedPlan);
                        runtime.clear_pending_follow_up_inputs();
                        runtime.state.clear_conversation_history();
                        context_manager.reset_for_fresh_execution();
                        session_stats.reset_for_fresh_execution();
                        let build_tool_limit = effective_max_tool_calls_for_approved_plan_execution(
                            harness_config.max_tool_calls_per_turn,
                        );
                        let max_session_turns = vt_cfg
                            .as_ref()
                            .map(|cfg| cfg.agent.max_conversation_turns)
                            .unwrap_or(vtcode_config::constants::defaults::DEFAULT_MAX_CONVERSATION_TURNS);
                        let (max_per_turn, max_per_session) =
                            resolve_safety_tool_call_limits(build_tool_limit, max_session_turns, false);
                        safety_validator.reset_for_fresh_execution(max_per_turn, max_per_session);
                        crate::agent::runloop::unified::planning_workflow::emit_context_reset(
                            harness_emitter.as_ref(),
                            turn_run_id.0.clone(),
                            turn_id.clone(),
                            previous_context_usage_percent,
                        );
                        handle.set_activity_state(ActivityState::StartingBuild);
                    }
                    approved_plan_execution_turn = true;
                    pending_approved_plan_execution_input = true;
                    let execution_directive =
                        build_approved_plan_execution_prompt(plan_execution_context, plan_seed.as_deref());
                    runtime
                        .state
                        .messages_mut()
                        .push(vtcode_core::llm::provider::Message::system(execution_directive));
                    handle.set_activity_state(ActivityState::Building);
                    persist_primary_agent(&mut session_archive, &active_primary_agent);
                }
                if executing_approved_plan {
                    let summary = approved_plan_execution_summary(
                        &tool_registry,
                        &outcome_result,
                        final_response_was_fallback,
                        !execution_modified_files.is_empty(),
                    )
                    .await;
                    last_approved_plan_summary_status = Some(summary.status);
                    let changed_files =
                        format_workspace_relative_paths(config.workspace.as_path(), &execution_modified_files);
                    let _ = renderer.line(
                        MessageStyle::Info,
                        &format!(
                            "Execution summary: {}; changed files: {changed_files}; verification: see the final response and task tracker; blockers: {}.",
                            summary.status.as_str(),
                            summary.blocker.as_deref().unwrap_or("none")
                        ),
                    );
                }
                emit_turn_execution_metrics(TurnExecutionMetrics {
                    attempts_made: 1,
                    retry_count: 0,
                    history_snapshot_bytes,
                    timeout_secs: harness_config.max_tool_wall_clock_secs,
                    elapsed_ms: turn_elapsed.as_millis(),
                    outcome: match &outcome_result {
                        RunLoopTurnLoopResult::Completed { .. } => "completed",
                        RunLoopTurnLoopResult::Aborted => "aborted",
                        RunLoopTurnLoopResult::Cancelled => "cancelled",
                        RunLoopTurnLoopResult::Exit => "exit",
                        RunLoopTurnLoopResult::Blocked { .. } => "blocked",
                    },
                });

                last_activity_time = Some(Instant::now());
                vtcode_core::tools::cache::FILE_CACHE.check_pressure_and_evict().await;
                tool_result_cache.write().await.check_pressure_and_evict();
                let blocked_turn = matches!(&outcome_result, RunLoopTurnLoopResult::Blocked { .. });
                let mut checkpoint_outcome = SessionCheckpointOutcome::without_archive(blocked_turn);
                persist_primary_agent(&mut session_archive, &active_primary_agent);
                if let Some(archive) = session_archive.as_ref() {
                    let messages: Vec<SessionMessage> =
                        runtime.state.messages.iter().map(SessionMessage::from).collect();
                    let mut recent_messages: Vec<SessionMessage> = runtime
                        .state
                        .messages
                        .iter()
                        .rev()
                        .take(RECENT_MESSAGE_LIMIT)
                        .map(SessionMessage::from)
                        .collect();
                    recent_messages.reverse();

                    let progress_turn = next_checkpoint_turn.saturating_sub(1).max(1);
                    let distinct_tools = session_stats.sorted_tools();
                    let skill_names: Vec<String> = loaded_skills.read().await.keys().cloned().collect();
                    let checkpoint_args = SessionProgressArgs {
                        total_messages: runtime.state.messages.len(),
                        distinct_tools,
                        messages,
                        recent_messages,
                        turn_number: progress_turn,
                        token_usage: None,
                        max_context_tokens: None,
                        loaded_skills: Some(skill_names),
                        turn_diagnostics: Some(turn_diagnostics),
                    };
                    checkpoint_outcome = persist_session_checkpoint(archive, checkpoint_args, blocked_turn).await;
                }
                let steering_update = {
                    let (_, steering) = runtime.split_mut();
                    if checkpoint_outcome.history_checkpoint_succeeded() {
                        steering.acknowledge_durable_follow_up_intents();
                    } else if session_archive.is_none() || checkpoint_outcome.history_persistence_disabled() {
                        steering.release_in_flight_follow_up_intents_without_persistence();
                    }
                    vtcode_core::compaction::memory_envelope::SessionMemoryEnvelopeUpdate {
                        pending_intents: Some(steering.pending_follow_up_intents_snapshot()),
                        applied_intent_ids: steering.applied_follow_up_intent_ids().iter().cloned().collect(),
                        ..Default::default()
                    }
                };
                if let Err(err) =
                    crate::agent::runloop::unified::turn::compaction::refresh_session_memory_envelope_async(
                        config.workspace.as_path(),
                        &harness_snapshot.session_id,
                        vt_cfg.as_ref(),
                        &runtime.state.messages,
                        &session_stats,
                        Some(&steering_update),
                    )
                    .await
                {
                    tracing::warn!(
                        error = %err,
                        session_id = %harness_snapshot.session_id,
                        "Failed to refresh session memory envelope after turn"
                    );
                }
                if let RunLoopTurnLoopResult::Blocked { reason } = &outcome_result {
                    write_blocked_handoff_after_checkpoint(
                        &config.workspace,
                        &harness_snapshot.session_id,
                        reason.as_deref().unwrap_or("Turn blocked due to repeated failing behavior."),
                        checkpoint_outcome.blocked_handoff_resume(),
                        &mut renderer,
                        harness_emitter.as_ref(),
                        Some(&handle),
                    );
                }
                match &outcome_result {
                    RunLoopTurnLoopResult::Completed { .. } => {
                        let handoff_resolved =
                            vtcode_core::core::agent::blocked_handoff::clear_current_blocked_handoff_for_session(
                                &config.workspace,
                                &harness_snapshot.session_id,
                            );
                        match handoff_resolved {
                            Ok(true) => {
                                if let Some(emitter) = harness_emitter.as_ref() {
                                    let _ = emitter.emit(harness_event(
                                        vtcode_core::exec::events::HarnessEventKind::BlockedHandoffResolved,
                                        Some("Blocked handoff resolved".to_owned()),
                                        None,
                                        None,
                                        None,
                                    ));
                                }
                            }
                            Ok(false) => {}
                            Err(err) => tracing::warn!(error = %err, "Failed to resolve current blocked handoff"),
                        }
                        session_stats.mark_turn_stalled(false, None);
                    }
                    RunLoopTurnLoopResult::Aborted => {
                        session_stats
                            .mark_turn_stalled(true, Some("Turn aborted due to an execution error.".to_string()));
                    }
                    RunLoopTurnLoopResult::Blocked { reason } => {
                        handle.set_placeholder(Some(
                            "Turn blocked · Type 'continue' to retry or describe changes...".to_string(),
                        ));
                        input_status_state.is_blocked = true;
                        session_stats.mark_turn_stalled(
                            true,
                            reason
                                .clone()
                                .or_else(|| Some("Turn blocked due to repeated failing tool behavior.".to_string())),
                        );
                        if !renderer.supports_inline_ui()
                            && session_stats.auto_permission_prompt_fallback_active()
                            && session_stats.last_auto_permission_denial().is_some()
                        {
                            session_end_reason = SessionEndReason::Error;
                            break;
                        }
                    }
                    _ => {
                        session_stats.mark_turn_stalled(false, None);
                    }
                }
                if matches!(session_end_reason, SessionEndReason::Exit) {
                    break;
                }
                continue;
            }
        }
        if let Some(archive) = session_archive.as_mut() {
            archive.set_primary_agent(active_primary_agent.active().name());
            let skill_names: Vec<String> = loaded_skills.read().await.keys().cloned().collect();
            archive.set_loaded_skills(skill_names);
            archive.set_continuation_metadata(session_stats.budget_limit().map(|(max_budget_usd, actual_cost_usd)| {
                session_archive::SessionContinuationMetadata::budget_limit(
                    max_budget_usd,
                    actual_cost_usd,
                    crate::agent::runloop::unified::turn::compaction::has_latest_memory_envelope(
                        &config.workspace,
                        thread_handle.thread_id().as_str(),
                    ),
                )
            }));
        }
        if let Some(emitter) = harness_emitter.as_ref() {
            let harness_snapshot = tool_registry.harness_context_snapshot();
            let (outcome_code, subtype) = resolve_thread_completion_status(
                &session_end_reason,
                session_stats.budget_limit().is_some(),
                last_approved_plan_summary_status,
                last_turn_result.as_ref(),
                last_turn_response_was_fallback,
            );
            let result = subtype
                .is_success()
                .then(|| latest_assistant_result_text(&runtime.state.messages))
                .flatten();
            let total_cost_usd = session_stats.total_cost_usd().and_then(serde_json::Number::from_f64);
            let event = crate::agent::runloop::unified::inline_events::harness::thread_completed_event(
                turn_run_id.0.clone(),
                harness_snapshot.session_id,
                subtype,
                outcome_code,
                result,
                session_stats.stop_reason().map(str::to_string),
                session_stats.total_usage(),
                total_cost_usd,
                session_stats.total_turns(),
            );
            // Always attempt exporter finalization after the terminal event.
            // Optional exporters are isolated inside `HarnessEventEmitter`,
            // while a canonical persistence error is retained and returned
            // only after `finish()` has had a chance to drain/close the sink.
            let terminal_event_error = emitter
                .emit(event)
                .err()
                .map(|error| error.context("failed to emit canonical thread.completed event"));
            // Fire one best-effort session-completion notification, mirroring the
            // per-turn outcome helper. Reuses the same completion_success/failure
            // gates as turn completion (no separate session config).
            super::notifications::emit_session_completion_notification(subtype, session_stats.total_turns()).await;
            if let Err(error) = emitter.finish().await {
                tracing::error!(
                    target: "vtcode.harness",
                    phase = "canonical_finish",
                    error = %error,
                    "failed to finalize canonical session persistence"
                );
                if terminal_event_error.is_none() {
                    return Err(error.context("failed to finalize canonical session persistence"));
                }
            }
            if let Some(error) = terminal_event_error {
                return Err(error);
            }
        }
        agent_touched_paths.extend(context_manager.tracked_instruction_activity_paths());
        // Skip persistent memory on interrupt-exits (it makes LLM API calls which
        // delay shutdown significantly). For normal exits, cap it with a timeout.
        if !matches!(session_end_reason, SessionEndReason::Exit) {
            match timeout(
                Duration::from_secs(5),
                vtcode_core::persistent_memory::finalize_persistent_memory(
                    &config,
                    vt_cfg.as_ref(),
                    &runtime.state.messages,
                ),
            )
            .await
            {
                Ok(Err(err)) => {
                    tracing::warn!("Failed to update persistent memory at session finalization: {}", err);
                }
                Err(_elapsed) => {
                    tracing::warn!("Persistent memory finalization timed out, skipping");
                }
                Ok(Ok(_)) => {}
            }
        }

        // Capture the response before finalization shuts down the inline TUI
        // and clears its screen. The owned copy remains available for the
        // plain stdout postamble after terminal restoration.
        let final_response = latest_assistant_result_text(&runtime.state.messages);
        if matches!(session_end_reason, SessionEndReason::NewSession) {
            next_session_primary_agent = Some(active_primary_agent.active().name().to_owned());
        }
        let finalization_output = match finalize_session(
            &mut renderer,
            lifecycle_hooks.as_ref(),
            &turn_id,
            session_end_reason,
            &mut session_archive,
            &session_stats,
            last_turn_diagnostics,
            &runtime.state.messages,
            linked_directories,
            async_mcp_manager.as_deref(),
            &handle,
            &mut session,
        )
        .await
        {
            Ok(output) => Some(output),
            Err(err) => {
                tracing::error!("Failed to finalize session: {}", err);
                renderer
                    .line(MessageStyle::Error, &format!("Failed to finalize session: {err}"))
                    .ok();
                None
            }
        };
        if let Some(next_resume) = resume_state.as_ref() {
            refresh_runtime_debug_context_for_next_session(config.workspace.as_path(), Some(next_resume)).await?;
            continue;
        }
        if matches!(session_end_reason, SessionEndReason::NewSession) {
            if config_watcher.should_reload() {
                if let Some(reloaded) = config_watcher.load_config() {
                    vt_cfg = Some(reloaded);
                    crate::agent::agents::apply_live_reload_overrides(vt_cfg.as_mut(), &config);
                    idle_config = extract_idle_config(vt_cfg.as_ref());
                    tracing::debug!("Configuration reloaded due to file changes");
                }
                if let Some(error) = config_watcher.take_reload_error() {
                    renderer.line(
                        MessageStyle::Warning,
                        &format!("Configuration reload rejected; keeping the last valid configuration: {error}"),
                    )?;
                }
            }

            refresh_runtime_debug_context_for_next_session(config.workspace.as_path(), None).await?;
            resume_state = None;
            pending_session_start_trigger = Some(SessionStartTrigger::NewSession);
            _consecutive_idle_cycles = 0;
            continue;
        }
        if config_watcher.should_reload() {
            if let Some(reloaded) = config_watcher.load_config() {
                vt_cfg = Some(reloaded);
                crate::agent::agents::apply_live_reload_overrides(vt_cfg.as_mut(), &config);
                idle_config = extract_idle_config(vt_cfg.as_ref());
                tracing::debug!("Configuration reloaded during idle period");
            }
            if let Some(error) = config_watcher.take_reload_error() {
                renderer.line(
                    MessageStyle::Warning,
                    &format!("Configuration reload rejected; keeping the last valid configuration: {error}"),
                )?;
            }
        }
        if idle_config.enabled
            && let Some(last_activity) = last_activity_time
        {
            let idle_duration = last_activity.elapsed().as_millis() as u64;
            if idle_duration >= idle_config.timeout_ms {
                _consecutive_idle_cycles += 1;
                if idle_config.backoff_ms > 0 {
                    if _consecutive_idle_cycles >= idle_config.max_cycles {
                        sleep(Duration::from_millis(idle_config.backoff_ms * 2)).await;
                        _consecutive_idle_cycles = 0;
                    } else {
                        sleep(Duration::from_millis(idle_config.backoff_ms)).await;
                    }
                }
            } else {
                _consecutive_idle_cycles = 0;
            }
        }

        let end_code_changes = capture_code_change_snapshot(&config.workspace, "end").await;
        let code_change_delta =
            compute_session_code_change_delta(start_code_changes.as_ref(), end_code_changes.as_ref());
        let finalization_succeeded = finalization_output.is_some();
        let resume_identifier = finalization_output
            .as_ref()
            .and_then(|output| output.archive_path.as_ref())
            .and_then(|path| path.file_stem())
            .and_then(|stem| stem.to_str());
        let trust_label = match session_bootstrap.acp_workspace_trust {
            Some(vtcode_core::config::AgentClientProtocolZedWorkspaceTrustMode::FullAuto) => "full auto",
            Some(vtcode_core::config::AgentClientProtocolZedWorkspaceTrustMode::ToolsPolicy) => "tools policy",
            None if full_auto => "full auto",
            None => "tools policy",
        };
        let provider_label = {
            let label = crate::agent::runloop::unified::session_setup::resolve_provider_label(&config, vt_cfg.as_ref());
            if label.is_empty() {
                provider_client.name().to_string()
            } else {
                label
            }
        };
        let reasoning_label = vt_cfg
            .as_ref()
            .map(|cfg| cfg.agent.reasoning_effort.as_str().to_string())
            .unwrap_or_else(|| config.reasoning_effort.as_str().to_string());
        let (code_additions, code_deletions) = code_change_delta.map(|d| (d.additions, d.deletions)).unwrap_or((0, 0));
        if !finalization_succeeded {
            let _ = vtcode_ui::tui::panic_hook::restore_tui();
        }
        let session_total_usage = session_stats.total_usage();
        print_exit_summary(ExitData {
            app_name: "VT Code",
            version: env!("CARGO_PKG_VERSION"),
            model: &config.model,
            provider: &provider_label,
            trust_label,
            reasoning: &reasoning_label,
            session_duration: session_started_at.elapsed(),
            prompt_tokens: session_total_usage.input_tokens,
            completion_tokens: session_total_usage.output_tokens,
            cached_tokens: session_total_usage.cached_input_tokens,
            cache_creation_tokens: session_total_usage.cache_creation_tokens,
            cache_hit_rate_percent: session_total_usage.cache_hit_rate().map(|rate| rate * 100.0),
            code_additions,
            code_deletions,
            final_response: final_response.as_deref(),
            resume_identifier,
            budget_limit: session_stats.budget_limit(),
        });
        if let Some(controller) = tool_registry.subagent_controller() {
            controller.signal_shutdown().await;
        }
        if matches!(session_end_reason, SessionEndReason::Error) {
            return Err(anyhow::anyhow!(
                "{}",
                session_stats
                    .turn_stall_reason()
                    .unwrap_or("Session ended with an execution error.")
            ));
        }
        break;
    }
    Ok(())
}

/// Load user prompts from recent session archives (last 24 hours) and inject
/// them into the history picker so Ctrl+R can search across sessions.
async fn load_archived_prompts_for_history(handle: &vtcode_ui::tui::app::InlineHandle) {
    let listings = match session_archive::list_recent_sessions(50).await {
        Ok(listings) => listings,
        Err(_) => return,
    };

    let mut entries = Vec::new();
    for listing in &listings {
        let session_label = listing.identifier();
        for msg in &listing.snapshot.messages {
            if msg.role != MessageRole::User {
                continue;
            }
            let content = msg.content.as_text();
            let trimmed = content.trim();
            if trimmed.is_empty() {
                continue;
            }
            // Use first line as the prompt preview
            let preview = trimmed.lines().next().unwrap_or(trimmed).chars().take(200).collect::<String>();
            // NOTE: `created_at` uses the session start time because per-message
            // timestamps are not stored in the archive format. The time label is
            // therefore an approximation of when the conversation happened.
            entries.push(ArchivedPromptEntry {
                content: preview,
                created_at: listing.snapshot.started_at,
                session_label: session_label.clone(),
            });
        }
    }

    entries.truncate(20);

    if !entries.is_empty() {
        handle.set_archived_history(entries);
    }
}
