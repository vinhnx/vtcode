use super::super::OpenRouterProvider;
use crate::provider::{
    LLMError, LLMNormalizedStream, LLMProvider, LLMRequest, LLMResponse, LLMStream, LLMStreamEvent,
    NormalizedStreamEvent,
};
use crate::providers::error_handling::{format_network_error, format_parse_error};
use crate::providers::shared::Utf8StreamDecoder;

use async_stream::try_stream;
use async_trait::async_trait;
use futures::StreamExt;
use hashbrown::{HashMap, HashSet};
use serde_json::Value;

use super::super::response_parser;

#[async_trait]
impl LLMProvider for OpenRouterProvider {
    fn name(&self) -> &str {
        "openrouter"
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_reasoning(&self, model: &str) -> bool {
        self.model_behavior
            .as_ref()
            .and_then(|b| b.model_supports_reasoning)
            .or_else(|| vtcode_config::models::model_catalog_entry("openrouter", model).map(|entry| entry.reasoning))
            .unwrap_or(false)
    }

    fn supports_reasoning_effort(&self, model: &str) -> bool {
        self.model_behavior
            .as_ref()
            .and_then(|b| b.model_supports_reasoning_effort)
            .or_else(|| {
                vtcode_config::models::model_catalog_entry("openrouter", model)
                    .map(|entry| !entry.reasoning_efforts.is_empty())
            })
            .unwrap_or(false)
    }

    fn supports_tools(&self, model: &str) -> bool {
        use vtcode_config::constants::models;
        !models::openrouter::TOOL_UNAVAILABLE_MODELS.contains(&model)
    }

    fn effective_context_size(&self, model: &str) -> usize {
        crate::provider::catalog_context_window("openrouter", model, 128_000)
    }

    async fn generate(&self, request: LLMRequest) -> Result<LLMResponse, LLMError> {
        let model = self.resolve_model(&request).to_string();
        let response = self.send_with_fallback(&request, Some(false)).await?;

        let response_json: Value = response.json().await.map_err(|e| format_parse_error("OpenRouter", &e))?;

        let include_cache_metrics = self.prompt_cache_enabled && self.prompt_cache_settings.report_savings;
        response_parser::parse_response(response_json, model, include_cache_metrics)
    }

    async fn stream(&self, request: LLMRequest) -> Result<LLMStream, LLMError> {
        let model = self.resolve_model(&request).to_string();
        let response = self.send_with_fallback(&request, Some(true)).await?;

        let stream = try_stream! {
            let mut body_stream = response.bytes_stream();
            let mut buf: Vec<u8> = Vec::new();
            let mut offset = 0usize;
            let mut decoder = Utf8StreamDecoder::new();
            let mut aggregator = crate::providers::shared::StreamAggregator::new(model);

            while let Some(chunk_result) = body_stream.next().await {
                let chunk = chunk_result.map_err(|e| format_network_error("OpenRouter", &e))?;
                decoder.push_bytes(&chunk, &mut buf);

                while let Some((split_idx, delimiter_len)) = crate::providers::shared::find_sse_boundary_bytes(&buf, offset) {
                    let event = std::str::from_utf8(&buf[offset..split_idx]).expect("valid utf-8 stream data");
                    offset = split_idx + delimiter_len;

                    if let Some(data_payload) = crate::providers::shared::extract_data_payload(event) {
                        let trimmed = data_payload.trim();
                        if trimmed.is_empty() || trimmed == "[DONE]" {
                            continue;
                        }

                        if let Ok(payload) = serde_json::from_str::<Value>(trimmed) {
                            if let Some(choices) = payload.get("choices").and_then(|v| v.as_array()) {
                                if let Some(choice) = choices.first() {
                                    if let Some(delta) = choice.get("delta") {
                                        // Handle dedicated reasoning field (e.g. reasoning_content or reasoning)
                                        if let Some(reasoning) = delta
                                            .get("reasoning_content")
                                            .or_else(|| delta.get("reasoning"))
                                            .and_then(|v| v.as_str())
                                        {
                                            if let Some(delta) = aggregator.handle_reasoning(reasoning) {
                                                yield LLMStreamEvent::Reasoning { delta };
                                            }
                                        }

                                        // Handle standard content field
                                        if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                                            for ev in aggregator.handle_content(content) {
                                                yield ev;
                                            }
                                        }

                                        // Handle structured reasoning_details field
                                        if let Some(reasoning_details) = delta
                                            .get("reasoning_details")
                                            .and_then(|v| v.as_array())
                                        {
                                            let prev_reasoning_len = aggregator.reasoning.len();
                                            aggregator.set_reasoning_details(reasoning_details);

                                            if let Some(new_reasoning) = crate::providers::common::extract_reasoning_text_from_detail_values(reasoning_details) {
                                                if new_reasoning.len() > prev_reasoning_len {
                                                    let delta = new_reasoning[prev_reasoning_len..].to_string();
                                                    if !delta.trim().is_empty() {
                                                        yield LLMStreamEvent::Reasoning { delta };
                                                    }
                                                }
                                            }
                                        }

                                        // Handle tool calls in deltas
                                        if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                                            aggregator.handle_tool_calls(tool_calls);
                                        }
                                    }

                                    if let Some(finish_reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                                        aggregator.set_finish_reason(crate::providers::common::map_finish_reason_common(finish_reason));
                                    }
                                }
                            }

                            if let Some(usage) = crate::providers::common::parse_usage_openai_format(&payload, true) {
                                aggregator.set_usage(usage);
                            }
                        }
                    }
                }

                // Drain the consumed prefix so `buf` stays bounded to the
                // unprocessed tail rather than growing for the entire stream.
                if offset > 0 {
                    buf.drain(..offset);
                    offset = 0;
                }
            }

            yield LLMStreamEvent::Completed { response: Box::new(aggregator.finalize()) };
        };

        Ok(Box::pin(stream))
    }

    async fn stream_normalized(&self, request: LLMRequest) -> Result<LLMNormalizedStream, LLMError> {
        let resolved_model = self.resolve_model(&request).to_string();
        let response = self.send_with_fallback(&request, Some(true)).await?;

        let stream = try_stream! {
            let mut body_stream = response.bytes_stream();
            let mut buf: Vec<u8> = Vec::new();
            let mut offset = 0usize;
            let mut decoder = Utf8StreamDecoder::new();
            let mut aggregator = crate::providers::shared::StreamAggregator::new(resolved_model);
            let mut seen_tool_calls = HashSet::new();
            let mut fabricated_ids: HashMap<usize, String> = HashMap::new();

            while let Some(chunk_result) = body_stream.next().await {
                let chunk = chunk_result.map_err(|e| format_network_error("OpenRouter", &e))?;
                decoder.push_bytes(&chunk, &mut buf);

                while let Some((split_idx, delimiter_len)) =
                    crate::providers::shared::find_sse_boundary_bytes(&buf, offset)
                {
                    let event = std::str::from_utf8(&buf[offset..split_idx]).expect("valid utf-8 stream data");
                    offset = split_idx + delimiter_len;

                    if let Some(data_payload) =
                        crate::providers::shared::extract_data_payload(event)
                    {
                        let trimmed = data_payload.trim();
                        if trimmed.is_empty() || trimmed == "[DONE]" {
                            continue;
                        }

                        if let Ok(payload) = serde_json::from_str::<Value>(trimmed) {
                            if let Some(choices) = payload.get("choices").and_then(|v| v.as_array()) {
                                if let Some(choice) = choices.first() {
                                    if let Some(delta) = choice.get("delta") {
                                        if let Some(reasoning) = delta
                                            .get("reasoning_content")
                                            .or_else(|| delta.get("reasoning"))
                                            .and_then(|v| v.as_str())
                                        {
                                            if let Some(delta) = aggregator.handle_reasoning(reasoning) {
                                                yield NormalizedStreamEvent::ReasoningDelta { delta };
                                            }
                                        }

                                        if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                                            for ev in aggregator.handle_content(content) {
                                                if let LLMStreamEvent::Token { delta } = ev {
                                                    yield NormalizedStreamEvent::TextDelta { delta };
                                                } else if let LLMStreamEvent::Reasoning { delta } = ev {
                                                    yield NormalizedStreamEvent::ReasoningDelta { delta };
                                                }
                                            }
                                        }

                                        if let Some(reasoning_details) = delta
                                            .get("reasoning_details")
                                            .and_then(|v| v.as_array())
                                        {
                                            let prev_reasoning_len = aggregator.reasoning.len();
                                            aggregator.set_reasoning_details(reasoning_details);

                                            if let Some(new_reasoning) =
                                                crate::providers::common::extract_reasoning_text_from_detail_values(reasoning_details)
                                            {
                                                if new_reasoning.len() > prev_reasoning_len {
                                                    let delta = new_reasoning[prev_reasoning_len..].to_string();
                                                    if !delta.trim().is_empty() {
                                                        yield NormalizedStreamEvent::ReasoningDelta { delta };
                                                    }
                                                }
                                            }
                                        }

                                        if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                                            // Patch fabricated ids into the payload handed to the
                                            // aggregator so the ids in streamed events match the
                                            // ids in the finalized response (lifecycle consumers
                                            // correlate by call_id).
                                            let mut patched_tool_calls = tool_calls.clone();
                                            for (position, tool_call) in patched_tool_calls.iter_mut().enumerate() {
                                                let index = tool_call
                                                    .get("index")
                                                    .and_then(|value| value.as_u64())
                                                    .map(|value| value as usize)
                                                    .unwrap_or(position);
                                                let call_id = match tool_call
                                                    .get("id")
                                                    .and_then(|value| value.as_str())
                                                    .filter(|value| !value.is_empty())
                                                    .map(ToOwned::to_owned)
                                                {
                                                    Some(call_id) => call_id,
                                                    None => {
                                                        let call_id = fabricated_ids
                                                            .entry(index)
                                                            .or_insert_with(crate::providers::shared::generate_tool_call_id)
                                                            .clone();
                                                        if let Some(object) = tool_call.as_object_mut() {
                                                            object.insert(
                                                                "id".to_string(),
                                                                Value::String(call_id.clone()),
                                                            );
                                                        }
                                                        call_id
                                                    }
                                                };
                                                if seen_tool_calls.insert(call_id.clone()) {
                                                    let name = tool_call
                                                        .get("function")
                                                        .and_then(|value| value.get("name"))
                                                        .and_then(|value| value.as_str())
                                                        .map(ToOwned::to_owned);
                                                    yield NormalizedStreamEvent::ToolCallStart {
                                                        call_id: call_id.clone(),
                                                        name,
                                                    };
                                                }
                                                if let Some(arguments) = tool_call
                                                    .get("function")
                                                    .and_then(|value| value.get("arguments"))
                                                    .and_then(|value| value.as_str())
                                                {
                                                    if !arguments.is_empty() {
                                                        yield NormalizedStreamEvent::ToolCallDelta {
                                                            call_id: call_id.clone(),
                                                            delta: arguments.to_string(),
                                                        };
                                                    }
                                                }
                                            }
                                            aggregator.handle_tool_calls(&patched_tool_calls);
                                        }
                                    }

                                    if let Some(finish_reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                                        aggregator.set_finish_reason(crate::providers::common::map_finish_reason_common(finish_reason));
                                    }
                                }
                            }

                            if let Some(usage) = crate::providers::common::parse_usage_openai_format(&payload, true) {
                                aggregator.set_usage(usage.clone());
                                yield NormalizedStreamEvent::Usage { usage };
                            }
                        }
                    }
                }

                // Drain the consumed prefix so `buf` stays bounded to the
                // unprocessed tail rather than growing for the entire stream.
                if offset > 0 {
                    buf.drain(..offset);
                    offset = 0;
                }
            }

            yield NormalizedStreamEvent::Done {
                response: Box::new(aggregator.finalize()),
            };
        };

        Ok(Box::pin(stream))
    }

    fn supported_models(&self) -> Vec<String> {
        use vtcode_config::constants::models;
        models::openrouter::SUPPORTED_MODELS.iter().map(|s| s.to_string()).collect()
    }

    fn validate_request(&self, request: &LLMRequest) -> Result<(), LLMError> {
        if request.messages.is_empty() {
            return Err(LLMError::InvalidRequest {
                message: "Messages cannot be empty".to_string(),
                metadata: None,
            });
        }
        Ok(())
    }
}
