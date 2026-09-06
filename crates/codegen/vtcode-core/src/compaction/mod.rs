use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::fmt::Write;
use vtcode_commons::llm::FinishReason;
use vtcode_config::constants::context::DEFAULT_COMPACTION_TRIGGER_RATIO;

use crate::config::types::{ReasoningEffortLevel, VerbosityLevel};
use crate::exec::events::CompactionMode;
use crate::llm::provider::{LLMProvider, LLMRequest, Message, MessageContent, MessageRole, ResponsesCompactionOptions};
use crate::llm::reasoning_effort::ReasoningEffortMapper;
use crate::llm::utils::truncate_to_token_limit;

pub mod auto;
pub mod memory_envelope;
pub mod prefire;
pub mod two_pass;

pub use crate::compaction::memory_envelope::{effective_context_budget, effective_session_context_budget};
pub use crate::compaction::prefire::{AsyncCompactionCache, PrefireState};

pub const SUPPRESS_NONE: u8 = 0;
pub const SUPPRESS_TURN: u8 = 1;
pub const SUPPRESS_STICKY: u8 = 2;
pub const SUPPRESS_UNTIL_SUCCESS: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SuppressReason {
    CreditBlock,
    Size,
    Auth,
    Schema,
    Other,
}

impl SuppressReason {
    fn suppress_state(self) -> u8 {
        match self {
            SuppressReason::Size | SuppressReason::Schema => SUPPRESS_STICKY,
            SuppressReason::CreditBlock | SuppressReason::Auth => SUPPRESS_UNTIL_SUCCESS,
            SuppressReason::Other => SUPPRESS_TURN,
        }
    }
}

/// Classify a deterministic compaction failure's error text into a fixed
/// [`SuppressReason`] (drives telemetry + sticky-vs-per-turn scope).
pub(crate) fn classify_suppress_reason(error_msg: &str) -> SuppressReason {
    let m = error_msg.to_ascii_lowercase();
    if m.contains("spending-limit")
        || m.contains("spending limit")
        || m.contains("out of credits")
        || m.contains("usage balance exhausted")
        || m.contains("usage limit reached")
    {
        SuppressReason::CreditBlock
    } else if m.contains("context length") || m.contains("too many tokens") {
        SuppressReason::Size
    } else if m.contains("status 401") || m.contains("unauthorized") {
        SuppressReason::Auth
    } else if m.contains("invalid_request_error") {
        SuppressReason::Schema
    } else {
        SuppressReason::Other
    }
}

const DEFAULT_COMPACTION_TARGET_THRESHOLD: f64 = 0.50;
const DEFAULT_COMPACTION_KEEP_LAST_MESSAGES: usize = 10;
const DEFAULT_RETAINED_USER_MESSAGE_TOKENS: usize = 20_000;
const DEFAULT_RETAINED_USER_MESSAGES: usize = 6;
/// Internal continuity budget. This is deliberately not configuration: changing
/// it changes the shape of every compacted request and therefore provider cache
/// behavior.
const CONTINUITY_TAIL_TARGET_TOKENS: usize = 20_000;
/// Keep history below the model window after reserving space for the system
/// prompt, memory envelope, summary framing, and the next response.
const COMPACTION_CONTEXT_OVERHEAD_FRACTION_DENOMINATOR: usize = 8;
const COMPACTION_CONTEXT_FIXED_OVERHEAD_TOKENS: usize = 512;
const SUMMARY_PREFIX: &str = "Previous conversation summary:\n";
const ABSTRACT_PREFIX: &str = "Earlier context (abstract):\n";
const DETAIL_PREFIX: &str = "Recent context (summary):\n";

/// Default summarization prompt. Structures the summary for continuity: after
/// reading it, the next context must feel like a seamless continuation, not a
/// fresh start. Kept as a `const` so `CompactionConfig::default` does not
/// re-allocate a ~1KB literal on every construction.
const DEFAULT_SUMMARY_PROMPT: &str = "Summarize the conversation so far using this exact structure. The goal is continuity: after reading this summary, the next context must feel like a seamless continuation of the same task, not a fresh start.\n\n## Goal\n[What the user is trying to accomplish]\n\n## Constraints & Preferences\n- [Requirements, preferences, or constraints from the user]\n\n## What I Was Just Doing\n[The single most recent action in progress: what step the agent was executing, which tool or edit was underway, and where it left off. This is the continuity anchor.]\n\n## Last Action & Result\n[The last completed action and its outcome (success, error, or partial). Include the exact error or status if relevant.]\n\n## Progress\n### Done\n- [Completed work]\n\n### In Progress\n- [Current work]\n\n### Blocked\n- [Blocking issues, if any]\n\n## Key Decisions\n- **[Decision]**: [Reason]\n\n## Next Steps\n1. [Most important next step]\n\n## Critical Context\n- [Facts needed to continue]\n\nKeep it concise and actionable. Always preserve the current task objective and acceptance criteria, file paths that were read or modified, test results and error messages, and decisions with their reasoning.";

/// Compaction configuration for context window management.
#[derive(Debug, Clone)]
pub struct CompactionConfig {
    /// Threshold (0.0-1.0) at which to trigger compaction.
    pub trigger_threshold: f64,
    /// Target usage ratio (0.0-1.0) after compaction.
    pub target_threshold: f64,
    /// Prompt for summarization.
    pub summary_prompt: String,
    /// Legacy short-circuit used to skip local compaction for tiny histories.
    pub keep_last_messages: usize,
    /// Total token budget reserved for retaining real user messages verbatim.
    pub retained_user_message_tokens: usize,
    /// Maximum number of recent user messages to retain verbatim.
    pub retained_user_messages: usize,
    /// Force local summarization even for short histories and providers with native compaction.
    pub always_summarize: bool,
    /// Enable hierarchical summarization (multi-level pyramid).
    ///
    /// When `true`, compaction produces three tiers instead of a flat summary:
    /// - **Abstract**: oldest turns compressed into 1-2 sentences
    /// - **Detail**: middle turns summarized into a paragraph
    /// - **Verbatim**: most recent turns kept as-is
    ///
    /// When `false` (default), all old turns become a single flat summary.
    pub hierarchical: bool,
    /// Auto-compaction suppression state. `SUPPRESS_NONE` (0) means compaction
    /// is allowed; any other value gates automatic compaction until the
    /// appropriate clearing event (success, model switch, etc.).
    pub auto_compact_suppressed: u8,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            trigger_threshold: DEFAULT_COMPACTION_TRIGGER_RATIO,
            target_threshold: DEFAULT_COMPACTION_TARGET_THRESHOLD,
            summary_prompt: DEFAULT_SUMMARY_PROMPT.to_string(),
            keep_last_messages: DEFAULT_COMPACTION_KEEP_LAST_MESSAGES,
            retained_user_message_tokens: DEFAULT_RETAINED_USER_MESSAGE_TOKENS,
            retained_user_messages: DEFAULT_RETAINED_USER_MESSAGES,
            always_summarize: false,
            hierarchical: false,
            auto_compact_suppressed: SUPPRESS_NONE,
        }
    }
}

/// Compact conversation history using the configured summarizer.
#[cfg_attr(feature = "profiling", hotpath::measure)]
pub async fn compact_history(
    provider: &dyn LLMProvider,
    model: &str,
    history: &[Message],
    config: &CompactionConfig,
) -> Result<Vec<Message>> {
    compact_history_with_budget(provider, model, history, config, None).await
}

/// Compact conversation history while enforcing a caller-resolved context
/// budget. A positive budget is the complete effective capacity for the
/// session (provider capacity intersected with any configured session cap),
/// so native and local compaction retain the same amount of history.
pub async fn compact_history_with_budget(
    provider: &dyn LLMProvider,
    model: &str,
    history: &[Message],
    config: &CompactionConfig,
    context_budget: Option<usize>,
) -> Result<Vec<Message>> {
    if history.is_empty() {
        return Ok(Vec::new());
    }

    if !config.always_summarize && history.len() <= config.keep_last_messages {
        // Message-count retention is only a fast path. A single large tool
        // result can exceed the resolved token budget even when the history
        // contains fewer messages than `keep_last_messages`.
        return Ok(bound_compacted_history_to_context(history.to_vec(), provider, model, context_budget));
    }

    if !config.always_summarize && provider.supports_responses_compaction(model) {
        let compacted = provider
            .compact_history(model, history)
            .await
            .context("Failed to compact history via Responses compact endpoint")?;
        return Ok(bound_compacted_history_to_context(
            normalize_provider_compacted_history(compacted, history),
            provider,
            model,
            context_budget,
        ));
    }

    let effective_config = context_bounded_compaction_config(provider, model, history, config, context_budget);
    let (summary_history, _) = split_continuity_history(history);
    let summary_prompt = build_summary_prompt(summary_history, &effective_config.summary_prompt);
    let request = LLMRequest {
        messages: std::sync::Arc::new(vec![Message::user(summary_prompt)]),
        model: model.to_string(),
        ..Default::default()
    };

    let response = provider
        .generate(request)
        .await
        .context("Failed to generate compaction summary")?;

    let summary = response.content.unwrap_or_default().trim().to_string();
    Ok(bound_compacted_history_to_context(
        build_local_compacted_history(
            history,
            &summary,
            effective_config.retained_user_message_tokens,
            effective_config.retained_user_messages,
            // Keep the same protocol-safe continuity tail as the live/manual
            // paths. Forked histories must not lose the newest working turn.
            true,
        ),
        provider,
        model,
        context_budget,
    ))
}

/// How the manual `/compact` command compacts for a given provider/model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionStrategy {
    /// Provider exposes a standalone on-demand compaction endpoint
    /// (OpenAI `/responses/compact`). Delegates to `LLMProvider::compact_history_with_options`.
    NativeStandalone,
    /// Provider compacts inline via request fields, threshold-triggered
    /// (Anthropic `compact_20260112`). Invoked through `LLMProvider::generate`
    /// with `context_management` set and `pause_after_compaction`.
    NativeInline,
    /// Universal fallback: summarize history via `LLMProvider::generate` and
    /// rebuild as a summary message plus retained recent user messages.
    /// Works for every provider.
    Local,
}

/// Select the manual-compaction strategy for a provider/model.
///
/// `NativeStandalone` when the provider opts in via `supports_manual_openai_compaction`
/// (e.g. OpenAI `/responses/compact`), `NativeInline` when the provider reports
/// inline compaction support via `supports_native_inline_compaction` (e.g.
/// Anthropic `compact_20260112`), otherwise `Local`.
///
/// Note: `supports_responses_compaction` is intentionally *not* the discriminator
/// for `NativeInline`. It is overloaded — true for both OpenAI-compatible
/// standalone compaction and Anthropic inline compaction — so OpenAI-compatible
/// custom endpoints (which report it but cannot serve an Anthropic
/// `compact_20260112` edit) would otherwise be misrouted to `NativeInline` and
/// waste a rejected `generate` call before falling back to `Local`.
pub fn manual_compaction_strategy(provider: &dyn LLMProvider, model: &str) -> CompactionStrategy {
    if provider.supports_manual_openai_compaction(model) {
        CompactionStrategy::NativeStandalone
    } else if provider.supports_native_inline_compaction(model) {
        CompactionStrategy::NativeInline
    } else {
        CompactionStrategy::Local
    }
}

/// Universally meaningful manual-compaction options.
///
/// Provider-specific extras (OpenAI `service_tier` / `prompt_cache_key` / `store` /
/// `include`) are intentionally absent: the manual `/compact` command exposes only
/// the options that apply across every provider.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManualCompactionOptions {
    /// Overrides the default summary/compaction prompt when set.
    pub instructions: Option<String>,
    /// Caps the summary/compaction output length on every provider.
    pub max_output_tokens: Option<u32>,
    /// Optional reasoning effort override for the compaction pass.
    pub reasoning_effort: Option<ReasoningEffortLevel>,
    /// Permit an explicit lower supported effort when the requested level is
    /// unavailable on the selected provider/model route. The default is
    /// strict blocking so compaction never silently changes reasoning
    /// fidelity.
    pub allow_reasoning_effort_downgrade: bool,
    /// Optional verbosity override for the compaction output.
    pub verbosity: Option<VerbosityLevel>,
}

impl From<ManualCompactionOptions> for ResponsesCompactionOptions {
    fn from(options: ManualCompactionOptions) -> Self {
        Self {
            instructions: options.instructions,
            max_output_tokens: options.max_output_tokens,
            reasoning_effort: options.reasoning_effort,
            verbosity: options.verbosity,
            responses_include: None,
            response_store: None,
            service_tier: None,
            prompt_cache_key: None,
        }
    }
}

impl CompactionConfig {
    /// Return a config with the manual options' instructions applied as the
    /// summary prompt override. The remaining option fields
    /// (`max_output_tokens`, `reasoning_effort`, `verbosity`) are applied to the
    /// summary `LLMRequest` directly by `summarize_locally`, not stored here.
    fn with_manual_overrides(self, options: &ManualCompactionOptions) -> Self {
        let summary_prompt = options
            .instructions
            .clone()
            .map(|instructions| instructions.trim().to_string())
            .filter(|instructions| !instructions.is_empty())
            .unwrap_or(self.summary_prompt);
        Self { summary_prompt, ..self }
    }
}

/// Compact history for the manual `/compact` command using provider-native
/// compaction when available, falling back to local summarization otherwise.
///
/// Returns the compacted messages and the `CompactionMode` that produced them
/// (`Provider` for native compaction, `Local` for client-side summarization).
#[cfg_attr(feature = "profiling", hotpath::measure)]
pub async fn compact_history_manual(
    provider: &dyn LLMProvider,
    model: &str,
    history: &[Message],
    config: &CompactionConfig,
    options: &ManualCompactionOptions,
) -> Result<(Vec<Message>, CompactionMode)> {
    compact_history_manual_with_budget(provider, model, history, config, options, None).await
}

/// Manual compaction with the resolved session context budget applied to every
/// provider strategy, including native results and local fallback summaries.
pub async fn compact_history_manual_with_budget(
    provider: &dyn LLMProvider,
    model: &str,
    history: &[Message],
    config: &CompactionConfig,
    options: &ManualCompactionOptions,
    context_budget: Option<usize>,
) -> Result<(Vec<Message>, CompactionMode)> {
    if history.is_empty() {
        return Ok((Vec::new(), CompactionMode::Local));
    }
    let options = resolve_manual_compaction_options(provider, model, options)?;
    match manual_compaction_strategy(provider, model) {
        CompactionStrategy::NativeStandalone => {
            let responses_options: ResponsesCompactionOptions = options.clone().into();
            let compacted = provider
                .compact_history_with_options(model, history, &responses_options)
                .await
                .context("Failed to compact history via provider-native compaction")?;
            Ok((
                bound_compacted_history_to_context(
                    normalize_provider_compacted_history(compacted, history),
                    provider,
                    model,
                    context_budget,
                ),
                CompactionMode::Provider,
            ))
        }
        CompactionStrategy::NativeInline => {
            compact_history_native_inline(provider, model, history, config, &options, context_budget).await
        }
        CompactionStrategy::Local => {
            let compacted = summarize_locally(provider, model, history, config, &options, context_budget).await?;
            Ok((compacted, CompactionMode::Local))
        }
    }
}

/// Resolve a manual compaction effort before selecting a provider strategy.
/// Every strategy eventually emits an `LLMRequest` or provider compaction
/// options, so validating once at this boundary prevents native, inline, and
/// hierarchical paths from silently coercing unsupported levels.
fn resolve_manual_compaction_options(
    provider: &dyn LLMProvider,
    model: &str,
    options: &ManualCompactionOptions,
) -> Result<ManualCompactionOptions> {
    let Some(requested) = options.reasoning_effort else {
        return Ok(options.clone());
    };

    let mapping = ReasoningEffortMapper::resolve(provider, model, requested, options.allow_reasoning_effort_downgrade)
        .with_context(|| {
            format!("Failed to resolve compaction reasoning effort for {} / {}", provider.name(), model)
        })?;

    if mapping.degraded() {
        tracing::warn!(
            provider = provider.name(),
            model,
            requested = %mapping.requested,
            effective = %mapping.effective,
            "Compaction reasoning effort explicitly downgraded"
        );
    }

    let mut resolved = options.clone();
    resolved.reasoning_effort = Some(mapping.effective);
    Ok(resolved)
}

/// Native inline compaction (Anthropic `compact_20260112`).
///
/// Forces a compaction pass by setting the minimum trigger threshold with
/// `pause_after_compaction: true`, so the response contains only the compaction
/// block. If compaction does not fire (history below the provider's minimum
/// trigger, currently 50k tokens for Anthropic), transparently falls back to
/// local summarization so the manual command always succeeds.
async fn compact_history_native_inline(
    provider: &dyn LLMProvider,
    model: &str,
    history: &[Message],
    config: &CompactionConfig,
    options: &ManualCompactionOptions,
    context_budget: Option<usize>,
) -> Result<(Vec<Message>, CompactionMode)> {
    const ANTHROPIC_COMPACT_TRIGGER_FLOOR: u64 = 50_000;

    let mut compact_edit = serde_json::Map::new();
    compact_edit.insert("type".to_string(), json!("compact_20260112"));
    compact_edit
        .insert("trigger".to_string(), json!({ "type": "input_tokens", "value": ANTHROPIC_COMPACT_TRIGGER_FLOOR }));
    compact_edit.insert("pause_after_compaction".to_string(), json!(true));
    if let Some(instructions) = options
        .instructions
        .as_ref()
        .map(|instructions| instructions.trim())
        .filter(|instructions| !instructions.is_empty())
    {
        compact_edit.insert("instructions".to_string(), json!(instructions));
    }

    let request = LLMRequest {
        messages: std::sync::Arc::new(history.to_vec()),
        model: model.to_string(),
        context_management: Some(json!({ "edits": [Value::Object(compact_edit)] })),
        max_tokens: options.max_output_tokens,
        reasoning_effort: options.reasoning_effort,
        verbosity: options.verbosity,
        ..Default::default()
    };

    // The inline compaction request is Anthropic-specific (`compact_20260112`).
    // If the provider does not actually support inline compaction (e.g. it was
    // selected because it exposes a different standalone Responses compact
    // endpoint) the request may be rejected. Per the manual `/compact` contract
    // ("always succeeds"), swallow the inline error and fall back to local
    // summarization rather than aborting the whole command.
    let response = match provider.generate(request).await {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(
                error = %error,
                "provider-native inline compaction request failed; \
                 falling back to local summarization"
            );
            let compacted = summarize_locally(provider, model, history, config, options, context_budget).await?;
            return Ok((compacted, CompactionMode::Local));
        }
    };

    if response.finish_reason == FinishReason::Pause
        && let Some(summary) = response
            .compaction
            .as_ref()
            .map(|summary| summary.trim())
            .filter(|summary| !summary.is_empty())
    {
        let effective_config = context_bounded_compaction_config(provider, model, history, config, context_budget);
        let compacted = build_summary_compacted_history(history, summary, &effective_config, true);
        return Ok((
            bound_compacted_history_to_context(compacted, provider, model, context_budget),
            CompactionMode::Provider,
        ));
    }

    // Compaction did not fire (e.g. history below the minimum trigger threshold);
    // fall back to local summarization so the manual command always succeeds.
    let compacted = summarize_locally(provider, model, history, config, options, context_budget).await?;
    Ok((compacted, CompactionMode::Local))
}

/// Local (provider-agnostic) summarization compaction.
///
/// Builds a summary prompt from the history, asks the provider to summarize via
/// `generate`, and rebuilds the history as a summary system message plus the
/// retained recent user messages. Applies the manual options to the summary
/// request.
///
/// When `config.hierarchical` is `true`, delegates to
/// [`summarize_locally_hierarchical`] which produces a multi-tier pyramid
/// (abstract + detail + verbatim) instead of a flat summary.
async fn summarize_locally(
    provider: &dyn LLMProvider,
    model: &str,
    history: &[Message],
    config: &CompactionConfig,
    options: &ManualCompactionOptions,
    context_budget: Option<usize>,
) -> Result<Vec<Message>> {
    if config.hierarchical {
        return summarize_locally_hierarchical(provider, model, history, config, options, context_budget).await;
    }

    let effective_config = context_bounded_compaction_config(
        provider,
        model,
        history,
        &config.clone().with_manual_overrides(options),
        context_budget,
    );
    let (summary_history, _) = split_continuity_history(history);
    let summary_prompt = build_summary_prompt(summary_history, &effective_config.summary_prompt);
    let request = LLMRequest {
        messages: std::sync::Arc::new(vec![Message::user(summary_prompt)]),
        model: model.to_string(),
        max_tokens: options.max_output_tokens,
        reasoning_effort: options.reasoning_effort,
        verbosity: options.verbosity,
        ..Default::default()
    };

    let response = provider
        .generate(request)
        .await
        .context("Failed to generate compaction summary")?;

    let summary = response.content.unwrap_or_default().trim().to_string();
    Ok(bound_compacted_history_to_context(
        build_summary_compacted_history(history, summary, &effective_config, true),
        provider,
        model,
        context_budget,
    ))
}

/// Hierarchical local summarization: abstract + detail + verbatim pyramid.
///
/// Splits the history into three bands and summarizes each with a different
/// compression target:
/// - **Abstract** (oldest third): 1-2 sentence overview
/// - **Detail** (middle third): paragraph-level summary
/// - **Verbatim** (newest third): kept as-is plus importance-weighted retained messages
///
/// This follows the hierarchical summarization strategy from the context window
/// management literature: recent turns verbatim, older turns as paragraph
/// summaries, oldest turns as a single abstract.
async fn summarize_locally_hierarchical(
    provider: &dyn LLMProvider,
    model: &str,
    history: &[Message],
    config: &CompactionConfig,
    options: &ManualCompactionOptions,
    context_budget: Option<usize>,
) -> Result<Vec<Message>> {
    let effective_config = context_bounded_compaction_config(
        provider,
        model,
        history,
        &config.clone().with_manual_overrides(options),
        context_budget,
    );
    let (summary_history, _) = split_continuity_history(history);

    // Split history into three bands at roughly equal thirds.
    let total = summary_history.len();
    let band_size = total / 3;
    let abstract_end = band_size;
    let detail_end = band_size * 2;

    // Band 1 (oldest): compress into 1-2 sentence abstract.
    let abstract_band = &summary_history[..abstract_end];
    let abstract_prompt = format!(
        "In 1-2 sentences, what was the overall goal and major progress in this \
         portion of the conversation?\n\n{}",
        build_summary_prompt(abstract_band, ""),
    );
    let abstract_request = LLMRequest {
        messages: std::sync::Arc::new(vec![Message::user(abstract_prompt)]),
        model: model.to_string(),
        max_tokens: Some(150),
        reasoning_effort: options.reasoning_effort,
        verbosity: options.verbosity,
        ..Default::default()
    };
    let abstract_response = provider
        .generate(abstract_request)
        .await
        .context("Failed to generate abstract summary")?;
    let abstract_summary = abstract_response.content.unwrap_or_default().trim().to_string();

    // Band 2 (middle): paragraph-level summary using the full summary prompt.
    let detail_band = &summary_history[abstract_end..detail_end];
    let detail_prompt = build_summary_prompt(detail_band, &effective_config.summary_prompt);
    let detail_request = LLMRequest {
        messages: std::sync::Arc::new(vec![Message::user(detail_prompt)]),
        model: model.to_string(),
        max_tokens: options.max_output_tokens,
        reasoning_effort: options.reasoning_effort,
        verbosity: options.verbosity,
        ..Default::default()
    };
    let detail_response = provider
        .generate(detail_request)
        .await
        .context("Failed to generate detail summary")?;
    let detail_summary = detail_response.content.unwrap_or_default().trim().to_string();

    // Band 3 (newest): retain verbatim via the bounded protocol tail.
    let recent_band = &summary_history[detail_end..];
    let retained = collect_retained_user_messages(
        recent_band,
        effective_config.retained_user_message_tokens,
        effective_config.retained_user_messages,
    );

    // Assemble: [abstract, detail, ...retained_recent, ...continuity_tail]
    let mut new_history = Vec::with_capacity(2 + retained.len());
    new_history.push(Message::system(format!("{ABSTRACT_PREFIX}{abstract_summary}")));
    new_history.push(Message::system(format!("{DETAIL_PREFIX}{detail_summary}")));
    new_history.extend(retained);
    // Live compaction: retain the most recent turn verbatim for continuity.
    for message in continuity_tail(history) {
        new_history.push(message.clone());
    }
    Ok(bound_compacted_history_to_context(new_history, provider, model, context_budget))
}

pub(crate) fn build_summary_prompt(history: &[Message], instructions: &str) -> String {
    // Pre-size for the header plus every (non-empty) message body, avoiding
    // repeated reallocations while the summary prompt is assembled.
    let estimated_len =
        instructions.len() + history.iter().map(|m| m.content.as_text().len()).sum::<usize>() + history.len() * 16;
    let mut formatted = String::with_capacity(estimated_len);
    let now: DateTime<Utc> = Utc::now();
    let _ = writeln!(&mut formatted, "Summary requested at {}.\n{}", now.to_rfc3339(), instructions);

    for message in history {
        let role = match message.role {
            MessageRole::System => "system",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
        };
        let content = message.content.as_text();
        if content.trim().is_empty() {
            continue;
        }
        let _ = writeln!(&mut formatted, "\n[{}]\n{}", role, content.trim());
    }

    formatted
}

fn compaction_history_budget(provider: &dyn LLMProvider, model: &str, context_budget: Option<usize>) -> Option<usize> {
    let context_size = context_budget
        .filter(|value| *value > 0)
        .unwrap_or_else(|| provider.effective_context_size(model));
    (context_size > 0).then(|| {
        context_size
            .saturating_sub(context_size / COMPACTION_CONTEXT_OVERHEAD_FRACTION_DENOMINATOR)
            .saturating_sub(COMPACTION_CONTEXT_FIXED_OVERHEAD_TOKENS)
    })
}

fn context_bounded_compaction_config(
    provider: &dyn LLMProvider,
    model: &str,
    history: &[Message],
    config: &CompactionConfig,
    context_budget: Option<usize>,
) -> CompactionConfig {
    let mut bounded = config.clone();
    if let Some(history_budget) = compaction_history_budget(provider, model, context_budget) {
        let continuity_tokens = continuity_tail(history).iter().map(Message::estimate_tokens).sum::<usize>();
        bounded.retained_user_message_tokens = bounded
            .retained_user_message_tokens
            .min(history_budget.saturating_sub(continuity_tokens));
    }
    bounded
}

/// Apply the model-window budget to provider-native results as well as local
/// results. Native providers can return a large prefix or metadata-heavy tool
/// calls, so retaining the tail alone is not sufficient to guarantee that the
/// next request fits.
/// Bound a compacted history to the resolved context budget, including any
/// caller-supplied session ceiling.
pub fn bound_compacted_history_to_context(
    compacted: Vec<Message>,
    provider: &dyn LLMProvider,
    model: &str,
    context_budget: Option<usize>,
) -> Vec<Message> {
    let Some(history_budget) = compaction_history_budget(provider, model, context_budget) else {
        return compacted;
    };
    if compacted.iter().map(Message::estimate_tokens).sum::<usize>() <= history_budget {
        return compacted;
    }

    let Some((tail_start, tail_end, _)) = continuity_tail_selection(&compacted) else {
        return compacted
            .into_iter()
            .scan(history_budget, |remaining, message| {
                if *remaining < 4 {
                    return None;
                }
                let bounded = bounded_message_preview(&message, *remaining);
                let used = bounded.estimate_tokens();
                if used > *remaining {
                    return None;
                }
                *remaining -= used;
                Some(bounded)
            })
            .collect();
    };

    let raw_tail = &compacted[tail_start..tail_end];
    let raw_tail_tokens = raw_tail.iter().map(Message::estimate_tokens).sum::<usize>();
    let tail = if raw_tail_tokens > history_budget {
        bounded_protocol_group(raw_tail, history_budget.max(4))
    } else {
        raw_tail.to_vec()
    };
    let tail_tokens = tail.iter().map(Message::estimate_tokens).sum::<usize>();
    let mut remaining = history_budget.saturating_sub(tail_tokens);
    let mut bounded_prefix = Vec::new();

    for (index, message) in compacted[..tail_start].iter().enumerate() {
        if remaining < 4 {
            break;
        }
        // Keep the leading summary/envelope message readable, but cap it so
        // the newest complete protocol groups retain priority.
        let message_budget = if index == 0 { remaining.min(4_096) } else { remaining };
        let bounded = bounded_message_preview(message, message_budget);
        let used = bounded.estimate_tokens();
        if used > remaining {
            if index == 0 {
                let fallback = Message::system(truncate_to_token_limit(
                    message.content.as_text().as_ref(),
                    remaining.saturating_sub(4),
                ));
                let fallback_tokens = fallback.estimate_tokens();
                if fallback_tokens <= remaining {
                    bounded_prefix.push(fallback);
                }
            }
            break;
        }
        remaining -= used;
        bounded_prefix.push(bounded);
    }

    bounded_prefix.extend(tail);
    bounded_prefix
}

pub(crate) fn build_local_compacted_history(
    history: &[Message],
    summary: &str,
    retained_user_message_tokens: usize,
    retained_user_messages: usize,
    include_continuity_tail: bool,
) -> Vec<Message> {
    let (retention_history, continuity) = if include_continuity_tail {
        split_continuity_history(history)
    } else {
        (history, Vec::new())
    };
    let retained_users =
        collect_retained_user_messages(retention_history, retained_user_message_tokens, retained_user_messages);
    let mut new_history = Vec::with_capacity(retained_users.len().saturating_add(1));
    new_history.push(Message::system(format!("{SUMMARY_PREFIX}{}", summary.trim())));
    new_history.extend(retained_users);

    // Continuity anchor: retain the newest complete protocol groups verbatim
    // within the fixed tail budget. Duplicate message text is still valid
    // across turns, so preserve the sequence by index rather than deduplicating
    // on role/content.
    if include_continuity_tail {
        for message in continuity {
            new_history.push(message);
        }
    }
    new_history
}

/// Return the newest complete user-anchored protocol groups that fit the fixed
/// continuity budget. The returned messages are owned because an oversized
/// individual group may need a bounded preview.
fn continuity_tail(history: &[Message]) -> Vec<Message> {
    let Some((start, end, oversized)) = continuity_tail_selection(history) else {
        return Vec::new();
    };
    if oversized {
        bounded_protocol_group(&history[start..end], CONTINUITY_TAIL_TARGET_TOKENS)
    } else {
        history[start..end].to_vec()
    }
}

/// Find one contiguous suffix of complete protocol groups. An incomplete
/// trailing assistant tool-call group is truncated at the assistant message,
/// preserving its user anchor while excluding the invalid protocol suffix.
fn continuity_tail_selection(history: &[Message]) -> Option<(usize, usize, bool)> {
    if history.is_empty() {
        return None;
    }
    let group_starts: Vec<usize> = history
        .iter()
        .enumerate()
        .filter_map(|(index, message)| (message.role == MessageRole::User).then_some(index))
        .collect();
    let last_group_index = group_starts.len().saturating_sub(1);
    let last_group_start = *group_starts.last()?;
    let last_group_end = history.len();
    let last_group = &history[last_group_start..last_group_end];
    let last_group_prefix_end = complete_protocol_group_prefix(last_group);
    let tail_end = last_group_start.saturating_add(last_group_prefix_end);

    let mut selected_start = None;
    let mut estimated_tokens = 0usize;
    for group_index in (0..group_starts.len()).rev() {
        let start = group_starts[group_index];
        let natural_end = group_starts.get(group_index + 1).copied().unwrap_or(history.len());
        let end = if group_index == last_group_index {
            tail_end
        } else {
            natural_end
        };
        if start == end {
            continue;
        }
        let group = &history[start..end];
        if group_index != last_group_index && !protocol_group_is_complete(group) {
            break;
        }
        let group_tokens = group.iter().map(Message::estimate_tokens).sum::<usize>();
        if selected_start.is_none() && group_tokens > CONTINUITY_TAIL_TARGET_TOKENS {
            return Some((start, end, true));
        }
        if estimated_tokens.saturating_add(group_tokens) > CONTINUITY_TAIL_TARGET_TOKENS {
            break;
        }
        selected_start = Some(start);
        estimated_tokens += group_tokens;
    }

    selected_start.map(|start| (start, tail_end, false))
}

fn protocol_group_is_complete(group: &[Message]) -> bool {
    complete_protocol_group_prefix(group) == group.len()
}

/// Return the length of the valid protocol prefix in a user-anchored group.
/// When a tool call is still pending, the prefix ends before the assistant
/// message that introduced it. This handles parallel tool calls and prevents
/// a partially answered group from entering the continuity tail.
fn complete_protocol_group_prefix(group: &[Message]) -> usize {
    if group.first().is_none_or(|message| message.role != MessageRole::User) {
        return 0;
    }

    let mut pending_tool_call_ids: Vec<&str> = Vec::new();
    let mut pending_origin = None;

    for (index, message) in group.iter().enumerate() {
        if !pending_tool_call_ids.is_empty() {
            if message.role != MessageRole::Tool {
                return pending_origin.unwrap_or(index);
            }
            let Some(tool_call_id) = message.tool_call_id.as_deref() else {
                return pending_origin.unwrap_or(index);
            };
            let Some(pending_index) = pending_tool_call_ids.iter().position(|pending_id| *pending_id == tool_call_id)
            else {
                return pending_origin.unwrap_or(index);
            };
            pending_tool_call_ids.swap_remove(pending_index);
            if pending_tool_call_ids.is_empty() {
                pending_origin = None;
            }
            continue;
        }

        // Older persisted histories may contain a tool result without the
        // assistant call metadata that introduced it. Preserve that legacy
        // pair rather than discarding an otherwise complete protocol group.
        if message.role == MessageRole::Tool {
            continue;
        }

        if message.role == MessageRole::Assistant
            && let Some(tool_calls) = message.tool_calls.as_ref()
            && !tool_calls.is_empty()
        {
            for (call_index, call) in tool_calls.iter().enumerate() {
                if call.id.is_empty() || tool_calls[..call_index].iter().any(|prior| prior.id == call.id) {
                    return index;
                }
            }
            pending_origin = Some(index);
            pending_tool_call_ids.extend(tool_calls.iter().map(|call| call.id.as_str()));
        }
    }

    pending_origin.unwrap_or(group.len())
}

fn split_continuity_history(history: &[Message]) -> (&[Message], Vec<Message>) {
    let Some((tail_start, _, _)) = continuity_tail_selection(history) else {
        return (history, Vec::new());
    };
    (&history[..tail_start], continuity_tail(history))
}

fn build_summary_compacted_history(
    history: &[Message],
    summary: impl AsRef<str>,
    config: &CompactionConfig,
    include_continuity_tail: bool,
) -> Vec<Message> {
    let (retention_history, continuity) = split_continuity_history(history);
    let retained_users = collect_retained_user_messages(
        retention_history,
        config.retained_user_message_tokens,
        config.retained_user_messages,
    );
    let mut compacted = Vec::with_capacity(retained_users.len().saturating_add(1));
    compacted.push(Message::system(format!("{SUMMARY_PREFIX}{}", summary.as_ref().trim())));
    compacted.extend(retained_users);
    if include_continuity_tail {
        for message in continuity {
            compacted.push(message);
        }
    }
    compacted
}

fn bounded_protocol_group(group: &[Message], token_budget: usize) -> Vec<Message> {
    let per_message_budget = (token_budget / group.len().max(1)).max(4);
    group
        .iter()
        .map(|message| bounded_message_preview(message, per_message_budget))
        .collect()
}

fn bounded_message_preview(message: &Message, token_budget: usize) -> Message {
    if message.estimate_tokens() <= token_budget {
        return message.clone();
    }
    let mut preview = message.clone();
    let available = token_budget.saturating_sub(4);
    let text = truncate_to_token_limit(message.content.as_text().as_ref(), available);
    preview.content = MessageContent::Text(text);

    // Tool-call arguments and provider metadata are part of the message's
    // token footprint even when `content` is empty. Preserve call IDs and
    // function names so the protocol group remains correlatable, but replace
    // oversized argument payloads with valid minimal JSON and drop optional
    // reasoning/signature metadata.
    preview.reasoning = preview
        .reasoning
        .map(|reasoning| truncate_to_token_limit(&reasoning, available.min(1_024)));
    preview.reasoning_details = None;
    preview.metadata = None;
    preview.origin_tool = None;
    if let Some(tool_calls) = preview.tool_calls.as_mut() {
        for call in tool_calls {
            if let Some(function) = call.function.as_mut()
                && function.arguments.len() > available.saturating_mul(4)
            {
                function.arguments = "{}".to_string();
            }
            if let Some(text) = call.text.as_mut() {
                *text = truncate_to_token_limit(text, available.min(1_024));
            }
            call.thought_signature = None;
        }
    }

    if preview.estimate_tokens() > token_budget {
        preview.content = MessageContent::Text(String::new());
        preview.reasoning = None;
        preview.reasoning_details = None;
        if let Some(tool_calls) = preview.tool_calls.as_mut() {
            for call in tool_calls {
                if let Some(function) = call.function.as_mut() {
                    function.arguments = "{}".to_string();
                }
                call.text = None;
                call.thought_signature = None;
            }
        }
    }
    preview
}

/// Keep the provider's compacted prefix, but apply the same bounded protocol
/// tail rules used by local compaction. A provider may return a summary only;
/// that remains valid, while malformed trailing tool calls are discarded.
fn normalize_provider_compacted_history(compacted: Vec<Message>, fallback_history: &[Message]) -> Vec<Message> {
    let mut compacted = compacted;
    while compacted.last().is_some_and(|message| {
        message.role == MessageRole::Assistant && message.tool_calls.as_ref().is_some_and(|calls| !calls.is_empty())
    }) {
        compacted.pop();
    }
    let selection = continuity_tail_selection(&compacted);
    let tail = selection.map(|(start, end, oversized)| {
        if oversized {
            bounded_protocol_group(&compacted[start..end], CONTINUITY_TAIL_TARGET_TOKENS)
        } else {
            compacted[start..end].to_vec()
        }
    });
    let mut normalized = if let Some((tail_start, _, _)) = selection {
        // `tail_end` may be before the physical end when a provider returned
        // only part of a tool-result group. Discard that invalid suffix by
        // using the selection's actual start, rather than subtracting the
        // selected tail length from the physical vector length.
        compacted[..tail_start].to_vec()
    } else {
        compacted
    };
    if tail.as_ref().is_none_or(Vec::is_empty) {
        // Some native endpoints return only their summary. Keep the newest
        // source protocol groups in that case rather than silently losing the
        // continuity anchor. Preserve sequence identity: two steering intents
        // may intentionally have identical text but distinct metadata IDs.
        normalized.extend(continuity_tail(fallback_history));
    } else if let Some(tail) = tail {
        normalized.extend(tail);
    }
    normalized
}

fn collect_retained_user_messages(history: &[Message], token_budget: usize, max_messages: usize) -> Vec<Message> {
    if token_budget == 0 || max_messages == 0 {
        return Vec::new();
    }

    // Phase 1: select up to `max_messages` user messages, scored by importance.
    let total = history.len();
    let mut user_scored: Vec<(usize, f64, &Message)> = history
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == MessageRole::User && !m.content.trim().is_empty())
        .map(|(i, m)| {
            let score = score_message(m, i, total);
            (i, score, m)
        })
        .collect();
    user_scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let mut selected: Vec<(usize, Message)> = Vec::with_capacity(max_messages.min(history.len()));
    let mut remaining = token_budget;

    for (original_idx, _score, message) in &user_scored {
        if selected.len() >= max_messages {
            break;
        }
        let estimated = message.estimate_tokens();
        if estimated <= remaining {
            selected.push((*original_idx, (*message).clone()));
            remaining = remaining.saturating_sub(estimated);
            continue;
        }
        if let Some(truncated) = truncate_user_message(message, remaining) {
            selected.push((*original_idx, truncated));
        }
        break;
    }

    // Phase 2: if budget remains, add high-value non-user messages (tool
    // results, assistant tool calls) that fit within the remaining capacity.
    if selected.len() < max_messages && remaining > 0 {
        let mut non_user_scored: Vec<(usize, f64, &Message)> = history
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role != MessageRole::User && m.role != MessageRole::System && is_retainable_message(m))
            .map(|(i, m)| {
                let score = score_message(m, i, total);
                (i, score, m)
            })
            .collect();
        non_user_scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        for (original_idx, _score, message) in &non_user_scored {
            if selected.len() >= max_messages {
                break;
            }
            let estimated = message.estimate_tokens();
            if estimated <= remaining {
                selected.push((*original_idx, (*message).clone()));
                remaining = remaining.saturating_sub(estimated);
            }
        }
    }

    // Re-sort by original conversation order, then enforce tool-call/turn
    // coherence so the compacted history is valid to send back to a provider.
    selected.sort_by_key(|(idx, _)| *idx);
    coherence_tool_call_pairs(history, &selected)
        .into_iter()
        .map(|(_, msg)| msg)
        .collect()
}

/// Keep retained tool-call turns internally consistent.
///
/// A `Tool` message references a tool call the model must have seen, and an
/// `Assistant` message that still carries `tool_calls` must be followed by the
/// results those calls produced. Sending either without its counterpart is
/// invalid: providers reject unmatched tool calls, and orphaned tool results
/// reference a call the model never observed. This pass:
///
/// - **Force-keeps** the `Tool` messages immediately following any retained
///   `Assistant` that carries `tool_calls`, so the model observes each call's
///   return value. A complete turn ends with its results, which survive even
///   if they push past the soft `max_messages` cap.
/// - **Drops** a retained `Tool` message whose calling `Assistant` (the message
///   directly before it in `history`) was *not* retained — an orphaned result
///   the model cannot reconcile.
///
/// Tool results that follow a plain `Assistant` (no `tool_calls`) are ordinary
/// turn output and are kept exactly as selected.
fn coherence_tool_call_pairs(history: &[Message], selected: &[(usize, Message)]) -> Vec<(usize, Message)> {
    let mut keep: std::collections::HashSet<usize> = selected.iter().map(|(i, _)| *i).collect();

    for (idx, msg) in selected {
        if msg.role == MessageRole::Assistant && msg.tool_calls.as_ref().is_some_and(|calls| !calls.is_empty()) {
            let mut j = *idx + 1;
            while let Some(next) = history.get(j) {
                if next.role == MessageRole::Tool {
                    keep.insert(j);
                    j += 1;
                } else {
                    break;
                }
            }
        }
    }

    // Emit every index in `keep` in original history order, dropping orphaned
    // Tool results whose calling Assistant turn was not retained.
    let mut indices: Vec<usize> = keep.iter().copied().collect();
    indices.sort_unstable();

    indices
        .into_iter()
        .filter(|idx| {
            let msg = &history[*idx];
            if msg.role != MessageRole::Tool {
                return true;
            }
            // Walk backward through this contiguous result run to decide
            // coherence against the calling assistant turn.
            let mut cursor = *idx;
            loop {
                match history.get(cursor) {
                    Some(m) if m.role == MessageRole::Tool => {
                        cursor = match cursor.checked_sub(1) {
                            Some(c) => c,
                            None => break,
                        };
                    }
                    Some(m)
                        if m.role == MessageRole::Assistant && m.tool_calls.as_ref().is_some_and(|c| !c.is_empty()) =>
                    {
                        // Reached the calling assistant: coherent only if it
                        // was retained.
                        return keep.contains(&cursor);
                    }
                    _ => {
                        // Plain assistant or boundary: ordinary output.
                        return true;
                    }
                }
            }
            true
        })
        .map(|idx| (idx, history[idx].clone()))
        .collect()
}

/// Score a message for importance-weighted retention during compaction.
///
/// Uses a weighted combination of content importance and recency:
/// - Messages containing errors, corrections, or tool results score higher
/// - Recent messages get a recency bonus
/// - Assistant messages with tool calls are moderately important
fn score_message(message: &Message, index: usize, total: usize) -> f64 {
    let content = message.content.as_text();
    let content_lower = content.to_lowercase();

    // Importance weight based on content signals.
    let importance = match message.role {
        MessageRole::User => {
            if contains_error_signal(&content_lower) {
                3.0
            } else if contains_correction_signal(&content_lower) {
                2.5
            } else {
                1.0
            }
        }
        MessageRole::Tool => {
            // Tool results contain factual data the model may need.
            2.0
        }
        MessageRole::Assistant => {
            if message.tool_calls.is_some() {
                // Assistant messages with tool calls show action taken.
                0.5
            } else {
                0.1
            }
        }
        MessageRole::System => 0.0,
    };

    // Recency bonus: linear from 0.0 (oldest) to 1.0 (newest).
    let recency = if total > 0 { index as f64 / total as f64 } else { 0.0 };

    importance + recency
}

/// Check if content contains error or failure signals.
fn contains_error_signal(content: &str) -> bool {
    content.contains("error")
        || content.contains("failed")
        || content.contains("failure")
        || content.contains("panic")
        || content.contains("bug")
        || content.contains("broken")
        || content.contains("regression")
}

/// Check if content contains user correction signals.
fn contains_correction_signal(content: &str) -> bool {
    content.contains("no,")
        || content.contains("wrong")
        || content.contains("actually")
        || content.contains("fix")
        || content.contains("instead")
        || content.contains("should be")
        || content.contains("don't")
}

/// Whether a message is worth retaining during compaction.
fn is_retainable_message(message: &Message) -> bool {
    match message.role {
        MessageRole::User => !message.content.trim().is_empty(),
        MessageRole::Tool => !message.content.trim().is_empty(),
        MessageRole::Assistant => {
            // Retain assistant messages that contain tool calls (action history).
            message.tool_calls.is_some()
        }
        MessageRole::System => false,
    }
}

fn truncate_user_message(message: &Message, token_budget: usize) -> Option<Message> {
    if token_budget <= 4 {
        return None;
    }

    let available_content_tokens = token_budget.saturating_sub(4);
    let truncated = truncate_to_token_limit(message.content.as_text().as_ref(), available_content_tokens);
    let trimmed = truncated.trim();
    if trimmed.is_empty() {
        return None;
    }

    Some(Message::user(trimmed.to_string()))
}

#[cfg(test)]
mod tests {
    use super::{
        COMPACTION_CONTEXT_FIXED_OVERHEAD_TOKENS, CompactionConfig, ManualCompactionOptions, compact_history,
        compact_history_manual, compact_history_manual_with_budget, continuity_tail, manual_compaction_strategy,
    };
    use crate::config::types::{ReasoningEffortLevel, VerbosityLevel};
    use crate::exec::events::CompactionMode;
    use crate::llm::provider::{
        LLMError, LLMProvider, LLMRequest, LLMResponse, Message, MessageRole, ResponsesCompactionOptions,
    };
    use async_trait::async_trait;
    use std::sync::Mutex;
    use vtcode_commons::llm::{FinishReason, ToolCall};

    struct StubProvider;

    struct NativeCompactionProvider;

    /// Provider that opts into the standalone manual-compaction path
    /// (`supports_manual_openai_compaction -> true`), e.g. OpenAI `/responses/compact`.
    struct ManualStandaloneProvider {
        last_options: Mutex<Option<ResponsesCompactionOptions>>,
    }

    /// Inline-compaction-capable provider (`supports_responses_compaction -> true`,
    /// `supports_manual_openai_compaction -> false`), e.g. Anthropic `compact_20260112`.
    /// Returns a `Pause` finish with a compaction block so the inline path succeeds.
    struct InlinePauseProvider {
        last_request: Mutex<Option<LLMRequest>>,
    }

    /// Capturing provider with no native support; used to assert the Local summary
    /// request carries the manual options.
    struct CapturingProvider {
        last_request: Mutex<Option<LLMRequest>>,
    }

    /// Local summarizer that exposes only the lower reasoning levels. This
    /// exercises the compaction boundary's strict block and explicit
    /// downgrade behavior before any hierarchical summary request is sent.
    struct LimitedReasoningProvider {
        last_request: Mutex<Option<LLMRequest>>,
    }

    /// Inline-dispatched provider whose inline `generate` rejects the Anthropic
    /// `compact_20260112` edit. Models providers that report
    /// `supports_responses_compaction` but are not Anthropic-style inline
    /// compactors; the dispatch must fall back to Local rather than aborting.
    struct InlineRejectingProvider;

    /// Models an OpenAI-compatible custom endpoint (non-`api.openai.com` host or
    /// `provider_key_override`): it exposes the Responses API
    /// (`supports_responses_compaction == true`) but neither the standalone
    /// `/responses/compact` endpoint nor Anthropic inline compaction. The dispatch
    /// must pick `Local` rather than misrouting it to `NativeInline` (which would
    /// send an Anthropic `compact_20260112` edit only to be rejected).
    struct CompatibleEndpointProvider;

    #[async_trait]
    impl LLMProvider for StubProvider {
        fn name(&self) -> &str {
            "stub"
        }

        async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
            Ok(LLMResponse::new("stub-model", "summary"))
        }

        fn supported_models(&self) -> Vec<String> {
            vec!["stub-model".to_string()]
        }

        fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
            Ok(())
        }

        fn effective_context_size(&self, _model: &str) -> usize {
            32_768
        }
    }

    #[async_trait]
    impl LLMProvider for NativeCompactionProvider {
        fn name(&self) -> &str {
            "native"
        }

        async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
            Ok(LLMResponse::new("stub-model", "summary"))
        }

        fn supported_models(&self) -> Vec<String> {
            vec!["stub-model".to_string()]
        }

        fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
            Ok(())
        }

        fn supports_responses_compaction(&self, _model: &str) -> bool {
            true
        }

        async fn compact_history(&self, _model: &str, _history: &[Message]) -> Result<Vec<Message>, LLMError> {
            Ok(vec![Message::system("provider compacted".to_string())])
        }
    }

    #[async_trait]
    impl LLMProvider for ManualStandaloneProvider {
        fn name(&self) -> &str {
            "manual-standalone"
        }

        async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
            Ok(LLMResponse::new("stub-model", "summary"))
        }

        fn supported_models(&self) -> Vec<String> {
            vec!["stub-model".to_string()]
        }

        fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
            Ok(())
        }

        fn supports_manual_openai_compaction(&self, _model: &str) -> bool {
            true
        }

        fn supports_reasoning_effort(&self, _model: &str) -> bool {
            true
        }

        fn supported_reasoning_efforts(&self, _model: &str) -> &'static [&'static str] {
            &["minimal", "low", "medium", "high", "xhigh", "max"]
        }

        async fn compact_history_with_options(
            &self,
            _model: &str,
            _history: &[Message],
            options: &ResponsesCompactionOptions,
        ) -> Result<Vec<Message>, LLMError> {
            *self.last_options.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(options.clone());
            Ok(vec![Message::system("provider standalone compacted".to_string())])
        }
    }

    #[async_trait]
    impl LLMProvider for InlinePauseProvider {
        fn name(&self) -> &str {
            "inline-pause"
        }

        async fn generate(&self, request: LLMRequest) -> Result<LLMResponse, LLMError> {
            *self.last_request.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(request);
            let mut response = LLMResponse::new("stub-model", "compacted by provider");
            response.finish_reason = FinishReason::Pause;
            response.compaction = Some("provider compaction summary".to_string());
            Ok(response)
        }

        fn supported_models(&self) -> Vec<String> {
            vec!["stub-model".to_string()]
        }

        fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
            Ok(())
        }

        fn supports_responses_compaction(&self, _model: &str) -> bool {
            true
        }

        fn supports_native_inline_compaction(&self, _model: &str) -> bool {
            true
        }
    }

    #[async_trait]
    impl LLMProvider for CapturingProvider {
        fn name(&self) -> &str {
            "capturing"
        }

        async fn generate(&self, request: LLMRequest) -> Result<LLMResponse, LLMError> {
            *self.last_request.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(request);
            Ok(LLMResponse::new("stub-model", "summary"))
        }

        fn supported_models(&self) -> Vec<String> {
            vec!["stub-model".to_string()]
        }

        fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
            Ok(())
        }

        fn supports_reasoning_effort(&self, _model: &str) -> bool {
            true
        }

        fn supported_reasoning_efforts(&self, _model: &str) -> &'static [&'static str] {
            &["minimal", "low", "medium", "high", "xhigh", "max"]
        }
    }

    #[async_trait]
    impl LLMProvider for LimitedReasoningProvider {
        fn name(&self) -> &str {
            "limited-reasoning"
        }

        async fn generate(&self, request: LLMRequest) -> Result<LLMResponse, LLMError> {
            *self.last_request.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(request);
            Ok(LLMResponse::new("stub-model", "summary"))
        }

        fn supported_models(&self) -> Vec<String> {
            vec!["stub-model".to_string()]
        }

        fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
            Ok(())
        }

        fn supports_reasoning_effort(&self, _model: &str) -> bool {
            true
        }

        fn supported_reasoning_efforts(&self, _model: &str) -> &'static [&'static str] {
            &["low", "medium", "high"]
        }
    }

    #[async_trait]
    impl LLMProvider for InlineRejectingProvider {
        fn name(&self) -> &str {
            "inline-rejecting"
        }

        async fn generate(&self, request: LLMRequest) -> Result<LLMResponse, LLMError> {
            // Reject only the inline compaction request (carries the Anthropic
            // `compact_20260112` edit); the Local summary request must succeed.
            if request.context_management.is_some() {
                return Err(LLMError::Provider {
                    message: "provider rejected inline compact edit".to_string(),
                    metadata: None,
                });
            }
            Ok(LLMResponse::new("stub-model", "summary"))
        }

        fn supported_models(&self) -> Vec<String> {
            vec!["stub-model".to_string()]
        }

        fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
            Ok(())
        }

        fn supports_responses_compaction(&self, _model: &str) -> bool {
            true
        }

        fn supports_native_inline_compaction(&self, _model: &str) -> bool {
            true
        }
    }

    #[async_trait]
    impl LLMProvider for CompatibleEndpointProvider {
        fn name(&self) -> &str {
            "compatible-endpoint"
        }

        async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
            Ok(LLMResponse::new("stub-model", "summary"))
        }

        fn supported_models(&self) -> Vec<String> {
            vec!["stub-model".to_string()]
        }

        fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
            Ok(())
        }

        // Reports Responses API support but neither standalone nor inline
        // compaction (defaults: supports_manual_openai_compaction and
        // supports_native_inline_compaction are both false).
        fn supports_responses_compaction(&self, _model: &str) -> bool {
            true
        }
    }

    fn sample_history() -> Vec<Message> {
        vec![
            Message::assistant("setup".to_string()),
            Message::user("first request".to_string()),
            Message::assistant("working".to_string()),
            Message::user("second request".to_string()),
        ]
    }

    /// Build an assistant message that carries a (single) pending tool call.
    fn assistant_with_calls(content: &str, call_id: &str) -> Message {
        let mut message = Message::assistant(content.to_string());
        message.tool_calls = Some(vec![ToolCall {
            id: call_id.to_string(),
            call_type: "function".to_string(),
            function: None,
            text: None,
            thought_signature: None,
        }]);
        message
    }

    #[test]
    fn collect_retained_keeps_tool_result_with_its_assistant() {
        // When the assistant tool-call turn is retained, its tool result must
        // survive so the turn stays coherent (the model sees each call's return).
        let history = vec![
            Message::user("u1".to_string()),
            assistant_with_calls("calling tool", "c1"),
            Message::tool_response("c1".to_string(), "r1".to_string()),
            Message::user("u2".to_string()),
        ];
        let retained = super::collect_retained_user_messages(&history, 20_000, 4);
        assert!(retained.iter().any(|m| m.content.as_text().contains("u1")));
        assert!(retained.iter().any(|m| m.content.as_text().contains("u2")));
        assert!(
            retained.iter().any(|m| m.content.as_text().contains("r1")),
            "tool result paired with its retained assistant must survive"
        );
    }

    #[test]
    fn collect_retained_drops_orphaned_tool_result() {
        // If the assistant tool-call turn is dropped (over the retention cap),
        // its tool result is orphaned — the model never saw the call — and must
        // not survive compaction, because an orphaned result is invalid to send
        // to a provider.
        let history = vec![
            Message::user("u1".to_string()),
            assistant_with_calls("calling tool", "c1"),
            Message::tool_response("c1".to_string(), "r1".to_string()),
            Message::user("u2".to_string()),
        ];
        let retained = super::collect_retained_user_messages(&history, 20_000, 3);
        assert!(retained.iter().any(|m| m.content.as_text().contains("u1")));
        assert!(retained.iter().any(|m| m.content.as_text().contains("u2")));
        assert!(!retained.iter().any(|m| m.content.as_text().contains("r1")), "orphaned tool result must be dropped");
    }

    #[tokio::test]
    async fn manual_compaction_strategy_picks_local_for_plain_provider() {
        assert_eq!(manual_compaction_strategy(&StubProvider, "stub-model"), super::CompactionStrategy::Local);
    }

    #[tokio::test]
    async fn manual_compaction_strategy_picks_native_standalone_for_manual_provider() {
        let provider = ManualStandaloneProvider { last_options: Mutex::new(None) };
        assert_eq!(manual_compaction_strategy(&provider, "stub-model"), super::CompactionStrategy::NativeStandalone);
    }

    #[tokio::test]
    async fn manual_compaction_strategy_picks_native_inline_for_responses_capable_provider() {
        let provider = InlinePauseProvider { last_request: Mutex::new(None) };
        assert_eq!(manual_compaction_strategy(&provider, "stub-model"), super::CompactionStrategy::NativeInline);
    }

    #[tokio::test]
    async fn manual_compaction_strategy_picks_local_for_compatible_endpoint() {
        // OpenAI-compatible custom endpoints report `supports_responses_compaction`
        // but cannot serve standalone `/responses/compact` or Anthropic inline
        // compaction; they must route to Local, not NativeInline.
        assert_eq!(
            manual_compaction_strategy(&CompatibleEndpointProvider, "stub-model"),
            super::CompactionStrategy::Local
        );
    }

    #[tokio::test]
    async fn compact_history_manual_uses_local_summary_for_plain_provider() {
        let history = sample_history();
        let config = CompactionConfig {
            always_summarize: true,
            ..CompactionConfig::default()
        };

        let (compacted, mode) =
            compact_history_manual(&StubProvider, "stub-model", &history, &config, &ManualCompactionOptions::default())
                .await
                .expect("manual compaction");

        assert_eq!(mode, CompactionMode::Local);
        assert_eq!(compacted.len(), 4);
        assert_eq!(compacted[0].content.as_text(), "Previous conversation summary:\nsummary");
        assert_eq!(compacted[1].content.as_text(), "first request");
        assert_eq!(compacted[2].content.as_text(), "working");
        assert_eq!(compacted[3].content.as_text(), "second request");
    }

    #[tokio::test]
    async fn compact_history_manual_uses_native_standalone_for_manual_provider() {
        let history = sample_history();
        let config = CompactionConfig::default();
        let provider = ManualStandaloneProvider { last_options: Mutex::new(None) };

        let (compacted, mode) =
            compact_history_manual(&provider, "stub-model", &history, &config, &ManualCompactionOptions::default())
                .await
                .expect("manual compaction");

        assert_eq!(mode, CompactionMode::Provider);
        assert_eq!(compacted.len(), 4);
        assert_eq!(compacted[0].content.as_text(), "provider standalone compacted");
        assert_eq!(compacted[1].content.as_text(), "first request");
        assert_eq!(compacted[2].content.as_text(), "working");
        assert_eq!(compacted[3].content.as_text(), "second request");
    }

    #[tokio::test]
    async fn native_compaction_respects_explicit_session_context_budget() {
        let mut history = Vec::new();
        for index in 0..24 {
            history.push(Message::user(format!("request-{index} {}", "context ".repeat(1_200))));
            history.push(Message::assistant(format!("completed request {index}")));
        }
        let provider = ManualStandaloneProvider { last_options: Mutex::new(None) };
        let (compacted, mode) = compact_history_manual_with_budget(
            &provider,
            "stub-model",
            &history,
            &CompactionConfig::default(),
            &ManualCompactionOptions::default(),
            Some(8_192),
        )
        .await
        .expect("native compaction");

        assert_eq!(mode, CompactionMode::Provider);
        let estimated_tokens = compacted.iter().map(Message::estimate_tokens).sum::<usize>();
        assert!(estimated_tokens <= 8_192 - COMPACTION_CONTEXT_FIXED_OVERHEAD_TOKENS);
    }

    #[tokio::test]
    async fn compact_history_manual_passes_options_to_native_standalone() {
        let history = sample_history();
        let config = CompactionConfig::default();
        let provider = ManualStandaloneProvider { last_options: Mutex::new(None) };
        let options = ManualCompactionOptions {
            instructions: Some("keep only decisions".to_string()),
            max_output_tokens: Some(256),
            reasoning_effort: Some(ReasoningEffortLevel::Minimal),
            verbosity: Some(VerbosityLevel::High),
            ..ManualCompactionOptions::default()
        };

        let (_compacted, mode) = compact_history_manual(&provider, "stub-model", &history, &config, &options)
            .await
            .expect("manual compaction");

        assert_eq!(mode, CompactionMode::Provider);
        let captured = provider.last_options.lock().unwrap().clone().expect("captured options");
        assert_eq!(captured.instructions.as_deref(), Some("keep only decisions"));
        assert_eq!(captured.max_output_tokens, Some(256));
        assert_eq!(captured.reasoning_effort, Some(ReasoningEffortLevel::Minimal));
        assert_eq!(captured.verbosity, Some(VerbosityLevel::High));
    }

    #[tokio::test]
    async fn compact_history_manual_uses_native_inline_when_pause_and_compaction_present() {
        let history = sample_history();
        let config = CompactionConfig::default();
        let provider = InlinePauseProvider { last_request: Mutex::new(None) };

        let (compacted, mode) =
            compact_history_manual(&provider, "stub-model", &history, &config, &ManualCompactionOptions::default())
                .await
                .expect("manual compaction");

        assert_eq!(mode, CompactionMode::Provider);
        assert_eq!(compacted.len(), 4);
        assert_eq!(compacted[0].content.as_text(), "Previous conversation summary:\nprovider compaction summary");
        assert_eq!(compacted[1].content.as_text(), "first request");
        assert_eq!(compacted[2].content.as_text(), "working");
        assert_eq!(compacted[3].content.as_text(), "second request");

        // The inline request must carry the `compact_20260112` edit with a forced
        // pause so the provider actually performs compaction on demand.
        let captured = provider.last_request.lock().unwrap().clone().expect("captured inline request");
        let context_management = captured
            .context_management
            .as_ref()
            .expect("context_management set on inline compaction request");
        let edit = &context_management["edits"][0];
        assert_eq!(edit["type"].as_str(), Some("compact_20260112"));
        assert_eq!(edit["pause_after_compaction"].as_bool(), Some(true));
        assert_eq!(edit["trigger"]["value"].as_u64(), Some(50_000));
    }

    #[tokio::test]
    async fn compact_history_manual_inline_request_carries_instructions_when_provided() {
        let history = sample_history();
        let config = CompactionConfig::default();
        let provider = InlinePauseProvider { last_request: Mutex::new(None) };
        let options = ManualCompactionOptions {
            instructions: Some("  keep only decisions  ".to_string()),
            ..ManualCompactionOptions::default()
        };

        let (_compacted, _mode) = compact_history_manual(&provider, "stub-model", &history, &config, &options)
            .await
            .expect("manual compaction");

        let captured = provider.last_request.lock().unwrap().clone().expect("captured inline request");
        let edit = &captured.context_management.as_ref().expect("context_management")["edits"][0];
        assert_eq!(edit["instructions"].as_str(), Some("keep only decisions"));
    }

    #[tokio::test]
    async fn compact_history_manual_falls_back_to_local_when_inline_compaction_not_fired() {
        let history = sample_history();
        let config = CompactionConfig::default();

        // NativeCompactionProvider is inline-capable but its `generate` returns a
        // normal `Stop` with no compaction block, so the inline attempt cannot
        // fire and the dispatch must transparently fall back to Local.
        let (compacted, mode) = compact_history_manual(
            &NativeCompactionProvider,
            "stub-model",
            &history,
            &config,
            &ManualCompactionOptions::default(),
        )
        .await
        .expect("manual compaction");

        assert_eq!(mode, CompactionMode::Local);
        assert_eq!(compacted.len(), 4);
        assert_eq!(compacted[0].content.as_text(), "Previous conversation summary:\nsummary");
    }

    #[tokio::test]
    async fn compact_history_manual_falls_back_to_local_when_inline_request_errors() {
        let history = sample_history();
        let config = CompactionConfig::default();

        // A provider dispatched to NativeInline that rejects the Anthropic
        // `compact_20260112` edit must not abort the whole command; the dispatch
        // falls back to Local summarization (the manual `/compact` contract:
        // always succeeds).
        let (compacted, mode) = compact_history_manual(
            &InlineRejectingProvider,
            "stub-model",
            &history,
            &config,
            &ManualCompactionOptions::default(),
        )
        .await
        .expect("manual compaction should fall back to local");

        assert_eq!(mode, CompactionMode::Local);
        assert_eq!(compacted.len(), 4);
        assert_eq!(compacted[0].content.as_text(), "Previous conversation summary:\nsummary");
    }

    #[tokio::test]
    async fn compact_history_manual_applies_options_to_local_summary_request() {
        let history = sample_history();
        let config = CompactionConfig {
            always_summarize: true,
            ..CompactionConfig::default()
        };
        let provider = CapturingProvider { last_request: Mutex::new(None) };
        let options = ManualCompactionOptions {
            instructions: Some("KEEP DECISIONS ONLY".to_string()),
            max_output_tokens: Some(128),
            reasoning_effort: Some(ReasoningEffortLevel::Minimal),
            verbosity: Some(VerbosityLevel::High),
            ..ManualCompactionOptions::default()
        };

        let (compacted, mode) = compact_history_manual(&provider, "stub-model", &history, &config, &options)
            .await
            .expect("manual compaction");

        assert_eq!(mode, CompactionMode::Local);
        let captured = provider.last_request.lock().unwrap().clone().expect("captured summary request");
        assert_eq!(captured.max_tokens, Some(128));
        assert_eq!(captured.reasoning_effort, Some(ReasoningEffortLevel::Minimal));
        assert_eq!(captured.verbosity, Some(VerbosityLevel::High));
        // The custom instructions override the default summary prompt.
        let prompt = captured.messages[0].content.as_text();
        assert!(prompt.contains("KEEP DECISIONS ONLY"));
        assert!(!prompt.contains("acceptance criteria"));
        assert_eq!(compacted[0].content.as_text(), "Previous conversation summary:\nsummary");
    }

    #[tokio::test]
    async fn compact_history_manual_blocks_unsupported_reasoning_before_summary() {
        let provider = LimitedReasoningProvider { last_request: Mutex::new(None) };
        let options = ManualCompactionOptions {
            reasoning_effort: Some(ReasoningEffortLevel::Max),
            ..ManualCompactionOptions::default()
        };
        let error = compact_history_manual(
            &provider,
            "stub-model",
            &sample_history(),
            &CompactionConfig {
                always_summarize: true,
                ..CompactionConfig::default()
            },
            &options,
        )
        .await
        .expect_err("unsupported compaction effort must block");

        assert!(
            error
                .downcast_ref::<crate::llm::reasoning_effort::ReasoningEffortUnsupported>()
                .is_some(),
            "strict compaction failure should preserve the capability diagnostic: {error:#}"
        );
        assert!(provider.last_request.lock().unwrap().is_none(), "provider must not receive a blocked request");
    }

    #[tokio::test]
    async fn compact_history_manual_downgrades_only_when_explicitly_enabled() {
        let provider = LimitedReasoningProvider { last_request: Mutex::new(None) };
        let options = ManualCompactionOptions {
            reasoning_effort: Some(ReasoningEffortLevel::Max),
            allow_reasoning_effort_downgrade: true,
            ..ManualCompactionOptions::default()
        };
        let config = CompactionConfig {
            always_summarize: true,
            hierarchical: true,
            ..CompactionConfig::default()
        };
        compact_history_manual(&provider, "stub-model", &sample_history(), &config, &options)
            .await
            .expect("explicit downgrade should permit compaction");

        let captured = provider.last_request.lock().unwrap().clone().expect("summary request captured");
        assert_eq!(captured.reasoning_effort, Some(ReasoningEffortLevel::High));
    }

    #[tokio::test]
    async fn compact_history_manual_returns_empty_for_empty_history() {
        let (compacted, mode) = compact_history_manual(
            &StubProvider,
            "stub-model",
            &[],
            &CompactionConfig::default(),
            &ManualCompactionOptions::default(),
        )
        .await
        .expect("manual compaction");

        assert!(compacted.is_empty());
        assert_eq!(mode, CompactionMode::Local);
    }

    #[tokio::test]
    async fn compact_history_rebuilds_history_around_summary_and_important_messages() {
        let history = vec![
            Message::assistant("setup".to_string()),
            Message::user("first request".to_string()),
            Message::assistant("working".to_string()),
            Message::tool_response("call-1".to_string(), "done".to_string()),
            Message::user("second request".to_string()),
            Message::assistant("final reply".to_string()),
        ];
        let config = CompactionConfig {
            always_summarize: true,
            ..CompactionConfig::default()
        };

        let compacted = compact_history(&StubProvider, "stub-model", &history, &config)
            .await
            .expect("compacted history");

        // Summary plus the complete newest protocol groups. The assistant/tool
        // messages remain paired with their user anchors in the continuity
        // tail.
        assert_eq!(compacted.len(), 6);
        assert_eq!(compacted[0].content.as_text(), "Previous conversation summary:\nsummary");
        assert_eq!(compacted[1].content.as_text(), "first request");
        assert_eq!(compacted[2].content.as_text(), "working");
        assert_eq!(compacted[3].content.as_text(), "done");
        assert_eq!(compacted[4].content.as_text(), "second request");
        assert_eq!(compacted[5].content.as_text(), "final reply");
    }

    #[tokio::test]
    async fn compact_history_preserves_continuity_tail_over_retention_budget() {
        let history = vec![
            Message::user("alpha beta gamma delta epsilon zeta".to_string()),
            Message::assistant("ack".to_string()),
            Message::user("newest request".to_string()),
        ];
        let config = CompactionConfig {
            always_summarize: true,
            retained_user_message_tokens: 8,
            ..CompactionConfig::default()
        };

        let compacted = compact_history(&StubProvider, "stub-model", &history, &config)
            .await
            .expect("compacted history");

        assert_eq!(compacted.len(), 4);
        assert_eq!(compacted[1].content.as_text(), "alpha beta gamma delta epsilon zeta");
        assert_eq!(compacted[2].content.as_text(), "ack");
        assert_eq!(compacted[3].content.as_text(), "newest request");
    }

    #[tokio::test]
    async fn compacted_history_respects_model_context_budget() {
        let mut history = Vec::new();
        for index in 0..24 {
            history.push(Message::user(format!("request-{index} {}", "context ".repeat(1_200))));
            history.push(Message::assistant(format!("completed request {index}")));
        }
        let config = CompactionConfig {
            always_summarize: true,
            ..CompactionConfig::default()
        };

        let compacted = compact_history(&StubProvider, "stub-model", &history, &config)
            .await
            .expect("compacted history");
        let estimated_tokens = compacted.iter().map(Message::estimate_tokens).sum::<usize>();

        assert!(estimated_tokens <= 32_768 - 512);
    }

    #[tokio::test]
    async fn compact_history_caps_retained_user_message_count() {
        let history = vec![
            Message::user("first request".to_string()),
            Message::assistant("ack".to_string()),
            Message::user("second request".to_string()),
            Message::assistant("ack".to_string()),
            Message::user("third request".to_string()),
            Message::assistant("ack".to_string()),
            Message::user("fourth request".to_string()),
            Message::assistant("ack".to_string()),
            Message::user("fifth request".to_string()),
        ];
        let config = CompactionConfig {
            always_summarize: true,
            retained_user_messages: 4,
            ..CompactionConfig::default()
        };

        let compacted = compact_history(&StubProvider, "stub-model", &history, &config)
            .await
            .expect("compacted history");

        let continuity_tail = compacted
            .iter()
            .skip(1)
            .map(|message| message.content.as_text().to_string())
            .collect::<Vec<_>>();
        assert_eq!(continuity_tail.len(), history.len());
        assert_eq!(continuity_tail[0], "first request");
        assert_eq!(continuity_tail[8], "fifth request");
    }

    #[tokio::test]
    async fn compact_history_forces_local_summary_when_always_summarize_is_enabled() {
        let history = vec![
            Message::user("first request".to_string()),
            Message::assistant("working".to_string()),
            Message::user("second request".to_string()),
        ];
        let config = CompactionConfig {
            always_summarize: true,
            ..CompactionConfig::default()
        };

        let compacted = compact_history(&NativeCompactionProvider, "stub-model", &history, &config)
            .await
            .expect("compacted history");

        assert_eq!(compacted.len(), 4);
        assert_eq!(compacted[0].content.as_text(), "Previous conversation summary:\nsummary");
        assert_eq!(compacted[1].content.as_text(), "first request");
        assert_eq!(compacted[2].content.as_text(), "working");
        assert_eq!(compacted[3].content.as_text(), "second request");
    }

    #[test]
    fn default_summary_prompt_preserves_required_compaction_context() {
        let prompt = CompactionConfig::default().summary_prompt;

        assert!(prompt.contains("acceptance criteria"));
        assert!(prompt.contains("file paths that were read or modified"));
        assert!(prompt.contains("test results and error messages"));
        assert!(prompt.contains("decisions with their reasoning"));
    }

    #[test]
    fn continuity_tail_keeps_complete_turn_but_drops_unmatched_tool_call() {
        // A completed turn: user -> assistant(tool call) -> tool result. The
        // tail must keep the whole turn intact (the tool result makes the
        // trailing assistant tool call valid to send).
        let complete = vec![
            Message::user("do the thing".into()),
            {
                let mut m = Message::assistant("calling".into());
                m.tool_calls = Some(vec![ToolCall::function("c1".into(), "run".into(), "{}".into())]);
                m
            },
            Message::tool_response("c1".into(), "ran".into()),
        ];
        assert_eq!(continuity_tail(&complete).len(), 3);

        // An interrupted turn: user -> assistant(tool call) with no tool
        // result. Sending the trailing assistant message to a provider is
        // invalid, so the tail must drop it and keep only the user message.
        let interrupted = vec![Message::user("do the thing".into()), {
            let mut m = Message::assistant("calling".into());
            m.tool_calls = Some(vec![ToolCall::function("c1".into(), "run".into(), "{}".into())]);
            m
        }];
        let tail = continuity_tail(&interrupted);
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].role, MessageRole::User);

        let parallel_complete = vec![
            Message::user("run both".into()),
            Message::assistant_with_tools(
                "calling".into(),
                vec![
                    ToolCall::function("c1".into(), "run".into(), "{}".into()),
                    ToolCall::function("c2".into(), "run".into(), "{}".into()),
                ],
            ),
            Message::tool_response("c1".into(), "first result".into()),
            Message::tool_response("c2".into(), "second result".into()),
        ];
        assert_eq!(continuity_tail(&parallel_complete).len(), 4);

        let parallel_interrupted = parallel_complete[..3].to_vec();
        let tail = continuity_tail(&parallel_interrupted);
        assert_eq!(tail.len(), 1, "a missing parallel result drops the whole assistant call");
        assert_eq!(tail[0].content.as_text(), "run both");
    }

    #[test]
    fn continuity_tail_keeps_newest_complete_groups_within_budget() {
        let mut history = Vec::new();
        for group_index in 0..12 {
            history.push(Message::user(format!("group-{group_index} {}", "alpha beta gamma delta ".repeat(2_000))));
            history.push(Message::assistant(format!("completed group {group_index}")));
        }

        let tail = continuity_tail(&history);
        let estimated_tokens = tail.iter().map(Message::estimate_tokens).sum::<usize>();

        assert!(estimated_tokens <= super::CONTINUITY_TAIL_TARGET_TOKENS);
        assert_eq!(tail.first().map(|message| message.role), Some(MessageRole::User));
        assert!(tail.iter().any(|message| message.content.as_text().contains("group-11")));
        assert!(!tail.iter().any(|message| message.content.as_text().contains("group-0 ")));
        assert_eq!(tail.len() % 2, 0, "protocol groups must remain atomic");
    }

    #[test]
    fn continuity_tail_bounds_an_oversized_newest_group() {
        let history = vec![
            Message::user("u".repeat(100_000)),
            Message::assistant("a".repeat(100_000)),
        ];

        let tail = continuity_tail(&history);

        assert_eq!(tail.len(), 2);
        assert!(tail.iter().map(Message::estimate_tokens).sum::<usize>() <= super::CONTINUITY_TAIL_TARGET_TOKENS);
    }

    #[test]
    fn continuity_tail_bounds_tool_call_metadata_without_breaking_correlation() {
        let mut assistant = Message::assistant(String::new());
        assistant.tool_calls = Some(vec![ToolCall::function(
            "call-large".into(),
            "run_command".into(),
            format!("{{\"command\":\"{}\"}}", "x".repeat(300_000)),
        )]);
        let history = vec![
            Message::user("run the command".into()),
            assistant,
            Message::tool_response("call-large".into(), "completed".into()),
        ];

        let tail = continuity_tail(&history);
        assert!(tail.iter().map(Message::estimate_tokens).sum::<usize>() <= super::CONTINUITY_TAIL_TARGET_TOKENS);
        let call = tail[1].tool_calls.as_ref().expect("tool call should remain").first().unwrap();
        assert_eq!(call.id, "call-large");
        assert_eq!(call.function.as_ref().unwrap().arguments, "{}");
        assert_eq!(tail[2].tool_call_id.as_deref(), Some("call-large"));
    }

    #[test]
    fn provider_normalization_falls_back_to_source_tail_and_drops_pending_call() {
        let source = vec![
            Message::user("latest request".into()),
            Message::assistant("finished".into()),
        ];
        let provider_output = vec![Message::system("provider summary".into())];

        let normalized = super::normalize_provider_compacted_history(provider_output, &source);

        assert_eq!(normalized.len(), 3);
        assert_eq!(normalized[0].content.as_text(), "provider summary");
        assert_eq!(normalized[1].content.as_text(), "latest request");
        assert_eq!(normalized[2].content.as_text(), "finished");

        let malformed = vec![Message::system("summary".into()), {
            let mut message = Message::assistant("pending".into());
            message.tool_calls = Some(vec![ToolCall::function("c1".into(), "run".into(), "{}".into())]);
            message
        }];
        let normalized = super::normalize_provider_compacted_history(malformed, &source);
        assert_eq!(normalized.len(), 3);
        assert_eq!(normalized[0].content.as_text(), "summary");
        assert_eq!(normalized[1].content.as_text(), "latest request");

        let partial = vec![
            Message::system("provider summary".into()),
            Message::user("older request".into()),
            Message::assistant("older result".into()),
            Message::user("latest request".into()),
            Message::assistant_with_tools(
                "calling".into(),
                vec![
                    ToolCall::function("c1".into(), "run".into(), "{}".into()),
                    ToolCall::function("c2".into(), "run".into(), "{}".into()),
                ],
            ),
            Message::tool_response("c1".into(), "partial result".into()),
        ];
        let normalized = super::normalize_provider_compacted_history(partial, &source);
        assert_eq!(normalized[0].content.as_text(), "provider summary");
        assert_eq!(normalized.last().unwrap().content.as_text(), "latest request");
        assert!(!normalized.iter().any(|message| message.tool_call_id.as_deref() == Some("c1")));
        assert!(!normalized.iter().any(|message| message.tool_calls.is_some()));
    }

    #[test]
    fn provider_normalization_preserves_duplicate_tagged_intents() {
        use vtcode_commons::message_metadata::MessageMetadata;

        let mut first = Message::user("same follow-up".into());
        first.metadata = Some(MessageMetadata::user_input(1, first.estimate_tokens()).with_intent_id("intent-1"));
        let mut second = Message::user("same follow-up".into());
        second.metadata = Some(MessageMetadata::user_input(2, second.estimate_tokens()).with_intent_id("intent-2"));
        let source = vec![first, second];

        let normalized = super::normalize_provider_compacted_history(vec![Message::system("summary".into())], &source);
        let intent_ids = normalized
            .iter()
            .filter_map(|message| message.metadata.as_ref().and_then(|metadata| metadata.intent_id()))
            .collect::<Vec<_>>();

        assert_eq!(intent_ids, vec!["intent-1", "intent-2"]);
    }

    /// History-growth verify-item: the local summarization prompt must exclude
    /// `Message.reasoning` / `reasoning_details` and serialize only the visible
    /// text content (`content.as_text()`). Reasoning traces are large and
    /// ephemeral; including them in every compaction summary would bloat the
    /// post-compaction context and re-inject stale chain-of-thought. The
    /// continuity tail preserves provider-protocol reasoning (Anthropic
    /// thinking signatures, OpenAI reasoning items) separately and is NOT
    /// summarized, so stripping reasoning here is safe and correct. This test
    /// pins that invariant so a change to `build_summary_prompt` cannot
    /// accidentally start including reasoning.
    #[test]
    fn build_summary_prompt_excludes_reasoning_traces() {
        use super::build_summary_prompt;

        let instructions = "Summarize the conversation.";
        // Assistant message carrying a reasoning trace alongside its visible
        // content. If `build_summary_prompt` ever reads `reasoning`, the
        // summary would contain "SECRET_REASONING" and "raw-only reasoning".
        let history = vec![
            Message::user("What is 2+2?".to_string()),
            Message::assistant("The answer is 4.".to_string())
                .with_reasoning(Some("SECRET_REASONING: I computed 2+2=4.".to_string())),
        ];

        let prompt = build_summary_prompt(&history, instructions);

        assert!(prompt.contains("The answer is 4."), "visible assistant text must appear in the summary prompt");
        assert!(
            !prompt.contains("SECRET_REASONING"),
            "Message.reasoning must NOT be included in the summary prompt -- \
             it would bloat every compaction pass with ephemeral chain-of-thought"
        );
        assert!(prompt.contains("Summarize the conversation."), "instructions must appear in the summary prompt");
    }

    #[test]
    fn coherence_force_keeps_tool_results_not_in_selection() {
        // Regression: `coherence_tool_call_pairs` force-keeps the Tool results
        // that follow a retained Assistant-with-tool_calls, but the previous
        // implementation derived its output from `selected` only, so those
        // force-kept messages silently vanished. A compacted history can then
        // carry an Assistant tool-call with no result, which providers reject.
        use super::coherence_tool_call_pairs;

        let history = vec![
            Message::user("check the code".to_string()),
            Message::assistant("Looking...".to_string()).with_tool_calls(vec![ToolCall {
                id: "call_1".to_string(),
                call_type: "function".to_string(),
                function: Some(crate::llm::provider::FunctionCall {
                    namespace: None,
                    name: "read_file".to_string(),
                    arguments: "{}".to_string(),
                }),
                text: None,
                thought_signature: None,
            }]),
            Message::tool_response("call_1".to_string(), "tool result that must survive".to_string()),
            Message::assistant("Done.".to_string()),
        ];

        // Selection keeps only the assistant-with-tool-calls turn, dropping the
        // following Tool result from the budget-driven selection.
        let selected = vec![(1, history[1].clone())];
        let result = coherence_tool_call_pairs(&history, &selected);

        let tool_results = result
            .iter()
            .filter(|(_, m)| m.role == MessageRole::Tool)
            .map(|(idx, m)| (*idx, m.content.as_text().to_string()))
            .collect::<Vec<_>>();

        assert_eq!(
            tool_results,
            vec![(2usize, "tool result that must survive".to_string())],
            "force-kept tool result must survive compaction even when not selected"
        );
        // History order must be preserved with the assistant preceding its result.
        let indices = result.iter().map(|(idx, _)| *idx).collect::<Vec<_>>();
        assert_eq!(indices, vec![1, 2]);
    }

    #[test]
    fn coherence_drops_orphaned_tool_results_of_unretained_assistant() {
        use super::coherence_tool_call_pairs;

        let history = vec![
            Message::user("check".to_string()),
            Message::assistant("Looking...".to_string()).with_tool_calls(vec![ToolCall {
                id: "call_1".to_string(),
                call_type: "function".to_string(),
                function: Some(crate::llm::provider::FunctionCall {
                    namespace: None,
                    name: "read_file".to_string(),
                    arguments: "{}".to_string(),
                }),
                text: None,
                thought_signature: None,
            }]),
            Message::tool_response("call_1".to_string(), "orphan result".to_string()),
        ];

        // The calling assistant was NOT retained; its orphaned Tool result must
        // be dropped so we never emit a result the model never saw a call for.
        let selected = vec![(0, history[0].clone())];
        let result = coherence_tool_call_pairs(&history, &selected);

        assert_eq!(result.len(), 1, "orphaned tool result must be dropped");
        assert_eq!(result[0].0, 0);
    }
}
