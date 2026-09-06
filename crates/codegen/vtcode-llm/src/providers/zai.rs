use reqwest::RequestBuilder;
use serde_json::{Map, Value};
use vtcode_config::constants::{env_vars, models, urls};

use super::openai_compat::{OpenAiCompatCore, OpenAiCompatSpec, SystemPromptPlacement, impl_openai_compat_provider};
use crate::provider::{LLMError, LLMRequest};

pub struct ZaiSpec;

fn resolve_zai_base_url(base_url: Option<String>) -> String {
    if let Some(url) = base_url {
        let trimmed = url.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    if let Ok(value) = std::env::var(env_vars::ZAI_BASE_URL) {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    if let Ok(legacy) = std::env::var(env_vars::Z_AI_BASE_URL) {
        let trimmed = legacy.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    urls::ZAI_API_BASE.to_string()
}

impl OpenAiCompatSpec for ZaiSpec {
    const NAME: &'static str = "Z.AI";
    const KEY: &'static str = "zai";
    const API_KEY_ENV: &'static str = "ZAI_API_KEY";
    const DEFAULT_MODEL: &'static str = models::zai::DEFAULT_MODEL;
    const DEFAULT_BASE_URL: &'static str = urls::ZAI_API_BASE;
    const BASE_URL_ENV: Option<&'static str> = Some(env_vars::ZAI_BASE_URL);
    const LISTED_MODELS: &'static [&'static str] = models::zai::SUPPORTED_MODELS;
    const VALIDATION_ALLOWLIST: Option<&'static [&'static str]> = Some(models::zai::SUPPORTED_MODELS);

    const SYSTEM_PROMPT: SystemPromptPlacement = SystemPromptPlacement::Omitted;
    const SUPPRESS_SAMPLING_WHEN_REASONING: bool = false;
    const DELTA_ORDER: super::shared::OpenAiDeltaOrder = super::shared::OpenAiDeltaOrder::ContentFirst;

    fn resolve_base_url(_api_key: &str, base_url: Option<String>) -> String {
        resolve_zai_base_url(base_url)
    }

    fn insert_tool_choice(_core: &OpenAiCompatCore<Self>, request: &LLMRequest, payload: &mut Map<String, Value>) {
        if let Some(choice) = &request.tool_choice {
            // Z.AI only supports "auto"; any other requested mode is coerced.
            let tool_choice_value = match choice {
                crate::provider::ToolChoice::Auto => choice.to_provider_format(Self::KEY),
                _ => Value::String("auto".to_string()),
            };
            payload.insert("tool_choice".to_string(), tool_choice_value);
        } else if request.tools.as_ref().is_some_and(|tools| !tools.is_empty()) {
            payload.insert("tool_choice".to_string(), Value::String("auto".to_string()));
        }
    }

    fn insert_reasoning(
        core: &OpenAiCompatCore<Self>,
        request: &LLMRequest,
        payload: &mut Map<String, Value>,
    ) -> Result<(), LLMError> {
        let has_preserved_reasoning = request.messages.iter().any(|message| {
            message.role == crate::provider::MessageRole::Assistant
                && message.reasoning.as_ref().is_some_and(|reasoning| !reasoning.is_empty())
        });

        if let Some(effort) = request.reasoning_effort {
            if effort == vtcode_config::types::ReasoningEffortLevel::None {
                payload.insert("thinking".to_owned(), serde_json::json!({"type": "disabled"}));
                return Ok(());
            }

            use crate::rig_adapter::RigProviderCapabilities;
            use vtcode_config::models::Provider;
            let supported = crate::provider::catalog_or_explicit_reasoning_efforts(
                Self::KEY,
                &request.model,
                core.model_behavior
                    .as_ref()
                    .and_then(|behavior| behavior.model_supports_reasoning_effort)
                    .unwrap_or(false),
            );
            let reasoning_params = RigProviderCapabilities::new(Provider::ZAI, &request.model)
                .reasoning_parameters_for_supported_efforts(effort, supported)
                ?
                .ok_or_else(|| LLMError::InvalidRequest {
                    message: format!(
                        "Reasoning effort `{effort}` is unsupported for Z.AI model `{}`; choose one of low, high, or max",
                        request.model
                    ),
                    metadata: None,
                })?;
            if let Some(params_obj) = reasoning_params.as_object() {
                for (k, v) in params_obj {
                    payload.insert(k.clone(), v.clone());
                }
            }
        }

        if has_preserved_reasoning {
            if let Some(thinking) = payload.get_mut("thinking").and_then(Value::as_object_mut) {
                thinking.insert("clear_thinking".to_owned(), Value::Bool(false));
            } else {
                payload.insert(
                    "thinking".to_owned(),
                    serde_json::json!({
                        "type": "enabled",
                        "clear_thinking": false
                    }),
                );
            }
        }

        Ok(())
    }

    fn finish_payload(
        _core: &OpenAiCompatCore<Self>,
        request: &LLMRequest,
        payload: &mut Map<String, Value>,
    ) -> Result<(), LLMError> {
        if let Some(do_sample) = request.do_sample {
            payload.insert("do_sample".to_owned(), Value::Bool(do_sample));
        }

        if request.stream && request.tools.as_ref().is_some_and(|tools| !tools.is_empty()) {
            payload.insert("tool_stream".to_string(), Value::Bool(true));
        }

        if request.output_format.is_some() {
            payload.insert("response_format".to_owned(), serde_json::json!({ "type": "json_object" }));
        }

        Ok(())
    }

    fn apply_auth(core: &OpenAiCompatCore<Self>, builder: RequestBuilder) -> RequestBuilder {
        builder.bearer_auth(&core.api_key).header("Accept-Language", "en-US,en")
    }
}

impl_openai_compat_provider!(ZAIProvider, ZaiSpec, {
    fn supports_reasoning(&self, model: &str) -> bool {
        // Codex-inspired robustness: Setting model_supports_reasoning to false
        // does NOT disable it for known reasoning models.
        model.contains("glm")
            || self
                .core
                .model_behavior
                .as_ref()
                .and_then(|b| b.model_supports_reasoning)
                .unwrap_or(false)
    }

    fn supports_reasoning_effort(&self, model: &str) -> bool {
        // Same robustness logic for reasoning effort
        model.contains("glm")
            || self
                .core
                .model_behavior
                .as_ref()
                .and_then(|b| b.model_supports_reasoning_effort)
                .unwrap_or(false)
    }
});

#[cfg(test)]
mod tests {
    use super::{ZAIProvider, resolve_zai_base_url};
    use crate::provider::{LLMRequest, Message, ToolChoice, ToolDefinition};
    use std::sync::Arc;
    use vtcode_config::constants::models;
    use vtcode_config::types::ReasoningEffortLevel;

    #[test]
    fn payload_includes_top_p() {
        let provider = ZAIProvider::new("test-key".to_string());
        let request = LLMRequest {
            model: models::zai::GLM_5_2.to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            top_p: Some(0.95),
            ..Default::default()
        };

        let payload = provider.core.convert_request(&request).expect("payload should be valid");
        let top_p = payload.get("top_p").and_then(|v| v.as_f64()).expect("top_p should be present");
        assert!((top_p - 0.95).abs() < 1e-6);
    }

    #[test]
    fn payload_enables_tool_stream_when_streaming_with_tools() {
        let provider = ZAIProvider::new("test-key".to_string());
        let request = LLMRequest {
            model: models::zai::GLM_5_2.to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            stream: true,
            tools: Some(Arc::new(vec![ToolDefinition::function(
                "get_weather".to_string(),
                "Get weather".to_string(),
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "location": {"type": "string"}
                    },
                    "required": ["location"]
                }),
            )])),
            ..Default::default()
        };

        let payload = provider.core.convert_request(&request).expect("payload should be valid");
        assert_eq!(payload.get("stream").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(payload.get("tool_stream").and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn payload_streaming_without_tools_does_not_set_tool_stream() {
        let provider = ZAIProvider::new("test-key".to_string());
        let request = LLMRequest {
            model: models::zai::GLM_5_2.to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            stream: true,
            ..Default::default()
        };

        let payload = provider.core.convert_request(&request).expect("payload should be valid");
        assert_eq!(payload.get("stream").and_then(|v| v.as_bool()), Some(true));
        assert!(payload.get("tool_stream").is_none());
    }

    #[test]
    fn zai_base_url_uses_explicit_override() {
        let resolved = resolve_zai_base_url(Some("https://api.z.ai/api/coding/paas/v4".to_string()));
        assert_eq!(resolved, "https://api.z.ai/api/coding/paas/v4");
    }

    #[test]
    fn payload_includes_do_sample() {
        let provider = ZAIProvider::new("test-key".to_string());
        let request = LLMRequest {
            model: models::zai::GLM_5_2.to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            do_sample: Some(false),
            ..Default::default()
        };

        let payload = provider.core.convert_request(&request).expect("payload should be valid");
        assert_eq!(payload.get("do_sample").and_then(|v| v.as_bool()), Some(false));
    }

    #[test]
    fn payload_disables_thinking_for_none_effort() {
        let provider = ZAIProvider::new("test-key".to_string());
        let request = LLMRequest {
            model: models::zai::GLM_5_2.to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            reasoning_effort: Some(ReasoningEffortLevel::None),
            ..Default::default()
        };

        let payload = provider.core.convert_request(&request).expect("payload should be valid");
        assert_eq!(payload.get("thinking").and_then(|v| v.get("type")).and_then(|v| v.as_str()), Some("disabled"));
    }

    #[test]
    fn payload_enables_thinking_for_low_effort() {
        let provider = ZAIProvider::new("test-key".to_string());
        let request = LLMRequest {
            model: models::zai::GLM_5_2.to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            reasoning_effort: Some(ReasoningEffortLevel::Low),
            ..Default::default()
        };

        let payload = provider.core.convert_request(&request).expect("payload should be valid");
        assert_eq!(payload.get("thinking").and_then(|v| v.get("type")).and_then(|v| v.as_str()), Some("enabled"));
        assert_eq!(payload.get("reasoning_effort").and_then(|v| v.as_str()), Some("low"));
    }

    #[test]
    fn payload_rejects_unsupported_xhigh_and_preserves_max() {
        let provider = ZAIProvider::new("test-key".to_string());
        let unsupported = LLMRequest {
            model: models::zai::GLM_5_3.to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            reasoning_effort: Some(ReasoningEffortLevel::XHigh),
            ..Default::default()
        };
        let error = provider
            .core
            .convert_request(&unsupported)
            .expect_err("unsupported effort must be blocked before transport");
        assert!(error.to_string().contains("unsupported"));

        let supported = LLMRequest {
            reasoning_effort: Some(ReasoningEffortLevel::Max),
            ..unsupported
        };
        let payload = provider.core.convert_request(&supported).expect("payload should be valid");
        assert_eq!(payload.get("thinking").and_then(|v| v.get("type")).and_then(|v| v.as_str()), Some("enabled"));
        assert_eq!(payload.get("reasoning_effort").and_then(|v| v.as_str()), Some("max"));
    }

    #[test]
    fn payload_enables_preserved_thinking_when_reasoning_history_present() {
        let provider = ZAIProvider::new("test-key".to_string());
        let mut assistant = Message::assistant("tool planning".to_string());
        assistant.reasoning = Some("reason step 1".to_string());

        let request = LLMRequest {
            model: models::zai::GLM_5_2.to_string(),
            messages: vec![assistant].into(),
            ..Default::default()
        };

        let payload = provider.core.convert_request(&request).expect("payload should be valid");
        assert_eq!(payload.get("thinking").and_then(|v| v.get("type")).and_then(|v| v.as_str()), Some("enabled"));
        assert_eq!(
            payload
                .get("thinking")
                .and_then(|v| v.get("clear_thinking"))
                .and_then(|v| v.as_bool()),
            Some(false)
        );
    }

    #[test]
    fn payload_serializes_assistant_reasoning_content() {
        let provider = ZAIProvider::new("test-key".to_string());
        let mut assistant = Message::assistant("answer".to_string());
        assistant.reasoning = Some("chain".to_string());

        let request = LLMRequest {
            model: models::zai::GLM_5_2.to_string(),
            messages: vec![assistant].into(),
            ..Default::default()
        };

        let payload = provider.core.convert_request(&request).expect("payload should be valid");
        let messages = payload
            .get("messages")
            .and_then(|v| v.as_array())
            .expect("messages should be serialized");
        let first = messages.first().expect("at least one message");
        assert_eq!(first.get("reasoning_content").and_then(|v| v.as_str()), Some("chain"));
    }

    #[test]
    fn payload_serializes_web_search_tool() {
        let provider = ZAIProvider::new("test-key".to_string());
        let request = LLMRequest {
            model: models::zai::GLM_5_2.to_string(),
            messages: vec![Message::user("latest economic events".to_string())].into(),
            tools: Some(Arc::new(vec![ToolDefinition::web_search(serde_json::json!({
                "enable": true,
                "search_engine": "search-prime",
                "count": 5
            }))])),
            ..Default::default()
        };

        let payload = provider.core.convert_request(&request).expect("payload should be valid");
        let tools = payload
            .get("tools")
            .and_then(|v| v.as_array())
            .expect("tools should be serialized");
        let first = tools.first().expect("at least one tool");
        assert_eq!(first.get("type").and_then(|v| v.as_str()), Some("web_search"));
        assert_eq!(
            first
                .get("web_search")
                .and_then(|v| v.get("search_engine"))
                .and_then(|v| v.as_str()),
            Some("search-prime")
        );
    }

    #[test]
    fn payload_tool_choice_auto_when_requested() {
        let provider = ZAIProvider::new("test-key".to_string());
        let request = LLMRequest {
            model: models::zai::GLM_5_2.to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            tool_choice: Some(ToolChoice::auto()),
            ..Default::default()
        };

        let payload = provider.core.convert_request(&request).expect("payload should be valid");
        assert_eq!(payload.get("tool_choice").and_then(|v| v.as_str()), Some("auto"));
    }

    #[test]
    fn payload_forces_tool_choice_to_auto_for_non_auto_permissions() {
        let provider = ZAIProvider::new("test-key".to_string());
        let request = LLMRequest {
            model: models::zai::GLM_5_2.to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            tool_choice: Some(ToolChoice::none()),
            ..Default::default()
        };

        let payload = provider.core.convert_request(&request).expect("payload should be valid");
        assert_eq!(payload.get("tool_choice").and_then(|v| v.as_str()), Some("auto"));
    }

    #[test]
    fn payload_defaults_tool_choice_to_auto_when_tools_provided() {
        let provider = ZAIProvider::new("test-key".to_string());
        let request = LLMRequest {
            model: models::zai::GLM_5_2.to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            tools: Some(Arc::new(vec![ToolDefinition::function(
                "get_weather".to_string(),
                "Get weather".to_string(),
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "location": {"type": "string"}
                    },
                    "required": ["location"]
                }),
            )])),
            ..Default::default()
        };

        let payload = provider.core.convert_request(&request).expect("payload should be valid");
        assert_eq!(payload.get("tool_choice").and_then(|v| v.as_str()), Some("auto"));
    }

    #[test]
    fn payload_enables_json_mode_when_output_format_requested() {
        let provider = ZAIProvider::new("test-key".to_string());
        let request = LLMRequest {
            model: models::zai::GLM_5_2.to_string(),
            messages: vec![Message::user("return json".to_string())].into(),
            output_format: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "sentiment": {"type": "string"}
                }
            })),
            ..Default::default()
        };

        let payload = provider.core.convert_request(&request).expect("payload should be valid");
        assert_eq!(
            payload
                .get("response_format")
                .and_then(|v| v.get("type"))
                .and_then(|v| v.as_str()),
            Some("json_object")
        );
    }

    #[test]
    fn payload_keeps_json_mode_when_thinking_disabled() {
        let provider = ZAIProvider::new("test-key".to_string());
        let request = LLMRequest {
            model: models::zai::GLM_5_2.to_string(),
            messages: vec![Message::user("return json".to_string())].into(),
            output_format: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "sentiment": {"type": "string"}
                }
            })),
            reasoning_effort: Some(ReasoningEffortLevel::None),
            ..Default::default()
        };

        let payload = provider.core.convert_request(&request).expect("payload should be valid");
        assert_eq!(
            payload
                .get("response_format")
                .and_then(|v| v.get("type"))
                .and_then(|v| v.as_str()),
            Some("json_object")
        );
        assert_eq!(payload.get("thinking").and_then(|v| v.get("type")).and_then(|v| v.as_str()), Some("disabled"));
    }
}
