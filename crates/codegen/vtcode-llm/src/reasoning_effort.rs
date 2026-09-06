//! Capability-driven reasoning validation before a provider request is sent.

use anyhow::Result;
use vtcode_config::types::ReasoningEffortLevel;

use crate::provider::LLMProvider;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReasoningEffortMapping {
    pub requested: ReasoningEffortLevel,
    pub effective: ReasoningEffortLevel,
}

impl ReasoningEffortMapping {
    pub fn degraded(self) -> bool {
        self.requested != self.effective
    }
}

/// A pre-request block, distinct from a retryable transport failure.
#[derive(Debug)]
pub struct ReasoningEffortUnsupported {
    pub requested: ReasoningEffortLevel,
    pub supported: compact_str::CompactString,
}

impl std::fmt::Display for ReasoningEffortUnsupported {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Requested reasoning effort `{}` is unsupported by this route (supported: {}). Select a supported effort or explicitly enable agent.allow_reasoning_effort_downgrade",
            self.requested, self.supported
        )
    }
}

impl std::error::Error for ReasoningEffortUnsupported {}

pub struct ReasoningEffortMapper;

impl ReasoningEffortMapper {
    pub fn resolve(
        provider: &dyn LLMProvider,
        model: &str,
        requested: ReasoningEffortLevel,
        allow_downgrade: bool,
    ) -> Result<ReasoningEffortMapping> {
        let supported = if let Some(levels) = crate::provider::catalog_reasoning_efforts(provider.name(), model) {
            levels
        } else if provider.supports_reasoning_effort(model) {
            provider.supported_reasoning_efforts(model)
        } else {
            &[]
        };
        Self::map(requested, supported, allow_downgrade)
    }

    pub fn map(
        requested: ReasoningEffortLevel,
        supported: &[&str],
        allow_downgrade: bool,
    ) -> Result<ReasoningEffortMapping> {
        if requested == ReasoningEffortLevel::None || supported.contains(&requested.as_str()) {
            return Ok(ReasoningEffortMapping { requested, effective: requested });
        }
        let ordered_levels = [
            ReasoningEffortLevel::Minimal,
            ReasoningEffortLevel::Low,
            ReasoningEffortLevel::Medium,
            ReasoningEffortLevel::High,
            ReasoningEffortLevel::XHigh,
            ReasoningEffortLevel::Max,
        ];
        if allow_downgrade
            && let Some(position) = ordered_levels.iter().position(|level| *level == requested)
            && let Some(effective) = ordered_levels
                .iter()
                .take(position)
                .rev()
                .find(|level| supported.contains(&level.as_str()))
        {
            return Ok(ReasoningEffortMapping { requested, effective: *effective });
        }
        Err(ReasoningEffortUnsupported { requested, supported: supported.join(", ").into() }.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actual_provider_reasoning_matrix_preserves_all_supported_levels() {
        use crate::providers::{AnthropicProvider, GeminiProvider, OpenAIProvider};
        use vtcode_config::constants::models;
        let providers: [(Box<dyn LLMProvider>, &str, &[&str]); 3] = [
            (
                Box::new(OpenAIProvider::new("offline-fixture".into())),
                models::openai::DEFAULT_MODEL,
                &["low", "medium", "high", "xhigh", "max"],
            ),
            (
                Box::new(AnthropicProvider::new("offline-fixture".into())),
                models::anthropic::DEFAULT_MODEL,
                &["low", "medium", "high", "xhigh", "max"],
            ),
            (
                Box::new(GeminiProvider::new("offline-fixture".into())),
                models::google::DEFAULT_MODEL,
                &["low", "medium", "high"],
            ),
        ];
        for (provider, model, expected_levels) in providers {
            assert_eq!(provider.supported_reasoning_efforts(model), expected_levels);
            for requested in [
                ReasoningEffortLevel::None,
                ReasoningEffortLevel::Minimal,
                ReasoningEffortLevel::Low,
                ReasoningEffortLevel::Medium,
                ReasoningEffortLevel::High,
                ReasoningEffortLevel::XHigh,
                ReasoningEffortLevel::Max,
                ReasoningEffortLevel::Unknown,
            ] {
                let result = ReasoningEffortMapper::resolve(provider.as_ref(), model, requested, false);
                let expected = requested == ReasoningEffortLevel::None || expected_levels.contains(&requested.as_str());
                assert_eq!(result.is_ok(), expected, "{} {model} {requested}", provider.name());
                if let Ok(mapping) = result {
                    assert_eq!(mapping.requested, mapping.effective);
                } else {
                    assert!(result.unwrap_err().downcast_ref::<ReasoningEffortUnsupported>().is_some());
                }
            }
        }
    }

    #[test]
    fn unknown_route_fails_closed_for_active_reasoning() {
        let provider = crate::providers::OpenAIProvider::new("offline-fixture".into());
        assert!(ReasoningEffortMapper::resolve(&provider, "unknown-route", ReasoningEffortLevel::High, false).is_err());
    }

    #[test]
    fn known_structured_reasoning_route_does_not_inherit_generic_efforts() {
        let provider = crate::providers::MinimaxProvider::from_config(
            Some("offline-fixture".into()),
            Some("MiniMax-M3".into()),
            None,
            None,
            None,
            None,
            None,
        );

        assert!(provider.supports_reasoning("MiniMax-M3"));
        assert!(provider.supports_reasoning_effort("MiniMax-M3"));
        assert!(provider.supported_reasoning_efforts("MiniMax-M3").is_empty());
        assert!(
            ReasoningEffortMapper::resolve(&provider, "MiniMax-M3", ReasoningEffortLevel::High, false).is_err(),
            "structured reasoning without catalog effort levels must block configurable effort"
        );
    }

    #[test]
    fn catalog_presence_wins_over_provider_generic_fallback() {
        assert_eq!(crate::provider::catalog_reasoning_efforts("minimax", "MiniMax-M3"), Some(&[][..]));
        assert!(crate::provider::catalog_or_generic_reasoning_efforts("minimax", "MiniMax-M3").is_empty());
        assert!(crate::provider::catalog_or_explicit_reasoning_efforts("minimax", "MiniMax-M3", true).is_empty());
    }

    #[test]
    fn custom_anthropic_effort_capability_uses_generic_levels() {
        let mut model_behavior = vtcode_config::core::ModelConfig::default();
        model_behavior.model_supports_reasoning_effort = Some(true);
        let provider = crate::providers::AnthropicProvider::from_config(
            Some("offline-fixture".into()),
            Some("custom-anthropic-model".into()),
            None,
            None,
            None,
            None,
            Some(model_behavior),
        );
        assert_eq!(provider.supported_reasoning_efforts("custom-anthropic-model"), &["low", "medium", "high"]);
        assert!(
            ReasoningEffortMapper::resolve(&provider, "custom-anthropic-model", ReasoningEffortLevel::High, false)
                .is_ok()
        );
        assert!(
            ReasoningEffortMapper::resolve(&provider, "custom-anthropic-model", ReasoningEffortLevel::Max, false)
                .is_err()
        );
    }

    #[test]
    fn reasoning_matrix_never_silently_loses_fidelity() {
        for supported in [
            &["low", "medium", "high", "xhigh", "max"][..],
            &["low", "medium", "high", "max"][..],
            &["minimal", "low", "medium", "high"][..],
        ] {
            for requested in [
                ReasoningEffortLevel::None,
                ReasoningEffortLevel::Minimal,
                ReasoningEffortLevel::Low,
                ReasoningEffortLevel::Medium,
                ReasoningEffortLevel::High,
                ReasoningEffortLevel::XHigh,
                ReasoningEffortLevel::Max,
                ReasoningEffortLevel::Unknown,
            ] {
                let strict = ReasoningEffortMapper::map(requested, supported, false);
                assert_eq!(
                    strict.is_ok(),
                    requested == ReasoningEffortLevel::None || supported.contains(&requested.as_str())
                );
                if let Ok(mapping) = strict {
                    assert!(!mapping.degraded());
                }
            }
        }
        assert_eq!(
            ReasoningEffortMapper::map(ReasoningEffortLevel::Max, &["high", "xhigh"], true)
                .unwrap()
                .effective,
            ReasoningEffortLevel::XHigh
        );
        assert_eq!(
            ReasoningEffortMapper::map(ReasoningEffortLevel::Max, &["high"], true)
                .unwrap()
                .effective,
            ReasoningEffortLevel::High
        );
        assert!(ReasoningEffortMapper::map(ReasoningEffortLevel::Unknown, &["high"], true).is_err());
    }
}
