//! Provider extension methods that depend on vtcode-config model catalogs.
//!
//! The `Provider` enum and its core methods are defined in `vtcode-commons`.
//! This module adds vtcode-config-specific extension methods via the
//! [`ProviderModelSupport`] trait.

pub use vtcode_commons::provider::Provider;

use super::{ModelId, model_catalog_entry};
use std::str::FromStr;

const GENERIC_REASONING_EFFORTS: &[&str] = &["low", "medium", "high"];

/// Extension trait on `Provider` for model-specific capability queries.
///
/// These methods require vtcode-config model catalogs and constants,
/// so they cannot live in vtcode-commons with the core `Provider` type.
pub trait ProviderModelSupport: AsRef<str> {
    /// Determine if the provider/model exposes structured reasoning output.
    ///
    /// Catalog metadata is authoritative for curated models. Unknown routes
    /// retain the provider's legacy capability fallback so custom endpoints
    /// continue to work without a catalog entry.
    fn supports_reasoning(&self, model: &str) -> bool {
        model_catalog_entry(self.as_ref(), model)
            .map(|entry| entry.reasoning)
            .unwrap_or_else(|| self.supports_reasoning_effort(model))
    }

    /// Determine if the provider supports configurable reasoning effort for the model.
    fn supports_reasoning_effort(&self, model: &str) -> bool;

    /// Exact effort levels accepted by the provider/model route.
    fn supported_reasoning_efforts(&self, model: &str) -> &'static [&'static str] {
        if let Some(entry) = model_catalog_entry(self.as_ref(), model) {
            return entry.reasoning_efforts;
        }
        if self.supports_reasoning_effort(model) {
            GENERIC_REASONING_EFFORTS
        } else {
            &[]
        }
    }

    /// Determine if the provider supports the `service_tier` request parameter.
    fn supports_service_tier(&self, model: &str) -> bool;
}

impl ProviderModelSupport for Provider {
    fn supports_reasoning_effort(&self, model: &str) -> bool {
        if let Some(entry) = model_catalog_entry(self.as_ref(), model) {
            // Curated metadata is the single source of truth for known routes.
            // This keeps newly generated catalog entries from depending on a
            // second hand-maintained provider table and blocks effort payloads
            // when a model only exposes structured reasoning.
            return !entry.reasoning_efforts.is_empty();
        }

        use crate::constants::models;

        match self {
            Provider::Gemini => models::google::REASONING_MODELS.contains(&model),
            Provider::OpenAI => models::openai::REASONING_MODELS.contains(&model),
            Provider::Anthropic => models::anthropic::REASONING_MODELS.contains(&model),
            Provider::Copilot => false,
            Provider::DeepSeek => model == models::deepseek::DEEPSEEK_V4_PRO || model == "deepseek-reasoner",
            Provider::Meta => models::meta::REASONING_MODELS.contains(&model),
            Provider::OpenRouter => {
                if let Ok(model_id) = ModelId::from_str(model) {
                    if let Some(meta) = crate::models::openrouter_generated::metadata_for(model_id) {
                        return meta.reasoning;
                    }
                    return false;
                }
                models::openrouter::REASONING_MODELS.contains(&model)
            }
            Provider::Ollama => models::ollama::REASONING_LEVEL_MODELS.contains(&model),
            Provider::OllamaCloud => models::ollama::REASONING_LEVEL_MODELS.contains(&model),
            Provider::LmStudio => models::lmstudio::REASONING_MODELS.contains(&model),
            Provider::LlamaCpp => models::llamacpp::REASONING_MODELS.contains(&model),
            Provider::Moonshot => models::moonshot::REASONING_MODELS.contains(&model),
            Provider::ZAI => models::zai::REASONING_MODELS.contains(&model),
            Provider::Minimax => models::minimax::SUPPORTED_MODELS.contains(&model),
            Provider::MiMo => models::mimo::SUPPORTED_MODELS.contains(&model),
            Provider::Mistral => models::mistral::SUPPORTED_MODELS.contains(&model),
            Provider::HuggingFace => models::huggingface::REASONING_MODELS.contains(&model),
            Provider::OpenCodeZen => {
                if models::opencode_zen::OPENAI_MODELS.contains(&model) {
                    Provider::OpenAI.supports_reasoning_effort(model)
                } else if models::opencode_zen::ANTHROPIC_MODELS.contains(&model) {
                    Provider::Anthropic.supports_reasoning_effort(model)
                } else {
                    false
                }
            }
            Provider::OpenCodeGo => false,
            Provider::Qwen => models::qwen::REASONING_MODELS.contains(&model),
            Provider::StepFun => models::stepfun::REASONING_MODELS.contains(&model),
            Provider::Evolink => models::evolink::REASONING_MODELS.contains(&model),
            Provider::Poolside => false,
            Provider::Vercel => !models::vercel::NON_REASONING_MODELS.contains(&model),
            Provider::XAI => models::xai::REASONING_MODELS.contains(&model),
            Provider::NVIDIA => models::nvidia::REASONING_MODELS.contains(&model),
            Provider::MergeGateway => models::merge_gateway::route_supports_reasoning(model),
        }
    }

    fn supports_service_tier(&self, model: &str) -> bool {
        use crate::constants::models;

        match self {
            Provider::OpenAI => models::openai::SERVICE_TIER_MODELS.contains(&model),
            _ => false,
        }
    }
}
