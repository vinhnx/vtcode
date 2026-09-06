use super::ZedAgent;
use crate::acp;
use std::str::FromStr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use tracing::warn;
use vtcode_config::auth::AuthCredentialsStoreMode;
use vtcode_core::config::models::{ModelId, Provider};
use vtcode_core::config::types::ReasoningEffortLevel;
use vtcode_core::core::threads::{ThreadBootstrap, build_thread_archive_metadata};
use vtcode_core::llm::ModelResolver;
use vtcode_core::llm::factory::get_factory;
use vtcode_core::llm::provider::{FinishReason, Message};
use vtcode_core::utils::session_archive::find_session_by_identifier;

use super::super::constants::SESSION_PREFIX;
use super::super::helpers::session_config_options;
use super::super::types::{SessionData, SessionHandle};

impl ZedAgent {
    fn session_reasoning_effort_for_thread(
        &self,
        thread: &vtcode_core::core::threads::ThreadRuntimeHandle,
    ) -> ReasoningEffortLevel {
        thread
            .metadata()
            .and_then(|metadata| ReasoningEffortLevel::parse(&metadata.reasoning_effort))
            .unwrap_or(self.config.reasoning_effort)
    }

    fn sync_thread_reasoning_effort(
        &self,
        thread: &vtcode_core::core::threads::ThreadRuntimeHandle,
        reasoning_effort: ReasoningEffortLevel,
    ) {
        if let Some(mut metadata) = thread.metadata() {
            metadata.reasoning_effort = reasoning_effort.as_str().to_string();
            thread.replace_metadata(Some(metadata));
        }
    }

    fn session_provider_for_thread(&self, thread: &vtcode_core::core::threads::ThreadRuntimeHandle) -> String {
        thread
            .metadata()
            .map(|metadata| metadata.provider)
            .unwrap_or_else(|| self.config.provider.clone())
    }

    fn session_model_for_thread(&self, thread: &vtcode_core::core::threads::ThreadRuntimeHandle) -> String {
        thread
            .metadata()
            .map(|metadata| metadata.model)
            .unwrap_or_else(|| self.config.model.clone())
    }

    fn session_primary_agent_for_thread(&self, thread: &vtcode_core::core::threads::ThreadRuntimeHandle) -> String {
        thread
            .metadata()
            .and_then(|metadata| metadata.primary_agent)
            .and_then(|primary_agent| self.primary_agents.resolve_id(&primary_agent))
            .map(ToString::to_string)
            .unwrap_or_else(|| self.primary_agents.default_id().to_string())
    }

    fn sync_thread_primary_agent(&self, thread: &vtcode_core::core::threads::ThreadRuntimeHandle, primary_agent: &str) {
        if let Some(mut metadata) = thread.metadata() {
            metadata.primary_agent = Some(primary_agent.to_string());
            thread.replace_metadata(Some(metadata));
        }
    }

    fn sync_thread_provider_and_model(
        &self,
        thread: &vtcode_core::core::threads::ThreadRuntimeHandle,
        provider: &str,
        model: &str,
    ) {
        if let Some(mut metadata) = thread.metadata() {
            metadata.provider = provider.to_string();
            metadata.model = model.to_string();
            thread.replace_metadata(Some(metadata));
        }
    }

    fn model_supports_thought_level(&self, provider: &str, model: &str) -> bool {
        ModelResolver::resolve(Some(provider), model, &[], None)
            .map(|resolved| resolved.reasoning_supported())
            .unwrap_or(false)
    }

    fn build_session_handle(
        &self,
        session_id: acp::SessionId,
        thread: vtcode_core::core::threads::ThreadRuntimeHandle,
    ) -> SessionHandle {
        let reasoning_effort = self.session_reasoning_effort_for_thread(&thread);
        let provider = self.session_provider_for_thread(&thread);
        let model = self.session_model_for_thread(&thread);
        let primary_agent = self.session_primary_agent_for_thread(&thread);
        SessionHandle {
            data: Arc::new(Mutex::new(SessionData {
                _session_id: session_id,
                thread,
                tool_notice_sent: std::sync::atomic::AtomicBool::new(false),
                primary_agent,
                reasoning_effort,
                provider,
                model,
                last_tool_call_at: None,
            })),
            cancel_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    pub(crate) fn register_session(&self) -> acp::SessionId {
        let raw_id = self.next_session_id.fetch_add(1, Ordering::Relaxed);
        let session_id = acp::SessionId::new(Arc::from(format!("{SESSION_PREFIX}-{raw_id}")));
        let metadata = build_thread_archive_metadata(
            self.config.workspace.as_path(),
            &self.config.model,
            &self.config.provider,
            &self.config.theme,
            self.config.reasoning_effort.as_str(),
        );
        let thread = self
            .thread_manager
            .start_thread_with_identifier(session_id.0.to_string(), ThreadBootstrap::new(Some(metadata)));
        let handle = self.build_session_handle(session_id.clone(), thread);
        if let Ok(mut guard) = self.sessions.lock() {
            drop(guard.insert(session_id.clone(), handle));
        }
        session_id
    }

    pub(crate) fn session_handle(&self, session_id: &acp::SessionId) -> Option<SessionHandle> {
        self.sessions.lock().unwrap_or_else(|e| e.into_inner()).get(session_id).cloned()
    }

    pub(super) fn push_message(&self, session: &SessionHandle, message: Message) {
        if let Ok(data) = session.data.lock() {
            data.thread.append_message(message);
        }
    }

    #[allow(dead_code, reason = "Intentional compatibility, platform, or test-only suppression.")]
    pub(super) fn should_send_tool_notice(&self, session: &SessionHandle) -> bool {
        session
            .data
            .lock()
            .map(|data| !data.tool_notice_sent.load(Ordering::Relaxed))
            .unwrap_or(false)
    }

    #[allow(dead_code, reason = "Intentional compatibility, platform, or test-only suppression.")]
    pub(super) fn mark_tool_notice_sent(&self, session: &SessionHandle) {
        if let Ok(data) = session.data.lock() {
            data.tool_notice_sent.store(true, Ordering::Relaxed);
        }
    }

    pub(super) fn update_session_primary_agent(&self, session: &SessionHandle, primary_agent: String) -> bool {
        let Some(primary_agent) = self.primary_agents.resolve_id(&primary_agent) else {
            return false;
        };
        let mut data = match session.data.lock() {
            Ok(guard) => guard,
            Err(_) => return false,
        };
        if data.primary_agent.eq_ignore_ascii_case(primary_agent) {
            return false;
        }
        data.primary_agent = primary_agent.to_string();
        self.sync_thread_primary_agent(&data.thread, &data.primary_agent);
        true
    }

    fn update_session_reasoning_effort(&self, session: &SessionHandle, reasoning_effort: ReasoningEffortLevel) -> bool {
        let mut data = match session.data.lock() {
            Ok(guard) => guard,
            Err(_) => return false,
        };
        if data.reasoning_effort == reasoning_effort {
            return false;
        }
        data.reasoning_effort = reasoning_effort;
        self.sync_thread_reasoning_effort(&data.thread, reasoning_effort);
        true
    }

    fn update_session_provider_and_model(&self, session: &SessionHandle, provider: String, model: String) -> bool {
        let mut data = match session.data.lock() {
            Ok(guard) => guard,
            Err(_) => return false,
        };
        if data.provider == provider && data.model == model {
            return false;
        }
        data.provider = provider;
        data.model = model;
        self.sync_thread_provider_and_model(&data.thread, &data.provider, &data.model);
        true
    }

    fn provider_default_model(&self, provider: &str) -> Option<String> {
        Provider::from_str(provider)
            .ok()
            .map(|value| ModelId::default_single_for_provider(value).as_str().to_string())
    }

    fn provider_supports_model(&self, provider: &str, model: &str) -> bool {
        let Ok(provider) = Provider::from_str(provider) else {
            return true;
        };
        ModelId::models_for_provider(provider)
            .iter()
            .any(|entry| entry.as_str() == model)
    }

    fn provider_select_options(&self, current_provider: &str) -> Vec<acp::SessionConfigSelectOption> {
        let mut providers = get_factory()
            .lock()
            .ok()
            .map(|factory| factory.list_providers())
            .unwrap_or_default();
        if providers.is_empty() {
            tracing::warn!("LLM factory has no registered providers, falling back to Provider::all_providers()");
            providers = Provider::all_providers()
                .into_iter()
                .map(|provider| provider.to_string())
                .collect();
        }

        if !providers.iter().any(|provider| provider.eq_ignore_ascii_case(current_provider)) {
            providers.push(current_provider.to_string());
        }

        providers.sort();
        providers.dedup_by(|left, right| left.eq_ignore_ascii_case(right));

        tracing::debug!(
            provider_count = providers.len(),
            providers = ?providers,
            current_provider = current_provider,
            "Building provider select options for ACP"
        );

        providers
            .into_iter()
            .map(|provider| {
                let name = Provider::from_str(&provider)
                    .ok()
                    .map(|parsed| parsed.label().to_string())
                    .unwrap_or_else(|| provider.clone());
                acp::SessionConfigSelectOption::new(provider, name)
            })
            .collect()
    }

    fn model_select_options(&self, provider: &str, current_model: &str) -> Vec<acp::SessionConfigSelectOption> {
        let mut options = Provider::from_str(provider)
            .ok()
            .map(|provider| {
                ModelId::models_for_provider(provider)
                    .into_iter()
                    .map(|model| {
                        acp::SessionConfigSelectOption::new(
                            model.as_str().into_owned(),
                            model.display_name().into_owned(),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        if !options.iter().any(|option| option.value.0.as_ref() == current_model) {
            options.push(acp::SessionConfigSelectOption::new(current_model.to_string(), current_model.to_string()));
        }

        options.sort_by(|left, right| left.value.0.cmp(&right.value.0));
        options
    }

    fn supports_provider(&self, provider: &str, current_provider: &str) -> bool {
        self.provider_select_options(current_provider)
            .iter()
            .any(|option| option.value.0.as_ref() == provider)
    }

    fn current_session_config_options(&self, session: &SessionHandle) -> Vec<acp::SessionConfigOption> {
        let data = match session.data.lock() {
            Ok(guard) => guard,
            Err(_) => return Vec::new(),
        };
        let provider_options = self.provider_select_options(&data.provider);
        let model_options = self.model_select_options(&data.provider, &data.model);
        let config_options = session_config_options(
            &data.primary_agent,
            &self.primary_agents,
            data.reasoning_effort,
            self.model_supports_thought_level(&data.provider, &data.model),
            &data.provider,
            provider_options,
            &data.model,
            model_options,
        );

        tracing::debug!(
            config_option_count = config_options.len(),
            primary_agent = %data.primary_agent,
            current_provider = %data.provider,
            current_model = %data.model,
            "Built session config options for ACP"
        );

        config_options
    }

    pub(super) fn resolved_messages(&self, session: &SessionHandle) -> Vec<Message> {
        let mut messages = Vec::with_capacity(10);
        if !self.system_prompt.trim().is_empty() {
            messages.push(Message::system(self.system_prompt.clone()));
        }

        let Ok(history) = session.data.lock() else {
            return messages;
        };
        if let Some(prompt) = self.primary_agents.prompt(&history.primary_agent) {
            messages.push(Message::system(prompt.to_string()));
        }
        messages.extend(history.thread.messages());
        messages
    }

    async fn attach_thread_from_archive(
        &self,
        session_id: &acp::SessionId,
        identifier: &str,
    ) -> anyhow::Result<SessionHandle> {
        let listing = find_session_by_identifier(identifier)
            .await?
            .ok_or_else(|| anyhow::anyhow!("unknown archived session '{identifier}'"))?;
        let thread = self
            .thread_manager
            .start_thread_with_identifier(listing.identifier(), ThreadBootstrap::from_listing(listing));
        let handle = self.build_session_handle(session_id.clone(), thread);
        if let Ok(mut guard) = self.sessions.lock() {
            drop(guard.insert(session_id.clone(), handle.clone()));
        }
        Ok(handle)
    }

    pub(super) fn stop_reason_from_finish(finish: FinishReason) -> acp::StopReason {
        match finish {
            FinishReason::Stop | FinishReason::ToolCalls => acp::StopReason::EndTurn,
            FinishReason::Length => acp::StopReason::MaxTokens,
            FinishReason::ContentFilter | FinishReason::Refusal | FinishReason::Error(_) => acp::StopReason::Refusal,
            FinishReason::Pause => acp::StopReason::EndTurn,
        }
    }

    /// Programmatic equivalent of the SACP `session/new` handler — exposed
    /// for tests and for the SACP handler shim to call.
    pub(crate) async fn new_session(
        &self,
        _req: acp::NewSessionRequest,
    ) -> Result<acp::NewSessionResponse, acp::Error> {
        let session_id = self.register_session();
        let config_options = self
            .session_handle(&session_id)
            .map(|session| self.current_session_config_options(&session))
            .unwrap_or_default();

        if let Err(error) = self.send_available_commands_update(&session_id).await {
            warn!(%error, "Failed to advertise initial slash commands");
        }

        Ok(acp::NewSessionResponse::new(session_id).config_options(config_options))
    }

    /// Programmatic equivalent of the SACP `session/load` handler.
    pub(crate) async fn load_session(
        &self,
        args: acp::LoadSessionRequest,
    ) -> Result<acp::LoadSessionResponse, acp::Error> {
        let session = if let Some(session) = self.session_handle(&args.session_id) {
            session
        } else {
            let identifier = args.session_id.0.as_ref();
            self.attach_thread_from_archive(&args.session_id, identifier)
                .await
                .map_err(|err| acp::Error::internal_error().data(err.to_string()))?
        };

        if let Err(error) = self.send_available_commands_update(&args.session_id).await {
            warn!(%error, "Failed to advertise slash commands on session load");
        }

        let config_options = self.current_session_config_options(&session);
        Ok(acp::LoadSessionResponse::new().config_options(config_options))
    }

    /// Programmatic equivalent of the SACP `session/set_config_option`
    /// handler. Used both by the SACP handler and by the test suite.
    pub(crate) async fn set_session_config_option(
        &self,
        args: acp::SetSessionConfigOptionRequest,
    ) -> Result<acp::SetSessionConfigOptionResponse, acp::Error> {
        use crate::zed::helpers::SESSION_CONFIG_MODEL_ID;
        use crate::zed::helpers::SESSION_CONFIG_PRIMARY_AGENT_ID;
        use crate::zed::helpers::SESSION_CONFIG_PROVIDER_ID;
        use crate::zed::helpers::SESSION_CONFIG_THOUGHT_LEVEL_ID;

        let Some(session) = self.session_handle(&args.session_id) else {
            return Err(acp::Error::invalid_params().data(serde_json::json!({
                "reason": "unknown_session"
            })));
        };

        let config_id = args.config_id.0.to_string();
        let value = match &args.value {
            acp::SessionConfigOptionValue::ValueId { value } => value.0.as_ref().to_string(),
            _ => {
                return Err(acp::Error::invalid_params().data(serde_json::json!({
                    "reason": "expected_string_config_option_value",
                    "config_id": config_id,
                })));
            }
        };
        let updated = match config_id.as_str() {
            SESSION_CONFIG_PRIMARY_AGENT_ID => {
                let Some(primary_agent) = self.primary_agents.resolve_id(&value) else {
                    return Err(acp::Error::invalid_params().data(serde_json::json!({
                        "reason": "unknown_primary_agent",
                        "value": value,
                    })));
                };
                self.update_session_primary_agent(&session, primary_agent.to_string())
            }
            SESSION_CONFIG_THOUGHT_LEVEL_ID => {
                let (session_provider, session_model) = {
                    let data = session.data.lock().map_err(|_err| acp::Error::internal_error())?;
                    (data.provider.clone(), data.model.clone())
                };
                if !self.model_supports_thought_level(&session_provider, &session_model) {
                    return Err(acp::Error::invalid_params().data(serde_json::json!({
                        "reason": "unsupported_config_option",
                        "config_id": config_id,
                    })));
                }
                let Some(reasoning_effort) = ReasoningEffortLevel::parse(&value) else {
                    return Err(acp::Error::invalid_params().data(serde_json::json!({
                        "reason": "unknown_thought_level",
                        "value": value,
                    })));
                };
                self.update_session_reasoning_effort(&session, reasoning_effort)
            }
            SESSION_CONFIG_PROVIDER_ID => {
                let provider = value.trim().to_lowercase();
                let current_provider = {
                    let data = session.data.lock().map_err(|_err| acp::Error::internal_error())?;
                    data.provider.clone()
                };
                if provider.is_empty() || !self.supports_provider(&provider, &current_provider) {
                    return Err(acp::Error::invalid_params().data(serde_json::json!({
                        "reason": "unknown_provider",
                        "value": value,
                    })));
                }
                let current_model = {
                    let data = session.data.lock().map_err(|_err| acp::Error::internal_error())?;
                    data.model.clone()
                };
                let resolved_model = if self.provider_supports_model(&provider, &current_model) {
                    current_model
                } else {
                    self.provider_default_model(&provider).ok_or_else(|| {
                        acp::Error::invalid_params().data(serde_json::json!({
                            "reason": "provider_has_no_default_model",
                            "provider": provider,
                        }))
                    })?
                };
                self.update_session_provider_and_model(&session, provider, resolved_model)
            }
            SESSION_CONFIG_MODEL_ID => {
                let model = value.trim();
                if model.is_empty() {
                    return Err(acp::Error::invalid_params().data(serde_json::json!({
                        "reason": "unknown_model",
                        "value": value,
                    })));
                }
                let provider = {
                    let data = session.data.lock().map_err(|_err| acp::Error::internal_error())?;
                    data.provider.clone()
                };
                if !self.provider_supports_model(&provider, model) {
                    return Err(acp::Error::invalid_params().data(serde_json::json!({
                        "reason": "model_not_supported_for_provider",
                        "provider": provider,
                        "model": model,
                    })));
                }
                self.update_session_provider_and_model(&session, provider, model.to_string())
            }
            _ => {
                return Err(acp::Error::invalid_params().data(serde_json::json!({
                    "reason": "unknown_config_option",
                    "config_id": config_id,
                })));
            }
        };

        if updated {
            let config_options = self.current_session_config_options(&session);
            let update = acp::ConfigOptionUpdate::new(config_options);
            drop(
                self.send_update(&args.session_id, acp::SessionUpdate::ConfigOptionUpdate(update))
                    .await,
            );
        }

        let config_options = self.current_session_config_options(&session);
        Ok(acp::SetSessionConfigOptionResponse::new(config_options))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zed::helpers::PrimaryAgentCatalog;
    use assert_fs::TempDir;
    use chrono::Utc;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;
    use vtcode_config::{SubagentDiscoveryInput, discover_subagents};
    use vtcode_core::config::core::PromptCachingConfig;
    use vtcode_core::config::types::{AgentConfig as CoreAgentConfig, ModelSelectionSource, UiSurfacePreference};
    use vtcode_core::config::{AgentClientProtocolZedConfig, CommandsConfig, ToolsConfig};
    use vtcode_core::core::agent::snapshots::{
        DEFAULT_CHECKPOINTS_ENABLED, DEFAULT_MAX_AGE_DAYS, DEFAULT_MAX_SNAPSHOTS,
    };
    use vtcode_core::utils::session_archive::{SessionArchiveMetadata, SessionListing, SessionSnapshot};

    async fn build_agent(workspace: &Path) -> ZedAgent {
        build_agent_with_default_primary_agent(workspace, "duck").await
    }

    async fn build_agent_with_default_primary_agent(workspace: &Path, default_primary_agent: &str) -> ZedAgent {
        let core_config = CoreAgentConfig {
            model: "gpt-5.6-sol".to_string(),
            api_key: String::new(),
            provider: "openai".to_string(),
            api_key_env: "TEST_API_KEY".to_string(),
            workspace: workspace.to_path_buf(),
            verbose: false,
            quiet: false,
            theme: "test".to_string(),
            reasoning_effort: ReasoningEffortLevel::Low,
            ui_surface: UiSurfacePreference::default(),
            prompt_cache: PromptCachingConfig::default(),
            model_source: ModelSelectionSource::WorkspaceConfig,
            custom_api_keys: BTreeMap::new(),
            checkpointing_enabled: DEFAULT_CHECKPOINTS_ENABLED,
            checkpointing_storage_dir: None,
            checkpointing_max_snapshots: DEFAULT_MAX_SNAPSHOTS,
            checkpointing_max_age_days: Some(DEFAULT_MAX_AGE_DAYS),
            max_conversation_turns: 1000,
            model_behavior: None,
            openai_chatgpt_auth: None,
        };

        let mut discovery_input = SubagentDiscoveryInput::new(workspace.to_path_buf());
        discovery_input.include_user_agents = false;
        let discovered = discover_subagents(&discovery_input).expect("discover primary agents");
        let primary_agents = PrimaryAgentCatalog::from_specs_with_default(&discovered.effective, default_primary_agent);

        ZedAgent::new(
            core_config,
            false,
            AuthCredentialsStoreMode::default(),
            AgentClientProtocolZedConfig::default(),
            ToolsConfig::default(),
            CommandsConfig::default(),
            String::new(),
            Some("Zed".to_string()),
            primary_agents,
        )
        .await
    }

    fn primary_agent(session: &SessionHandle) -> String {
        session.data.lock().map(|data| data.primary_agent.clone()).unwrap_or_default()
    }

    fn reasoning_effort(session: &SessionHandle) -> ReasoningEffortLevel {
        session
            .data
            .lock()
            .map(|data| data.reasoning_effort)
            .unwrap_or(ReasoningEffortLevel::Low)
    }

    fn provider(session: &SessionHandle) -> String {
        session.data.lock().map(|data| data.provider.clone()).unwrap_or_default()
    }

    fn model(session: &SessionHandle) -> String {
        session.data.lock().map(|data| data.model.clone()).unwrap_or_default()
    }

    #[tokio::test]
    async fn build_session_handle_restores_session_config_from_thread_metadata() {
        let temp = TempDir::new().unwrap();
        let agent = build_agent(temp.path()).await;
        let listing = SessionListing {
            path: temp.path().join("session-vtcode-acp-archive.json"),
            snapshot: SessionSnapshot {
                metadata: SessionArchiveMetadata::new(
                    "vtcode",
                    temp.path().to_string_lossy(),
                    "gpt-5.6-sol",
                    "openai",
                    "test",
                    "xhigh",
                )
                .with_primary_agent("build"),
                started_at: Utc::now(),
                ended_at: Utc::now(),
                total_messages: 0,
                distinct_tools: Vec::new(),
                transcript: Vec::new(),
                messages: Vec::new(),
                progress: None,
                error_logs: Vec::new(),
            },
        };
        let thread = agent
            .thread_manager
            .start_thread_with_identifier("session-vtcode-acp-archive", ThreadBootstrap::from_listing(listing));

        let handle = agent.build_session_handle(acp::SessionId::new("session-1"), thread);

        assert_eq!(primary_agent(&handle), "build");
        assert_eq!(reasoning_effort(&handle), ReasoningEffortLevel::XHigh);
        assert_eq!(provider(&handle), "openai");
        assert_eq!(model(&handle), "gpt-5.6-sol");
    }

    #[tokio::test]
    async fn build_session_handle_falls_back_for_unknown_metadata_primary_agent() {
        let temp = TempDir::new().unwrap();
        let agent = build_agent(temp.path()).await;
        let listing = SessionListing {
            path: temp.path().join("session-vtcode-acp-archive.json"),
            snapshot: SessionSnapshot {
                metadata: SessionArchiveMetadata::new(
                    "vtcode",
                    temp.path().to_string_lossy(),
                    "gpt-5.6-sol",
                    "openai",
                    "test",
                    "low",
                )
                .with_primary_agent("missing"),
                started_at: Utc::now(),
                ended_at: Utc::now(),
                total_messages: 0,
                distinct_tools: Vec::new(),
                transcript: Vec::new(),
                messages: Vec::new(),
                progress: None,
                error_logs: Vec::new(),
            },
        };
        let thread = agent
            .thread_manager
            .start_thread_with_identifier("session-vtcode-acp-archive", ThreadBootstrap::from_listing(listing));

        let handle = agent.build_session_handle(acp::SessionId::new("session-1"), thread);

        assert_eq!(primary_agent(&handle), "duck");
    }

    #[tokio::test]
    async fn register_session_falls_back_for_unknown_default_primary_agent() {
        let temp = TempDir::new().unwrap();
        let agent = build_agent_with_default_primary_agent(temp.path(), "research").await;
        let session_id = agent.register_session();
        let session = agent.session_handle(&session_id).unwrap();

        // "research" is not in the discovered specs, so the resolver falls
        // back to the built-in "build" agent.
        assert_eq!(primary_agent(&session), "build");
    }

    #[tokio::test]
    async fn register_session_uses_known_default_primary_agent_ids() {
        for primary_agent in ["duck", "plan", "build", "auto"] {
            let temp = TempDir::new().unwrap();
            let agent = build_agent_with_default_primary_agent(temp.path(), primary_agent).await;
            let session_id = agent.register_session();
            let session = agent.session_handle(&session_id).unwrap();

            assert_eq!(self::primary_agent(&session), primary_agent);
        }
    }

    #[tokio::test]
    async fn register_session_uses_custom_default_primary_agent() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(".vtcode/agents")).unwrap();
        fs::write(
            temp.path().join(".vtcode/agents/research.md"),
            r#"---
name: research
description: Research primary
mode: primary
permissions:
  default: deny
---
Research primary prompt."#,
        )
        .unwrap();

        let agent = build_agent_with_default_primary_agent(temp.path(), "research").await;
        let session_id = agent.register_session();
        let session = agent.session_handle(&session_id).unwrap();

        assert_eq!(primary_agent(&session), "research");
    }

    #[tokio::test]
    async fn update_session_primary_agent_updates_session_data() {
        let temp = TempDir::new().unwrap();
        let agent = build_agent(temp.path()).await;
        let session_id = agent.register_session();
        let session = agent.session_handle(&session_id).unwrap();

        assert!(agent.update_session_primary_agent(&session, "build".to_string()));
        assert_eq!(primary_agent(&session), "build");
        assert_eq!(
            session
                .data
                .lock()
                .ok()
                .and_then(|data| data.thread.metadata())
                .and_then(|metadata| metadata.primary_agent)
                .as_deref(),
            Some("build")
        );
    }

    #[tokio::test]
    async fn update_session_reasoning_effort_syncs_thread_metadata() {
        let temp = TempDir::new().unwrap();
        let agent = build_agent(temp.path()).await;
        let session_id = agent.register_session();
        let session = agent.session_handle(&session_id).unwrap();

        assert!(agent.update_session_reasoning_effort(&session, ReasoningEffortLevel::High));
        assert_eq!(
            session
                .data
                .lock()
                .ok()
                .and_then(|data| data.thread.metadata())
                .map(|metadata| metadata.reasoning_effort)
                .as_deref(),
            Some("high")
        );
    }

    #[tokio::test]
    async fn current_session_config_options_omit_thought_level_when_model_lacks_support() {
        let temp = TempDir::new().unwrap();
        let mut agent = build_agent(temp.path()).await;
        agent.config.model = "claude-haiku-3".to_string();
        agent.config.provider = "anthropic".to_string();
        let session_id = agent.register_session();
        let session = agent.session_handle(&session_id).unwrap();

        let config_options = agent.current_session_config_options(&session);

        assert_eq!(config_options.len(), 3);
        assert_eq!(config_options[0].id, acp::SessionConfigId::new("primary_agent"));
    }
}
