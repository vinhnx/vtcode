use anyhow::{Context, Result};
use std::str::FromStr;

use crate::config::api_keys::{ApiKeySources, get_api_key_with_mode};
use crate::config::constants::model_helpers;
use crate::config::loader::VTCodeConfig;
use crate::config::models::{ModelId, Provider, model_catalog_entry};
use crate::config::types::AgentConfig as RuntimeAgentConfig;
use crate::llm::factory::{ProviderConfig, create_provider_with_config, infer_provider_from_model};
use crate::llm::provider::LLMProvider;
use vtcode_config::auth::AuthCredentialsStoreMode;

/// Features that may use a lightweight (cheaper, faster) model instead of the
/// primary model to reduce cost and latency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightweightFeature {
    /// Conversation memory summarization.
    Memory,
    /// Generating prompt suggestions for the user.
    PromptSuggestions,
    /// Refining user-provided prompts before execution.
    PromptRefinement,
    /// Reviewing auto-permission decisions.
    AutoPermissionReview,
    /// Probing auto-permission suitability.
    AutoPermissionProbe,
    /// Summarizing large file reads.
    LargeReadSummary,
    /// Summarizing web-fetched content.
    WebSummary,
    /// Summarizing git history.
    GitHistorySummary,
    /// Running as a subagent delegate.
    Subagent,
    /// Diagnosing a failed or non-zero tool execution from bounded evidence.
    ToolFailureDiagnosis,
}

/// A resolved provider-and-model pair that identifies a specific LLM endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRoute {
    /// Lowercase provider registry key (e.g. `"openai"`, `"anthropic"`).
    pub provider_name: String,
    /// Model identifier string (e.g. `"gpt-5"`, `"claude-4-sonnet"`).
    pub model: String,
}

/// Indicates how a lightweight route was resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightweightRouteSource {
    /// An explicit per-feature model override was provided.
    FeatureOverride,
    /// The shared `[agent.small_model]` config provided a model name.
    SharedConfigured,
    /// The shared small-model config is enabled but no model was named, so one
    /// was selected automatically.
    SharedAutomatic,
    /// No lightweight model applies; the main model is used directly.
    MainModel,
}

/// Result of resolving which model a lightweight feature should use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LightweightRouteResolution {
    /// The model route that should be used for the feature.
    pub primary: ModelRoute,
    /// The original main-model route, if the primary differs from it.
    pub fallback: Option<ModelRoute>,
    /// How this resolution was determined.
    pub source: LightweightRouteSource,
    /// Non-fatal warning generated during resolution (e.g. a cross-provider override).
    pub warning: Option<String>,
}

impl LightweightRouteResolution {
    /// Return `true` when a model other than the main model was selected.
    pub fn uses_lightweight_model(&self) -> bool {
        !matches!(self.source, LightweightRouteSource::MainModel)
    }

    /// Return a reference to the main-model fallback route, if one exists.
    pub fn fallback_to_main_model(&self) -> Option<&ModelRoute> {
        self.fallback.as_ref()
    }
}

/// Resolve the best model route for a given lightweight feature.
///
/// Checks, in priority order: explicit per-feature override, shared small-model
/// config, and automatic model selection for the active provider.
pub fn resolve_lightweight_route(
    runtime_config: &RuntimeAgentConfig,
    vt_cfg: Option<&VTCodeConfig>,
    feature: LightweightFeature,
    explicit_override_model: Option<&str>,
) -> LightweightRouteResolution {
    let main_route = main_model_route(runtime_config);
    let main_provider = main_route.provider_name.as_str();

    let mut warning = None;
    if let Some(configured_model) = explicit_override_model.map(str::trim).filter(|value| !value.is_empty()) {
        if let Some(route) = route_for_candidate(main_provider, configured_model, vt_cfg) {
            return LightweightRouteResolution {
                fallback: (route != main_route).then_some(main_route),
                primary: route,
                source: LightweightRouteSource::FeatureOverride,
                warning: None,
            };
        }

        warning = Some(format!(
            "ignored lightweight override model '{configured_model}' because it does not match the active provider '{main_provider}'"
        ));
    }

    let Some(vt_cfg) = vt_cfg else {
        return LightweightRouteResolution {
            primary: main_route,
            fallback: None,
            source: LightweightRouteSource::MainModel,
            warning,
        };
    };

    let shared_cfg = &vt_cfg.agent.small_model;
    if !shared_cfg.enabled || !feature_uses_shared_model(shared_cfg, feature) {
        return LightweightRouteResolution {
            primary: main_route,
            fallback: None,
            source: LightweightRouteSource::MainModel,
            warning,
        };
    }

    let configured_model = shared_cfg.model.trim();
    if !configured_model.is_empty() {
        if let Some(route) = route_for_candidate(main_provider, configured_model, Some(vt_cfg)) {
            return LightweightRouteResolution {
                fallback: (route != main_route).then_some(main_route),
                primary: route,
                source: LightweightRouteSource::SharedConfigured,
                warning,
            };
        }

        warning = Some(format!(
            "ignored lightweight model '{configured_model}' because it does not match the active provider '{main_provider}'"
        ));
    }

    let primary = ModelRoute {
        provider_name: main_route.provider_name.clone(),
        model: auto_lightweight_model(main_provider, &main_route.model),
    };
    LightweightRouteResolution {
        fallback: (primary != main_route).then_some(main_route),
        primary,
        source: LightweightRouteSource::SharedAutomatic,
        warning,
    }
}

/// Build a [`ModelRoute`] for the primary model from runtime configuration.
pub fn main_model_route(runtime_config: &RuntimeAgentConfig) -> ModelRoute {
    let provider_name = if runtime_config.provider.trim().is_empty() {
        infer_provider_from_model(&runtime_config.model)
            .map(|provider| provider.to_string().to_lowercase())
            .unwrap_or_else(|| "gemini".to_string())
    } else {
        runtime_config.provider.to_lowercase()
    };

    ModelRoute { provider_name, model: runtime_config.model.clone() }
}

/// Automatically select a lightweight model for the given provider and active model.
///
/// Prefers a same-generation efficient variant when available, then falls back
/// to provider defaults.
pub fn auto_lightweight_model(provider_name: &str, active_model: &str) -> String {
    let trimmed_model = active_model.trim();
    let Some(provider) = known_provider_from_name(provider_name).or_else(|| infer_provider_from_model(trimmed_model))
    else {
        // A custom endpoint without catalog metadata cannot safely receive a
        // built-in provider's fallback model. Keep its active model on the
        // route until the endpoint declares lightweight metadata.
        return trimmed_model.to_string();
    };

    if let Some(lightweight_model) = catalog_lightweight_model(provider, trimmed_model) {
        return lightweight_model;
    }

    if trimmed_model.is_empty() {
        return provider_default_lightweight_model(provider)
            .map(str::to_string)
            .unwrap_or_default();
    }

    // An unmapped model may be a user-managed endpoint. Never replace it with
    // an unrelated provider default just because the provider is known.
    trimmed_model.to_string()
}

/// Return the list of available lightweight model choices for the given provider.
///
/// The automatically selected model is always first in the list.
pub fn lightweight_model_choices(provider_name: &str, active_model: &str) -> Vec<String> {
    let provider = resolve_provider_for_model(provider_name, active_model);
    let auto_model = auto_lightweight_model(provider_name, active_model);
    let mut choices = Vec::new();

    if !auto_model.trim().is_empty() {
        choices.push(auto_model.clone());
    }
    if !active_model.trim().is_empty() {
        choices.push(active_model.trim().to_string());
    }

    if let Some(provider) = provider
        && let Some(models) = model_helpers::supported_for(provider.as_ref())
    {
        for model in models {
            let include = model
                .parse::<ModelId>()
                .map(|model_id| model_id.is_efficient_variant())
                .unwrap_or(false)
                || model.eq_ignore_ascii_case(active_model.trim());
            if include {
                choices.push((*model).to_string());
            }
        }
    }

    choices.sort();
    choices.dedup();
    if let Some(auto_index) = choices
        .iter()
        .position(|candidate| candidate.eq_ignore_ascii_case(auto_model.as_str()))
    {
        let auto = choices.remove(auto_index);
        choices.insert(0, auto);
    }
    choices
}

/// Instantiate an [`LLMProvider`] for the given model route.
///
/// Resolves the API key from the runtime config or environment and delegates
/// to the global factory.
pub fn create_provider_for_model_route(
    route: &ModelRoute,
    runtime_config: &RuntimeAgentConfig,
    vt_cfg: Option<&VTCodeConfig>,
) -> Result<Box<dyn LLMProvider>> {
    let storage_mode = vt_cfg.map(|cfg| cfg.agent.credential_storage_mode).unwrap_or_default();
    let api_key = resolve_api_key_for_model_route_with_mode(route, runtime_config, storage_mode);
    create_provider_with_config(
        &route.provider_name,
        ProviderConfig {
            api_key,
            openai_chatgpt_auth: runtime_config.openai_chatgpt_auth.clone(),
            copilot_auth: vt_cfg.map(|cfg| cfg.auth.copilot.clone()),
            base_url: None,
            model: Some(route.model.clone()),
            prompt_cache: Some(runtime_config.prompt_cache.clone()),
            timeouts: None,
            openai: vt_cfg.map(|cfg| cfg.provider.openai.clone()),
            anthropic: vt_cfg.map(|cfg| cfg.provider.anthropic.clone()),
            model_behavior: runtime_config.model_behavior.clone(),
            workspace_root: Some(runtime_config.workspace.clone()),
        },
    )
    .with_context(|| {
        format!("Failed to initialize lightweight provider '{}' for model '{}'", route.provider_name, route.model)
    })
}

/// Resolve the API key for a model route, using the runtime key when the route
/// targets the same provider as the main model, or falling back to environment
/// variables otherwise.
pub fn resolve_api_key_for_model_route(route: &ModelRoute, runtime_config: &RuntimeAgentConfig) -> Option<String> {
    resolve_api_key_for_model_route_with_mode(route, runtime_config, AuthCredentialsStoreMode::default())
}

/// Resolve a lightweight route key using the configured secure-storage mode.
pub fn resolve_api_key_for_model_route_with_mode(
    route: &ModelRoute,
    runtime_config: &RuntimeAgentConfig,
    storage_mode: AuthCredentialsStoreMode,
) -> Option<String> {
    if route
        .provider_name
        .eq_ignore_ascii_case(main_model_route(runtime_config).provider_name.as_str())
        && !runtime_config.api_key.trim().is_empty()
    {
        return Some(runtime_config.api_key.clone());
    }

    get_api_key_with_mode(&route.provider_name, &ApiKeySources::default(), storage_mode).ok()
}

fn feature_uses_shared_model(
    shared_cfg: &vtcode_config::core::agent::AgentSmallModelConfig,
    feature: LightweightFeature,
) -> bool {
    match feature {
        LightweightFeature::Memory => shared_cfg.use_for_memory,
        LightweightFeature::LargeReadSummary => shared_cfg.use_for_large_reads,
        LightweightFeature::WebSummary => shared_cfg.use_for_web_summary,
        LightweightFeature::GitHistorySummary => shared_cfg.use_for_git_history,
        LightweightFeature::PromptSuggestions
        | LightweightFeature::PromptRefinement
        | LightweightFeature::AutoPermissionReview
        | LightweightFeature::AutoPermissionProbe
        | LightweightFeature::Subagent
        | LightweightFeature::ToolFailureDiagnosis => true,
    }
}

fn route_for_candidate(
    main_provider: &str,
    candidate_model: &str,
    vt_cfg: Option<&VTCodeConfig>,
) -> Option<ModelRoute> {
    let is_declared_custom_model = vt_cfg
        .and_then(|cfg| cfg.custom_provider(main_provider))
        .is_some_and(|provider| provider.effective_models().iter().any(|model| model == candidate_model));

    if infer_provider_from_model(candidate_model)
        .map(|provider| !provider.as_ref().eq_ignore_ascii_case(main_provider))
        .unwrap_or(false)
        && !is_declared_custom_model
    {
        return None;
    }

    Some(ModelRoute {
        provider_name: main_provider.to_string(),
        model: candidate_model.to_string(),
    })
}

fn resolve_provider_for_model(provider_name: &str, active_model: &str) -> Option<Provider> {
    known_provider_from_name(provider_name).or_else(|| infer_provider_from_model(active_model))
}

fn known_provider_from_name(provider_name: &str) -> Option<Provider> {
    Provider::from_str(provider_name.trim()).ok()
}

fn provider_default_lightweight_model(provider: Provider) -> Option<&'static str> {
    let provider_name = provider.as_ref();
    let default_model = model_helpers::default_for(provider_name)?;
    let catalog_lightweight = model_catalog_entry(provider_name, default_model)
        .and_then(|entry| entry.lightweight_model)
        .filter(|model| !model.trim().is_empty());
    Some(catalog_lightweight.unwrap_or(default_model))
}

/// Resolve the catalog-backed lightweight sibling for a provider/model pair.
///
/// `ModelId` and generated catalog entries are two views of the same catalog;
/// keeping their precedence in one helper prevents the automatic route and
/// provider-default route from drifting apart.
fn catalog_lightweight_model(provider: Provider, active_model: &str) -> Option<String> {
    if let Some(entry) = model_catalog_entry(provider.as_ref(), active_model)
        && let Some(target) = entry.lightweight_model
        && !target.trim().is_empty()
    {
        return Some(normalize_catalog_target(provider, target));
    }

    if let Ok(model_id) = active_model.parse::<ModelId>() {
        if model_id.provider() != provider {
            return None;
        }
        if model_id.is_efficient_variant() {
            return Some(model_id.as_str().to_string());
        }
        if let Some(lightweight_model) = model_id.preferred_lightweight_variant() {
            return Some(lightweight_model.as_str().to_string());
        }
    }

    None
}

fn normalize_catalog_target(provider: Provider, target: &str) -> String {
    let target = target.trim();
    if provider == Provider::Evolink && !target.contains('/') {
        return format!("evolink/{target}");
    }
    target.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime_config() -> RuntimeAgentConfig {
        RuntimeAgentConfig {
            model: ModelId::GPT56Sol.as_str().to_string(),
            api_key: "test-key".to_string(),
            provider: "openai".to_string(),
            openai_chatgpt_auth: None,
            api_key_env: "OPENAI_API_KEY".to_string(),
            workspace: std::env::temp_dir().join("vtcode-lightweight-routing-tests"),
            verbose: false,
            quiet: false,
            theme: "default".to_string(),
            reasoning_effort: Default::default(),
            ui_surface: Default::default(),
            prompt_cache: Default::default(),
            model_source: Default::default(),
            custom_api_keys: Default::default(),
            checkpointing_enabled: false,
            checkpointing_storage_dir: None,
            checkpointing_max_snapshots: 0,
            checkpointing_max_age_days: None,
            max_conversation_turns: 0,
            model_behavior: None,
        }
    }

    #[test]
    fn explicit_override_uses_active_provider() {
        let runtime = runtime_config();
        let route = resolve_lightweight_route(
            &runtime,
            Some(&VTCodeConfig::default()),
            LightweightFeature::Memory,
            Some("gpt-5-mini"),
        );

        assert_eq!(route.primary.provider_name, "openai");
        assert_eq!(route.primary.model, "gpt-5-mini");
        assert_eq!(route.source, LightweightRouteSource::FeatureOverride);
    }

    #[test]
    fn cross_provider_shared_model_falls_back_to_auto_same_provider() {
        let runtime = runtime_config();
        let mut vt_cfg = VTCodeConfig::default();
        vt_cfg.agent.small_model.model = "claude-4-5-haiku".to_string();

        let route = resolve_lightweight_route(&runtime, Some(&vt_cfg), LightweightFeature::PromptSuggestions, None);

        assert_eq!(route.primary.provider_name, "openai");
        assert_eq!(route.primary.model, ModelId::GPT56Terra.as_str());
        assert_eq!(route.source, LightweightRouteSource::SharedAutomatic);
        assert!(route.warning.is_some());
    }

    #[test]
    fn explicit_override_accepts_declared_custom_provider_model_that_infers_builtin_provider() {
        // Arrange: the custom provider explicitly declares a GPT-slugged model.
        let mut runtime = runtime_config();
        runtime.provider = "my-gateway".to_string();
        let mut vt_cfg = VTCodeConfig::default();
        vt_cfg.custom_providers.push(vtcode_config::core::CustomProviderConfig {
            name: "my-gateway".to_string(),
            models: vec![" gpt-5.6-luna ".to_string()],
            ..Default::default()
        });

        // Act: resolve an explicitly overridden model with surrounding whitespace.
        let route = resolve_lightweight_route(
            &runtime,
            Some(&vt_cfg),
            LightweightFeature::PromptSuggestions,
            Some("  gpt-5.6-luna  "),
        );

        // Assert: declaration wins over built-in provider inference and is normalized.
        assert_eq!(
            route.primary,
            ModelRoute {
                provider_name: "my-gateway".to_string(),
                model: "gpt-5.6-luna".to_string(),
            }
        );
        assert_eq!(route.source, LightweightRouteSource::FeatureOverride);
        assert!(route.warning.is_none());
    }

    #[test]
    fn shared_small_model_accepts_declared_custom_provider_model_that_infers_builtin_provider() {
        // Arrange: shared small-model config selects a model declared by the active custom provider.
        let mut runtime = runtime_config();
        runtime.provider = "my-gateway".to_string();
        let mut vt_cfg = VTCodeConfig::default();
        vt_cfg.agent.small_model.model = "  gpt-5.6-luna  ".to_string();
        vt_cfg.custom_providers.push(vtcode_config::core::CustomProviderConfig {
            name: "my-gateway".to_string(),
            models: vec!["gpt-5.6-luna".to_string()],
            ..Default::default()
        });

        // Act: resolve the shared configured lightweight model.
        let route = resolve_lightweight_route(&runtime, Some(&vt_cfg), LightweightFeature::PromptSuggestions, None);

        // Assert: the declared model is selected through the active custom provider.
        assert_eq!(
            route.primary,
            ModelRoute {
                provider_name: "my-gateway".to_string(),
                model: "gpt-5.6-luna".to_string(),
            }
        );
        assert_eq!(route.source, LightweightRouteSource::SharedConfigured);
        assert!(route.warning.is_none());
    }

    #[test]
    fn auto_lightweight_model_prefers_same_generation_openai_sibling() {
        assert_eq!(auto_lightweight_model("openai", &ModelId::GPT56Sol.as_str()), ModelId::GPT56Terra.as_str());
    }

    #[test]
    fn auto_lightweight_model_uses_catalog_anthropic_pair() {
        assert_eq!(
            auto_lightweight_model("anthropic", &ModelId::ClaudeSonnet5.as_str()),
            ModelId::ClaudeSonnet5.as_str()
        );
        assert_eq!(auto_lightweight_model("anthropic", "claude-sonnet-4.5"), "claude-sonnet-4.5");
    }

    #[test]
    fn auto_lightweight_model_uses_lower_generation_glm_pair() {
        assert_eq!(auto_lightweight_model("zai", &ModelId::ZaiGlm52.as_str()), ModelId::ZaiGlm52.as_str());
    }

    #[test]
    fn auto_lightweight_model_prefers_same_generation_gemini_flash_lite() {
        assert_eq!(auto_lightweight_model("gemini", &ModelId::Gemini37Flash.as_str()), ModelId::Gemini37Flash.as_str());
    }

    #[test]
    fn auto_lightweight_model_infers_family_for_custom_provider() {
        assert_eq!(auto_lightweight_model("mycorp", &ModelId::GPT56Sol.as_str()), ModelId::GPT56Terra.as_str());
    }

    #[test]
    fn auto_lightweight_model_keeps_unmapped_custom_route() {
        assert_eq!(auto_lightweight_model("mycorp", "custom-model-v1"), "custom-model-v1");
    }

    #[test]
    fn auto_lightweight_model_keeps_unmapped_model_for_known_provider() {
        assert_eq!(auto_lightweight_model("openai", "custom-model-v1"), "custom-model-v1");
    }

    #[test]
    fn auto_lightweight_model_keeps_unmapped_evolink_route() {
        assert_eq!(auto_lightweight_model("evolink", "evolink/deepseek-v4-pro"), "evolink/deepseek-v4-pro");
    }

    #[test]
    fn lightweight_model_choices_do_not_invent_provider_for_unknown_route() {
        assert_eq!(lightweight_model_choices("mycorp", "custom-model-v1"), vec!["custom-model-v1"]);
    }

    #[test]
    fn disabled_feature_uses_main_model() {
        let runtime = runtime_config();
        let mut vt_cfg = VTCodeConfig::default();
        vt_cfg.agent.small_model.use_for_memory = false;

        let route = resolve_lightweight_route(&runtime, Some(&vt_cfg), LightweightFeature::Memory, None);

        assert_eq!(route.primary.model, ModelId::GPT56Sol.as_str());
        assert_eq!(route.source, LightweightRouteSource::MainModel);
        assert!(route.fallback.is_none());
    }
}
