//! Turn-snapshot capture and per-turn resolution.
//!
//! Captures [`TurnRequestSnapshot`] once per turn from `ctx`/`vt_cfg`
//! (provider name, prompt-cache shaping mode, active model, deferred-tool
//! policy, reasoning effort, etc.) so the rest of request assembly
//! (`prompt_assembly`, `tool_shaping`, `context_management`,
//! `response_chain`, and the `request_builder` orchestrator) consumes a
//! stable, already-resolved view of turn state instead of re-deriving it at
//! each call site. Invariant: the snapshot is read-only to its consumers;
//! nothing outside this module mutates a captured `TurnRequestSnapshot`.

use vtcode_core::ActivePrimaryAgent;
use vtcode_core::config::{OpenAIPromptCacheKeyMode, PromptCachingConfig};
use vtcode_core::core::agent::features::FeatureSet;
use vtcode_core::llm::provider::{self as uni};

use super::super::resolve_effective_request_model;
use crate::agent::runloop::unified::incremental_system_prompt::PromptCacheShapingMode;
use crate::agent::runloop::unified::run_loop_context::TurnExecutionSnapshot;
use crate::agent::runloop::unified::session_setup::active_deferred_tool_policy;
use crate::agent::runloop::unified::turn::context::TurnProcessingContext;

/// Default turn timeout when no explicit configuration is set (seconds).
const DEFAULT_TURN_TIMEOUT_SECS: u64 = 300;

pub(super) fn is_openai_prompt_cache_enabled(
    provider_name: &str,
    global_prompt_cache_enabled: bool,
    openai_prompt_cache_enabled: bool,
) -> bool {
    provider_name.eq_ignore_ascii_case("openai") && global_prompt_cache_enabled && openai_prompt_cache_enabled
}

pub(super) fn resolve_prompt_cache_shaping_mode(
    provider_name: &str,
    prompt_cache: &PromptCachingConfig,
) -> PromptCacheShapingMode {
    debug_assert_eq!(provider_name, provider_name.to_ascii_lowercase());

    if !prompt_cache.cache_friendly_prompt_shaping || !prompt_cache.is_provider_enabled(provider_name) {
        return PromptCacheShapingMode::Disabled;
    }

    if matches!(provider_name, "anthropic" | "minimax") {
        PromptCacheShapingMode::AnthropicBlockRuntimeContext
    } else {
        PromptCacheShapingMode::TrailingRuntimeContext
    }
}

#[derive(Clone)]
pub(super) struct TurnRequestSnapshot {
    pub provider_name: String,
    pub planning_active: bool,
    pub full_auto: bool,
    pub tool_free_recovery: bool,
    pub recovery_reason: Option<String>,
    pub request_user_input_enabled: bool,
    pub context_window_size: usize,
    pub turn_timeout_secs: u64,
    pub active_model: String,
    pub active_primary_agent: ActivePrimaryAgent,
    pub openai_prompt_cache_enabled: bool,
    pub openai_prompt_cache_key_mode: OpenAIPromptCacheKeyMode,
    pub prompt_cache_shaping_mode: PromptCacheShapingMode,
    pub capabilities: uni::ProviderCapabilities,
    pub execution: TurnExecutionSnapshot,
    /// Whether the active [`DeferredToolPolicy`] for this turn is
    /// client-local (`client_tool_search` enabled, no provider-hosted tool
    /// search available). When true, deferred tool definitions are omitted
    /// from the wire payload in [`build_turn_request`] and a `[Deferred
    /// Tools]` summary is appended to the system prompt in
    /// [`build_prompt_output`]. Always `false` for Anthropic/OpenAI hosted
    /// policies, which must keep every deferred tool on the wire.
    ///
    /// [`DeferredToolPolicy`]: vtcode_core::tools::handlers::DeferredToolPolicy
    /// [`build_turn_request`]: super::request_builder::build_turn_request
    /// [`build_prompt_output`]: super::prompt_assembly::build_prompt_output
    pub client_local_tool_deferral: bool,
}

pub(super) fn capture_turn_request_snapshot(
    ctx: &mut TurnProcessingContext<'_>,
    active_model: &str,
    tool_free_recovery: bool,
) -> TurnRequestSnapshot {
    let prompt_cache_config = &ctx.config.prompt_cache;
    let planning_active = ctx.is_planning_active();
    let provider_name = ctx.provider_client.name().to_ascii_lowercase();
    let openai_prompt_cache_enabled = is_openai_prompt_cache_enabled(
        &provider_name,
        prompt_cache_config.enabled,
        prompt_cache_config.providers.openai.enabled,
    );
    let prompt_cache_shaping_mode = resolve_prompt_cache_shaping_mode(&provider_name, prompt_cache_config);
    let request_user_input_enabled = FeatureSet::from_config(ctx.vt_cfg)
        .request_user_input_enabled(planning_active, ctx.renderer.supports_inline_ui())
        // After a permanent `request_user_input` denial (e.g. non-interactive
        // runtime), suppress the tool from the catalog so the model stops
        // seeing and retrying it. `mark_interview_denied()` is set from both
        // `handle_failure` and the permission-flow denial path.
        && !ctx.plan_session.is_interview_denied();
    let active_primary_agent = ctx.active_primary_agent.active().clone();
    let active_model = resolve_effective_request_model(active_model, &active_primary_agent);
    let context_window_size =
        vtcode_core::compaction::effective_context_budget(ctx.vt_cfg, ctx.provider_client.as_ref(), &active_model);
    let turn_timeout_secs = ctx
        .vt_cfg
        .map(|cfg| cfg.optimization.agent_execution.max_execution_time_secs)
        .unwrap_or(DEFAULT_TURN_TIMEOUT_SECS);
    let openai_prompt_cache_key_mode = prompt_cache_config.providers.openai.prompt_cache_key_mode.clone();
    let full_auto = ctx.full_auto;
    let capabilities = uni::get_cached_capabilities(&**ctx.provider_client, &active_model);
    let client_local_tool_deferral =
        active_deferred_tool_policy(&*ctx.config, ctx.vt_cfg, &**ctx.provider_client).is_client_local();

    TurnRequestSnapshot {
        provider_name,
        planning_active,
        full_auto,
        tool_free_recovery,
        recovery_reason: ctx.recovery_reason().map(str::to_string),
        request_user_input_enabled,
        context_window_size,
        turn_timeout_secs,
        active_model,
        active_primary_agent,
        openai_prompt_cache_enabled,
        openai_prompt_cache_key_mode,
        prompt_cache_shaping_mode,
        capabilities,
        execution: ctx.harness_state.execution_snapshot(),
        client_local_tool_deferral,
    }
}

#[allow(
    dead_code,
    reason = "Reasoning-effort wiring lands in a follow-up; keep resolver for that phase."
)]
pub(super) fn resolve_effective_reasoning_effort(
    cfg: Option<&vtcode_core::config::loader::VTCodeConfig>,
    turn_snapshot: &TurnRequestSnapshot,
) -> Option<vtcode_core::config::types::ReasoningEffortLevel> {
    if !turn_snapshot.capabilities.reasoning_effort || turn_snapshot.tool_free_recovery {
        return None;
    }

    turn_snapshot
        .active_primary_agent
        .reasoning_effort
        .or_else(|| cfg.map(|cfg| cfg.agent.reasoning_effort))
}
