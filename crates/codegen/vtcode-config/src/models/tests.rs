use super::*;
use crate::constants::{model_helpers, models};
use std::str::FromStr;

#[test]
fn test_model_string_conversion() {
    // Gemini models
    assert_eq!(ModelId::Gemini37Flash.as_str(), models::GEMINI_3_7_FLASH);
    // OpenAI models
    assert_eq!(ModelId::GPT56Sol.as_str(), models::GPT_5_6_SOL);
    assert_eq!(ModelId::GPT56Luna.as_str(), models::openai::GPT_5_6_LUNA);
    // Anthropic models
    assert_eq!(ModelId::ClaudeOpus5.as_str(), models::CLAUDE_OPUS_5);
    assert_eq!(ModelId::ClaudeSonnet5.as_str(), models::CLAUDE_SONNET_5);
    // DeepSeek models
    assert_eq!(ModelId::DeepSeekV4Pro.as_str(), models::deepseek::DEEPSEEK_V4_PRO);
    assert_eq!(ModelId::DeepSeekV4Flash.as_str(), models::deepseek::DEEPSEEK_V4_FLASH);
    assert_eq!(ModelId::DeepSeekV4Flash.as_str(), models::deepseek::DEEPSEEK_V4_FLASH);
    // Official Meta AI models
    assert_eq!(ModelId::MetaMuseSpark11.as_str(), models::meta::MUSE_SPARK_1_1);
    assert_eq!(ModelId::MetaMuseSpark12.as_str(), models::meta::MUSE_SPARK_1_2);
    assert_eq!(ModelId::MetaMuseSpark12Contributor.as_str(), models::meta::MUSE_SPARK_1_2_CONTRIBUTOR);
    // Hugging Face models
    assert_eq!(ModelId::HuggingFaceGlm52Novita.as_str(), models::huggingface::ZAI_GLM_5_2_NOVITA);
    assert_eq!(ModelId::HuggingFaceKimiK3Together.as_str(), models::huggingface::KIMI_K3_TOGETHER);
    // xAI models
    assert_eq!(ModelId::XaiGrok46.as_str(), models::xai::GROK_4_6);
    assert_eq!(ModelId::XaiGrok46.as_str(), models::xai::GROK_4_6);
    assert_eq!(ModelId::XaiGrokBuild01.as_str(), models::xai::GROK_BUILD_0_1);
    // OpenCode models
    for entry in openrouter_generated::ENTRIES {
        assert_eq!(entry.variant.as_str(), entry.id);
    }
}

#[test]
fn test_model_from_string() {
    // Gemini models
    assert_eq!(models::GEMINI_3_7_FLASH.parse::<ModelId>().unwrap(), ModelId::Gemini37Flash);
    // OpenAI models
    assert_eq!(models::GPT_5_6_SOL.parse::<ModelId>().unwrap(), ModelId::GPT56Sol);
    assert_eq!(models::GPT_5_CODEX.parse::<ModelId>().unwrap(), ModelId::GPT56Sol);
    assert_eq!(models::openai::GPT_5_6_LUNA.parse::<ModelId>().unwrap(), ModelId::GPT56Luna);
    assert_eq!(models::openai::GPT_5_6_LUNA.parse::<ModelId>().unwrap(), ModelId::GPT56Luna);
    assert_eq!(models::openai::GPT_OSS_20B.parse::<ModelId>().unwrap(), ModelId::OpenAIGptOss20b);
    assert_eq!(models::openai::GPT_OSS_120B.parse::<ModelId>().unwrap(), ModelId::OpenAIGptOss120b);
    // Anthropic models
    assert_eq!(models::CLAUDE_SONNET_5.parse::<ModelId>().unwrap(), ModelId::ClaudeSonnet5);
    assert_eq!(models::CLAUDE_SONNET_5.parse::<ModelId>().unwrap(), ModelId::ClaudeSonnet5);
    assert_eq!(models::CLAUDE_OPUS_5.parse::<ModelId>().unwrap(), ModelId::ClaudeOpus5);
    assert_eq!(models::CLAUDE_SONNET_5.parse::<ModelId>().unwrap(), ModelId::ClaudeSonnet5);
    // DeepSeek models
    assert_eq!(models::deepseek::DEEPSEEK_V4_PRO.parse::<ModelId>().unwrap(), ModelId::DeepSeekV4Pro);
    assert_eq!(models::deepseek::DEEPSEEK_V4_FLASH.parse::<ModelId>().unwrap(), ModelId::DeepSeekV4Flash);
    assert_eq!(models::meta::MUSE_SPARK_1_1.parse::<ModelId>().unwrap(), ModelId::MetaMuseSpark11);
    assert_eq!(models::meta::MUSE_SPARK_1_2.parse::<ModelId>().unwrap(), ModelId::MetaMuseSpark12);
    assert_eq!(
        models::meta::MUSE_SPARK_1_2_CONTRIBUTOR.parse::<ModelId>().unwrap(),
        ModelId::MetaMuseSpark12Contributor
    );
    assert_eq!(models::ollama::DEEPSEEK_V4_PRO_CLOUD.parse::<ModelId>().unwrap(), ModelId::OllamaDeepseekV4ProCloud);
    // Hugging Face models
    assert_eq!(models::huggingface::ZAI_GLM_5_2_NOVITA.parse::<ModelId>().unwrap(), ModelId::HuggingFaceGlm52Novita);
    assert_eq!(
        models::huggingface::KIMI_K3_TOGETHER.parse::<ModelId>().unwrap(),
        ModelId::HuggingFaceKimiK3Together
    );
    assert_eq!(models::xai::GROK_4_6.parse::<ModelId>().unwrap(), ModelId::XaiGrok46);
    assert_eq!(models::xai::GROK_4_6.parse::<ModelId>().unwrap(), ModelId::XaiGrok46);
    for entry in openrouter_generated::ENTRIES {
        assert_eq!(entry.id.parse::<ModelId>().unwrap(), entry.variant);
    }
    // Invalid model
    assert!("invalid-model".parse::<ModelId>().is_err());
}

#[test]
fn test_provider_parsing() {
    assert_eq!("gemini".parse::<Provider>().unwrap(), Provider::Gemini);
    assert_eq!("openai".parse::<Provider>().unwrap(), Provider::OpenAI);
    assert_eq!("anthropic".parse::<Provider>().unwrap(), Provider::Anthropic);
    assert_eq!("deepseek".parse::<Provider>().unwrap(), Provider::DeepSeek);
    assert_eq!("meta".parse::<Provider>().unwrap(), Provider::Meta);
    assert_eq!("meta-ai".parse::<Provider>().unwrap(), Provider::Meta);
    assert_eq!("nvidia".parse::<Provider>().unwrap(), Provider::NVIDIA);
    assert_eq!("nvidia-nim".parse::<Provider>().unwrap(), Provider::NVIDIA);
    assert_eq!("merge-gateway".parse::<Provider>().unwrap(), Provider::MergeGateway);
    assert_eq!("merge_gateway".parse::<Provider>().unwrap(), Provider::MergeGateway);
    assert_eq!("openrouter".parse::<Provider>().unwrap(), Provider::OpenRouter);
    assert_eq!("zai".parse::<Provider>().unwrap(), Provider::ZAI);
    assert_eq!("moonshot".parse::<Provider>().unwrap(), Provider::Moonshot);
    assert_eq!("opencode-zen".parse::<Provider>().unwrap(), Provider::OpenCodeZen);
    assert_eq!("opencode-go".parse::<Provider>().unwrap(), Provider::OpenCodeGo);
    assert_eq!("lmstudio".parse::<Provider>().unwrap(), Provider::LmStudio);
    assert!("invalid-provider".parse::<Provider>().is_err());
}

#[test]
fn test_model_providers() {
    assert_eq!(ModelId::Gemini37Flash.provider(), Provider::Gemini);
    assert_eq!(ModelId::GPT56Sol.provider(), Provider::OpenAI);
    assert_eq!(ModelId::ClaudeOpus5.provider(), Provider::Anthropic);
    assert_eq!(ModelId::ClaudeSonnet5.provider(), Provider::Anthropic);
    assert_eq!(ModelId::ClaudeSonnet5.provider(), Provider::Anthropic);
    assert_eq!(ModelId::DeepSeekV4Pro.provider(), Provider::DeepSeek);
    assert_eq!(ModelId::MetaMuseSpark12.provider(), Provider::Meta);
    assert_eq!(ModelId::NvidiaNemotron3Ultra550bA55b.provider(), Provider::NVIDIA);
    assert_eq!(ModelId::NvidiaDeepseekV4Flash0731.provider(), Provider::NVIDIA);
    assert_eq!(ModelId::MergeGatewayDefaultRouting.provider(), Provider::MergeGateway);
    assert_eq!(ModelId::MergeGatewayOpenAIGpt55.provider(), Provider::MergeGateway);
    assert_eq!(ModelId::MergeGatewayAnthropicClaudeOpus5.provider(), Provider::MergeGateway);
    assert_eq!(ModelId::MergeGatewayGoogleGemini36Flash.provider(), Provider::MergeGateway);
    assert_eq!(ModelId::MergeGatewayGoogleGemini37Flash.provider(), Provider::MergeGateway);
    assert_eq!(ModelId::MergeGatewayGoogleGemini38Flash.provider(), Provider::MergeGateway);
    assert_eq!(ModelId::MergeGatewayAnthropicClaudeFable51.provider(), Provider::MergeGateway);
    assert_eq!(ModelId::MergeGatewayDeepseekV4Pro0813.provider(), Provider::MergeGateway);
    assert_eq!(ModelId::MergeGatewayDeepseekV4Flash0731.provider(), Provider::MergeGateway);
    assert_eq!(ModelId::MergeGatewayDeepseekV4Flash0731Fast.provider(), Provider::MergeGateway);
    assert_eq!(ModelId::MergeGatewayXaiGrok46.provider(), Provider::MergeGateway);
    assert_eq!(ModelId::MergeGatewayQwen38Max.provider(), Provider::MergeGateway);
    assert_eq!(ModelId::MergeGatewayMinimaxH3.provider(), Provider::MergeGateway);
    assert_eq!(ModelId::MergeGatewayMoonshotKimiK3.provider(), Provider::MergeGateway);
    assert_eq!(ModelId::MergeGatewayThinkingMachinesInkling.provider(), Provider::MergeGateway);
    assert_eq!(ModelId::MergeGatewayMetaMuseSpark11.provider(), Provider::MergeGateway);
    assert_eq!(ModelId::MergeGatewayMetaMuseSpark13.provider(), Provider::MergeGateway);
    assert_eq!(ModelId::MergeGatewayOpenAIGpt56Luna.provider(), Provider::MergeGateway);
    assert_eq!(ModelId::MergeGatewayOpenAIGpt56Sol.provider(), Provider::MergeGateway);
    assert_eq!(ModelId::MergeGatewayOpenAIGpt56Terra.provider(), Provider::MergeGateway);
    assert_eq!(ModelId::ZaiGlm52.provider(), Provider::ZAI);
    assert_eq!(ModelId::OpenCodeGoMinimaxM3.provider(), Provider::OpenCodeGo);
    assert_eq!(ModelId::OllamaGptOss20b.provider(), Provider::Ollama);
    assert_eq!(ModelId::OllamaGptOss120bCloud.provider(), Provider::OllamaCloud);
    assert_eq!(ModelId::OpenRouterAnthropicClaudeSonnet5.provider(), Provider::OpenRouter);

    for entry in openrouter_generated::ENTRIES {
        assert_eq!(entry.variant.provider(), Provider::OpenRouter);
    }
}

#[test]
fn test_provider_defaults() {
    assert_eq!(ModelId::default_orchestrator_for_provider(Provider::Gemini), ModelId::Gemini37Flash);
    assert_eq!(ModelId::default_orchestrator_for_provider(Provider::OpenAI), ModelId::GPT56Sol);
    assert_eq!(ModelId::default_orchestrator_for_provider(Provider::Anthropic), ModelId::ClaudeOpus5);
    assert_eq!(ModelId::default_orchestrator_for_provider(Provider::DeepSeek), ModelId::DeepSeekV4Pro);
    assert_eq!(ModelId::default_orchestrator_for_provider(Provider::Meta), ModelId::MetaMuseSpark13);
    assert_eq!(ModelId::default_orchestrator_for_provider(Provider::NVIDIA), ModelId::NvidiaNemotron3Ultra550bA55b);
    assert_eq!(
        ModelId::default_orchestrator_for_provider(Provider::OpenRouter),
        ModelId::OpenRouterXiaomiMimoV25Pro
    );
    assert_eq!(ModelId::default_orchestrator_for_provider(Provider::Ollama), ModelId::OllamaGptOss20b);
    assert_eq!(ModelId::default_orchestrator_for_provider(Provider::ZAI), ModelId::ZaiGlm53);
    assert_eq!(ModelId::default_orchestrator_for_provider(Provider::OpenCodeGo), ModelId::OpenCodeGoMinimaxM3);
    assert_eq!(ModelId::default_orchestrator_for_provider(Provider::XAI), ModelId::XaiGrok46);
    assert_eq!(ModelId::default_single_for_provider(Provider::XAI), ModelId::XaiGrok46);
    assert_eq!(
        ModelId::default_orchestrator_for_provider(Provider::MergeGateway),
        ModelId::MergeGatewayDefaultRouting
    );
    assert_eq!(ModelId::default_single_for_provider(Provider::MergeGateway), ModelId::MergeGatewayDefaultRouting);
}

#[test]
fn test_model_defaults() {
    assert_eq!(ModelId::default(), ModelId::ClaudeSonnet5);
    assert_eq!(ModelId::default_model(), ModelId::ClaudeSonnet5);
    assert_eq!(ModelId::default_orchestrator(), ModelId::ClaudeSonnet5);
}

#[test]
fn test_model_variants() {
    // Flash variants
    assert!(ModelId::Gemini37Flash.is_flash_variant());
    assert!(!ModelId::GPT56Sol.is_flash_variant());

    // Pro variants
    assert!(ModelId::GPT56Sol.is_pro_variant());
    assert!(ModelId::GPT56Sol.is_pro_variant());
    assert!(ModelId::ClaudeOpus5.is_pro_variant());
    assert!(ModelId::ClaudeSonnet5.is_pro_variant());
    assert!(ModelId::DeepSeekV4Pro.is_pro_variant());
    assert!(ModelId::MetaMuseSpark13.is_pro_variant());
    assert!(ModelId::MetaMuseSpark13Contributor.is_pro_variant());
    assert!(ModelId::MetaMuseSpark12.is_pro_variant());
    assert!(ModelId::XaiGrok46.is_pro_variant());
    assert!(!ModelId::Gemini37Flash.is_pro_variant());

    // Efficient variants
    assert!(ModelId::Gemini37Flash.is_efficient_variant());
    assert!(ModelId::GPT56Luna.is_efficient_variant());
    assert!(ModelId::DeepSeekV4Flash.is_efficient_variant());
    assert!(ModelId::MetaMuseSpark11.is_efficient_variant());
    assert!(!ModelId::GPT56Sol.is_efficient_variant());

    for entry in openrouter_generated::ENTRIES {
        assert_eq!(entry.variant.is_efficient_variant(), entry.efficient);
    }

    // Top tier models
    assert!(ModelId::GPT56Sol.is_top_tier());
    assert!(ModelId::ClaudeOpus5.is_top_tier());
    assert!(ModelId::ClaudeSonnet5.is_top_tier());
    assert!(ModelId::DeepSeekV4Pro.is_top_tier());
    assert!(ModelId::MetaMuseSpark13.is_top_tier());
    assert!(ModelId::MetaMuseSpark13Contributor.is_top_tier());
    assert!(ModelId::MetaMuseSpark12.is_top_tier());
    assert!(ModelId::ZaiGlm52.is_top_tier());
    assert!(ModelId::Gemini37Flash.is_top_tier());
    assert!(ModelId::XaiGrok46.is_top_tier());
    assert!(!ModelId::OpenAIGptOss20b.is_top_tier());

    for entry in openrouter_generated::ENTRIES {
        assert_eq!(entry.variant.is_top_tier(), entry.top_tier);
    }
}

#[test]
fn test_preferred_lightweight_variant() {
    assert_eq!(ModelId::GPT56Sol.preferred_lightweight_variant(), Some(ModelId::GPT56Terra));
    assert_eq!(ModelId::ClaudeSonnet5.preferred_lightweight_variant(), None);
    assert_eq!(ModelId::Gemini37Flash.preferred_lightweight_variant(), None);
    assert_eq!(ModelId::ZaiGlm52.preferred_lightweight_variant(), None);
    assert_eq!(ModelId::MetaMuseSpark12.preferred_lightweight_variant(), Some(ModelId::MetaMuseSpark11));
    assert_eq!(ModelId::MetaMuseSpark13.preferred_lightweight_variant(), Some(ModelId::MetaMuseSpark12));
    assert_eq!(
        ModelId::MetaMuseSpark13Contributor.preferred_lightweight_variant(),
        Some(ModelId::MetaMuseSpark12Contributor)
    );
    assert_eq!(ModelId::GPT56Luna.preferred_lightweight_variant(), None);
    assert_eq!(ModelId::XaiGrok46.preferred_lightweight_variant(), Some(ModelId::XaiGrokBuild01));
}

#[test]
fn test_model_generation() {
    // Gemini generations
    assert_eq!(ModelId::Gemini37Flash.generation(), "3.7");

    // OpenAI generations
    assert_eq!(ModelId::GPT56Sol.generation(), "5.6");
    assert_eq!(ModelId::GPT56Terra.generation(), "5.6");
    assert_eq!(ModelId::GPT56Luna.generation(), "5.6");
    assert_eq!(ModelId::OpenAIGptOss20b.generation(), "5");

    // Anthropic generations
    assert_eq!(ModelId::ClaudeOpus5.generation(), "5");
    assert_eq!(ModelId::ClaudeSonnet5.generation(), "5");

    // DeepSeek generations
    assert_eq!(ModelId::DeepSeekV4Pro.generation(), "4");
    assert_eq!(ModelId::DeepSeekV4Flash.generation(), "4");
    assert_eq!(ModelId::MetaMuseSpark11.generation(), "Muse-Spark-1.1");
    assert_eq!(ModelId::MetaMuseSpark12.generation(), "Muse-Spark-1.2");
    assert_eq!(ModelId::MetaMuseSpark12Contributor.generation(), "Muse-Spark-1.2");
    assert_eq!(ModelId::MetaMuseSpark13.generation(), "Muse-Spark-1.3");
    assert_eq!(ModelId::MetaMuseSpark13Contributor.generation(), "Muse-Spark-1.3");

    // Z.AI generations
    assert_eq!(ModelId::ZaiGlm52.generation(), "5.2");
    assert_eq!(ModelId::OpenCodeGoMinimaxM3.generation(), "m3");
    // xAI generations
    assert_eq!(ModelId::XaiGrok46.generation(), "4.6");

    for entry in openrouter_generated::ENTRIES {
        assert_eq!(entry.variant.generation(), entry.generation);
    }
}

#[test]
fn test_models_for_provider() {
    let gemini_models = ModelId::models_for_provider(Provider::Gemini);
    assert!(gemini_models.contains(&ModelId::Gemini37Flash));
    assert!(!gemini_models.contains(&ModelId::GPT56Sol));

    let openai_models = ModelId::models_for_provider(Provider::OpenAI);
    assert!(openai_models.contains(&ModelId::GPT56Sol));
    assert!(openai_models.contains(&ModelId::GPT56Sol));
    assert!(!openai_models.contains(&ModelId::Gemini37Flash));

    let anthropic_models = ModelId::models_for_provider(Provider::Anthropic);
    assert!(anthropic_models.contains(&ModelId::ClaudeOpus5));
    assert!(anthropic_models.contains(&ModelId::ClaudeSonnet5));
    assert!(anthropic_models.contains(&ModelId::ClaudeSonnet5));
    assert!(!anthropic_models.contains(&ModelId::GPT56Sol));

    let deepseek_models = ModelId::models_for_provider(Provider::DeepSeek);
    assert!(deepseek_models.contains(&ModelId::DeepSeekV4Pro));
    assert!(deepseek_models.contains(&ModelId::DeepSeekV4Flash));

    let meta_models = ModelId::models_for_provider(Provider::Meta);
    assert_eq!(
        meta_models,
        &[
            ModelId::MetaMuseSpark13,
            ModelId::MetaMuseSpark13Contributor,
            ModelId::MetaMuseSpark12,
            ModelId::MetaMuseSpark12Contributor,
            ModelId::MetaMuseSpark11
        ]
    );

    let nvidia_models = ModelId::models_for_provider(Provider::NVIDIA);
    assert_eq!(nvidia_models.len(), 5);
    assert!(nvidia_models.contains(&ModelId::NvidiaNemotron3Ultra550bA55b));
    assert!(nvidia_models.contains(&ModelId::NvidiaZaiGlm52));

    let merge_gateway_models = ModelId::models_for_provider(Provider::MergeGateway);
    assert_eq!(merge_gateway_models.len(), 22);
    assert!(merge_gateway_models.contains(&ModelId::MergeGatewayDefaultRouting));
    assert!(merge_gateway_models.contains(&ModelId::MergeGatewayOpenAIGpt55));
    assert!(merge_gateway_models.contains(&ModelId::MergeGatewayGoogleGemini37Flash));
    assert!(merge_gateway_models.contains(&ModelId::MergeGatewayGoogleGemini38Flash));
    assert!(merge_gateway_models.contains(&ModelId::MergeGatewayAnthropicClaudeFable51));
    assert!(merge_gateway_models.contains(&ModelId::MergeGatewayDeepseekV4Flash0731Fast));
    assert!(merge_gateway_models.contains(&ModelId::MergeGatewayDeepseekV4Pro0813));
    assert!(merge_gateway_models.contains(&ModelId::MergeGatewayOpenAIGpt56Terra));
    assert!(merge_gateway_models.contains(&ModelId::MergeGatewayMetaMuseSpark13));
    assert!(merge_gateway_models.contains(&ModelId::MergeGatewayOpenAIGpt6Astra));

    let openrouter_models = ModelId::models_for_provider(Provider::OpenRouter);
    assert!(openrouter_models.contains(&ModelId::OpenRouterOpenAIGpt5));
    assert!(openrouter_models.contains(&ModelId::OpenRouterMetaMuseGlimmer30b));
    assert!(openrouter_models.contains(&ModelId::OpenRouterMetaMuseSpark12));
    for entry in openrouter_generated::ENTRIES {
        assert!(openrouter_models.contains(&entry.variant));
    }

    let zai_models = ModelId::models_for_provider(Provider::ZAI);
    assert!(zai_models.contains(&ModelId::ZaiGlm52));

    let xai_models = ModelId::models_for_provider(Provider::XAI);
    assert!(xai_models.contains(&ModelId::XaiGrok46));
    assert!(xai_models.contains(&ModelId::XaiGrok46));
    assert!(!xai_models.contains(&ModelId::GPT56Sol));

    let opencode_go_models = ModelId::models_for_provider(Provider::OpenCodeGo);
    assert!(opencode_go_models.contains(&ModelId::OpenCodeGoMinimaxM3));

    let ollama_models = ModelId::models_for_provider(Provider::Ollama);
    assert!(ollama_models.contains(&ModelId::OllamaGptOss20b));

    let ollama_cloud_models = ModelId::models_for_provider(Provider::OllamaCloud);
    assert!(ollama_cloud_models.contains(&ModelId::OllamaGptOss20bCloud));
    assert!(ollama_cloud_models.contains(&ModelId::OllamaGptOss120bCloud));
    assert!(ollama_cloud_models.contains(&ModelId::OllamaDeepseekV4FlashCloud));
    assert!(ollama_cloud_models.contains(&ModelId::OllamaDeepseekV4ProCloud));
    assert!(ollama_cloud_models.contains(&ModelId::OllamaMinimaxM3Cloud));
    assert!(ollama_cloud_models.contains(&ModelId::OllamaMinimaxM3Cloud));
    assert!(ollama_cloud_models.contains(&ModelId::OllamaGlm52Cloud));

    let hf_models = ModelId::models_for_provider(Provider::HuggingFace);
    assert!(hf_models.contains(&ModelId::HuggingFaceGlm52Novita));
}

#[test]
fn test_ollama_cloud_models() {
    use crate::constants::models;

    // Test parsing of Ollama cloud models
    let model_pairs = vec![
        (ModelId::OllamaGptOss20bCloud, models::ollama::GPT_OSS_20B_CLOUD),
        (ModelId::OllamaGptOss120bCloud, models::ollama::GPT_OSS_120B_CLOUD),
        (ModelId::OllamaDeepseekV4FlashCloud, models::ollama::DEEPSEEK_V4_FLASH_CLOUD),
        (ModelId::OllamaDeepseekV4ProCloud, models::ollama::DEEPSEEK_V4_PRO_CLOUD),
        (ModelId::OllamaMinimaxM3Cloud, models::ollama::MINIMAX_M3_CLOUD),
        (ModelId::OllamaMinimaxM3Cloud, models::ollama::MINIMAX_M3_CLOUD),
        (ModelId::OllamaGlm52Cloud, models::ollama::GLM_5_2_CLOUD),
        (ModelId::OllamaKimiK3Cloud, models::ollama::KIMI_K3_CLOUD),
        (ModelId::OllamaKimiK27CodeCloud, models::ollama::KIMI_K2_7_CODE_CLOUD),
        (ModelId::OllamaKimiK3Cloud, models::ollama::KIMI_K3_CLOUD),
    ];

    for (model_id, expected_str) in model_pairs {
        assert_eq!(model_id.as_str(), expected_str);
        assert_eq!(ModelId::from_str(expected_str).unwrap(), model_id);
        assert_eq!(model_id.provider(), Provider::OllamaCloud);

        // Verify display names are not empty
        assert!(!model_id.display_name().is_empty());

        // Verify descriptions are not empty
        assert!(!model_id.description().is_empty());

        // Verify generation is not empty
        assert!(!model_id.generation().is_empty());
    }
}

#[test]
fn test_fallback_models() {
    let fallbacks = ModelId::fallback_models();
    assert!(!fallbacks.is_empty());
    assert!(fallbacks.contains(&ModelId::Gemini37Flash));
    assert!(fallbacks.contains(&ModelId::GPT56Sol));
    assert!(fallbacks.contains(&ModelId::GPT56Sol));
    assert!(fallbacks.contains(&ModelId::ClaudeOpus5));
    assert!(fallbacks.contains(&ModelId::ClaudeSonnet5));
    assert!(fallbacks.contains(&ModelId::DeepSeekV4Pro));
    assert!(fallbacks.contains(&ModelId::ZaiGlm53));
}

#[test]
fn test_provider_local_helpers() {
    assert!(Provider::Ollama.is_local());
    assert!(Provider::LmStudio.is_local());
    assert!(Provider::LlamaCpp.is_local());
    assert!(!Provider::OpenAI.is_local());
    assert!(Provider::Ollama.is_dynamic());
    assert!(Provider::LmStudio.is_dynamic());
    assert!(Provider::LlamaCpp.is_dynamic());
    assert!(Provider::Copilot.is_dynamic());
    assert!(!Provider::OpenAI.is_dynamic());
    assert!(Provider::Ollama.local_install_instructions().is_some());
    assert!(Provider::LmStudio.local_install_instructions().is_some());
    assert!(Provider::LlamaCpp.local_install_instructions().is_some());
    assert!(Provider::OpenAI.local_install_instructions().is_none());
}

#[test]
fn test_core_capability_helpers() {
    assert_eq!(ModelId::DeepSeekV4Pro.non_reasoning_variant(), None);
    assert_eq!(ModelId::XaiGrok46.non_reasoning_variant(), Some(ModelId::XaiGrokBuild01));
    assert!(ModelId::GPT56Sol.supports_shell_tool());
    assert!(ModelId::GPT56Sol.supports_shell_tool());
    assert!(!ModelId::GPT56Sol.supports_apply_patch_tool());
    assert!(Provider::XAI.supports_reasoning_effort(models::xai::GROK_4_6));
    assert!(ModelId::XaiGrok46.is_reasoning_variant());
}

#[test]
fn catalog_reasoning_does_not_imply_configurable_effort() {
    assert!(Provider::OpenRouter.supports_reasoning("meta/muse-spark-1.2"));
    assert!(!Provider::OpenRouter.supports_reasoning_effort("meta/muse-spark-1.2"));
    assert!(
        Provider::OpenRouter
            .supported_reasoning_efforts("meta/muse-spark-1.2")
            .is_empty()
    );
}

#[test]
fn namespaced_evolink_models_use_upstream_catalog_metadata() {
    let entry = model_catalog_entry("evolink", "evolink/deepseek-v4-pro").expect("Evolink catalog entry");
    assert_eq!(entry.context_window, 163_840);
    assert!(entry.reasoning);
    assert_eq!(entry.reasoning_efforts, &["low", "medium", "high"]);
}

#[test]
fn test_generated_model_capability_lookup() {
    let gpt54_catalog = model_catalog_entry("openai", "gpt-5.6-sol").expect("gpt-5.4 metadata");
    assert_eq!(gpt54_catalog.context_window, 1_050_000);
    assert!(gpt54_catalog.tool_call);
    assert_eq!(gpt54_catalog.input_modalities, &["text", "image"]);
    let gpt55_catalog = model_catalog_entry("openai", "gpt-5.6-sol").expect("gpt-5.5 metadata");
    assert_eq!(gpt55_catalog.context_window, 1_050_000);
    assert!(gpt55_catalog.tool_call);
    assert_eq!(gpt55_catalog.input_modalities, &["text", "image"]);

    let gemini_catalog = model_catalog_entry("google", "gemini-3.7-flash").expect("gemini-3.7-flash metadata");
    assert_eq!(gemini_catalog.provider, "gemini");
    assert_eq!(gemini_catalog.context_window, 1_048_576);

    let nvidia_catalog = model_catalog_entry("nvidia", models::nvidia::DEFAULT_MODEL).expect("NVIDIA metadata");
    assert_eq!(nvidia_catalog.context_window, 1_000_000);
    assert!(nvidia_catalog.reasoning);
    assert!(nvidia_catalog.tool_call);
    assert!(catalog_provider_keys().contains(&"nvidia"));
    let merge_gateway_catalog =
        model_catalog_entry("merge-gateway", models::merge_gateway::DEFAULT_ROUTING).expect("Merge metadata");
    assert_eq!(merge_gateway_catalog.context_window, 128_000);
    assert!(!merge_gateway_catalog.reasoning);
    assert!(merge_gateway_catalog.tool_call);
    assert!(catalog_provider_keys().contains(&"merge-gateway"));
    let merge_gateway_gemini =
        model_catalog_entry("merge-gateway", models::merge_gateway::GOOGLE_GEMINI_3_7_FLASH).expect("Gemini metadata");
    assert_eq!(merge_gateway_gemini.context_window, 1_000_000);
    assert!(merge_gateway_gemini.vision);
    let merge_gateway_gpt =
        model_catalog_entry("merge-gateway", models::merge_gateway::OPENAI_GPT_5_6_SOL).expect("GPT metadata");
    assert_eq!(merge_gateway_gpt.context_window, 1_100_000);
    assert!(merge_gateway_gpt.vision);
    let ollama_cloud_catalog =
        model_catalog_entry("ollama", models::ollama::DEEPSEEK_V4_FLASH_CLOUD).expect("Ollama Cloud metadata");
    assert_eq!(ollama_cloud_catalog.context_window, 1_000_000);

    let xai_catalog = model_catalog_entry("xai", models::xai::GROK_4_6).expect("grok-4.6 metadata");
    assert_eq!(xai_catalog.context_window, 500_000);
    assert!(xai_catalog.reasoning);
    assert!(xai_catalog.tool_call);
    assert_eq!(xai_catalog.input_modalities, &["text", "image"]);

    let openai_models = supported_models_for_provider("openai").expect("openai models");
    assert!(openai_models.contains(&models::GPT_5_6_SOL));
    assert!(catalog_provider_keys().contains(&"openai"));
    let openrouter_models = supported_models_for_provider("openrouter").expect("openrouter models");
    assert!(openrouter_models.contains(&"openai/gpt-5"));
    let meta_models = supported_models_for_provider("meta-ai").expect("Meta AI models");
    assert!(meta_models.contains(&models::meta::MUSE_SPARK_1_2));
    let _opencode_zen_models = supported_models_for_provider("opencode-zen");
    let opencode_go_models = supported_models_for_provider("opencode-go").expect("opencode go models");
    assert!(opencode_go_models.contains(&models::opencode_go::MINIMAX_M3));
    let xai_models = supported_models_for_provider("xai").expect("xai models");
    assert!(xai_models.contains(&models::xai::GROK_4_6));
    assert!(catalog_provider_keys().contains(&"xai"));

    assert_eq!(ModelId::GPT56Sol.input_modalities(), &["text", "image"]);
    assert_eq!(ModelId::GPT56Sol.input_modalities(), &["text", "image"]);
    assert_eq!(ModelId::Gemini37Flash.input_modalities(), &["text", "image", "video", "audio", "pdf"]);
    assert_eq!(ModelId::ClaudeOpus5.input_modalities(), &["text", "image"]);
    assert_eq!(ModelId::OpenRouterOpenAIGpt5Chat.input_modalities(), &["file", "image", "text"]);

    assert!(ModelId::GPT56Sol.supports_tool_calls());
    assert!(ModelId::GPT56Sol.supports_tool_calls());
    assert!(ModelId::Gemini37Flash.supports_tool_calls());
    assert!(!ModelId::OpenRouterOpenAIGpt5Chat.supports_tool_calls());
}

#[test]
fn test_gpt_5_5_dated_alias_round_trips_to_gpt55_capabilities() {
    assert_eq!(ModelId::from_str(models::openai::GPT_5_6_SOL).unwrap(), ModelId::GPT56Sol);
    assert_eq!(ModelId::GPT56Sol.input_modalities(), &["text", "image"]);
    assert!(
        models::openai::RESPONSES_API_MODELS.contains(&models::openai::GPT_5_6_SOL),
        "dated GPT-5.5 alias should stay on the Responses API path"
    );
    assert!(
        Provider::OpenAI.supports_reasoning_effort(models::openai::GPT_5_6_SOL),
        "dated GPT-5.5 alias should inherit reasoning-effort support"
    );
    assert!(
        Provider::OpenAI.supports_service_tier(models::openai::GPT_5_6_SOL),
        "dated GPT-5.5 alias should inherit service-tier support"
    );
}

#[test]
fn test_model_helpers_include_curated_opencode_models() {
    let zen_models = model_helpers::supported_for("opencode-zen").expect("opencode zen helpers");
    assert!(zen_models.contains(&models::opencode_zen::GPT_5_6_SOL));
    assert!(zen_models.contains(&models::opencode_zen::CLAUDE_SONNET_5));
    assert!(!zen_models.contains(&models::opencode_zen::GPT_5_6));
    assert_eq!(model_helpers::default_for("opencode-zen"), Some(models::opencode_zen::DEFAULT_MODEL));

    let go_models = model_helpers::supported_for("opencode-go").expect("opencode go helpers");
    assert!(go_models.contains(&models::opencode_go::MINIMAX_M3));
    assert!(go_models.contains(&models::opencode_go::GLM_5_2));
    assert_eq!(model_helpers::default_for("opencode-go"), Some(models::opencode_go::DEFAULT_MODEL));
    assert!(model_helpers::is_valid("merge-gateway", "deepseek/deepseek-v4-pro"));
    assert!(!model_helpers::is_valid("merge-gateway", "   "));
}

#[test]
fn test_enum_variants_match_all_models_collection() {
    let src = include_str!("model_id.rs");
    let mut in_enum = false;
    let mut enum_variants = std::collections::BTreeSet::new();

    for raw in src.lines() {
        let line = raw.trim();
        if line.starts_with("pub enum ModelId") {
            in_enum = true;
            continue;
        }
        if in_enum && line.starts_with('}') {
            break;
        }
        if !in_enum || line.is_empty() || line.starts_with("//") || line.starts_with("///") || line.starts_with("#[") {
            continue;
        }
        if let Some((name, _)) = line.split_once(',') {
            let variant_name = name.trim().to_string();
            // Custom is a runtime-only variant not included in all_models()
            if !variant_name.starts_with("Custom") {
                enum_variants.insert(variant_name);
            }
        }
    }

    let all_models_vec = ModelId::all_models();
    let all_models: std::collections::BTreeSet<String> =
        all_models_vec.iter().map(|model| format!("{model:?}")).collect();

    assert_eq!(all_models_vec.len(), all_models.len(), "all_models should not contain duplicate variants");
    let only_in_all: Vec<_> = all_models.difference(&enum_variants).cloned().collect();
    let only_in_enum: Vec<_> = enum_variants.difference(&all_models).cloned().collect();
    assert!(
        all_models == enum_variants,
        "all_models and enum_variants should match. only_in_all={:?} only_in_enum={:?} all_models={:?} enum_variants={:?}",
        only_in_all,
        only_in_enum,
        all_models,
        enum_variants
    );
}

#[test]
fn test_all_models_have_non_empty_metadata_and_parse() {
    for model in ModelId::all_models() {
        assert!(!model.as_str().is_empty());
        assert!(!model.display_name().is_empty());
        assert!(!model.description().is_empty());
        assert!(!model.generation().is_empty());
        let parsed = match model {
            ModelId::OpenCodeGoGlm52 => ModelId::from_str("opencode-go/glm-5.2"),
            ModelId::OpenCodeGoKimiK27Code => ModelId::from_str("opencode-go/kimi-k2.7-code"),
            ModelId::OpenCodeGoMimoV25 => ModelId::from_str("opencode-go/mimo-v2.5"),
            ModelId::OpenCodeGoMimoV25Pro => ModelId::from_str("opencode-go/mimo-v2.5-pro"),
            ModelId::OpenCodeGoMinimaxM3 => ModelId::from_str("opencode-go/minimax-m3"),
            ModelId::OpenCodeGoQwen37Max => ModelId::from_str("opencode-go/qwen3.7-max"),
            ModelId::OpenCodeGoQwen37Plus => ModelId::from_str("opencode-go/qwen3.7-plus"),
            ModelId::OpenCodeGoQwen36Plus => ModelId::from_str("opencode-go/qwen3.6-plus"),
            ModelId::OpenCodeGoDeepseekV4Pro => ModelId::from_str("opencode-go/deepseek-v4-pro"),
            ModelId::OpenCodeGoDeepseekV4Flash => ModelId::from_str("opencode-go/deepseek-v4-flash"),
            // Qwen third-party variants share model strings with their native providers;
            // `deepseek-v4-flash`, `deepseek-v4-pro` resolve to native variants.
            ModelId::QwenDeepSeekV4Flash | ModelId::QwenDeepSeekV4Pro => {
                continue;
            }
            // LlamaCpp/Ollama GPT-OSS-20B share the same model string as OpenAI's variant;
            // `gpt-oss-20b` resolves to OpenAIGptOss20b first.
            ModelId::LlamaCppGptOss20b | ModelId::OllamaGptOss20b => continue,
            // GLM-5.2 is shared with OpenRouter; bare parsing preserves the
            // existing OpenRouter precedence.
            ModelId::NvidiaZaiGlm52 => continue,
            // OpenCode Go routes Grok 4.5 through a prefix, while the bare
            // model id remains owned by xAI.
            ModelId::OpenCodeGoGlm53
            | ModelId::OpenCodeGoGpt56Luna
            | ModelId::OpenCodeGoKimiK3
            | ModelId::OpenCodeGoMuseSpark12Contributor
            | ModelId::OpenCodeGoQwen38Max
            | ModelId::OpenCodeGoHy3 => continue,
            // Merge Gateway deliberately reuses upstream provider/model ids;
            // bare parsing preserves OpenRouter precedence for overlapping ids.
            ModelId::MergeGatewayOpenAIGpt55
            | ModelId::MergeGatewayAnthropicClaudeOpus5
            | ModelId::MergeGatewayGoogleGemini36Flash
            | ModelId::MergeGatewayGoogleGemini37Flash
            | ModelId::MergeGatewayGoogleGemini38Flash
            | ModelId::MergeGatewayMetaMuseSpark13
            | ModelId::MergeGatewayOpenAIGpt6Astra => continue,
            // Vercel AI Gateway reuses `vendor/model` ids that overlap with
            // OpenRouter/Merge Gateway; bare parsing preserves the existing
            // precedence and explicit provider configuration selects Vercel.
            ModelId::VercelAnthropicClaudeSonnet5
            | ModelId::VercelAnthropicClaudeOpus5
            | ModelId::VercelOpenAiGpt6Astra
            | ModelId::VercelOpenAiGpt56Sol
            | ModelId::VercelOpenAiGpt56Luna
            | ModelId::VercelGoogleGemini38Flash
            | ModelId::VercelDeepseekV4Pro
            | ModelId::VercelDeepseekV4Flash
            | ModelId::VercelMoonshotaiKimiK3
            | ModelId::VercelMoonshotaiKimiK27Code => continue,
            _ => ModelId::from_str(&model.as_str()),
        };
        assert_eq!(parsed.unwrap(), model);
    }
}

#[test]
fn from_config_accepts_custom_provider_model() {
    let custom = crate::core::CustomProviderConfig {
        name: "zen-free".to_string(),
        display_name: "Zen Free".to_string(),
        base_url: "https://opencode.ai/zen/v1".to_string(),
        context_window: None,
        api_key_env: "OPENCODE_API_KEY".to_string(),
        auth: None,
        model: "".to_string(),
        models: vec!["deepseek-v4-flash-free".to_string(), "x-preview-f-free".to_string()],
        ..crate::core::CustomProviderConfig::default()
    };

    let parsed =
        ModelId::from_config("x-preview-f-free", "zen-free", &Default::default(), std::slice::from_ref(&custom))
            .unwrap();
    assert_eq!(parsed, ModelId::Custom("zen-free".to_string(), "x-preview-f-free".to_string()));
    assert_eq!(parsed.as_str(), "x-preview-f-free");

    // Case-insensitive provider matching.
    let parsed = ModelId::from_config("x-preview-f-free", "Zen-Free", &Default::default(), &[custom]).unwrap();
    assert_eq!(parsed, ModelId::Custom("zen-free".to_string(), "x-preview-f-free".to_string()));
}

#[test]
fn from_config_accepts_provider_override_model() {
    let mut overrides = std::collections::BTreeMap::new();
    overrides.insert(
        "opencode-zen".to_string(),
        crate::core::ProviderOverrideConfig {
            models: vec!["my-fine-tuned-gpt".to_string()],
            base_url: None,
            api_key_env: None,
        },
    );

    let parsed = ModelId::from_config("my-fine-tuned-gpt", "opencode-zen", &overrides, &[]).unwrap();
    assert_eq!(parsed, ModelId::Custom("opencode-zen".to_string(), "my-fine-tuned-gpt".to_string()));
}

#[test]
fn from_config_rejects_custom_model_for_unrelated_provider() {
    let custom = crate::core::CustomProviderConfig {
        name: "zen-free".to_string(),
        display_name: "Zen Free".to_string(),
        base_url: "https://opencode.ai/zen/v1".to_string(),
        context_window: None,
        api_key_env: "OPENCODE_API_KEY".to_string(),
        auth: None,
        model: "".to_string(),
        models: vec!["x-preview-f-free".to_string()],
        ..crate::core::CustomProviderConfig::default()
    };

    // The model exists for zen-free but not for openai: standard parsing must fail.
    assert!(ModelId::from_config("x-preview-f-free", "openai", &Default::default(), &[custom]).is_err());
}

#[test]
fn from_config_falls_through_to_catalog_for_known_models() {
    let parsed = ModelId::from_config("gpt-5.6-sol", "openai", &Default::default(), &[]).unwrap();
    assert_eq!(parsed, ModelId::GPT56Sol);
}

#[test]
fn catalog_fallbacks_preserve_provider_and_are_acyclic() {
    for model in ModelId::all_models() {
        let mut seen = vec![model.clone()];
        let mut current = model.clone();
        while let Some(next) = current.preferred_lightweight_variant() {
            assert_eq!(next.provider(), model.provider());
            assert!(!seen.contains(&next), "fallback cycle for {model}");
            seen.push(next.clone());
            current = next;
        }
    }
}

#[test]
fn missing_catalog_routes_have_no_speculative_fallback_or_tier() {
    for model in [
        ModelId::CopilotGPT52Codex,
        ModelId::CopilotGPT54,
        ModelId::CopilotClaudeSonnet46,
        ModelId::EvolinkDeepseekV4Pro,
        ModelId::MoonshotKimiK3,
        ModelId::MoonshotKimiK27Code,
        ModelId::PoolsideLagunaM1,
        ModelId::PoolsideLagunaS21,
    ] {
        assert!(!model.is_pro_variant(), "{model}");
        assert!(model.preferred_lightweight_variant().is_none(), "{model}");
    }
}
