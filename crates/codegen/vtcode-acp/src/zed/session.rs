//! Top-level glue: wire the [`ZedAgent`] into an SACP `AgentToClient`
//! connection over stdio.

use crate::register_acp_connection;
use crate::workspace::{DefaultWorkspaceTrustSynchronizer, WorkspaceTrustSyncOutcome, WorkspaceTrustSynchronizer};
use crate::zed::agent::ZedAgent;
use crate::zed::agent::handlers::install_handlers;
use crate::zed::connection::ConnectionHandle;
use agent_client_protocol::{Agent, Client, ConnectionTo, Stdio};
use anyhow::{Context, Result};
use std::future::pending;
use std::sync::Arc;
use tracing::{error, info, warn};
use vtcode_config::{SubagentDiscoveryInput, discover_subagents};
use vtcode_core::config::VTCodeConfig;
use vtcode_core::config::types::AgentConfig as CoreAgentConfig;
use vtcode_core::prompts::system::generate_system_instruction_with_config;

use super::constants::{
    WORKSPACE_TRUST_ALREADY_SATISFIED_LOG, WORKSPACE_TRUST_DOWNGRADE_SKIPPED_LOG, WORKSPACE_TRUST_UPGRADE_LOG,
};
use super::helpers::PrimaryAgentCatalog;

pub async fn run_acp_agent(config: &CoreAgentConfig, vt_cfg: &VTCodeConfig, title: Option<String>) -> Result<()> {
    let zed_config = &vt_cfg.acp.zed;
    let desired_trust_level = zed_config.workspace_trust.to_workspace_trust_level();
    let trust_synchronizer = DefaultWorkspaceTrustSynchronizer::new();
    match trust_synchronizer
        .synchronize(&config.workspace, desired_trust_level)
        .await
        .context("Failed to synchronize workspace trust for ACP bridge")?
    {
        WorkspaceTrustSyncOutcome::Upgraded { previous, new } => {
            info!(previous = ?previous, new = ?new, "{}", WORKSPACE_TRUST_UPGRADE_LOG);
        }
        WorkspaceTrustSyncOutcome::AlreadyMatches(level) => {
            info!(level = ?level, "{}", WORKSPACE_TRUST_ALREADY_SATISFIED_LOG);
        }
        WorkspaceTrustSyncOutcome::SkippedDowngrade(current) => {
            info!(
                current = ?current,
                requested = ?zed_config.workspace_trust,
                "{}",
                WORKSPACE_TRUST_DOWNGRADE_SKIPPED_LOG
            );
        }
    }

    let content = generate_system_instruction_with_config(&Default::default(), &config.workspace, Some(vt_cfg)).await;
    let system_prompt = if let Some(text) = content.parts.first().and_then(|p| p.as_text()) {
        text.to_string()
    } else {
        String::new()
    };
    let tools_config = vt_cfg.tools.clone();
    let commands_config = vt_cfg.commands.clone();
    let credential_storage_mode = vt_cfg.agent.credential_storage_mode;
    let allow_reasoning_effort_downgrade = vt_cfg.agent.allow_reasoning_effort_downgrade;
    let discovered = discover_subagents(&SubagentDiscoveryInput::new(config.workspace.clone()))
        .context("Failed to discover primary agents for ACP bridge")?;
    let primary_agents =
        PrimaryAgentCatalog::from_specs_with_default(&discovered.effective, &vt_cfg.default_primary_agent);

    let local_set = tokio::task::LocalSet::new();
    let config_clone = config.clone();
    let zed_config_clone = zed_config.clone();
    let title_clone = title.clone();

    let result = local_set
        .run_until(Box::pin(async move {
            let tools_config_clone = tools_config.clone();
            let commands_config_clone = commands_config.clone();
            let agent = ZedAgent::new(
                config_clone,
                allow_reasoning_effort_downgrade,
                credential_storage_mode,
                zed_config_clone,
                tools_config_clone,
                commands_config_clone,
                system_prompt,
                title_clone,
                primary_agents,
            )
            .await;
            let agent: Arc<ZedAgent> = Arc::new(agent);

            // Build the SACP agent-side connection, then attach the
            // vtcode request/notification handlers around `ZedAgent`.
            let builder = install_handlers(Agent.builder().name("vtcode"), Arc::clone(&agent));

            // The SACP dispatch loop never exposes `ConnectionTo` outside
            // of handler closures, so we use `connect_with` to capture
            // it. The closure parks on `pending()` so the spawned task
            // lives for the entire connection lifetime (and is cancelled
            // automatically when the connection closes).
            let attach_agent = Arc::clone(&agent);
            builder
                .connect_with(Stdio::new(), async move |cx: ConnectionTo<Client>| {
                    let handle = ConnectionHandle::new(cx);
                    if let Err(existing) = register_acp_connection(Arc::clone(&handle)) {
                        warn!("ACP client already registered; continuing with existing instance");
                        drop(existing);
                    }
                    attach_agent.attach_client(Arc::clone(&handle));
                    // Park the main task so the connection stays alive.
                    drop(pending::<agent_client_protocol::Result<()>>().await);
                    Ok(())
                })
                .await
                .map_err(|error| anyhow::anyhow!("ACP stdio connection failed: {error}"))?;

            Ok::<(), anyhow::Error>(())
        }))
        .await;

    if let Err(error) = result {
        error!(%error, "ACP bridge task failed");
        return Err(error);
    }
    Ok(())
}
