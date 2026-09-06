use super::OpenAIProvider;
use crate::client::LLMClient;
use crate::provider::{self, LLMNormalizedStream};
use crate::types as llm_types;
use async_trait::async_trait;
use vtcode_config::constants::models;

#[async_trait]
impl provider::LLMProvider for OpenAIProvider {
    fn name(&self) -> &str {
        self.provider_key_override.as_deref().unwrap_or("openai")
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_non_streaming(&self, model: &str) -> bool {
        let requested = if model.trim().is_empty() {
            self.model.as_ref()
        } else {
            model
        };

        !self.is_chatgpt_backend() && !Self::requires_streaming_responses(requested)
    }

    fn effective_context_size(&self, model: &str) -> usize {
        if let Some(context_window) = self.context_window_override {
            return context_window;
        }

        let requested = if model.trim().is_empty() {
            self.model.as_ref()
        } else {
            model
        };

        vtcode_config::models::model_catalog_entry(self.name(), requested)
            .map(|entry| entry.context_window)
            .filter(|context_window| *context_window > 0)
            .unwrap_or(128_000)
    }

    fn supports_reasoning(&self, model: &str) -> bool {
        let requested = if model.trim().is_empty() {
            self.model.as_ref()
        } else {
            model
        };

        // Codex-inspired robustness: Setting model_supports_reasoning to false
        // does NOT disable it for known reasoning models.
        models::openai::REASONING_MODELS.contains(&requested)
            || self
                .model_behavior
                .as_ref()
                .and_then(|b| b.model_supports_reasoning)
                .unwrap_or(false)
    }

    fn supports_reasoning_effort(&self, model: &str) -> bool {
        let requested = if model.trim().is_empty() {
            self.model.as_ref()
        } else {
            model
        };

        // Same robustness logic for reasoning effort
        models::openai::REASONING_MODELS.iter().any(|candidate| *candidate == requested)
            || self
                .model_behavior
                .as_ref()
                .and_then(|b| b.model_supports_reasoning_effort)
                .unwrap_or(false)
    }

    fn supports_tools(&self, model: &str) -> bool {
        let requested = if model.trim().is_empty() {
            self.model.as_ref()
        } else {
            model
        };

        !models::openai::TOOL_UNAVAILABLE_MODELS.contains(&requested)
    }

    fn supports_responses_compaction(&self, model: &str) -> bool {
        if self.is_chatgpt_backend() {
            return false;
        }
        let requested = if model.trim().is_empty() {
            self.model.as_ref()
        } else {
            model
        };
        !matches!(self.responses_api_state(requested), super::super::types::ResponsesApiState::Disabled)
    }

    fn supports_native_allowed_tools(&self, model: &str) -> bool {
        self.supports_responses_allowed_tools(model)
    }

    fn supports_manual_openai_compaction(&self, model: &str) -> bool {
        let requested = if model.trim().is_empty() {
            self.model.as_ref()
        } else {
            model
        };
        self.supports_manual_openai_compaction_for_model(requested)
    }

    fn manual_openai_compaction_unavailable_message(&self, model: &str) -> String {
        self.manual_openai_compaction_unavailable_message_for_model(model)
    }

    async fn stream(&self, request: provider::LLMRequest) -> Result<provider::LLMStream, provider::LLMError> {
        self.stream_request(request).await
    }

    async fn stream_normalized(
        &self,
        request: provider::LLMRequest,
    ) -> Result<LLMNormalizedStream, provider::LLMError> {
        self.stream_normalized_request(request).await
    }

    async fn generate(&self, request: provider::LLMRequest) -> Result<provider::LLMResponse, provider::LLMError> {
        self.generate_request(request).await
    }

    async fn compact_history(
        &self,
        model: &str,
        history: &[provider::Message],
    ) -> Result<Vec<provider::Message>, provider::LLMError> {
        if !self.supports_responses_compaction(model) {
            return Err(provider::LLMError::Provider {
                message: "OpenAI Responses compaction is not supported for this endpoint/model".to_string(),
                metadata: None,
            });
        }

        self.compact_history_request(model, history).await
    }

    async fn compact_history_with_options(
        &self,
        model: &str,
        history: &[provider::Message],
        options: &provider::ResponsesCompactionOptions,
    ) -> Result<Vec<provider::Message>, provider::LLMError> {
        let requested = if model.trim().is_empty() {
            self.model.as_ref()
        } else {
            model
        };
        if !self.supports_manual_openai_compaction_for_model(requested) {
            return Err(provider::LLMError::Provider {
                message: self.manual_openai_compaction_unavailable_message_for_model(requested),
                metadata: None,
            });
        }

        self.compact_history_request_with_options(requested, history, options).await
    }

    fn supported_models(&self) -> Vec<String> {
        if let Some(models) = &self.supported_models_override {
            return models.clone();
        }
        if self.provider_key_override.is_some() {
            return vec![self.model.to_string()];
        }
        models::openai::SUPPORTED_MODELS.iter().map(|s| s.to_string()).collect()
    }

    fn validate_request(&self, request: &provider::LLMRequest) -> Result<(), provider::LLMError> {
        let supported_models = (!self.is_native_openai_api()).then(|| self.supported_models());

        let display_name = self.provider_display_override.as_deref().unwrap_or("OpenAI");
        let key = self.provider_key_override.as_deref().unwrap_or("openai");
        super::super::super::common::validate_request_common(request, display_name, key, supported_models.as_deref())
    }
}

#[async_trait]
impl LLMClient for OpenAIProvider {
    async fn generate(&mut self, prompt: &str) -> Result<llm_types::LLMResponse, provider::LLMError> {
        let request = super::super::super::common::make_default_request(prompt, &self.model);
        let request_model = request.model.to_string();
        let response = provider::LLMProvider::generate(self, request).await?;

        Ok(llm_types::LLMResponse {
            content: Some(response.content.unwrap_or_default()),
            model: request_model,
            usage: response.usage.map(super::super::super::common::convert_usage_to_llm_types),
            reasoning: response.reasoning,
            reasoning_details: response.reasoning_details,
            request_id: response.request_id,
            organization_id: response.organization_id,
            finish_reason: response.finish_reason,
            tool_calls: response.tool_calls,
            tool_references: response.tool_references,
            compaction: None,
        })
    }

    fn model_id(&self) -> &str {
        &self.model
    }
}
