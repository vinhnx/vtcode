use serde_json::{Map, Value};
use vtcode_config::constants::{env_vars, models, urls};

use super::extract_reasoning_trace;
use super::openai_compat::{OpenAiCompatCore, OpenAiCompatSpec, impl_openai_compat_provider};
use crate::provider::{LLMError, LLMRequest};

pub struct QwenSpec;

fn qwen_reasoning(message: &Value, choice: &Value) -> Option<String> {
    message
        .get("reasoning_content")
        .and_then(extract_reasoning_trace)
        .or_else(|| choice.get("reasoning_content").and_then(extract_reasoning_trace))
}

impl OpenAiCompatSpec for QwenSpec {
    const NAME: &'static str = "Qwen";
    const KEY: &'static str = "qwen";
    const API_KEY_ENV: &'static str = "QWEN_API_KEY";
    const DEFAULT_MODEL: &'static str = models::qwen::DEFAULT_MODEL;
    const DEFAULT_BASE_URL: &'static str = urls::QWEN_API_BASE;
    const BASE_URL_ENV: Option<&'static str> = Some(env_vars::QWEN_BASE_URL);
    const LISTED_MODELS: &'static [&'static str] = models::qwen::SUPPORTED_MODELS;
    const VALIDATION_ALLOWLIST: Option<&'static [&'static str]> = Some(models::qwen::SUPPORTED_MODELS);

    const STREAM_OPTIONS_INCLUDE_USAGE: bool = true;
    const INCLUDE_USER_ID: bool = true;
    const RESPONSE_REASONING_EXTRACTOR: Option<super::openai_compat::ReasoningExtractor> = Some(qwen_reasoning);

    fn resolve_api_key(api_key: Option<String>) -> String {
        api_key
            .or_else(|| std::env::var("QWEN_API_KEY").ok().filter(|key| !key.trim().is_empty()))
            .or_else(|| std::env::var("DASHSCOPE_API_KEY").ok().filter(|key| !key.trim().is_empty()))
            .unwrap_or_default()
    }

    fn response_cache_metrics(core: &OpenAiCompatCore<Self>) -> bool {
        core.prompt_cache_enabled
    }

    fn stream_cache_metrics(_core: &OpenAiCompatCore<Self>) -> bool {
        true
    }

    fn insert_reasoning(
        _core: &OpenAiCompatCore<Self>,
        request: &LLMRequest,
        payload: &mut Map<String, Value>,
    ) -> Result<(), LLMError> {
        if let Some(effort) = request.reasoning_effort {
            let enable_thinking = effort != vtcode_config::types::ReasoningEffortLevel::None;
            payload.insert("enable_thinking".to_owned(), Value::Bool(enable_thinking));
        }
        Ok(())
    }
}

impl_openai_compat_provider!(QwenProvider, QwenSpec, {
    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_structured_output(&self, _model: &str) -> bool {
        true
    }

    fn supports_reasoning(&self, model: &str) -> bool {
        let requested = if model.trim().is_empty() {
            &self.core.model
        } else {
            model
        };

        self.core
            .model_behavior
            .as_ref()
            .and_then(|b| b.model_supports_reasoning)
            .unwrap_or(false)
            || requested == models::qwen::DEEPSEEK_V4_FLASH
            || requested == models::qwen::DEEPSEEK_V4_PRO
    }

    fn supports_reasoning_effort(&self, _model: &str) -> bool {
        self.core
            .model_behavior
            .as_ref()
            .and_then(|b| b.model_supports_reasoning_effort)
            .unwrap_or(false)
    }

    fn effective_context_size(&self, model: &str) -> usize {
        let requested = if model.trim().is_empty() {
            &self.core.model
        } else {
            model
        };
        crate::provider::catalog_context_window(
            "qwen",
            requested,
            match requested {
                models::qwen::DEEPSEEK_V4_FLASH | models::qwen::DEEPSEEK_V4_PRO => 1_048_576,
                _ => 131_072,
            },
        )
    }
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{Message, ToolChoice};
    use std::sync::Arc;
    use vtcode_config::types::ReasoningEffortLevel;

    fn provider() -> QwenProvider {
        QwenProvider::from_config(
            Some("test-key".to_string()),
            Some("qwen3-max".to_string()),
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
            model: "qwen3-max".to_string(),
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

        assert_eq!(payload["model"], "qwen3-max");
        let messages = payload["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "system guidance");
        assert_eq!(payload["max_tokens"], 512);
        assert_eq!(payload["temperature"], 0.5);
        assert_eq!(payload["top_p"], 0.25);
        assert_eq!(payload["stream"], true);
        assert_eq!(payload["stream_options"]["include_usage"], true);
        assert_eq!(payload["tool_choice"], "auto");
        assert_eq!(payload["user_id"], "user-42");
        assert!(payload.get("enable_thinking").is_none());
    }

    #[test]
    fn golden_payload_thinking_toggles_and_suppresses_sampling() {
        let mut request = base_request();
        request.reasoning_effort = Some(ReasoningEffortLevel::High);
        let payload = provider().core.convert_request(&request).unwrap();
        assert_eq!(payload["enable_thinking"], true);
        assert!(payload.get("temperature").is_none());
        assert!(payload.get("top_p").is_none());

        let mut request = base_request();
        request.reasoning_effort = Some(ReasoningEffortLevel::None);
        let payload = provider().core.convert_request(&request).unwrap();
        assert_eq!(payload["enable_thinking"], false);
        assert_eq!(payload["temperature"], 0.5);
        assert_eq!(payload["top_p"], 0.25);
    }
}
