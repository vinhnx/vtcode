use anyhow::{Result, anyhow};
use std::str::FromStr;

use vtcode_config::MiMoAuthMethod;
use vtcode_config::OpenAIServiceTier;
use vtcode_config::VTCodeConfig;
use vtcode_config::api_keys::CredentialSource;
use vtcode_config::auth::AuthCredentialsStoreMode;
use vtcode_config::core::CustomProviderConfig;
use vtcode_core::config::constants::reasoning;
use vtcode_core::config::models::{ModelId, Provider};
use vtcode_core::config::types::ReasoningEffortLevel;
use vtcode_core::llm::{DynamicModelMeta, ModelAvailability, ModelResolver, ResolvedModel};

use super::options::{ModelOption, find_option_index};

#[derive(Clone, Debug)]
pub(super) struct SelectionDetail {
    pub(super) provider_key: String,
    pub(super) provider_label: String,
    pub(super) provider_enum: Option<Provider>,
    pub(super) model_id: String,
    pub(super) model_display: String,
    pub(super) known_model: bool,
    pub(super) context_window: Option<usize>,
    pub(super) reasoning_supported: bool,
    pub(super) reasoning_effort_supported: bool,
    pub(super) reasoning_optional: bool,
    pub(super) reasoning_off_model: Option<ModelId>,
    pub(super) service_tier_supported: bool,
    pub(super) requires_api_key: bool,
    pub(super) uses_chatgpt_auth: bool,
    pub(super) env_key: String,
    pub(super) mimo_auth_method: Option<MiMoAuthMethod>,
}

#[derive(Clone, Copy)]
pub(super) enum ReasoningChoice {
    Level(ReasoningEffortLevel),
    Disable,
}

#[derive(Clone, Copy)]
pub(super) enum ServiceTierChoice {
    ProjectDefault,
    Flex,
    Priority,
}

#[derive(Clone)]
pub(crate) struct ModelSelectionResult {
    pub(crate) provider: String,
    pub(crate) provider_label: String,
    pub(crate) provider_enum: Option<Provider>,
    pub(crate) model: String,
    pub(crate) model_display: String,
    pub(crate) known_model: bool,
    pub(crate) context_window: Option<usize>,
    pub(crate) reasoning_supported: bool,
    pub(crate) reasoning: ReasoningEffortLevel,
    pub(crate) reasoning_changed: bool,
    pub(crate) service_tier_supported: bool,
    pub(crate) service_tier: Option<OpenAIServiceTier>,
    pub(crate) service_tier_changed: bool,
    pub(crate) api_key: Option<String>,
    /// Source of a credential discovered by the central resolver. `None`
    /// means the user entered the key during this picker session.
    pub(crate) credential_source: Option<CredentialSource>,
    pub(crate) env_key: String,
    pub(crate) requires_api_key: bool,
    pub(crate) uses_chatgpt_auth: bool,
    pub(crate) mimo_auth_method: Option<MiMoAuthMethod>,
}

pub(super) fn parse_model_selection(
    options: &[ModelOption],
    input: &str,
    vt_cfg: Option<&VTCodeConfig>,
) -> Result<SelectionDetail> {
    let trimmed = input.trim();
    if !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_digit()) {
        if let Some(option) = options.iter().find(|option| option.id.eq_ignore_ascii_case(trimmed)) {
            return Ok(selection_from_option_with_mode(option, storage_mode(vt_cfg)));
        }
        if let Ok(index) = trimmed.parse::<usize>() {
            if let Some(option) = options.get(index) {
                return Ok(selection_from_option_with_mode(option, storage_mode(vt_cfg)));
            }
        }
        return Err(anyhow!("Invalid model selection. Use provider and model name (e.g., 'openai gpt-5')"));
    }

    let mut parts = input.split_whitespace();
    let Some(provider_token) = parts.next() else {
        return Err(anyhow!("Please provide a provider and model identifier."));
    };
    let model_token = parts.collect::<Vec<&str>>().join(" ");
    if model_token.trim().is_empty() {
        return Err(anyhow!("Provide both provider and model. Example: 'openai gpt-5'"));
    }

    let provider_lower = provider_token.to_ascii_lowercase();
    let provider_enum = Provider::from_str(&provider_lower).ok();
    let custom_provider = vt_cfg.and_then(|cfg| cfg.custom_provider(&provider_lower));

    if let Some(provider) = provider_enum
        && let Some(option_index) = find_option_index(provider, model_token.trim(), options)
        && let Some(option) = options.get(option_index)
    {
        return Ok(selection_from_option_with_mode(option, storage_mode(vt_cfg)));
    }

    let uses_command_auth = custom_provider.is_some_and(|provider| provider.uses_command_auth());
    let provider_label = custom_provider
        .map(|provider| provider.display_name.clone())
        .or_else(|| provider_enum.map(|provider| provider.label().to_string()))
        .unwrap_or_else(|| title_case(&provider_lower));
    let env_key = custom_provider
        .map(|provider| {
            if provider.uses_command_auth() {
                String::new()
            } else {
                provider.resolved_api_key_env()
            }
        })
        .or_else(|| {
            provider_enum.map(|provider| {
                vt_cfg
                    .and_then(|cfg| cfg.provider_overrides.get(provider.as_ref()))
                    .and_then(|override_config| override_config.api_key_env.as_deref())
                    .filter(|configured| !configured.trim().is_empty())
                    .unwrap_or(provider.default_api_key_env())
                    .to_string()
            })
        })
        .unwrap_or_else(|| derive_env_key(&provider_lower));
    if custom_provider.is_some() && provider_enum.is_none() {
        let profile = custom_provider.map(|provider| provider.resolved_profile(model_token.trim()));
        return Ok(SelectionDetail {
            provider_key: provider_lower,
            provider_label,
            provider_enum: None,
            model_id: model_token.trim().to_string(),
            model_display: model_token.trim().to_string(),
            known_model: false,
            context_window: profile.as_ref().and_then(|profile| profile.context_window),
            reasoning_supported: profile.as_ref().and_then(|profile| profile.supports_reasoning).unwrap_or(false),
            reasoning_effort_supported: profile
                .as_ref()
                .and_then(|profile| profile.supports_reasoning_effort)
                .unwrap_or(false),
            reasoning_optional: true,
            reasoning_off_model: None,
            service_tier_supported: false,
            requires_api_key: !uses_command_auth,
            uses_chatgpt_auth: false,
            env_key,
            mimo_auth_method: None,
        });
    }
    if let Some(provider) = provider_enum {
        let storage_mode = vt_cfg.map(|cfg| cfg.agent.credential_storage_mode).unwrap_or_default();
        let resolved = ModelResolver::resolve_with_mode_and_api_key_env(
            Some(provider.as_ref()),
            model_token.trim(),
            &[],
            None,
            Some(&env_key),
            storage_mode,
        )
        .ok_or_else(|| {
            anyhow::anyhow!("unable to resolve model `{}` for provider `{}`", model_token.trim(), provider.as_ref())
        })?;
        return Ok(selection_from_resolved(
            provider_lower,
            provider_label,
            Some(provider),
            resolved,
            true,
            None,
            env_key,
        ));
    }

    Ok(SelectionDetail {
        provider_key: provider_lower,
        provider_label,
        provider_enum,
        model_id: model_token.trim().to_string(),
        model_display: model_token.trim().to_string(),
        known_model: false,
        context_window: None,
        reasoning_supported: false,
        reasoning_effort_supported: false,
        reasoning_optional: true,
        reasoning_off_model: None,
        service_tier_supported: false,
        requires_api_key: true,
        uses_chatgpt_auth: false,
        env_key,
        mimo_auth_method: None,
    })
}

pub(super) fn selection_from_option(option: &ModelOption) -> SelectionDetail {
    selection_from_option_with_mode(option, AuthCredentialsStoreMode::default())
}

pub(super) fn selection_from_option_with_mode(
    option: &ModelOption,
    storage_mode: AuthCredentialsStoreMode,
) -> SelectionDetail {
    let resolved = ModelResolver::resolve_with_mode_and_api_key_env(
        Some(option.provider.as_ref()),
        &option.id,
        &[],
        None,
        Some(&option.api_key_env),
        storage_mode,
    )
    .unwrap_or_else(|| {
        // Fallback: create a minimal ResolvedModel for static options.
        // Use MissingCredential so the picker prompts for an API key instead of
        // silently skipping credential steps for an unresolved model.
        ResolvedModel {
            provider: option.provider,
            model_id: option.id.clone(),
            api_key_env: option.api_key_env.clone(),
            catalog: None,
            dynamic: None,
            availability: ModelAvailability::MissingCredential,
        }
    });
    selection_from_resolved(
        option.provider.to_string(),
        option.provider.label().to_string(),
        Some(option.provider),
        resolved,
        false,
        option.reasoning_alternative.clone(),
        option.api_key_env.clone(),
    )
}

#[cfg(test)]
#[allow(dead_code, reason = "Intentional compatibility, platform, or test-only suppression.")]
pub(super) fn selection_from_dynamic(
    provider: Provider,
    model_id: &str,
    display_name: &str,
    description: Option<&str>,
    context_window: Option<usize>,
) -> SelectionDetail {
    selection_from_dynamic_with_mode(
        provider,
        model_id,
        display_name,
        description,
        context_window,
        AuthCredentialsStoreMode::default(),
    )
}

#[cfg(test)]
fn selection_from_dynamic_with_mode(
    provider: Provider,
    model_id: &str,
    display_name: &str,
    description: Option<&str>,
    context_window: Option<usize>,
    storage_mode: AuthCredentialsStoreMode,
) -> SelectionDetail {
    selection_from_dynamic_with_api_key_env(
        provider,
        model_id,
        display_name,
        description,
        context_window,
        None,
        storage_mode,
    )
}

pub(super) fn selection_from_dynamic_with_api_key_env(
    provider: Provider,
    model_id: &str,
    display_name: &str,
    description: Option<&str>,
    context_window: Option<usize>,
    api_key_env: Option<&str>,
    storage_mode: AuthCredentialsStoreMode,
) -> SelectionDetail {
    let env_key = api_key_env
        .map(str::trim)
        .filter(|env_key| !env_key.is_empty())
        .unwrap_or_else(|| provider.default_api_key_env())
        .to_string();
    let resolved = ModelResolver::resolve_with_mode_and_api_key_env(
        Some(provider.as_ref()),
        model_id,
        &[vtcode_core::llm::DynamicModelRef { provider, model_id }],
        Some(DynamicModelMeta {
            display_name: display_name.to_string(),
            description: description.map(ToOwned::to_owned),
            context_window,
        }),
        Some(&env_key),
        storage_mode,
    )
    .unwrap_or_else(|| {
        // Fallback: create a minimal ResolvedModel for dynamic models.
        // Use MissingCredential so the picker prompts for an API key instead of
        // silently skipping credential steps for an unresolved model.
        ResolvedModel {
            provider,
            model_id: model_id.to_string(),
            api_key_env: env_key.clone(),
            catalog: None,
            dynamic: Some(DynamicModelMeta {
                display_name: display_name.to_string(),
                description: description.map(ToOwned::to_owned),
                context_window,
            }),
            availability: ModelAvailability::MissingCredential,
        }
    });
    selection_from_resolved(
        provider.to_string(),
        provider.label().to_string(),
        Some(provider),
        resolved,
        true,
        None,
        env_key,
    )
}

fn storage_mode(vt_cfg: Option<&VTCodeConfig>) -> AuthCredentialsStoreMode {
    vt_cfg.map(|cfg| cfg.agent.credential_storage_mode).unwrap_or_default()
}

pub(super) fn selections_from_custom_provider(provider: &CustomProviderConfig) -> Vec<SelectionDetail> {
    let env_key = if provider.uses_command_auth() {
        String::new()
    } else {
        provider.resolved_api_key_env()
    };

    provider
        .effective_models()
        .into_iter()
        .map(|model_id| SelectionDetail {
            provider_key: provider.name.to_lowercase(),
            provider_label: provider.display_name.clone(),
            provider_enum: None,
            model_id: model_id.clone(),
            model_display: model_id.clone(),
            known_model: false,
            context_window: provider.resolved_profile(&model_id).context_window,
            reasoning_supported: provider.resolved_profile(&model_id).supports_reasoning.unwrap_or(false),
            reasoning_effort_supported: provider.resolved_profile(&model_id).supports_reasoning_effort.unwrap_or(false),
            reasoning_optional: true,
            reasoning_off_model: None,
            service_tier_supported: false,
            requires_api_key: !provider.uses_command_auth(),
            uses_chatgpt_auth: false,
            env_key: env_key.clone(),
            mimo_auth_method: None,
        })
        .collect()
}

fn selection_from_resolved(
    provider_key: String,
    provider_label: String,
    provider_enum: Option<Provider>,
    resolved: ResolvedModel,
    reasoning_optional: bool,
    reasoning_off_model: Option<ModelId>,
    env_key: String,
) -> SelectionDetail {
    SelectionDetail {
        provider_key,
        provider_label,
        provider_enum,
        model_id: resolved.model_id.clone(),
        model_display: resolved.display_name().into_owned(),
        known_model: resolved.known_model(),
        context_window: resolved.context_window(),
        reasoning_supported: resolved.reasoning_supported(),
        reasoning_effort_supported: resolved.reasoning_effort_supported(),
        reasoning_optional,
        reasoning_off_model,
        service_tier_supported: resolved.service_tier_supported(),
        requires_api_key: resolved.availability.requires_api_key(),
        uses_chatgpt_auth: resolved.availability.uses_managed_auth(),
        env_key,
        mimo_auth_method: None,
    }
}

impl SelectionDetail {
    /// Return effort levels accepted by the selected route.
    ///
    /// Built-in and dynamic providers resolve through the shared model
    /// catalog. Custom providers retain the documented generic effort set
    /// when their profile explicitly enables configurable reasoning.
    pub(super) fn reasoning_effort_levels(&self) -> Vec<ReasoningEffortLevel> {
        let mut levels = Vec::new();
        if supports_gpt5_none_reasoning(&self.model_id) {
            levels.push(ReasoningEffortLevel::None);
        }

        if let Some(provider) = self.provider_enum {
            if let Some(resolved) = ModelResolver::resolve(Some(provider.as_ref()), &self.model_id, &[], None) {
                levels.extend(
                    resolved
                        .supported_reasoning_efforts()
                        .iter()
                        .filter_map(|level| ReasoningEffortLevel::parse(level)),
                );
            }
        } else if self.reasoning_effort_supported {
            levels.extend([
                ReasoningEffortLevel::Low,
                ReasoningEffortLevel::Medium,
                ReasoningEffortLevel::High,
            ]);
        }

        levels.sort_unstable_by_key(|level| match level {
            ReasoningEffortLevel::None => 0,
            ReasoningEffortLevel::Minimal => 1,
            ReasoningEffortLevel::Low => 2,
            ReasoningEffortLevel::Medium => 3,
            ReasoningEffortLevel::High => 4,
            ReasoningEffortLevel::XHigh => 5,
            ReasoningEffortLevel::Max => 6,
            ReasoningEffortLevel::Unknown => usize::MAX,
        });
        levels.dedup();
        levels
    }
}

pub(super) fn reasoning_level_label(level: ReasoningEffortLevel) -> &'static str {
    match level {
        ReasoningEffortLevel::None => "None (Fast)",
        ReasoningEffortLevel::Unknown => "Unknown",
        ReasoningEffortLevel::Minimal => "Minimal (Fastest)",
        ReasoningEffortLevel::Low => reasoning::LABEL_LOW,
        ReasoningEffortLevel::Medium => reasoning::LABEL_MEDIUM,
        ReasoningEffortLevel::High => reasoning::LABEL_HIGH,
        ReasoningEffortLevel::XHigh => "Extra High",
        ReasoningEffortLevel::Max => "Max",
    }
}

pub(super) fn supports_gpt5_none_reasoning(model_id: &str) -> bool {
    matches!(model_id, "gpt-5.6" | "gpt-5.6-sol" | "gpt-5.5-2026-04-23" | "gpt-5.6-terra" | "gpt-5.6-luna")
        || matches!(model_id, "gpt-5.2-codex" | "gpt-5-codex")
}

#[cfg(test)]
fn catalog_supports_reasoning_effort(model_id: &str, effort: &str) -> bool {
    ModelResolver::resolve(None, model_id, &[], None)
        .is_some_and(|resolved| resolved.supported_reasoning_efforts().contains(&effort))
}

#[cfg(test)]
pub(super) fn supports_xhigh_reasoning(model_id: &str) -> bool {
    catalog_supports_reasoning_effort(model_id, "xhigh")
}

#[cfg(test)]
pub(super) fn supports_max_reasoning(model_id: &str) -> bool {
    catalog_supports_reasoning_effort(model_id, "max")
}

pub(super) fn reasoning_level_description(level: ReasoningEffortLevel) -> &'static str {
    match level {
        ReasoningEffortLevel::None => "No reasoning overhead - fastest responses",
        ReasoningEffortLevel::Unknown => "Reasoning capability could not be determined",
        ReasoningEffortLevel::Minimal => "Minimal reasoning overhead - very fast responses",
        ReasoningEffortLevel::Low => reasoning::DESCRIPTION_LOW,
        ReasoningEffortLevel::Medium => reasoning::DESCRIPTION_MEDIUM,
        ReasoningEffortLevel::High => reasoning::DESCRIPTION_HIGH,
        ReasoningEffortLevel::XHigh => "Hardest long-running tasks; high cost and latency",
        ReasoningEffortLevel::Max => "Uncapped maximum reasoning; highest cost and latency",
    }
}

pub(super) fn service_tier_label(service_tier: Option<OpenAIServiceTier>) -> &'static str {
    match service_tier {
        Some(OpenAIServiceTier::Flex) => "Flex",
        Some(OpenAIServiceTier::Priority) => "Priority",
        None => "Project default",
    }
}

pub(super) fn is_cancel_command(input: &str) -> bool {
    input.eq_ignore_ascii_case("cancel")
        || input.eq_ignore_ascii_case("/cancel")
        || input.eq_ignore_ascii_case("abort")
        || input.eq_ignore_ascii_case("quit")
}

pub(super) fn derive_env_key(provider: &str) -> String {
    let mut key = String::new();
    for ch in provider.chars() {
        if ch.is_ascii_alphanumeric() {
            key.push(ch.to_ascii_uppercase());
        } else {
            key.push('_');
        }
    }
    key = key.trim_end_matches('_').to_string();
    if !key.ends_with("_API_KEY") {
        if !key.is_empty() && !key.ends_with('_') {
            key.push('_');
        }
        key.push_str("API_KEY");
    }
    key
}

pub(super) fn title_case(value: &str) -> String {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut result = String::new();
    result.push(first.to_ascii_uppercase());
    result.push_str(&chars.as_str().to_ascii_lowercase());
    result
}

#[cfg(test)]
mod tests {
    use vtcode_core::config::models::Provider;
    use vtcode_core::llm::{ModelAvailability, ModelResolver};

    #[test]
    fn managed_auth_provider_skips_api_key_requirement() {
        assert_eq!(ModelResolver::availability(Provider::Copilot, "copilot"), ModelAvailability::ManagedAuthAvailable);
    }
}
