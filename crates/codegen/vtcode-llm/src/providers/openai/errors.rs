//! OpenAI provider error handling and formatting utilities.
//!
//! This module contains error detection, formatting, and recovery logic
//! for OpenAI API interactions.

use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde_json::Value;

use crate::providers::common::extract_header;
use vtcode_config::constants::models;
use vtcode_config::models::model_catalog_entry;

#[derive(Debug, Default, PartialEq, Eq)]
struct OpenAIErrorDetails {
    message: Option<String>,
    code: Option<String>,
    error_type: Option<String>,
    param: Option<String>,
}

fn parse_openai_error_details(body: &str) -> OpenAIErrorDetails {
    let Ok(json) = serde_json::from_str::<Value>(body) else {
        return OpenAIErrorDetails::default();
    };

    let error = json.get("error").unwrap_or(&json);
    OpenAIErrorDetails {
        message: error
            .get("message")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                // FastAPI / alternate: {"detail": "..."}
                json.get("detail")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                    .filter(|value| !value.trim().is_empty())
            }),
        code: error
            .get("code")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .filter(|value| !value.trim().is_empty()),
        error_type: error
            .get("type")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .filter(|value| !value.trim().is_empty()),
        param: error
            .get("param")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .filter(|value| !value.trim().is_empty()),
    }
}

/// Detect if an OpenAI API error indicates the model was not found or is inaccessible.
pub(crate) fn is_model_not_found(status: StatusCode, error_text: &str) -> bool {
    if !matches!(status, StatusCode::NOT_FOUND | StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY) {
        return false;
    }

    let lower = error_text.to_ascii_lowercase();
    lower.contains("model_not_found")
        || (lower.contains("model") && lower.contains("does not exist"))
        || (lower.contains("model") && lower.contains("not found"))
        || lower.contains("unknown model")
}

/// Provide a fallback model when the requested model is unavailable.
pub(crate) fn fallback_model_if_not_found(model: &str) -> Option<String> {
    if let Some(entry) = model_catalog_entry("openai", model) {
        return entry
            .lightweight_model
            .filter(|candidate| *candidate != model)
            .map(str::to_owned);
    }

    match model {
        m if m == models::openai::GPT_5_MINI => Some(models::openai::GPT_5.to_string()),
        m if m == models::openai::GPT_5_NANO => Some(models::openai::GPT_5_MINI.to_string()),
        _ => None,
    }
}

/// Format an OpenAI API error with request metadata.
pub(crate) fn format_openai_error(
    status: StatusCode,
    body: &str,
    headers: &HeaderMap,
    context: &str,
    client_request_id: Option<&str>,
) -> String {
    let request_id = extract_header(headers, &["x-request-id", "request-id", "openai-request-id"])
        .unwrap_or_else(|| "<none>".to_string());
    let effective_client_request_id = client_request_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| extract_header(headers, &["x-client-request-id"]));
    let organization_id = extract_header(headers, &["openai-organization", "x-organization-id"]);
    let retry_after = extract_header(headers, &["retry-after"]);
    let error_details = parse_openai_error_details(body);
    let trimmed_body: String = body.chars().take(2_000).collect();
    let mut metadata_parts = vec![format!("request_id={request_id}")];
    if let Some(client_request_id) = effective_client_request_id {
        metadata_parts.push(format!("client_request_id={client_request_id}"));
    }
    if let Some(code) = error_details.code.as_deref() {
        metadata_parts.push(format!("code={code}"));
    }
    if let Some(error_type) = error_details.error_type.as_deref() {
        metadata_parts.push(format!("type={error_type}"));
    }
    if let Some(param) = error_details.param.as_deref() {
        metadata_parts.push(format!("param={param}"));
    }
    if let Some(retry_after) = retry_after.as_deref() {
        metadata_parts.push(format!("retry_after={retry_after}"));
    }
    if let Some(organization_id) = organization_id.as_deref() {
        metadata_parts.push(format!("organization={organization_id}"));
    }

    let mut formatted = format!("{} (status {}) [{}]", context, status, metadata_parts.join(" "));
    if let Some(message) = error_details.message.as_deref() {
        formatted.push_str(&format!(" Message: {message}"));
    }
    if !trimmed_body.is_empty() && error_details.message.as_deref() != Some(trimmed_body.as_str()) {
        formatted.push_str(&format!(" Body: {trimmed_body}"));
    }
    formatted
}

/// Detect if an error indicates Responses API is not supported for this model/endpoint.
pub(crate) fn is_responses_api_unsupported(status: StatusCode, body: &str) -> bool {
    let lower = body.to_ascii_lowercase();

    (status == StatusCode::NOT_FOUND && lower.trim().is_empty())
        || lower.contains("not enabled for the responses api")
        || lower.contains("responses api") && (lower.contains("unsupported") || lower.contains("not supported"))
        || lower.contains("invalid api parameter")
        || lower.contains("unsupported parameter")
        || lower.contains("1210")
        || lower.contains("invalid_request_error")
}

/// Detect if an OpenAI API error indicates `service_tier=flex` is unsupported.
pub(crate) fn is_flex_service_tier_unsupported(status: StatusCode, body: &str) -> bool {
    if status != StatusCode::BAD_REQUEST {
        return false;
    }

    let lower = body.to_ascii_lowercase();
    lower.contains("flex")
        && (lower.contains("flex is not available for this model")
            || (lower.contains("service tier") || lower.contains("service_tier"))
                && (lower.contains("not available") || lower.contains("unsupported")))
}

#[cfg(test)]
mod tests {
    use super::{
        OpenAIErrorDetails, format_openai_error, is_flex_service_tier_unsupported, is_model_not_found,
        is_responses_api_unsupported, parse_openai_error_details,
    };
    use reqwest::StatusCode;
    use reqwest::header::{HeaderMap, HeaderValue};

    #[test]
    fn model_not_found_requires_model_specific_body() {
        assert!(!is_model_not_found(StatusCode::NOT_FOUND, ""));
        assert!(is_model_not_found(StatusCode::NOT_FOUND, "model_not_found"));
        assert!(is_model_not_found(StatusCode::BAD_REQUEST, "The requested model does not exist"));
    }

    #[test]
    fn responses_api_unsupported_keeps_blank_404_fallback() {
        assert!(is_responses_api_unsupported(StatusCode::NOT_FOUND, ""));
        assert!(is_responses_api_unsupported(
            StatusCode::BAD_REQUEST,
            "This endpoint is not enabled for the Responses API"
        ));
        assert!(!is_responses_api_unsupported(StatusCode::NOT_FOUND, "model_not_found"));
    }

    #[test]
    fn flex_service_tier_detection_matches_backend_error() {
        assert!(is_flex_service_tier_unsupported(
            StatusCode::BAD_REQUEST,
            r#"{"error":{"message":"Flex is not available for this model.","type":"invalid_request_error"}}"#
        ));
        assert!(!is_flex_service_tier_unsupported(
            StatusCode::BAD_REQUEST,
            r#"{"error":{"message":"Bad request","type":"invalid_request_error"}}"#
        ));
        assert!(!is_flex_service_tier_unsupported(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"message":"Flex is not available for this model.","type":"invalid_request_error"}}"#
        ));
    }

    #[test]
    fn parse_openai_error_details_extracts_message_and_codes() {
        let details = parse_openai_error_details(
            r#"{"error":{"message":"Bad request","type":"invalid_request_error","param":"text.verbosity","code":"unsupported_parameter"}}"#,
        );

        assert_eq!(details.message.as_deref(), Some("Bad request"));
        assert_eq!(details.error_type.as_deref(), Some("invalid_request_error"));
        assert_eq!(details.param.as_deref(), Some("text.verbosity"));
        assert_eq!(details.code.as_deref(), Some("unsupported_parameter"));
    }

    #[test]
    fn parse_openai_error_details_handles_empty_body() {
        let details = parse_openai_error_details("");

        assert_eq!(details, OpenAIErrorDetails::default());
    }

    #[test]
    fn format_openai_error_surfaces_debugging_metadata() {
        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", HeaderValue::from_static("req_123"));
        headers.insert("retry-after", HeaderValue::from_static("30"));
        headers.insert("openai-organization", HeaderValue::from_static("org_456"));

        let formatted = format_openai_error(
            StatusCode::BAD_REQUEST,
            r#"{"error":{"message":"Bad request","type":"invalid_request_error","param":"text.verbosity","code":"unsupported_parameter"}}"#,
            &headers,
            "Responses API error",
            Some("vtcode-abc"),
        );

        assert!(formatted.contains("request_id=req_123"));
        assert!(formatted.contains("client_request_id=vtcode-abc"));
        assert!(formatted.contains("retry_after=30"));
        assert!(formatted.contains("organization=org_456"));
        assert!(formatted.contains("type=invalid_request_error"));
        assert!(formatted.contains("code=unsupported_parameter"));
        assert!(formatted.contains("param=text.verbosity"));
        assert!(formatted.contains("Message: Bad request"));
    }
}
