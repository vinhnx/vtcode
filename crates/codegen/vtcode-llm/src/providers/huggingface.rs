#![allow(
    clippy::bind_instead_of_map,
    clippy::collapsible_if,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]

use crate::error_display::format_llm_error;
use crate::provider::{
    LLMError, LLMErrorMetadata, LLMProvider, LLMRequest, LLMResponse, LLMStream, LLMStreamEvent, MessageRole,
    ToolDefinition,
};
use crate::providers::shared::{
    NoopStreamTelemetry, StreamTelemetry, Utf8StreamDecoder, function_output_value_from_message_content,
};
use async_stream::try_stream;
use async_trait::async_trait;
use futures::StreamExt;
use reqwest::{Client as HttpClient, Response, StatusCode};
use serde_json::{Value, json};
use vtcode_commons::sanitizer::sanitize_provider_diagnostic;
use vtcode_config::TimeoutsConfig;
use vtcode_config::constants::{env_vars, models, urls};
use vtcode_config::core::{AnthropicConfig, ModelConfig, PromptCachingConfig};

use super::common::{
    assistant_interleaved_history_text, ensure_model, impl_llm_client, is_minimax_m2_model, map_finish_reason_common,
    normalize_reasoning_detail_objects, override_base_url, parse_response_openai_format, resolve_model,
};
use super::error_handling::{format_network_error, format_parse_error};

const PROVIDER_NAME: &str = "HuggingFace";
const PROVIDER_KEY: &str = "huggingface";
const JSON_INSTRUCTION: &str = "Return JSON that matches the provided schema.";

pub struct HuggingFaceProvider {
    api_key: String,
    http_client: HttpClient,
    base_url: String,
    model: String,
    _timeouts: TimeoutsConfig,
    model_behavior: Option<ModelConfig>,
}

impl HuggingFaceProvider {
    pub fn new(api_key: String) -> Self {
        Self::with_model_internal(api_key, models::huggingface::DEFAULT_MODEL.to_string(), None, None, None)
    }

    fn with_model(api_key: String, model: String) -> Self {
        Self::with_model_internal(api_key, model, None, None, None)
    }

    pub fn with_timeouts(api_key: String, timeouts: TimeoutsConfig) -> Self {
        Self::with_model_internal(api_key, models::huggingface::DEFAULT_MODEL.to_string(), None, Some(timeouts), None)
    }

    fn with_model_internal(
        api_key: String,
        model: String,
        base_url: Option<String>,
        timeouts: Option<TimeoutsConfig>,
        model_behavior: Option<ModelConfig>,
    ) -> Self {
        use crate::http_client::HttpClientFactory;

        let timeouts = timeouts.unwrap_or_default();

        Self {
            api_key,
            http_client: HttpClientFactory::for_llm(&timeouts),
            base_url: override_base_url(urls::HUGGINGFACE_API_BASE, base_url, Some(env_vars::HUGGINGFACE_BASE_URL)),
            model,
            _timeouts: timeouts,
            model_behavior,
        }
    }

    pub fn from_config(
        api_key: Option<String>,
        model: Option<String>,
        base_url: Option<String>,
        _prompt_cache: Option<PromptCachingConfig>,
        timeouts: Option<TimeoutsConfig>,
        _anthropic: Option<AnthropicConfig>,
        model_behavior: Option<ModelConfig>,
    ) -> Self {
        let api_key_value = api_key.unwrap_or_default();
        let model_value = resolve_model(model, models::huggingface::DEFAULT_MODEL);
        Self::with_model_internal(api_key_value, model_value, base_url, timeouts, model_behavior)
    }

    fn normalize_model_id(&self, model: &str) -> Result<String, LLMError> {
        let model = model.trim();
        let lower = model.to_ascii_lowercase();

        if lower.starts_with(&models::huggingface::STEP_3_5_FLASH_BASE.to_ascii_lowercase()) {
            if !model.contains(':') {
                return Ok(format!(
                    "{}:{}",
                    models::huggingface::STEP_3_5_FLASH_BASE,
                    models::huggingface::STEP_3_5_FLASH_PROVIDER
                ));
            }
            if let Some((base, provider)) = model.rsplit_once(':')
                && provider.eq_ignore_ascii_case("fastest")
            {
                return Ok(format!("{}:{}", base, models::huggingface::STEP_3_5_FLASH_PROVIDER));
            }
        }

        if lower.contains("minimax-m2") && !model.contains(':') {
            return Err(LLMError::Provider {
                message: format_llm_error(
                    PROVIDER_NAME,
                    "MiniMax models require explicit provider selection (:novita suffix). \n                    Use 'MiniMaxAI/MiniMax-M2.5:novita'.",
                ),
                metadata: None,
            });
        }

        if lower.contains("glm-5") && !model.contains(':') {
            return Err(LLMError::Provider {
                message: format_llm_error(
                    PROVIDER_NAME,
                    "GLM models require explicit provider selection on HuggingFace.",
                ),
                metadata: None,
            });
        }

        Ok(model.to_string())
    }

    fn serialize_tools_huggingface(&self, tools: &[ToolDefinition]) -> Option<Vec<Value>> {
        crate::providers::common::serialize_tools_openai_format(tools)
    }

    fn serialize_messages_huggingface_chat(&self, request: &LLMRequest) -> Result<Vec<Value>, LLMError> {
        use serde_json::{Map, json};

        let mut messages = Vec::with_capacity(request.messages.len());

        for message in request.messages.iter() {
            message
                .validate_for_provider(PROVIDER_KEY)
                .map_err(|e| LLMError::InvalidRequest { message: e, metadata: None })?;

            let mut message_map = Map::with_capacity(4);
            message_map.insert("role".to_owned(), Value::String(message.role.as_generic_str().to_owned()));

            if let Some(interleaved_content) = assistant_interleaved_history_text(message, &request.model) {
                message_map.insert("content".to_owned(), Value::String(interleaved_content));
            } else {
                match &message.content {
                    crate::provider::MessageContent::Text(text) => {
                        message_map.insert("content".to_owned(), Value::String(text.clone()));
                    }
                    crate::provider::MessageContent::Parts(parts) => {
                        let has_images = parts.iter().any(crate::provider::ContentPart::is_image);
                        if has_images {
                            let parts_json: Vec<Value> = parts
                            .iter()
                            .map(|part| match part {
                                crate::provider::ContentPart::Text { text } => {
                                    json!({ "type": "text", "text": text })
                                }
                                crate::provider::ContentPart::Image {
                                    data,
                                    mime_type,
                                    ..
                                } => {
                                    json!({
                                        "type": "image_url",
                                        "image_url": {
                                            "url": format!("data:{};base64,{}", mime_type, data)
                                        }
                                    })
                                }
                                crate::provider::ContentPart::File {
                                    filename,
                                    file_id,
                                    file_url,
                                    ..
                                } => {
                                    let fallback = filename
                                        .clone()
                                        .or_else(|| file_id.clone())
                                        .or_else(|| file_url.clone())
                                        .unwrap_or_else(|| "attached file".to_string());
                                    json!({ "type": "text", "text": format!("[File input not directly supported: {}]", fallback) })
                                }
                            })
                            .collect();
                            message_map.insert("content".to_owned(), Value::Array(parts_json));
                        } else {
                            let text = message.content.as_text().into_owned();
                            message_map.insert("content".to_owned(), Value::String(text));
                        }
                    }
                }
            }

            if let Some(tool_calls) = &message.tool_calls {
                let serialized_calls = tool_calls
                    .iter()
                    .filter_map(|call| {
                        call.function.as_ref().map(|func| {
                            json!({
                                "id": &call.id,
                                "type": "function",
                                "function": {
                                    "name": &func.name,
                                    "arguments": &func.arguments
                                }
                            })
                        })
                    })
                    .collect::<Vec<_>>();
                message_map.insert("tool_calls".to_owned(), Value::Array(serialized_calls));
            }

            if let Some(tool_call_id) = &message.tool_call_id {
                message_map.insert("tool_call_id".to_owned(), Value::String(tool_call_id.clone()));
            }

            if message.role == MessageRole::Assistant
                && is_minimax_m2_model(&request.model)
                && let Some(reasoning_details) = &message.reasoning_details
                && !reasoning_details.is_empty()
            {
                let normalized_details = normalize_reasoning_detail_objects(reasoning_details);
                if !normalized_details.is_empty() {
                    message_map.insert("reasoning_details".to_owned(), Value::Array(normalized_details));
                }
            }

            messages.push(Value::Object(message_map));
        }

        Ok(messages)
    }

    fn format_for_chat_completions(&self, request: &LLMRequest) -> Result<Value, LLMError> {
        let mut messages = self.serialize_messages_huggingface_chat(request)?;
        let is_glm = self.is_glm_model(&request.model);

        if let Some(system) = &request.system_prompt {
            let has_system = messages.first().and_then(|m| m.get("role")).and_then(|r| r.as_str()) == Some("system");
            if !has_system {
                messages.insert(
                    0,
                    json!({
                        "role": "system",
                        "content": system
                    }),
                );
            }
        }

        let mut payload = json!({
            "model": request.model,
            "messages": messages,
            "stream": request.stream,
        });

        if request.stream && request.tools.is_some() && is_glm {
            payload["tool_stream"] = json!(true);
        }

        if let Some(max_tokens) = request.max_tokens {
            payload["max_tokens"] = json!(max_tokens);
        }

        if let Some(tools) = &request.tools {
            if let Some(serialized) = self.serialize_tools_huggingface(tools) {
                payload["tools"] = json!(serialized);

                if let Some(choice) = &request.tool_choice {
                    payload["tool_choice"] = choice.to_provider_format("openai");
                }
            }
        }

        if let Some(temperature) = request.temperature {
            payload["temperature"] = json!(super::common::sampling_param_f64(temperature));
        }

        if let Some(top_p) = request.top_p {
            payload["top_p"] = json!(super::common::sampling_param_f64(top_p));
        }

        if let Some(top_k) = request.top_k {
            payload["top_k"] = json!(top_k);
        }

        if let Some(effort) = request.reasoning_effort {
            use crate::rig_adapter::RigProviderCapabilities;
            use vtcode_config::models::Provider;
            let supported = self.supported_reasoning_efforts(&request.model);
            if let Some(reasoning_params) = RigProviderCapabilities::new(Provider::HuggingFace, &request.model)
                .reasoning_parameters_for_supported_efforts(effort, supported)?
            {
                if let Some(params_obj) = reasoning_params.as_object() {
                    for (k, v) in params_obj {
                        payload[k] = v.clone();
                    }
                }
            }
        }

        if request.output_format.is_some() && !is_glm {
            payload["response_format"] = json!({ "type": "json_object" });
        }

        Ok(payload)
    }

    fn is_glm_model(&self, model: &str) -> bool {
        let lower = model.to_ascii_lowercase();
        lower.contains("glm")
    }

    fn is_deepseek_model(&self, model: &str) -> bool {
        let lower = model.to_ascii_lowercase();
        lower.contains("deepseek")
    }

    fn is_minimax_model(&self, model: &str) -> bool {
        let lower = model.to_ascii_lowercase();
        lower.contains("minimax")
    }

    fn apply_model_defaults(&self, request: &mut LLMRequest) {
        if self.is_minimax_model(&request.model) {
            if request.temperature.is_none() {
                request.temperature = Some(1.0);
            }
            if request.top_p.is_none() {
                request.top_p = Some(0.95);
            }
            if request.top_k.is_none() {
                request.top_k = Some(40);
            }
        }
    }

    fn add_json_instruction(&self, payload: &mut Value) -> Result<(), LLMError> {
        if let Some(instructions) = payload.get_mut("instructions") {
            if let Some(text) = instructions.as_str() {
                if !text.contains("Return JSON") {
                    *instructions = json!(format!("{}\n\n{}", text, JSON_INSTRUCTION));
                }
            }
        } else {
            payload["instructions"] = json!(JSON_INSTRUCTION);
        }

        Ok(())
    }

    fn format_for_responses_api(&self, request: &LLMRequest) -> Result<Value, LLMError> {
        let mut input = Vec::new();

        for msg in request.messages.iter() {
            let convert_parts = |parts: &[crate::provider::ContentPart]| -> Value {
                let parts_json: Vec<Value> = parts
                    .iter()
                    .map(|part| match part {
                        crate::provider::ContentPart::Text { text } => {
                            json!({ "type": "input_text", "text": text })
                        }
                        crate::provider::ContentPart::Image { data, mime_type, .. } => {
                            json!({
                                "type": "input_image",
                                "image_url": format!("data:{};base64,{}", mime_type, data)
                            })
                        }
                        crate::provider::ContentPart::File { filename, file_id, file_url, .. } => {
                            let fallback = filename
                                .clone()
                                .or_else(|| file_id.clone())
                                .or_else(|| file_url.clone())
                                .unwrap_or_else(|| "attached file".to_string());
                            json!({
                                "type": "input_text",
                                "text": format!("[File input not directly supported: {}]", fallback)
                            })
                        }
                    })
                    .collect();
                json!(parts_json)
            };

            match msg.role {
                MessageRole::System | MessageRole::User => {
                    if msg.role == MessageRole::System && request.system_prompt.is_some() {
                        if let crate::provider::MessageContent::Text(text) = &msg.content {
                            if request.system_prompt.as_ref().map(|s| s.as_ref()) == Some(text.as_str()) {
                                continue;
                            }
                        }
                    }

                    let role = if msg.role == MessageRole::System {
                        "system"
                    } else {
                        "user"
                    };

                    let mut message_obj = json!({
                        "type": "message",
                        "role": role,
                    });

                    match &msg.content {
                        crate::provider::MessageContent::Text(text) => {
                            message_obj["content"] = json!(text);
                        }
                        crate::provider::MessageContent::Parts(parts) => {
                            message_obj["content"] = convert_parts(parts);
                        }
                    }

                    input.push(message_obj);
                }
                MessageRole::Assistant => {
                    let has_content = match &msg.content {
                        crate::provider::MessageContent::Text(text) => !text.is_empty(),
                        crate::provider::MessageContent::Parts(parts) => !parts.is_empty(),
                    };

                    if has_content {
                        let mut message_obj = json!({
                            "type": "message",
                            "role": "assistant",
                        });

                        match &msg.content {
                            crate::provider::MessageContent::Text(text) => {
                                message_obj["content"] = json!(text);
                            }
                            crate::provider::MessageContent::Parts(parts) => {
                                message_obj["content"] = convert_parts(parts);
                            }
                        }

                        input.push(message_obj);
                    }

                    if let Some(tool_calls) = &msg.tool_calls {
                        for tc in tool_calls {
                            if let Some(func) = &tc.function {
                                input.push(json!({
                                    "type": "function_call",
                                    "call_id": tc.id,
                                    "name": func.name,
                                    "arguments": func.arguments
                                }));
                            }
                        }
                    }
                }
                MessageRole::Tool => {
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": msg.tool_call_id.clone().unwrap_or_default(),
                        "output": function_output_value_from_message_content(&msg.content)
                    }));
                }
            }
        }

        let mut payload = json!({
            "model": request.model,
            "input": input,
            "stream": request.stream,
        });

        if let Some(system_prompt) = &request.system_prompt {
            payload["instructions"] = json!(system_prompt);
        }

        if let Some(effort) = request.reasoning_effort {
            use vtcode_config::types::ReasoningEffortLevel;
            if effort != ReasoningEffortLevel::None {
                payload["reasoning"] = json!({ "effort": effort.as_str() });
            }
        }

        if let Some(max_tokens) = request.max_tokens {
            payload["max_tokens"] = json!(max_tokens);
        }
        if let Some(temperature) = request.temperature {
            payload["temperature"] = json!(super::common::sampling_param_f64(temperature));
        }
        if let Some(top_p) = request.top_p {
            payload["top_p"] = json!(super::common::sampling_param_f64(top_p));
        }
        if let Some(top_k) = request.top_k {
            payload["top_k"] = json!(top_k);
        }

        if let Some(tools) = &request.tools {
            if let Some(serialized) = self.serialize_tools_huggingface(tools) {
                payload["tools"] = json!(serialized);

                if let Some(choice) = &request.tool_choice {
                    payload["tool_choice"] = choice.to_provider_format("openai");
                }
            }
        }

        if request.output_format.is_some() || request.tools.is_some() {
            self.add_json_instruction(&mut payload)?;
        }

        if request.output_format.is_some() && !self.is_glm_model(&request.model) {
            payload["response_format"] = json!({ "type": "json_object" });
        }

        Ok(payload)
    }

    fn should_use_responses_api(&self, _request: &LLMRequest) -> bool {
        false
    }

    fn format_error(&self, status: StatusCode, body: &str) -> LLMError {
        let message = if body.contains("\"code\":\"model_not_supported\"")
            && body.contains(models::huggingface::STEP_3_5_FLASH_BASE)
        {
            format!(
                "HuggingFace API error ({}): Step 3.5 Flash requires the '{}' provider. \
Enable that provider in your HuggingFace Inference Providers settings, or switch to another model.",
                status,
                models::huggingface::STEP_3_5_FLASH_PROVIDER
            )
        } else {
            format!("HuggingFace API error ({status}): {body}")
        };

        LLMError::Provider {
            message: format_llm_error(PROVIDER_NAME, &message),
            metadata: Some(LLMErrorMetadata::new(
                PROVIDER_NAME,
                Some(status.as_u16()),
                None,
                None,
                None,
                None,
                Some(sanitize_provider_diagnostic(body.as_bytes())),
            )),
        }
    }

    fn parse_responses_api_format(json: &Value, model: String) -> Result<LLMResponse, LLMError> {
        let convenience_text = json.get("output_text").and_then(|t| t.as_str());

        let json_obj = json.get("response").unwrap_or(json);

        let output = json_obj.get("output").and_then(|v| v.as_array());

        let output_arr = match output {
            Some(arr) => arr,
            None => {
                if let Some(text) = convenience_text {
                    return Ok(LLMResponse {
                        content: Some(text.to_string()),
                        tool_calls: None,
                        model,
                        usage: None,
                        finish_reason: crate::provider::FinishReason::Stop,
                        reasoning: None,
                        reasoning_details: None,
                        tool_references: Vec::new(),
                        request_id: None,
                        organization_id: None,
                        compaction: None,
                    });
                }

                return Err(LLMError::Provider {
                    message: format_llm_error(PROVIDER_NAME, "Not a Responses API format"),
                    metadata: None,
                });
            }
        };

        let mut content_fragments: Vec<String> = Vec::new();
        let mut reasoning_fragments: Vec<String> = Vec::new();
        let mut tool_calls: Vec<crate::provider::ToolCall> = Vec::new();

        for item in output_arr {
            let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");

            match item_type {
                "message" => {
                    if let Some(content_arr) = item.get("content").and_then(|c| c.as_array()) {
                        for entry in content_arr {
                            let entry_type = entry.get("type").and_then(|t| t.as_str()).unwrap_or("");
                            match entry_type {
                                "text" | "output_text" => {
                                    if let Some(text) = entry.get("text").and_then(|t| t.as_str()) {
                                        if !text.is_empty() {
                                            content_fragments.push(text.to_string());
                                        }
                                    }
                                }
                                "reasoning" => {
                                    if let Some(text) = entry.get("text").and_then(|t| t.as_str()) {
                                        if !text.is_empty() {
                                            reasoning_fragments.push(text.to_string());
                                        }
                                    }
                                }
                                "function_call" | "tool_call" => {
                                    if let Some(call) = Self::parse_responses_tool_call(entry) {
                                        tool_calls.push(call);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                "function_call" | "tool_call" => {
                    if let Some(call) = Self::parse_responses_tool_call(item) {
                        tool_calls.push(call);
                    }
                }
                "reasoning" => {
                    if let Some(summary_arr) = item.get("summary").and_then(|s| s.as_array()) {
                        for summary in summary_arr {
                            if let Some(text) = summary.get("text").and_then(|t| t.as_str()) {
                                if !text.is_empty() {
                                    reasoning_fragments.push(text.to_string());
                                }
                            }
                        }
                    } else if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                        reasoning_fragments.push(text.to_string());
                    }
                }
                _ => {}
            }
        }

        let content = if content_fragments.is_empty() {
            convenience_text.map(|t| t.to_string())
        } else {
            Some(content_fragments.join(""))
        };

        let reasoning = if reasoning_fragments.is_empty() {
            None
        } else {
            Some(reasoning_fragments.join("\n\n"))
        };

        let finish_reason = if !tool_calls.is_empty() {
            crate::provider::FinishReason::ToolCalls
        } else {
            crate::provider::FinishReason::Stop
        };

        let usage_value = json.get("usage").or_else(|| json_obj.get("usage"));
        let usage = usage_value.map(|usage_value| crate::provider::Usage {
            prompt_tokens: usage_value
                .get("input_tokens")
                .or_else(|| usage_value.get("prompt_tokens"))
                .and_then(|pt| pt.as_u64())
                .unwrap_or(0) as u32,
            completion_tokens: usage_value
                .get("output_tokens")
                .or_else(|| usage_value.get("completion_tokens"))
                .and_then(|ct| ct.as_u64())
                .unwrap_or(0) as u32,
            total_tokens: usage_value.get("total_tokens").and_then(|tt| tt.as_u64()).unwrap_or(0) as u32,
            cached_prompt_tokens: None,
            cache_creation_tokens: None,
            cache_read_tokens: None,
            iterations: None,
        });

        Ok(LLMResponse {
            content,
            tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
            model,
            usage,
            finish_reason,
            reasoning,
            reasoning_details: None,
            tool_references: Vec::new(),
            request_id: None,
            organization_id: None,
            compaction: None,
        })
    }

    fn parse_responses_tool_call(item: &Value) -> Option<crate::provider::ToolCall> {
        let call_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let function_obj = item.get("function").and_then(|v| v.as_object());
        let name = function_obj.and_then(|f| f.get("name").and_then(|n| n.as_str()))?;
        let arguments = function_obj.and_then(|f| f.get("arguments"));

        let serialized = arguments.map_or("{}".to_owned(), |args| {
            if args.is_string() {
                args.as_str().unwrap_or("{}").to_string()
            } else {
                args.to_string()
            }
        });

        Some(crate::provider::ToolCall::function(call_id.to_string(), name.to_string(), serialized))
    }

    async fn parse_response(
        &self,
        response: Response,
        model: String,
        use_responses_api: bool,
    ) -> Result<LLMResponse, LLMError> {
        let status = response.status();

        if !status.is_success() {
            let body = crate::providers::common::read_provider_error_body(response).await;
            return Err(self.format_error(status, &body));
        }

        let json: Value = response.json().await.map_err(|err| format_parse_error(PROVIDER_NAME, &err))?;

        if use_responses_api {
            if json.get("output").is_some() {
                return Self::parse_responses_api_format(&json, model);
            }
        }

        parse_response_openai_format::<fn(&Value, &Value) -> Option<String>>(json, PROVIDER_NAME, model, false, None)
    }

    fn available_models() -> Vec<String> {
        models::huggingface::SUPPORTED_MODELS.iter().map(|s| s.to_string()).collect()
    }

    fn get_endpoint(&self, use_responses_api: bool) -> String {
        let base = self.base_url.trim_end_matches('/');
        if use_responses_api {
            format!("{base}/responses")
        } else {
            super::common::chat_completions_url(base)
        }
    }
}

#[async_trait]
impl LLMProvider for HuggingFaceProvider {
    fn name(&self) -> &str {
        PROVIDER_KEY
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_reasoning(&self, model: &str) -> bool {
        // Codex-inspired robustness: Setting model_supports_reasoning to false
        // does NOT disable it for known reasoning models.
        models::huggingface::REASONING_MODELS.contains(&model)
            || self
                .model_behavior
                .as_ref()
                .and_then(|b| b.model_supports_reasoning)
                .unwrap_or(false)
    }

    fn supports_reasoning_effort(&self, model: &str) -> bool {
        // Same robustness logic for reasoning effort
        self.is_glm_model(model)
            || self.is_deepseek_model(model)
            || self
                .model_behavior
                .as_ref()
                .and_then(|b| b.model_supports_reasoning_effort)
                .unwrap_or(false)
    }

    fn supports_tools(&self, _model: &str) -> bool {
        true
    }

    fn supports_parallel_tool_config(&self, _model: &str) -> bool {
        false
    }

    fn supports_structured_output(&self, _model: &str) -> bool {
        true
    }

    fn supports_context_caching(&self, _model: &str) -> bool {
        false
    }

    fn effective_context_size(&self, model: &str) -> usize {
        crate::provider::catalog_context_window("huggingface", model, 128_000)
    }

    async fn generate(&self, mut request: LLMRequest) -> Result<LLMResponse, LLMError> {
        let model = ensure_model(&mut request, &self.model);

        self.apply_model_defaults(&mut request);
        self.validate_request(&request)?;

        let model_id = self.normalize_model_id(&request.model)?;
        request.model = model_id;

        let use_responses_api = self.should_use_responses_api(&request);
        let payload = if use_responses_api {
            self.format_for_responses_api(&request)?
        } else {
            self.format_for_chat_completions(&request)?
        };

        let endpoint = self.get_endpoint(use_responses_api);

        let response = self
            .http_client
            .post(&endpoint)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&payload)
            .send()
            .await
            .map_err(|err| format_network_error(PROVIDER_NAME, &err))?;

        self.parse_response(response, model, use_responses_api).await
    }

    async fn stream(&self, mut request: LLMRequest) -> Result<LLMStream, LLMError> {
        let model = ensure_model(&mut request, &self.model);

        self.apply_model_defaults(&mut request);
        self.validate_request(&request)?;
        request.stream = true;

        let model_id = self.normalize_model_id(&request.model)?;
        request.model = model_id;

        let use_responses_api = self.should_use_responses_api(&request);
        let payload = if use_responses_api {
            self.format_for_responses_api(&request)?
        } else {
            self.format_for_chat_completions(&request)?
        };

        let endpoint = self.get_endpoint(use_responses_api);

        let response = self
            .http_client
            .post(&endpoint)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&payload)
            .send()
            .await
            .map_err(|err| format_network_error(PROVIDER_NAME, &err))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = crate::providers::common::read_provider_error_body(response).await;
            return Err(self.format_error(status, &body));
        }

        self.create_stream(response, model, use_responses_api).await
    }

    fn supported_models(&self) -> Vec<String> {
        Self::available_models()
    }

    fn validate_request(&self, request: &LLMRequest) -> Result<(), LLMError> {
        if request.messages.is_empty() {
            return Err(LLMError::InvalidRequest {
                message: format_llm_error(PROVIDER_NAME, "Messages cannot be empty"),
                metadata: None,
            });
        }

        if request.model.trim().is_empty() {
            return Err(LLMError::InvalidRequest {
                message: format_llm_error(PROVIDER_NAME, "Model identifier cannot be empty"),
                metadata: None,
            });
        }

        Ok(())
    }
}

impl HuggingFaceProvider {
    async fn create_stream(
        &self,
        response: Response,
        model: String,
        use_responses_api: bool,
    ) -> Result<LLMStream, LLMError> {
        let mut bytes_stream = response.bytes_stream();
        let mut buffer = String::with_capacity(4096);
        let mut decoder = Utf8StreamDecoder::new();
        let mut aggregator = crate::providers::shared::StreamAggregator::new(model.clone());
        let telemetry = NoopStreamTelemetry;

        let stream = try_stream! {
            'outer: while let Some(chunk_result) = bytes_stream.next().await {
                let chunk = chunk_result.map_err(|err| format_network_error(PROVIDER_NAME, &err))?;
                buffer.push_str(&decoder.push(&chunk));

                if buffer.len() > 128_000 {
                    Err(LLMError::Provider {
                        message: format_llm_error(PROVIDER_NAME, "Stream buffer exceeded maximum size (128KB)"),
                        metadata: None,
                    })?;
                }

                while let Some(newline_pos) = buffer.find('\n') {
                    // Borrow the line from `buffer` instead of copying it into a
                    // `String` per SSE event. The `drain` is deferred until
                    // `serde_json::from_str` produces an owned `Value`, so the
                    // borrow (`line`/`data`) is dead before `buffer` mutates.
                    let line = buffer[..newline_pos].trim();

                    if line.is_empty() || line.starts_with(':') {
                        buffer.drain(..=newline_pos);
                        continue;
                    }

                    let data = match line.strip_prefix("data: ") {
                        Some(stripped) => stripped,
                        None => {
                            buffer.drain(..=newline_pos);
                            continue;
                        }
                    };

                    if data == "[DONE]" {
                        buffer.drain(..=newline_pos);
                        break 'outer;
                    }

                    let event: Value = match serde_json::from_str(data) {
                        Ok(v) => v,
                        Err(_) => {
                            buffer.drain(..=newline_pos);
                            continue;
                        }
                    };

                    // `event` is now owned; all borrows of `buffer` (via
                    // `line`/`data`) are dead. Safe to drain the consumed line.
                    buffer.drain(..=newline_pos);

                    if use_responses_api {
                        let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");

                        match event_type {
                            "response.output_text.delta" | "output_text.delta" => {
                                if let Some(delta) = event.get("delta").and_then(|d| d.as_str()) {
                                    telemetry.on_content_delta(delta);
                                    for ev in aggregator.handle_content(delta) {
                                        yield ev;
                                    }
                                }
                                continue;
                            }
                            "response.reasoning.delta" | "reasoning.delta" => {
                                if let Some(delta) = event.get("delta").and_then(|d| d.as_str()) {
                                    if let Some(d) = aggregator.handle_reasoning(delta) {
                                        telemetry.on_reasoning_delta(&d);
                                        yield LLMStreamEvent::Reasoning { delta: d };
                                    }
                                }
                                continue;
                            }
                            "response.function_call_arguments.delta" | "tool_call.delta" => {
                                telemetry.on_tool_call_delta();
                                continue;
                            }
                            "response.completed" => {
                                if let Some(response_obj) = event.get("response") {
                                    if let Ok(response) = Self::parse_responses_api_format(response_obj, model.clone()) {
                                        let final_agg_response = aggregator.finalize();
                                        let mut merged_response = response;
                                        if merged_response.content.is_none() {
                                            merged_response.content = final_agg_response.content;
                                        }
                                        if merged_response.reasoning.is_none() {
                                            merged_response.reasoning = final_agg_response.reasoning;
                                        }
                                        if merged_response.tool_calls.is_none() {
                                            merged_response.tool_calls = final_agg_response.tool_calls;
                                        }
                                        if merged_response.usage.is_none() {
                                            merged_response.usage = final_agg_response.usage;
                                        }
                                        yield LLMStreamEvent::Completed { response: Box::new(merged_response) };
                                        return;
                                    }
                                }
                                break 'outer;
                            }
                            "response.done" => {
                                break 'outer;
                            }
                            _ => {}
                        }
                    }

                    if let Some(choices_arr) = event.get("choices").and_then(|c| c.as_array()) {
                        if let Some(choice) = choices_arr.first() {
                            if let Some(delta_obj) = choice.get("delta") {
                                if let Some(content) = delta_obj.get("content").and_then(|c| c.as_str()) {
                                    telemetry.on_content_delta(content);
                                    for ev in aggregator.handle_content(content) {
                                        yield ev;
                                    }
                                }

                                if let Some(reason) = delta_obj.get("reasoning_content").and_then(|r| r.as_str()) {
                                    if let Some(d) = aggregator.handle_reasoning(reason) {
                                        telemetry.on_reasoning_delta(&d);
                                        yield LLMStreamEvent::Reasoning { delta: d };
                                    }
                                }

                                if let Some(reasoning_details) = delta_obj
                                    .get("reasoning_details")
                                    .and_then(|details| details.as_array())
                                {
                                    aggregator.set_reasoning_details(reasoning_details);
                                }

                                if let Some(tool_calls_arr) = delta_obj.get("tool_calls").and_then(|tc| tc.as_array()) {
                                    aggregator.handle_tool_calls(tool_calls_arr);
                                    telemetry.on_tool_call_delta();
                                }
                            }

                            if let Some(finish_reason_str) = choice.get("finish_reason").and_then(|fr| fr.as_str()) {
                                aggregator.set_finish_reason(map_finish_reason_common(finish_reason_str));
                                if let Some(usage_value) = event.get("usage") {
                                    aggregator.set_usage(crate::provider::Usage {
                                        prompt_tokens: usage_value.get("prompt_tokens").and_then(|pt| pt.as_u64()).unwrap_or(0) as u32,
                                        completion_tokens: usage_value.get("completion_tokens").and_then(|ct| ct.as_u64()).unwrap_or(0) as u32,
                                        total_tokens: usage_value.get("total_tokens").and_then(|tt| tt.as_u64()).unwrap_or(0) as u32,
                                        cached_prompt_tokens: None,
                                        cache_creation_tokens: None,
                                        cache_read_tokens: None,
                                        iterations: None,
                                    });
                                }

                                break 'outer;
                            }
                        }
                    }
                }
            }

            yield LLMStreamEvent::Completed { response: Box::new(aggregator.finalize()) };
        };

        Ok(Box::pin(stream))
    }
}

impl_llm_client!(HuggingFaceProvider);

#[cfg(test)]
mod tests {
    use super::HuggingFaceProvider;
    use crate::provider::{LLMRequest, Message, ToolDefinition};
    use crate::providers::common::{is_minimax_m2_model, normalize_reasoning_detail_object};
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn minimax_model_detection_handles_variants() {
        assert!(is_minimax_m2_model("MiniMaxAI/MiniMax-M2.5:novita"));
        assert!(is_minimax_m2_model("minimax-m2.5"));
        assert!(!is_minimax_m2_model("deepseek-r1"));
    }

    #[test]
    fn normalize_reasoning_detail_decodes_stringified_object() {
        let parsed = normalize_reasoning_detail_object(&json!("{\"type\":\"reasoning.text\",\"text\":\"step\"}"))
            .expect("expected a parsed reasoning detail object");
        assert!(parsed.is_object());
        assert_eq!(parsed["type"], "reasoning.text");
    }

    #[test]
    fn serialize_messages_normalizes_minimax_reasoning_details() {
        let provider =
            HuggingFaceProvider::with_model("test-key".to_string(), "MiniMaxAI/MiniMax-M2.5:novita".to_string());
        let request = LLMRequest {
            model: "MiniMaxAI/MiniMax-M2.5:novita".to_string(),
            messages: vec![
                Message::assistant("answer".to_string())
                    .with_reasoning_details(Some(vec![json!("{\"type\":\"reasoning.text\",\"text\":\"chain\"}")])),
            ]
            .into(),
            ..Default::default()
        };

        let messages = provider
            .serialize_messages_huggingface_chat(&request)
            .expect("message serialization should succeed");
        assert!(messages[0]["reasoning_details"].is_array());
        assert!(messages[0]["reasoning_details"][0].is_object());
    }

    #[test]
    fn serialize_messages_rehydrates_glm_interleaved_history_into_content() {
        let provider = HuggingFaceProvider::with_model("test-key".to_string(), "zai-org/GLM-5.1:novita".into());
        let request = LLMRequest {
            model: "zai-org/GLM-5.1:novita".to_string(),
            messages: vec![Message::assistant("done".to_string()).with_reasoning(Some("trace".to_string()))].into(),
            ..Default::default()
        };

        let messages = provider
            .serialize_messages_huggingface_chat(&request)
            .expect("message serialization should succeed");

        assert_eq!(messages[0]["content"], json!("<think>trace</think>done"));
    }

    #[test]
    fn normalize_step35_flash_provider_suffix() {
        let provider = HuggingFaceProvider::with_model("test-key".to_string(), "stepfun-ai/Step-3.5-Flash".to_string());

        let normalized = provider
            .normalize_model_id("stepfun-ai/Step-3.5-Flash")
            .expect("normalization should succeed");
        assert_eq!(normalized, "stepfun-ai/Step-3.5-Flash:featherless-ai".to_string());

        let normalized_legacy = provider
            .normalize_model_id("stepfun-ai/Step-3.5-Flash:fastest")
            .expect("legacy suffix normalization should succeed");
        assert_eq!(normalized_legacy, "stepfun-ai/Step-3.5-Flash:featherless-ai".to_string());
    }

    #[test]
    fn format_for_chat_completions_keeps_apply_patch_as_function_tool() {
        let provider =
            HuggingFaceProvider::with_model("test-key".to_string(), "Qwen/Qwen3-Coder-480B-A35B-Instruct".to_string());
        let request = LLMRequest {
            model: "Qwen/Qwen3-Coder-480B-A35B-Instruct".to_string(),
            messages: vec![Message::user("apply a patch".to_string())].into(),
            tools: Some(Arc::new(vec![ToolDefinition::apply_patch("Apply patches".to_string())])),
            ..Default::default()
        };

        let payload = provider
            .format_for_chat_completions(&request)
            .expect("payload should serialize");

        assert_eq!(payload["tools"][0]["type"], "function");
        assert_eq!(payload["tools"][0]["function"]["name"], "apply_patch");
    }
}
