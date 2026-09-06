mod memory_envelope;
mod recovery_preview;

pub(crate) use self::memory_envelope::{refresh_session_memory_envelope, refresh_session_memory_envelope_async};
pub(crate) use self::recovery_preview::build_recovery_context_previews_with_workspace;

pub(crate) use vtcode_core::compaction::memory_envelope::{
    MemoryEnvelopePersistence, MemoryEnvelopePlacement, SessionMemoryEnvelope, SessionMemoryEnvelopeUpdate,
    build_session_memory_envelope, build_zero_cost_summarized_fork_history, configured_retained_user_messages,
    dedup_repeated_file_reads_for_local_compaction, default_memory_envelope_path_for_session,
    derive_continuity_summary, effective_compaction_threshold, has_latest_memory_envelope,
    insert_memory_envelope_message, latest_memory_envelope_path_for_session,
    latest_memory_envelope_path_for_session_async, load_latest_memory_envelope, load_latest_memory_envelope_async,
    local_compaction_config, persist_memory_envelope_async, persist_memory_envelope_async_with_update,
    read_task_tracker_snapshot, read_task_tracker_snapshot_async, resolve_effective_compaction_threshold,
    should_persist_memory_envelope, strip_existing_memory_envelope, write_memory_envelope_to_path,
    write_memory_envelope_to_path_async,
};

// Test-only symbols referenced by the runloop compaction test suite.
#[cfg(test)]
pub(crate) use vtcode_core::compaction::memory_envelope::{
    DEDUPED_FILE_READ_NOTE, SESSION_MEMORY_ENVELOPE_SCHEMA_VERSION, inject_latest_memory_envelope,
    resolve_compaction_threshold,
};
#[cfg(test)]
pub(crate) use vtcode_core::persistent_memory::{GroundedFactRecord, dedup_latest_facts};

use anyhow::Result;
use serde_json::{Value, json};
use std::path::Path;
use vtcode_core::compaction::auto::{AutoCompactionInput, auto_compact_messages};
use vtcode_core::config::loader::VTCodeConfig;
use vtcode_core::hooks::LifecycleHookEngine;
use vtcode_core::llm::provider::{LLMProvider, Message, MessageRole};
use vtcode_core::persistent_memory::normalize_whitespace;

use crate::agent::runloop::unified::context_manager::ContextManager;
use crate::agent::runloop::unified::inline_events::harness::{HarnessEventEmitter, compact_boundary_event};
use crate::agent::runloop::unified::state::SessionStats;
use vtcode_core::core::agent::request_envelope::SegmentBoundaryReason;

const RECOVERY_PREVIEW_MAX_CHARS: usize = 220;
const RECOVERY_PREVIEW_MAX_TOOL_OUTPUTS: usize = 3;
const RECOVERY_PREVIEW_MAX_TOTAL_CHARS: usize = 4 * 1024;
const RECOVERY_PREVIEW_USER_LABEL: &str = "Latest user request";
const RECOVERY_PREVIEW_TOOL_LABEL: &str = "Latest tool output";
const RECOVERY_PREVIEW_ASSISTANT_LABEL: &str = "Latest assistant text";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CompactionEnvelopeMode {
    persistence: MemoryEnvelopePersistence,
    placement: MemoryEnvelopePlacement,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompactionOutcome {
    pub original_len: usize,
    pub compacted_len: usize,
    pub mode: vtcode_core::exec::events::CompactionMode,
    pub history_artifact_path: Option<String>,
}

#[derive(Clone, Copy)]
pub(crate) struct CompactionContext<'a> {
    provider: &'a dyn LLMProvider,
    model: &'a str,
    session_id: &'a str,
    thread_id: &'a str,
    workspace_root: &'a Path,
    vt_cfg: Option<&'a VTCodeConfig>,
    lifecycle_hooks: Option<&'a LifecycleHookEngine>,
    harness_emitter: Option<&'a HarnessEventEmitter>,
}

impl<'a> CompactionContext<'a> {
    pub(crate) fn new(
        provider: &'a dyn LLMProvider,
        model: &'a str,
        session_id: &'a str,
        thread_id: &'a str,
        workspace_root: &'a Path,
        vt_cfg: Option<&'a VTCodeConfig>,
        lifecycle_hooks: Option<&'a LifecycleHookEngine>,
        harness_emitter: Option<&'a HarnessEventEmitter>,
    ) -> Self {
        Self {
            provider,
            model,
            session_id,
            thread_id,
            workspace_root,
            vt_cfg,
            lifecycle_hooks,
            harness_emitter,
        }
    }
}

pub(crate) struct CompactionState<'a> {
    history: &'a mut Vec<Message>,
    session_stats: &'a mut SessionStats,
    context_manager: &'a mut ContextManager,
    steering_update: Option<SessionMemoryEnvelopeUpdate>,
}

impl<'a> CompactionState<'a> {
    pub(crate) fn new(
        history: &'a mut Vec<Message>,
        session_stats: &'a mut SessionStats,
        context_manager: &'a mut ContextManager,
    ) -> Self {
        Self {
            history,
            session_stats,
            context_manager,
            steering_update: None,
        }
    }

    pub(crate) fn with_steering_update(mut self, steering_update: SessionMemoryEnvelopeUpdate) -> Self {
        self.steering_update = Some(steering_update);
        self
    }

    fn with_optional_steering_update(mut self, steering_update: Option<SessionMemoryEnvelopeUpdate>) -> Self {
        self.steering_update = steering_update;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CompactionPlan {
    trigger: vtcode_core::exec::events::CompactionTrigger,
    boundary_reason: SegmentBoundaryReason,
    envelope_mode: CompactionEnvelopeMode,
}

fn boundary_reason_for_compaction_trigger(
    trigger: vtcode_core::exec::events::CompactionTrigger,
) -> SegmentBoundaryReason {
    match trigger {
        vtcode_core::exec::events::CompactionTrigger::Recovery => SegmentBoundaryReason::Recovery,
        vtcode_core::exec::events::CompactionTrigger::ModelSwitch => SegmentBoundaryReason::Model,
        vtcode_core::exec::events::CompactionTrigger::Auto
        | vtcode_core::exec::events::CompactionTrigger::Manual
        | vtcode_core::exec::events::CompactionTrigger::Unknown => SegmentBoundaryReason::Compaction,
    }
}

#[allow(
    clippy::cast_sign_loss,
    reason = "Intentional compatibility, platform, or test-only suppression."
)] // context_size is usize (non-negative), ratio is positive
pub(crate) fn build_server_compaction_context_management(
    configured_threshold: Option<u64>,
    provider_context_size: usize,
    session_context_budget: usize,
) -> Option<Value> {
    resolve_effective_compaction_threshold(configured_threshold, provider_context_size, session_context_budget).map(
        |compact_threshold| {
            json!([{
                "type": "compaction",
                "compact_threshold": compact_threshold,
            }])
        },
    )
}

pub(crate) async fn build_summarized_fork_history(
    provider: &dyn LLMProvider,
    model: &str,
    source_session_id: &str,
    target_session_id: &str,
    workspace_root: &Path,
    vt_cfg: Option<&VTCodeConfig>,
    source_history: &[Message],
    prefer_saved_summary: bool,
) -> Result<Vec<Message>> {
    if source_history.is_empty() {
        return Ok(Vec::new());
    }

    let mut source_history = source_history.to_vec();
    let source_envelope = load_latest_memory_envelope_async(workspace_root, source_session_id).await;
    if let Some(envelope) = source_envelope.as_ref() {
        strip_existing_memory_envelope(&mut source_history);
        insert_memory_envelope_message(&mut source_history, envelope, MemoryEnvelopePlacement::Start);
    }

    let context_budget = match vtcode_core::compaction::effective_context_budget(vt_cfg, provider, model) {
        0 => None,
        budget => Some(budget),
    };
    let mut compacted = if prefer_saved_summary && source_envelope.is_some() {
        build_zero_cost_summarized_fork_history(
            &source_history,
            source_envelope.as_ref(),
            configured_retained_user_messages(vt_cfg),
        )
    } else {
        let compaction_input = dedup_repeated_file_reads_for_local_compaction(&source_history);
        vtcode_core::compaction::compact_history_with_budget(
            provider,
            model,
            &compaction_input,
            &local_compaction_config(vt_cfg, true),
            context_budget,
        )
        .await?
    };

    let _ = persist_memory_envelope_async(
        workspace_root,
        target_session_id,
        vt_cfg,
        &source_history,
        &[],
        &mut compacted,
        MemoryEnvelopePersistence::InMemoryOnly,
        MemoryEnvelopePlacement::Start,
        source_envelope.as_ref(),
    )
    .await?;
    compacted = vtcode_core::compaction::bound_compacted_history_to_context(compacted, provider, model, context_budget);

    Ok(compacted)
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "Retained as a testable compatibility entry point for in-place compaction."
    )
)]
pub(crate) async fn compact_history_in_place(
    provider: &dyn LLMProvider,
    model: &str,
    session_id: &str,
    workspace_root: &Path,
    vt_cfg: Option<&VTCodeConfig>,
    history: &mut Vec<Message>,
    session_stats: &mut SessionStats,
    context_manager: &mut ContextManager,
) -> Result<Option<CompactionOutcome>> {
    compact_history_in_place_with_events(
        CompactionContext::new(provider, model, session_id, session_id, workspace_root, vt_cfg, None, None),
        CompactionState::new(history, session_stats, context_manager),
        vtcode_core::exec::events::CompactionTrigger::Manual,
    )
    .await
}

pub(crate) async fn compact_history_in_place_with_events(
    context: CompactionContext<'_>,
    state: CompactionState<'_>,
    trigger: vtcode_core::exec::events::CompactionTrigger,
) -> Result<Option<CompactionOutcome>> {
    compact_history_segment_in_place(
        context,
        state,
        CompactionPlan {
            trigger,
            boundary_reason: boundary_reason_for_compaction_trigger(trigger),
            envelope_mode: CompactionEnvelopeMode {
                persistence: MemoryEnvelopePersistence::PersistToDisk,
                placement: MemoryEnvelopePlacement::Start,
            },
        },
    )
    .await
}

pub(crate) async fn manual_compact_history_in_place(
    context: CompactionContext<'_>,
    state: CompactionState<'_>,
    options: &vtcode_core::compaction::ManualCompactionOptions,
    native_only: bool,
) -> Result<Option<CompactionOutcome>> {
    run_manual_compaction(context, state, options, native_only, vtcode_core::exec::events::CompactionTrigger::Manual)
        .await
}

/// Compact the conversation when the main session model or provider is switched
/// mid-session, so the newly selected model starts from a summary rather than
/// the outgoing model's raw trace. Mirrors `/compact` (forces `always_summarize`
/// via `local_compaction_config(vt_cfg, true)` and routes through the same
/// strategy dispatch) but is tagged with `CompactionTrigger::ModelSwitch`.
pub(crate) async fn compact_history_on_model_switch_in_place(
    context: CompactionContext<'_>,
    state: CompactionState<'_>,
) -> Result<Option<CompactionOutcome>> {
    run_manual_compaction(
        context,
        state,
        &vtcode_core::compaction::ManualCompactionOptions::default(),
        false,
        vtcode_core::exec::events::CompactionTrigger::ModelSwitch,
    )
    .await
}

async fn run_manual_compaction(
    context: CompactionContext<'_>,
    state: CompactionState<'_>,
    options: &vtcode_core::compaction::ManualCompactionOptions,
    native_only: bool,
    trigger: vtcode_core::exec::events::CompactionTrigger,
) -> Result<Option<CompactionOutcome>> {
    let CompactionContext {
        provider,
        model,
        session_id,
        thread_id,
        workspace_root,
        vt_cfg,
        lifecycle_hooks,
        harness_emitter,
    } = context;
    let CompactionState {
        history,
        session_stats,
        context_manager,
        steering_update,
    } = state;

    // `--native-only` preserves the legacy strict behavior: refuse unless the
    // provider exposes a real standalone compaction endpoint (OpenAI
    // `/responses/compact`). Without the flag, every provider proceeds via the
    // strategy dispatch (native standalone, native inline, or local summary).
    if native_only && !provider.supports_manual_openai_compaction(model) {
        anyhow::bail!(provider.manual_openai_compaction_unavailable_message(model));
    }

    let previous_response_chain_present = session_stats.previous_response_id_for(provider.name(), model).is_some();
    let mut compaction_input = history.clone();
    strip_existing_memory_envelope(&mut compaction_input);
    let original_history = compaction_input.clone();
    let context_budget = match vtcode_core::compaction::effective_context_budget(vt_cfg, provider, model) {
        0 => None,
        budget => Some(budget),
    };
    let mut compaction_options = options.clone();
    compaction_options.allow_reasoning_effort_downgrade =
        vt_cfg.is_some_and(|config| config.agent.allow_reasoning_effort_downgrade);
    let (compacted, compaction_mode) = vtcode_core::compaction::compact_history_manual_with_budget(
        provider,
        model,
        &compaction_input,
        &local_compaction_config(vt_cfg, true),
        &compaction_options,
        context_budget,
    )
    .await?;
    if compacted == compaction_input {
        return Ok(None);
    }

    apply_compacted_history(
        CompactionContext {
            provider,
            model,
            session_id,
            thread_id,
            workspace_root,
            vt_cfg,
            lifecycle_hooks,
            harness_emitter,
        },
        CompactionState::new(history, session_stats, context_manager).with_optional_steering_update(steering_update),
        CompactionPlan {
            trigger,
            boundary_reason: boundary_reason_for_compaction_trigger(trigger),
            envelope_mode: CompactionEnvelopeMode {
                persistence: MemoryEnvelopePersistence::PersistToDisk,
                placement: MemoryEnvelopePlacement::Start,
            },
        },
        original_history,
        previous_response_chain_present,
        compacted,
        compaction_mode,
        context_budget,
        true,
    )
    .await
    .map(Some)
}

pub(crate) async fn compact_history_for_recovery_in_place(
    context: CompactionContext<'_>,
    state: CompactionState<'_>,
    preserve_from_index: usize,
) -> Result<Option<CompactionOutcome>> {
    // This is a one-shot safety action requested by post-tool recovery, not
    // normal threshold-driven auto-compaction. It intentionally bypasses the
    // auto-compaction enable/suppression gates; the recovery state machine
    // bounds the call to one attempt and blocks if the prefix cannot shrink.
    compact_history_before_index_in_place(
        context,
        state,
        preserve_from_index,
        CompactionPlan {
            trigger: vtcode_core::exec::events::CompactionTrigger::Recovery,
            boundary_reason: SegmentBoundaryReason::Recovery,
            envelope_mode: CompactionEnvelopeMode {
                persistence: MemoryEnvelopePersistence::PersistToDisk,
                placement: MemoryEnvelopePlacement::Start,
            },
        },
    )
    .await
}

async fn compact_history_segment_in_place(
    context: CompactionContext<'_>,
    state: CompactionState<'_>,
    plan: CompactionPlan,
) -> Result<Option<CompactionOutcome>> {
    compact_history_segment_in_place_with_boundary(context, state, plan, true).await
}

async fn compact_history_segment_in_place_with_boundary(
    context: CompactionContext<'_>,
    state: CompactionState<'_>,
    plan: CompactionPlan,
    begin_segment: bool,
) -> Result<Option<CompactionOutcome>> {
    let CompactionContext {
        provider,
        model,
        session_id,
        thread_id,
        workspace_root,
        vt_cfg,
        lifecycle_hooks,
        harness_emitter,
    } = context;
    let CompactionState {
        history,
        session_stats,
        context_manager,
        steering_update,
    } = state;

    let previous_response_chain_present = session_stats.previous_response_id_for(provider.name(), model).is_some();
    let mut compaction_input = history.clone();
    strip_existing_memory_envelope(&mut compaction_input);
    let original_history = compaction_input.clone();

    // Route through the same strategy dispatch as the manual `/compact` command
    // (`compact_history_manual`), so auto/recovery/targeted compaction uses the
    // correct native strategy per provider instead of the legacy binary
    // `supports_responses_compaction` path. NativeInline (Anthropic) replaces a
    // hard `Err` (Anthropic does not override `compact_history`) with a graceful
    // Local fallback, so recovery never aborts.
    let config = local_compaction_config(vt_cfg, false);
    let context_budget = match vtcode_core::compaction::effective_context_budget(vt_cfg, provider, model) {
        0 => None,
        budget => Some(budget),
    };
    let manual_options = vtcode_core::compaction::ManualCompactionOptions {
        allow_reasoning_effort_downgrade: vt_cfg.is_some_and(|config| config.agent.allow_reasoning_effort_downgrade),
        ..Default::default()
    };
    let strategy = vtcode_core::compaction::manual_compaction_strategy(provider, model);
    let compaction_history = if matches!(strategy, vtcode_core::compaction::CompactionStrategy::Local) {
        dedup_repeated_file_reads_for_local_compaction(&compaction_input)
    } else {
        compaction_input.clone()
    };
    // Preserve the legacy small-segment short-circuit: auto/recovery/targeted
    // pass `always_summarize=false`, so skip compaction for tiny segments.
    if !config.always_summarize && compaction_history.len() <= config.keep_last_messages {
        return Ok(None);
    }
    let (compacted, compaction_mode) = vtcode_core::compaction::compact_history_manual_with_budget(
        provider,
        model,
        &compaction_history,
        &config,
        &manual_options,
        context_budget,
    )
    .await?;

    if compacted == compaction_history {
        return Ok(None);
    }

    apply_compacted_history(
        CompactionContext {
            provider,
            model,
            session_id,
            thread_id,
            workspace_root,
            vt_cfg,
            lifecycle_hooks,
            harness_emitter,
        },
        CompactionState::new(history, session_stats, context_manager).with_optional_steering_update(steering_update),
        plan,
        original_history,
        previous_response_chain_present,
        compacted,
        compaction_mode,
        context_budget,
        begin_segment,
    )
    .await
    .map(Some)
}

async fn apply_compacted_history(
    context: CompactionContext<'_>,
    state: CompactionState<'_>,
    plan: CompactionPlan,
    original_history: Vec<Message>,
    previous_response_chain_present: bool,
    compacted: Vec<Message>,
    compaction_mode: vtcode_core::exec::events::CompactionMode,
    context_budget: Option<usize>,
    begin_segment: bool,
) -> Result<CompactionOutcome> {
    let CompactionContext {
        provider,
        model,
        session_id,
        thread_id,
        workspace_root,
        vt_cfg,
        lifecycle_hooks,
        harness_emitter,
    } = context;
    let CompactionState {
        history,
        session_stats,
        context_manager,
        steering_update,
    } = state;

    let original_len = original_history.len();
    if let Some(lifecycle_hooks) = lifecycle_hooks {
        let outcome = lifecycle_hooks
            .run_pre_compact(plan.trigger, compaction_mode, original_len, compacted.len(), None)
            .await?;
        for message in outcome.messages {
            tracing::debug!(message = %message.text, "pre-compact hook message");
        }
    }

    let mut compacted = compacted;
    let touched_files = session_stats.recent_touched_files();
    let envelope = persist_memory_envelope_async_with_update(
        workspace_root,
        session_id,
        vt_cfg,
        &original_history,
        &touched_files,
        &mut compacted,
        plan.envelope_mode.persistence,
        plan.envelope_mode.placement,
        None,
        steering_update.as_ref(),
    )
    .await?;
    compacted = vtcode_core::compaction::bound_compacted_history_to_context(compacted, provider, model, context_budget);
    let history_artifact_path = envelope.as_ref().and_then(|item| item.history_artifact_path.clone());
    let compacted_len = compacted.len();
    let segment_transition = begin_segment.then(|| session_stats.begin_request_segment(plan.boundary_reason));
    *history = compacted;
    session_stats.clear_previous_response_chain_for(provider.name(), model);
    context_manager.take_compaction_pending();
    context_manager.reset_token_pressure_after_compaction();
    if let Some(ref envelope) = envelope {
        tracing::info!(
            provider = %provider.name(),
            model = %model,
            turn = compacted_len,
            tool_count = 0usize,
            parallelized = false,
            compaction_mode = %compaction_mode.as_str(),
            grounded_fact_count = envelope.grounded_facts.len(),
            previous_response_chain_present,
            "Injected session memory envelope"
        );
    }
    tracing::info!(
        provider = %provider.name(),
        model = %model,
        turn = original_len,
        tool_count = 0usize,
        parallelized = false,
        compaction_mode = %compaction_mode.as_str(),
        grounded_fact_count = envelope.as_ref().map_or(0, |item| item.grounded_facts.len()),
        previous_response_chain_present,
        "Applied conversation compaction"
    );
    if let Some(harness_emitter) = harness_emitter
        && let Some(segment_transition) = segment_transition.as_ref()
    {
        let event = compact_boundary_event(
            thread_id.to_string(),
            plan.trigger,
            compaction_mode,
            original_len,
            compacted_len,
            history_artifact_path.clone(),
            Some(segment_transition),
        );
        if let Err(err) = harness_emitter.emit(event) {
            tracing::debug!(error = %err, "harness compact boundary event emission failed");
        }
    }

    Ok(CompactionOutcome {
        original_len,
        compacted_len,
        mode: compaction_mode,
        history_artifact_path,
    })
}

async fn compact_history_before_index_in_place(
    context: CompactionContext<'_>,
    state: CompactionState<'_>,
    preserve_from_index: usize,
    plan: CompactionPlan,
) -> Result<Option<CompactionOutcome>> {
    let CompactionContext {
        provider,
        model,
        session_id,
        thread_id,
        workspace_root,
        vt_cfg,
        lifecycle_hooks,
        harness_emitter,
    } = context;
    let CompactionState {
        history,
        session_stats,
        context_manager,
        steering_update,
    } = state;

    if preserve_from_index == 0 {
        return Ok(None);
    }

    if preserve_from_index >= history.len() {
        return compact_history_segment_in_place(
            CompactionContext {
                provider,
                model,
                session_id,
                thread_id,
                workspace_root,
                vt_cfg,
                lifecycle_hooks,
                harness_emitter,
            },
            CompactionState::new(history, session_stats, context_manager)
                .with_optional_steering_update(steering_update),
            plan,
        )
        .await;
    }

    let original_len = history.len();
    let mut prefix = history[..preserve_from_index].to_vec();
    let suffix = history[preserve_from_index..].to_vec();
    let Some(prefix_outcome) = compact_history_segment_in_place_with_boundary(
        CompactionContext {
            provider,
            model,
            session_id,
            thread_id,
            workspace_root,
            vt_cfg,
            lifecycle_hooks,
            harness_emitter: None,
        },
        CompactionState::new(&mut prefix, session_stats, context_manager)
            .with_optional_steering_update(steering_update),
        plan,
        false,
    )
    .await?
    else {
        return Ok(None);
    };

    let segment_transition = session_stats.begin_request_segment(plan.boundary_reason);
    history.clear();
    history.extend(prefix);
    history.extend(suffix);

    let compacted_len = history.len();
    let history_artifact_path = prefix_outcome.history_artifact_path.clone();
    if let Some(harness_emitter) = harness_emitter {
        let event = compact_boundary_event(
            thread_id.to_string(),
            plan.trigger,
            prefix_outcome.mode,
            original_len,
            compacted_len,
            history_artifact_path.clone(),
            Some(&segment_transition),
        );
        if let Err(err) = harness_emitter.emit(event) {
            tracing::debug!(error = %err, "harness compact boundary event emission failed");
        }
    }

    Ok(Some(CompactionOutcome {
        original_len,
        compacted_len,
        mode: prefix_outcome.mode,
        history_artifact_path,
    }))
}

#[cfg(test)]
pub(crate) async fn compact_history_from_index_in_place(
    provider: &dyn LLMProvider,
    model: &str,
    session_id: &str,
    workspace_root: &Path,
    vt_cfg: Option<&VTCodeConfig>,
    history: &mut Vec<Message>,
    start_index: usize,
    session_stats: &mut SessionStats,
    context_manager: &mut ContextManager,
) -> Result<Option<CompactionOutcome>> {
    if start_index >= history.len() {
        return Ok(None);
    }
    let context = CompactionContext::new(provider, model, session_id, session_id, workspace_root, vt_cfg, None, None);

    if start_index == 0 {
        return compact_history_segment_in_place(
            context,
            CompactionState::new(history, session_stats, context_manager),
            CompactionPlan {
                trigger: vtcode_core::exec::events::CompactionTrigger::Manual,
                boundary_reason: SegmentBoundaryReason::Compaction,
                envelope_mode: CompactionEnvelopeMode {
                    persistence: MemoryEnvelopePersistence::InMemoryOnly,
                    placement: MemoryEnvelopePlacement::Start,
                },
            },
        )
        .await;
    }

    let prefix = history[..start_index].to_vec();
    let mut suffix = history[start_index..].to_vec();
    let Some(suffix_outcome) = compact_history_segment_in_place_with_boundary(
        context,
        CompactionState::new(&mut suffix, session_stats, context_manager),
        CompactionPlan {
            trigger: vtcode_core::exec::events::CompactionTrigger::Manual,
            boundary_reason: SegmentBoundaryReason::Compaction,
            envelope_mode: CompactionEnvelopeMode {
                persistence: MemoryEnvelopePersistence::InMemoryOnly,
                placement: MemoryEnvelopePlacement::Start,
            },
        },
        false,
    )
    .await?
    else {
        return Ok(None);
    };

    let _segment_transition = session_stats.begin_request_segment(SegmentBoundaryReason::Compaction);
    history.clear();
    history.extend(prefix);
    history.extend(suffix);

    Ok(Some(CompactionOutcome {
        original_len: start_index + suffix_outcome.original_len,
        compacted_len: start_index + suffix_outcome.compacted_len,
        mode: suffix_outcome.mode,
        history_artifact_path: suffix_outcome.history_artifact_path,
    }))
}

pub(crate) async fn maybe_auto_compact_history(
    context: CompactionContext<'_>,
    state: CompactionState<'_>,
) -> Result<Option<CompactionOutcome>> {
    let CompactionContext {
        provider,
        model,
        session_id,
        thread_id,
        workspace_root,
        vt_cfg,
        harness_emitter,
        ..
    } = context;
    let CompactionState {
        history,
        session_stats,
        context_manager,
        steering_update,
    } = state;

    let current_prompt_pressure_tokens = context_manager.current_token_usage();
    let force_compaction = context_manager.compaction_pending();
    if let Some(hard_threshold) = effective_compaction_threshold(vt_cfg, provider, model)
        && current_prompt_pressure_tokens < hard_threshold
        && !force_compaction
    {
        context_manager.mark_compaction_pending_at_soft_threshold(Some(hard_threshold));
        return Ok(None);
    }

    // Delegate to the shared compaction orchestrator (used by both runloops).
    // It enforces the `auto_compaction_enabled` gate, the token threshold, and
    // the engine + memory-envelope + artifact compression in one place.
    let mut compacted_history = history.clone();
    let manual_options = vtcode_core::compaction::ManualCompactionOptions {
        allow_reasoning_effort_downgrade: vt_cfg.is_some_and(|config| config.agent.allow_reasoning_effort_downgrade),
        ..Default::default()
    };
    let Some(outcome) = auto_compact_messages(
        AutoCompactionInput {
            provider,
            model,
            session_id,
            workspace_root,
            vt_cfg,
            current_token_usage: current_prompt_pressure_tokens,
            reserved_output_tokens: provider
                .sampling_overrides(model)
                .max_tokens
                .map_or(vtcode_core::compaction::memory_envelope::DEFAULT_OUTPUT_RESERVE_TOKENS, |value| {
                    value as usize
                }),
            touched_files: &session_stats.recent_touched_files(),
            engine_cfg: local_compaction_config(vt_cfg, false),
            manual_options,
            placement: MemoryEnvelopePlacement::BeforeLastUserOrSummary,
            prefire: Some(&session_stats.prefire),
            auto_compact_suppressed: &mut session_stats.auto_compact_suppressed,
            force_compaction,
            steering_update: steering_update.as_ref(),
        },
        &mut compacted_history,
    )
    .await?
    else {
        return Ok(None);
    };

    // Binary-specific post-step: reset response-chain and token tracking, then
    // emit the canonical `thread.compact_boundary` event.
    session_stats.clear_previous_response_chain_for(provider.name(), model);
    context_manager.reset_token_pressure_after_compaction();
    let segment_transition = session_stats.begin_request_segment(SegmentBoundaryReason::Compaction);
    *history = compacted_history;
    if let Some(harness_emitter) = harness_emitter {
        let event = compact_boundary_event(
            thread_id.to_string(),
            vtcode_core::exec::events::CompactionTrigger::Auto,
            outcome.mode,
            outcome.original_len,
            outcome.compacted_len,
            outcome.history_artifact_path.clone(),
            Some(&segment_transition),
        );
        let _ = harness_emitter.emit(event);
    }
    tracing::info!(
        provider = %provider.name(),
        model = %model,
        original_len = outcome.original_len,
        compacted_len = outcome.compacted_len,
        compaction_mode = %outcome.mode.as_str(),
        "Applied automatic conversation compaction"
    );
    Ok(Some(CompactionOutcome {
        original_len: outcome.original_len,
        compacted_len: outcome.compacted_len,
        mode: outcome.mode,
        history_artifact_path: outcome.history_artifact_path,
    }))
}

#[cfg(test)]
mod tests;
