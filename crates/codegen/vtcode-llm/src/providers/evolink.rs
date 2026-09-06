use serde_json::{Map, Value};

use crate::client::LLMClient;
use crate::error_display;
use crate::provider::{FinishReason, LLMError, LLMProvider, LLMRequest, LLMResponse, LLMStream, LLMStreamEvent};
use vtcode_config::constants::{env_vars, models, urls};
use vtcode_config::core::PromptCachingConfig;
use vtcode_config::types::ReasoningEffortLevel;

use super::common::{collect_history_system_directives, merge_system_prompt_with_history_directives};
use super::error_handling::{format_network_error, handle_openai_http_error};
use super::extract_reasoning_trace;
use super::openai_compat::{OpenAiCompatCore, OpenAiCompatSpec};

const PROVIDER_NAME: &str = "Evolink";
const PROVIDER_KEY: &str = "evolink";
const PRIMARY_API_KEY_ENV: &str = "EVOLINK_API_KEY";

pub struct EvolinkSpec;

/// Evolink's gateway expects bare upstream model names (e.g. `gpt-5.2`).
/// The curated `ModelId` catalog namespaces entries as `evolink/<model>`, so
/// strip that prefix before sending the request upstream.
fn normalize(model: &str) -> &str {
    model.trim().strip_prefix("evolink/").unwrap_or(model.trim())
}

fn evolink_reasoning(message: &Value, choice: &Value) -> Option<String> {
    message
        .get("reasoning")
        .and_then(extract_reasoning_trace)
        .or_else(|| message.get("reasoning_content").and_then(extract_reasoning_trace))
        .or_else(|| choice.get("reasoning").and_then(extract_reasoning_trace))
}

fn reasoning_effort_value(effort: ReasoningEffortLevel) -> Option<&'static str> {
    match effort {
        ReasoningEffortLevel::None | ReasoningEffortLevel::Unknown => None,
        ReasoningEffortLevel::Minimal | ReasoningEffortLevel::Low => Some("low"),
        ReasoningEffortLevel::Medium => Some("medium"),
        ReasoningEffortLevel::High | ReasoningEffortLevel::XHigh | ReasoningEffortLevel::Max => Some("high"),
    }
}

impl OpenAiCompatSpec for EvolinkSpec {
    const NAME: &'static str = PROVIDER_NAME;
    const KEY: &'static str = PROVIDER_KEY;
    const API_KEY_ENV: &'static str = PRIMARY_API_KEY_ENV;
    const DEFAULT_MODEL: &'static str = models::evolink::DEFAULT_MODEL;
    const DEFAULT_BASE_URL: &'static str = urls::EVOLINK_API_BASE;
    const BASE_URL_ENV: Option<&'static str> = Some(env_vars::EVOLINK_BASE_URL);
    const LISTED_MODELS: &'static [&'static str] = models::evolink::SUPPORTED_MODELS;
    // Evolink is a gateway whose upstream catalog changes over time, so do
    // not constrain requests to the curated `SUPPORTED_MODELS` list.
    const VALIDATION_ALLOWLIST: Option<&'static [&'static str]> = None;

    const STREAM_REASONING_FIELDS: &'static [&'static str] = &["reasoning", "reasoning_content"];
    const RESPONSE_REASONING_EXTRACTOR: Option<super::openai_compat::ReasoningExtractor> = Some(evolink_reasoning);

    fn resolve_api_key(api_key: Option<String>) -> String {
        api_key
            .or_else(|| std::env::var(Self::API_KEY_ENV).ok().filter(|key| !key.trim().is_empty()))
            .unwrap_or_default()
    }

    /// Evolink's gateway expects bare upstream model names (e.g. `gpt-5.2`).
    /// The curated `ModelId` catalog namespaces entries as `evolink/<model>`, so
    /// strip that prefix before sending the request upstream.
    fn normalize_model(model: String) -> String {
        normalize(&model).to_string()
    }

    fn prompt_cache_enabled(_prompt_cache: Option<&PromptCachingConfig>) -> bool {
        false
    }

    fn insert_reasoning(
        _core: &OpenAiCompatCore<Self>,
        request: &LLMRequest,
        payload: &mut Map<String, Value>,
    ) -> Result<(), LLMError> {
        if let Some(effort) = request.reasoning_effort
            && let Some(mapped) = reasoning_effort_value(effort)
        {
            payload.insert("reasoning_effort".to_owned(), Value::String(mapped.to_string()));
        }
        Ok(())
    }
}

pub struct EvolinkProvider {
    core: OpenAiCompatCore<EvolinkSpec>,
}

impl EvolinkProvider {
    fn new(api_key: String) -> Self {
        Self::with_model(api_key, models::evolink::DEFAULT_MODEL.to_string())
    }

    fn with_model(api_key: String, model: String) -> Self {
        Self { core: OpenAiCompatCore::direct(api_key, model) }
    }

    pub fn new_with_client(
        api_key: String,
        model: String,
        http_client: reqwest::Client,
        base_url: String,
        _timeouts: vtcode_config::TimeoutsConfig,
    ) -> Self {
        Self {
            core: OpenAiCompatCore::from_parts(api_key, model, http_client, base_url),
        }
    }

    pub fn from_config(
        api_key: Option<String>,
        model: Option<String>,
        base_url: Option<String>,
        _prompt_cache: Option<PromptCachingConfig>,
        timeouts: Option<vtcode_config::TimeoutsConfig>,
        _anthropic: Option<vtcode_config::core::AnthropicConfig>,
        model_behavior: Option<vtcode_config::core::ModelConfig>,
    ) -> Self {
        Self {
            core: OpenAiCompatCore::from_config(api_key, model, base_url, _prompt_cache, timeouts, model_behavior),
        }
    }

    fn is_anthropic_model(model: &str) -> bool {
        models::evolink::is_anthropic_format(model)
    }

    fn convert_to_anthropic_format(&self, request: &LLMRequest) -> Result<Value, LLMError> {
        let mut payload = Map::with_capacity(8);
        let model = normalize(&request.model).to_string();
        payload.insert("model".to_owned(), Value::String(model));

        // Anthropic uses a top-level `system` field, so promote the same
        // canonical history directives that the direct Anthropic adapter uses.
        let history_system_directives = collect_history_system_directives(request);
        let system_prompt = merge_system_prompt_with_history_directives(
            request.system_prompt.as_deref(),
            &history_system_directives,
            "[History Directives]",
        );
        if let Some(system_prompt) = system_prompt {
            let trimmed = system_prompt.trim();
            if !trimmed.is_empty() {
                payload.insert("system".to_owned(), Value::String(trimmed.to_string()));
            }
        }

        // Convert messages to Anthropic format (user/assistant only, no system)
        let anthropic_messages: Vec<Value> = request
            .messages
            .iter()
            .filter(|msg| msg.role != crate::provider::MessageRole::System)
            .map(|msg| {
                let role = match msg.role {
                    crate::provider::MessageRole::User => "user",
                    crate::provider::MessageRole::Assistant => "assistant",
                    _ => "user",
                };
                serde_json::json!({
                    "role": role,
                    "content": msg.content.as_text()
                })
            })
            .collect();
        payload.insert("messages".to_owned(), Value::Array(anthropic_messages));

        let max_tokens = request.max_tokens.unwrap_or(8192);
        payload.insert("max_tokens".to_owned(), Value::Number(serde_json::Number::from(max_tokens as u64)));

        if let Some(temperature) = request.temperature {
            payload.insert("temperature".to_owned(), Value::Number(super::common::float_to_json_number(temperature)?));
        }

        if request.stream {
            payload.insert("stream".to_owned(), Value::Bool(true));
        }

        Ok(Value::Object(payload))
    }

    fn parse_anthropic_response(response_json: Value, model: String) -> Result<LLMResponse, LLMError> {
        let content = response_json.get("content").and_then(|c| c.as_array()).map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| {
                    if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                        block.get("text").and_then(|t| t.as_str()).map(String::from)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("")
        });

        let usage = response_json.get("usage").map(|u| {
            let prompt_tokens = u.get("input_tokens").and_then(|t| t.as_u64()).unwrap_or(0) as u32;
            let completion_tokens = u.get("output_tokens").and_then(|t| t.as_u64()).unwrap_or(0) as u32;
            crate::provider::Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
                cached_prompt_tokens: u.get("cache_read_input_tokens").and_then(|t| t.as_u64()).map(|v| v as u32),
                cache_creation_tokens: u.get("cache_creation_input_tokens").and_then(|t| t.as_u64()).map(|v| v as u32),
                cache_read_tokens: None,
                iterations: None,
            }
        });

        let finish_reason = match response_json.get("stop_reason").and_then(|r| r.as_str()) {
            Some("end_turn") | Some("stop_sequence") => FinishReason::Stop,
            Some("max_tokens") => FinishReason::Length,
            Some("tool_use") => FinishReason::ToolCalls,
            _ => FinishReason::Stop,
        };

        Ok(LLMResponse {
            content,
            tool_calls: None,
            model,
            usage,
            finish_reason,
            reasoning: None,
            reasoning_details: None,
            tool_references: Vec::new(),
            request_id: response_json.get("id").and_then(|id| id.as_str()).map(String::from),
            organization_id: None,
            compaction: None,
        })
    }

    async fn generate_anthropic(&self, mut request: LLMRequest, model: String) -> Result<LLMResponse, LLMError> {
        request.stream = false;
        let payload = self.convert_to_anthropic_format(&request)?;
        let url = format!("{}/messages", self.core.base_url.trim_end_matches('/'));

        let response = self
            .core
            .http_client
            .post(&url)
            .bearer_auth(&self.core.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&payload)
            .send()
            .await
            .map_err(|error| format_network_error(PROVIDER_NAME, &error))?;

        let response = handle_openai_http_error(response, PROVIDER_NAME, PRIMARY_API_KEY_ENV).await?;

        let response_json: Value = response.json().await.map_err(|error| LLMError::Provider {
            message: error_display::format_llm_error(
                PROVIDER_NAME,
                &format!("failed to parse Anthropic response: {error}"),
            ),
            metadata: None,
        })?;

        Self::parse_anthropic_response(response_json, model)
    }
}

#[async_trait::async_trait]
impl LLMProvider for EvolinkProvider {
    fn name(&self) -> &str {
        EvolinkSpec::KEY
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_tools(&self, _model: &str) -> bool {
        true
    }

    fn supports_structured_output(&self, _model: &str) -> bool {
        true
    }

    fn supports_vision(&self, _model: &str) -> bool {
        true
    }

    fn supports_reasoning(&self, model: &str) -> bool {
        let requested = if model.trim().is_empty() {
            self.core.model.as_str()
        } else {
            normalize(model)
        };

        self.core
            .model_behavior
            .as_ref()
            .and_then(|behavior| behavior.model_supports_reasoning)
            .unwrap_or(false)
            || models::evolink::REASONING_MODELS.contains(&requested)
    }

    fn supports_reasoning_effort(&self, model: &str) -> bool {
        let requested = if model.trim().is_empty() {
            self.core.model.as_str()
        } else {
            normalize(model)
        };

        self.core
            .model_behavior
            .as_ref()
            .and_then(|behavior| behavior.model_supports_reasoning_effort)
            .unwrap_or(false)
            || models::evolink::REASONING_MODELS.contains(&requested)
    }

    async fn generate(&self, mut request: LLMRequest) -> Result<LLMResponse, LLMError> {
        self.core.prepare(&mut request);
        let model = request.model.clone();

        if Self::is_anthropic_model(&model) {
            return self.generate_anthropic(request, model).await;
        }

        self.core.generate_prepared(request).await
    }

    async fn stream(&self, mut request: LLMRequest) -> Result<LLMStream, LLMError> {
        self.core.prepare(&mut request);
        self.validate_request(&request)?;
        let model = request.model.clone();

        // Anthropic models: fall back to non-streaming via generate_anthropic
        if Self::is_anthropic_model(&model) {
            request.stream = false;
            let response = self.generate_anthropic(request, model).await?;
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<LLMStreamEvent, LLMError>>();
            let _ = tx.send(Ok(LLMStreamEvent::Completed { response: Box::new(response) }));
            let stream = async_stream::try_stream! {
                let mut receiver = rx;
                while let Some(event) = receiver.recv().await {
                    yield event?;
                }
            };
            return Ok(Box::pin(stream));
        }

        request.stream = true;
        self.core.stream_prepared(request).await
    }

    fn supported_models(&self) -> Vec<String> {
        self.core.supported_models()
    }

    fn validate_request(&self, request: &LLMRequest) -> Result<(), LLMError> {
        // Evolink is a gateway whose upstream catalog changes over time, so do
        // not constrain requests to the curated `SUPPORTED_MODELS` list (see
        // `EvolinkSpec::VALIDATION_ALLOWLIST`).
        self.core.validate(request)
    }
}

#[async_trait::async_trait]
impl LLMClient for EvolinkProvider {
    async fn generate(&mut self, prompt: &str) -> Result<LLMResponse, LLMError> {
        let request = super::common::make_default_request(prompt, &self.core.model);
        Ok(LLMProvider::generate(self, request).await?)
    }

    fn model_id(&self) -> &str {
        &self.core.model
    }
}

#[cfg(test)]
mod tests {
    use super::EvolinkProvider;
    use crate::provider::{LLMRequest, Message, ToolChoice};
    use std::sync::Arc;
    use vtcode_config::constants::{models, urls};
    use vtcode_config::types::ReasoningEffortLevel;

    #[test]
    fn normalizes_namespaced_model_for_wire() {
        let provider = EvolinkProvider::with_model("test-key".to_string(), "evolink/gpt-5.6".to_string());
        assert_eq!(provider.model_id_for_test(), models::evolink::GPT_5_6);
    }

    #[test]
    fn defaults_to_direct_base_url() {
        let provider = EvolinkProvider::new("test-key".to_string());
        assert_eq!(provider.base_url_for_test(), urls::EVOLINK_API_BASE);
    }

    #[test]
    fn payload_strips_prefix_and_maps_reasoning_effort() {
        let provider = EvolinkProvider::new("test-key".to_string());
        let mut request = LLMRequest {
            model: "evolink/deepseek-v4-pro".to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            reasoning_effort: Some(ReasoningEffortLevel::High),
            ..Default::default()
        };
        provider.core.prepare(&mut request);
        let payload = provider.core.convert_request(&request).expect("payload should be valid");

        assert_eq!(payload.get("model").and_then(|value| value.as_str()), Some(models::evolink::DEEPSEEK_V4_PRO));
        assert_eq!(payload.get("reasoning_effort").and_then(|value| value.as_str()), Some("high"));
        assert!(payload.get("temperature").is_none());
    }

    #[test]
    fn golden_payload_basic_shape() {
        let provider = EvolinkProvider::new("test-key".to_string());
        let mut request = LLMRequest {
            model: "evolink/gpt-5.6".to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            system_prompt: Some(Arc::from("system guidance")),
            max_tokens: Some(512),
            temperature: Some(0.5),
            top_p: Some(0.25),
            stream: true,
            tool_choice: Some(ToolChoice::Auto),
            metadata: Some(serde_json::json!({"user_id": "user-42"})),
            ..Default::default()
        };
        provider.core.prepare(&mut request);
        let payload = provider.core.convert_request(&request).expect("payload should be valid");

        assert_eq!(payload.get("model").and_then(|value| value.as_str()), Some(models::evolink::GPT_5_6));
        let messages = payload.get("messages").and_then(|v| v.as_array()).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "system guidance");
        assert_eq!(payload["max_tokens"], 512);
        assert_eq!(payload["temperature"], 0.5);
        assert_eq!(payload["top_p"], 0.25);
        assert_eq!(payload["stream"], true);
        assert!(payload.get("stream_options").is_none());
        assert!(payload.get("user_id").is_none());
        assert_eq!(payload["tool_choice"], "auto");
        assert!(payload.get("reasoning_effort").is_none());
    }

    #[test]
    fn golden_anthropic_payload_shape() {
        let provider = EvolinkProvider::new("test-key".to_string());
        let payload = provider
            .convert_to_anthropic_format(&LLMRequest {
                model: "evolink/claude-x".to_string(),
                messages: vec![Message::user("hello".to_string())].into(),
                system_prompt: Some(Arc::from("system guidance")),
                temperature: Some(0.5),
                stream: false,
                ..Default::default()
            })
            .expect("payload should be valid");

        assert_eq!(payload["system"], "system guidance");
        let messages = payload.get("messages").and_then(|v| v.as_array()).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(payload["max_tokens"], 8192);
        assert_eq!(payload["temperature"], 0.5);
        assert!(payload.get("stream").is_none());
    }

    #[test]
    fn anthropic_payload_promotes_history_system_directives() {
        let provider = EvolinkProvider::new("test-key".to_string());
        let payload = provider
            .convert_to_anthropic_format(&LLMRequest {
                model: "evolink/claude-x".to_string(),
                messages: vec![
                    Message::user("run a command".to_string()),
                    Message::system("Only you see that command's output".to_string()),
                    Message::user("continue".to_string()),
                ]
                .into(),
                system_prompt: Some(Arc::from("system guidance")),
                ..Default::default()
            })
            .expect("payload should promote history directives");

        assert!(payload["system"].as_str().is_some_and(|system| {
            system.contains("system guidance") && system.contains("Only you see that command's output")
        }));
        let messages = payload.get("messages").and_then(|v| v.as_array()).unwrap();
        assert_eq!(messages.len(), 2);
        assert!(messages.iter().all(|message| message["role"] != "system"));
    }

    impl EvolinkProvider {
        fn model_id_for_test(&self) -> &str {
            &self.core.model
        }

        fn base_url_for_test(&self) -> &str {
            &self.core.base_url
        }
    }
}
