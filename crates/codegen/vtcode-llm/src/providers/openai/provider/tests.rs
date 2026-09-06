use super::super::CustomProviderAuthHandle;
use super::super::backend_setup::{
    CHATGPT_CODEX_BASE, ChatGptSubscriptionAuthSource, OpenAIBackendKind, OpenAIBackendRefreshBehaviour,
    OpenAIBackendSetup,
};
use super::super::tool_serialization;
use super::*;
use crate::provider::{LLMProvider, NormalizedStreamEvent, ParallelToolConfig};
use futures::StreamExt;
use reqwest::StatusCode;
use rig::providers::chatgpt::ChatGPTAuth as RigChatGptAuth;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;
use vtcode_config::TimeoutsConfig;
use vtcode_config::auth::{AuthCredentialsStoreMode, OpenAIChatGptAuthHandle, OpenAIChatGptSession};
use vtcode_config::constants::urls;
use vtcode_config::core::CustomProviderCommandAuthConfig;
use vtcode_config::core::{
    CustomProviderApiFormat, OpenAIHostedShellConfig, OpenAIHostedShellDomainSecret, OpenAIHostedShellEnvironment,
    OpenAIHostedShellNetworkPolicy, OpenAIHostedShellNetworkPolicyType, OpenAIHostedSkill, OpenAIHostedSkillVersion,
    OpenAIServiceTier, PromptCacheRetention,
};
use vtcode_config::{OpenAIAuthConfig, auth::OpenAIChatGptSessionRefresher};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ─── Test Fixtures ───────────────────────────────────────────────────────────
struct ExternalSessionRefresher {
    calls: Arc<Mutex<usize>>,
}

#[async_trait::async_trait]
impl OpenAIChatGptSessionRefresher for ExternalSessionRefresher {
    async fn refresh_session(&self, current: &OpenAIChatGptSession) -> anyhow::Result<OpenAIChatGptSession> {
        let mut calls = self.calls.lock().expect("mutex should lock");
        *calls += 1;
        let mut refreshed = current.clone();
        refreshed.access_token = "oauth-access-refreshed".to_string();
        refreshed.refreshed_at = current.refreshed_at.saturating_add(1);
        refreshed.expires_at = None;
        Ok(refreshed)
    }
}

fn write_token_lines(dir: &std::path::Path, tokens: &[&str]) {
    std::fs::write(dir.join("tokens.txt"), tokens.join("\n")).expect("write tokens");
}
fn custom_provider_auth_fixture(dir: &TempDir, tokens: &[&str]) -> CustomProviderCommandAuthConfig {
    write_token_lines(dir.path(), tokens);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let script_path = dir.path().join("print-token.sh");
        std::fs::write(
            &script_path,
            r#"#!/bin/sh
first_line=$(sed -n '1p' tokens.txt)
printf '%s\n' "$first_line"
tail -n +2 tokens.txt > tokens.next
mv tokens.next tokens.txt
"#,
        )
        .expect("write script");
        let mut perms = std::fs::metadata(&script_path).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).expect("set permissions");
        CustomProviderCommandAuthConfig {
            command: "./print-token.sh".to_string(),
            args: Vec::new(),
            cwd: Some(dir.path().to_path_buf()),
            timeout_ms: 5_000,
            refresh_interval_ms: 60_000,
        }
    }
    #[cfg(windows)]
    {
        let script_path = dir.path().join("print-token.ps1");
        std::fs::write(
            &script_path,
            r#"$lines = Get-Content -Path tokens.txt
if ($lines.Count -eq 0) { exit 1 }
Write-Output $lines[0]
$lines | Select-Object -Skip 1 | Set-Content -Path tokens.txt
"#,
        )
        .expect("write script");
        CustomProviderCommandAuthConfig {
            command: "powershell".to_string(),
            args: vec![
                "-NoProfile".to_string(),
                "-ExecutionPolicy".to_string(),
                "Bypass".to_string(),
                "-File".to_string(),
                script_path.to_string_lossy().into_owned(),
            ],
            cwd: Some(dir.path().to_path_buf()),
            timeout_ms: 5_000,
            refresh_interval_ms: 60_000,
        }
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else {
        "unknown panic".to_string()
    }
}

async fn start_mock_server_or_skip() -> Option<MockServer> {
    match tokio::spawn(async { MockServer::start().await }).await {
        Ok(s) => Some(s),
        Err(e) if e.is_panic() => {
            let msg = panic_message(e.into_panic());
            if msg.contains("Operation not permitted") || msg.contains("PermissionDenied") {
                return None;
            }
            panic!("mock server should start: {msg}");
        }
        Err(e) => panic!("mock server task should complete: {e}"),
    }
}

// ─── Helper constructors ─────────────────────────────────────────────────────
fn tool_def(name: &str, desc: &str, schema: Value) -> provider::ToolDefinition {
    provider::ToolDefinition::function(name.to_owned(), desc.to_owned(), schema)
}

fn sample_tool() -> provider::ToolDefinition {
    tool_def(
        "search_workspace",
        "Search project files",
        json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false}),
    )
}

fn shell_tool() -> provider::ToolDefinition {
    tool_def(
        "shell",
        "Execute a shell command and return its output.",
        json!({"type":"object","properties":{"command":{"type":"string"}},"required":["command"],"additionalProperties":false}),
    )
}

fn sample_request(model: &str) -> provider::LLMRequest {
    provider::LLMRequest {
        messages: vec![provider::Message::user("Hello".to_owned())].into(),
        tools: Some(Arc::new(vec![sample_tool()])),
        model: model.to_string(),
        ..Default::default()
    }
}

fn shell_request(model: &str) -> provider::LLMRequest {
    provider::LLMRequest {
        messages: vec![provider::Message::user("Run pwd".to_owned())].into(),
        tools: Some(Arc::new(vec![shell_tool()])),
        model: model.to_string(),
        ..Default::default()
    }
}

fn test_provider(base_url: &str, model: &str) -> OpenAIProvider {
    let http_client = reqwest::Client::builder().no_proxy().build().expect("test client should build");
    OpenAIProvider::new_with_client(
        "test-key".to_string(),
        None,
        model.to_string(),
        http_client,
        base_url.to_string(),
        TimeoutsConfig::default(),
    )
}

#[test]
fn effective_context_size_uses_catalog_capacity_and_explicit_override() {
    let provider = OpenAIProvider::new("offline-fixture".to_string());
    assert_eq!(provider.effective_context_size("gpt-6-astra"), 1_050_000);
    assert_eq!(provider.effective_context_size("unknown-model"), 128_000);
    assert_eq!(provider.with_context_window(Some(32_000)).effective_context_size("gpt-6-astra"), 32_000);
}

fn native_openai_mock_base_url(server: &MockServer) -> String {
    server.uri().replacen("http://", "http://api.openai.com@", 1)
}

fn chatgpt_mock_base_url(server: &MockServer) -> String {
    server.uri().replacen("http://", "http://chatgpt.com@", 1)
}

fn sample_chatgpt_auth_handle() -> OpenAIChatGptAuthHandle {
    OpenAIChatGptAuthHandle::new(
        OpenAIChatGptSession {
            openai_api_key: String::new(),
            id_token: "id-token".to_string(),
            access_token: "oauth-access".to_string(),
            refresh_token: "refresh-token".to_string(),
            account_id: Some("acc_123".to_string()),
            email: Some("test@example.com".to_string()),
            plan: Some("plus".to_string()),
            obtained_at: 1,
            refreshed_at: u64::MAX / 2,
            expires_at: None,
        },
        OpenAIAuthConfig::default(),
        AuthCredentialsStoreMode::File,
    )
}

// ─── Config builders ─────────────────────────────────────────────────────────
fn priority_openai_config() -> OpenAIConfig {
    OpenAIConfig {
        service_tier: Some(OpenAIServiceTier::Priority),
        ..Default::default()
    }
}

fn flex_openai_config() -> OpenAIConfig {
    OpenAIConfig {
        service_tier: Some(OpenAIServiceTier::Flex),
        ..Default::default()
    }
}

fn hosted_shell_openai_config() -> OpenAIConfig {
    OpenAIConfig {
        hosted_shell: OpenAIHostedShellConfig {
            enabled: true,
            environment: OpenAIHostedShellEnvironment::ContainerAuto,
            container_id: None,
            file_ids: vec!["file_123".to_string()],
            skills: vec![OpenAIHostedSkill::SkillReference {
                skill_id: "skill_123".to_string(),
                version: OpenAIHostedSkillVersion::default(),
            }],
            network_policy: OpenAIHostedShellNetworkPolicy::default(),
        },
        ..Default::default()
    }
}

fn chatgpt_backend_provider(model: &str) -> OpenAIProvider {
    OpenAIProvider::from_config(
        Some(String::new()),
        Some(sample_chatgpt_auth_handle()),
        Some(model.to_string()),
        None,
        None,
        None,
        None,
        None,
        None,
    )
}

fn native_openai_provider(model: &str) -> OpenAIProvider {
    OpenAIProvider::with_model(String::new(), model.to_string())
}

fn compatible_endpoint_provider(model: &str, base_url: &str) -> OpenAIProvider {
    OpenAIProvider::from_config(
        Some(String::new()),
        None,
        Some(model.to_string()),
        Some(base_url.to_string()),
        None,
        None,
        None,
        None,
        None,
    )
}
// ─── Assertion helpers ───────────────────────────────────────────────────────

fn assert_str_field(value: &Value, key: &str, expected: &str) {
    assert_eq!(value.get(key).and_then(Value::as_str), Some(expected), "field '{key}' mismatch");
}

fn assert_str_field_obj(obj: &serde_json::Map<String, Value>, key: &str, expected: &str) {
    assert_eq!(obj.get(key).and_then(Value::as_str), Some(expected), "field '{key}' mismatch");
}

fn assert_absent(value: &Value, key: &str) {
    assert!(value.get(key).is_none(), "field '{key}' should be absent");
}

fn without_responses_backend_fields(mut payload: Value) -> Value {
    let map = payload.as_object_mut().expect("responses payload should be an object");
    for field in [
        "include",
        "output_types",
        "previous_response_id",
        "prompt_cache_retention",
        "sampling_parameters",
        "store",
        "text",
    ] {
        map.remove(field);
    }
    payload
}

fn without_responses_backend_capability_fields(mut payload: Value) -> Value {
    let map = payload.as_object_mut().expect("responses payload should be an object");
    for field in [
        "include",
        "output_types",
        "prompt_cache_retention",
        "sampling_parameters",
        "text",
    ] {
        map.remove(field);
    }
    payload
}

fn get_input_array(payload: &Value) -> &[Value] {
    payload
        .get("input")
        .and_then(Value::as_array)
        .expect("input array should exist")
}

fn input_role_at(payload: &Value, index: usize) -> Option<&str> {
    get_input_array(payload)
        .get(index)
        .and_then(|v| v.get("role"))
        .and_then(Value::as_str)
}

fn input_type_at(payload: &Value, index: usize) -> Option<&str> {
    get_input_array(payload)
        .get(index)
        .and_then(|v| v.get("type"))
        .and_then(Value::as_str)
}

fn input_call_id_at(payload: &Value, index: usize) -> Option<&str> {
    get_input_array(payload)
        .get(index)
        .and_then(|v| v.get("call_id"))
        .and_then(Value::as_str)
}

fn responses_payload_for(model: &str, provider: &OpenAIProvider) -> Value {
    provider
        .convert_to_openai_responses_format(&sample_request(model))
        .expect("conversion should succeed")
}

fn responses_allowed_tools_request(model: &str) -> provider::LLMRequest {
    provider::LLMRequest {
        messages: vec![provider::Message::user("Hello".to_owned())].into(),
        tools: Some(Arc::new(vec![
            sample_tool(),
            provider::ToolDefinition::web_search(json!({})),
            provider::ToolDefinition::file_search(json!({"vector_store_ids":["vs_123"]})),
        ])),
        tool_choice: Some(provider::ToolChoice::allowed_tools_auto(vec![
            "web_search".to_string(),
            "search_workspace".to_string(),
        ])),
        model: model.to_string(),
        ..Default::default()
    }
}

fn chat_payload_for(model: &str, provider: &OpenAIProvider) -> Value {
    provider
        .convert_to_openai_format(&sample_request(model))
        .expect("conversion should succeed")
}

fn mock_retry_by_call_count(seen_tokens: Arc<Mutex<Vec<String>>>) -> impl Fn(&wiremock::Request) -> ResponseTemplate {
    move |request: &wiremock::Request| {
        let bearer = request
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .expect("authorization header required")
            .to_string();
        seen_tokens.lock().expect("mutex not poisoned").push(bearer);
        let count = seen_tokens.lock().expect("mutex not poisoned").len();
        match count {
            1 => ResponseTemplate::new(401),
            2 => ResponseTemplate::new(200),
            n => ResponseTemplate::new(500).set_body_string(format!("unexpected retry count: {n}")),
        }
    }
}

fn mock_service_tier_fallback(
    seen: Arc<Mutex<Vec<Option<String>>>>,
    success_body: Value,
) -> impl Fn(&wiremock::Request) -> ResponseTemplate {
    move |request: &wiremock::Request| {
        let payload: Value = serde_json::from_slice(&request.body).expect("valid json body");
        let tier = payload.get("service_tier").and_then(Value::as_str).map(ToOwned::to_owned);
        seen.lock().expect("mutex not poisoned").push(tier.clone());
        match tier.as_deref() {
            Some("flex") => ResponseTemplate::new(400).set_body_json(json!({
                "error": {"message": "Flex is not available for this model.", "type": "invalid_request_error"}
            })),
            None => ResponseTemplate::new(200).set_body_json(success_body.clone()),
            other => ResponseTemplate::new(500).set_body_string(format!("unexpected service tier: {other:?}")),
        }
    }
}

#[allow(dead_code, reason = "Intentional compatibility, platform, or test-only suppression.")]
fn schema_keyword_path(value: &Value, keywords: &[&str], path: &str) -> Option<String> {
    match value {
        Value::Object(map) => {
            let properties_object = path.ends_with(".properties");
            if !properties_object {
                for keyword in keywords {
                    if map.contains_key(*keyword) {
                        return Some(format!("{path}.{keyword}"));
                    }
                }
            }
            for (key, nested) in map {
                if let Some(found) = schema_keyword_path(nested, keywords, &format!("{path}.{key}")) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => items
            .iter()
            .enumerate()
            .find_map(|(i, nested)| schema_keyword_path(nested, keywords, &format!("{path}[{i}]"))),
        _ => None,
    }
}

// ─── Auth & Retry Tests ──────────────────────────────────────────────────────

#[test]
fn openai_auth_backend_setup_uses_api_key_base_url_and_static_auth() {
    let setup = OpenAIBackendSetup::from_api_key_config(None);
    let transport = setup.transport();
    let responses = setup.responses_defaults();

    assert_eq!(setup.base_url(), urls::OPENAI_API_BASE);
    assert!(matches!(setup.kind(), OpenAIBackendKind::ApiKey));
    assert_eq!(setup.refresh_behaviour(), OpenAIBackendRefreshBehaviour::StaticBearer);
    assert!(transport.websocket);
    assert!(transport.chat_completions_fallback);
    assert!(transport.responses_compaction_endpoint);
    assert!(!responses.force_store_false);
    assert!(responses.include_prompt_cache_retention);
    assert!(!responses.include_encrypted_reasoning);
}

#[test]
fn chatgpt_auth_backend_setup_uses_rig_chatgpt_by_default() {
    let setup = OpenAIBackendSetup::from_chatgpt_subscription_config(None);
    let transport = setup.transport();
    let responses = setup.responses_defaults();

    assert_eq!(setup.base_url(), CHATGPT_CODEX_BASE);
    assert!(matches!(
        setup.kind(),
        OpenAIBackendKind::ChatGptSubscription(ChatGptSubscriptionAuthSource::RigChatGpt)
    ));
    assert_eq!(setup.refresh_behaviour(), OpenAIBackendRefreshBehaviour::RefreshableChatGptSession);
    assert!(!transport.websocket);
    assert!(!transport.chat_completions_fallback);
    assert!(!transport.responses_compaction_endpoint);
    assert!(responses.force_store_false);
    assert!(!responses.include_prompt_cache_retention);
    assert!(responses.include_encrypted_reasoning);
    assert!(!responses.include_structured_history_in_input);
    assert!(responses.preserve_structured_history_on_replay);
}

#[test]
fn chatgpt_auth_backend_setup_keeps_compatibility_auth_behind_boundary() {
    let setup = OpenAIBackendSetup::chatgpt_subscription_compatibility(CHATGPT_CODEX_BASE.to_string());

    assert_eq!(
        setup.kind(),
        &OpenAIBackendKind::ChatGptSubscription(ChatGptSubscriptionAuthSource::CodexAppServerCompatibility)
    );
    assert!(
        setup.uses_refreshable_auth(),
        "ChatGPT compatibility auth must remain refreshable behind the backend setup boundary"
    );

    let auth = setup.request_auth_from_session(OpenAIChatGptSession {
        openai_api_key: "exchanged-api-key".to_string(),
        id_token: "id-token".to_string(),
        access_token: "oauth-access".to_string(),
        refresh_token: "refresh-token".to_string(),
        account_id: Some("acc_123".to_string()),
        email: Some("test@example.com".to_string()),
        plan: Some("plus".to_string()),
        obtained_at: 1,
        refreshed_at: 1,
        expires_at: None,
    });
    assert_eq!(auth.bearer_token, "oauth-access");
    assert!(auth.rig_chatgpt_auth.is_none(), "compatibility fallback should not masquerade as Rig-backed auth");
}

#[test]
fn chatgpt_auth_backend_setup_applies_account_headers_without_session_id() {
    let setup = OpenAIBackendSetup::from_chatgpt_subscription_config(None);
    let auth = setup.request_auth_from_session(OpenAIChatGptSession {
        openai_api_key: "exchanged-api-key".to_string(),
        id_token: "id-token".to_string(),
        access_token: "oauth-access".to_string(),
        refresh_token: "refresh-token".to_string(),
        account_id: Some("acc_123".to_string()),
        email: Some("test@example.com".to_string()),
        plan: Some("plus".to_string()),
        obtained_at: 1,
        refreshed_at: 1,
        expires_at: None,
    });
    assert_eq!(auth.bearer_token, "oauth-access");
    assert_eq!(auth.chatgpt_account_id.as_deref(), Some("acc_123"));
    assert!(matches!(
        auth.rig_chatgpt_auth.as_ref(),
        Some(RigChatGptAuth::AccessToken {
            access_token,
            account_id: Some(account_id),
        }) if access_token == "oauth-access" && account_id.as_str() == "acc_123"
    ));

    let request = setup
        .authorize_request(
            reqwest::Client::builder()
                .no_proxy()
                .build()
                .expect("test client")
                .get("http://example.com"),
            &auth,
        )
        .build()
        .expect("request should build");

    assert_eq!(request.headers().get("authorization").and_then(|v| v.to_str().ok()), Some("Bearer oauth-access"));
    assert_eq!(request.headers().get("ChatGPT-Account-Id").and_then(|v| v.to_str().ok()), Some("acc_123"));
    assert_eq!(request.headers().get("originator").and_then(|v| v.to_str().ok()), Some("codex_cli_rs"));
    assert!(
        request.headers().get("session_id").is_none(),
        "ChatGPT subscription auth must not forward VT_SESSION_ID or any other conversation/session identifier"
    );
}

#[test]
fn api_key_and_chatgpt_subscription_share_responses_item_history_builder() {
    let native_provider = native_openai_provider(models::openai::GPT_5_6);
    let chatgpt_provider = chatgpt_backend_provider(models::openai::GPT_5_6);
    let mut request = provider::LLMRequest {
        messages: vec![
            provider::Message::system("Follow the local policy.".to_owned()),
            provider::Message::user("Summarise the repository state.".to_owned()),
            provider::Message::assistant("There is one modified file.".to_owned()),
            provider::Message::user("Continue.".to_owned()),
        ]
        .into(),
        model: models::openai::GPT_5_6.to_string(),
        stream: true,
        tools: Some(Arc::new(vec![sample_tool()])),
        ..Default::default()
    };
    request.previous_response_id = Some("resp_previous_123".to_owned());
    request.response_store = Some(true);
    request.responses_include = Some(vec!["output_text.annotations".to_owned()]);

    let native_payload = native_provider
        .convert_to_openai_responses_format(&request)
        .expect("native payload should build");
    let chatgpt_payload = chatgpt_provider
        .convert_to_openai_responses_format(&request)
        .expect("chatgpt payload should build");

    assert_eq!(
        native_payload.get("input"),
        chatgpt_payload.get("input"),
        "API-key and ChatGPT paths must share the same Responses item/history builder"
    );
    assert_eq!(native_payload.get("instructions"), chatgpt_payload.get("instructions"));
    assert_eq!(
        without_responses_backend_fields(native_payload.clone()),
        without_responses_backend_fields(chatgpt_payload.clone())
    );

    assert_absent(&native_payload, "previous_response_id");
    assert_absent(&chatgpt_payload, "previous_response_id");
    assert_eq!(native_payload.get("store").and_then(Value::as_bool), Some(false));
    assert_eq!(
        chatgpt_payload.get("store").and_then(Value::as_bool),
        Some(false),
        "ChatGPT subscription requests must force store=false"
    );
}

#[test]
fn openai_responses_native_emits_store_false_without_previous_response_id() {
    let provider = native_openai_provider(models::openai::GPT_5_6);
    let mut request = sample_request(models::openai::GPT_5_6);
    request.previous_response_id = Some("resp_previous_123".to_string());

    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("native payload should build");

    assert_eq!(payload.get("store").and_then(Value::as_bool), Some(false));
    assert_absent(&payload, "previous_response_id");
}

#[test]
fn chatgpt_responses_emits_store_false_without_previous_response_id() {
    let provider = chatgpt_backend_provider(models::openai::GPT_5_6);
    let mut request = sample_request(models::openai::GPT_5_6);
    request.previous_response_id = Some("resp_previous_123".to_string());

    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("chatgpt payload should build");

    assert_eq!(payload.get("store").and_then(Value::as_bool), Some(false));
    assert_absent(&payload, "previous_response_id");
}

#[test]
fn openai_and_chatgpt_share_responses_payload_builder_except_backend() {
    let native_provider = native_openai_provider(models::openai::GPT_5_6);
    let chatgpt_provider = chatgpt_backend_provider(models::openai::GPT_5_6);
    let mut request = provider::LLMRequest {
        messages: vec![
            provider::Message::system("Follow the local policy.".to_owned()),
            provider::Message::user("Summarise the repository state.".to_owned()),
            provider::Message::assistant("There is one modified file.".to_owned()),
            provider::Message::user("Continue.".to_owned()),
        ]
        .into(),
        model: models::openai::GPT_5_6.to_string(),
        stream: true,
        ..Default::default()
    };
    request.previous_response_id = Some("resp_previous_123".to_owned());
    request.response_store = Some(true);
    request.prompt_cache_key = Some("vtcode:session".to_owned());

    let native_payload = native_provider
        .convert_to_openai_responses_format(&request)
        .expect("native payload should build");
    let chatgpt_payload = chatgpt_provider
        .convert_to_openai_responses_format(&request)
        .expect("chatgpt payload should build");

    assert_eq!(
        without_responses_backend_capability_fields(native_payload),
        without_responses_backend_capability_fields(chatgpt_payload)
    );
}

#[test]
fn openai_and_chatgpt_preserve_prompt_cache_key() {
    let native_provider = native_openai_provider(models::openai::GPT_5_6);
    let chatgpt_provider = chatgpt_backend_provider(models::openai::GPT_5_6);
    let mut request = sample_request(models::openai::GPT_5_6);
    request.prompt_cache_key = Some("vtcode:session".to_owned());

    let native_payload = native_provider
        .convert_to_openai_responses_format(&request)
        .expect("native payload should build");
    let chatgpt_payload = chatgpt_provider
        .convert_to_openai_responses_format(&request)
        .expect("chatgpt payload should build");

    assert_eq!(native_payload.get("prompt_cache_key").and_then(Value::as_str), Some("vtcode:session"));
    assert_eq!(chatgpt_payload.get("prompt_cache_key").and_then(Value::as_str), Some("vtcode:session"));
}

#[test]
fn openai_and_chatgpt_replay_structured_history_on_continuation_turns() {
    let native_provider = native_openai_provider(models::openai::GPT_5_CODEX);
    let chatgpt_provider = chatgpt_backend_provider(models::openai::GPT_5_CODEX);
    let mut request = provider::LLMRequest {
        messages: vec![
            provider::Message::user("Inspect the workspace.".to_owned()),
            provider::Message::assistant_with_tools(
                "Running the requested check.".to_owned(),
                vec![provider::ToolCall::function(
                    "call_1".to_string(),
                    "exec_command".to_string(),
                    "{\"command\":\"cargo check\"}".to_string(),
                )],
            )
            .with_reasoning_details(Some(vec![json!({
                "type": "reasoning",
                "id": "rs_1",
                "encrypted_content": "opaque_reasoning",
                "summary": [{"type": "summary_text", "text": "checked command"}],
            })]))
            .with_phase(Some(provider::AssistantPhase::Commentary)),
            provider::Message::tool_response(
                "call_1".to_string(),
                "{\"output\":\"Finished `dev` profile\",\"exit_code\":0}".to_string(),
            ),
            provider::Message::assistant("The check completed successfully.".to_owned())
                .with_phase(Some(provider::AssistantPhase::FinalAnswer)),
            provider::Message::user("Continue with the next step.".to_owned()),
        ]
        .into(),
        model: models::openai::GPT_5_CODEX.to_string(),
        stream: true,
        ..Default::default()
    };
    request.previous_response_id = Some("resp_previous_123".to_owned());

    for payload in [
        native_provider
            .convert_to_openai_responses_format(&request)
            .expect("native payload should build"),
        chatgpt_provider
            .convert_to_openai_responses_format(&request)
            .expect("chatgpt payload should build"),
    ] {
        assert_eq!(payload.get("store").and_then(Value::as_bool), Some(false));
        assert_absent(&payload, "previous_response_id");
        let input = get_input_array(&payload);
        assert!(
            input.iter().any(|item| item.get("role").and_then(Value::as_str) == Some("user")
                && item.to_string().contains("Inspect the workspace.")),
            "continuation replay must keep the first user turn, not only the suffix"
        );
        assert!(
            input.iter().any(|item| item.get("role").and_then(Value::as_str) == Some("user")
                && item.to_string().contains("Continue with the next step.")),
            "continuation replay must keep the latest user turn"
        );
        assert!(input.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("reasoning")
                && item.get("encrypted_content").and_then(Value::as_str) == Some("opaque_reasoning")
        }));
        assert!(input.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("function_call")
                && item.get("call_id").and_then(Value::as_str) == Some("call_1")
        }));
        assert!(input.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("function_call_output")
                && item.get("call_id").and_then(Value::as_str) == Some("call_1")
        }));
    }
}

#[tokio::test]
async fn chatgpt_backend_uses_oauth_access_token_and_account_header() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };
    let provider = OpenAIProvider::new_with_client(
        "api-key".to_string(),
        Some(sample_chatgpt_auth_handle()),
        models::openai::GPT_5.to_string(),
        reqwest::Client::builder().no_proxy().build().expect("test client"),
        chatgpt_mock_base_url(&server),
        TimeoutsConfig::default(),
    );
    assert!(matches!(
        provider.backend_setup.kind(),
        OpenAIBackendKind::ChatGptSubscription(ChatGptSubscriptionAuthSource::RigChatGpt)
    ));

    let auth = provider.request_auth_from_session(OpenAIChatGptSession {
        openai_api_key: "exchanged-api-key".to_string(),
        id_token: "id-token".to_string(),
        access_token: "oauth-access".to_string(),
        refresh_token: "refresh-token".to_string(),
        account_id: Some("acc_123".to_string()),
        email: Some("test@example.com".to_string()),
        plan: Some("plus".to_string()),
        obtained_at: 1,
        refreshed_at: 1,
        expires_at: None,
    });
    assert_eq!(auth.bearer_token, "oauth-access");
    assert_eq!(auth.chatgpt_account_id.as_deref(), Some("acc_123"));

    let request = provider
        .authorize_with_api_key(provider.http_client.get("http://example.com"), &auth)
        .build()
        .expect("request should build");
    assert_eq!(request.headers().get("authorization").and_then(|v| v.to_str().ok()), Some("Bearer oauth-access"));
    assert_eq!(request.headers().get("ChatGPT-Account-Id").and_then(|v| v.to_str().ok()), Some("acc_123"));
}

#[tokio::test]
async fn api_key_responses_stream_sends_metadata_and_preserves_usage() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };
    let captured = Arc::new(Mutex::new(None::<Value>));
    let captured_for_mock = Arc::clone(&captured);
    let stream_body = concat!(
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello from api key stream\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_api_stream\",\"output\":[],\"usage\":{\"input_tokens\":21,\"output_tokens\":6,\"total_tokens\":27,\"input_tokens_details\":{\"cached_tokens\":8}}}}\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(move |req: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&req.body).expect("valid json");
            *captured_for_mock.lock().expect("not poisoned") = Some(json!({
                "body": body,
                "beta": req.headers.get("openai-beta").and_then(|v| v.to_str().ok()),
                "client_request_id": req.headers.get("x-client-request-id").and_then(|v| v.to_str().ok()),
                "turn_metadata": req.headers.get("x-turn-metadata").and_then(|v| v.to_str().ok()),
            }));
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(stream_body)
        })
        .expect(1)
        .mount(&server)
        .await;

    let provider = OpenAIProvider::new_with_client(
        "test-key".to_string(),
        None,
        models::openai::GPT_5.to_string(),
        reqwest::Client::builder().no_proxy().build().expect("test client"),
        native_openai_mock_base_url(&server),
        TimeoutsConfig::default(),
    );

    let mut stream = provider
        .stream(provider::LLMRequest {
            messages: vec![provider::Message::user("Hello".to_string())].into(),
            model: models::openai::GPT_5.to_string(),
            response_store: Some(true),
            metadata: Some(json!({"commit": "abc123"})),
            ..Default::default()
        })
        .await
        .expect("stream should start");
    let mut text = String::new();
    let mut completed = None;
    while let Some(event) = stream.next().await {
        match event.expect("stream event should parse") {
            provider::LLMStreamEvent::Token { delta } => text.push_str(&delta),
            provider::LLMStreamEvent::Completed { response } => {
                completed = Some(*response);
                break;
            }
            provider::LLMStreamEvent::Reasoning { .. }
            | provider::LLMStreamEvent::ReasoningSignature { .. }
            | provider::LLMStreamEvent::ReasoningStage { .. } => {}
        }
    }

    let response = completed.expect("completion event should be emitted");
    assert_eq!(text, "hello from api key stream");
    assert_eq!(response.content.as_deref(), Some("hello from api key stream"));
    assert_eq!(response.request_id.as_deref(), Some("resp_api_stream"));
    let usage = response.usage.expect("final usage should be preserved");
    assert_eq!(usage.prompt_tokens, 21);
    assert_eq!(usage.completion_tokens, 6);
    assert_eq!(usage.total_tokens, 27);
    assert_eq!(usage.cached_prompt_tokens, None);

    let captured = captured.lock().expect("not poisoned").clone().expect("request captured");
    assert_eq!(captured.get("beta").and_then(Value::as_str), Some("responses=v1"));
    assert!(
        captured
            .get("client_request_id")
            .and_then(Value::as_str)
            .is_some_and(|value| value.starts_with("vtcode-"))
    );
    assert_eq!(captured.get("turn_metadata").and_then(Value::as_str), Some(r#"{"commit":"abc123"}"#));
    let payload = captured.get("body").expect("body captured");
    assert_eq!(payload.get("stream").and_then(Value::as_bool), Some(true));
    assert_eq!(payload.get("store").and_then(Value::as_bool), Some(false));
    assert_ne!(payload.get("include").and_then(Value::as_array), Some(&vec![json!("reasoning.encrypted_content")]));
}

#[tokio::test]
async fn chatgpt_responses_stream_accepts_empty_final_output_after_text_delta() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };
    let captured = Arc::new(Mutex::new(None::<Value>));
    let captured_for_mock = Arc::clone(&captured);
    let stream_body = concat!(
        "data: {\"type\":\"response.queued\",\"response\":{\"id\":\"resp_chatgpt_stream\",\"status\":\"queued\"}}\n\n",
        "data: {\"type\":\"response.file_search_call.searching\",\"item_id\":\"fs_1\",\"output_index\":0}\n\n",
        "data: {\"type\":\"response.code_interpreter_call_code.delta\",\"item_id\":\"ci_1\",\"output_index\":1,\"sequence_number\":1,\"delta\":\"print(1)\"}\n\n",
        "data: {\"type\":\"response.mcp_call_arguments.delta\",\"item_id\":\"mcp_1\",\"call_id\":\"call_mcp\",\"output_index\":2,\"sequence_number\":2,\"delta\":\"{\\\"query\\\":\\\"vtcode\\\"}\"}\n\n",
        "data: {\"type\":\"response.image_generation_call.partial_image\",\"item_id\":\"img_1\",\"output_index\":3,\"sequence_number\":3,\"partial_image_b64\":\"ZmFrZQ==\"}\n\n",
        "data: {\"type\":\"response.custom_tool_call_input.done\",\"item_id\":\"custom_1\",\"call_id\":\"call_custom\",\"output_index\":4,\"sequence_number\":4,\"input\":\"{\\\"cmd\\\":\\\"test\\\"}\"}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello from stream\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_chatgpt_stream\",\"output\":[],\"usage\":{\"input_tokens\":13,\"output_tokens\":4,\"total_tokens\":17}}}\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(move |req: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&req.body).expect("valid json");
            *captured_for_mock.lock().expect("not poisoned") = Some(json!({
                "body": body,
                "account": req.headers.get("chatgpt-account-id").and_then(|v| v.to_str().ok()),
                "originator": req.headers.get("originator").and_then(|v| v.to_str().ok()),
                "beta": req.headers.get("openai-beta").and_then(|v| v.to_str().ok()),
                "has_session_id": req.headers.contains_key("session_id"),
            }));
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(stream_body)
        })
        .expect(1)
        .mount(&server)
        .await;

    let provider = OpenAIProvider::new_with_client(
        String::new(),
        Some(sample_chatgpt_auth_handle()),
        models::openai::GPT_5_6_SOL.to_string(),
        reqwest::Client::builder().no_proxy().build().expect("test client"),
        chatgpt_mock_base_url(&server),
        TimeoutsConfig::default(),
    );

    let mut stream = provider
        .stream(provider::LLMRequest {
            messages: vec![provider::Message::user("Hello".to_string())].into(),
            model: models::openai::GPT_5_6_SOL.to_string(),
            ..Default::default()
        })
        .await
        .expect("stream should start");
    let mut text = String::new();
    let mut completed = None;
    while let Some(event) = stream.next().await {
        match event.expect("stream event should parse") {
            provider::LLMStreamEvent::Token { delta } => text.push_str(&delta),
            provider::LLMStreamEvent::Completed { response } => {
                completed = Some(*response);
                break;
            }
            provider::LLMStreamEvent::Reasoning { .. }
            | provider::LLMStreamEvent::ReasoningSignature { .. }
            | provider::LLMStreamEvent::ReasoningStage { .. } => {}
        }
    }

    let response = completed.expect("completion event should be emitted");
    assert_eq!(text, "hello from stream");
    assert_eq!(response.content.as_deref(), Some("hello from stream"));
    assert_eq!(response.request_id.as_deref(), Some("resp_chatgpt_stream"));
    let usage = response.usage.expect("final usage should be preserved");
    assert_eq!(usage.prompt_tokens, 13);
    assert_eq!(usage.completion_tokens, 4);
    assert_eq!(usage.total_tokens, 17);

    let payload = captured.lock().expect("not poisoned").clone().expect("payload captured");
    assert_eq!(payload.get("account").and_then(Value::as_str), Some("acc_123"));
    assert_eq!(payload.get("originator").and_then(Value::as_str), Some("codex_cli_rs"));
    assert_eq!(payload.get("beta").and_then(Value::as_str), Some("responses=v1"));
    assert_eq!(
        payload.get("has_session_id").and_then(Value::as_bool),
        Some(false),
        "ChatGPT Responses requests must not include a session_id header"
    );
    let payload = payload.get("body").expect("body captured");
    assert_eq!(payload.get("store").and_then(Value::as_bool), Some(false));
    assert_eq!(payload.get("include").and_then(Value::as_array), Some(&vec![json!("reasoning.encrypted_content")]));
}

#[tokio::test]
async fn external_chatgpt_auth_retries_with_refreshed_tokens_after_401() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };
    let refresh_calls = Arc::new(Mutex::new(0usize));
    let seen_bearer_tokens = Arc::new(Mutex::new(Vec::new()));

    Mock::given(method("GET"))
        .and(path("/auth-retry"))
        .respond_with(mock_retry_by_call_count(Arc::clone(&seen_bearer_tokens)))
        .expect(2)
        .mount(&server)
        .await;

    let provider = OpenAIProvider::new_with_client(
        "api-key".to_string(),
        Some(OpenAIChatGptAuthHandle::new_external(
            OpenAIChatGptSession {
                openai_api_key: String::new(),
                id_token: "id-token".to_string(),
                access_token: "oauth-access".to_string(),
                refresh_token: String::new(),
                account_id: Some("acc_123".to_string()),
                email: Some("test@example.com".to_string()),
                plan: Some("plus".to_string()),
                obtained_at: 1,
                refreshed_at: u64::MAX / 2,
                expires_at: None,
            },
            true,
            Arc::new(ExternalSessionRefresher { calls: Arc::clone(&refresh_calls) }),
        )),
        models::openai::GPT_5.to_string(),
        reqwest::Client::builder().no_proxy().build().expect("test client"),
        chatgpt_mock_base_url(&server),
        TimeoutsConfig::default(),
    );

    let response = provider
        .send_authorized(|auth| {
            provider
                .authorize_with_api_key(provider.http_client.get(format!("{}{}", server.uri(), "/auth-retry")), auth)
        })
        .await
        .expect("request should succeed after refresh");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        seen_bearer_tokens.lock().expect("mutex not poisoned").as_slice(),
        &[
            "Bearer oauth-access".to_string(),
            "Bearer oauth-access-refreshed".to_string()
        ]
    );
    assert_eq!(*refresh_calls.lock().expect("mutex not poisoned"), 1);
}

#[tokio::test]
async fn custom_provider_auth_retries_with_refreshed_tokens_after_401() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };
    let tempdir = TempDir::new().expect("tempdir");
    let seen_bearer_tokens = Arc::new(Mutex::new(Vec::new()));

    Mock::given(method("GET"))
        .and(path("/custom-auth-retry"))
        .respond_with(mock_retry_by_call_count(Arc::clone(&seen_bearer_tokens)))
        .expect(2)
        .mount(&server)
        .await;

    let provider = OpenAIProvider::from_custom_config(
        "mycorp".to_string(),
        "MyCorp".to_string(),
        None,
        Some(models::openai::GPT_5.to_string()),
        Some(native_openai_mock_base_url(&server)),
        None,
        None,
        None,
        None,
        Some(CustomProviderAuthHandle::new(
            custom_provider_auth_fixture(&tempdir, &["first-token", "second-token"]),
            None,
        )),
        None,
    );

    let response = provider
        .send_authorized(|auth| {
            provider.authorize_with_api_key(
                provider.http_client.get(format!("{}{}", server.uri(), "/custom-auth-retry")),
                auth,
            )
        })
        .await
        .expect("request should succeed after command-auth refresh");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        seen_bearer_tokens.lock().expect("mutex not poisoned").as_slice(),
        &["Bearer first-token".to_string(), "Bearer second-token".to_string()]
    );
}

fn chat_completion_response_body(text: &str) -> Value {
    json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "created": 0,
        "model": "test-model",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": text },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
    })
}

fn responses_api_response_body(text: &str) -> Value {
    json!({
        "id": "resp_test",
        "object": "response",
        "model": "test-model",
        "output": [{
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": text
            }]
        }],
        "usage": { "input_tokens": 1, "output_tokens": 1 }
    })
}

#[tokio::test]
async fn explicit_openai_chat_format_keeps_chat_completions_path() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_completion_response_body("chat override")))
        .expect(1)
        .mount(&server)
        .await;

    let provider = OpenAIProvider::from_custom_config(
        "custom".to_string(),
        "Custom".to_string(),
        Some("test-key".to_string()),
        Some(models::openai::GPT_5_6.to_string()),
        Some(native_openai_mock_base_url(&server)),
        None,
        None,
        None,
        None,
        None,
        Some(vec![models::openai::GPT_5_6.to_string()]),
    )
    .with_api_format_override(Some(CustomProviderApiFormat::OpenAIChat));

    let response = provider
        .generate(sample_request(models::openai::GPT_5_6))
        .await
        .expect("chat override should succeed");

    assert_eq!(response.content.as_deref(), Some("chat override"));
}

#[tokio::test]
async fn explicit_openai_responses_format_keeps_responses_path() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(responses_api_response_body("responses override")))
        .expect(1)
        .mount(&server)
        .await;

    let provider = OpenAIProvider::from_custom_config(
        "custom".to_string(),
        "Custom".to_string(),
        Some("test-key".to_string()),
        Some(models::openai::GPT_5_6.to_string()),
        Some(native_openai_mock_base_url(&server)),
        None,
        None,
        None,
        None,
        None,
        Some(vec![models::openai::GPT_5_6.to_string()]),
    )
    .with_api_format_override(Some(CustomProviderApiFormat::OpenAIResponses));

    let response = provider
        .generate(sample_request(models::openai::GPT_5_6))
        .await
        .expect("responses override should succeed");

    assert_eq!(response.content.as_deref(), Some("responses override"));
}

#[tokio::test]
async fn auto_openai_format_retains_model_aware_responses_behavior() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"auto responses\"}\n\n\
             data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_auto\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let provider = OpenAIProvider::from_custom_config(
        "custom".to_string(),
        "Custom".to_string(),
        Some("test-key".to_string()),
        Some(models::openai::GPT_5_6_SOL.to_string()),
        Some(native_openai_mock_base_url(&server)),
        None,
        None,
        None,
        None,
        None,
        Some(vec![models::openai::GPT_5_6_SOL.to_string()]),
    );

    let response = provider
        .generate(sample_request(models::openai::GPT_5_6_SOL))
        .await
        .expect("auto behavior should still use responses path");

    assert_eq!(response.content.as_deref(), Some("auto responses"));
}

// ─── Tool Serialization Tests ────────────────────────────────────────────────

#[test]
fn serialize_tools_wraps_function_definition() {
    let serialized = tool_serialization::serialize_tools(&[sample_tool()], models::openai::DEFAULT_MODEL)
        .expect("tools should serialize");
    let tool = serialized.as_array().expect("array")[0].as_object().expect("object");
    assert_eq!(tool.get("type").and_then(Value::as_str), Some("function"));
    assert!(tool.contains_key("function"));
    assert_str_field_obj(tool, "name", "search_workspace");
    assert_eq!(tool.get("description").and_then(Value::as_str).unwrap_or_default(), "Search project files");

    let func = tool
        .get("function")
        .and_then(Value::as_object)
        .expect("function payload missing");
    assert_str_field_obj(func, "name", "search_workspace");
    assert!(func.contains_key("parameters"));
    assert_eq!(tool.get("parameters").and_then(Value::as_object), func.get("parameters").and_then(Value::as_object));
}

#[test]
fn serialize_tools_dedupes_duplicate_names() {
    let dup =
        provider::ToolDefinition::function("search_workspace".to_owned(), "dup".to_owned(), json!({"type": "object"}));
    let serialized = tool_serialization::serialize_tools(&[sample_tool(), dup], models::openai::DEFAULT_MODEL)
        .expect("tools should serialize cleanly");
    assert_eq!(serialized.as_array().expect("array").len(), 1, "duplicate names should be dropped");
}

#[test]
fn code_search_tool_serialization_preserves_simple_constraints() {
    let tool = provider::ToolDefinition::function(
        vtcode_config::constants::tools::CODE_SEARCH.to_owned(),
        "Search code".to_owned(),
        vtcode_utility_tool_specs::code_search_parameters(),
    );
    let serialized = tool_serialization::serialize_tools(std::slice::from_ref(&tool), models::openai::DEFAULT_MODEL)
        .expect("chat tools should serialise");
    let chat_parameters = &serialized.as_array().expect("chat tool array")[0]["parameters"];

    assert_eq!(chat_parameters["required"], json!(["query"]));
    assert_eq!(chat_parameters["additionalProperties"], false);
    let mut property_names = chat_parameters["properties"]
        .as_object()
        .expect("chat properties")
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    property_names.sort_unstable();
    assert_eq!(property_names, ["file_types", "max_results", "path", "query", "result_types"]);
    assert_eq!(chat_parameters["properties"]["max_results"]["minimum"], 1);
    assert_eq!(chat_parameters["properties"]["max_results"]["maximum"], 100);
    assert!(chat_parameters.get("anyOf").is_none());

    let responses =
        tool_serialization::serialize_tools_for_responses(&[tool], None).expect("responses tools should serialise");
    let responses_parameters = &responses.as_array().expect("responses tool array")[0]["parameters"];
    assert_eq!(responses_parameters, chat_parameters);
}

#[test]
fn responses_tools_dedupes_apply_patch_and_function() {
    let tools = vec![
        provider::ToolDefinition::apply_patch("Apply patches".to_owned()),
        provider::ToolDefinition::function("apply_patch".to_owned(), "alt apply".to_owned(), json!({"type": "object"})),
    ];
    let serialized =
        tool_serialization::serialize_tools_for_responses(&tools, None).expect("responses tools should serialize");
    let arr = serialized.as_array().expect("array");
    assert_eq!(arr.len(), 1, "apply_patch should be deduped");
    assert_eq!(arr[0].get("type").and_then(Value::as_str), Some("function"));
    assert_eq!(arr[0].get("name").and_then(Value::as_str), Some("apply_patch"));
}

#[test]
fn responses_payload_serializes_hosted_tool_search_and_deferred_function() {
    let tools = vec![
        provider::ToolDefinition::hosted_tool_search(),
        provider::ToolDefinition::function(
            "search_docs".to_owned(),
            "Search internal docs".to_owned(),
            json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false}),
        ).with_defer_loading(true),
    ];
    let payload =
        tool_serialization::serialize_tools_for_responses(&tools, None).expect("tools should serialize for responses");
    let arr = payload.as_array().expect("tool array");
    assert!(arr.iter().any(|t| t.get("type").and_then(Value::as_str) == Some("tool_search")));
    let deferred = arr
        .iter()
        .find(|t| t.get("name").and_then(Value::as_str) == Some("search_docs"))
        .expect("deferred function should be present");
    assert_eq!(deferred["defer_loading"], json!(true));
}

#[test]
fn responses_payload_emits_allowed_tools_for_native_openai_from_stable_catalogue() {
    let provider = native_openai_provider(models::openai::GPT_5);
    let request = responses_allowed_tools_request(models::openai::GPT_5);
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");

    let full_tools = payload["tools"].as_array().expect("tools should be present");
    assert_eq!(full_tools.len(), 3, "full stable catalogue should remain");
    assert_eq!(
        full_tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>(),
        vec!["search_workspace"]
    );
    assert!(
        full_tools
            .iter()
            .any(|tool| tool.get("type").and_then(Value::as_str) == Some("web_search")),
        "hosted web_search should remain in full catalogue"
    );

    let choice = payload["tool_choice"]
        .as_object()
        .expect("allowed_tools choice should be an object");
    assert_str_field_obj(choice, "type", "allowed_tools");
    assert_str_field_obj(choice, "mode", "auto");
    assert_eq!(
        choice["tools"].as_array().expect("allowed tools array"),
        &vec![json!("search_workspace"), json!("web_search")]
    );
}

#[test]
fn responses_payload_emits_allowed_tools_for_chatgpt_backend() {
    let provider = chatgpt_backend_provider(models::openai::GPT_5);
    let request = responses_allowed_tools_request(models::openai::GPT_5);
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");

    assert_eq!(payload["tool_choice"]["type"].as_str(), Some("allowed_tools"));
}

#[test]
fn responses_payload_omits_allowed_tools_for_compatible_endpoint() {
    let provider = compatible_endpoint_provider(models::openai::GPT_5, "https://example.test/v1");
    let request = responses_allowed_tools_request(models::openai::GPT_5);
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");

    assert_eq!(payload["tool_choice"].as_str(), Some("auto"));
}

#[test]
fn responses_payload_omits_allowed_tools_for_non_responses_model() {
    let provider = native_openai_provider(models::openai::GPT_OSS_20B);
    let request = responses_allowed_tools_request(models::openai::GPT_OSS_20B);
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");

    assert_eq!(payload["tool_choice"].as_str(), Some("auto"));
}

#[test]
fn responses_payload_preserves_any_of_enum_and_nullable_type_unions() {
    let tools = vec![provider::ToolDefinition::function(
        "schema_rich_tool".to_owned(),
        "Schema rich tool".to_owned(),
        json!({
            "type": "object",
            "properties": {
                "open": {
                    "anyOf": [
                        {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "ref_id": {"type": "string"},
                                    "lineno": {"type": ["integer", "null"]}
                                },
                                "required": ["ref_id"],
                                "additionalProperties": false
                            }
                        },
                        {"type": "null"}
                    ]
                },
                "response_length": {
                    "type": "string",
                    "enum": ["short", "medium", "long"]
                },
                "message": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    )];

    let payload =
        tool_serialization::serialize_tools_for_responses(&tools, None).expect("tools should serialize for responses");
    let params = &payload.as_array().expect("tool array")[0]["parameters"];

    assert!(params["properties"]["open"]["anyOf"].is_array());
    assert_eq!(params["properties"]["response_length"]["enum"], json!(["short", "medium", "long"]));
    assert_eq!(params["properties"]["message"]["type"], json!(["string", "null"]));
}

#[test]
fn chat_payload_serializes_deferred_function_for_tool_search() {
    let deferred = provider::ToolDefinition::function(
        "search_docs".to_owned(),
        "Search internal docs".to_owned(),
        json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false}),
    ).with_defer_loading(true);
    let payload =
        tool_serialization::serialize_tools(&[deferred], models::openai::GPT_5_6_SOL).expect("tools should serialize");
    assert_eq!(payload.as_array().expect("array")[0]["defer_loading"], json!(true));
}

// ─── Chat Completions Payload Tests ──────────────────────────────────────────

#[test]
fn chat_completions_payload_uses_function_wrapper() {
    let provider = native_openai_provider(models::openai::DEFAULT_MODEL);
    let payload = chat_payload_for(models::openai::DEFAULT_MODEL, &provider);
    let tools = payload.get("tools").and_then(Value::as_array).expect("tools should exist");
    let tool = tools[0].as_object().expect("tool entry should be object");
    assert!(tool.contains_key("function"));
    assert_eq!(tool.get("name").and_then(Value::as_str), Some("search_workspace"));
}

#[test]
fn chat_completions_applies_gpt_5_5_addendum_only_for_gpt_5_5() {
    let provider = native_openai_provider(models::openai::GPT_5_6_SOL);
    let mut request = sample_request(models::openai::GPT_5_6_SOL);
    request.system_prompt = Some(Arc::from("You are a helpful assistant."));
    let payload = provider.convert_to_openai_format(&request).expect("conversion should succeed");
    let messages = payload
        .get("messages")
        .and_then(Value::as_array)
        .expect("messages should exist");
    let system_content = messages
        .first()
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .expect("system content should be a string");
    assert!(system_content.contains("You are a helpful assistant."));
    assert!(system_content.contains("## GPT-5.5 OpenAI Addendum"));

    let provider = native_openai_provider(models::openai::GPT_5_6_SOL);
    let mut request = sample_request(models::openai::GPT_5_6_SOL);
    request.system_prompt = Some(Arc::from("You are a helpful assistant."));
    let payload = provider.convert_to_openai_format(&request).expect("conversion should succeed");
    let messages = payload
        .get("messages")
        .and_then(Value::as_array)
        .expect("messages should exist");
    let system_content = messages
        .first()
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .expect("system content should be a string");
    assert!(!system_content.contains("## GPT-5.5 OpenAI Addendum"));
}

#[test]
fn chat_completions_uses_max_completion_tokens_field() {
    let provider = compatible_endpoint_provider(models::openai::DEFAULT_MODEL, "https://api.openai.com/v1");
    let mut request = sample_request(models::openai::DEFAULT_MODEL);
    request.max_tokens = Some(512);
    let payload = provider.convert_to_openai_format(&request).expect("conversion should succeed");
    assert_eq!(payload.get("max_completion_tokens").and_then(Value::as_u64), Some(512));
    assert!(payload.get("max_tokens").is_none());
}

#[test]
fn custom_openai_provider_uses_compatible_max_tokens_field_even_on_openai_url() {
    let provider = OpenAIProvider::from_custom_config(
        "openai-compatible".to_string(),
        "OpenAI-compatible".to_string(),
        Some(String::new()),
        Some(models::openai::DEFAULT_MODEL.to_string()),
        Some("https://api.openai.com/v1".to_string()),
        None,
        None,
        None,
        None,
        None,
        None,
    );
    let mut request = sample_request(models::openai::DEFAULT_MODEL);
    request.max_tokens = Some(512);

    let payload = provider.convert_to_openai_format(&request).expect("conversion should succeed");

    assert_eq!(payload.get("max_tokens").and_then(Value::as_u64), Some(512));
    assert!(payload.get("max_completion_tokens").is_none());
}

#[test]
fn chat_completions_applies_temperature_independent_of_max_tokens() {
    let provider = native_openai_provider(models::openai::GPT_5_6);
    let mut request = sample_request(models::openai::GPT_5_6);
    request.temperature = Some(0.4);
    let payload = provider.convert_to_openai_format(&request).expect("conversion should succeed");
    assert!(payload.get("max_completion_tokens").is_none());
    let temp = payload
        .get("temperature")
        .and_then(Value::as_f64)
        .expect("temperature should be present");
    assert!((temp - 0.4).abs() < 1e-6);
}

#[test]
fn chat_completions_omits_temperature_for_gpt_5_5_with_reasoning() {
    let provider = native_openai_provider(models::openai::GPT_5_6_SOL);
    let mut request = sample_request(models::openai::GPT_5_6_SOL);
    request.reasoning_effort = Some(vtcode_config::types::ReasoningEffortLevel::Medium);
    request.temperature = Some(0.4);
    let payload = provider.convert_to_openai_format(&request).expect("conversion should succeed");
    assert!(payload.get("temperature").is_none());
}

#[test]
fn chat_completions_keeps_temperature_for_gpt_5_5_without_reasoning() {
    let provider = native_openai_provider(models::openai::GPT_5_6_SOL);
    let mut request = sample_request(models::openai::GPT_5_6_SOL);
    request.reasoning_effort = Some(vtcode_config::types::ReasoningEffortLevel::None);
    request.temperature = Some(0.4);
    let payload = provider.convert_to_openai_format(&request).expect("conversion should succeed");
    let temp = payload
        .get("temperature")
        .and_then(Value::as_f64)
        .expect("temperature should be present");
    assert!((temp - 0.4).abs() < 1e-6);
}

#[test]
fn chat_payload_omits_assistant_phase_metadata() {
    let provider = native_openai_provider(models::openai::DEFAULT_MODEL);
    let request = provider::LLMRequest {
        messages: vec![
            provider::Message::user("Start".to_owned()),
            provider::Message::assistant("Working".to_owned()).with_phase(Some(provider::AssistantPhase::Commentary)),
        ]
        .into(),
        model: models::openai::DEFAULT_MODEL.to_string(),
        ..Default::default()
    };
    let payload = provider.convert_to_openai_format(&request).expect("conversion should succeed");
    let messages = payload
        .get("messages")
        .and_then(Value::as_array)
        .expect("messages should exist");
    assert!(messages[1].get("phase").is_none());
}

#[test]
fn chat_payload_rejects_file_url_content_parts() {
    let provider = native_openai_provider(models::openai::DEFAULT_MODEL);
    let request = provider::LLMRequest {
        messages: vec![provider::Message::user_with_parts(vec![
            provider::ContentPart::file_from_url("https://example.com/doc.pdf".to_string()),
        ])]
        .into(),
        model: models::openai::DEFAULT_MODEL.to_string(),
        ..Default::default()
    };
    let err = provider
        .convert_to_openai_format(&request)
        .expect_err("chat payload should reject file_url");
    match err {
        provider::LLMError::InvalidRequest { message, .. } => {
            assert!(message.contains("does not support file_url"));
        }
        other => panic!("unexpected error variant: {other:?}"),
    }
}

#[test]
fn chat_payload_includes_prompt_cache_key_for_native_openai() {
    let provider = native_openai_provider(models::openai::DEFAULT_MODEL);
    let mut request = sample_request(models::openai::DEFAULT_MODEL);
    request.prompt_cache_key = Some("vtcode:openai:session-abc".to_string());
    let payload = provider.convert_to_openai_format(&request).expect("conversion should succeed");
    assert_eq!(payload.get("prompt_cache_key").and_then(Value::as_str), Some("vtcode:openai:session-abc"));
}

#[test]
fn chat_payload_uses_provider_level_service_tier_for_native_openai() {
    let provider = OpenAIProvider::from_config(
        Some("key".to_owned()),
        None,
        Some(models::openai::DEFAULT_MODEL.to_string()),
        None,
        None,
        None,
        None,
        Some(priority_openai_config()),
        None,
    );
    let payload = chat_payload_for(models::openai::DEFAULT_MODEL, &provider);
    assert_eq!(payload.get("service_tier").and_then(Value::as_str), Some("priority"));
}

#[test]
fn chat_payload_uses_flex_service_tier_for_native_openai() {
    let provider = OpenAIProvider::from_config(
        Some("key".to_owned()),
        None,
        Some(models::openai::DEFAULT_MODEL.to_string()),
        None,
        None,
        None,
        None,
        Some(flex_openai_config()),
        None,
    );
    let payload = chat_payload_for(models::openai::DEFAULT_MODEL, &provider);
    assert_eq!(payload.get("service_tier").and_then(Value::as_str), Some("flex"));
}

#[test]
fn chat_payload_omits_service_tier_for_models_without_service_tier_support() {
    let provider = OpenAIProvider::from_config(
        Some("key".to_owned()),
        None,
        Some(models::openai::GPT_OSS_20B.to_string()),
        None,
        None,
        None,
        None,
        Some(priority_openai_config()),
        None,
    );
    let payload = chat_payload_for(models::openai::GPT_OSS_20B, &provider);
    assert!(payload.get("service_tier").is_none());
}

#[test]
fn chat_payload_omits_service_tier_for_non_native_openai_base_url() {
    let provider = OpenAIProvider::from_config(
        Some("key".to_owned()),
        None,
        Some(models::openai::DEFAULT_MODEL.to_string()),
        Some("https://example.local/v1".to_string()),
        None,
        None,
        None,
        Some(priority_openai_config()),
        None,
    );
    let payload = chat_payload_for(models::openai::DEFAULT_MODEL, &provider);
    assert!(payload.get("service_tier").is_none());
}

// ─── Responses API Payload Tests ─────────────────────────────────────────────

#[test]
fn responses_payload_uses_function_wrapper() {
    let provider = native_openai_provider(models::openai::GPT_5);
    let payload = responses_payload_for(models::openai::GPT_5, &provider);
    let tools = payload.get("tools").and_then(Value::as_array).expect("tools should exist");
    let tool = tools[0].as_object().expect("tool entry should be object");
    assert_eq!(tool.get("type").and_then(Value::as_str), Some("function"));
    assert_eq!(tool.get("name").and_then(Value::as_str), Some("search_workspace"));
    assert!(tool.contains_key("parameters"));
}

#[test]
fn responses_payload_omits_default_verbosity_for_gpt_5_2_codex() {
    let provider = native_openai_provider(models::openai::GPT_5_CODEX);
    let payload = responses_payload_for(models::openai::GPT_5_CODEX, &provider);
    assert_absent(&payload, "text");
}

#[test]
fn responses_payload_ignores_configured_verbosity_for_gpt_5_2_codex() {
    let provider = native_openai_provider(models::openai::GPT_5_CODEX);
    let mut request = sample_request(models::openai::GPT_5_CODEX);
    request.verbosity = Some(vtcode_config::types::VerbosityLevel::Medium);
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    assert_absent(&payload, "text");
}

#[test]
fn responses_payload_omits_default_verbosity_for_gpt_5_3_codex() {
    let provider = native_openai_provider(models::openai::GPT_5_CODEX);
    let payload = responses_payload_for(models::openai::GPT_5_CODEX, &provider);
    assert_absent(&payload, "text");
}

#[test]
fn responses_payload_keeps_configured_verbosity_for_gpt_5_3_codex() {
    let provider = native_openai_provider(models::openai::GPT_5_CODEX);
    let mut request = sample_request(models::openai::GPT_5_CODEX);
    request.verbosity = Some(vtcode_config::types::VerbosityLevel::High);
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    assert_eq!(payload.get("text").and_then(|t| t.get("verbosity")).and_then(Value::as_str), Some("high"));
}

#[test]
fn responses_payload_keeps_configured_verbosity_for_gpt_5_4() {
    let provider = native_openai_provider(models::openai::GPT_5_6_SOL);
    let mut request = sample_request(models::openai::GPT_5_6_SOL);
    request.verbosity = Some(vtcode_config::types::VerbosityLevel::High);
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    assert_eq!(payload.get("text").and_then(|t| t.get("verbosity")).and_then(Value::as_str), Some("high"));
}

#[test]
fn responses_payload_passes_context_management() {
    let provider = native_openai_provider(models::openai::GPT_5);
    let mut request = sample_request(models::openai::GPT_5);
    request.context_management = Some(json!([{"type": "compaction", "compact_threshold": 200000}]));
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    let mgmt = payload
        .get("context_management")
        .and_then(Value::as_array)
        .expect("context_management should be present");
    assert_eq!(mgmt.len(), 1);
    assert_eq!(mgmt[0].get("type").and_then(Value::as_str), Some("compaction"));
}

#[test]
fn responses_payload_sets_instructions_from_system_prompt() {
    let provider = native_openai_provider(models::openai::GPT_5);
    let mut request = sample_request(models::openai::GPT_5);
    request.system_prompt = Some(Arc::from("You are a helpful assistant."));
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    assert_str_field(&payload, "instructions", "You are a helpful assistant.");
    let input = get_input_array(&payload);
    assert_eq!(input.first().and_then(|v| v.get("role")).and_then(Value::as_str), Some("user"));
}

#[test]
fn responses_payload_applies_gpt_5_5_addendum_only_for_gpt_5_5() {
    let provider = native_openai_provider(models::openai::GPT_5_6_SOL);
    let mut request = sample_request(models::openai::GPT_5_6_SOL);
    request.system_prompt = Some(Arc::from("You are a helpful assistant."));
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    let instructions = payload
        .get("instructions")
        .and_then(Value::as_str)
        .expect("instructions should exist");
    assert!(instructions.contains("You are a helpful assistant."));
    assert!(instructions.contains("## GPT-5.5 OpenAI Addendum"));

    let provider = native_openai_provider(models::openai::GPT_5_6_SOL);
    let mut request = sample_request(models::openai::GPT_5_6_SOL);
    request.system_prompt = Some(Arc::from("You are a helpful assistant."));
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    let instructions = payload
        .get("instructions")
        .and_then(Value::as_str)
        .expect("instructions should exist");
    assert!(!instructions.contains("## GPT-5.5 OpenAI Addendum"));
}

#[test]
fn responses_payload_treats_gpt_5_5_dated_alias_like_gpt_5_5() {
    assert!(OpenAIProvider::is_responses_api_model(models::openai::GPT_5_6_SOL));

    let provider = OpenAIProvider::from_config(
        Some("key".to_owned()),
        None,
        Some(models::openai::GPT_5_6_SOL.to_string()),
        None,
        None,
        None,
        None,
        Some(priority_openai_config()),
        None,
    );
    let mut request = sample_request(models::openai::GPT_5_6_SOL);
    request.system_prompt = Some(Arc::from("You are a helpful assistant."));
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");

    assert_eq!(payload.get("service_tier").and_then(Value::as_str), Some("priority"));
    let instructions = payload
        .get("instructions")
        .and_then(Value::as_str)
        .expect("instructions should exist");
    assert!(instructions.contains("## GPT-5.5 OpenAI Addendum"));
}

#[test]
fn responses_payload_omits_previous_response_and_includes_optional_fields() {
    let provider = native_openai_provider(models::openai::GPT_5);
    let mut request = sample_request(models::openai::GPT_5);
    request.previous_response_id = Some("resp_previous_123".to_string());
    request.response_store = Some(false);
    request.responses_include = Some(vec![
        "reasoning.encrypted_content".to_string(),
        "output_text.annotations".to_string(),
    ]);
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    assert_absent(&payload, "previous_response_id");
    assert_eq!(payload.get("store").and_then(Value::as_bool), Some(false));
    let include = payload
        .get("include")
        .and_then(Value::as_array)
        .expect("include should be present");
    assert_eq!(include.len(), 2);
    assert_eq!(include.first().and_then(Value::as_str), Some("reasoning.encrypted_content"));
}

#[test]
fn compatible_responses_payload_omits_previous_response_id() {
    let provider = compatible_endpoint_provider(models::openai::GPT_5, "https://compat.example/v1");
    let mut request = sample_request(models::openai::GPT_5);
    request.previous_response_id = Some("resp_previous_123".to_string());
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    assert_absent(&payload, "previous_response_id");
    assert_eq!(payload.get("store").and_then(Value::as_bool), Some(false));
}

#[test]
fn responses_payload_serializes_user_input_file_by_id() {
    let provider = native_openai_provider(models::openai::GPT_5);
    let request = provider::LLMRequest {
        messages: vec![provider::Message::user_with_parts(vec![
            provider::ContentPart::text("Summarize this file".to_string()),
            provider::ContentPart::file_from_id("file-abc123".to_string()),
        ])]
        .into(),
        model: models::openai::GPT_5.to_string(),
        ..Default::default()
    };
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    let input = get_input_array(&payload);
    let content = input[0]
        .get("content")
        .and_then(Value::as_array)
        .expect("user content should be an array");
    assert!(content.iter().any(|part| {
        part.get("type").and_then(Value::as_str) == Some("input_file")
            && part.get("file_id").and_then(Value::as_str) == Some("file-abc123")
    }));
}

#[test]
fn responses_payload_serializes_user_input_file_data() {
    let provider = native_openai_provider(models::openai::GPT_5);
    let request = provider::LLMRequest {
        messages: vec![provider::Message::user_with_parts(vec![
            provider::ContentPart::text("Summarize this file".to_string()),
            provider::ContentPart::file_from_data("report.pdf".to_string(), "aGVsbG8=".to_string()),
        ])]
        .into(),
        model: models::openai::GPT_5.to_string(),
        ..Default::default()
    };
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    let input = get_input_array(&payload);
    let content = input[0]
        .get("content")
        .and_then(Value::as_array)
        .expect("user content should be an array");
    assert!(content.iter().any(|part| {
        part.get("type").and_then(Value::as_str) == Some("input_file")
            && part.get("filename").and_then(Value::as_str) == Some("report.pdf")
            && part.get("file_data").and_then(Value::as_str) == Some("aGVsbG8=")
    }));
}

// ─── Hosted Shell Tests ──────────────────────────────────────────────────────

#[test]
fn responses_payload_uses_hosted_shell_when_enabled() {
    let provider = OpenAIProvider::from_config(
        Some(String::new()),
        None,
        Some(models::openai::GPT_5.to_string()),
        Some("https://api.openai.com/v1".to_string()),
        None,
        None,
        None,
        Some(hosted_shell_openai_config()),
        None,
    );
    let request = shell_request(models::openai::GPT_5);
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    let tool = payload["tools"][0].as_object().expect("tool entry should be object");
    assert_str_field_obj(tool, "type", "shell");
    assert_eq!(tool["environment"]["type"].as_str(), Some("container_auto"));
    assert_eq!(tool["environment"]["network_policy"]["type"].as_str(), Some("disabled"));
    assert_eq!(tool["environment"]["file_ids"][0].as_str(), Some("file_123"));
    assert_eq!(tool["environment"]["skills"][0]["type"].as_str(), Some("skill_reference"));
    assert!(tool["environment"]["skills"][0].get("version").is_none());
    let output_types = payload["output_types"].as_array().expect("output types should be present");
    assert!(output_types.iter().any(|v| v.as_str() == Some("shell_call")));
}

#[test]
fn responses_payload_serializes_hosted_shell_allowlist_and_domain_secrets() {
    let provider = OpenAIProvider::from_config(
        Some(String::new()),
        None,
        Some(models::openai::GPT_5.to_string()),
        Some("https://api.openai.com/v1".to_string()),
        None,
        None,
        None,
        Some(OpenAIConfig {
            hosted_shell: OpenAIHostedShellConfig {
                enabled: true,
                environment: OpenAIHostedShellEnvironment::ContainerAuto,
                container_id: None,
                file_ids: Vec::new(),
                skills: Vec::new(),
                network_policy: OpenAIHostedShellNetworkPolicy {
                    policy_type: OpenAIHostedShellNetworkPolicyType::Allowlist,
                    allowed_domains: vec!["httpbin.org".to_string()],
                    domain_secrets: vec![OpenAIHostedShellDomainSecret {
                        domain: "httpbin.org".to_string(),
                        name: "API_KEY".to_string(),
                        value: "debug-secret-123".to_string(),
                    }],
                },
            },
            ..Default::default()
        }),
        None,
    );
    let request = shell_request(models::openai::GPT_5);
    let payload = provider.convert_to_openai_responses_format(&request).expect("should succeed");
    let np = &payload["tools"][0]["environment"]["network_policy"];
    assert_eq!(np["type"].as_str(), Some("allowlist"));
    assert_eq!(np["allowed_domains"][0].as_str(), Some("httpbin.org"));
    assert_eq!(np["domain_secrets"][0]["domain"].as_str(), Some("httpbin.org"));
    assert_eq!(np["domain_secrets"][0]["name"].as_str(), Some("API_KEY"));
    assert_eq!(np["domain_secrets"][0]["value"].as_str(), Some("debug-secret-123"));
}

#[test]
fn responses_payload_omits_explicit_latest_version_and_uses_container_reference() {
    let provider = OpenAIProvider::from_config(
        Some(String::new()),
        None,
        Some(models::openai::GPT_5.to_string()),
        Some("https://api.openai.com/v1".to_string()),
        None,
        None,
        None,
        Some(OpenAIConfig {
            hosted_shell: OpenAIHostedShellConfig {
                enabled: true,
                environment: OpenAIHostedShellEnvironment::ContainerAuto,
                container_id: None,
                file_ids: Vec::new(),
                skills: vec![OpenAIHostedSkill::SkillReference {
                    skill_id: "skill_123".to_string(),
                    version: OpenAIHostedSkillVersion::String(" latest ".to_string()),
                }],
                network_policy: OpenAIHostedShellNetworkPolicy::default(),
            },
            ..Default::default()
        }),
        None,
    );
    let request = shell_request(models::openai::GPT_5);
    let payload = provider.convert_to_openai_responses_format(&request).expect("should succeed");
    assert!(payload["tools"][0]["environment"]["skills"][0].get("version").is_none());

    // Container reference should use container_id, omit file_ids/skills
    let provider2 = OpenAIProvider::from_config(
        Some(String::new()),
        None,
        Some(models::openai::GPT_5.to_string()),
        Some("https://api.openai.com/v1".to_string()),
        None,
        None,
        None,
        Some(OpenAIConfig {
            hosted_shell: OpenAIHostedShellConfig {
                enabled: true,
                environment: OpenAIHostedShellEnvironment::ContainerReference,
                container_id: Some("cntr_123".to_string()),
                file_ids: vec!["file_ignored".to_string()],
                skills: vec![OpenAIHostedSkill::SkillReference {
                    skill_id: "skill_ignored".to_string(),
                    version: OpenAIHostedSkillVersion::default(),
                }],
                network_policy: OpenAIHostedShellNetworkPolicy::default(),
            },
            ..Default::default()
        }),
        None,
    );
    let request2 = shell_request(models::openai::GPT_5);
    let payload2 = provider2.convert_to_openai_responses_format(&request2).expect("should succeed");
    let env = &payload2["tools"][0]["environment"];
    assert_eq!(env["type"].as_str(), Some("container_reference"));
    assert_eq!(env["container_id"].as_str(), Some("cntr_123"));
    assert!(env.get("file_ids").is_none() && env.get("skills").is_none());
}

// Hosted shell fallback: keeps local shell tool when conditions aren't met
#[test]
fn hosted_shell_keeps_local_tool_when_conditions_not_met() {
    // Non-native URL
    let p1 = OpenAIProvider::from_config(
        Some(String::new()),
        None,
        Some(models::openai::GPT_5.to_string()),
        Some("https://example.com/v1".to_string()),
        None,
        None,
        None,
        Some(hosted_shell_openai_config()),
        None,
    );
    let payload1 = p1
        .convert_to_openai_responses_format(&shell_request(models::openai::GPT_5))
        .expect("should succeed");
    let t1 = payload1["tools"][0].as_object().expect("tool");
    assert_str_field_obj(t1, "type", "function");
    assert_str_field_obj(t1, "name", "shell");

    // Blank container reference
    let p2 = OpenAIProvider::from_config(
        Some(String::new()),
        None,
        Some(models::openai::GPT_5.to_string()),
        Some("https://api.openai.com/v1".to_string()),
        None,
        None,
        None,
        Some(OpenAIConfig {
            hosted_shell: OpenAIHostedShellConfig {
                enabled: true,
                environment: OpenAIHostedShellEnvironment::ContainerReference,
                container_id: Some("   ".to_string()),
                file_ids: Vec::new(),
                skills: Vec::new(),
                network_policy: OpenAIHostedShellNetworkPolicy::default(),
            },
            ..Default::default()
        }),
        None,
    );
    let payload2 = p2
        .convert_to_openai_responses_format(&shell_request(models::openai::GPT_5))
        .expect("should succeed");
    let t2 = payload2["tools"][0].as_object().expect("tool");
    assert_str_field_obj(t2, "type", "function");
    assert_str_field_obj(t2, "name", "shell");

    // Blank skill ID
    let p3 = OpenAIProvider::from_config(
        Some(String::new()),
        None,
        Some(models::openai::GPT_5.to_string()),
        Some("https://api.openai.com/v1".to_string()),
        None,
        None,
        None,
        Some(OpenAIConfig {
            hosted_shell: OpenAIHostedShellConfig {
                enabled: true,
                environment: OpenAIHostedShellEnvironment::ContainerAuto,
                container_id: None,
                file_ids: Vec::new(),
                skills: vec![OpenAIHostedSkill::SkillReference {
                    skill_id: "   ".to_string(),
                    version: OpenAIHostedSkillVersion::default(),
                }],
                network_policy: OpenAIHostedShellNetworkPolicy::default(),
            },
            ..Default::default()
        }),
        None,
    );
    let payload3 = p3
        .convert_to_openai_responses_format(&shell_request(models::openai::GPT_5))
        .expect("should succeed");
    assert_str_field_obj(payload3["tools"][0].as_object().expect("tool"), "type", "function");

    // Empty allowlist
    let p4 = OpenAIProvider::from_config(
        Some(String::new()),
        None,
        Some(models::openai::GPT_5.to_string()),
        Some("https://api.openai.com/v1".to_string()),
        None,
        None,
        None,
        Some(OpenAIConfig {
            hosted_shell: OpenAIHostedShellConfig {
                enabled: true,
                environment: OpenAIHostedShellEnvironment::ContainerAuto,
                container_id: None,
                file_ids: Vec::new(),
                skills: Vec::new(),
                network_policy: OpenAIHostedShellNetworkPolicy {
                    policy_type: OpenAIHostedShellNetworkPolicyType::Allowlist,
                    allowed_domains: Vec::new(),
                    domain_secrets: Vec::new(),
                },
            },
            ..Default::default()
        }),
        None,
    );
    let payload4 = p4
        .convert_to_openai_responses_format(&shell_request(models::openai::GPT_5))
        .expect("should succeed");
    assert_str_field_obj(payload4["tools"][0].as_object().expect("tool"), "type", "function");
}

// ─── Validation & Schema Tests ───────────────────────────────────────────────
#[test]
fn responses_validation_rejects_single_inline_file_over_limit() {
    let request = provider::LLMRequest {
        messages: vec![provider::Message::user_with_parts(vec![
            provider::ContentPart::file_from_data("report.pdf".to_string(), "aGVsbG8=".to_string()),
        ])]
        .into(),
        model: models::openai::GPT_5.to_string(),
        ..Default::default()
    };
    let err = OpenAIProvider::validate_inline_file_inputs_with_limit(&request, 4)
        .expect_err("inline file should exceed limit");
    match err {
        provider::LLMError::InvalidRequest { message, .. } => {
            assert!(message.contains("50 MB request limit"));
            assert!(message.contains("report.pdf"));
        }
        other => panic!("unexpected error variant: {other:?}"),
    }
}

#[test]
fn responses_validation_rejects_combined_inline_files_over_limit() {
    let request = provider::LLMRequest {
        messages: vec![provider::Message::user_with_parts(vec![
            provider::ContentPart::file_from_data("a.txt".to_string(), "YWJj".to_string()),
            provider::ContentPart::file_from_data("b.txt".to_string(), "ZGVm".to_string()),
        ])]
        .into(),
        model: models::openai::GPT_5.to_string(),
        ..Default::default()
    };
    let err = OpenAIProvider::validate_inline_file_inputs_with_limit(&request, 5)
        .expect_err("combined inline files should exceed limit");
    match err {
        provider::LLMError::InvalidRequest { message, .. } => {
            assert!(message.contains("50 MB request limit"));
            assert!(message.contains("total inline file bytes = 6"));
        }
        other => panic!("unexpected error variant: {other:?}"),
    }
}

// NOTE: responses_function_tools_sanitize_openai_incompatible_parameter_keywords and
// responses_function_tools_strip_openai_schema_combinators_from_builtin_tools were removed
// because they required vtcode-core::tools types which are not available in vtcode-llm.
// If this test coverage is needed, move them to vtcode-core.

#[test]
fn responses_function_tools_add_empty_properties_for_bare_object_schema() {
    let provider = native_openai_provider(models::openai::GPT_5_CODEX);
    let request = provider::LLMRequest {
        messages: vec![provider::Message::user("Hello".to_owned())].into(),
        tools: Some(Arc::new(vec![provider::ToolDefinition::function(
            "vtcode-clippy".to_owned(),
            "Run clippy on the workspace".to_owned(),
            json!({"type": "object", "additionalProperties": true}),
        )])),
        model: models::openai::GPT_5_CODEX.to_string(),
        ..Default::default()
    };
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    let params = payload["tools"]
        .as_array()
        .expect("tool array")
        .iter()
        .find(|t| t.get("name").and_then(Value::as_str) == Some("vtcode-clippy"))
        .and_then(|t| t.get("parameters"))
        .expect("vtcode-clippy parameters");
    assert_eq!(params["type"].as_str(), Some("object"));
    assert_eq!(params["properties"], json!({}));
    assert_eq!(params["additionalProperties"], json!({"type": "string"}));
}

// ─── Specialized Tool Types ──────────────────────────────────────────────────
#[test]
fn responses_payload_serializes_hosted_web_search_tool() {
    let provider = native_openai_provider(models::openai::GPT_5);
    let request = provider::LLMRequest {
        messages: vec![provider::Message::user("Find the latest VT Code news".to_owned())].into(),
        tools: Some(Arc::new(vec![provider::ToolDefinition::web_search(
            json!({"search_context_size": "medium"}),
        )])),
        model: models::openai::GPT_5.to_string(),
        ..Default::default()
    };
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    let tools = payload.get("tools").and_then(Value::as_array).expect("tools should exist");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].get("type").and_then(Value::as_str), Some("web_search"));
    assert_eq!(tools[0].get("search_context_size").and_then(Value::as_str), Some("medium"));
}

#[test]
fn responses_payload_serializes_file_search_tool() {
    let provider = native_openai_provider(models::openai::GPT_5);
    let request = provider::LLMRequest {
        messages: vec![provider::Message::user("Search the docs vector store".to_owned())].into(),
        tools: Some(Arc::new(vec![provider::ToolDefinition::file_search(
            json!({"vector_store_ids": ["vs_docs"]}),
        )])),
        model: models::openai::GPT_5.to_string(),
        ..Default::default()
    };
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    let tools = payload.get("tools").and_then(Value::as_array).expect("tools should exist");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].get("type").and_then(Value::as_str), Some("file_search"));
    assert_eq!(
        tools[0]
            .get("vector_store_ids")
            .and_then(Value::as_array)
            .and_then(|ids| ids.first())
            .and_then(Value::as_str),
        Some("vs_docs")
    );
}

#[test]
fn responses_payload_keeps_distinct_remote_mcp_tools() {
    let provider = native_openai_provider(models::openai::GPT_5);
    let request = provider::LLMRequest {
        messages: vec![provider::Message::user("Use both MCP servers".to_owned())].into(),
        tools: Some(Arc::new(vec![
            provider::ToolDefinition::mcp(json!({
                "server_label": "dmcp",
                "server_url": "https://dmcp-server.deno.dev/sse",
                "require_approval": "never"
            })),
            provider::ToolDefinition::mcp(json!({
                "server_label": "docs",
                "server_url": "https://docs.example/sse",
                "require_approval": "never"
            })),
        ])),
        model: models::openai::GPT_5.to_string(),
        ..Default::default()
    };
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    let tools = payload.get("tools").and_then(Value::as_array).expect("tools should exist");
    assert_eq!(tools.len(), 2);
    assert!(
        tools
            .iter()
            .any(|t| t.get("server_label").and_then(Value::as_str) == Some("dmcp"))
    );
    assert!(
        tools
            .iter()
            .any(|t| t.get("server_label").and_then(Value::as_str) == Some("docs"))
    );
}

// ─── ChatGPT Backend History Tests ───────────────────────────────────────────

#[test]
fn chatgpt_backend_omits_previous_response_id_from_responses_payload() {
    let provider = chatgpt_backend_provider(models::openai::GPT_5_CODEX);
    let mut request = sample_request(models::openai::GPT_5_CODEX);
    request.previous_response_id = Some("resp_previous_123".to_string());
    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed");
    assert_absent(&payload, "previous_response_id");
}

// Helper to build ChatGPT backend history payload
fn chatgpt_codex_payload(messages: Vec<provider::Message>, model: &str) -> Value {
    let provider = chatgpt_backend_provider(model);
    let request = provider::LLMRequest {
        messages: messages.into(),
        model: model.to_string(),
        ..Default::default()
    };
    provider
        .convert_to_openai_responses_format(&request)
        .expect("conversion should succeed")
}

#[test]
fn chatgpt_backend_keeps_plain_assistant_history_structured_for_codex() {
    let payload = chatgpt_codex_payload(
        vec![
            provider::Message::user("What is this project?".to_owned()),
            provider::Message::assistant("VT Code is a Rust Cargo workspace.".to_owned())
                .with_phase(Some(provider::AssistantPhase::FinalAnswer)),
            provider::Message::user("Tell me more.".to_owned()),
        ],
        models::openai::GPT_5_CODEX,
    );
    let input = get_input_array(&payload);
    assert_eq!(input.len(), 3);
    assert_eq!(input_role_at(&payload, 0), Some("user"));
    assert_eq!(input_role_at(&payload, 1), Some("assistant"));
    assert_absent(&input[1], "phase");
    assert_eq!(input_role_at(&payload, 2), Some("user"));
    assert!(
        payload["instructions"]
            .as_str()
            .unwrap()
            .contains("You are Codex, based on GPT-5.")
    );
}

#[test]
fn chatgpt_backend_preserves_reasoning_detail_items_for_codex_follow_up() {
    let payload = chatgpt_codex_payload(
        vec![
            provider::Message::assistant("Hello. What would you like me to do?".to_owned())
                .with_reasoning_details(Some(vec![json!({
                    "type": "reasoning",
                    "id": "rs_1",
                    "summary": [{"type":"summary_text","text":"task prompt"}]
                })]))
                .with_phase(Some(provider::AssistantPhase::FinalAnswer)),
            provider::Message::user("tell me more".to_owned()),
        ],
        models::openai::GPT_5_CODEX,
    );
    let input = get_input_array(&payload);
    assert_eq!(input.len(), 3);
    assert_eq!(input_type_at(&payload, 0), Some("reasoning"));
    assert_eq!(input_role_at(&payload, 1), Some("assistant"));
    assert_absent(&input[1], "phase");
    assert_eq!(input_role_at(&payload, 2), Some("user"));
}

#[test]
fn chatgpt_backend_keeps_tool_turn_history_structured_for_codex() {
    let payload = chatgpt_codex_payload(
        vec![
            provider::Message::user("run cargo check".to_owned()),
            provider::Message::assistant_with_tools(
                String::new(),
                vec![provider::ToolCall::function(
                    "call_1".to_string(),
                    "exec_command".to_string(),
                    "{\"command\":\"cargo check\"}".to_string(),
                )],
            ),
            provider::Message::tool_response(
                "call_1".to_string(),
                "{\"output\":\"Finished `dev` profile\",\"exit_code\":0}".to_string(),
            ),
            provider::Message::assistant("cargo check completed successfully.".to_owned())
                .with_phase(Some(provider::AssistantPhase::FinalAnswer)),
            provider::Message::user("who are you".to_owned()),
        ],
        models::openai::GPT_5_CODEX,
    );
    let input = get_input_array(&payload);
    assert_eq!(input.len(), 5);
    assert_eq!(input_role_at(&payload, 0), Some("user"));
    assert_eq!(input_type_at(&payload, 1), Some("function_call"));
    assert_eq!(input_call_id_at(&payload, 1), Some("call_1"));
    assert_eq!(input_type_at(&payload, 2), Some("function_call_output"));
    assert_eq!(input_call_id_at(&payload, 2), Some("call_1"));
    assert_eq!(input_role_at(&payload, 3), Some("assistant"));
    assert_eq!(input_role_at(&payload, 4), Some("user"));
    assert_absent(&input[3], "phase");
}

// Parametrized phase omission tests for ChatGPT backend models
#[test]
fn chatgpt_backend_omits_assistant_phase_for_codex_models() {
    for model in [models::openai::GPT_5_CODEX, models::openai::GPT_5_6_SOL] {
        let payload = chatgpt_codex_payload(
            vec![
                provider::Message::user("Run the next check.".to_owned()),
                provider::Message::assistant("Checking.".to_owned())
                    .with_phase(Some(provider::AssistantPhase::Commentary)),
                provider::Message::assistant("Done.".to_owned())
                    .with_phase(Some(provider::AssistantPhase::FinalAnswer)),
                provider::Message::user("Continue.".to_owned()),
            ],
            model,
        );
        let input = get_input_array(&payload);
        assert_eq!(input.len(), 4);
        assert_absent(&input[1], "phase");
        assert_absent(&input[2], "phase");
    }
}

#[test]
fn chatgpt_backend_preserves_structured_tool_turns_with_paired_function_calls() {
    for model in [models::openai::GPT_5_CODEX, models::openai::GPT_5_6_SOL] {
        let payload = chatgpt_codex_payload(
            vec![
                provider::Message::user("Investigate the failing check.".to_owned()),
                provider::Message::assistant_with_tools(
                    "Checking the first command output.".to_owned(),
                    vec![provider::ToolCall::function(
                        "call_1".to_string(),
                        "exec_command".to_string(),
                        "{\"command\":\"cargo check -p vtcode-core\"}".to_string(),
                    )],
                )
                .with_phase(Some(provider::AssistantPhase::Commentary)),
                provider::Message::tool_response(
                    "call_1".to_string(),
                    "{\"output\":\"warning: example\",\"exit_code\":0}".to_string(),
                ),
                provider::Message::assistant("Need one more inspection step.".to_owned())
                    .with_phase(Some(provider::AssistantPhase::Commentary)),
                provider::Message::user("Continue.".to_owned()),
            ],
            model,
        );
        let input = get_input_array(&payload);
        assert_eq!(input.len(), 6);
        assert_eq!(input_role_at(&payload, 0), Some("user"));
        assert_eq!(input_role_at(&payload, 1), Some("assistant"));
        assert_absent(&input[1], "phase");
        assert_eq!(input_type_at(&payload, 2), Some("function_call"));
        assert_eq!(input_call_id_at(&payload, 2), Some("call_1"));
        assert_eq!(input_type_at(&payload, 3), Some("function_call_output"));
        assert_eq!(input_call_id_at(&payload, 3), Some("call_1"));
        assert_eq!(input_role_at(&payload, 4), Some("assistant"));
        assert_absent(&input[4], "phase");
        assert_eq!(input_role_at(&payload, 5), Some("user"));
    }
}

#[test]
fn chatgpt_backend_replays_prior_direct_tool_turns() {
    let payload = chatgpt_codex_payload(
        vec![
            provider::Message::user("run cargo fmt".to_owned()),
            provider::Message::assistant_with_tools(
                String::new(),
                vec![provider::ToolCall::function(
                    "direct_exec_command_1".to_string(),
                    "exec_command".to_string(),
                    "{\"command\":\"cargo fmt\"}".to_string(),
                )],
            ),
            provider::Message::tool_response(
                "direct_exec_command_1".to_string(),
                "{\"output\":\"\",\"exit_code\":0,\"backend\":\"pipe\"}".to_string(),
            ),
            provider::Message::assistant("cargo fmt completed successfully.".to_owned())
                .with_phase(Some(provider::AssistantPhase::FinalAnswer)),
            provider::Message::user("continue".to_owned()),
        ],
        models::openai::GPT_5_CODEX,
    );
    let input = get_input_array(&payload);
    assert_eq!(input.len(), 5);
    assert_eq!(input_type_at(&payload, 1), Some("function_call"));
    assert_eq!(input_type_at(&payload, 2), Some("function_call_output"));
    assert_eq!(input_call_id_at(&payload, 1), Some("direct_exec_command_1"));
    assert_eq!(input_call_id_at(&payload, 2), Some("direct_exec_command_1"));
    assert!(input.iter().all(|item| {
        let t = item.get("type").and_then(Value::as_str);
        t != Some("tool_call") && t != Some("tool_result")
    }));
}

#[test]
fn chatgpt_backend_synthesizes_missing_function_call_outputs_for_orphan_calls() {
    let payload = chatgpt_codex_payload(
        vec![
            provider::Message::user("Run commands".to_owned()),
            provider::Message::assistant_with_tools(
                String::new(),
                vec![provider::ToolCall::function(
                    "call_orphan".to_string(),
                    "exec_command".to_string(),
                    "{\"command\":\"echo orphan\"}".to_string(),
                )],
            ),
            provider::Message::assistant_with_tools(
                String::new(),
                vec![provider::ToolCall::function(
                    "call_paired".to_string(),
                    "exec_command".to_string(),
                    "{\"command\":\"echo paired\"}".to_string(),
                )],
            ),
            provider::Message::tool_response(
                "call_paired".to_string(),
                "{\"output\":\"paired\",\"exit_code\":0}".to_string(),
            ),
            provider::Message::user("continue".to_owned()),
        ],
        models::openai::GPT_5_CODEX,
    );
    let input = get_input_array(&payload);
    assert!(input.iter().any(|i| {
        i.get("type").and_then(Value::as_str) == Some("function_call")
            && i.get("call_id").and_then(Value::as_str) == Some("call_orphan")
    }));
    assert!(input.iter().any(|i| {
        i.get("type").and_then(Value::as_str) == Some("function_call_output")
            && i.get("call_id").and_then(Value::as_str) == Some("call_orphan")
            && i.get("output").and_then(Value::as_str) == Some("aborted")
    }));
    assert!(input.iter().any(|i| {
        i.get("type").and_then(Value::as_str) == Some("function_call_output")
            && i.get("call_id").and_then(Value::as_str) == Some("call_paired")
    }));
}

// Phase behavior: native OpenAI includes assistant phase, non-native omits it
#[test]
fn responses_payload_phase_behavior() {
    // Native: includes phase for assistant, omits for user/tool
    let native = native_openai_provider(models::openai::GPT_5_6_SOL);
    let request = provider::LLMRequest {
        messages: vec![
            provider::Message::user("Start".to_owned()).with_phase(Some(provider::AssistantPhase::Commentary)),
            provider::Message::assistant("Checking.".to_owned()).with_phase(Some(provider::AssistantPhase::Commentary)),
            provider::Message::assistant_with_tools(
                "Looking up.".to_owned(),
                vec![provider::ToolCall::function(
                    "call_1".to_string(),
                    "search_workspace".to_string(),
                    r#"{"query":"x"}"#.to_string(),
                )],
            )
            .with_phase(Some(provider::AssistantPhase::Commentary)),
            provider::Message::tool_response("call_1".to_string(), "{\"ok\":true}".to_string())
                .with_phase(Some(provider::AssistantPhase::FinalAnswer)),
        ]
        .into(),
        model: models::openai::GPT_5_6_SOL.to_string(),
        ..Default::default()
    };
    let payload = native.convert_to_openai_responses_format(&request).expect("should succeed");
    let input = get_input_array(&payload);
    assert!(input[0].get("phase").is_none(), "user omits phase");
    assert_eq!(input[1].get("phase").and_then(Value::as_str), Some("commentary"));
    assert_eq!(input[2].get("phase").and_then(Value::as_str), Some("commentary"));
    assert!(input[3].get("phase").is_none(), "tool response omits phase");

    // Non-native: omits phase entirely
    let non_native = compatible_endpoint_provider(models::openai::GPT_5_6_SOL, "https://example.local/v1");
    let request2 = provider::LLMRequest {
        messages: vec![
            provider::Message::user("Start".to_owned()),
            provider::Message::assistant("Checking.".to_owned()).with_phase(Some(provider::AssistantPhase::Commentary)),
        ]
        .into(),
        model: models::openai::GPT_5_6_SOL.to_string(),
        ..Default::default()
    };
    let payload2 = non_native
        .convert_to_openai_responses_format(&request2)
        .expect("should succeed");
    assert!(get_input_array(&payload2)[1].get("phase").is_none());
}

// ─── ChatGPT Backend Omissions ───────────────────────────────────────────────

#[test]
fn chatgpt_backend_forces_store_false_and_omits_output_sampling_cache() {
    let provider = OpenAIProvider::from_config(
        Some(String::new()),
        Some(sample_chatgpt_auth_handle()),
        Some(models::openai::GPT_5_6.to_string()),
        None,
        None,
        None,
        None,
        Some(OpenAIConfig { responses_store: Some(true), ..Default::default() }),
        None,
    );
    let payload = responses_payload_for(models::openai::GPT_5_6, &provider);
    assert_eq!(payload.get("store").and_then(Value::as_bool), Some(false));
    assert_absent(&payload, "output_types");
    assert_absent(&payload, "sampling_parameters");
    assert_absent(&payload, "prompt_cache_retention");

    // With temperature/top_p set, still omitted
    let mut request = sample_request(models::openai::GPT_5_6);
    request.temperature = Some(0.4);
    request.top_p = Some(0.8);
    let payload2 = provider.convert_to_openai_responses_format(&request).expect("should succeed");
    assert_absent(&payload2, "sampling_parameters");
}

#[test]
fn chatgpt_backend_includes_encrypted_reasoning_and_preserves_configured_includes() {
    let provider = chatgpt_backend_provider(models::openai::GPT_5_6_SOL);
    let payload = responses_payload_for(models::openai::GPT_5_6_SOL, &provider);
    assert_eq!(payload.get("include").and_then(Value::as_array), Some(&vec![json!("reasoning.encrypted_content")]));

    let provider = OpenAIProvider::from_config(
        Some(String::new()),
        Some(sample_chatgpt_auth_handle()),
        Some(models::openai::GPT_5_6_SOL.to_string()),
        None,
        None,
        None,
        None,
        Some(OpenAIConfig {
            responses_include: vec![
                "output_text.annotations".to_string(),
                " reasoning.encrypted_content ".to_string(),
            ],
            ..Default::default()
        }),
        None,
    );
    let payload = responses_payload_for(models::openai::GPT_5_6_SOL, &provider);
    assert_eq!(
        payload.get("include").and_then(Value::as_array),
        Some(&vec![json!("output_text.annotations"), json!("reasoning.encrypted_content"),])
    );
}

#[test]
fn chatgpt_backend_disables_chat_completions_fallback() {
    let provider = chatgpt_backend_provider(models::openai::GPT_5_6);
    assert!(provider.is_chatgpt_backend());
    assert!(!provider.allows_chat_completions_fallback());
}

// ─── Compaction & Responses API State ────────────────────────────────────────

#[test]
fn supports_responses_compaction_tracks_responses_api_availability() {
    let openai = native_openai_provider(models::openai::GPT_5);
    assert!(openai.supports_responses_compaction(models::openai::GPT_5));
    let compatible = compatible_endpoint_provider(models::openai::GPT_5, "https://compat.example/v1");
    assert!(compatible.supports_responses_compaction(models::openai::GPT_5));
    let xai = compatible_endpoint_provider(models::openai::GPT_5, "https://api.x.ai/v1");
    assert!(!xai.supports_responses_compaction(models::openai::GPT_5));
}

#[test]
fn supports_manual_openai_compaction_is_native_only() {
    let openai = native_openai_provider(models::openai::GPT_5);
    assert!(openai.supports_manual_openai_compaction(models::openai::GPT_5));
    assert!(
        !compatible_endpoint_provider(models::openai::GPT_5, "https://compat.example/v1")
            .supports_manual_openai_compaction(models::openai::GPT_5)
    );
    assert!(
        !OpenAIProvider::from_custom_config(
            "custom".to_string(),
            "Custom".to_string(),
            Some(String::new()),
            Some(models::openai::GPT_5.to_string()),
            Some("https://api.openai.com/v1".to_string()),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .supports_manual_openai_compaction(models::openai::GPT_5)
    );
    assert!(!chatgpt_backend_provider(models::openai::GPT_5).supports_manual_openai_compaction(models::openai::GPT_5));
    assert!(!openai.supports_manual_openai_compaction("gpt-4.1"));
}

#[test]
fn manual_openai_compaction_unavailable_message_mentions_backend() {
    let chatgpt = chatgpt_backend_provider(models::openai::GPT_5);
    let msg = chatgpt.manual_openai_compaction_unavailable_message(models::openai::GPT_5);
    assert!(msg.contains("ChatGPT subscription auth via chatgpt.com backend"));
    let compatible = compatible_endpoint_provider(models::openai::GPT_5, "https://compat.example/v1");
    let msg2 = compatible.manual_openai_compaction_unavailable_message(models::openai::GPT_5);
    assert!(msg2.contains("configured OpenAI-compatible endpoint (https://compat.example/v1)"));
    let openai = native_openai_provider(models::openai::GPT_5);
    let msg3 = openai.manual_openai_compaction_unavailable_message("gpt-4.1");
    assert!(msg3.contains("native OpenAI API (api.openai.com)"));
    assert!(msg3.contains("openai / native OpenAI API (api.openai.com) / gpt-4.1"));
}

// ─── Supported Models & Harmony ──────────────────────────────────────────────

#[test]
fn supported_models_include_current_reasoning_models() {
    let supported = OpenAIProvider::new("key".to_owned()).supported_models();
    // Current reasoning models must be in the supported list.
    assert!(supported.contains(&"gpt-5.6-sol".to_string()));
    assert!(supported.contains(&models::openai::DEFAULT_MODEL.to_string()));
    // Deprecated o-series models are removed from the picker but retained in
    // REASONING_MODELS for backward-compat routing.
    assert!(!supported.contains(&models::openai::O3.to_string()));
    assert!(!supported.contains(&models::openai::O4_MINI.to_string()));
    assert!(models::openai::REASONING_MODELS.contains(&models::openai::O3));
    assert!(models::openai::REASONING_MODELS.contains(&models::openai::O4_MINI));
}

#[test]
fn harmony_detection_handles_common_variants() {
    assert!(OpenAIProvider::uses_harmony("gpt-oss-20b"));
    assert!(OpenAIProvider::uses_harmony("openai/gpt-oss-20b:free"));
    assert!(OpenAIProvider::uses_harmony("OPENAI/GPT-OSS-120B"));
    assert!(!OpenAIProvider::uses_harmony("gpt-5"));
    assert!(!OpenAIProvider::uses_harmony("gpt-oss:20b"));
}

// ─── Prompt Cache & Websocket ────────────────────────────────────────────────

use vtcode_config::core::PromptCachingConfig;

#[test]
fn responses_payload_includes_prompt_cache_retention_for_native_openai() {
    let mut pc = PromptCachingConfig::default();
    pc.providers.openai.prompt_cache_retention = Some(PromptCacheRetention::H24);
    let provider = OpenAIProvider::from_config(
        Some("key".to_owned()),
        None,
        Some(models::openai::GPT_5_6.to_string()),
        None,
        Some(pc),
        None,
        None,
        None,
        None,
    );
    // Responses API model
    let payload = responses_payload_for(models::openai::GPT_5_CODEX, &provider);
    assert_eq!(payload.get("prompt_cache_retention").and_then(Value::as_str), Some("24h"));
    // Chat Completions model - should NOT have it
    let chat_payload = chat_payload_for(models::openai::GPT_5, &provider);
    assert_absent(&chat_payload, "prompt_cache_retention");
}

#[test]
fn responses_payload_includes_prompt_cache_key_for_native_openai() {
    let provider = native_openai_provider(models::openai::GPT_5_6);
    let mut request = sample_request(models::openai::GPT_5_6);
    request.prompt_cache_key = Some("vtcode:openai:session-123".to_string());
    let payload = provider.convert_to_openai_responses_format(&request).expect("should succeed");
    assert_eq!(payload.get("prompt_cache_key").and_then(Value::as_str), Some("vtcode:openai:session-123"));
}

#[test]
fn responses_payload_omits_prompt_cache_key_for_non_native() {
    let provider = compatible_endpoint_provider(models::openai::GPT_5_6, "https://example.local/v1");
    let mut request = sample_request(models::openai::GPT_5_6);
    request.prompt_cache_key = Some("vtcode:openai:session-xyz".to_string());
    assert_absent(&provider.convert_to_openai_responses_format(&request).expect("should succeed"), "prompt_cache_key");
}

#[test]
fn prompt_cache_retention_excluded_when_not_set_and_for_unsupported_models() {
    let mut pc = PromptCachingConfig::default();
    pc.providers.openai.prompt_cache_retention = None;
    let provider = OpenAIProvider::from_config(
        Some("key".to_string()),
        None,
        Some(models::openai::GPT_5_6.to_string()),
        None,
        Some(pc),
        None,
        None,
        None,
        None,
    );
    let mut request = sample_request(models::openai::GPT_5_6);
    request.stream = true;
    assert_absent(
        &provider.convert_to_openai_responses_format(&request).expect("should succeed"),
        "prompt_cache_retention",
    );

    // Unsupported model also omits it
    let mut pc2 = PromptCachingConfig::default();
    pc2.providers.openai.prompt_cache_retention = Some(PromptCacheRetention::H24);
    let provider2 = OpenAIProvider::from_config(
        Some("key".to_string()),
        None,
        Some(models::openai::GPT_OSS_20B.to_string()),
        None,
        Some(pc2),
        None,
        None,
        None,
        None,
    );
    assert_absent(&responses_payload_for(models::openai::GPT_OSS_20B, &provider2), "prompt_cache_retention");
}

#[test]
fn provider_from_config_respects_prompt_cache_and_websocket_gating() {
    let mut pc = PromptCachingConfig::default();
    pc.providers.openai.prompt_cache_retention = Some(PromptCacheRetention::InMemory);
    let provider = OpenAIProvider::from_config(
        Some("key".to_string()),
        None,
        Some(models::openai::GPT_5_6.to_string()),
        None,
        Some(pc.clone()),
        None,
        None,
        None,
        None,
    );
    assert_eq!(provider.prompt_cache_settings.prompt_cache_retention, Some(PromptCacheRetention::InMemory));

    let native_ws = OpenAIProvider::from_config(
        Some("key".to_string()),
        None,
        Some(models::openai::GPT_5_6.to_string()),
        None,
        None,
        None,
        None,
        Some(OpenAIConfig { websocket_mode: true, ..Default::default() }),
        None,
    );
    assert!(native_ws.websocket_mode_enabled(models::openai::GPT_5_6));

    let compatible_ws = OpenAIProvider::from_config(
        Some("key".to_string()),
        None,
        Some(models::openai::GPT_5_6.to_string()),
        Some("https://compat.example/v1".to_string()),
        None,
        None,
        None,
        Some(OpenAIConfig { websocket_mode: true, ..Default::default() }),
        None,
    );
    assert!(compatible_ws.websocket_mode_enabled(models::openai::GPT_5_6));

    let custom_ws = OpenAIProvider::from_custom_config(
        "mycorp".to_string(),
        "MyCorp".to_string(),
        Some("key".to_string()),
        Some(models::openai::GPT_5_6.to_string()),
        Some("https://compat.example/v1".to_string()),
        None,
        None,
        Some(OpenAIConfig { websocket_mode: true, ..Default::default() }),
        None,
        None,
        None,
    );
    assert!(custom_ws.websocket_mode_enabled(models::openai::GPT_5_6));

    let chatgpt_ws = OpenAIProvider::from_config(
        Some(String::new()),
        Some(sample_chatgpt_auth_handle()),
        Some(models::openai::GPT_5_6.to_string()),
        None,
        None,
        None,
        None,
        Some(OpenAIConfig { websocket_mode: true, ..Default::default() }),
        None,
    );
    assert!(!chatgpt_ws.websocket_mode_enabled(models::openai::GPT_5_6));

    let xai_ws = OpenAIProvider::from_config(
        Some("key".to_string()),
        None,
        Some(models::openai::GPT_5_6.to_string()),
        Some("https://api.x.ai/v1".to_string()),
        None,
        None,
        None,
        Some(OpenAIConfig { websocket_mode: true, ..Default::default() }),
        None,
    );
    assert!(!xai_ws.websocket_mode_enabled(models::openai::GPT_5_6));
}

// ─── Max Tokens & Reasoning ──────────────────────────────────────────────────

#[test]
fn responses_payload_uses_max_output_tokens_field() {
    let provider = native_openai_provider(models::openai::GPT_5);
    let mut request = sample_request(models::openai::GPT_5);
    request.max_tokens = Some(512);
    let payload = provider.convert_to_openai_responses_format(&request).expect("should succeed");
    assert_eq!(payload.get("max_output_tokens").and_then(Value::as_u64), Some(512));
    assert_absent(&payload, "max_completion_tokens");
}

#[test]
fn chatgpt_backend_omits_max_output_tokens_and_blocks_unsupported_minimal_reasoning() {
    let provider = chatgpt_backend_provider(models::openai::GPT_5_CODEX);
    let mut request = sample_request(models::openai::GPT_5_CODEX);
    request.max_tokens = Some(512);
    assert_absent(&provider.convert_to_openai_responses_format(&request).expect("should succeed"), "max_output_tokens");

    request.max_tokens = None;
    request.reasoning_effort = Some(vtcode_config::types::ReasoningEffortLevel::Minimal);
    let error = provider
        .convert_to_openai_responses_format(&request)
        .expect_err("unsupported effort must be blocked without explicit degradation");
    assert!(error.to_string().contains("unsupported by this route"));
}

#[test]
fn responses_payload_defaults_gpt_5_4_reasoning_to_none() {
    let payload =
        responses_payload_for(models::openai::GPT_5_6_SOL, &native_openai_provider(models::openai::GPT_5_6_SOL));
    assert_eq!(payload.get("reasoning").and_then(|r| r.get("effort")).and_then(Value::as_str), Some("none"));
}

#[test]
fn responses_payload_defaults_gpt_5_3_codex_reasoning_to_high() {
    let payload =
        responses_payload_for(models::openai::GPT_5_CODEX, &native_openai_provider(models::openai::GPT_5_CODEX));
    assert_eq!(payload.get("reasoning").and_then(|r| r.get("effort")).and_then(Value::as_str), Some("high"));
}

#[test]
fn responses_payload_omits_sampling_parameters_for_gpt_5_4_high_reasoning() {
    let provider = native_openai_provider(models::openai::GPT_5_6_SOL);
    let mut request = sample_request(models::openai::GPT_5_6_SOL);
    request.reasoning_effort = Some(vtcode_config::types::ReasoningEffortLevel::High);
    request.temperature = Some(0.4);
    request.top_p = Some(0.9);
    assert_absent(
        &provider.convert_to_openai_responses_format(&request).expect("should succeed"),
        "sampling_parameters",
    );
}

#[test]
fn responses_payload_omits_penalties_when_sampling_gate_closed() {
    let provider = native_openai_provider(models::openai::GPT_5_6_SOL);
    let mut request = sample_request(models::openai::GPT_5_6_SOL);
    request.reasoning_effort = Some(vtcode_config::types::ReasoningEffortLevel::High);
    request.presence_penalty = Some(0.1);
    request.frequency_penalty = Some(-0.5);
    let payload = provider.convert_to_openai_responses_format(&request).expect("should succeed");
    assert_absent(&payload, "sampling_parameters");
}

// ─── Streaming Tests ─────────────────────────────────────────────────────────

#[test]
fn openai_models_support_streaming() {
    for model in [
        models::openai::GPT,
        models::openai::GPT_5,
        models::openai::GPT_5_6_SOL,
        models::openai::GPT_5_6_SOL,
        models::openai::GPT_5_MINI,
        models::openai::GPT_5_NANO,
    ] {
        let provider = test_provider("http://test", model);
        assert!(provider.supports_streaming(), "Model {model} should support streaming");
    }
}

#[test]
fn native_stream_required_models_disable_non_streaming() {
    for model in [
        models::openai::GPT,
        models::openai::GPT_5_6_SOL,
        models::openai::GPT_5_6_SOL,
        models::openai::GPT_5_6_SOL,
        models::openai::GPT_5_6_SOL,
    ] {
        let provider = test_provider("http://test", model);
        assert!(!provider.supports_non_streaming(model), "Model {model} should require streaming");
    }
}

#[test]
fn chatgpt_backend_keeps_streaming_for_codex_and_disables_non_streaming() {
    let provider = chatgpt_backend_provider(models::openai::GPT_5_CODEX);
    assert!(provider.supports_streaming());
    assert!(!provider.supports_non_streaming(models::openai::GPT_5_CODEX));
}

// ─── Harmony Parsing ─────────────────────────────────────────────────────────

#[test]
fn parse_harmony_tool_names_and_calls() {
    assert_eq!(
        OpenAIProvider::parse_harmony_tool_name("functions.code_search"),
        vtcode_config::constants::tools::CODE_SEARCH
    );
    assert_eq!(OpenAIProvider::parse_harmony_tool_name("grep"), "grep");
    assert_eq!(OpenAIProvider::parse_harmony_tool_name("container.exec"), "exec_command");
    assert_eq!(OpenAIProvider::parse_harmony_tool_name("unknown.tool"), "tool");
    assert!(!OpenAIProvider::uses_harmony("gpt-oss:20b"));

    let (name, args) = OpenAIProvider::parse_harmony_tool_call_from_text(
        r#"to=functions.code_search {"query":"Widget", "path":"src"}"#,
    )
    .expect("should parse");
    assert_eq!(name, vtcode_config::constants::tools::CODE_SEARCH);
    assert_eq!(args["query"], json!("Widget"));
    assert_eq!(args["path"], json!("src"));

    let (name2, args2) =
        OpenAIProvider::parse_harmony_tool_call_from_text(r#"to=container.exec {"cmd":["ls", "-la"]}"#)
            .expect("should parse");
    assert_eq!(name2, "exec_command");
    assert_eq!(args2["cmd"], json!(["ls", "-la"]));

    let text = r#"<|start|>assistant to=functions.lookup_weather<|channel|>commentary <|constrain|>json<|message|>{"location":"San Francisco"}<|call|>"#;
    let (name3, args3) = OpenAIProvider::parse_harmony_tool_call_from_text(text).expect("should parse");
    assert_eq!(name3, "lookup_weather");
    assert_eq!(args3["location"], json!("San Francisco"));

    let text2 = r#"<|start|>assistant to=functions.lookup_weather<|channel|>commentary <|constrain|>json<|message|>{'location':'San Francisco'}<|call|>"#;
    let (name4, _) = OpenAIProvider::parse_harmony_tool_call_from_text(text2).expect("should parse");
    assert_eq!(name4, "lookup_weather");
}

// ─── Retry & Fallback Tests ──────────────────────────────────────────────────

#[tokio::test]
async fn responses_request_retries_with_fallback_model_after_not_found() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };
    let provider = test_provider(&server.uri(), models::openai::GPT_5_NANO);
    let seen_models = Arc::new(Mutex::new(Vec::new()));
    let seen_for_mock = Arc::clone(&seen_models);

    Mock::given(method("POST")).and(path("/responses"))
        .respond_with(move |req: &wiremock::Request| {
            let payload: Value = serde_json::from_slice(&req.body).expect("valid json");
            let model = payload.get("model").and_then(Value::as_str).expect("model required");
            seen_for_mock.lock().expect("not poisoned").push(model.to_string());
            match model {
                models::openai::GPT_5_NANO => ResponseTemplate::new(404).set_body_string("model_not_found"),
                models::openai::GPT_5_MINI => ResponseTemplate::new(200).set_body_json(json!({
                    "id": "resp_fallback", "status": "completed",
                    "output": [{"type":"message","role":"assistant","content":[{"type":"output_text","text":"fallback response"}]}]
                })),
                other => ResponseTemplate::new(500).set_body_string(format!("unexpected: {other}")),
            }
        }).expect(2).mount(&server).await;

    let response = provider
        .generate(provider::LLMRequest {
            messages: vec![provider::Message::user("Hello".to_string())].into(),
            model: models::openai::GPT_5_NANO.to_string(),
            ..Default::default()
        })
        .await
        .expect("fallback should succeed");
    assert_eq!(response.content.as_deref(), Some("fallback response"));
    assert_eq!(
        seen_models.lock().expect("not poisoned").as_slice(),
        &[
            models::openai::GPT_5_NANO.to_string(),
            models::openai::GPT_5_MINI.to_string()
        ]
    );
}

#[tokio::test]
async fn responses_request_retries_without_flex_service_tier() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };
    let provider = OpenAIProvider::from_config(
        Some("key".to_owned()),
        None,
        Some(models::openai::GPT_5_CODEX.to_string()),
        Some(native_openai_mock_base_url(&server)),
        None,
        None,
        None,
        Some(flex_openai_config()),
        None,
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_for_mock = Arc::clone(&seen);

    Mock::given(method("POST")).and(path("/responses"))
        .respond_with(mock_service_tier_fallback(seen_for_mock, json!({
            "id": "resp_retry", "status": "completed",
            "output": [{"type":"message","role":"assistant","content":[{"type":"output_text","text":"retry without flex succeeded"}]}]
        }))).expect(2).mount(&server).await;

    let response = provider
        .generate(provider::LLMRequest {
            messages: vec![provider::Message::user("Hello".to_string())].into(),
            model: models::openai::GPT_5_CODEX.to_string(),
            ..Default::default()
        })
        .await
        .expect("retry without flex should succeed");
    assert_eq!(response.content.as_deref(), Some("retry without flex succeeded"));
    assert_eq!(seen.lock().expect("not poisoned").as_slice(), &[Some("flex".to_string()), None]);
}

// ─── Request Metadata & Content Type ─────────────────────────────────────────

#[tokio::test]
async fn responses_requests_include_client_request_id_and_debug_metadata() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };
    let provider = test_provider(&server.uri(), models::openai::GPT_5);
    Mock::given(method("POST")).and(path("/responses"))
        .respond_with(|req: &wiremock::Request| {
            let req_id = req.headers.get("x-client-request-id").and_then(|v| v.to_str().ok())
                .expect("x-client-request-id required");
            assert!(req_id.starts_with("vtcode-"));
            ResponseTemplate::new(400)
                .insert_header("x-request-id", "req_123")
                .insert_header("retry-after", "15")
                .set_body_string(r#"{"error":{"message":"Bad request","type":"invalid_request_error","param":"text.verbosity","code":"unsupported_parameter"}}"#)
        }).expect(1).mount(&server).await;
    let err = provider
        .generate(provider::LLMRequest {
            messages: vec![provider::Message::user("Hello".to_string())].into(),
            model: models::openai::GPT_5.to_string(),
            ..Default::default()
        })
        .await
        .expect_err("should surface error");
    let text = err.to_string();
    assert!(
        text.contains("request_id=req_123")
            && text.contains("client_request_id=vtcode-")
            && text.contains("retry_after=15")
            && text.contains("type=invalid_request_error")
    );
}

// ─── Manual Compaction ───────────────────────────────────────────────────────

#[tokio::test]
async fn manual_compaction_payload_includes_selected_fields_and_appends_instructions() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };
    let provider = test_provider(&native_openai_mock_base_url(&server), models::openai::GPT_5_6_SOL);
    let captured = Arc::new(Mutex::new(None::<Value>));
    let captured_for_mock = Arc::clone(&captured);
    Mock::given(method("POST"))
        .and(path("/responses/compact"))
        .respond_with(move |req: &wiremock::Request| {
            *captured_for_mock.lock().expect("not poisoned") =
                Some(serde_json::from_slice(&req.body).expect("valid json"));
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "resp_compact", "status": "completed",
                "output": [{"type":"message","role":"assistant","content":[{"type":"output_text","text":"compacted"}]}]
            }))
        })
        .expect(1)
        .mount(&server)
        .await;

    let compacted = provider
        .compact_history_with_options(
            models::openai::GPT_5_6_SOL,
            &[
                provider::Message::system("Preserve decisions.".to_string()),
                provider::Message::user("Summarize.".to_string()),
            ],
            &provider::ResponsesCompactionOptions {
                instructions: Some("Terse.".to_string()),
                max_output_tokens: Some(321),
                // GPT-5.6 Sol advertises low as its smallest native effort;
                // unsupported Minimal requests are rejected before transport.
                reasoning_effort: Some(vtcode_config::types::ReasoningEffortLevel::Low),
                verbosity: Some(vtcode_config::types::VerbosityLevel::High),
                responses_include: Some(vec!["reasoning.encrypted_content".to_string()]),
                response_store: Some(true),
                service_tier: Some("priority".to_string()),
                prompt_cache_key: Some("lineage-key".to_string()),
            },
        )
        .await
        .expect("compaction should succeed");
    assert_eq!(compacted.len(), 1);

    let p = captured.lock().expect("not poisoned").clone().expect("payload captured");
    assert_eq!(p["model"], json!(models::openai::GPT_5_6_SOL));
    assert_eq!(p["max_output_tokens"], json!(321));
    assert_eq!(p["service_tier"], json!("priority"));
    assert_eq!(p["store"], json!(false));
    assert_eq!(p["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(p["reasoning"]["effort"], json!("low"));
    assert_eq!(p["text"]["verbosity"], json!("high"));
    assert_eq!(p["prompt_cache_key"], json!("lineage-key"));
    assert!(p.get("previous_response_id").is_none() && p.get("output_types").is_none() && p.get("stream").is_none());
    let instr = p["instructions"].as_str().expect("instructions required");
    assert!(
        instr.contains("Preserve decisions.")
            && instr.contains("[Manual Compaction Instructions]")
            && instr.contains("Terse.")
    );
}

// ─── Debug redaction for OpenAIRequestAuth ───────────────────────────────────

#[test]
fn openai_request_auth_debug_redacts_bearer_token() {
    use super::super::backend_setup::OpenAIRequestAuth;

    let auth = OpenAIRequestAuth::bearer_token("sk-secret-bearer-token".to_string());
    let debug_str = format!("{auth:?}");
    assert!(!debug_str.contains("sk-secret-bearer-token"), "bearer token leaked in Debug: {debug_str}");
}

#[test]
fn openai_request_auth_debug_redacts_rig_chatgpt_auth() {
    use super::super::backend_setup::OpenAIRequestAuth;

    let auth = OpenAIRequestAuth {
        bearer_token: "rig-secret-access".to_string(),
        chatgpt_account_id: Some("acc_123".to_string()),
        rig_chatgpt_auth: Some(RigChatGptAuth::AccessToken {
            access_token: "rig-secret-access".to_string(),
            account_id: Some("acc_123".to_string()),
        }),
    };
    let debug_str = format!("{auth:?}");
    assert!(!debug_str.contains("rig-secret-access"), "rig access token leaked in Debug: {debug_str}");
    // Non-secret metadata should still be visible.
    assert!(debug_str.contains("acc_123"), "account_id should be visible: {debug_str}");
}
