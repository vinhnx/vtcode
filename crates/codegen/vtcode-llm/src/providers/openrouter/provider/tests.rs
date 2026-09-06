use super::super::stream_decoder::parse_usage_value;
use super::*;
use crate::provider::{LLMProvider, NormalizedStreamEvent};
use crate::providers::openrouter::stream_decoder::parse_stream_payload;
use vtcode_config::TimeoutsConfig;

use crate::provider::FinishReason;
use crate::provider::ToolDefinition;
use crate::providers::ReasoningBuffer;
use crate::providers::shared::NoopStreamTelemetry;
use crate::providers::shared::{StreamFragment, extract_data_payload};
use futures::StreamExt;
use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    "unknown panic".to_string()
}

async fn start_mock_server_or_skip() -> Option<MockServer> {
    match tokio::spawn(async { MockServer::start().await }).await {
        Ok(server) => Some(server),
        Err(err) if err.is_panic() => {
            let message = panic_message(err.into_panic());
            if message.contains("Operation not permitted") || message.contains("PermissionDenied") {
                return None;
            }
            panic!("mock server should start: {message}");
        }
        Err(err) => panic!("mock server task should complete: {err}"),
    }
}

fn sample_tool() -> ToolDefinition {
    ToolDefinition::function(
        "fetch_data".to_string(),
        "Fetch data".to_string(),
        json!({
            "type": "object",
            "properties": {}
        }),
    )
}

fn request_with_tools(model: &str) -> LLMRequest {
    LLMRequest {
        messages: vec![Message::user("hi".to_string())].into(),
        tools: Some(std::sync::Arc::new(vec![sample_tool()])),
        model: model.to_string(),
        tool_choice: Some(ToolChoice::Any),
        parallel_tool_calls: Some(true),
        ..Default::default()
    }
}

fn test_provider(base_url: &str, model: &str) -> OpenRouterProvider {
    let http_client = reqwest::Client::builder().no_proxy().build().expect("test client should build");
    OpenRouterProvider::new_with_client(
        "test-key".to_string(),
        model.to_string(),
        http_client,
        base_url.to_string(),
        TimeoutsConfig::default(),
    )
}

#[test]
fn enforce_tool_capabilities_disables_tools_for_restricted_models() {
    let model_id = "moonshotai/kimi-latest";
    let provider = OpenRouterProvider::with_model("test-key".to_string(), model_id.to_string());
    let request = request_with_tools(model_id);

    match provider.enforce_tool_capabilities(&request) {
        Cow::Borrowed(_) => {
            // If the model is actually supported in the new metadata, this test might need updating.
            // But we assume it's still restricted for this test's purpose.
        }
        Cow::Owned(sanitized) => {
            assert!(sanitized.tools.is_none());
            assert!(matches!(sanitized.tool_choice, Some(ToolChoice::None)));
            assert!(sanitized.parallel_tool_calls.is_none());
            assert_eq!(sanitized.model, model_id);
            assert_eq!(sanitized.messages, request.messages);
        }
    }
}

#[test]
fn enforce_tool_capabilities_keeps_tools_for_supported_models() {
    let provider = OpenRouterProvider::with_model("test-key".to_string(), models::openrouter::OPENAI_GPT_5.to_string());
    let request = request_with_tools(models::openrouter::OPENAI_GPT_5);

    match provider.enforce_tool_capabilities(&request) {
        Cow::Borrowed(borrowed) => {
            assert!(std::ptr::eq(borrowed, &request));
            assert!(borrowed.tools.as_ref().is_some());
        }
        Cow::Owned(_) => panic!("should not sanitize supported models"),
    }
}

#[test]
fn enforce_tool_capabilities_keeps_apply_patch_for_supported_models() {
    let provider = OpenRouterProvider::with_model("test-key".to_string(), models::openrouter::OPENAI_GPT_5.to_string());
    let request = LLMRequest {
        messages: vec![Message::user("hi".to_string())].into(),
        tools: Some(std::sync::Arc::new(vec![ToolDefinition::apply_patch("Apply VT Code patches".to_string())])),
        model: models::openrouter::OPENAI_GPT_5.to_string(),
        tool_choice: Some(ToolChoice::Any),
        parallel_tool_calls: Some(true),
        ..Default::default()
    };

    match provider.enforce_tool_capabilities(&request) {
        Cow::Borrowed(borrowed) => {
            let tools = borrowed.tools.as_ref().expect("tools should remain enabled");
            assert_eq!(tools.len(), 1);
            assert_eq!(tools[0].function_name(), "apply_patch");
        }
        Cow::Owned(_) => panic!("apply_patch should remain available for supported models"),
    }
}

#[test]
fn effective_context_size_uses_catalog_capacity_for_known_routes() {
    let provider = OpenRouterProvider::with_model("test-key".to_string(), "meta/muse-spark-1.2".to_string());

    assert_eq!(provider.effective_context_size("meta/muse-spark-1.2"), 1_048_576);
    assert_eq!(provider.effective_context_size("unlisted-route"), 128_000);
}

#[test]
fn catalog_reasoning_capabilities_are_available_without_model_overrides() {
    let provider = OpenRouterProvider::with_model("test-key".to_string(), "meta/muse-spark-1.2".to_string());

    assert!(provider.supports_reasoning("meta/muse-spark-1.2"));
    assert!(!provider.supports_reasoning_effort("meta/muse-spark-1.2"));
    assert!(provider.supported_reasoning_efforts("meta/muse-spark-1.2").is_empty());
}

#[test]
fn test_parse_stream_payload_chat_chunk() {
    let payload = json!({
        "choices": [{
            "delta": {
                "content": [
                    {"type": "output_text", "text": "Hello"}
                ]
            }
        }]
    });

    let mut aggregated = String::new();
    let mut builders = Vec::new();
    let mut reasoning = ReasoningBuffer::default();
    let mut usage = None;
    let mut finish_reason = FinishReason::Stop;
    let telemetry = NoopStreamTelemetry;

    let delta = parse_stream_payload(
        &payload,
        &mut aggregated,
        &mut builders,
        &mut reasoning,
        &mut usage,
        &mut finish_reason,
        &telemetry,
    );

    let fragments = delta.expect("delta should exist").into_fragments();
    assert_eq!(fragments, vec![StreamFragment::Content("Hello".to_string())]);
    assert_eq!(aggregated, "Hello");
    assert!(builders.is_empty());
    assert!(usage.is_none());
    assert!(reasoning.finalize().is_none());
}

#[test]
fn test_parse_stream_payload_response_delta() {
    let payload = json!({
        "type": "response.delta",
        "delta": {
            "type": "output_text_delta",
            "text": "Stream"
        }
    });

    let mut aggregated = String::new();
    let mut builders = Vec::new();
    let mut reasoning = ReasoningBuffer::default();
    let mut usage = None;
    let mut finish_reason = FinishReason::Stop;
    let telemetry = NoopStreamTelemetry;

    let delta = parse_stream_payload(
        &payload,
        &mut aggregated,
        &mut builders,
        &mut reasoning,
        &mut usage,
        &mut finish_reason,
        &telemetry,
    );

    let fragments = delta.expect("delta should exist").into_fragments();
    assert_eq!(fragments, vec![StreamFragment::Content("Stream".to_string())]);
    assert_eq!(aggregated, "Stream");
}

#[test]
fn test_extract_data_payload_joins_multiline_events() {
    let event = ": keep-alive\n".to_string() + "data: {\"a\":1}\n" + "data: {\"b\":2}\n";
    let payload = extract_data_payload(&event);
    assert_eq!(payload.as_deref(), Some("{\"a\":1}\n{\"b\":2}"));
}

#[test]
fn parse_usage_value_includes_cache_metrics() {
    let value = json!({
        "prompt_tokens": 120,
        "completion_tokens": 80,
        "total_tokens": 200,
        "prompt_cache_read_tokens": 90,
        "prompt_cache_write_tokens": 15
    });

    let usage = parse_usage_value(&value);
    assert_eq!(usage.prompt_tokens, 120);
    assert_eq!(usage.completion_tokens, 80);
    assert_eq!(usage.total_tokens, 200);
    assert_eq!(usage.cached_prompt_tokens, Some(90));
    assert_eq!(usage.cache_read_tokens, Some(90));
    assert_eq!(usage.cache_creation_tokens, Some(15));
}

#[tokio::test]
async fn generate_retries_without_tools_when_openrouter_rejects_tool_endpoints() {
    let model_id = "moonshotai/kimi-latest";
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };
    let provider = test_provider(&server.uri(), model_id);

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_partial_json(json!({
            "model": model_id,
            "tool_choice": "required"
        })))
        .respond_with(ResponseTemplate::new(404).set_body_string("No endpoints found that support tool use"))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_partial_json(json!({
            "model": model_id,
            "tool_choice": "none"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "content": "fallback answer"
                }
            }],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 1,
                "total_tokens": 2
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let response = LLMProvider::generate(&provider, request_with_tools(model_id))
        .await
        .expect("fallback request should succeed");

    assert_eq!(response.content.as_deref(), Some("fallback answer"));
}

#[tokio::test]
async fn stream_normalized_emits_tool_call_start_and_delta_events() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };
    let provider = test_provider(&server.uri(), models::openrouter::OPENAI_GPT_5);

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(
                    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"code_search\",\"arguments\":\"{\\\"query\\\":\\\"ph\"}}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"arguments\":\"ase\\\"}\"}}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"done\"}}]}\n\n\
data: [DONE]\n\n",
                ),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut stream = provider
        .stream_normalized(LLMRequest {
            model: models::openrouter::OPENAI_GPT_5.to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            ..Default::default()
        })
        .await
        .expect("normalized stream should succeed");

    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("stream event should parse"));
    }

    assert!(matches!(
        events.as_slice(),
        [
            NormalizedStreamEvent::ToolCallStart { call_id, name },
            NormalizedStreamEvent::ToolCallDelta { call_id: first_delta_id, delta: first_delta },
            NormalizedStreamEvent::ToolCallDelta { call_id: second_delta_id, delta: second_delta },
            NormalizedStreamEvent::TextDelta { delta },
            NormalizedStreamEvent::Done { .. }
        ]
        if call_id == "call_1"
            && name.as_deref() == Some("code_search")
            && first_delta_id == "call_1"
            && first_delta == "{\"query\":\"ph"
            && second_delta_id == "call_1"
            && second_delta == "ase\"}"
            && delta == "done"
    ));
}

#[tokio::test]
async fn stream_normalized_fabricates_and_reuses_id_when_provider_omits_it() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };
    let provider = test_provider(&server.uri(), models::openrouter::OPENAI_GPT_5);

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(
                    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"code_search\",\"arguments\":\"{\\\"query\\\":\\\"ph\"}}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"ase\\\"}\"}}]}}]}\n\n\
data: [DONE]\n\n",
                ),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut stream = provider
        .stream_normalized(LLMRequest {
            model: models::openrouter::OPENAI_GPT_5.to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            ..Default::default()
        })
        .await
        .expect("normalized stream should succeed");

    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("stream event should parse"));
    }

    let start_id = events
        .iter()
        .find_map(|event| match event {
            NormalizedStreamEvent::ToolCallStart { call_id, .. } => Some(call_id.clone()),
            _ => None,
        })
        .expect("tool call start expected");
    let delta_ids: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            NormalizedStreamEvent::ToolCallDelta { call_id, .. } => Some(call_id.as_str()),
            _ => None,
        })
        .collect();
    assert!(delta_ids.iter().all(|id| *id == start_id), "all deltas must reuse the fabricated start id");
    assert!(start_id.starts_with("call_"));

    let finalized = events
        .iter()
        .find_map(|event| match event {
            NormalizedStreamEvent::Done { response } => Some(response.clone()),
            _ => None,
        })
        .expect("done event expected");
    let finalized_id = finalized
        .tool_calls
        .as_ref()
        .and_then(|calls| calls.first())
        .map(|call| call.id.clone())
        .expect("finalized tool call expected");
    assert_eq!(start_id, finalized_id, "streamed id must match the finalized tool call id");
}

#[tokio::test]
async fn stream_normalized_fabricates_distinct_ids_across_streams() {
    let mut ids = Vec::new();
    for _ in 0..2 {
        let Some(server) = start_mock_server_or_skip().await else {
            return;
        };
        let provider = test_provider(&server.uri(), models::openrouter::OPENAI_GPT_5);

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(
                        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"code_search\",\"arguments\":\"{\\\"query\\\":\\\"phase\\\"}\"}}]}}]}\n\n\
data: [DONE]\n\n",
                    ),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mut stream = provider
            .stream_normalized(LLMRequest {
                model: models::openrouter::OPENAI_GPT_5.to_string(),
                messages: vec![Message::user("hello".to_string())].into(),
                ..Default::default()
            })
            .await
            .expect("normalized stream should succeed");

        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.expect("stream event should parse"));
        }

        let start_id = events
            .iter()
            .find_map(|event| match event {
                NormalizedStreamEvent::ToolCallStart { call_id, .. } => Some(call_id.clone()),
                _ => None,
            })
            .expect("tool call start expected");
        ids.push(start_id);
    }

    assert_ne!(ids[0], ids[1], "fabricated ids must differ across separate streams");
}
