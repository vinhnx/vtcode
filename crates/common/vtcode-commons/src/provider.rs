//! Provider enum and core definitions shared across VT Code crates.
//!
//! This module provides the [`Provider`] enum and its pure methods. Methods
//! that depend on model catalogs or vtcode-config internals
//! (`supports_reasoning_effort`, `supports_service_tier`) remain in
//! `vtcode-config` as extension methods.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Error returned when parsing a provider string fails.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderParseError {
    #[error("Invalid provider: '{}'. Supported providers: {}", .0, crate::provider::Provider::all_providers().iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", "))]
    InvalidProvider(String),
}

/// Supported AI model providers
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Provider {
    /// Google Gemini models
    Gemini,
    /// OpenAI GPT models
    #[default]
    OpenAI,
    /// Anthropic Claude models
    Anthropic,
    /// GitHub Copilot preview integration
    Copilot,
    /// DeepSeek native models
    DeepSeek,
    /// Official Meta AI Muse models
    Meta,
    /// OpenRouter marketplace models
    OpenRouter,
    /// Local Ollama models
    Ollama,
    /// Ollama Cloud hosted models (ollama.com)
    OllamaCloud,
    /// LM Studio local models
    LmStudio,
    /// llama.cpp local models
    LlamaCpp,
    /// Moonshot.ai models
    Moonshot,
    /// Z.AI GLM models
    ZAI,
    /// MiniMax models
    Minimax,
    /// Xiaomi MiMo models
    MiMo,
    /// Mistral AI models
    Mistral,
    /// Hugging Face Inference Providers
    HuggingFace,
    /// OpenCode Zen gateway (pay-as-you-go)
    OpenCodeZen,
    /// OpenCode Go subscription
    OpenCodeGo,
    /// Alibaba Cloud Qwen models
    Qwen,
    /// StepFun models
    StepFun,
    /// Evolink multi-model gateway
    Evolink,
    /// Poolside AI models
    Poolside,
    /// xAI Grok models
    XAI,
    /// NVIDIA NIM OpenAI-compatible models
    NVIDIA,
    /// Merge Gateway multi-provider routing gateway
    MergeGateway,
    /// Vercel AI Gateway multi-provider routing gateway
    Vercel,
}

impl Provider {
    /// Get the default API key environment variable for this provider
    pub fn default_api_key_env(&self) -> &'static str {
        match self {
            Provider::Gemini => "GEMINI_API_KEY",
            Provider::OpenAI => "OPENAI_API_KEY",
            Provider::Anthropic => "ANTHROPIC_API_KEY",
            Provider::Copilot => "",
            Provider::DeepSeek => "DEEPSEEK_API_KEY",
            Provider::Meta => "META_API_KEY",
            Provider::OpenRouter => "OPENROUTER_API_KEY",
            Provider::Ollama => "OLLAMA_API_KEY",
            Provider::OllamaCloud => "OLLAMA_API_KEY",
            Provider::LmStudio => "LMSTUDIO_API_KEY",
            Provider::LlamaCpp => "LLAMACPP_API_KEY",
            Provider::Moonshot => "MOONSHOT_API_KEY",
            Provider::ZAI => "ZAI_API_KEY",
            Provider::Minimax => "MINIMAX_API_KEY",
            Provider::MiMo => "MIMO_API_KEY",
            Provider::Mistral => "MISTRAL_API_KEY",
            Provider::HuggingFace => "HF_TOKEN",
            Provider::OpenCodeZen => "OPENCODE_ZEN_API_KEY",
            Provider::OpenCodeGo => "OPENCODE_GO_API_KEY",
            Provider::Qwen => "QWEN_API_KEY",
            Provider::StepFun => "STEPFUN_API_KEY",
            Provider::Evolink => "EVOLINK_API_KEY",
            Provider::Poolside => "POOLSIDE_API_KEY",
            Provider::XAI => "XAI_API_KEY",
            Provider::NVIDIA => "NVIDIA_API_KEY",
            Provider::MergeGateway => "MERGE_GATEWAY_API_KEY",
            Provider::Vercel => "AI_GATEWAY_API_KEY",
        }
    }

    /// Get all supported providers
    pub fn all_providers() -> Vec<Provider> {
        vec![
            Provider::OpenAI,
            Provider::Anthropic,
            Provider::Copilot,
            Provider::Minimax,
            Provider::MiMo,
            Provider::Mistral,
            Provider::Gemini,
            Provider::DeepSeek,
            Provider::Meta,
            Provider::HuggingFace,
            Provider::OpenRouter,
            Provider::Ollama,
            Provider::OllamaCloud,
            Provider::LmStudio,
            Provider::LlamaCpp,
            Provider::Moonshot,
            Provider::ZAI,
            Provider::OpenCodeZen,
            Provider::OpenCodeGo,
            Provider::Qwen,
            Provider::StepFun,
            Provider::Evolink,
            Provider::Poolside,
            Provider::XAI,
            Provider::NVIDIA,
            Provider::MergeGateway,
            Provider::Vercel,
        ]
    }

    /// Human-friendly label for display purposes
    pub fn label(&self) -> &'static str {
        match self {
            Provider::Gemini => "Gemini",
            Provider::OpenAI => "OpenAI",
            Provider::Anthropic => "Anthropic",
            Provider::Copilot => "GitHub Copilot",
            Provider::DeepSeek => "DeepSeek",
            Provider::Meta => "Meta AI",
            Provider::OpenRouter => "OpenRouter",
            Provider::Ollama => "Ollama",
            Provider::OllamaCloud => "Ollama Cloud",
            Provider::LmStudio => "LM Studio",
            Provider::LlamaCpp => "llama.cpp",
            Provider::Moonshot => "Moonshot",
            Provider::ZAI => "Z.AI",
            Provider::Minimax => "MiniMax",
            Provider::MiMo => "Xiaomi MiMo",
            Provider::Mistral => "Mistral",
            Provider::HuggingFace => "Hugging Face",
            Provider::OpenCodeZen => "OpenCode Zen",
            Provider::OpenCodeGo => "OpenCode Go",
            Provider::Qwen => "Qwen",
            Provider::StepFun => "StepFun",
            Provider::Evolink => "Evolink",
            Provider::Poolside => "Poolside",
            Provider::XAI => "xAI",
            Provider::NVIDIA => "NVIDIA",
            Provider::MergeGateway => "Merge Gateway",
            Provider::Vercel => "Vercel AI Gateway",
        }
    }

    pub fn is_dynamic(&self) -> bool {
        matches!(self, Provider::Copilot) || self.is_local()
    }

    pub fn is_local(&self) -> bool {
        matches!(self, Provider::Ollama | Provider::LmStudio | Provider::LlamaCpp)
    }

    pub fn local_install_instructions(&self) -> Option<&'static str> {
        match self {
            Provider::Ollama => Some(
                "Ollama server is not running. To start:\n  1. Install Ollama from https://ollama.com\n  2. Run 'ollama serve' in a terminal\n  3. Pull models using 'ollama pull <model-name>' (e.g., 'ollama pull gpt-oss:20b')",
            ),
            Provider::LmStudio => Some(
                "LM Studio server is not running. To start:\n  1. Install LM Studio from https://lmstudio.ai\n  2. Open LM Studio and start the Local Server on port 1234\n  3. Load the model you want to use",
            ),
            Provider::LlamaCpp => Some(
                "llama.cpp server is not running. To start:\n  1. Install llama.cpp from https://llama.app or your package manager\n  2. Run 'llama-server -m /path/to/model.gguf --port 8080'\n  3. Keep the server running while VT Code connects",
            ),
            _ => None,
        }
    }

    pub fn uses_managed_auth(&self) -> bool {
        matches!(self, Provider::Copilot)
    }

    pub fn platform_url(&self) -> Option<&'static str> {
        match self {
            Provider::Gemini => Some("https://aistudio.google.com/app/apikey"),
            Provider::OpenAI => Some("https://platform.openai.com/api-keys"),
            Provider::Anthropic => Some("https://console.anthropic.com"),
            Provider::Copilot => None,
            Provider::DeepSeek => Some("https://platform.deepseek.com"),
            Provider::Meta => Some("https://developer.meta.com/ai/models"),
            Provider::OpenRouter => Some("https://openrouter.ai/api/v1/auth/keys"),
            Provider::Ollama => None,
            Provider::OllamaCloud => Some("https://ollama.com"),
            Provider::LmStudio => None,
            Provider::LlamaCpp => None,
            Provider::Moonshot => Some("https://platform.kimi.ai/console/api-keys"),
            Provider::ZAI => Some("https://z.ai/docs"),
            Provider::Minimax => Some("https://platform.minimax.io/docs"),
            Provider::MiMo => Some("https://platform.xiaomimimo.com/docs/en-US/welcome"),
            Provider::Mistral => Some("https://console.mistral.ai"),
            Provider::HuggingFace => Some("https://huggingface.co/settings/tokens"),
            Provider::OpenCodeZen => Some("https://opencode.ai/docs/zen/"),
            Provider::OpenCodeGo => Some("https://opencode.ai/docs/go/"),
            Provider::Qwen => Some("https://dashscope.console.aliyun.com"),
            Provider::StepFun => Some("https://platform.stepfun.com"),
            Provider::Evolink => Some("https://evolink.ai/dashboard/keys"),
            Provider::Poolside => Some("https://poolside.ai"),
            Provider::XAI => Some("https://docs.x.ai"),
            Provider::NVIDIA => Some("https://build.nvidia.com"),
            Provider::MergeGateway => Some("https://dashboard.merge.dev"),
            Provider::Vercel => Some("https://vercel.com/docs/ai-gateway/getting-started"),
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Provider::Gemini => write!(f, "gemini"),
            Provider::OpenAI => write!(f, "openai"),
            Provider::Anthropic => write!(f, "anthropic"),
            Provider::Copilot => write!(f, "copilot"),
            Provider::DeepSeek => write!(f, "deepseek"),
            Provider::Meta => write!(f, "meta"),
            Provider::OpenRouter => write!(f, "openrouter"),
            Provider::Ollama => write!(f, "ollama"),
            Provider::OllamaCloud => write!(f, "ollama-cloud"),
            Provider::LmStudio => write!(f, "lmstudio"),
            Provider::LlamaCpp => write!(f, "llamacpp"),
            Provider::Moonshot => write!(f, "moonshot"),
            Provider::ZAI => write!(f, "zai"),
            Provider::Minimax => write!(f, "minimax"),
            Provider::MiMo => write!(f, "mimo"),
            Provider::Mistral => write!(f, "mistral"),
            Provider::HuggingFace => write!(f, "huggingface"),
            Provider::OpenCodeZen => write!(f, "opencode-zen"),
            Provider::OpenCodeGo => write!(f, "opencode-go"),
            Provider::Qwen => write!(f, "qwen"),
            Provider::StepFun => write!(f, "stepfun"),
            Provider::Evolink => write!(f, "evolink"),
            Provider::Poolside => write!(f, "poolside"),
            Provider::XAI => write!(f, "xai"),
            Provider::NVIDIA => write!(f, "nvidia"),
            Provider::MergeGateway => write!(f, "merge-gateway"),
            Provider::Vercel => write!(f, "vercel"),
        }
    }
}

impl AsRef<str> for Provider {
    fn as_ref(&self) -> &str {
        match self {
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
}

impl FromStr for Provider {
    type Err = ProviderParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "gemini" | "google" => Ok(Provider::Gemini),
            "openai" => Ok(Provider::OpenAI),
            "anthropic" => Ok(Provider::Anthropic),
            "copilot" => Ok(Provider::Copilot),
            "deepseek" => Ok(Provider::DeepSeek),
            "meta" | "meta-ai" => Ok(Provider::Meta),
            "openrouter" => Ok(Provider::OpenRouter),
            "ollama" => Ok(Provider::Ollama),
            "ollama-cloud" | "ollamacloud" | "ollama_cloud" => Ok(Provider::OllamaCloud),
            "lmstudio" => Ok(Provider::LmStudio),
            "llamacpp" | "llama.cpp" | "llama-cpp" => Ok(Provider::LlamaCpp),
            "moonshot" => Ok(Provider::Moonshot),
            "zai" => Ok(Provider::ZAI),
            "minimax" => Ok(Provider::Minimax),
            "mimo" => Ok(Provider::MiMo),
            "mistral" => Ok(Provider::Mistral),
            "huggingface" => Ok(Provider::HuggingFace),
            "opencode-zen" | "opencodezen" => Ok(Provider::OpenCodeZen),
            "opencode-go" | "opencodego" => Ok(Provider::OpenCodeGo),
            "qwen" => Ok(Provider::Qwen),
            "stepfun" => Ok(Provider::StepFun),
            "evolink" => Ok(Provider::Evolink),
            "poolside" => Ok(Provider::Poolside),
            "xai" => Ok(Provider::XAI),
            "nvidia" | "nvidia-nim" => Ok(Provider::NVIDIA),
            "merge-gateway" | "merge_gateway" | "mergegateway" => Ok(Provider::MergeGateway),
            "vercel" | "vercel-ai-gateway" | "ai-gateway" => Ok(Provider::Vercel),
            _ => Err(ProviderParseError::InvalidProvider(s.to_string())),
        }
    }
}
