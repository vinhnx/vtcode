use serde_json::{Map, Value};
use vtcode_config::constants::{env_vars, models, urls};

use super::extract_reasoning_trace;
use super::openai_compat::{OpenAiCompatCore, OpenAiCompatSpec, SystemPromptPlacement, impl_openai_compat_provider};

pub struct XaiSpec;

fn xai_reasoning(message: &Value, _choice: &Value) -> Option<String> {
    message.get("reasoning_content").and_then(extract_reasoning_trace)
}

impl OpenAiCompatSpec for XaiSpec {
    const NAME: &'static str = "xAI";
    const KEY: &'static str = "xai";
    const API_KEY_ENV: &'static str = "XAI_API_KEY";
    const DEFAULT_MODEL: &'static str = models::xai::DEFAULT_MODEL;
    const DEFAULT_BASE_URL: &'static str = urls::XAI_API_BASE;
    const BASE_URL_ENV: Option<&'static str> = Some(env_vars::XAI_BASE_URL);
    const LISTED_MODELS: &'static [&'static str] = models::xai::SUPPORTED_MODELS;
    const VALIDATION_ALLOWLIST: Option<&'static [&'static str]> = Some(models::xai::SUPPORTED_MODELS);

    const SYSTEM_PROMPT: SystemPromptPlacement = SystemPromptPlacement::FirstMessage;
    const STREAM_OPTIONS_INCLUDE_USAGE: bool = true;
    const INCLUDE_USER_ID: bool = true;
    const RESPONSE_REASONING_EXTRACTOR: Option<super::openai_compat::ReasoningExtractor> = Some(xai_reasoning);

    fn insert_reasoning(
        _core: &OpenAiCompatCore<Self>,
        request: &crate::provider::LLMRequest,
        payload: &mut Map<String, Value>,
    ) -> Result<(), crate::provider::LLMError> {
        if let Some(effort) = request.reasoning_effort {
            if !matches!(
                effort,
                vtcode_config::types::ReasoningEffortLevel::None | vtcode_config::types::ReasoningEffortLevel::Unknown
            ) {
                // xAI natively supports `low`/`medium`/`high`/`xhigh` only:
                // no native `minimal` (clamp to `low`) or `max` (clamp to `xhigh`).
                // Older models treat `xhigh` as `high`.
                let value = match effort {
                    vtcode_config::types::ReasoningEffortLevel::Minimal
                    | vtcode_config::types::ReasoningEffortLevel::Low => "low",
                    vtcode_config::types::ReasoningEffortLevel::Max => "xhigh",
                    other => other.as_str(),
                };
                payload.insert("reasoning_effort".to_owned(), serde_json::json!(value));
            }
        }
        Ok(())
    }

    fn response_cache_metrics(core: &OpenAiCompatCore<Self>) -> bool {
        core.prompt_cache_enabled
    }

    fn stream_cache_metrics(_core: &OpenAiCompatCore<Self>) -> bool {
        true
    }
}

impl_openai_compat_provider!(XAIProvider, XaiSpec, {
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
            || models::xai::REASONING_MODELS.contains(&requested)
    }

    fn supports_reasoning_effort(&self, model: &str) -> bool {
        let requested = if model.trim().is_empty() {
            &self.core.model
        } else {
            model
        };
        self.core
            .model_behavior
            .as_ref()
            .and_then(|b| b.model_supports_reasoning_effort)
            .unwrap_or_else(|| {
                vtcode_config::models::model_catalog_entry("xai", requested)
                    .is_some_and(|entry| !entry.reasoning_efforts.is_empty())
            })
    }
});

#[cfg(test)]
mod tests {
    use super::XAIProvider;
    use crate::provider::{LLMRequest, Message, ToolChoice};
    use std::sync::Arc;
    use vtcode_config::constants::models;
    use vtcode_config::types::ReasoningEffortLevel;

    fn base_request() -> LLMRequest {
        LLMRequest {
            messages: vec![Message::user("hello".to_string())].into(),
            system_prompt: Some(Arc::from("system guidance")),
            model: models::xai::DEFAULT_MODEL.to_string(),
            max_tokens: Some(512),
            temperature: Some(0.5),
            top_p: Some(0.25),
            stream: true,
            tool_choice: Some(ToolChoice::Auto),
            ..Default::default()
        }
    }

    #[test]
    fn golden_payload_basic_shape() {
        let provider = XAIProvider::new("test-key".to_string());
        let payload = provider.core.convert_request(&base_request()).unwrap();

        assert_eq!(payload["model"], models::xai::DEFAULT_MODEL);
        let messages = payload["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "system guidance");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(payload["max_tokens"], 512);
        assert_eq!(payload["temperature"], 0.5);
        assert_eq!(payload["top_p"], 0.25);
        assert_eq!(payload["stream"], true);
        assert_eq!(payload["stream_options"]["include_usage"], true);
        assert_eq!(payload["tool_choice"], "auto");
    }

    #[test]
    fn golden_payload_with_reasoning_effort() {
        let provider = XAIProvider::new("test-key".to_string());

        let mut request = base_request();
        request.reasoning_effort = Some(ReasoningEffortLevel::High);
        let payload = provider.core.convert_request(&request).unwrap();
        assert_eq!(payload["reasoning_effort"], "high");
    }

    #[test]
    fn max_effort_clamps_to_xhigh() {
        let provider = XAIProvider::new("test-key".to_string());

        let mut request = base_request();
        request.reasoning_effort = Some(ReasoningEffortLevel::Max);
        let payload = provider.core.convert_request(&request).unwrap();
        // No native `max`; closest supported level is `xhigh`.
        assert_eq!(payload["reasoning_effort"], "xhigh");

        // No native `minimal` either; closest supported level is `low`.
        request.reasoning_effort = Some(ReasoningEffortLevel::Minimal);
        let payload = provider.core.convert_request(&request).unwrap();
        assert_eq!(payload["reasoning_effort"], "low");
    }
}
