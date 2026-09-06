#![allow(
    clippy::collapsible_if,
    clippy::manual_contains,
    clippy::nonminimal_bool,
    clippy::single_match,
    unused_imports,
    reason = "The OpenAI provider keeps compatibility imports and provider-specific branches for feature-gated request paths."
)]

use crate::error_display;
use crate::provider;
use crate::provider::LLMProvider;
use hashbrown::{HashMap, HashSet};
use reqwest::Client as HttpClient;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
#[cfg(debug_assertions)]
use std::time::Instant;
use tokio::sync::Mutex as AsyncMutex;
use tracing::debug;
use uuid::Uuid;
use vtcode_commons::file_input::{MAX_INPUT_FILE_BYTES, decoded_base64_size};
use vtcode_commons::model_family::find_family_for_model;
use vtcode_config::TimeoutsConfig;
use vtcode_config::auth::{OpenAIChatGptAuthHandle, OpenAIChatGptSession};
use vtcode_config::constants::models;
use vtcode_config::core::{
    AnthropicConfig, ModelConfig, OpenAIConfig, OpenAIHostedShellConfig, OpenAIPromptCacheSettings, OpenAIServiceTier,
    PromptCachingConfig,
};

// Import from extracted modules
use super::CustomProviderAuthHandle;
use super::backend_setup::{OpenAIBackendSetup, OpenAIRequestAuth};
use super::harmony;
use super::request_builder;
use super::response_parser;
use super::responses_api::parse_responses_payload;
use super::types::{MAX_COMPLETION_TOKENS_FIELD, OpenAIResponsesPayload, ResponsesApiState};

mod generation;
mod streaming;
mod websocket;

use self::websocket::{OpenAIResponsesWebSocketContinuationCache, OpenAIResponsesWebSocketSession};
use super::super::{
    common::{extract_prompt_cache_settings, parse_client_prompt_common, resolve_model},
    extract_reasoning_trace,
};
use crate::system_prompt::default_system_prompt;
use vtcode_config::core::CustomProviderApiFormat;

const INLINE_FILE_LIMIT_ERROR_PREFIX: &str = "Inline OpenAI input_file payload exceeds the 50 MB request limit";

pub struct OpenAIProvider {
    api_key: Arc<str>,
    /// Override provider key for custom providers (e.g., "mycorp").
    /// When `None`, defaults to `"openai"`.
    provider_key_override: Option<Arc<str>>,
    /// Override display name for custom providers.
    /// When `None`, defaults to `"OpenAI"`.
    provider_display_override: Option<Arc<str>>,
    custom_provider_auth: Option<CustomProviderAuthHandle>,
    api_format_override: Option<CustomProviderApiFormat>,
    openai_chatgpt_auth: Option<OpenAIChatGptAuthHandle>,
    http_client: HttpClient,
    base_url: Arc<str>,
    responses_url: Arc<str>,
    responses_compact_url: Arc<str>,
    chat_completions_url: Arc<str>,
    backend_setup: OpenAIBackendSetup,
    model: Arc<str>,
    supported_models_override: Option<Vec<String>>,
    context_window_override: Option<usize>,
    responses_api_modes: Mutex<HashMap<String, ResponsesApiState>>,
    prompt_cache_enabled: bool,
    prompt_cache_settings: OpenAIPromptCacheSettings,
    model_behavior: Option<ModelConfig>,
    websocket_mode: bool,
    responses_store: Option<bool>,
    responses_include: Vec<String>,
    service_tier: Option<OpenAIServiceTier>,
    hosted_shell: OpenAIHostedShellConfig,
    websocket_session: AsyncMutex<Option<OpenAIResponsesWebSocketSession>>,
    websocket_continuation_cache: Mutex<Option<OpenAIResponsesWebSocketContinuationCache>>,
    /// Cache of models where `service_tier=flex` was rejected by the backend.
    /// Once a model is marked unsupported, subsequent requests skip the flex
    /// tier entirely, avoiding the wasted first request + retry round-trip.
    service_tier_unsupported_cache: Mutex<HashMap<String, bool>>,
}

impl OpenAIProvider {
    fn requires_streaming_responses(model: &str) -> bool {
        matches!(model, models::openai::GPT | models::openai::GPT_5_6_SOL)
    }

    fn model_supports_reasoning_summaries(model: &str) -> bool {
        find_family_for_model(model).supports_reasoning_summaries
    }

    fn normalize_reasoning_output(model: &str, mut response: provider::LLMResponse) -> provider::LLMResponse {
        if !Self::model_supports_reasoning_summaries(model) {
            response.reasoning = None;
            response.reasoning_details = None;
        }

        response
    }

    fn is_responses_api_model(model: &str) -> bool {
        models::openai::RESPONSES_API_MODELS.contains(&model)
    }

    fn uses_harmony(model: &str) -> bool {
        harmony::uses_harmony(model)
    }

    fn requires_responses_api(model: &str) -> bool {
        model == models::openai::GPT_5
    }

    fn default_responses_state(model: &str) -> ResponsesApiState {
        if Self::requires_responses_api(model) {
            ResponsesApiState::Required
        } else if Self::is_responses_api_model(model) {
            ResponsesApiState::Allowed
        } else {
            ResponsesApiState::Disabled
        }
    }

    pub fn new(api_key: String) -> Self {
        Self::with_model_internal(
            api_key,
            None,
            models::openai::DEFAULT_MODEL.to_string(),
            None,
            None,
            TimeoutsConfig::default(),
            None,
            None,
        )
    }

    fn with_model(api_key: String, model: String) -> Self {
        Self::with_model_internal(api_key, None, model, None, None, TimeoutsConfig::default(), None, None)
    }

    pub(crate) fn new_with_client(
        api_key: String,
        openai_chatgpt_auth: Option<OpenAIChatGptAuthHandle>,
        model: String,
        http_client: reqwest::Client,
        base_url: String,
        _timeouts: TimeoutsConfig,
    ) -> Self {
        use hashbrown::HashMap;
        use std::sync::Arc;
        use std::sync::Mutex;

        let backend_setup = if openai_chatgpt_auth.is_some() {
            OpenAIBackendSetup::chatgpt_subscription_rig(base_url.clone())
        } else {
            OpenAIBackendSetup::api_key(base_url.clone())
        };

        Self {
            api_key: Arc::from(api_key.as_str()),
            provider_key_override: None,
            provider_display_override: None,
            custom_provider_auth: None,
            api_format_override: None,
            openai_chatgpt_auth,
            http_client,
            base_url: Arc::from(base_url.as_str()),
            responses_url: Arc::from(format!("{base_url}/responses")),
            responses_compact_url: Arc::from(format!("{base_url}/responses/compact")),
            chat_completions_url: Arc::from(format!("{base_url}/chat/completions")),
            backend_setup,
            model: Arc::from(model.as_str()),
            supported_models_override: None,
            context_window_override: None,
            prompt_cache_enabled: false,
            prompt_cache_settings: Default::default(),
            responses_api_modes: Mutex::new(HashMap::new()),
            model_behavior: None,
            websocket_mode: false,
            responses_store: None,
            responses_include: Vec::new(),
            service_tier: None,
            hosted_shell: OpenAIHostedShellConfig::default(),
            websocket_session: AsyncMutex::new(None),
            websocket_continuation_cache: Mutex::new(None),
            service_tier_unsupported_cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn from_config(
        api_key: Option<String>,
        openai_chatgpt_auth: Option<OpenAIChatGptAuthHandle>,
        model: Option<String>,
        base_url: Option<String>,
        prompt_cache: Option<PromptCachingConfig>,
        timeouts: Option<TimeoutsConfig>,
        _anthropic: Option<AnthropicConfig>,
        openai: Option<OpenAIConfig>,
        model_behavior: Option<ModelConfig>,
    ) -> Self {
        let api_key_value = api_key.unwrap_or_default();
        let model_value = resolve_model(model, models::openai::DEFAULT_MODEL);

        Self::with_model_internal(
            api_key_value,
            openai_chatgpt_auth,
            model_value,
            prompt_cache,
            base_url,
            timeouts.unwrap_or_default(),
            openai,
            model_behavior,
        )
    }

    /// Create a custom OpenAI-compatible provider with overridden identity.
    #[expect(
        clippy::too_many_arguments,
        reason = "Intentional compatibility, platform, test, or API-shape suppression."
    )]
    pub fn from_custom_config(
        provider_key: String,
        display_name: String,
        api_key: Option<String>,
        model: Option<String>,
        base_url: Option<String>,
        prompt_cache: Option<PromptCachingConfig>,
        timeouts: Option<TimeoutsConfig>,
        openai: Option<OpenAIConfig>,
        model_behavior: Option<ModelConfig>,
        custom_provider_auth: Option<CustomProviderAuthHandle>,
        supported_models_override: Option<Vec<String>>,
    ) -> Self {
        let mut provider = Self::from_config(
            api_key,
            None, // no chatgpt auth for custom providers
            model,
            base_url,
            prompt_cache,
            timeouts,
            None, // no anthropic config
            openai,
            model_behavior,
        );
        provider.provider_key_override = Some(Arc::from(provider_key.as_str()));
        provider.provider_display_override = Some(Arc::from(display_name.as_str()));
        provider.custom_provider_auth = custom_provider_auth;
        provider.api_format_override = None;
        if provider.custom_provider_auth.is_some() {
            provider.backend_setup = provider.backend_setup.clone().with_custom_command_auth();
        }
        provider.supported_models_override = supported_models_override;
        provider
    }

    pub(crate) fn with_api_format_override(mut self, api_format: Option<CustomProviderApiFormat>) -> Self {
        self.api_format_override = api_format;
        self
    }

    /// Set an optional context window override for a custom provider.
    pub fn with_context_window(mut self, context_window: Option<usize>) -> Self {
        self.context_window_override = context_window;
        self
    }

    fn with_model_internal(
        api_key: String,
        openai_chatgpt_auth: Option<OpenAIChatGptAuthHandle>,
        model: String,
        prompt_cache: Option<PromptCachingConfig>,
        base_url: Option<String>,
        timeouts: TimeoutsConfig,
        openai: Option<OpenAIConfig>,
        model_behavior: Option<ModelConfig>,
    ) -> Self {
        let (prompt_cache_enabled, prompt_cache_settings) = extract_prompt_cache_settings(
            prompt_cache,
            |providers| &providers.openai,
            |cfg, provider_settings| cfg.enabled && provider_settings.enabled,
        );

        let backend_setup = if openai_chatgpt_auth.is_some() {
            OpenAIBackendSetup::from_chatgpt_subscription_config(base_url)
        } else {
            OpenAIBackendSetup::from_api_key_config(base_url)
        };
        let resolved_base_url = backend_setup.base_url().to_string();

        let mut responses_api_modes = HashMap::new();
        let default_state = Self::default_responses_state(&model);
        let is_chatgpt_backend = backend_setup.is_chatgpt_codex_backend();
        let is_xai = resolved_base_url.contains("api.x.ai");
        let websocket_mode = openai.as_ref().map(|cfg| cfg.websocket_mode).unwrap_or(false);
        let responses_store = openai.as_ref().and_then(|cfg| cfg.responses_store);
        let responses_include = openai
            .as_ref()
            .map(|cfg| {
                cfg.responses_include
                    .iter()
                    .map(|value| value.trim())
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let service_tier = openai.as_ref().and_then(|cfg| cfg.service_tier);
        let hosted_shell = openai.as_ref().map(|cfg| cfg.hosted_shell.clone()).unwrap_or_default();

        let initial_state = if is_xai {
            ResponsesApiState::Disabled
        } else if is_chatgpt_backend {
            match default_state {
                ResponsesApiState::Disabled => ResponsesApiState::Allowed,
                state => state,
            }
        } else {
            default_state
        };
        responses_api_modes.insert(model.clone(), initial_state);

        use crate::http_client::HttpClientFactory;
        let http_client = HttpClientFactory::for_llm(&timeouts);

        Self {
            api_key: Arc::from(api_key.as_str()),
            provider_key_override: None,
            provider_display_override: None,
            custom_provider_auth: None,
            api_format_override: None,
            openai_chatgpt_auth,
            http_client,
            base_url: Arc::from(resolved_base_url.as_str()),
            responses_url: Arc::from(format!("{resolved_base_url}/responses")),
            responses_compact_url: Arc::from(format!("{resolved_base_url}/responses/compact")),
            chat_completions_url: Arc::from(format!("{resolved_base_url}/chat/completions")),
            backend_setup,
            model: Arc::from(model.as_str()),
            supported_models_override: None,
            context_window_override: None,
            responses_api_modes: Mutex::new(responses_api_modes),
            prompt_cache_enabled,
            prompt_cache_settings,
            model_behavior,
            websocket_mode,
            responses_store,
            responses_include,
            service_tier,
            hosted_shell,
            websocket_session: AsyncMutex::new(None),
            websocket_continuation_cache: Mutex::new(None),
            service_tier_unsupported_cache: Mutex::new(HashMap::new()),
        }
    }

    fn is_native_openai_api(&self) -> bool {
        self.provider_key_override.is_none() && self.backend_setup.is_native_openai_api()
    }

    fn supports_manual_openai_compaction_for_model(&self, model: &str) -> bool {
        self.is_native_openai_api()
            && !self.uses_chatgpt_auth()
            && !matches!(self.responses_api_state(model), ResponsesApiState::Disabled)
    }

    fn manual_openai_compaction_unavailable_message_for_model(&self, model: &str) -> String {
        let requested = if model.trim().is_empty() {
            self.model.as_ref()
        } else {
            model
        };

        let (backend, reason) = if self.uses_chatgpt_auth() {
            (
                "ChatGPT subscription auth via chatgpt.com backend".to_string(),
                "ChatGPT subscription auth does not expose the standalone `/responses/compact` endpoint".to_string(),
            )
        } else if self.provider_key_override.is_some() {
            (
                format!("custom OpenAI-compatible provider endpoint ({})", self.base_url),
                "custom OpenAI-compatible provider endpoints do not expose the standalone `/responses/compact` endpoint"
                    .to_string(),
            )
        } else if !self.base_url.contains("api.openai.com") {
            (
                format!("configured OpenAI-compatible endpoint ({})", self.base_url),
                "the standalone `/responses/compact` endpoint is only served by the native OpenAI API host".to_string(),
            )
        } else {
            (
                "native OpenAI API (api.openai.com)".to_string(),
                "this model is not Responses-compatible on the native OpenAI API".to_string(),
            )
        };

        format!(
            "`--native-only` `/compact` requires a native server-side compaction endpoint, which is unavailable for this configuration. Active provider/backend/model: {} / {} / {}. Reason: {}. Run `/compact` without `--native-only` to compact via the local summarization fallback.",
            self.name(),
            backend,
            requested,
            reason,
        )
    }

    fn websocket_mode_enabled(&self, model: &str) -> bool {
        self.websocket_mode
            && self.backend_setup.transport().websocket
            && !matches!(self.responses_api_state(model), ResponsesApiState::Disabled)
    }

    fn hosted_shell_for_model(&self, model: &str) -> Option<&OpenAIHostedShellConfig> {
        (self.is_native_openai_api()
            && !matches!(self.responses_api_state(model), ResponsesApiState::Disabled)
            && self.hosted_shell.enabled
            && self.hosted_shell.is_valid_for_runtime())
        .then_some(&self.hosted_shell)
    }

    fn supports_responses_allowed_tools(&self, model: &str) -> bool {
        self.supports_tools(model)
            && Self::is_responses_api_model(model)
            && !matches!(self.responses_api_state(model), ResponsesApiState::Disabled)
            && (self.is_native_openai_api() || self.is_chatgpt_backend())
    }

    fn authorize_with_api_key(
        &self,
        builder: reqwest::RequestBuilder,
        auth: &OpenAIRequestAuth,
    ) -> reqwest::RequestBuilder {
        self.backend_setup.authorize_request(builder, auth)
    }

    fn uses_chatgpt_auth(&self) -> bool {
        self.backend_setup.uses_chatgpt_subscription_auth()
    }

    fn uses_refreshable_auth(&self) -> bool {
        self.backend_setup.uses_refreshable_auth() || self.custom_provider_auth.is_some()
    }

    fn is_chatgpt_backend(&self) -> bool {
        self.backend_setup.is_chatgpt_codex_backend()
    }

    fn allows_chat_completions_fallback(&self) -> bool {
        self.backend_setup.transport().chat_completions_fallback
    }

    fn auth_retryable_status(status: StatusCode) -> bool {
        matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
    }

    fn new_client_request_id() -> String {
        format!("vtcode-{}", Uuid::new_v4())
    }

    fn format_network_error(&self, error: impl std::fmt::Display) -> provider::LLMError {
        let label = self.provider_display_override.as_deref().unwrap_or("OpenAI");
        provider::LLMError::Network {
            message: error_display::format_llm_error(label, &format!("Network error: {error}")),
            metadata: None,
        }
    }

    fn format_auth_error(&self, error: impl std::fmt::Display) -> provider::LLMError {
        let label = self.provider_display_override.as_deref().unwrap_or("OpenAI");
        provider::LLMError::Authentication {
            message: error_display::format_llm_error(label, &format!("Authentication error: {error}")),
            metadata: None,
        }
    }

    async fn current_api_key(&self) -> Result<String, provider::LLMError> {
        if let Some(handle) = &self.custom_provider_auth {
            return handle.current_token().await.map_err(|e| self.format_auth_error(e));
        }

        let Some(handle) = &self.openai_chatgpt_auth else {
            return Ok(self.api_key.to_string());
        };

        handle.refresh_if_needed().await.map_err(|e| self.format_auth_error(e))?;
        handle.current_api_key().map_err(|e| self.format_auth_error(e))
    }

    fn request_auth_from_session(&self, session: OpenAIChatGptSession) -> OpenAIRequestAuth {
        self.backend_setup.request_auth_from_session(session)
    }

    async fn current_request_auth(&self) -> Result<OpenAIRequestAuth, provider::LLMError> {
        if let Some(handle) = &self.custom_provider_auth {
            return Ok(OpenAIRequestAuth::bearer_token(
                handle.current_token().await.map_err(|e| self.format_auth_error(e))?,
            ));
        }

        let Some(handle) = &self.openai_chatgpt_auth else {
            return Ok(OpenAIRequestAuth::bearer_token(self.api_key.to_string()));
        };

        handle.refresh_if_needed().await.map_err(|e| self.format_auth_error(e))?;
        let session = handle.snapshot().map_err(|e| self.format_auth_error(e))?;
        Ok(self.request_auth_from_session(session))
    }

    async fn refresh_request_auth_for_retry(&self) -> Result<OpenAIRequestAuth, provider::LLMError> {
        if let Some(handle) = &self.custom_provider_auth {
            return Ok(OpenAIRequestAuth::bearer_token(
                handle.force_refresh().await.map_err(|e| self.format_auth_error(e))?,
            ));
        }

        let Some(handle) = &self.openai_chatgpt_auth else {
            return Ok(OpenAIRequestAuth::bearer_token(self.api_key.to_string()));
        };

        handle.force_refresh().await.map_err(|e| self.format_auth_error(e))?;
        let session = handle.snapshot().map_err(|e| self.format_auth_error(e))?;
        Ok(self.request_auth_from_session(session))
    }

    async fn refresh_api_key_for_retry(&self) -> Result<String, provider::LLMError> {
        if let Some(handle) = &self.custom_provider_auth {
            return handle.force_refresh().await.map_err(|e| self.format_auth_error(e));
        }

        let Some(handle) = &self.openai_chatgpt_auth else {
            return Ok(self.api_key.to_string());
        };

        handle.force_refresh().await.map_err(|e| self.format_auth_error(e))?;
        handle.current_api_key().map_err(|e| self.format_auth_error(e))
    }

    async fn send_authorized<F>(&self, build_request: F) -> Result<reqwest::Response, provider::LLMError>
    where
        F: Fn(&OpenAIRequestAuth) -> reqwest::RequestBuilder,
    {
        let auth = self.current_request_auth().await?;
        let response = build_request(&auth).send().await.map_err(|e| self.format_network_error(e))?;

        if self.uses_refreshable_auth() && Self::auth_retryable_status(response.status()) {
            let retry_auth = self.refresh_request_auth_for_retry().await?;
            return build_request(&retry_auth)
                .send()
                .await
                .map_err(|e| self.format_network_error(e));
        }

        Ok(response)
    }

    fn supports_temperature_parameter(model: &str) -> bool {
        vtcode_config::models::model_catalog_entry("openai", model)
            .map(|entry| entry.supports_sampling)
            .unwrap_or_else(|| {
                !matches!(model, models::openai::GPT_5 | models::openai::GPT_5_MINI | models::openai::GPT_5_NANO)
            })
    }

    fn responses_api_state(&self, model: &str) -> ResponsesApiState {
        if let Some(api_format) = self.api_format_override {
            return match api_format {
                CustomProviderApiFormat::Auto => Self::default_responses_state(model),
                CustomProviderApiFormat::OpenAIChat => ResponsesApiState::Disabled,
                CustomProviderApiFormat::OpenAIResponses => ResponsesApiState::Required,
                CustomProviderApiFormat::AnthropicMessages => ResponsesApiState::Disabled,
            };
        }

        let mut modes = match self.responses_api_modes.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                tracing::warn!("OpenAI responses_api_modes mutex poisoned, recovering");
                poisoned.into_inner()
            }
        };
        *modes
            .entry(model.to_string())
            .or_insert_with(|| Self::default_responses_state(model))
    }

    fn set_responses_api_state(&self, model: &str, state: ResponsesApiState) {
        let mut modes = match self.responses_api_modes.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                tracing::warn!("OpenAI responses_api_modes mutex poisoned, recovering");
                poisoned.into_inner()
            }
        };
        modes.insert(model.to_string(), state);
    }

    fn validate_inline_file_inputs(request: &provider::LLMRequest) -> Result<(), provider::LLMError> {
        Self::validate_inline_file_inputs_with_limit(request, MAX_INPUT_FILE_BYTES)
    }

    fn validate_inline_file_inputs_with_limit(
        request: &provider::LLMRequest,
        max_inline_file_bytes: u64,
    ) -> Result<(), provider::LLMError> {
        let mut total_inline_file_bytes = 0u64;

        for message in request.messages.iter() {
            let provider::MessageContent::Parts(parts) = &message.content else {
                continue;
            };

            for part in parts {
                let provider::ContentPart::File { filename, file_data, .. } = part else {
                    continue;
                };
                let Some(file_data) = file_data else {
                    continue;
                };

                let inline_file_bytes = decoded_base64_size(file_data).map_err(|error| {
                    let formatted = error_display::format_llm_error(
                        "OpenAI",
                        &format!("Invalid inline input_file payload: {error}"),
                    );
                    provider::LLMError::InvalidRequest { message: formatted, metadata: None }
                })?;

                if inline_file_bytes > max_inline_file_bytes {
                    let file_label = filename.as_deref().unwrap_or("attached file");
                    let formatted = error_display::format_llm_error(
                        "OpenAI",
                        &format!("{INLINE_FILE_LIMIT_ERROR_PREFIX}: '{file_label}' is {inline_file_bytes} bytes"),
                    );
                    return Err(provider::LLMError::InvalidRequest { message: formatted, metadata: None });
                }

                total_inline_file_bytes = total_inline_file_bytes.checked_add(inline_file_bytes).ok_or_else(|| {
                    provider::LLMError::InvalidRequest {
                        message: error_display::format_llm_error("OpenAI", INLINE_FILE_LIMIT_ERROR_PREFIX),
                        metadata: None,
                    }
                })?;
            }
        }

        if total_inline_file_bytes > max_inline_file_bytes {
            let formatted = error_display::format_llm_error(
                "OpenAI",
                &format!("{INLINE_FILE_LIMIT_ERROR_PREFIX}: total inline file bytes = {total_inline_file_bytes}"),
            );
            return Err(provider::LLMError::InvalidRequest { message: formatted, metadata: None });
        }

        Ok(())
    }

    fn convert_to_openai_format(&self, request: &provider::LLMRequest) -> Result<Value, provider::LLMError> {
        let is_native_openai = self.is_native_openai_api();
        let prompt_cache_key = if is_native_openai {
            request.prompt_cache_key.as_deref()
        } else {
            None
        };
        let default_service_tier = if is_native_openai {
            self.service_tier.map(OpenAIServiceTier::as_str)
        } else {
            None
        };
        let ctx = request_builder::ChatRequestContext {
            model: &self.model,
            is_native_openai,
            supports_tools: self.supports_tools(&request.model),
            supports_parallel_tool_config: self.supports_parallel_tool_config(&request.model),
            supports_temperature: Self::supports_temperature_parameter(&request.model),
            prompt_cache_key,
            default_service_tier,
        };

        request_builder::build_chat_request(request, &ctx)
    }

    fn convert_to_openai_responses_format(&self, request: &provider::LLMRequest) -> Result<Value, provider::LLMError> {
        Self::validate_inline_file_inputs(request)?;

        let is_native_openai = self.is_native_openai_api();
        let prompt_cache_key = if is_native_openai || self.is_chatgpt_backend() {
            request.prompt_cache_key.as_deref()
        } else {
            None
        };
        let default_service_tier = if is_native_openai {
            self.service_tier.map(OpenAIServiceTier::as_str)
        } else {
            None
        };
        let backend_defaults = self.backend_setup.responses_defaults();
        let ctx = request_builder::ResponsesRequestContext {
            supports_tools: self.supports_tools(&request.model),
            supports_allowed_tools: self.supports_responses_allowed_tools(&request.model),
            supports_parallel_tool_config: self.supports_parallel_tool_config(&request.model),
            supports_temperature: Self::supports_temperature_parameter(&request.model),
            supports_reasoning_effort: self.supports_reasoning_effort(&request.model),
            supported_reasoning_efforts: self.supported_reasoning_efforts(&request.model),
            supports_reasoning: self.supports_reasoning(&request.model),
            is_responses_api_model: Self::is_responses_api_model(&request.model),
            include_max_output_tokens: is_native_openai,
            include_output_types: backend_defaults.include_output_types,
            include_sampling_parameters: backend_defaults.include_sampling_parameters,
            force_response_store_false: true,
            include_assistant_phase: is_native_openai,
            prompt_cache_key,
            include_prompt_cache_retention: backend_defaults.include_prompt_cache_retention,
            prompt_cache_retention: self.prompt_cache_settings.prompt_cache_retention.as_ref().map(|r| r.as_str()),
            default_service_tier,
            default_response_store: self.responses_store,
            default_responses_include: (!self.responses_include.is_empty())
                .then_some(self.responses_include.as_slice()),
            include_encrypted_reasoning: backend_defaults.include_encrypted_reasoning,
            hosted_shell: self.hosted_shell_for_model(&request.model),
            include_structured_history_in_input: backend_defaults.include_structured_history_in_input,
            preserve_structured_history_on_replay: backend_defaults.preserve_structured_history_on_replay,
            preserve_assistant_phase_on_replay: false,
            reasoning_context: None,
            safety_identifier: None,
        };

        request_builder::build_responses_request(request, &ctx)
    }

    fn parse_openai_response(
        &self,
        response_json: Value,
        model: String,
    ) -> Result<provider::LLMResponse, provider::LLMError> {
        let include_cached_prompt_tokens = self.prompt_cache_enabled && self.prompt_cache_settings.surface_metrics;
        let response =
            response_parser::parse_chat_response(response_json, model.clone(), include_cached_prompt_tokens)?;
        Ok(Self::normalize_reasoning_output(&model, response))
    }

    fn parse_openai_responses_response(
        &self,
        response_json: Value,
        model: String,
    ) -> Result<provider::LLMResponse, provider::LLMError> {
        let include_metrics = self.prompt_cache_enabled && self.prompt_cache_settings.surface_metrics;
        let response = parse_responses_payload(response_json, model.clone(), include_metrics)?;
        Ok(Self::normalize_reasoning_output(&model, response))
    }
}

#[cfg(test)]
mod tests;

mod harmony_client;
mod provider_impl;
