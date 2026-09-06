use serde_json::{Value, json};

use crate::error_display;
use crate::provider::{LLMError, LLMProvider, LLMRequest, MessageRole};
use crate::providers::common::{
    assistant_interleaved_history_text, normalize_reasoning_detail_objects, serialize_message_content_openai_for_role,
};
use crate::rig_adapter::RigProviderCapabilities;
use vtcode_config::models::Provider;

use super::OpenRouterProvider;

impl OpenRouterProvider {
    pub(super) fn convert_to_openrouter_format(&self, request: &LLMRequest) -> Result<Value, LLMError> {
        let resolved_model = self.resolve_model(request);
        let mut messages = Vec::new();

        if let Some(system_prompt) = &request.system_prompt {
            messages.push(json!({
                "role": vtcode_config::constants::message_roles::SYSTEM,
                "content": system_prompt
            }));
        }

        for msg in request.messages.iter() {
            let role = msg.role.as_openai_str();
            let content_value = assistant_interleaved_history_text(msg, resolved_model)
                .map(Value::String)
                .unwrap_or_else(|| serialize_message_content_openai_for_role(&msg.role, &msg.content));

            let mut message = json!({
                "role": role,
                "content": content_value
            });

            if msg.role == MessageRole::Assistant {
                if let Some(tool_calls) = &msg.tool_calls
                    && !tool_calls.is_empty()
                {
                    let tool_calls_json: Vec<Value> = tool_calls
                        .iter()
                        .filter_map(|tc| {
                            tc.function.as_ref().map(|func| {
                                json!({
                                    "id": tc.id,
                                    "type": "function",
                                    "function": {
                                        "name": func.name,
                                        "arguments": func.arguments
                                    }
                                })
                            })
                        })
                        .collect();
                    message["tool_calls"] = Value::Array(tool_calls_json);
                }

                if let Some(reasoning_details) = &msg.reasoning_details
                    && !reasoning_details.is_empty()
                    && (crate::providers::common::is_minimax_m2_model(resolved_model)
                        || crate::providers::common::is_interleaved_thinking_model(resolved_model))
                {
                    let normalized_details = normalize_reasoning_detail_objects(reasoning_details);
                    if !normalized_details.is_empty() {
                        message["reasoning_details"] = Value::Array(normalized_details);
                    }
                }
            }

            if msg.role == MessageRole::Tool {
                match &msg.tool_call_id {
                    Some(tool_call_id) => {
                        message["tool_call_id"] = Value::String(tool_call_id.clone());
                    }
                    None => {
                        let formatted_error = error_display::format_llm_error(
                            "OpenRouter",
                            "Tool response message missing required tool_call_id",
                        );
                        return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
                    }
                }
            }

            messages.push(message);
        }

        if messages.is_empty() {
            let formatted_error = error_display::format_llm_error("OpenRouter", "No messages provided");
            return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
        }

        let mut provider_request = json!({
            "model": resolved_model,
            "messages": messages,
            "stream": request.stream
        });

        if let Some(max_tokens) = request.max_tokens {
            provider_request["max_tokens"] = json!(max_tokens);
        }

        if let Some(temperature) = request.temperature {
            provider_request["temperature"] =
                Value::Number(crate::providers::common::float_to_json_number(temperature)?);
        }

        if let Some(tools) = &request.tools
            && !tools.is_empty()
        {
            let tools_json: Vec<Value> = tools
                .iter()
                .filter_map(|tool| {
                    let func = tool.function.as_ref()?;
                    Some(json!({
                        "type": "function",
                        "function": {
                            "name": func.name,
                            "description": func.description,
                            "parameters": func.parameters
                        }
                    }))
                })
                .collect();
            provider_request["tools"] = Value::Array(tools_json);
        }

        if let Some(tool_choice) = &request.tool_choice {
            provider_request["tool_choice"] = tool_choice.to_provider_format("openai");
        }

        if let Some(parallel) = request.parallel_tool_calls {
            provider_request["parallel_tool_calls"] = Value::Bool(parallel);
        }

        if let Some(effort) = request.reasoning_effort {
            let supported = self.supported_reasoning_efforts(resolved_model);
            if let Some(payload) = RigProviderCapabilities::new(Provider::OpenRouter, resolved_model)
                .reasoning_parameters_for_supported_efforts(effort, supported)?
            {
                provider_request["reasoning"] = payload;
            }
        }

        Ok(provider_request)
    }
}

#[cfg(test)]
mod tests {
    use super::OpenRouterProvider;
    use crate::provider::{LLMRequest, Message};
    use crate::providers::common::{is_minimax_m2_model, normalize_reasoning_detail_object};
    use serde_json::json;

    #[test]
    fn openrouter_minimax_model_detection_handles_variants() {
        assert!(is_minimax_m2_model("minimax/minimax-m2.5"));
        assert!(is_minimax_m2_model("MiniMax-M2.5"));
        assert!(!is_minimax_m2_model("openai/gpt-5"));
    }

    #[test]
    fn normalize_reasoning_detail_for_minimax_decodes_stringified_object() {
        let parsed = normalize_reasoning_detail_object(&json!(r#"{"type":"reasoning.text","id":"r1","text":"trace"}"#));
        assert!(parsed.as_ref().is_some_and(|value| value.is_object()));
        assert_eq!(parsed.expect("parsed")["type"], json!("reasoning.text"));
    }

    #[test]
    fn normalize_reasoning_detail_for_minimax_rejects_plain_text() {
        assert!(normalize_reasoning_detail_object(&json!("hello")).is_none());
    }

    #[test]
    fn openrouter_payload_rehydrates_glm_interleaved_history_into_content() {
        let provider = OpenRouterProvider::new("test-key".to_string());
        let request = LLMRequest {
            model: "z-ai/glm-5".to_string(),
            messages: vec![Message::assistant("done".to_string()).with_reasoning(Some("trace".to_string()))].into(),
            ..Default::default()
        };

        let payload = provider
            .convert_to_openrouter_format(&request)
            .expect("payload should serialize");
        let messages = payload["messages"].as_array().expect("messages should be present");

        assert_eq!(messages[0]["content"], json!("<think>trace</think>done"));
    }

    #[test]
    fn openrouter_custom_reasoning_capability_preserves_requested_effort() {
        use vtcode_config::core::ModelConfig;

        let mut model_behavior = ModelConfig::default();
        model_behavior.model_supports_reasoning_effort = Some(true);
        let provider = OpenRouterProvider::from_config(
            Some("test-key".to_owned()),
            Some("custom-route".to_owned()),
            None,
            None,
            None,
            None,
            Some(model_behavior),
        );
        let request = LLMRequest {
            model: "custom-route".to_owned(),
            messages: vec![Message::user("hello".to_owned())].into(),
            reasoning_effort: Some(vtcode_config::types::ReasoningEffortLevel::High),
            ..Default::default()
        };

        let payload = provider
            .convert_to_openrouter_format(&request)
            .expect("custom capability should validate");
        assert_eq!(payload["reasoning"]["effort"], json!("high"));
    }

    #[test]
    fn openrouter_rejects_unsupported_reasoning_instead_of_dropping_it() {
        let provider = OpenRouterProvider::new("test-key".to_string());
        let request = LLMRequest {
            model: "unknown-route".to_owned(),
            messages: vec![Message::user("hello".to_owned())].into(),
            reasoning_effort: Some(vtcode_config::types::ReasoningEffortLevel::High),
            ..Default::default()
        };

        let error = provider
            .convert_to_openrouter_format(&request)
            .expect_err("unsupported reasoning must block the request");
        assert!(error.to_string().contains("unsupported"));
    }

    #[test]
    fn openrouter_payload_serializes_compact_temperature() {
        let provider = OpenRouterProvider::new("test-key".to_string());
        let request = LLMRequest {
            model: "openai/gpt-5".to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            temperature: Some(0.7),
            ..Default::default()
        };

        let payload = provider
            .convert_to_openrouter_format(&request)
            .expect("payload should serialize");

        assert_eq!(payload["temperature"].as_f64(), Some(0.7));
        assert_eq!(
            payload.get("temperature").expect("temperature present").to_string(),
            "0.7",
            "wire form must be compact, not the f32->f64 widening tail"
        );
    }
}
