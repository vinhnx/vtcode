use crate::provider::{LLMError, LLMProvider, LLMRequest, LLMResponse, LLMStream, Message};
use async_trait::async_trait;
use std::env;
use vtcode_config::TimeoutsConfig;
use vtcode_config::constants::{env_vars, models, urls};
use vtcode_config::core::{AnthropicConfig, ModelConfig, PromptCachingConfig};

use super::anthropic::AnthropicProvider;
use super::common::{ensure_model, impl_llm_client, resolve_model};

pub struct MinimaxProvider {
    inner: AnthropicProvider,
    model: String,
}

impl MinimaxProvider {
    pub fn new(api_key: String) -> Self {
        Self::from_config(Some(api_key), Some(models::minimax::DEFAULT_MODEL.to_string()), None, None, None, None, None)
    }

    pub fn with_model(api_key: String, model: String) -> Self {
        Self::from_config(Some(api_key), Some(model), None, None, None, None, None)
    }

    pub fn new_with_client(
        api_key: String,
        model: String,
        http_client: reqwest::Client,
        base_url: String,
        timeouts: TimeoutsConfig,
    ) -> Self {
        let resolved_model = resolve_model(Some(model), models::minimax::DEFAULT_MODEL);
        let resolved_base = resolve_minimax_base_url(Some(base_url));

        Self {
            inner: AnthropicProvider::new_with_client(
                api_key,
                resolved_model.clone(),
                http_client,
                resolved_base,
                timeouts,
            ),
            model: resolved_model,
        }
    }

    pub fn from_config(
        api_key: Option<String>,
        model: Option<String>,
        base_url: Option<String>,
        prompt_cache: Option<PromptCachingConfig>,
        timeouts: Option<TimeoutsConfig>,
        anthropic: Option<AnthropicConfig>,
        model_behavior: Option<ModelConfig>,
    ) -> Self {
        let resolved_model = resolve_model(model, models::minimax::DEFAULT_MODEL);
        let resolved_base = resolve_minimax_base_url(base_url);

        Self {
            inner: AnthropicProvider::from_config(
                api_key,
                Some(resolved_model.clone()),
                Some(resolved_base),
                prompt_cache,
                timeouts,
                anthropic,
                model_behavior,
            ),
            model: resolved_model,
        }
    }
}

fn resolve_minimax_base_url(base_url: Option<String>) -> String {
    fn sanitize(value: &str) -> Option<String> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.trim_end_matches('/').to_string())
        }
    }

    fn is_official_minimax_host(url: &str) -> bool {
        let lower = url.to_ascii_lowercase();
        [
            "://api.minimax.io",
            "://platform.minimax.io",
            "api.minimax.io",
            "platform.minimax.io",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
    }

    let resolved = base_url
        .and_then(|value| sanitize(&value))
        .or_else(|| env::var(env_vars::MINIMAX_BASE_URL).ok().and_then(|value| sanitize(&value)))
        .or_else(|| sanitize(urls::MINIMAX_API_BASE))
        .unwrap_or_else(|| urls::MINIMAX_API_BASE.trim_end_matches('/').to_string());

    let mut normalized = resolved;

    if normalized.ends_with("/messages") {
        normalized = normalized.trim_end_matches("/messages").trim_end_matches('/').to_string();
    }

    if let Some(pos) = normalized.find("/v1/") {
        normalized = normalized[..pos + 3].to_string();
    }

    let mut without_v1 = normalized.trim_end_matches('/').to_string();
    if without_v1.ends_with("/v1") {
        without_v1 = without_v1.trim_end_matches("/v1").trim_end_matches('/').to_string();
    }

    if is_official_minimax_host(&without_v1) && !without_v1.to_ascii_lowercase().contains("/anthropic") {
        without_v1 = format!("{}/anthropic", without_v1.trim_end_matches('/'));
    }

    format!("{}/v1", without_v1.trim_end_matches('/'))
}

#[async_trait]
impl LLMProvider for MinimaxProvider {
    fn name(&self) -> &str {
        "minimax"
    }

    fn supports_streaming(&self) -> bool {
        self.inner.supports_streaming()
    }

    fn supports_reasoning(&self, model: &str) -> bool {
        self.inner.supports_reasoning(model)
    }

    fn supports_reasoning_effort(&self, model: &str) -> bool {
        self.inner.supports_reasoning_effort(model)
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

    fn effective_context_size(&self, model: &str) -> usize {
        self.inner.effective_context_size(model)
    }

    async fn compact_history(&self, model: &str, history: &[Message]) -> Result<Vec<Message>, LLMError> {
        self.inner.compact_history(model, history).await
    }

    async fn generate(&self, mut request: LLMRequest) -> Result<LLMResponse, LLMError> {
        ensure_model(&mut request, &self.model);
        self.inner.generate(request).await
    }

    async fn stream(&self, mut request: LLMRequest) -> Result<LLMStream, LLMError> {
        ensure_model(&mut request, &self.model);
        self.inner.stream(request).await
    }

    fn supported_models(&self) -> Vec<String> {
        models::minimax::SUPPORTED_MODELS.iter().map(|s| s.to_string()).collect()
    }

    fn validate_request(&self, request: &LLMRequest) -> Result<(), LLMError> {
        self.inner.validate_request(request)
    }
}

impl_llm_client!(MinimaxProvider);

#[cfg(test)]
mod tests {
    use super::{MinimaxProvider, resolve_minimax_base_url};
    use crate::client::LLMClient;
    use crate::provider::LLMProvider;
    use vtcode_config::constants::models;

    #[test]
    fn resolve_minimax_base_url_defaults_to_anthropic_v1() {
        assert_eq!(resolve_minimax_base_url(None), "https://api.minimax.io/anthropic/v1");
    }

    #[test]
    fn resolve_minimax_base_url_normalizes_root_host_to_anthropic_v1() {
        assert_eq!(
            resolve_minimax_base_url(Some("https://api.minimax.io".to_string())),
            "https://api.minimax.io/anthropic/v1"
        );
        assert_eq!(
            resolve_minimax_base_url(Some("https://api.minimax.io/v1".to_string())),
            "https://api.minimax.io/anthropic/v1"
        );
    }

    #[test]
    fn resolve_minimax_base_url_keeps_explicit_anthropic_path() {
        assert_eq!(
            resolve_minimax_base_url(Some("https://api.minimax.io/anthropic".to_string())),
            "https://api.minimax.io/anthropic/v1"
        );
        assert_eq!(
            resolve_minimax_base_url(Some("https://api.minimax.io/anthropic/v1/messages".to_string())),
            "https://api.minimax.io/anthropic/v1"
        );
    }

    #[test]
    fn resolve_minimax_base_url_respects_custom_proxy_path() {
        assert_eq!(
            resolve_minimax_base_url(Some("https://proxy.example.com/minimax".to_string())),
            "https://proxy.example.com/minimax/v1"
        );
    }

    #[test]
    fn minimax_provider_preserves_provider_name_and_default_model() {
        let provider = MinimaxProvider::from_config(Some("test-key".to_string()), None, None, None, None, None, None);

        assert_eq!(provider.name(), "minimax");
        assert_eq!(provider.model_id(), models::minimax::DEFAULT_MODEL);
        assert_eq!(
            provider.supported_models(),
            vec![
                models::minimax::MINIMAX_M3.to_string(),
                models::minimax::MINIMAX_M2_7.to_string(),
            ]
        );
    }

    #[test]
    fn minimax_provider_blank_model_falls_back_to_default() {
        let provider = MinimaxProvider::from_config(
            Some("test-key".to_string()),
            Some("   ".to_string()),
            None,
            None,
            None,
            None,
            None,
        );

        assert_eq!(provider.model_id(), models::minimax::DEFAULT_MODEL);
    }

    #[test]
    fn minimax_provider_supports_streaming() {
        let provider = MinimaxProvider::from_config(Some("test-key".to_string()), None, None, None, None, None, None);

        assert!(provider.supports_streaming());
    }
}
