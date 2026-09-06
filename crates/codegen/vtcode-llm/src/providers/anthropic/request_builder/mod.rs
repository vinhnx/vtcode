//! Request building for Anthropic Claude API.

mod hardening;
mod messages;
mod system;
mod thinking;
pub(crate) mod tools;

use crate::provider::{
    AnthropicOptionalStringOverride, AnthropicOptionalU32Override, AnthropicThinkingConfig, LLMError, LLMRequest,
    PromptCacheProfile,
};
use crate::providers::anthropic_types::{
    AnthropicAdvisorCaching, AnthropicAdvisorTool, AnthropicFallbackParam, AnthropicOutputConfig,
    AnthropicOutputFormat, AnthropicRequest, AnthropicTaskBudget, AnthropicTool, CacheControl, ThinkingConfig,
    ThinkingDisplay,
};
use serde_json::{Value, json};
use vtcode_config::constants::reasoning;
use vtcode_config::core::{AdvisorConfig, AnthropicConfig, AnthropicPromptCacheSettings};
use vtcode_config::types::ReasoningEffortLevel;

use super::capabilities::{
    default_effort_for_model, effort_allowed_for_model, rejects_sampling, resolve_model_name,
    supports_assistant_prefill, supports_effort, supports_mid_conversation_system_messages, supports_task_budget,
};
use super::prompt_cache::{get_messages_cache_ttl, get_tools_cache_ttl};
use messages::{build_messages, hoist_largest_user_message};
use system::{SystemPromptBuildResult, build_system_prompt};
use thinking::build_thinking_config;
use tools::{build_tool_choice, build_tools};

#[cfg(test)]
pub(crate) use messages::tool_result_blocks;

pub(crate) struct RequestBuilderContext<'a> {
    pub(crate) prompt_cache_enabled: bool,
    pub(crate) prompt_cache_settings: &'a AnthropicPromptCacheSettings,
    pub(crate) anthropic_config: &'a AnthropicConfig,
    pub(crate) model: &'a str,
}

fn resolve_messages_ttl(request: &LLMRequest, ctx: &RequestBuilderContext<'_>) -> &'static str {
    if !ctx.prompt_cache_enabled {
        return "5m";
    }

    match request.prompt_cache_profile {
        Some(PromptCacheProfile::BudgetContinuation) => "1h",
        None => get_messages_cache_ttl(ctx.prompt_cache_settings),
    }
}

pub(crate) fn convert_to_anthropic_format(
    request: &LLMRequest,
    ctx: &RequestBuilderContext,
) -> Result<Value, LLMError> {
    let resolved_model = resolve_model_name(&request.model, ctx.model);
    let tools_ttl = if ctx.prompt_cache_enabled {
        get_tools_cache_ttl(ctx.prompt_cache_settings)
    } else {
        "5m"
    };

    let messages_ttl = resolve_messages_ttl(request, ctx);

    let tools_cache_control = if ctx.prompt_cache_enabled && ctx.prompt_cache_settings.cache_tool_definitions {
        Some(CacheControl {
            control_type: "ephemeral".into(),
            ttl: Some(tools_ttl.into()),
        })
    } else {
        None
    };

    let system_cache_control = if ctx.prompt_cache_enabled && ctx.prompt_cache_settings.cache_system_messages {
        Some(CacheControl {
            control_type: "ephemeral".into(),
            ttl: Some(tools_ttl.into()),
        })
    } else {
        None
    };

    let max_breakpoints = if ctx.prompt_cache_enabled {
        ctx.prompt_cache_settings.max_breakpoints as usize
    } else {
        0
    };
    let mut breakpoints_remaining = max_breakpoints;

    let tools_breakpoints_before = breakpoints_remaining;
    let mut tools = build_tools(request, &tools_cache_control, &mut breakpoints_remaining)?;
    let tools_breakpoints_used = tools_breakpoints_before.saturating_sub(breakpoints_remaining);

    // Inject the Anthropic server-side advisor tool when enabled and the executor
    // model forms a valid pair with the configured advisor model.
    let advisor_injected =
        if let Some(advisor_tool) = resolve_advisor_tool(resolved_model, &ctx.anthropic_config.advisor) {
            let mut built = tools.unwrap_or_default();
            built.push(advisor_tool);
            tools = Some(built);
            true
        } else {
            false
        };

    let allow_mid_conversation_system = supports_mid_conversation_system_messages(resolved_model, ctx.model);

    let SystemPromptBuildResult {
        mut system_value,
        breakpoints_used,
        has_uncached_runtime_context,
    } = build_system_prompt(request, &system_cache_control, breakpoints_remaining, !allow_mid_conversation_system);
    breakpoints_remaining = breakpoints_remaining.saturating_sub(breakpoints_used);

    // When the advisor tool is active, append a system-prompt block guiding the
    // executor model on when to invoke it (per Anthropic's recommended prompt
    // for coding tasks).
    if advisor_injected {
        let advisor_guidance = concat!(
            "You have access to an advisor tool that pairs a faster executor model with a ",
            "higher-intelligence advisor model for strategic guidance mid-generation. ",
            "Use the advisor tool when you:\n",
            "- Need a second opinion on a complex architectural decision\n",
            "- Are unsure about the best approach to a multi-step problem\n",
            "- Want to validate your plan before executing many tool calls\n",
            "- Hit a blocker you cannot resolve alone\n",
            "When the advisor returns guidance, incorporate it into your response. ",
            "If the advisor suggests a different approach, weigh it against your own reasoning.",
        );
        let guidance_block = json!({
            "type": "text",
            "text": advisor_guidance,
        });
        match &mut system_value {
            Some(Value::Array(blocks)) => {
                blocks.push(guidance_block);
            }
            Some(Value::String(text)) => {
                let existing = std::mem::take(text);
                system_value = Some(Value::Array(vec![json!({ "type": "text", "text": existing }), guidance_block]));
            }
            // build_system_prompt only produces String, Array, or None —
            // this arm is defensive for forward-compatibility.
            _ => {
                system_value = Some(Value::Array(vec![guidance_block]));
            }
        }
    }

    let messages_cache_control = if ctx.prompt_cache_enabled && ctx.prompt_cache_settings.cache_user_messages {
        Some(CacheControl {
            control_type: "ephemeral".into(),
            ttl: Some(messages_ttl.into()),
        })
    } else {
        None
    };

    let needs_hoisting = request
        .coding_agent_settings
        .as_ref()
        .is_some_and(|s| s.long_context_optimization)
        && request.messages.len() > 1;

    // Only clone the message vector when hoisting will actually mutate it.
    // In the common case (no long-context optimization or single message),
    // borrow the original slice and skip the deep copy.
    let mut hoisted_messages: Vec<crate::provider::Message>;
    let messages_to_process: &[crate::provider::Message] = if needs_hoisting {
        hoisted_messages = request.messages.as_ref().clone();
        hoist_largest_user_message(&mut hoisted_messages);
        &hoisted_messages
    } else {
        request.messages.as_ref().as_slice()
    };

    let messages_breakpoints_before = breakpoints_remaining;
    let messages = build_messages(
        request,
        messages_to_process,
        &messages_cache_control,
        ctx.prompt_cache_settings,
        &mut breakpoints_remaining,
        ctx.model,
    )?;
    let messages_breakpoints_used = messages_breakpoints_before.saturating_sub(breakpoints_remaining);
    let explicit_breakpoints_used = max_breakpoints.saturating_sub(breakpoints_remaining);

    let (thinking_val, reasoning_val) = build_thinking_config(request, ctx.anthropic_config, ctx.model)?;

    let final_tool_choice = build_tool_choice(request, &thinking_val);
    let anthropic_overrides = request.anthropic_request_overrides.as_ref();
    let thinking_is_adaptive = matches!(thinking_val, Some(ThinkingConfig::Adaptive { .. }));

    let adaptive_effort = if thinking_is_adaptive && request.effort.is_none() {
        request
            .reasoning_effort
            .map(|effort| effort_from_reasoning_for_adaptive(effort).to_string())
    } else {
        None
    };
    let effort_value = if supports_effort(resolved_model, ctx.model) && thinking_is_adaptive {
        match anthropic_overrides.map(|overrides| &overrides.effort) {
            Some(AnthropicOptionalStringOverride::Explicit(effort)) => Some(effort.to_ascii_lowercase()),
            Some(AnthropicOptionalStringOverride::Omit) => None,
            _ => request
                .effort
                .as_ref()
                .map(|effort| effort.to_ascii_lowercase())
                .or_else(|| adaptive_effort.as_ref().map(|effort| effort.to_ascii_lowercase()))
                .or_else(|| {
                    thinking_val.as_ref().and_then(|_| {
                        let configured_effort = ctx.anthropic_config.effort.as_str();
                        if effort_allowed_for_model(resolved_model, ctx.model, configured_effort) {
                            Some(configured_effort.to_string())
                        } else {
                            default_effort_for_model(resolved_model, ctx.model).map(str::to_string)
                        }
                    })
                }),
        }
    } else {
        None
    };
    let task_budget = if supports_task_budget(resolved_model, ctx.model) {
        match anthropic_overrides.map(|overrides| &overrides.task_budget_tokens) {
            Some(AnthropicOptionalU32Override::Explicit(total)) => {
                Some(AnthropicTaskBudget { budget_type: "tokens".to_string(), total: *total })
            }
            Some(AnthropicOptionalU32Override::Omit) => None,
            _ => ctx
                .anthropic_config
                .task_budget_tokens
                .map(|total| AnthropicTaskBudget { budget_type: "tokens".to_string(), total }),
        }
    } else {
        None
    };
    let output_format = request
        .output_format
        .as_ref()
        .map(|schema| AnthropicOutputFormat::JsonSchema { schema: schema.clone() });
    let output_config = if effort_value.is_some() || task_budget.is_some() || output_format.is_some() {
        Some(AnthropicOutputConfig {
            effort: effort_value,
            task_budget,
            format: output_format,
        })
    } else {
        None
    };

    let effective_temperature = if thinking_val.is_some() || rejects_sampling(resolved_model, ctx.model) {
        None
    } else {
        request.temperature
    };

    let top_level_cache_control =
        if ctx.prompt_cache_enabled && explicit_breakpoints_used < max_breakpoints && !has_uncached_runtime_context {
            let ttl = if messages_breakpoints_used > 0 {
                messages_ttl
            } else if breakpoints_used > 0 || tools_breakpoints_used > 0 {
                tools_ttl
            } else {
                messages_ttl
            };

            Some(CacheControl {
                control_type: "ephemeral".into(),
                ttl: Some(ttl.into()),
            })
        } else {
            None
        };

    let mut anthropic_request = AnthropicRequest {
        model: resolved_model.to_string(),
        max_tokens: request.max_tokens.unwrap_or(if thinking_val.is_some() { 16000 } else { 4096 }),
        cache_control: top_level_cache_control,
        messages,
        system: system_value,
        temperature: effective_temperature,
        tools,
        tool_choice: final_tool_choice,
        thinking: thinking_val,
        reasoning: reasoning_val,
        output_config: output_config.map(Into::into),
        context_management: request.context_management.clone(),
        fallbacks: request.fallbacks.as_ref().map(|fallbacks| {
            fallbacks
                .iter()
                .map(|fb| AnthropicFallbackParam {
                    model: fb.model.clone(),
                    max_tokens: fb.max_tokens,
                    thinking: fb.thinking.as_ref().map(|t| match t {
                        AnthropicThinkingConfig::Disabled => ThinkingConfig::Disabled,
                        AnthropicThinkingConfig::Enabled { budget_tokens, display } => ThinkingConfig::Enabled {
                            budget_tokens: *budget_tokens,
                            display: display.as_ref().and_then(|d| match d.as_str() {
                                "summarized" => Some(ThinkingDisplay::Summarized),
                                "omitted" => Some(ThinkingDisplay::Omitted),
                                _ => None,
                            }),
                        },
                        AnthropicThinkingConfig::Adaptive { display } => ThinkingConfig::Adaptive {
                            display: display.as_ref().and_then(|d| match d.as_str() {
                                "summarized" => Some(ThinkingDisplay::Summarized),
                                "omitted" => Some(ThinkingDisplay::Omitted),
                                _ => None,
                            }),
                        },
                    }),
                })
                .collect()
        }),
        fallback_credit_token: request.fallback_credit_token.clone(),
        stream: request.stream,
    };

    hardening::strip_globally_orphaned_tool_blocks(&mut anthropic_request.messages);
    hardening::enforce_tool_use_result_adjacency(&mut anthropic_request.messages);
    hardening::hoist_tool_results_to_front(&mut anthropic_request.messages);
    hardening::guard_trailing_assistant_message(
        &mut anthropic_request.messages,
        supports_assistant_prefill(resolved_model, ctx.model),
    );

    serde_json::to_value(anthropic_request).map_err(|e| LLMError::Provider {
        message: format!("Serialization error: {e}"),
        metadata: None,
    })
}

fn effort_from_reasoning_for_adaptive(effort: ReasoningEffortLevel) -> &'static str {
    match effort {
        ReasoningEffortLevel::None | ReasoningEffortLevel::Minimal | ReasoningEffortLevel::Low => reasoning::LOW,
        _ => effort.as_str(),
    }
}

/// Whether the given model is an Anthropic model eligible to act as an advisor
/// executor. Server-side advisor tooling is only available on Anthropic models.
///
/// Model ids may carry a `-YYYYMMDD` version pin; this strips the single known
/// dated suffix before checking against the supported set. Keep this in lockstep
/// with `vtcode_config::constants::models::anthropic::normalize_model_id` so the
/// executor check and the advisor-pair validation agree on normalization.
fn is_anthropic_executor_model(model: &str) -> bool {
    use vtcode_config::constants::models::anthropic::{SUPPORTED_MODELS, normalize_model_id};
    let normalized = normalize_model_id(model);
    SUPPORTED_MODELS.contains(&normalized)
}

/// Single source of truth for the Anthropic server-side advisor tool.
///
/// Resolves and builds the `advisor_20260301` tool, or returns `None` when the
/// advisor is disabled, the executor is not an eligible Anthropic model, or the
/// executor/advisor model pair fails `validate_advisor_pair`. Both the request
/// builder (tool injection) and the provider (beta-header gating) call this so
/// the two can never disagree.
pub(crate) fn resolve_advisor_tool(executor: &str, advisor: &AdvisorConfig) -> Option<AnthropicTool> {
    if !advisor.enabled {
        return None;
    }

    if !is_anthropic_executor_model(executor) {
        return None;
    }

    let advisor_model = if advisor.model.is_empty() {
        vtcode_config::constants::models::anthropic::default_advisor_model(executor).to_string()
    } else {
        advisor.model.clone()
    };

    if let Err(reason) = vtcode_config::constants::models::anthropic::validate_advisor_pair(executor, &advisor_model) {
        tracing::warn!(%reason, "advisor tool disabled: invalid model pair");
        return None;
    }

    // Validate max_tokens if specified (API minimum is 1024).
    if let Some(max_tokens) = advisor.max_tokens
        && max_tokens < 1024
    {
        tracing::warn!(max_tokens, "advisor tool disabled: max_tokens must be >= 1024");
        return None;
    }

    let caching = advisor.caching.and_then(|c| {
        c.enabled.then_some(AnthropicAdvisorCaching {
            cache_type: "ephemeral".to_string(),
            ttl: c.ttl.as_str().to_string(),
        })
    });

    Some(AnthropicTool::Advisor(AnthropicAdvisorTool {
        tool_type: "advisor_20260301".to_string(),
        name: "advisor".to_string(),
        model: advisor_model,
        max_uses: advisor.max_uses,
        max_tokens: advisor.max_tokens,
        caching,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vtcode_config::core::AdvisorConfig;

    fn advisor_config(enabled: bool, model: &str, max_uses: Option<u32>) -> AdvisorConfig {
        AdvisorConfig {
            enabled,
            model: model.to_string(),
            max_uses,
            max_tokens: None,
            caching: None,
        }
    }

    #[test]
    fn resolve_advisor_tool_disabled_returns_none() {
        let cfg = advisor_config(false, "", None);
        assert!(resolve_advisor_tool("claude-sonnet-5", &cfg).is_none());
    }

    #[test]
    fn resolve_advisor_tool_non_anthropic_executor_returns_none() {
        let cfg = advisor_config(true, "", None);
        assert!(resolve_advisor_tool("gpt-4o", &cfg).is_none());
    }

    #[test]
    fn resolve_advisor_tool_defaults_to_valid_pair() {
        let cfg = advisor_config(true, "", None);
        let tool = resolve_advisor_tool("claude-sonnet-5", &cfg);
        assert!(matches!(tool, Some(AnthropicTool::Advisor(_))));
        if let Some(AnthropicTool::Advisor(t)) = tool {
            assert_eq!(t.model, "claude-opus-5");
            assert_eq!(t.name, "advisor");
            assert_eq!(t.tool_type, "advisor_20260301");
        }
    }

    #[test]
    fn resolve_advisor_tool_invalid_pair_returns_none() {
        // Advisor less capable than the executor must be rejected.
        let cfg = advisor_config(true, "claude-sonnet-5", None);
        assert!(resolve_advisor_tool("claude-opus-5", &cfg).is_none());
    }

    #[test]
    fn resolve_advisor_tool_accepts_self_advising_model() {
        let cfg = advisor_config(true, "claude-fable-5", None);
        assert!(resolve_advisor_tool("claude-fable-5", &cfg).is_some());
        // Fable may only advise Fable.
        assert!(resolve_advisor_tool("claude-opus-5", &cfg).is_none());
    }

    #[test]
    fn resolve_advisor_tool_supports_dated_executor_suffix() {
        let cfg = advisor_config(true, "", None);
        // A dated pin for a currently supported model must not break the
        // supported-model check.
        assert!(resolve_advisor_tool("claude-sonnet-5-20251001", &cfg).is_some());
    }
}
