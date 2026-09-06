use serde_json::Value;
use vtcode_config::constants::{env_vars, models, urls};

use super::openai_compat::{OpenAiCompatCore, OpenAiCompatSpec, impl_openai_compat_provider};

pub struct PoolsideSpec;

fn no_reasoning(_message: &Value, _choice: &Value) -> Option<String> {
    None
}

impl OpenAiCompatSpec for PoolsideSpec {
    const NAME: &'static str = "Poolside";
    const KEY: &'static str = "poolside";
    const API_KEY_ENV: &'static str = "POOLSIDE_API_KEY";
    const DEFAULT_MODEL: &'static str = models::poolside::DEFAULT_MODEL;
    const DEFAULT_BASE_URL: &'static str = urls::POOLSIDE_API_BASE;
    const BASE_URL_ENV: Option<&'static str> = Some(env_vars::POOLSIDE_BASE_URL);
    const LISTED_MODELS: &'static [&'static str] = models::poolside::SUPPORTED_MODELS;
    const VALIDATION_ALLOWLIST: Option<&'static [&'static str]> = Some(models::poolside::SUPPORTED_MODELS);

    const SUPPRESS_SAMPLING_WHEN_REASONING: bool = false;
    const STREAM_OPTIONS_INCLUDE_USAGE: bool = true;
    const INCLUDE_USER_ID: bool = true;
    const STREAM_REASONING_FIELDS: &'static [&'static str] = &[];
    const DELTA_ORDER: super::shared::OpenAiDeltaOrder = super::shared::OpenAiDeltaOrder::ContentFirst;
    // Poolside responses never carry reasoning text; the explicit no-op
    // extractor also disables the default reasoning_content fallback.
    const RESPONSE_REASONING_EXTRACTOR: Option<super::openai_compat::ReasoningExtractor> = Some(no_reasoning);

    fn resolve_api_key(api_key: Option<String>) -> String {
        api_key
            .or_else(|| std::env::var(Self::API_KEY_ENV).ok().filter(|key| !key.trim().is_empty()))
            .unwrap_or_default()
    }

    fn response_cache_metrics(core: &OpenAiCompatCore<Self>) -> bool {
        core.prompt_cache_enabled
    }

    fn stream_cache_metrics(_core: &OpenAiCompatCore<Self>) -> bool {
        true
    }
}

impl_openai_compat_provider!(PoolsideProvider, PoolsideSpec, {
    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_structured_output(&self, _model: &str) -> bool {
        true
    }

    fn supports_reasoning(&self, _model: &str) -> bool {
        true
    }

    fn effective_context_size(&self, model: &str) -> usize {
        crate::provider::catalog_context_window("poolside", model, 131_072)
    }
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{LLMRequest, Message, ToolChoice};
    use std::sync::Arc;
    use vtcode_config::types::ReasoningEffortLevel;

    fn provider() -> PoolsideProvider {
        PoolsideProvider::from_config(
            Some("test-key".to_string()),
            Some("malibu-latest".to_string()),
            Some("https://example.test/v1".to_string()),
            None,
            None,
            None,
            None,
        )
    }

    fn base_request() -> LLMRequest {
        LLMRequest {
            messages: vec![Message::user("hello".to_string())].into(),
            system_prompt: Some(Arc::from("system guidance")),
            model: "malibu-latest".to_string(),
            max_tokens: Some(512),
            temperature: Some(0.5),
            top_p: Some(0.25),
            stream: true,
            tool_choice: Some(ToolChoice::Auto),
            metadata: Some(serde_json::json!({"user_id": "user-42"})),
            ..Default::default()
        }
    }

    #[test]
    fn golden_payload_basic_shape() {
        let payload = provider().core.convert_request(&base_request()).unwrap();

        assert_eq!(payload["model"], "malibu-latest");
        let messages = payload["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "system guidance");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "hello");
        assert_eq!(payload["max_tokens"], 512);
        assert_eq!(payload["temperature"], 0.5);
        assert_eq!(payload["top_p"], 0.25);
        assert_eq!(payload["stream"], true);
        assert_eq!(payload["stream_options"]["include_usage"], true);
        assert_eq!(payload["tool_choice"], "auto");
        assert_eq!(payload["user_id"], "user-42");
    }

    #[test]
    fn golden_payload_reasoning_does_not_suppress_sampling() {
        let mut request = base_request();
        request.reasoning_effort = Some(ReasoningEffortLevel::High);
        let payload = provider().core.convert_request(&request).unwrap();
        assert_eq!(payload["temperature"], 0.5);
        assert_eq!(payload["top_p"], 0.25);
        assert!(payload.get("reasoning_effort").is_none());
    }

    #[test]
    fn golden_payload_omits_empty_system_prompt() {
        let mut request = base_request();
        request.system_prompt = Some(Arc::from("   "));
        request.stream = false;
        let payload = provider().core.convert_request(&request).unwrap();
        let messages = payload["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert!(payload.get("stream").is_none());
        assert!(payload.get("stream_options").is_none());
    }
}
