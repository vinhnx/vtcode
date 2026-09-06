//! SACP `AgentToClient` handler registration for `ZedAgent`.
//!
//! The bridge translates SACP request/notification handlers into calls on
//! the existing [`ZedAgent`] methods. The methods themselves are mostly
//! preserved from the pre-1.0.0 trait-based implementation; the only thing
//! that changed is the wiring layer (this file) and the connection storage
//! ([`crate::zed::connection::ConnectionHandle`]).
//!
//! ## Spawn pattern
//!
//! [`acp::PromptRequest`] is the one handler that needs to
//! drive the SACP event loop forward *while* it runs. Per the SACP docs we
//! therefore use [`ConnectionTo::spawn`] from inside the handler closure,
//! and the actual prompt logic runs in the spawned task. The spawned task
//! has full access to `block_task()` and the agent's client-side helpers.
//!
//! All other handlers are quick and can be served synchronously without
//! `spawn`.
//!
//! ## Connection access
//!
//! The canonical `ConnectionHandle` is stashed in
//! [`crate::register_acp_connection`] by `run_acp_agent` before the
//! SACP event loop starts. Handlers reach the same handle through the
//! agent's `self.client()` accessor — they must **not** re-wrap the
//! per-handler `cx` into a new `ConnectionHandle` or they will race
//! each other on the global `Mutex<Option<Arc<ConnectionHandle>>>`
//! inside the agent.

use super::super::constants::*;
use super::super::helpers::{agent_implementation_info, text_chunk};
use super::super::types::{PlanProgress, ToolRuntime};
use super::ZedAgent;
use crate::acp;
use crate::acp::Error as SdkError;
use agent_client_protocol::schema::v1::{
    AuthenticateRequest, AuthenticateResponse, CancelNotification, InitializeRequest, InitializeResponse,
    LoadSessionRequest, LoadSessionResponse, NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse,
    SetSessionConfigOptionRequest, SetSessionConfigOptionResponse,
};
use agent_client_protocol::{
    Agent, Builder, Client, ConnectionTo, HandleDispatchFrom, Responder, RunWithConnectionTo, on_receive_notification,
    on_receive_request,
};
use futures::StreamExt;
use serde_json::json;
use std::sync::Arc;
use tracing::warn;
use vtcode_core::config::api_keys::{ApiKeySources, get_api_key_with_mode};
use vtcode_core::llm::factory::ProviderConfig;
use vtcode_core::llm::factory::create_provider_with_config;
use vtcode_core::llm::provider::{LLMRequest, LLMStreamEvent, Message};

/// Register every SACP `AgentToClient` request/notification handler that the
/// vtcode bridge implements. The agent must be `Send + Sync + 'static` so
/// that the SACP `Builder` can move the handlers onto its background task.
pub fn install_handlers<H, R>(
    builder: Builder<Agent, H, R>,
    agent: Arc<ZedAgent>,
) -> Builder<Agent, impl HandleDispatchFrom<Client>, R>
where
    H: HandleDispatchFrom<Client>,
    R: RunWithConnectionTo<Client>,
{
    builder
        .on_receive_request(
            {
                let agent = Arc::clone(&agent);
                move |req: InitializeRequest, request_cx: Responder<InitializeResponse>, _cx| {
                    let agent = Arc::clone(&agent);
                    async move { handle_initialize(agent, req, request_cx).await }
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = Arc::clone(&agent);
                move |_req: AuthenticateRequest, request_cx: Responder<AuthenticateResponse>, _cx| {
                    let agent = Arc::clone(&agent);
                    async move { handle_authenticate(agent, request_cx).await }
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = Arc::clone(&agent);
                move |req: NewSessionRequest, request_cx: Responder<NewSessionResponse>, _cx| {
                    let agent = Arc::clone(&agent);
                    async move { handle_new_session(agent, req, request_cx).await }
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = Arc::clone(&agent);
                move |req: LoadSessionRequest, request_cx: Responder<LoadSessionResponse>, _cx| {
                    let agent = Arc::clone(&agent);
                    async move { handle_load_session(agent, req, request_cx).await }
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = Arc::clone(&agent);
                move |req: SetSessionConfigOptionRequest, request_cx: Responder<SetSessionConfigOptionResponse>, _cx| {
                    let agent = Arc::clone(&agent);
                    async move { handle_set_session_config_option(agent, req, request_cx).await }
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = Arc::clone(&agent);
                move |req: PromptRequest, request_cx: Responder<PromptResponse>, cx| {
                    let agent = Arc::clone(&agent);
                    async move { handle_prompt(agent, req, request_cx, cx).await }
                }
            },
            on_receive_request!(),
        )
        .on_receive_notification(
            {
                let agent = Arc::clone(&agent);
                move |notif: CancelNotification, _cx: ConnectionTo<Client>| {
                    let agent = Arc::clone(&agent);
                    async move { handle_cancel(agent, notif).await }
                }
            },
            on_receive_notification!(),
        )
}

async fn handle_initialize(
    agent: Arc<ZedAgent>,
    args: InitializeRequest,
    request_cx: Responder<InitializeResponse>,
) -> Result<(), SdkError> {
    let caps = args.client_capabilities.clone();
    if let Ok(mut guard) = agent.client_capabilities.lock() {
        *guard = Some(caps);
    }
    if args.protocol_version != acp::ProtocolVersion::V1 {
        warn!(
            requested = %args.protocol_version,
            "{}",
            INITIALIZE_VERSION_MISMATCH_LOG
        );
    }
    let mut capabilities = acp::AgentCapabilities::default();
    capabilities.prompt_capabilities.embedded_context = true;
    capabilities.prompt_capabilities.image = true;
    capabilities.prompt_capabilities.audio = true;
    capabilities.mcp_capabilities.http = true;
    capabilities.mcp_capabilities.sse = false;
    capabilities.load_session = true;

    let auth_methods = build_auth_methods();
    let response = InitializeResponse::new(acp::ProtocolVersion::V1)
        .agent_capabilities(capabilities)
        .agent_info(agent_implementation_info(agent.title()))
        .auth_methods(auth_methods);
    request_cx.respond(response)
}

fn build_auth_methods() -> Vec<acp::AuthMethod> {
    let mut methods = vec![
        acp::AuthMethod::Agent(
            acp::AuthMethodAgent::new("oauth-openai", "OpenAI OAuth")
                .description("Authenticate with OpenAI via OAuth 2.0 with PKCE"),
        ),
        acp::AuthMethod::Agent(
            acp::AuthMethodAgent::new("oauth-openrouter", "OpenRouter OAuth")
                .description("Authenticate with OpenRouter via OAuth 2.0 with PKCE"),
        ),
    ];
    methods.push(acp::AuthMethod::Terminal(
        acp::AuthMethodTerminal::new("terminal-login", "Terminal Login")
            .description("Interactive terminal-based authentication via vtcode login command")
            .args(vec!["login".to_string()]),
    ));
    methods.push(acp::AuthMethod::EnvVar(acp::AuthMethodEnvVar::new("env-api-keys", "API Key", env_api_keys())));
    methods.push(acp::AuthMethod::EnvVar(acp::AuthMethodEnvVar::new(
        "env-base-urls",
        "API Base URL",
        env_base_urls(),
    )));
    methods
}

fn env_api_keys() -> Vec<acp::AuthEnvVar> {
    const ENTRIES: &[(&str, &str, bool)] = &[
        ("OPENAI_API_KEY", "OpenAI", false),
        ("ANTHROPIC_API_KEY", "Anthropic", false),
        ("GEMINI_API_KEY", "Google Gemini", false),
        ("OPENROUTER_API_KEY", "OpenRouter", false),
        ("DEEPSEEK_API_KEY", "DeepSeek", false),
        ("META_API_KEY", "Meta AI", false),
        ("MODEL_API_KEY", "Meta AI (documented)", false),
        ("ZAI_API_KEY", "Z.AI", false),
        ("MOONSHOT_API_KEY", "Moonshot", false),
        ("MINIMAX_API_KEY", "MiniMax", false),
        ("GROQ_API_KEY", "Groq", false),
        ("XAI_API_KEY", "xAI", false),
        ("COHERE_API_KEY", "Cohere", false),
        ("HF_TOKEN", "Hugging Face", false),
        ("MISTRAL_API_KEY", "Mistral", false),
        ("GOOGLE_API_KEY", "Google (alt)", true),
        ("OLLAMA_API_KEY", "Ollama", true),
        ("OLLAMA_API_KEY", "Ollama Cloud", false),
        ("LMSTUDIO_API_KEY", "LM Studio", true),
    ];
    ENTRIES
        .iter()
        .map(|(name, label, optional)| {
            let mut var = acp::AuthEnvVar::new(*name).label(*label);
            if *optional {
                var = var.optional(true);
            }
            var
        })
        .collect()
}

fn env_base_urls() -> Vec<acp::AuthEnvVar> {
    const ENTRIES: &[(&str, &str)] = &[
        ("OPENAI_BASE_URL", "OpenAI"),
        ("ANTHROPIC_BASE_URL", "Anthropic"),
        ("GEMINI_BASE_URL", "Gemini"),
        ("OPENROUTER_BASE_URL", "OpenRouter"),
        ("DEEPSEEK_BASE_URL", "DeepSeek"),
        ("META_BASE_URL", "Meta AI"),
        ("ZAI_BASE_URL", "Z.AI"),
        ("MOONSHOT_BASE_URL", "Moonshot"),
        ("MINIMAX_BASE_URL", "MiniMax"),
        ("XAI_BASE_URL", "xAI"),
        ("HUGGINGFACE_BASE_URL", "Hugging Face"),
        ("OLLAMA_BASE_URL", "Ollama"),
        ("LMSTUDIO_BASE_URL", "LM Studio"),
    ];
    ENTRIES
        .iter()
        .map(|(name, label)| acp::AuthEnvVar::new(*name).label(*label).optional(true))
        .collect()
}

async fn handle_authenticate(
    _agent: Arc<ZedAgent>,
    request_cx: Responder<AuthenticateResponse>,
) -> Result<(), SdkError> {
    request_cx.respond(AuthenticateResponse::default())
}

async fn handle_new_session(
    agent: Arc<ZedAgent>,
    req: NewSessionRequest,
    request_cx: Responder<NewSessionResponse>,
) -> Result<(), SdkError> {
    let response = agent.new_session(req).await?;
    request_cx.respond(response)
}

async fn handle_load_session(
    agent: Arc<ZedAgent>,
    req: LoadSessionRequest,
    request_cx: Responder<LoadSessionResponse>,
) -> Result<(), SdkError> {
    let response = agent.load_session(req).await?;
    request_cx.respond(response)
}

async fn handle_set_session_config_option(
    agent: Arc<ZedAgent>,
    req: SetSessionConfigOptionRequest,
    request_cx: Responder<SetSessionConfigOptionResponse>,
) -> Result<(), SdkError> {
    let response = agent.set_session_config_option(req).await?;
    request_cx.respond(response)
}

async fn handle_cancel(agent: Arc<ZedAgent>, notif: CancelNotification) -> Result<(), SdkError> {
    if let Some(session) = agent.session_handle(&notif.session_id) {
        session.cancel_flag.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(())
}

async fn handle_prompt(
    agent: Arc<ZedAgent>,
    req: PromptRequest,
    request_cx: Responder<PromptResponse>,
    cx: ConnectionTo<Client>,
) -> Result<(), SdkError> {
    // The prompt handler drives several SACP RPCs (`fs/read_text_file`,
    // `terminal/create`, `session/request_permission`) from inside the
    // prompt loop. Those would deadlock if called directly on the
    // dispatch loop's task, so we spawn the work onto a child task that
    // is allowed to use `block_task()`. The canonical `ConnectionHandle`
    // was registered globally by `run_acp_agent` — we do not re-wrap
    // `cx` into a new handle here, that would race with concurrent
    // prompts on the agent's internal `Mutex<Option<Arc<ConnectionHandle>>>`.
    cx.spawn({
        let agent = Arc::clone(&agent);
        async move {
            let result = run_prompt(agent, req).await;
            if let Err(error) = request_cx.respond_with_result(result) {
                warn!(%error, "Failed to send prompt response");
            }
            Ok(())
        }
    })
    .map_err(|error| SdkError::internal_error().data(error.to_string()))?;
    Ok(())
}

async fn run_prompt(agent: Arc<ZedAgent>, args: PromptRequest) -> Result<PromptResponse, SdkError> {
    let Some(session) = agent.session_handle(&args.session_id) else {
        return Err(SdkError::invalid_params().data(json!({ "reason": "unknown_session" })));
    };

    session.cancel_flag.store(false, std::sync::atomic::Ordering::Relaxed);

    let user_message = agent.resolve_prompt(&args.session_id, &args.prompt).await?;

    agent.push_message(&session, Message::user(user_message.clone()));

    let (session_provider_name, session_model, session_reasoning_effort) = {
        let data = session.data.lock().map_err(|_err| SdkError::internal_error())?;
        (data.provider.clone(), data.model.clone(), data.reasoning_effort)
    };

    let session_api_key = resolve_api_key_for_provider(&agent, &session_provider_name);
    let provider = create_provider_with_config(
        &session_provider_name,
        ProviderConfig {
            api_key: Some(session_api_key),
            openai_chatgpt_auth: if session_provider_name.eq_ignore_ascii_case("openai") {
                agent.config.openai_chatgpt_auth.clone()
            } else {
                None
            },
            copilot_auth: None,
            base_url: None,
            model: Some(session_model.clone()),
            prompt_cache: Some(agent.config.prompt_cache.clone()),
            timeouts: None,
            openai: None,
            anthropic: None,
            model_behavior: agent.config.model_behavior.clone(),
            workspace_root: Some(agent.config.workspace.clone()),
        },
    )
    .map_err(|err| SdkError::internal_error().data(err.to_string()))?;

    let supports_streaming = provider.supports_streaming();
    let reasoning_effort = if session_reasoning_effort == vtcode_core::config::types::ReasoningEffortLevel::None {
        None
    } else {
        let mapping = vtcode_core::llm::reasoning_effort::ReasoningEffortMapper::resolve(
            provider.as_ref(),
            &session_model,
            session_reasoning_effort,
            agent.allow_reasoning_effort_downgrade,
        )
        .map_err(|error| {
            SdkError::invalid_params().data(json!({
                "reason": "unsupported_reasoning_effort",
                "detail": error.to_string(),
            }))
        })?;
        if mapping.degraded() {
            warn!(
                requested = %mapping.requested,
                effective = %mapping.effective,
                model = %session_model,
                "ACP reasoning effort explicitly downgraded"
            );
        }
        Some(mapping.effective)
    };

    let mut stop_reason = acp::StopReason::EndTurn;
    let mut assistant_message = String::with_capacity(4096);
    let client_supports_read_text_file = agent.client_supports_read_text_file();
    let provider_supports_tools = provider.supports_tools(&session_model);
    let mut primary_agent = {
        let data = session.data.lock().map_err(|_err| SdkError::internal_error())?;
        data.primary_agent.clone()
    };
    let availability = agent.tool_availability(provider_supports_tools, client_supports_read_text_file);
    let mut enabled_tools = Vec::with_capacity(5);
    for (tool, runtime) in availability {
        if matches!(runtime, ToolRuntime::Enabled) {
            enabled_tools.push(tool);
        }
    }

    let mut has_local_tools = agent.local_tools_available(&primary_agent);
    let mut tools_allowed = provider_supports_tools && (!enabled_tools.is_empty() || has_local_tools);
    let mut tool_definitions = agent
        .tool_definitions(provider_supports_tools, &enabled_tools, &primary_agent)
        .map(Arc::new);
    let mut messages = agent.resolved_messages(&session);
    let allow_streaming = supports_streaming && !tools_allowed;

    let mut plan = PlanProgress::new(tools_allowed);
    if plan.has_entries() {
        drop(agent.send_plan_update(&args.session_id, &plan).await);
        if plan.complete_analysis() {
            drop(agent.send_plan_update(&args.session_id, &plan).await);
        }
    }

    if allow_streaming {
        let request = LLMRequest {
            messages: Arc::new(messages.clone()),
            model: session_model.clone(),
            stream: true,
            tools: tool_definitions,
            tool_choice: agent.tool_choice(tools_allowed),
            reasoning_effort,
            ..Default::default()
        };

        let mut stream = provider
            .stream(request)
            .await
            .map_err(|err| SdkError::internal_error().data(err.to_string()))?;

        drop(agent.advance_plan_to_response(&args.session_id, &mut plan).await);

        while let Some(event) = stream.next().await {
            let event = event.map_err(|err| SdkError::internal_error().data(err.to_string()))?;

            if session.cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                stop_reason = acp::StopReason::Cancelled;
                break;
            }

            match event {
                LLMStreamEvent::Token { delta } => {
                    if !delta.is_empty() {
                        assistant_message.push_str(&delta);
                        let chunk = text_chunk(delta);
                        drop(
                            agent
                                .send_update(&args.session_id, acp::SessionUpdate::AgentMessageChunk(chunk))
                                .await,
                        );
                    }
                }
                LLMStreamEvent::Reasoning { delta } => {
                    if !delta.is_empty() {
                        let chunk = text_chunk(delta);
                        drop(
                            agent
                                .send_update(&args.session_id, acp::SessionUpdate::AgentThoughtChunk(chunk))
                                .await,
                        );
                    }
                }
                LLMStreamEvent::ReasoningStage { .. } => {}
                LLMStreamEvent::ReasoningSignature { .. } => {}
                LLMStreamEvent::Completed { response } => {
                    if assistant_message.is_empty()
                        && let Some(content) = response.content
                    {
                        if !content.is_empty() {
                            let chunk = text_chunk(content.clone());
                            drop(
                                agent
                                    .send_update(&args.session_id, acp::SessionUpdate::AgentMessageChunk(chunk))
                                    .await,
                            );
                        }
                        assistant_message.push_str(&content);
                    }

                    if let Some(reasoning) = response.reasoning.filter(|reasoning| !reasoning.is_empty()) {
                        let chunk = text_chunk(reasoning);
                        drop(
                            agent
                                .send_update(&args.session_id, acp::SessionUpdate::AgentThoughtChunk(chunk))
                                .await,
                        );
                    }

                    stop_reason = ZedAgent::stop_reason_from_finish(response.finish_reason);
                    break;
                }
            }
        }
    } else {
        let mut tool_loop_count = 0usize;
        loop {
            if session.cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                stop_reason = acp::StopReason::Cancelled;
                break;
            }

            let request = LLMRequest {
                messages: Arc::new(messages.clone()),
                model: session_model.clone(),
                tools: tool_definitions.clone(),
                tool_choice: agent.tool_choice(tools_allowed),
                reasoning_effort,
                ..Default::default()
            };

            let response = provider
                .generate(request)
                .await
                .map_err(|err| SdkError::internal_error().data(err.to_string()))?;

            if session.cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                stop_reason = acp::StopReason::Cancelled;
                break;
            }

            if tools_allowed && let Some(tool_calls) = response.tool_calls.clone().filter(|calls| !calls.is_empty()) {
                if agent.tool_loop_limit_reached(tool_loop_count) {
                    let message = agent.tool_loop_limit_message();
                    drop(agent.advance_plan_to_response(&args.session_id, &mut plan).await);
                    drop(
                        agent
                            .send_update(
                                &args.session_id,
                                acp::SessionUpdate::AgentMessageChunk(text_chunk(message.clone())),
                            )
                            .await,
                    );
                    assistant_message = message;
                    stop_reason = acp::StopReason::EndTurn;
                    break;
                }
                tool_loop_count = tool_loop_count.saturating_add(1);
                if plan.start_context() {
                    drop(agent.send_plan_update(&args.session_id, &plan).await);
                }
                agent.push_message(
                    &session,
                    Message::assistant_with_tools(response.content.clone().unwrap_or_default(), tool_calls.clone()),
                );
                let tool_results = match agent.execute_tool_calls(&session, &args.session_id, &tool_calls).await {
                    Ok(results) => results,
                    Err(error) => {
                        warn!(%error, "Tool execution failed");
                        break;
                    }
                };
                if plan.complete_context() {
                    drop(agent.send_plan_update(&args.session_id, &plan).await);
                }
                for result in tool_results {
                    agent.push_message(&session, Message::tool_response(result.tool_call_id, result.llm_response));
                }
                if session.cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                    stop_reason = acp::StopReason::Cancelled;
                    break;
                }
                messages = agent.resolved_messages(&session);
                primary_agent = {
                    let data = session.data.lock().map_err(|_err| SdkError::internal_error())?;
                    data.primary_agent.clone()
                };
                has_local_tools = agent.local_tools_available(&primary_agent);
                tools_allowed = provider_supports_tools && (!enabled_tools.is_empty() || has_local_tools);
                tool_definitions = agent
                    .tool_definitions(provider_supports_tools, &enabled_tools, &primary_agent)
                    .map(Arc::new);
                continue;
            }

            if let Some(content) = &response.content {
                if !content.is_empty() {
                    drop(agent.advance_plan_to_response(&args.session_id, &mut plan).await);
                    if session.cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                        stop_reason = acp::StopReason::Cancelled;
                        break;
                    }
                    let chunk = text_chunk(content.clone());
                    drop(
                        agent
                            .send_update(&args.session_id, acp::SessionUpdate::AgentMessageChunk(chunk))
                            .await,
                    );
                }
                assistant_message = content.clone();
            }

            if let Some(reasoning) = response.reasoning.filter(|reasoning| !reasoning.is_empty()) {
                if session.cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                    stop_reason = acp::StopReason::Cancelled;
                    break;
                }
                let chunk = text_chunk(reasoning);
                drop(
                    agent
                        .send_update(&args.session_id, acp::SessionUpdate::AgentThoughtChunk(chunk))
                        .await,
                );
            }

            stop_reason = ZedAgent::stop_reason_from_finish(response.finish_reason);
            break;
        }
    }

    if stop_reason != acp::StopReason::Cancelled && !assistant_message.is_empty() {
        agent.push_message(&session, Message::assistant(assistant_message));
    }

    if stop_reason != acp::StopReason::Cancelled {
        if plan.complete_context() {
            drop(agent.send_plan_update(&args.session_id, &plan).await);
        }
        if plan.complete_response() {
            drop(agent.send_plan_update(&args.session_id, &plan).await);
        }
    }

    Ok(PromptResponse::new(stop_reason))
}

fn resolve_api_key_for_provider(agent: &ZedAgent, provider: &str) -> String {
    if provider.eq_ignore_ascii_case(&agent.config.provider) && !agent.config.api_key.is_empty() {
        return agent.config.api_key.clone();
    }

    get_api_key_with_mode(provider, &ApiKeySources::default(), agent.credential_storage_mode).unwrap_or_default()
}
