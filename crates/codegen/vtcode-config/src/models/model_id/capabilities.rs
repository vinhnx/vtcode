use crate::models::{Provider, ProviderModelSupport};

use super::ModelId;

#[allow(dead_code, reason = "Intentional compatibility, platform, or test-only suppression.")]
mod capability_generated {
    include!(concat!(env!("OUT_DIR"), "/model_capabilities.rs"));
}

/// Catalog metadata generated from `docs/models.json`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelPricing {
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelCatalogEntry {
    pub(crate) provider: &'static str,
    id: &'static str,
    pub display_name: &'static str,
    pub description: &'static str,
    pub context_window: usize,
    max_output_tokens: Option<usize>,
    pub reasoning: bool,
    pub reasoning_efforts: &'static [&'static str],
    pub is_pro: bool,
    pub lightweight_model: Option<&'static str>,
    pub tool_call: bool,
    pub vision: bool,
    pub input_modalities: &'static [&'static str],
    pub caching: bool,
    pub structured_output: bool,
    pub supports_sampling: bool,
    pub supports_logprobs: bool,
    pub prompt_cache_ttl: Option<&'static str>,
    pub prompt_contract: Option<&'static str>,
    pub pricing: ModelPricing,
}

fn catalog_provider_key(provider: &str) -> &str {
    if provider.eq_ignore_ascii_case("google") || provider.eq_ignore_ascii_case("gemini") {
        "gemini"
    } else if provider.eq_ignore_ascii_case("openai") {
        "openai"
    } else if provider.eq_ignore_ascii_case("anthropic") {
        "anthropic"
    } else if provider.eq_ignore_ascii_case("deepseek") {
        "deepseek"
    } else if provider.eq_ignore_ascii_case("meta") || provider.eq_ignore_ascii_case("meta-ai") {
        "meta"
    } else if provider.eq_ignore_ascii_case("openrouter") {
        "openrouter"
    } else if provider.eq_ignore_ascii_case("ollama") {
        "ollama"
    } else if provider.eq_ignore_ascii_case("lmstudio") {
        "lmstudio"
    } else if provider.eq_ignore_ascii_case("llamacpp") || provider.eq_ignore_ascii_case("llama.cpp") {
        "llamacpp"
    } else if provider.eq_ignore_ascii_case("moonshot") {
        "moonshot"
    } else if provider.eq_ignore_ascii_case("zai") {
        "zai"
    } else if provider.eq_ignore_ascii_case("minimax") {
        "minimax"
    } else if provider.eq_ignore_ascii_case("huggingface") {
        "huggingface"
    } else if provider.eq_ignore_ascii_case("stepfun") {
        "stepfun"
    } else if provider.eq_ignore_ascii_case("evolink") {
        "evolink"
    } else if provider.eq_ignore_ascii_case("poolside") {
        "poolside"
    } else if provider.eq_ignore_ascii_case("xai") {
        "xai"
    } else if provider.eq_ignore_ascii_case("nvidia") {
        "nvidia"
    } else if provider.eq_ignore_ascii_case("merge-gateway") {
        "merge-gateway"
    } else if provider.eq_ignore_ascii_case("vercel") || provider.eq_ignore_ascii_case("vercel-ai-gateway") {
        "vercel"
    } else {
        provider
    }
}

fn capability_provider_key(provider: Provider) -> &'static str {
    match provider {
        Provider::Gemini => "gemini",
        Provider::OpenAI => "openai",
        Provider::Anthropic => "anthropic",
        Provider::Copilot => "copilot",
        Provider::DeepSeek => "deepseek",
        Provider::Meta => "meta",
        Provider::OpenRouter => "openrouter",
        Provider::Ollama => "ollama",
        Provider::OllamaCloud => "ollama-cloud",
        Provider::LmStudio => "lmstudio",
        Provider::LlamaCpp => "llamacpp",
        Provider::Moonshot => "moonshot",
        Provider::ZAI => "zai",
        Provider::Minimax => "minimax",
        Provider::MiMo => "mimo",
        Provider::Mistral => "mistral",
        Provider::HuggingFace => "huggingface",
        Provider::OpenCodeZen => "opencode-zen",
        Provider::OpenCodeGo => "opencode-go",
        Provider::Qwen => "qwen",
        Provider::StepFun => "stepfun",
        Provider::Evolink => "evolink",
        Provider::Poolside => "poolside",
        Provider::XAI => "xai",
        Provider::NVIDIA => "nvidia",
        Provider::MergeGateway => "merge-gateway",
        Provider::Vercel => "vercel",
    }
}

fn catalog_lookup_id<'a>(provider: &str, id: &'a str) -> &'a str {
    let provider_key = catalog_provider_key(provider);
    if provider_key == "evolink"
        && let Some((prefix, model_id)) = id.split_once('/')
        && prefix.eq_ignore_ascii_case(provider_key)
    {
        // Evolink namespaces its ModelId values to avoid collisions with
        // first-class providers, while its catalog stores the upstream model
        // id that the gateway receives (for example, `deepseek-v4-pro`).
        return model_id;
    }
    id
}

fn generated_catalog_entry(provider: &str, id: &str) -> Option<ModelCatalogEntry> {
    let provider_key = catalog_provider_key(provider);
    let lookup_id = catalog_lookup_id(provider_key, id);
    capability_generated::metadata_for(provider_key, lookup_id).map(|entry| ModelCatalogEntry {
        provider: entry.provider,
        id: entry.id,
        display_name: entry.display_name,
        description: entry.description,
        context_window: entry.context_window,
        max_output_tokens: entry.max_output_tokens,
        reasoning: entry.reasoning,
        reasoning_efforts: entry.reasoning_efforts,
        is_pro: entry.is_pro,
        lightweight_model: entry.lightweight_model,
        tool_call: entry.tool_call,
        vision: entry.vision,
        input_modalities: entry.input_modalities,
        caching: entry.caching,
        structured_output: entry.structured_output,
        supports_sampling: entry.supports_sampling,
        supports_logprobs: entry.supports_logprobs,
        prompt_cache_ttl: entry.prompt_cache_ttl,
        prompt_contract: entry.prompt_contract,
        pricing: ModelPricing {
            input: entry.pricing.input,
            output: entry.pricing.output,
            cache_read: entry.pricing.cache_read,
            cache_write: entry.pricing.cache_write,
        },
    })
}

pub fn model_catalog_entry(provider: &str, id: &str) -> Option<ModelCatalogEntry> {
    generated_catalog_entry(provider, id)
}

pub fn supported_models_for_provider(provider: &str) -> Option<&'static [&'static str]> {
    capability_generated::models_for_provider(catalog_provider_key(provider))
}

pub fn catalog_provider_keys() -> &'static [&'static str] {
    capability_generated::PROVIDERS
}

impl ModelId {
    fn generated_capabilities(&self) -> Option<ModelCatalogEntry> {
        generated_catalog_entry(capability_provider_key(self.provider()), &self.as_str())
    }

    /// Preferred built-in lightweight sibling or lower-tier fallback for this model.
    pub fn preferred_lightweight_variant(&self) -> Option<Self> {
        let target_id = self.generated_capabilities()?.lightweight_model?;
        Self::all_models().into_iter().find(|candidate| {
            candidate != self && candidate.provider() == self.provider() && candidate.as_str() == target_id
        })
    }

    /// Attempt to find a non-reasoning variant for this model.
    pub fn non_reasoning_variant(&self) -> Option<Self> {
        if let Some(meta) = self.openrouter_metadata() {
            if !meta.reasoning {
                return None;
            }

            let vendor = meta.vendor;
            let mut candidates: Vec<Self> = Self::openrouter_vendor_groups()
                .into_iter()
                .find(|(candidate_vendor, _)| *candidate_vendor == vendor)
                .map(|(_, models)| {
                    models
                        .iter()
                        .filter(|&candidate| candidate != self)
                        .filter(|&candidate| {
                            candidate.openrouter_metadata().map(|other| !other.reasoning).unwrap_or(false)
                        })
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();

            if candidates.is_empty() {
                return None;
            }

            candidates.sort_by_key(|candidate| {
                candidate
                    .openrouter_metadata()
                    .map(|data| (!data.efficient, data.display))
                    .unwrap_or((true, ""))
            });

            return candidates.into_iter().next();
        }

        self.preferred_lightweight_variant()
            .filter(|candidate| !candidate.is_reasoning_variant())
    }

    /// Check if this is a "flash" variant (optimized for speed)
    pub fn is_flash_variant(&self) -> bool {
        matches!(
            self,
            ModelId::Gemini37Flash
                | ModelId::Gemini38Flash
                | ModelId::MergeGatewayGoogleGemini36Flash
                | ModelId::MergeGatewayGoogleGemini37Flash
                | ModelId::MergeGatewayGoogleGemini38Flash
                | ModelId::EvolinkGemini35Flash
                | ModelId::EvolinkDeepseekV4Flash
                | ModelId::VercelGoogleGemini38Flash
                | ModelId::VercelDeepseekV4Flash
                | ModelId::VercelOpenAiGpt56Luna
                | ModelId::VercelAnthropicClaudeHaiku45
                | ModelId::OpenRouterStepfunStep35FlashFree
                | ModelId::HuggingFaceStep35Flash
                | ModelId::StepFun37Flash
                | ModelId::HuggingFaceDeepseekV4FlashNovita
                | ModelId::MergeGatewayDeepseekV4Flash0731
                | ModelId::MergeGatewayDeepseekV4Flash0731Fast
                | ModelId::DeepSeekV4Flash
                | ModelId::DeepSeekV4FlashVisionExp
                | ModelId::ZaiGlm53Flash
                | ModelId::MergeGatewayZaiGlm53Flash
                | ModelId::HuggingFaceGlm53FlashTogether
        )
    }

    /// Check if this is a "pro" variant (optimized for capability)
    pub fn is_pro_variant(&self) -> bool {
        self.generated_capabilities().is_some_and(|entry| entry.is_pro)
    }

    /// Check if this is an optimized/efficient variant
    pub fn is_efficient_variant(&self) -> bool {
        if let Some(meta) = self.openrouter_metadata() {
            return meta.efficient;
        }
        matches!(
            self,
            ModelId::Gemini37Flash
                | ModelId::Gemini38Flash
                | ModelId::MergeGatewayGoogleGemini37Flash
                | ModelId::MergeGatewayGoogleGemini38Flash
                | ModelId::GPT56Luna
                | ModelId::MergeGatewayOpenAIGpt56Luna
                | ModelId::CopilotGPT54Mini
                | ModelId::DeepSeekV4Flash
                | ModelId::DeepSeekV4FlashVisionExp
                | ModelId::MergeGatewayDeepseekV4Flash0731
                | ModelId::MergeGatewayDeepseekV4Flash0731Fast
                | ModelId::MetaMuseSpark11
                | ModelId::MergeGatewayMinimaxH3
                | ModelId::HuggingFaceStep35Flash
                | ModelId::HuggingFaceDeepseekV4FlashNovita
                | ModelId::PoolsideLagunaXs2
                | ModelId::OpenCodeGoMimoV25
                | ModelId::OpenCodeGoQwen37Plus
                | ModelId::OpenCodeGoQwen36Plus
                | ModelId::OpenCodeGoDeepseekV4Flash
                | ModelId::XaiGrokBuild01
                | ModelId::ZaiGlm53Flash
                | ModelId::MergeGatewayZaiGlm53Flash
                | ModelId::HuggingFaceGlm53FlashTogether
        )
    }

    /// Check if this is a top-tier model
    pub fn is_top_tier(&self) -> bool {
        if let Some(meta) = self.openrouter_metadata() {
            return meta.top_tier;
        }
        matches!(
            self,
            ModelId::Gemini38Flash
                | ModelId::Gemini37Flash
                | ModelId::MergeGatewayGoogleGemini37Flash
                | ModelId::MergeGatewayGoogleGemini38Flash
                | ModelId::GPT6Astra
                | ModelId::GPT56Sol
                | ModelId::MergeGatewayOpenAIGpt56Sol
                | ModelId::MergeGatewayOpenAIGpt56Terra
                | ModelId::MergeGatewayOpenAIGpt56Luna
                | ModelId::MergeGatewayOpenAIGpt6Astra
                | ModelId::ClaudeSonnet5
                | ModelId::ClaudeFable5
                | ModelId::ClaudeFable51
                | ModelId::MergeGatewayAnthropicClaudeFable51
                | ModelId::ClaudeMythos5
                | ModelId::ClaudeMythos51
                | ModelId::ClaudeOpus5
                | ModelId::OpenCodeGoGlm53
                | ModelId::OpenCodeGoGlm52
                | ModelId::OpenCodeGoKimiK27Code
                | ModelId::OpenCodeGoMimoV25Pro
                | ModelId::OpenCodeGoMinimaxM3
                | ModelId::OpenCodeGoQwen37Max
                | ModelId::OpenCodeGoQwen37Plus
                | ModelId::OpenCodeGoDeepseekV4Pro
                | ModelId::DeepSeekV4Pro
                | ModelId::MergeGatewayDeepseekV4Pro0813
                | ModelId::MetaMuseSpark13
                | ModelId::MetaMuseSpark13Contributor
                | ModelId::MetaMuseSpark12
                | ModelId::MergeGatewayMetaMuseSpark13
                | ModelId::ZaiGlm53
                | ModelId::ZaiGlm53Flash
                | ModelId::ZaiGlm52
                | ModelId::MergeGatewayZaiGlm53Flash
                | ModelId::HuggingFaceGlm53FlashTogether
                | ModelId::HuggingFaceGlm53Together
                | ModelId::OpenRouterStepfunStep35FlashFree
                | ModelId::HuggingFaceDeepseekV4FlashNovita
                | ModelId::HuggingFaceDeepseekV4ProTogether
                | ModelId::HuggingFaceGlm52Novita
                | ModelId::HuggingFaceMinimaxM3Novita
                | ModelId::HuggingFaceDeepseekV4ProNovita
                | ModelId::OpenRouterMoonshotaiKimiK3
                | ModelId::OpenRouterMoonshotaiKimiK27Code
                | ModelId::MoonshotKimiK3
                | ModelId::MergeGatewayMoonshotKimiK3
                | ModelId::MoonshotKimiK27Code
                | ModelId::PoolsideLagunaM1
                | ModelId::PoolsideLagunaS21
                | ModelId::OllamaGlm52Cloud
                | ModelId::OllamaGlm53Cloud
                | ModelId::XaiGrok46
                | ModelId::MergeGatewayXaiGrok46
                | ModelId::XaiGrok420Reasoning
        )
    }

    /// Determine whether the model is a reasoning-capable variant
    pub fn is_reasoning_variant(&self) -> bool {
        if let Some(meta) = self.openrouter_metadata() {
            return meta.reasoning;
        }
        self.provider().supports_reasoning(&self.as_str())
    }

    /// Determine whether the model supports tool calls/function execution
    pub fn supports_tool_calls(&self) -> bool {
        if let Some(meta) = self.generated_capabilities() {
            return meta.tool_call;
        }
        if let Some(meta) = self.openrouter_metadata() {
            return meta.tool_call;
        }
        true
    }

    /// Ordered list of supported input modalities when VT Code has metadata for this model.
    pub fn input_modalities(&self) -> &'static [&'static str] {
        self.generated_capabilities().map(|meta| meta.input_modalities).unwrap_or(&[])
    }

    /// Get the generation/version string for this model
    pub fn generation(&self) -> &'static str {
        if let Some(meta) = self.openrouter_metadata() {
            return meta.generation;
        }
        match self {
            // Gemini generations
            // OpenAI generations
            ModelId::GPT6Astra => "6",
            ModelId::GPT56Sol | ModelId::GPT56Terra | ModelId::GPT56Luna => "5.6",
            ModelId::OpenAIGptOss20b | ModelId::OpenAIGptOss120b => "5",
            // Anthropic generations
            ModelId::ClaudeSonnet5 => "5",
            ModelId::ClaudeFable5 => "5",
            ModelId::ClaudeFable51 => "5.1",
            ModelId::ClaudeMythos5 => "5",
            ModelId::ClaudeMythos51 => "5.1",
            ModelId::ClaudeOpus5 => "5",
            // DeepSeek generations
            ModelId::DeepSeekV4Pro | ModelId::DeepSeekV4Flash | ModelId::DeepSeekV4FlashVisionExp => "4",
            ModelId::MergeGatewayDeepseekV4Pro0813 => "4-pro-0813",
            ModelId::MergeGatewayDeepseekV4Flash0731 => "4-flash-0731",
            ModelId::MetaMuseSpark11 => "Muse-Spark-1.1",
            ModelId::MetaMuseSpark12 | ModelId::MetaMuseSpark12Contributor => "Muse-Spark-1.2",
            ModelId::MetaMuseSpark13 | ModelId::MetaMuseSpark13Contributor => "Muse-Spark-1.3",
            // Z.AI generations
            ModelId::ZaiGlm53 | ModelId::ZaiGlm53Flash | ModelId::MergeGatewayZaiGlm53Flash => "5.3",
            ModelId::ZaiGlm52 => "5.2",
            ModelId::Gemini36Flash => "3.6",
            ModelId::Gemini37Flash => "3.7",
            ModelId::Gemini38Flash => "3.8",
            ModelId::MergeGatewayGoogleGemini37Flash => "3.7",
            ModelId::MergeGatewayGoogleGemini38Flash => "3.8",
            ModelId::OpenCodeGoGlm53 => "5.3",
            ModelId::OpenCodeGoGlm52 => "5.2",
            ModelId::OpenCodeGoGpt56Luna => "5.6-luna",
            ModelId::OpenCodeGoKimiK3 => "k3",
            ModelId::OpenCodeGoKimiK27Code => "k2.7",
            ModelId::OpenCodeGoMimoV25 | ModelId::OpenCodeGoMimoV25Pro => "v2.5",
            ModelId::OpenCodeGoMinimaxM3 => "m3",
            ModelId::OpenCodeGoMuseSpark12Contributor => "Muse-Spark-1.2",
            ModelId::OpenCodeGoQwen38Max => "3.8-max",
            ModelId::OpenCodeGoQwen37Max => "3.7-max",
            ModelId::OpenCodeGoQwen37Plus => "3.7-plus",
            ModelId::OpenCodeGoQwen36Plus => "3.6-plus",
            ModelId::OpenCodeGoDeepseekV4Pro | ModelId::OpenCodeGoDeepseekV4Flash => "v4",
            ModelId::OpenCodeGoHy3 => "hy3",
            ModelId::OllamaGptOss20b => "oss",
            ModelId::OllamaGptOss20bCloud => "oss-cloud",
            ModelId::OllamaGptOss120bCloud => "oss-cloud",
            ModelId::OllamaDeepseekV4FlashCloud => "deepseek-v4-flash",
            ModelId::OllamaDeepseekV4ProCloud => "deepseek-v4-pro",
            ModelId::OllamaMinimaxM3Cloud => "minimax-m3",
            ModelId::OllamaGlm52Cloud => "glm-5.2",
            ModelId::OllamaGlm53Cloud => "glm-5.3",
            ModelId::OllamaKimiK27CodeCloud => "kimi-k2.7-code",
            ModelId::OllamaLagunaXs2 => "laguna-xs.2",
            ModelId::OllamaGemma4 => "gemma-4",
            ModelId::LlamaCppGemma426bA4b => "4",
            ModelId::LlamaCppGemma4E4b => "4",
            ModelId::LlamaCppGptOss20b => "oss",
            ModelId::LlamaCppStep35Flash => "3.5",
            // MiniMax models
            ModelId::MinimaxM3 => "M3",
            // Moonshot models
            ModelId::MoonshotKimiK3 => "k3",
            ModelId::MergeGatewayMoonshotKimiK3 => "k3",
            ModelId::MoonshotKimiK27Code => "k2.7",
            // Hugging Face generations
            ModelId::HuggingFaceOpenAIGptOss20b => "oss",
            ModelId::HuggingFaceOpenAIGptOss120b => "oss",
            ModelId::HuggingFaceMinimaxM3Novita => "m3",
            ModelId::HuggingFaceGlm52Novita => "5.2",
            ModelId::HuggingFaceGlm53FlashTogether => "5.3",
            ModelId::HuggingFaceGlm53Together => "5.3",
            ModelId::HuggingFaceDeepseekV4FlashNovita => "v4-flash",
            ModelId::HuggingFaceDeepseekV4ProTogether => "v4-pro",
            ModelId::HuggingFaceDeepseekV4ProNovita => "v4-pro",
            ModelId::HuggingFaceStep35Flash => "3.5",
            // xAI models
            ModelId::XaiGrokBuild01 => "build-0.1",
            ModelId::XaiGrok46 => "4.6",
            ModelId::MergeGatewayXaiGrok46 => "4.6",
            ModelId::XaiGrok420Reasoning => "4.20",
            // Poolside models
            ModelId::PoolsideLagunaM1 => "laguna-m.1",
            ModelId::PoolsideLagunaXs2 => "laguna-xs.2",
            ModelId::PoolsideLagunaS21 => "laguna-s.2.1",
            // Qwen models
            ModelId::QwenDeepSeekV4Flash | ModelId::QwenDeepSeekV4Pro => "v4",
            ModelId::MergeGatewayDefaultRouting => "routing",
            ModelId::MergeGatewayOpenAIGpt55 => "5.5",
            ModelId::MergeGatewayAnthropicClaudeOpus5 => "5",
            ModelId::MergeGatewayGoogleGemini36Flash => "3.6",
            ModelId::MergeGatewayAnthropicClaudeFable51 => "5.1",
            ModelId::MergeGatewayDeepseekV4Flash0731Fast => "4-flash-0731-fast",
            ModelId::MergeGatewayQwen38Max => "3.8-max",
            ModelId::MergeGatewayMinimaxH3 => "H3",
            ModelId::MergeGatewayThinkingMachinesInkling => "Inkling",
            ModelId::MergeGatewayMetaMuseSpark11 => "Muse-Spark-1.1",
            ModelId::MergeGatewayMetaMuseSpark13 => "Muse-Spark-1.3",
            ModelId::MergeGatewayOpenAIGpt56Luna
            | ModelId::MergeGatewayOpenAIGpt56Sol
            | ModelId::MergeGatewayOpenAIGpt56Terra => "5.6",
            ModelId::MergeGatewayOpenAIGpt6Astra => "6",
            // Vercel AI Gateway models
            ModelId::VercelAnthropicClaudeSonnet5 => "5",
            ModelId::VercelAnthropicClaudeOpus5 => "5",
            ModelId::VercelAnthropicClaudeHaiku45 => "4.5",
            ModelId::VercelOpenAiGpt56Sol | ModelId::VercelOpenAiGpt56Luna => "5.6",
            ModelId::VercelOpenAiGpt6Astra => "6",
            ModelId::VercelOpenAiGpt53Codex => "5.3-codex",
            ModelId::VercelGoogleGemini31ProPreview => "3.1-pro",
            ModelId::VercelGoogleGemini38Flash => "3.8",
            ModelId::VercelDeepseekV4Pro => "v4-pro",
            ModelId::VercelDeepseekV4Flash => "v4-flash",
            ModelId::VercelMoonshotaiKimiK3 => "k3",
            ModelId::VercelMoonshotaiKimiK27Code => "k2.7",
            ModelId::VercelAlibabaQwen38Max => "3.8-max",
            ModelId::VercelAlibabaQwen3CoderNext => "coder-next",
            ModelId::VercelMinimaxM3 => "M3",
            ModelId::VercelMistralDevstral2 => "devstral-2",
            _ => "unknown",
        }
    }

    /// Determine if this model supports GPT-5.1+/5.2+/5.3+ shell tool type
    pub(crate) fn supports_shell_tool(&self) -> bool {
        matches!(self, ModelId::GPT6Astra | ModelId::GPT56Sol | ModelId::GPT56Terra | ModelId::GPT56Luna)
    }

    /// Determine if this model supports optimized apply_patch tool
    pub fn supports_apply_patch_tool(&self) -> bool {
        false
    }
}
