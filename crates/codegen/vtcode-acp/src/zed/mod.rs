use anyhow::Result;
use async_trait::async_trait;
use vtcode_core::core::interfaces::acp::{AcpClientAdapter, AcpLaunchParams};

mod agent;
pub(crate) mod connection;
pub(crate) mod constants;
mod helpers;
mod session;
mod types;

use session::run_acp_agent;

#[derive(Debug, Default, Clone, Copy)]
pub struct ZedAcpAdapter;

#[async_trait(?Send)]
impl AcpClientAdapter for ZedAcpAdapter {
    async fn serve(&self, params: AcpLaunchParams<'_>) -> Result<()> {
        run_acp_agent(params.agent_config, params.runtime_config, Some("Zed".to_string())).await
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct StandardAcpAdapter;

#[async_trait(?Send)]
impl AcpClientAdapter for StandardAcpAdapter {
    async fn serve(&self, params: AcpLaunchParams<'_>) -> Result<()> {
        run_acp_agent(params.agent_config, params.runtime_config, None).await
    }
}

#[cfg(test)]
#[allow(
    unused_imports,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
mod tests {
    use super::agent::ZedAgent;
    use super::*;
    use crate::tooling::{TOOL_LIST_FILES_ITEMS_KEY, TOOL_LIST_FILES_RESULT_KEY, TOOL_LIST_FILES_URI_ARG};
    use crate::zed::helpers::{
        PrimaryAgentCatalog, SESSION_CONFIG_MODEL_ID, SESSION_CONFIG_PRIMARY_AGENT_ID, SESSION_CONFIG_PROVIDER_ID,
        SESSION_CONFIG_THOUGHT_LEVEL_ID,
    };
    use agent_client_protocol::schema::v1::{
        LoadSessionRequest, NewSessionRequest, SetSessionConfigOptionRequest, ToolCallStatus,
    };
    use agent_client_protocol::schema::v1::{SessionConfigKind, SessionConfigSelectOptions};
    use assert_fs::TempDir;
    use serde_json::{Value, json};
    use std::collections::BTreeMap;
    use std::path::Path;
    use tokio::fs;
    use vtcode_config::auth::AuthCredentialsStoreMode;
    use vtcode_config::{SubagentDiscoveryInput, discover_subagents};
    use vtcode_core::config::core::PromptCachingConfig;
    use vtcode_core::config::models::{ModelId, Provider};
    use vtcode_core::config::types::{
        AgentConfig as CoreAgentConfig, ModelSelectionSource, ReasoningEffortLevel, UiSurfacePreference,
    };
    use vtcode_core::config::{AgentClientProtocolZedConfig, CommandsConfig, ToolsConfig};
    use vtcode_core::core::agent::snapshots::{
        DEFAULT_CHECKPOINTS_ENABLED, DEFAULT_MAX_AGE_DAYS, DEFAULT_MAX_SNAPSHOTS,
    };

    async fn build_agent(workspace: &Path) -> ZedAgent {
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

        let mut zed_config = AgentClientProtocolZedConfig::default();
        zed_config.tools.list_files = true;
        zed_config.tools.read_file = false;

        let tools_config = ToolsConfig::default();
        let mut discovery_input = SubagentDiscoveryInput::new(workspace.to_path_buf());
        discovery_input.include_user_agents = false;
        let discovered = discover_subagents(&discovery_input).expect("discover primary agents");
        let primary_agents = PrimaryAgentCatalog::from_specs_with_default(&discovered.effective, "duck");

        ZedAgent::new(
            core_config,
            false,
            AuthCredentialsStoreMode::default(),
            zed_config,
            tools_config,
            CommandsConfig::default(),
            String::new(),
            Some("Zed".to_string()),
            primary_agents,
        )
        .await
    }

    fn list_items_from_payload(payload: &Value) -> Vec<Value> {
        payload
            .get(TOOL_LIST_FILES_RESULT_KEY)
            .and_then(Value::as_object)
            .and_then(|result| result.get(TOOL_LIST_FILES_ITEMS_KEY))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    fn primary_agent_select_values(config_options: &[crate::acp::SessionConfigOption]) -> Vec<String> {
        config_options
            .iter()
            .find_map(|option| {
                (option.id == crate::acp::SessionConfigId::new(SESSION_CONFIG_PRIMARY_AGENT_ID)).then_some(&option.kind)
            })
            .and_then(|kind| match kind {
                SessionConfigKind::Select(select) => Some(match &select.options {
                    SessionConfigSelectOptions::Ungrouped(options) => {
                        options.iter().map(|option| option.value.0.as_ref().to_string()).collect()
                    }
                    _ => Vec::new(),
                }),
                _ => None,
            })
            .unwrap_or_default()
    }

    fn primary_agent_select_labels(config_options: &[crate::acp::SessionConfigOption]) -> Vec<String> {
        config_options
            .iter()
            .find_map(|option| {
                (option.id == crate::acp::SessionConfigId::new(SESSION_CONFIG_PRIMARY_AGENT_ID)).then_some(&option.kind)
            })
            .and_then(|kind| match kind {
                SessionConfigKind::Select(select) => Some(match &select.options {
                    SessionConfigSelectOptions::Ungrouped(options) => {
                        options.iter().map(|option| option.name.clone()).collect()
                    }
                    _ => Vec::new(),
                }),
                _ => None,
            })
            .unwrap_or_default()
    }

    fn primary_agent_current_value(config_options: &[crate::acp::SessionConfigOption]) -> Option<String> {
        config_options.iter().find_map(|option| {
            if option.id == crate::acp::SessionConfigId::new(SESSION_CONFIG_PRIMARY_AGENT_ID) {
                match &option.kind {
                    SessionConfigKind::Select(select) => Some(select.current_value.0.as_ref().to_string()),
                    _ => None,
                }
            } else {
                None
            }
        })
    }

    #[tokio::test]
    async fn run_list_files_defaults_to_workspace_root() {
        let temp = TempDir::new().unwrap();
        let subdir = temp.path().join("src");
        fs::create_dir(&subdir).await.unwrap();
        let file_path = subdir.join("sample.txt");
        fs::write(&file_path, "hello").await.unwrap();

        let agent = build_agent(temp.path()).await;
        let report = agent.run_list_files(&json!({"path": "src"})).await.unwrap();

        assert!(matches!(report.status, ToolCallStatus::Completed));
        let payload = report.raw_output.unwrap();
        let items = list_items_from_payload(&payload);
        assert!(
            items.iter().any(|item| {
                item.get("name")
                    .and_then(Value::as_str)
                    .map(|name| name == "sample.txt")
                    .or_else(|| {
                        item.get("path")
                            .and_then(Value::as_str)
                            .map(|path| path.ends_with("sample.txt"))
                    })
                    .unwrap_or(false)
            }),
            "payload: {payload}"
        );
    }

    #[tokio::test]
    async fn run_list_files_accepts_uri_argument() {
        let temp = TempDir::new().unwrap();
        let nested = temp.path().join("nested");
        fs::create_dir_all(&nested).await.unwrap();
        let inner = nested.join("inner.txt");
        fs::write(&inner, "data").await.unwrap();

        let agent = build_agent(temp.path()).await;
        let uri = format!("file://{}", nested.to_string_lossy());
        let report = agent.run_list_files(&json!({ TOOL_LIST_FILES_URI_ARG: uri })).await.unwrap();

        assert!(matches!(report.status, ToolCallStatus::Completed));
        let payload = report.raw_output.unwrap();
        let items = list_items_from_payload(&payload);
        assert!(items.iter().any(|item| {
            item.get("path")
                .and_then(Value::as_str)
                .map(|path| path.contains("inner.txt"))
                .unwrap_or(false)
        }));
    }

    #[tokio::test]
    async fn load_session_returns_existing_session_state() {
        let temp = TempDir::new().unwrap();
        let agent = build_agent(temp.path()).await;
        let session_id = agent.register_session();

        {
            let session = agent.session_handle(&session_id).unwrap();
            let mut data = session.data.lock().unwrap();
            data.primary_agent = "build".to_string();
            data.reasoning_effort = ReasoningEffortLevel::High;
        }

        let args = LoadSessionRequest::new(session_id, temp.path());
        let response = agent.load_session(args).await.unwrap();

        assert!(response.modes.is_none());
        let config_options = response.config_options.unwrap();
        assert_eq!(config_options.len(), 4);
        assert!(config_options.iter().any(|option| {
            option.id == crate::acp::SessionConfigId::new(SESSION_CONFIG_PRIMARY_AGENT_ID)
                && matches!(
                    &option.kind,
                    SessionConfigKind::Select(select)
                        if select.current_value
                            == crate::acp::SessionConfigValueId::new("build")
                )
        }));
        assert!(config_options.iter().any(|option| {
            option.id == crate::acp::SessionConfigId::new(SESSION_CONFIG_THOUGHT_LEVEL_ID)
                && matches!(
                    &option.kind,
                    SessionConfigKind::Select(select)
                        if select.current_value
                            == crate::acp::SessionConfigValueId::new("high")
                )
        }));
        assert!(config_options.iter().any(|option| {
            option.id == crate::acp::SessionConfigId::new(SESSION_CONFIG_PROVIDER_ID)
                && matches!(
                    &option.kind,
                    SessionConfigKind::Select(select)
                        if select.current_value
                            == crate::acp::SessionConfigValueId::new("openai")
                )
        }));
        assert!(config_options.iter().any(|option| {
            option.id == crate::acp::SessionConfigId::new(SESSION_CONFIG_MODEL_ID)
                && matches!(
                    &option.kind,
                    SessionConfigKind::Select(select)
                        if select.current_value
                            == crate::acp::SessionConfigValueId::new("gpt-5.6-sol")
                )
        }));
    }

    #[tokio::test]
    async fn new_session_returns_config_options() {
        let temp = TempDir::new().unwrap();
        let agent = build_agent(temp.path()).await;

        let response = agent.new_session(NewSessionRequest::new(temp.path())).await.unwrap();

        assert!(response.modes.is_none());
        let config_options = response.config_options.unwrap();
        assert_eq!(config_options.len(), 4);
        assert!(config_options.iter().any(|option| {
            option.id == crate::acp::SessionConfigId::new(SESSION_CONFIG_PRIMARY_AGENT_ID)
                && matches!(
                    &option.kind,
                    SessionConfigKind::Select(select)
                        if select.current_value
                            == crate::acp::SessionConfigValueId::new("duck")
                )
        }));
        assert!(config_options.iter().any(|option| {
            option.id == crate::acp::SessionConfigId::new(SESSION_CONFIG_THOUGHT_LEVEL_ID)
                && matches!(
                    &option.kind,
                    SessionConfigKind::Select(select)
                        if select.current_value
                            == crate::acp::SessionConfigValueId::new("low")
                )
        }));
        assert!(config_options.iter().any(|option| {
            option.id == crate::acp::SessionConfigId::new(SESSION_CONFIG_PROVIDER_ID)
                && matches!(
                    &option.kind,
                    SessionConfigKind::Select(select)
                        if select.current_value
                            == crate::acp::SessionConfigValueId::new("openai")
                )
        }));
        assert!(config_options.iter().any(|option| {
            option.id == crate::acp::SessionConfigId::new(SESSION_CONFIG_MODEL_ID)
                && matches!(
                    &option.kind,
                    SessionConfigKind::Select(select)
                        if select.current_value
                            == crate::acp::SessionConfigValueId::new("gpt-5.6-sol")
                )
        }));
    }

    #[tokio::test]
    async fn set_session_config_option_updates_primary_agent() {
        let temp = TempDir::new().unwrap();
        let agent = build_agent(temp.path()).await;
        let session_id = agent.register_session();

        let response = agent
            .set_session_config_option(SetSessionConfigOptionRequest::new(
                session_id.clone(),
                SESSION_CONFIG_PRIMARY_AGENT_ID,
                "build",
            ))
            .await
            .unwrap();

        let session = agent.session_handle(&session_id).unwrap();
        assert_eq!(session.data.lock().unwrap().primary_agent, "build");
        assert!(response.config_options.iter().any(|option| {
            option.id == crate::acp::SessionConfigId::new(SESSION_CONFIG_PRIMARY_AGENT_ID)
                && matches!(
                    &option.kind,
                    SessionConfigKind::Select(select)
                        if select.current_value
                            == crate::acp::SessionConfigValueId::new("build")
                )
        }));
    }

    #[tokio::test]
    async fn set_session_config_option_accepts_only_known_primary_agent_ids() {
        let temp = TempDir::new().unwrap();
        let agent = build_agent(temp.path()).await;
        let session_id = agent.register_session();

        for primary_agent in ["duck", "plan", "build", "auto"] {
            let response = agent
                .set_session_config_option(SetSessionConfigOptionRequest::new(
                    session_id.clone(),
                    SESSION_CONFIG_PRIMARY_AGENT_ID,
                    primary_agent,
                ))
                .await
                .unwrap();

            let session = agent.session_handle(&session_id).unwrap();
            assert_eq!(session.data.lock().unwrap().primary_agent, primary_agent);
            assert_eq!(primary_agent_current_value(&response.config_options), Some(primary_agent.to_string()));
        }
    }

    #[tokio::test]
    async fn set_session_config_option_rejects_unknown_primary_agent() {
        let temp = TempDir::new().unwrap();
        let agent = build_agent(temp.path()).await;
        let session_id = agent.register_session();
        let session = agent.session_handle(&session_id).unwrap();
        session.data.lock().unwrap().primary_agent = "build".to_string();

        let result = agent
            .set_session_config_option(SetSessionConfigOptionRequest::new(
                session_id.clone(),
                SESSION_CONFIG_PRIMARY_AGENT_ID,
                "research",
            ))
            .await;

        let error = result.expect_err("unknown primary agent should be rejected");
        let error = format!("{error:?}");
        assert!(error.contains("unknown_primary_agent"));
        assert!(error.contains("research"));
        assert_eq!(session.data.lock().unwrap().primary_agent, "build");
    }

    #[tokio::test]
    async fn session_config_options_include_custom_primary_agent() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(".vtcode/agents")).await.unwrap();
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
        .await
        .unwrap();
        let agent = build_agent(temp.path()).await;
        let session_id = agent.register_session();
        let session = agent.session_handle(&session_id).unwrap();
        session.data.lock().unwrap().primary_agent = "research".to_string();

        let response = agent
            .load_session(LoadSessionRequest::new(session_id, temp.path()))
            .await
            .unwrap();
        let config_options = response.config_options.unwrap();

        assert_eq!(
            primary_agent_select_values(&config_options),
            ["duck", "plan", "build", "auto", "research"].map(str::to_string).to_vec()
        );
        assert!(primary_agent_select_labels(&config_options).contains(&"Research primary".into()));
        assert_eq!(primary_agent_current_value(&config_options), Some("research".to_string()));
    }

    #[tokio::test]
    async fn session_config_options_use_overridden_builtin_primary_agent_metadata() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(".vtcode/agents")).await.unwrap();
        fs::write(
            temp.path().join(".vtcode/agents/build.md"),
            r#"---
name: build
description: Project Build
mode: primary
permissions:
  default: deny
aliases:
  - project-builder
---
Project build prompt."#,
        )
        .await
        .unwrap();
        let agent = build_agent(temp.path()).await;
        let session_id = agent.register_session();
        let session = agent.session_handle(&session_id).unwrap();

        let response = agent
            .set_session_config_option(SetSessionConfigOptionRequest::new(
                session_id,
                SESSION_CONFIG_PRIMARY_AGENT_ID,
                "project-builder",
            ))
            .await
            .unwrap();

        assert_eq!(session.data.lock().unwrap().primary_agent, "build");
        assert!(primary_agent_select_labels(&response.config_options).contains(&"Project Build".into()));
    }

    #[tokio::test]
    async fn set_session_config_option_updates_reasoning_effort() {
        let temp = TempDir::new().unwrap();
        let agent = build_agent(temp.path()).await;
        let session_id = agent.register_session();

        let response = agent
            .set_session_config_option(SetSessionConfigOptionRequest::new(
                session_id.clone(),
                SESSION_CONFIG_THOUGHT_LEVEL_ID,
                "xhigh",
            ))
            .await
            .unwrap();

        let session = agent.session_handle(&session_id).unwrap();
        assert_eq!(session.data.lock().unwrap().reasoning_effort, ReasoningEffortLevel::XHigh);
        assert!(response.config_options.iter().any(|option| {
            option.id == crate::acp::SessionConfigId::new(SESSION_CONFIG_THOUGHT_LEVEL_ID)
                && matches!(
                    &option.kind,
                    SessionConfigKind::Select(select)
                        if select.current_value
                            == crate::acp::SessionConfigValueId::new("xhigh")
                )
        }));
    }

    #[tokio::test]
    async fn set_session_config_option_updates_provider_and_auto_switches_model() {
        let temp = TempDir::new().unwrap();
        let agent = build_agent(temp.path()).await;
        let session_id = agent.register_session();
        let anthropic_default = ModelId::default_single_for_provider(Provider::Anthropic).as_str().into_owned();

        let response = agent
            .set_session_config_option(SetSessionConfigOptionRequest::new(
                session_id.clone(),
                SESSION_CONFIG_PROVIDER_ID,
                "anthropic",
            ))
            .await
            .unwrap();

        let session = agent.session_handle(&session_id).unwrap();
        assert_eq!(session.data.lock().unwrap().provider, "anthropic");
        assert_eq!(session.data.lock().unwrap().model, anthropic_default);
        assert!(response.config_options.iter().any(|option| {
            option.id == crate::acp::SessionConfigId::new(SESSION_CONFIG_PROVIDER_ID)
                && matches!(
                    &option.kind,
                    SessionConfigKind::Select(select)
                        if select.current_value
                            == crate::acp::SessionConfigValueId::new("anthropic")
                )
        }));
        assert!(response.config_options.iter().any(|option| {
            option.id == crate::acp::SessionConfigId::new(SESSION_CONFIG_MODEL_ID)
                && matches!(
                    &option.kind,
                    SessionConfigKind::Select(select)
                        if select.current_value
                            == crate::acp::SessionConfigValueId::new(anthropic_default.as_str())
                )
        }));
    }

    #[tokio::test]
    async fn set_session_config_option_updates_model_for_provider() {
        let temp = TempDir::new().unwrap();
        let agent = build_agent(temp.path()).await;
        let session_id = agent.register_session();

        let response = agent
            .set_session_config_option(SetSessionConfigOptionRequest::new(
                session_id.clone(),
                SESSION_CONFIG_MODEL_ID,
                "gpt-5.6-luna",
            ))
            .await
            .unwrap();

        let session = agent.session_handle(&session_id).unwrap();
        assert_eq!(session.data.lock().unwrap().provider, "openai");
        assert_eq!(session.data.lock().unwrap().model, "gpt-5.6-luna");
        assert!(response.config_options.iter().any(|option| {
            option.id == crate::acp::SessionConfigId::new(SESSION_CONFIG_MODEL_ID)
                && matches!(
                    &option.kind,
                    SessionConfigKind::Select(select)
                        if select.current_value
                            == crate::acp::SessionConfigValueId::new("gpt-5.6-luna")
                )
        }));
    }
}
