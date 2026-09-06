use crate::acp;
use crate::permissions::{AcpPermissionPrompter, DefaultPermissionPrompter};
use crate::tooling::AcpToolRegistry;
use crate::zed::connection::ConnectionHandle;
use hashbrown::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::warn;
use vtcode_config::auth::AuthCredentialsStoreMode;
use vtcode_core::config::ToolDocumentationMode;
use vtcode_core::config::types::{AgentConfig as CoreAgentConfig, CapabilityLevel};
use vtcode_core::config::{AgentClientProtocolZedConfig, CommandsConfig, ToolsConfig};
use vtcode_core::core::threads::ThreadManager;
use vtcode_core::tools::file_ops::FileOpsTool;
use vtcode_core::tools::grep_file::GrepSearchManager;
use vtcode_core::tools::handlers::{SessionSurface, SessionToolsConfig, ToolModelCapabilities};
use vtcode_core::tools::registry::ToolRegistry as CoreToolRegistry;

use super::helpers::PrimaryAgentCatalog;
use super::types::SessionHandle;

pub(crate) mod handlers;
mod prompt;
mod session_state;
mod tool_config;
#[cfg(test)]
mod tool_config_tests;
mod tool_execution;
mod tool_execution_local;
mod updates;

/// SACP-style agent bridge. `Send + Sync` so it can be moved into SACP
/// `cx.spawn` tasks and held inside the global connection registry.
pub(crate) struct ZedAgent {
    config: CoreAgentConfig,
    allow_reasoning_effort_downgrade: bool,
    credential_storage_mode: AuthCredentialsStoreMode,
    system_prompt: String,
    sessions: Arc<Mutex<HashMap<acp::SessionId, SessionHandle>>>,
    next_session_id: AtomicU64,
    acp_tool_registry: Arc<AcpToolRegistry>,
    permission_prompter: Arc<dyn AcpPermissionPrompter + Send + Sync>,
    local_tool_registry: CoreToolRegistry,
    file_ops_tool: Option<FileOpsTool>,
    thread_manager: ThreadManager,
    client_capabilities: Arc<Mutex<Option<acp::ClientCapabilities>>>,
    client: Arc<Mutex<Option<Arc<ConnectionHandle>>>>,
    title: Option<String>,
    primary_agents: PrimaryAgentCatalog,
    tool_loop_limit: usize,
    tool_call_delay: Option<Duration>,
}

impl ZedAgent {
    pub(crate) async fn new(
        config: CoreAgentConfig,
        allow_reasoning_effort_downgrade: bool,
        credential_storage_mode: AuthCredentialsStoreMode,
        zed_config: AgentClientProtocolZedConfig,
        tools_config: ToolsConfig,
        commands_config: CommandsConfig,
        system_prompt: String,
        title: Option<String>,
        primary_agents: PrimaryAgentCatalog,
    ) -> Self {
        let read_file_enabled = zed_config.tools.read_file;
        let workspace_root = config.workspace.clone();
        let tool_loop_limit = tools_config.max_tool_loops;
        let tool_call_delay = tools_config.tool_call_delay();
        let file_ops_tool = if zed_config.tools.list_files {
            let search_root = workspace_root.clone();
            Some(FileOpsTool::new(workspace_root.clone(), Arc::new(GrepSearchManager::new(search_root))))
        } else {
            None
        };
        let list_files_enabled = file_ops_tool.is_some();
        let core_tool_registry = CoreToolRegistry::new(config.workspace.clone()).await;
        if let Err(error) = core_tool_registry
            .apply_tool_runtime_config(&commands_config, &tools_config)
            .await
        {
            warn!(%error, "Failed to apply tools configuration to ACP tool registry");
        }
        let local_definitions = core_tool_registry
            .model_tools(
                SessionToolsConfig::full_public(
                    SessionSurface::Acp,
                    CapabilityLevel::CodeSearch,
                    ToolDocumentationMode::default(),
                    ToolModelCapabilities::default(),
                )
                .with_tool_profile(tools_config.profile),
            )
            .await;
        let acp_tool_registry = Arc::new(AcpToolRegistry::new(
            workspace_root.as_path(),
            read_file_enabled,
            list_files_enabled,
            local_definitions,
        ));
        let permission_prompter: Arc<dyn AcpPermissionPrompter + Send + Sync> =
            Arc::new(DefaultPermissionPrompter::new(Arc::clone(&acp_tool_registry) as Arc<_>));

        Self {
            config,
            allow_reasoning_effort_downgrade,
            credential_storage_mode,
            system_prompt,
            sessions: Arc::new(Mutex::new(HashMap::with_capacity(10))),
            next_session_id: AtomicU64::new(0),
            acp_tool_registry,
            permission_prompter,
            local_tool_registry: core_tool_registry,
            file_ops_tool,
            thread_manager: ThreadManager::new(),
            client_capabilities: Arc::new(Mutex::new(None)),
            client: Arc::new(Mutex::new(None)),
            title,
            primary_agents,
            tool_loop_limit,
            tool_call_delay,
        }
    }

    /// Attach the live SACP `cx` handle. Called once after the SACP
    /// connection has been opened.
    pub(crate) fn attach_client(&self, client: Arc<ConnectionHandle>) {
        if let Ok(mut guard) = self.client.lock() {
            *guard = Some(client);
        }
    }

    /// Borrow the SACP `cx` handle, if one is attached.
    fn client(&self) -> Option<Arc<ConnectionHandle>> {
        self.client.lock().unwrap_or_else(|e| e.into_inner()).as_ref().cloned()
    }

    /// Optional human-readable title used during `initialize`.
    fn title(&self) -> Option<String> {
        self.title.clone()
    }
}
