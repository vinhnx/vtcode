use crate::provider::{AnthropicThinkingDisplayOverride, AnthropicThinkingModeOverride, LLMRequest};
use crate::providers::anthropic_types::{ThinkingConfig, ThinkingDisplay};
use crate::rig_adapter::RigProviderCapabilities;
use serde_json::{Value, json};
use std::env;
use tracing::warn;
use vtcode_config::constants::env_vars;
use vtcode_config::core::AnthropicConfig;
use vtcode_config::models::Provider;
use vtcode_config::types::ReasoningEffortLevel;

use vtcode_config::constants::models::anthropic;

use super::super::capabilities::{
    claude_thinking_profile, default_effort_for_model, effort_is_at_most_high, matches_model, resolve_model_name,
    supports_reasoning_effort,
};

fn resolve_configured_thinking_display(anthropic_config: &AnthropicConfig) -> Option<ThinkingDisplay> {
    anthropic_config.thinking_display.and_then(|d| match d {
        vtcode_config::ThinkingDisplayMode::Summarized => Some(ThinkingDisplay::Summarized),
        vtcode_config::ThinkingDisplayMode::Omitted => Some(ThinkingDisplay::Omitted),
        vtcode_config::ThinkingDisplayMode::Unknown => None,
    })
}

fn resolve_thinking_display(request: &LLMRequest, anthropic_config: &AnthropicConfig) -> Option<ThinkingDisplay> {
    if let Some(overrides) = request.anthropic_request_overrides.as_ref() {
        return match overrides.thinking_display {
            AnthropicThinkingDisplayOverride::Inherit => None,
            AnthropicThinkingDisplayOverride::Summarized => Some(ThinkingDisplay::Summarized),
            AnthropicThinkingDisplayOverride::Omitted => Some(ThinkingDisplay::Omitted),
        };
    }

    resolve_configured_thinking_display(anthropic_config)
}

fn manual_thinking_config(
    budget: u32,
    max_tokens: Option<u32>,
    display: Option<ThinkingDisplay>,
) -> Option<ThinkingConfig> {
    if budget < 1024 {
        return None;
    }

    let max_tokens = max_tokens.unwrap_or(16000);
    let effective_budget = budget.min(max_tokens.saturating_sub(100)).max(1024);
    Some(ThinkingConfig::Enabled { budget_tokens: effective_budget, display })
}

pub(crate) fn build_thinking_config(
    request: &LLMRequest,
    anthropic_config: &AnthropicConfig,
    default_model: &str,
) -> Result<(Option<ThinkingConfig>, Option<Value>), crate::provider::LLMError> {
    let resolved_model = resolve_model_name(&request.model, default_model);
    let profile = claude_thinking_profile(resolved_model, default_model);
    let display = resolve_thinking_display(request, anthropic_config);
    let default_thinking = profile.is_some_and(|p| p.default_thinking_enabled);

    if let Some(overrides) = request.anthropic_request_overrides.as_ref() {
        match overrides.thinking_mode {
            AnthropicThinkingModeOverride::Disabled => {
                if default_thinking {
                    if matches_model(resolved_model, anthropic::CLAUDE_OPUS_5) {
                        if effort_is_at_most_high(request, anthropic_config) {
                            return Ok((Some(ThinkingConfig::Disabled), None));
                        }
                        return Ok((None, None));
                    }
                    if matches_model(resolved_model, anthropic::CLAUDE_SONNET_5) {
                        return Ok((Some(ThinkingConfig::Disabled), None));
                    }
                }
                return Ok((None, None));
            }
            AnthropicThinkingModeOverride::Adaptive => {
                return Ok((Some(ThinkingConfig::Adaptive { display }), None));
            }
            AnthropicThinkingModeOverride::ManualBudget(budget) => {
                return Ok((manual_thinking_config(budget, request.max_tokens, display), None));
            }
            AnthropicThinkingModeOverride::Inherit => {}
        }
    }

    let thinking_enabled = if default_thinking {
        if !anthropic_config.extended_thinking_enabled {
            tracing::warn!(
                model = %request.model,
                "extended_thinking_enabled=false overridden by model default thinking profile; thinking will be enabled"
            );
        }
        true
    } else {
        anthropic_config.extended_thinking_enabled && supports_reasoning_effort(resolved_model, default_model)
    };

    if thinking_enabled {
        if profile.is_some_and(|p| matches!(p.mode, super::super::capabilities::ClaudeThinkingMode::Adaptive)) {
            if profile.is_some_and(|p| p.supports_manual_budget)
                && let Some(explicit_budget) = request.thinking_budget
            {
                return Ok((manual_thinking_config(explicit_budget, request.max_tokens, display), None));
            }
            return Ok((Some(ThinkingConfig::Adaptive { display }), None));
        }

        let max_thinking_tokens: Option<u32> =
            env::var(env_vars::MAX_THINKING_TOKENS).ok().and_then(|v| v.parse().ok());

        let budget = if let Some(explicit_budget) = request.thinking_budget {
            explicit_budget
        } else if let Some(env_budget) = max_thinking_tokens {
            env_budget
        } else if let Some(effort) = request.reasoning_effort {
            match effort {
                ReasoningEffortLevel::None | ReasoningEffortLevel::Unknown => 0,
                ReasoningEffortLevel::Minimal => 1024,
                ReasoningEffortLevel::Low => 4096,
                ReasoningEffortLevel::Medium => 8192,
                ReasoningEffortLevel::High => 16384,
                ReasoningEffortLevel::XHigh => 32768,
                ReasoningEffortLevel::Max => 32768,
            }
        } else {
            anthropic_config.interleaved_thinking_budget_tokens
        };

        if let Some(thinking) = manual_thinking_config(budget, request.max_tokens, display) {
            return Ok((Some(thinking), None));
        }
    } else if let Some(effort) = request.reasoning_effort {
        if profile.is_some_and(|p| matches!(p.mode, super::super::capabilities::ClaudeThinkingMode::Adaptive)) {
            return Ok((None, None));
        }

        if let Some(payload) =
            RigProviderCapabilities::new(Provider::Anthropic, &request.model).reasoning_parameters(effort)?
        {
            return Ok((None, Some(payload)));
        } else {
            return Ok((None, Some(json!({ "effort": effort.as_str() }))));
        }
    }

    Ok((None, None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vtcode_config::constants::models::anthropic;

    #[test]
    fn ignores_explicit_budget_for_opus_4_8() {
        let request = LLMRequest {
            model: anthropic::CLAUDE_OPUS_5.to_string(),
            thinking_budget: Some(2048),
            ..Default::default()
        };
        let config = AnthropicConfig::default();
        let (thinking, _) =
            build_thinking_config(&request, &config, anthropic::DEFAULT_MODEL).expect("thinking config");

        assert!(matches!(thinking, Some(ThinkingConfig::Adaptive { .. })));
    }

    #[test]
    fn adaptive_thinking_includes_summarized_display() {
        let request = LLMRequest {
            model: anthropic::CLAUDE_SONNET_5.to_string(),
            ..Default::default()
        };
        let config = AnthropicConfig {
            thinking_display: Some(vtcode_config::ThinkingDisplayMode::Summarized),
            ..AnthropicConfig::default()
        };
        let (thinking, _) =
            build_thinking_config(&request, &config, anthropic::DEFAULT_MODEL).expect("thinking config");

        match thinking {
            Some(ThinkingConfig::Adaptive { display: Some(ThinkingDisplay::Summarized) }) => {}
            other => panic!("expected Adaptive with Summarized display, got {other:?}"),
        }
    }

    #[test]
    fn adaptive_thinking_includes_omitted_display() {
        let request = LLMRequest {
            model: anthropic::CLAUDE_SONNET_5.to_string(),
            ..Default::default()
        };
        let config = AnthropicConfig {
            thinking_display: Some(vtcode_config::ThinkingDisplayMode::Omitted),
            ..AnthropicConfig::default()
        };
        let (thinking, _) =
            build_thinking_config(&request, &config, anthropic::DEFAULT_MODEL).expect("thinking config");

        match thinking {
            Some(ThinkingConfig::Adaptive { display: Some(ThinkingDisplay::Omitted) }) => {}
            other => panic!("expected Adaptive with Omitted display, got {other:?}"),
        }
    }

    #[test]
    fn adaptive_thinking_includes_display_for_sonnet_4_6_when_configured() {
        let request = LLMRequest {
            model: anthropic::CLAUDE_SONNET_5.to_string(),
            ..Default::default()
        };
        let config = AnthropicConfig {
            thinking_display: Some(vtcode_config::ThinkingDisplayMode::Summarized),
            ..AnthropicConfig::default()
        };
        let (thinking, _) =
            build_thinking_config(&request, &config, anthropic::DEFAULT_MODEL).expect("thinking config");

        match thinking {
            Some(ThinkingConfig::Adaptive { display: Some(ThinkingDisplay::Summarized) }) => {}
            other => panic!("expected Adaptive with Summarized display, got {other:?}"),
        }
    }

    #[test]
    fn thinking_display_defaults_to_none() {
        let request = LLMRequest {
            model: anthropic::CLAUDE_SONNET_5.to_string(),
            ..Default::default()
        };
        let config = AnthropicConfig::default();
        let (thinking, _) =
            build_thinking_config(&request, &config, anthropic::DEFAULT_MODEL).expect("thinking config");

        match thinking {
            Some(ThinkingConfig::Adaptive { display: None }) => {}
            other => panic!("expected Adaptive with no display, got {other:?}"),
        }
    }
}
