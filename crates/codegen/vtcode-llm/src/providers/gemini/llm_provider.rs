use super::helpers::InteractionStreamState;
use super::*;
use crate::providers::shared::{StreamAssemblyError, extract_data_payload, find_sse_boundary_bytes};

#[async_trait]
impl LLMProvider for GeminiProvider {
    fn name(&self) -> &str {
        "gemini"
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_reasoning(&self, model: &str) -> bool {
        // Codex-inspired robustness: Setting model_supports_reasoning to false
        // does NOT disable it for known reasoning models.
        models::google::REASONING_MODELS.contains(&model)
            || self
                .model_behavior
                .as_ref()
                .and_then(|b| b.model_supports_reasoning)
                .unwrap_or(false)
    }

    fn supports_reasoning_effort(&self, model: &str) -> bool {
        // Same robustness logic for reasoning effort
        models::google::REASONING_MODELS.contains(&model)
            || self
                .model_behavior
                .as_ref()
                .and_then(|b| b.model_supports_reasoning_effort)
                .unwrap_or(false)
    }

    fn supports_context_caching(&self, model: &str) -> bool {
        models::google::CACHING_MODELS.contains(&model)
    }

    fn effective_context_size(&self, model: &str) -> usize {
        let fallback = if model.contains("gemini-3.1") {
            1_048_576
        } else if model.contains("3") || model.contains("1.5-pro") {
            2_097_152
        } else {
            1_048_576
        };
        crate::provider::catalog_context_window("gemini", model, fallback)
    }

    async fn generate(&self, request: LLMRequest) -> Result<LLMResponse, LLMError> {
        let model = request.model.clone();
        if self.should_use_interactions(&request) {
            let interaction_request = self.convert_to_interaction_request(&request)?;
            let url = format!("{}/interactions", self.base_url);
            let response = self
                .http_client
                .post(&url)
                .header("x-goog-api-key", self.api_key.as_ref())
                .json(&interaction_request)
                .send()
                .await
                .map_err(|e| format_network_error("Gemini", &e))?;

            if !response.status().is_success() {
                let status = response.status();
                let error_text = crate::providers::common::read_provider_error_body(response).await;
                return Err(Self::handle_http_error(status, &error_text));
            }

            let interaction_response: Interaction =
                response.json().await.map_err(|e| format_parse_error("Gemini", &e))?;

            return Self::convert_from_interaction_response(interaction_response, model);
        }

        let gemini_request = self.convert_to_gemini_request(&request)?;

        let url = format!("{}/models/{}:generateContent", self.base_url, request.model);

        let response = self
            .http_client
            .post(&url)
            .header("x-goog-api-key", self.api_key.as_ref())
            .json(&gemini_request)
            .send()
            .await
            .map_err(|e| format_network_error("Gemini", &e))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = crate::providers::common::read_provider_error_body(response).await;
            return Err(Self::handle_http_error(status, &error_text));
        }

        let gemini_response: GenerateContentResponse =
            response.json().await.map_err(|e| format_parse_error("Gemini", &e))?;

        Self::convert_from_gemini_response(gemini_response, model)
    }

    async fn stream(&self, request: LLMRequest) -> Result<LLMStream, LLMError> {
        if self.should_use_interactions(&request) {
            let model = request.model.clone();
            let interaction_request = self.convert_to_interaction_request(&request)?;
            let url = format!("{}/interactions?alt=sse", self.base_url);
            let response = self
                .http_client
                .post(&url)
                .header("x-goog-api-key", self.api_key.as_ref())
                .json(&interaction_request)
                .send()
                .await
                .map_err(|e| format_network_error("Gemini", &e))?;

            if !response.status().is_success() {
                let status = response.status();
                let error_text = crate::providers::common::read_provider_error_body(response).await;
                return Err(Self::handle_http_error(status, &error_text));
            }

            let stream = {
                try_stream! {
                    let mut body_stream = response.bytes_stream();
                    let mut buf: Vec<u8> = Vec::new();
                    let mut offset = 0usize;
                    let mut decoder = crate::providers::shared::Utf8StreamDecoder::new();
                    let mut state = InteractionStreamState::default();

                    while let Some(chunk_result) = body_stream.next().await {
                        let chunk = chunk_result
                            .map_err(|err| format_network_error("Gemini", &err))?;

                        decoder.push_bytes(&chunk, &mut buf);

                        while let Some((split_idx, delimiter_len)) = find_sse_boundary_bytes(&buf, offset) {
                            let event = std::str::from_utf8(&buf[offset..split_idx])
                                .map_err(|e| StreamAssemblyError::InvalidPayload(format!("non-utf-8 stream data: {e}")).into_llm_error("Gemini"))?;
                            offset = split_idx + delimiter_len;

                            let Some(data_payload) = extract_data_payload(event) else {
                                continue;
                            };

                            let trimmed_payload = data_payload.trim();
                            if trimmed_payload.is_empty() || trimmed_payload == "[DONE]" {
                                continue;
                            }

                            let payload: Value = serde_json::from_str(trimmed_payload)
                                .map_err(|err| {
                                    StreamAssemblyError::InvalidPayload(err.to_string())
                                        .into_llm_error("Gemini")
                                })?;

                            for stream_event in Self::apply_interaction_stream_payload(&mut state, &payload)? {
                                yield stream_event;
                            }
                        }

                        // Drain the consumed prefix so `buf` stays bounded to
                        // the unprocessed tail rather than growing for the
                        // entire stream lifetime.
                        if offset > 0 {
                            buf.drain(..offset);
                            offset = 0;
                        }
                    }

                    if !state.completed {
                        let formatted_error = error_display::format_llm_error(
                            "Gemini",
                            "Interactions stream ended without an interaction.complete event",
                        );
                        Err(LLMError::Provider {
                            message: formatted_error,
                            metadata: None,
                        })?;
                    }

                    let response =
                        Self::finalize_interaction_stream_state(state, model)?;
                    yield LLMStreamEvent::Completed { response: Box::new(response) };
                }
            };
            return Ok(Box::pin(stream));
        }

        let model = request.model.clone();
        let gemini_request = self.convert_to_gemini_request(&request)?;

        let url = format!("{}/models/{}:streamGenerateContent", self.base_url, request.model);

        let response = self
            .http_client
            .post(&url)
            .header("x-goog-api-key", self.api_key.as_ref())
            .json(&gemini_request)
            .send()
            .await
            .map_err(|e| format_network_error("Gemini", &e))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = crate::providers::common::read_provider_error_body(response).await;
            return Err(Self::handle_http_error(status, &error_text));
        }

        let (event_tx, event_rx) = mpsc::unbounded_channel::<Result<LLMStreamEvent, LLMError>>();
        let completion_sender = event_tx.clone();

        let streaming_timeout = self.timeouts.streaming_ceiling_seconds;

        let model_clone = model.clone();
        tokio::spawn(async move {
            let config = StreamingConfig::with_total_timeout(streaming_timeout);
            let mut processor = StreamingProcessor::with_config(config);
            let event_sender = completion_sender.clone();
            let mut aggregator = crate::providers::shared::StreamAggregator::new(model_clone.clone());

            let mut on_chunk = |chunk: &str| -> Result<(), StreamingError> {
                if chunk.is_empty() {
                    return Ok(());
                }

                if let Some(delta) = Self::apply_stream_delta(&mut aggregator.content, chunk) {
                    if delta.is_empty() {
                        return Ok(());
                    }

                    for event in aggregator.sanitizer.process_chunk(&delta) {
                        event_sender.send(Ok(event)).map_err(|_e| StreamingError::StreamingError {
                            message: "Streaming consumer dropped".to_string(),
                            partial_content: Some(chunk.to_string()),
                        })?;
                    }
                }
                Ok(())
            };

            let result = processor.process_stream(response, &mut on_chunk).await;
            match result {
                Ok(mut streaming_response) => {
                    if streaming_response.candidates.is_empty() && !aggregator.content.trim().is_empty() {
                        streaming_response.candidates.push(StreamingCandidate {
                            content: Content {
                                role: "model".to_string(),
                                parts: vec![Part::Text {
                                    text: aggregator.content.clone(),
                                    thought_signature: None,
                                }],
                            },
                            finish_reason: None,
                            index: Some(0),
                        });
                    }

                    match Self::convert_from_streaming_response(streaming_response, model_clone) {
                        Ok(mut final_response) => {
                            let aggregator_response = aggregator.finalize();
                            if final_response.reasoning.is_none() {
                                final_response.reasoning = aggregator_response.reasoning;
                            }
                            if final_response.content.is_none() {
                                final_response.content = aggregator_response.content;
                            }

                            let _ = completion_sender
                                .send(Ok(LLMStreamEvent::Completed { response: Box::new(final_response) }));
                        }
                        Err(err) => {
                            let _ = completion_sender.send(Err(err));
                        }
                    }
                }
                Err(error) => {
                    let mapped = Self::map_streaming_error(error);
                    let _ = completion_sender.send(Err(mapped));
                }
            }
        });

        drop(event_tx);

        let stream = {
            let mut receiver = event_rx;
            try_stream! {
                while let Some(event) = receiver.recv().await {
                    yield event?;
                }
            }
        };

        Ok(Box::pin(stream))
    }

    fn supported_models(&self) -> Vec<String> {
        models::google::SUPPORTED_MODELS.iter().map(|s| s.to_string()).collect()
    }

    fn validate_request(&self, request: &LLMRequest) -> Result<(), LLMError> {
        if GeminiProvider::uses_latest_gemini_api(&request.model) {
            if request.temperature.is_some() || request.top_p.is_some() || request.top_k.is_some() {
                tracing::warn!(
                    model = %request.model,
                    temperature = ?request.temperature,
                    top_p = ?request.top_p,
                    top_k = ?request.top_k,
                    "Sampling parameters (temperature, top_p, top_k) are deprecated for this Gemini model and will be ignored by the API"
                );
            }
        }

        if request.previous_response_id.is_some() && request.response_store == Some(false) {
            let formatted_error = error_display::format_llm_error(
                "Gemini",
                "Interactions with previous_interaction_id cannot set store=false",
            );
            return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
        }

        if !models::google::SUPPORTED_MODELS.iter().any(|m| *m == request.model) {
            let formatted_error =
                error_display::format_llm_error("Gemini", &format!("Unsupported model: {}", request.model));
            return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
        }

        if let Some(max_tokens) = request.max_tokens {
            let model = request.model.as_str();
            let max_output_tokens = if model.contains("3") { 65536 } else { 8192 };

            if max_tokens > max_output_tokens {
                let formatted_error = error_display::format_llm_error(
                    "Gemini",
                    &format!(
                        "Requested max_tokens ({max_tokens}) exceeds model limit ({max_output_tokens}) for {model}"
                    ),
                );
                return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
            }
        }

        Ok(())
    }
}
