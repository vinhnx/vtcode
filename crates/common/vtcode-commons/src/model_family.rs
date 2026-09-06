//! Model family definitions and capability groupings.
//!
//! A model family groups models that share certain characteristics like
//! context windows, supported features, and prompting strategies.

use serde::{Deserialize, Serialize};

use crate::provider::Provider;
use crate::reasoning::ReasoningEffortLevel;

/// Default context window for most models
const DEFAULT_CONTEXT_WINDOW: i64 = 128_000;

/// Large context window (for models like Gemini)
const LARGE_CONTEXT_WINDOW: i64 = 1_048_576;

/// Medium context window
const MEDIUM_CONTEXT_WINDOW: i64 = 200_000;

/// Shell tool type preference for a model family
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum ShellToolType {
    /// Use default shell tool behavior
    #[default]
    Default,
    /// Use shell command tool
    ShellCommand,
    /// Use local shell execution
    Local,
    /// Use Codex exec_command pattern (Codex-style)
    ExecCommand,
}

/// Truncation policy for model output
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TruncationPolicy {
    /// Truncate by byte count
    Bytes(usize),
    /// Truncate by token count
    Tokens(usize),
    /// No truncation
    None,
}

impl Default for TruncationPolicy {
    fn default() -> Self {
        TruncationPolicy::Bytes(10_000)
    }
}

/// A model family groups models that share certain characteristics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelFamily {
    /// The full model slug used to derive this model family
    slug: String,

    /// The model family name (e.g., "gemini-2.5", "claude-opus")
    pub family: String,

    /// The provider this model belongs to
    pub provider: Provider,

    /// Maximum supported context window, if known
    context_window: Option<i64>,

    /// Optional legacy token threshold for automatic compaction. Runtime
    /// compaction uses the resolved provider/session budget instead of
    /// deriving a percentage from `context_window`.
    auto_compact_token_limit: Option<i64>,

    /// Whether the model supports reasoning summaries
    pub supports_reasoning_summaries: bool,

    /// Default reasoning effort for this model family
    default_reasoning_effort: Option<ReasoningEffortLevel>,

    /// Whether the model supports parallel tool calls
    supports_parallel_tool_calls: bool,

    /// Whether the model needs special apply_patch instructions
    needs_special_apply_patch_instructions: bool,

    /// Preferred shell tool type for this model family
    shell_type: ShellToolType,

    /// Truncation policy for model output
    truncation_policy: TruncationPolicy,

    /// Names of experimental tools supported by this model family
    experimental_supported_tools: Vec<String>,

    /// Percentage of context window considered usable for inputs
    effective_context_window_percent: i64,

    /// Whether the model supports verbosity settings
    support_verbosity: bool,

    /// Whether the model supports tool use
    supports_tool_use: bool,

    /// Whether the model supports streaming
    supports_streaming: bool,

    /// Whether the model supports thinking/reasoning output
    supports_thinking: bool,
}

impl Default for ModelFamily {
    fn default() -> Self {
        Self {
            slug: String::new(),
            family: String::new(),
            provider: Provider::default(),
            context_window: Some(DEFAULT_CONTEXT_WINDOW),
            auto_compact_token_limit: None,
            supports_reasoning_summaries: false,
            default_reasoning_effort: None,
            supports_parallel_tool_calls: false,
            needs_special_apply_patch_instructions: false,
            shell_type: ShellToolType::Default,
            truncation_policy: TruncationPolicy::default(),
            experimental_supported_tools: Vec::new(),
            effective_context_window_percent: 95,
            support_verbosity: false,
            supports_tool_use: true,
            supports_streaming: true,
            supports_thinking: false,
        }
    }
}

impl ModelFamily {
    /// Create a new model family with the given slug
    fn new(slug: impl Into<String>, family: impl Into<String>, provider: Provider) -> Self {
        Self {
            slug: slug.into(),
            family: family.into(),
            provider,
            ..Default::default()
        }
    }

    /// Get the explicitly configured legacy auto-compact token limit.
    fn auto_compact_token_limit(&self) -> Option<i64> {
        self.auto_compact_token_limit
    }

    /// Get the model slug
    pub fn get_model_slug(&self) -> &str {
        &self.slug
    }

    /// Check if this family supports a specific feature
    fn supports_feature(&self, feature: &str) -> bool {
        match feature {
            "reasoning" | "thinking" => self.supports_thinking,
            "tool_use" | "tools" => self.supports_tool_use,
            "streaming" => self.supports_streaming,
            "parallel_tools" => self.supports_parallel_tool_calls,
            _ => self.experimental_supported_tools.contains(&feature.to_string()),
        }
    }
}

/// Macro to simplify model family definitions
#[macro_export]
macro_rules! model_family {
    (
        $slug:expr, $family:expr, $provider:expr $(, $key:ident : $value:expr )* $(,)?
    ) => {{
        let mut mf = $crate::model_family::ModelFamily::new($slug, $family, $provider);
        $(
            mf.$key = $value;
        )*
        mf
    }};
}

/// Internal helper that returns a `ModelFamily` for the given model slug.
pub fn find_family_for_model(slug: &str) -> ModelFamily {
    if let Some((provider, raw_slug)) = opencode_provider_and_raw_slug(slug) {
        let mut family = find_family_for_model(raw_slug);
        family.slug = slug.to_string();
        family.provider = provider;
        return family;
    }

    // Gemini models
    if slug.starts_with("gemini-3") {
        return model_family!(
            slug, "gemini-3", Provider::Gemini,
            context_window: Some(LARGE_CONTEXT_WINDOW),
            supports_thinking: true,
            supports_parallel_tool_calls: true,
            supports_reasoning_summaries: true,
        );
    }
    if slug.starts_with("gemini") {
        return model_family!(
            slug, "gemini", Provider::Gemini,
            context_window: Some(LARGE_CONTEXT_WINDOW),
        );
    }

    // OpenAI models
    if slug.starts_with("gpt-6") {
        return model_family!(
            slug, "gpt-6", Provider::OpenAI,
            context_window: Some(LARGE_CONTEXT_WINDOW),
            supports_thinking: true,
            supports_parallel_tool_calls: true,
        );
    }
    if slug.starts_with("gpt-5") {
        return model_family!(
            slug, "gpt-5", Provider::OpenAI,
            context_window: Some(DEFAULT_CONTEXT_WINDOW),
            supports_thinking: true,
            supports_parallel_tool_calls: true,
        );
    }
    if slug.starts_with("codex") {
        return model_family!(
            slug, "codex", Provider::OpenAI,
            context_window: Some(MEDIUM_CONTEXT_WINDOW),
            supports_thinking: true,
            shell_type: ShellToolType::ExecCommand,
        );
    }
    if slug.starts_with("gpt-oss") || slug.contains("gpt-oss") {
        return model_family!(
            slug, "gpt-oss", Provider::OpenAI,
            context_window: Some(96_000),
        );
    }
    if slug.starts_with("o3") || slug.starts_with("o4") {
        return model_family!(
            slug, "o-series", Provider::OpenAI,
            context_window: Some(MEDIUM_CONTEXT_WINDOW),
            supports_thinking: true,
            supports_reasoning_summaries: true,
            needs_special_apply_patch_instructions: true,
        );
    }

    // Anthropic models
    if slug.starts_with("claude-opus") || slug.contains("opus") {
        return model_family!(
            slug, "claude-opus", Provider::Anthropic,
            context_window: Some(MEDIUM_CONTEXT_WINDOW),
            supports_thinking: true,
            supports_parallel_tool_calls: true,
        );
    }
    if slug.starts_with("claude-sonnet") || slug.contains("sonnet") {
        return model_family!(
            slug, "claude-sonnet", Provider::Anthropic,
            context_window: Some(MEDIUM_CONTEXT_WINDOW),
            supports_thinking: true,
        );
    }
    if slug.starts_with("claude-haiku") || slug.contains("haiku") {
        return model_family!(
            slug, "claude-haiku", Provider::Anthropic,
            context_window: Some(MEDIUM_CONTEXT_WINDOW),
        );
    }
    if slug.starts_with("claude") {
        return model_family!(
            slug, "claude", Provider::Anthropic,
            context_window: Some(MEDIUM_CONTEXT_WINDOW),
        );
    }

    // DeepSeek models
    if slug.contains("deepseek") && slug.contains("reason") {
        return model_family!(
            slug, "deepseek-reasoner", Provider::DeepSeek,
            context_window: Some(DEFAULT_CONTEXT_WINDOW),
            supports_thinking: true,
        );
    }
    if slug.contains("deepseek") {
        return model_family!(
            slug, "deepseek", Provider::DeepSeek,
            context_window: Some(DEFAULT_CONTEXT_WINDOW),
        );
    }

    // Official Meta AI Muse models
    if slug.starts_with("muse-spark-") {
        return model_family!(
            slug, "muse-spark", Provider::Meta,
            context_window: Some(LARGE_CONTEXT_WINDOW),
            supports_thinking: true,
            supports_parallel_tool_calls: true,
            supports_reasoning_summaries: false,
        );
    }

    // Z.AI GLM models
    if slug.contains("glm-5") {
        return model_family!(
            slug, "glm-5", Provider::ZAI,
            context_window: Some(DEFAULT_CONTEXT_WINDOW),
            supports_thinking: true,
        );
    }
    if slug.contains("glm") {
        return model_family!(
            slug, "glm", Provider::ZAI,
            context_window: Some(DEFAULT_CONTEXT_WINDOW),
        );
    }

    // MiniMax models
    if slug.contains("minimax") {
        return model_family!(
            slug, "minimax", Provider::Minimax,
            context_window: Some(DEFAULT_CONTEXT_WINDOW),
            supports_thinking: true,
        );
    }

    // Moonshot/Kimi models
    if slug.contains("kimi") || slug.contains("moonshot") {
        return model_family!(
            slug, "kimi", Provider::Moonshot,
            context_window: Some(DEFAULT_CONTEXT_WINDOW),
            supports_thinking: slug.contains("thinking"),
        );
    }

    // Qwen models (via OpenRouter or Ollama)
    if slug.contains("qwen") {
        return model_family!(
            slug, "qwen", Provider::OpenRouter,
            context_window: Some(DEFAULT_CONTEXT_WINDOW),
            supports_thinking: slug.contains("thinking"),
        );
    }

    // Ollama local models
    if slug.starts_with("ollama/") || slug.contains(":") {
        return model_family!(
            slug, "ollama-local", Provider::Ollama,
            context_window: Some(DEFAULT_CONTEXT_WINDOW),
        );
    }

    // OpenRouter models (fallback for unrecognized patterns)
    if slug.contains("/") {
        return model_family!(
            slug, "openrouter", Provider::OpenRouter,
            context_window: Some(DEFAULT_CONTEXT_WINDOW),
        );
    }

    // Default fallback
    model_family!(
        slug, "unknown", Provider::default(),
        context_window: Some(DEFAULT_CONTEXT_WINDOW),
    )
}

fn opencode_provider_and_raw_slug(slug: &str) -> Option<(Provider, &str)> {
    if let Some(raw_slug) = slug.strip_prefix("opencode-go/") {
        Some((Provider::OpenCodeGo, raw_slug))
    } else if let Some(raw_slug) = slug.strip_prefix("opencode/").or_else(|| slug.strip_prefix("opencode-zen/")) {
        Some((Provider::OpenCodeZen, raw_slug))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gemini_family_detection() {
        let family = find_family_for_model("gemini-3-flash-preview");
        assert_eq!(family.family, "gemini-3");
        assert_eq!(family.provider, Provider::Gemini);
        assert!(family.context_window.unwrap() >= LARGE_CONTEXT_WINDOW);
    }

    #[test]
    fn test_gpt5_family_detection() {
        let family = find_family_for_model("gpt-5-codex");
        assert_eq!(family.family, "gpt-5");
        assert_eq!(family.provider, Provider::OpenAI);
        assert!(family.supports_thinking);
    }

    #[test]
    fn test_claude_family_detection() {
        let family = find_family_for_model("claude-opus-4.5");
        assert_eq!(family.family, "claude-opus");
        assert_eq!(family.provider, Provider::Anthropic);
    }

    #[test]
    fn test_opencode_zen_family_detection_preserves_provider() {
        let family = find_family_for_model("opencode/gpt-5.4");
        assert_eq!(family.family, "gpt-5");
        assert_eq!(family.provider, Provider::OpenCodeZen);
        assert!(family.supports_thinking);
    }

    #[test]
    fn test_opencode_go_family_detection_preserves_provider() {
        let family = find_family_for_model("opencode-go/kimi-k2.5");
        assert_eq!(family.family, "kimi");
        assert_eq!(family.provider, Provider::OpenCodeGo);
    }

    #[test]
    fn test_auto_compact_limit_requires_explicit_value() {
        let family = ModelFamily {
            context_window: Some(100_000),
            ..Default::default()
        };
        assert_eq!(family.auto_compact_token_limit(), None);

        let family = ModelFamily {
            context_window: Some(100_000),
            auto_compact_token_limit: Some(90_000),
            ..Default::default()
        };
        assert_eq!(family.auto_compact_token_limit(), Some(90_000));
    }

    #[test]
    fn test_supports_feature() {
        let family = ModelFamily {
            supports_thinking: true,
            supports_tool_use: true,
            ..Default::default()
        };
        assert!(family.supports_feature("thinking"));
        assert!(family.supports_feature("tool_use"));
        assert!(!family.supports_feature("unknown"));
    }
}
