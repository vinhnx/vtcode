use async_stream::try_stream;
use async_trait::async_trait;
use compact_str::format_compact;
use once_cell::sync::Lazy;
use rustc_hash::FxHashMap;
use std::sync::RwLock;
use vtcode_commons::llm::BackendKind;

use vtcode_commons::tool_types::CompactStr;

use super::{
    LLMNormalizedStream, LLMRequest, LLMResponse, LLMStream, LLMStreamEvent, Message, ResponsesCompactionOptions,
    SamplingOverrides,
};
pub use vtcode_commons::llm::{LLMError, LLMErrorMetadata};

/// Generic effort levels supported by providers that advertise configurable
/// reasoning without exposing model-specific catalog metadata.
pub(crate) const GENERIC_REASONING_EFFORTS: &[&str] = &["low", "medium", "high"];

/// Return the catalog's effort metadata while preserving the distinction
/// between an unknown route and a known route that only exposes structured
/// reasoning. An empty catalog list is authoritative: it means the route does
/// not accept a configurable effort payload.
pub(crate) fn catalog_reasoning_efforts(provider: &str, model: &str) -> Option<&'static [&'static str]> {
    vtcode_config::models::model_catalog_entry(provider, model).map(|entry| entry.reasoning_efforts)
}

/// Resolve the catalog's exact effort list while retaining the explicit
/// generic contract for provider routes that advertise effort support without
/// a built-in model entry (for example, an OpenAI-compatible custom endpoint).
pub(crate) fn catalog_or_generic_reasoning_efforts(provider: &str, model: &str) -> &'static [&'static str] {
    catalog_reasoning_efforts(provider, model).unwrap_or(GENERIC_REASONING_EFFORTS)
}

/// Resolve catalog levels while allowing an explicitly configured custom route
/// to advertise the shared generic effort contract. Unsupported unknown routes
/// remain empty so request builders fail closed.
pub(crate) fn catalog_or_explicit_reasoning_efforts(
    provider: &str,
    model: &str,
    explicitly_supported: bool,
) -> &'static [&'static str] {
    catalog_reasoning_efforts(provider, model)
        .or_else(|| explicitly_supported.then_some(GENERIC_REASONING_EFFORTS))
        .unwrap_or(&[])
}

/// Resolve a model's catalog context window with a provider-specific fallback.
///
/// Provider adapters use this helper instead of maintaining independent model
/// name tables. Explicit provider overrides remain the caller's responsibility
/// and are applied before this lookup.
pub(crate) fn catalog_context_window(provider: &str, model: &str, fallback: usize) -> usize {
    vtcode_config::models::model_catalog_entry(provider, model)
        .map(|entry| entry.context_window)
        .filter(|context_window| *context_window > 0)
        .unwrap_or(fallback)
}

/// Cached provider capabilities to reduce repeated trait method calls
#[derive(Debug, Clone)]
pub struct ProviderCapabilities {
    pub(crate) provider_name: String,
    pub(crate) model: String,
    pub streaming: bool,
    pub reasoning: bool,
    pub reasoning_effort: bool,
    pub tools: bool,
    pub parallel_tool_config: bool,
    pub(crate) structured_output: bool,
    pub(crate) context_caching: bool,
    pub responses_compaction: bool,
    pub context_edits: bool,
    /// Whether the selected provider/model can carry Anthropic's native
    /// turn-scoped system-message lifecycle field on the wire.
    pub turn_scoped_system_messages: bool,
    pub(crate) vision: bool,
    pub(crate) context_size: usize,
}

impl ProviderCapabilities {
    fn detect(provider: &dyn LLMProvider, model: &str) -> Self {
        Self {
            provider_name: provider.name().to_string(),
            model: model.to_string(),
            streaming: provider.supports_streaming(),
            reasoning: provider.supports_reasoning(model),
            reasoning_effort: !provider.supported_reasoning_efforts(model).is_empty(),
            tools: provider.supports_tools(model),
            parallel_tool_config: provider.supports_parallel_tool_config(model),
            structured_output: provider.supports_structured_output(model),
            context_caching: provider.supports_context_caching(model),
            responses_compaction: provider.supports_responses_compaction(model),
            context_edits: provider.supports_context_edits(model),
            turn_scoped_system_messages: provider.supports_turn_scoped_system_messages(model),
            vision: provider.supports_vision(model),
            context_size: provider.effective_context_size(model),
        }
    }

    pub(crate) fn has_advanced_features(&self) -> bool {
        self.reasoning || self.structured_output || self.context_caching || self.reasoning_effort
    }

    pub(crate) fn summary(&self) -> String {
        let mut features = Vec::new();

        if self.streaming {
            features.push("streaming");
        }
        if self.reasoning {
            features.push("advanced-reasoning");
        }
        if self.reasoning_effort {
            features.push("reasoning-effort");
        }
        if self.structured_output {
            features.push("structured-output");
        }
        if self.context_caching {
            features.push("context-caching");
        }
        if self.parallel_tool_config {
            features.push("parallel-tools");
        }
        if self.responses_compaction {
            features.push("responses-compaction");
        }
        if self.context_edits {
            features.push("context-edits");
        }

        let features_str = if features.is_empty() {
            "basic".to_string()
        } else {
            features.join(", ")
        };

        format!("{} ({} tokens): {}", self.model, self.context_size, features_str)
    }
}

/// Global cache for provider capabilities (provider_name::model -> capabilities)
static CAPABILITY_CACHE: Lazy<RwLock<FxHashMap<CompactStr, ProviderCapabilities>>> =
    Lazy::new(|| RwLock::new(FxHashMap::default()));

/// Extract and cache provider capabilities for a given provider and model
pub fn get_cached_capabilities(provider: &dyn LLMProvider, model: &str) -> ProviderCapabilities {
    let cache_key = format_compact!("{}::{}::{}", provider.name(), model, provider.effective_context_size(model));

    // Check if already cached
    if let Ok(cache) = CAPABILITY_CACHE.read()
        && let Some(caps) = cache.get(&cache_key)
    {
        return caps.clone();
    }

    // Compute capabilities
    let caps = ProviderCapabilities::detect(provider, model);

    // Cache for future use
    if let Ok(mut cache) = CAPABILITY_CACHE.write() {
        cache.insert(cache_key, caps.clone());
    }

    caps
}

/// Universal LLM provider trait
#[async_trait]
pub trait LLMProvider: Send + Sync {
    /// Provider name (e.g., "gemini", "openai", "anthropic")
    fn name(&self) -> &str;

    /// The canonical backend kind for this provider.
    ///
    /// Defaults to matching on [`name()`](LLMProvider::name) against the
    /// well-known provider names. Providers should override this when their
    /// name does not match the canonical mapping (e.g., dynamic names).
    fn backend_kind(&self) -> BackendKind {
        match self.name() {
            "gemini" => BackendKind::Gemini,
            "openai" => BackendKind::OpenAI,
            "anthropic" => BackendKind::Anthropic,
            "deepseek" => BackendKind::DeepSeek,
            "meta" => BackendKind::Meta,
            "mistral" => BackendKind::Mistral,
            "openrouter" => BackendKind::OpenRouter,
            "ollama" => BackendKind::Ollama,
            "llamacpp" => BackendKind::LlamaCpp,
            "zai" => BackendKind::ZAI,
            "moonshot" => BackendKind::Moonshot,
            "huggingface" => BackendKind::HuggingFace,
            "minimax" => BackendKind::Minimax,
            "mimo" => BackendKind::MiMo,
            "opencode-zen" => BackendKind::OpenCodeZen,
            "opencode-go" => BackendKind::OpenCodeGo,
            "qwen" => BackendKind::Qwen,
            "stepfun" => BackendKind::StepFun,
            "evolink" => BackendKind::Evolink,
            "poolside" => BackendKind::Poolside,
            "nvidia" => BackendKind::Nvidia,
            "merge-gateway" => BackendKind::MergeGateway,
            "vercel" => BackendKind::Vercel,
            _ => BackendKind::OpenAI,
        }
    }

    /// Whether the provider has native streaming support
    fn supports_streaming(&self) -> bool {
        false
    }

    /// Whether the provider can service non-streaming generation requests for the model.
    fn supports_non_streaming(&self, _model: &str) -> bool {
        true
    }

    /// Whether the provider surfaces structured reasoning traces for the given model
    fn supports_reasoning(&self, _model: &str) -> bool {
        false
    }

    /// Whether the provider accepts configurable reasoning effort for the model
    fn supports_reasoning_effort(&self, _model: &str) -> bool {
        false
    }

    /// Exact levels accepted by this route; providers may override catalog metadata.
    fn supported_reasoning_efforts(&self, model: &str) -> &'static [&'static str] {
        if !self.supports_reasoning_effort(model) {
            return &[];
        }
        catalog_or_generic_reasoning_efforts(self.name(), model)
    }

    /// Provider/model-specific sampling parameter overrides.
    ///
    /// Custom-provider profiles may pin exact sampling values per model; every
    /// field defaults to `None`, meaning the agent loop's global config value
    /// applies unchanged.
    fn sampling_overrides(&self, _model: &str) -> SamplingOverrides {
        SamplingOverrides::default()
    }

    /// Whether the provider supports structured tool calling for the given model
    fn supports_tools(&self, _model: &str) -> bool {
        true
    }

    /// Whether the provider understands parallel tool configuration payloads
    fn supports_parallel_tool_config(&self, _model: &str) -> bool {
        false
    }

    /// Whether the provider supports structured output (JSON schema guarantees)
    fn supports_structured_output(&self, _model: &str) -> bool {
        false
    }

    /// Whether the provider supports prompt/context caching
    fn supports_context_caching(&self, _model: &str) -> bool {
        false
    }

    /// Whether the provider supports vision (image analysis) for given model
    fn supports_vision(&self, _model: &str) -> bool {
        false
    }

    /// Whether the provider supports Responses API server-side compaction.
    fn supports_responses_compaction(&self, _model: &str) -> bool {
        false
    }

    /// Whether the request path can enforce a provider-native `allowed_tools` subset.
    fn supports_native_allowed_tools(&self, _model: &str) -> bool {
        false
    }

    /// Whether the provider supports provider-native context editing such as
    /// tool-result clearing.
    fn supports_context_edits(&self, _model: &str) -> bool {
        false
    }

    /// Whether the selected provider/model can carry Anthropic's native
    /// turn-scoped system-message lifecycle field on the wire.
    ///
    /// This is intentionally narrower than [`supports_context_edits`]: a
    /// provider can expose Anthropic-shaped requests without supporting the
    /// `clear_at` field, and a provider name does not necessarily identify the
    /// wire protocol for every model. The runtime keeps the typed marker in
    /// canonical history either way, translating it to an ordinary system or
    /// history directive when this capability is false.
    fn supports_turn_scoped_system_messages(&self, _model: &str) -> bool {
        false
    }

    /// Whether the provider supports the interactive manual `/compact` command path.
    ///
    /// This is narrower than general Responses compaction support and may exclude
    /// compatible endpoints that do not match VT Code's native OpenAI UX contract.
    fn supports_manual_openai_compaction(&self, _model: &str) -> bool {
        false
    }

    /// Whether the provider supports threshold-triggered inline compaction via
    /// request fields (Anthropic `compact_20260112`).
    ///
    /// This is distinct from [`supports_responses_compaction`](LLMProvider::supports_responses_compaction),
    /// which is overloaded: OpenAI-compatible endpoints report it for their
    /// standalone `/responses/compact` endpoint while Anthropic reports it for
    /// inline compaction. Only the latter can be driven through `generate` with a
    /// `compact_20260112` context-management edit, so the unified compaction
    /// dispatch uses this method (not the overloaded flag) to pick the
    /// `NativeInline` strategy and avoid sending an Anthropic-specific payload to
    /// an OpenAI-compatible endpoint (which would only be rejected and fall back
    /// to local summarization anyway).
    fn supports_native_inline_compaction(&self, _model: &str) -> bool {
        false
    }

    /// Explain why the `--native-only` manual `/compact` path is unavailable.
    ///
    /// This message only surfaces when the user explicitly passes `--native-only`
    /// and the provider does not expose a native server-side compaction endpoint.
    /// The plain `/compact` command is provider-agnostic and always falls back to
    /// local summarization, so it is never refused on capability grounds.
    fn manual_openai_compaction_unavailable_message(&self, model: &str) -> String {
        format!(
            "`--native-only` `/compact` requires a provider that exposes a native server-side compaction endpoint, which this provider does not. Active provider/model: {} / {}. Run `/compact` without `--native-only` to compact via the universal local summarization fallback.",
            self.name(),
            model,
        )
    }

    /// Get the effective context window size for a model.
    ///
    /// Curated catalog capacity is the default for providers that do not have
    /// a narrower route-specific limit. Adapters with an explicit endpoint
    /// ceiling can still override this method and keep that ceiling intact.
    fn effective_context_size(&self, model: &str) -> usize {
        catalog_context_window(self.name(), model, 128_000)
    }

    /// Compact conversation history using provider-native Responses `/compact`
    /// support when available.
    async fn compact_history(&self, _model: &str, _history: &[Message]) -> Result<Vec<Message>, LLMError> {
        Err(LLMError::Provider {
            message: "Conversation compaction is not supported by this provider".to_string(),
            metadata: None,
        })
    }

    /// Compact conversation history with standalone Responses compaction options.
    async fn compact_history_with_options(
        &self,
        _model: &str,
        _history: &[Message],
        _options: &ResponsesCompactionOptions,
    ) -> Result<Vec<Message>, LLMError> {
        Err(LLMError::Provider {
            message: "manual OpenAI compaction is not supported by this provider".to_string(),
            metadata: None,
        })
    }

    /// Generate completion
    async fn generate(&self, request: LLMRequest) -> Result<LLMResponse, LLMError>;

    /// Stream completion (optional)
    async fn stream(&self, request: LLMRequest) -> Result<LLMStream, LLMError> {
        // Default implementation falls back to non-streaming
        let response = self.generate(request).await?;
        let stream = try_stream! {
            yield LLMStreamEvent::Completed { response: Box::new(response) };
        };
        Ok(Box::pin(stream))
    }

    /// Normalized streaming contract layered on top of the legacy provider stream.
    async fn stream_normalized(&self, request: LLMRequest) -> Result<LLMNormalizedStream, LLMError> {
        let mut legacy_stream = self.stream(request).await?;
        let stream = try_stream! {
            while let Some(event) = futures::StreamExt::next(&mut legacy_stream).await {
                for normalized in event?.into_normalized() {
                    yield normalized;
                }
            }
        };
        Ok(Box::pin(stream))
    }

    /// Provider-specific streaming path that can service interactive runtime
    /// requests while the stream is active. Copilot uses this to bridge ACP
    /// tool calls and permission prompts back into VT Code's turn runtime.
    #[cfg(feature = "copilot")]
    fn start_copilot_prompt_session<'a>(
        &'a self,
        _request: LLMRequest,
        _tools: &'a [super::ToolDefinition],
    ) -> Option<crate::copilot::CopilotPromptSessionFuture<'a>> {
        None
    }

    /// Get supported models
    fn supported_models(&self) -> Vec<String>;

    /// Fetch account balance for this provider, if supported.
    async fn get_balance(&self) -> Result<Option<vtcode_commons::llm::BalanceInfo>, LLMError> {
        Ok(None)
    }

    /// Validate request for this provider
    fn validate_request(&self, request: &LLMRequest) -> Result<(), LLMError>;
}

/// Provider-local context capacity discovered for the selected model.
/// All transport and capability behavior remains delegated to the underlying provider.
pub struct ContextWindowProvider {
    inner: Box<dyn LLMProvider>,
    model: CompactStr,
    context_window: usize,
}

impl ContextWindowProvider {
    /// Unknown/zero metadata preserves the backend's usable capacity.
    pub fn wrap(inner: Box<dyn LLMProvider>, model: &str, context_window: Option<usize>) -> Box<dyn LLMProvider> {
        match context_window.filter(|value| *value > 0) {
            Some(context_window) => Box::new(Self { inner, model: model.into(), context_window }),
            None => inner,
        }
    }
}

#[async_trait]
impl LLMProvider for ContextWindowProvider {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn backend_kind(&self) -> BackendKind {
        self.inner.backend_kind()
    }

    fn supports_streaming(&self) -> bool {
        self.inner.supports_streaming()
    }

    fn supports_non_streaming(&self, model: &str) -> bool {
        self.inner.supports_non_streaming(model)
    }

    fn supports_reasoning(&self, model: &str) -> bool {
        self.inner.supports_reasoning(model)
    }

    fn supports_reasoning_effort(&self, model: &str) -> bool {
        self.inner.supports_reasoning_effort(model)
    }

    fn supported_reasoning_efforts(&self, model: &str) -> &'static [&'static str] {
        self.inner.supported_reasoning_efforts(model)
    }

    fn sampling_overrides(&self, model: &str) -> SamplingOverrides {
        self.inner.sampling_overrides(model)
    }

    fn supports_tools(&self, model: &str) -> bool {
        self.inner.supports_tools(model)
    }

    fn supports_parallel_tool_config(&self, model: &str) -> bool {
        self.inner.supports_parallel_tool_config(model)
    }

    fn supports_structured_output(&self, model: &str) -> bool {
        self.inner.supports_structured_output(model)
    }

    fn supports_context_caching(&self, model: &str) -> bool {
        self.inner.supports_context_caching(model)
    }

    fn supports_vision(&self, model: &str) -> bool {
        self.inner.supports_vision(model)
    }

    fn supports_responses_compaction(&self, model: &str) -> bool {
        self.inner.supports_responses_compaction(model)
    }

    fn supports_native_allowed_tools(&self, model: &str) -> bool {
        self.inner.supports_native_allowed_tools(model)
    }

    fn supports_context_edits(&self, model: &str) -> bool {
        self.inner.supports_context_edits(model)
    }

    fn supports_turn_scoped_system_messages(&self, model: &str) -> bool {
        self.inner.supports_turn_scoped_system_messages(model)
    }

    fn supports_manual_openai_compaction(&self, model: &str) -> bool {
        self.inner.supports_manual_openai_compaction(model)
    }

    fn supports_native_inline_compaction(&self, model: &str) -> bool {
        self.inner.supports_native_inline_compaction(model)
    }

    fn manual_openai_compaction_unavailable_message(&self, model: &str) -> String {
        self.inner.manual_openai_compaction_unavailable_message(model)
    }

    fn effective_context_size(&self, model: &str) -> usize {
        let requested_model = if model.trim().is_empty() {
            self.model.as_str()
        } else {
            model
        };
        if requested_model == self.model.as_str() {
            self.context_window
        } else {
            self.inner.effective_context_size(requested_model)
        }
    }

    async fn compact_history(&self, model: &str, history: &[Message]) -> Result<Vec<Message>, LLMError> {
        self.inner.compact_history(model, history).await
    }

    async fn compact_history_with_options(
        &self,
        model: &str,
        history: &[Message],
        options: &ResponsesCompactionOptions,
    ) -> Result<Vec<Message>, LLMError> {
        self.inner.compact_history_with_options(model, history, options).await
    }

    async fn generate(&self, request: LLMRequest) -> Result<LLMResponse, LLMError> {
        self.inner.generate(request).await
    }

    async fn stream(&self, request: LLMRequest) -> Result<LLMStream, LLMError> {
        self.inner.stream(request).await
    }

    async fn stream_normalized(&self, request: LLMRequest) -> Result<LLMNormalizedStream, LLMError> {
        self.inner.stream_normalized(request).await
    }

    #[cfg(feature = "copilot")]
    fn start_copilot_prompt_session<'a>(
        &'a self,
        request: LLMRequest,
        tools: &'a [super::ToolDefinition],
    ) -> Option<crate::copilot::CopilotPromptSessionFuture<'a>> {
        self.inner.start_copilot_prompt_session(request, tools)
    }

    fn supported_models(&self) -> Vec<String> {
        self.inner.supported_models()
    }

    async fn get_balance(&self) -> Result<Option<vtcode_commons::llm::BalanceInfo>, LLMError> {
        self.inner.get_balance().await
    }

    fn validate_request(&self, request: &LLMRequest) -> Result<(), LLMError> {
        self.inner.validate_request(request)
    }
}
