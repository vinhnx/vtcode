//! MCP client integration for ToolRegistry.

use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde_json::Value;
use tracing::{debug, warn};
use vtcode_commons::{ErrorCategory, classify_anyhow_error};

use crate::mcp::{McpClient, McpToolExecutor, McpToolInfo};
use crate::tools::mcp::build_mcp_registration;

use super::ToolRegistry;
use super::mcp_helpers::normalize_mcp_tool_identifier;

impl ToolRegistry {
    /// Set the MCP client for this registry.
    pub async fn with_mcp_client(self, mcp_client: Arc<McpClient>) -> Self {
        *self.mcp_client.write() = Some(mcp_client);
        self.mcp_tool_index.write().await.clear();
        self.mcp_reverse_index.write().await.clear();
        *self.cached_available_tools.write() = None;
        self.initialized.store(false, std::sync::atomic::Ordering::Relaxed);
        self
    }

    /// Attach an MCP client without consuming the registry.
    pub async fn set_mcp_client(&self, mcp_client: Arc<McpClient>) {
        *self.mcp_client.write() = Some(mcp_client);
        self.mcp_tool_index.write().await.clear();
        self.mcp_reverse_index.write().await.clear();
        *self.cached_available_tools.write() = None;
        self.initialized.store(false, std::sync::atomic::Ordering::Relaxed);
    }

    /// Detach the current MCP client and clear MCP tool indexes.
    pub async fn clear_mcp_client(&self) {
        *self.mcp_client.write() = None;
        self.mcp_tool_index.write().await.clear();
        self.mcp_reverse_index.write().await.clear();
        *self.cached_available_tools.write() = None;
        self.initialized.store(false, std::sync::atomic::Ordering::Relaxed);
    }

    /// Get the MCP client if available.
    pub fn mcp_client(&self) -> Option<Arc<McpClient>> {
        self.mcp_client.read().clone()
    }

    /// List all MCP tools.
    pub async fn list_mcp_tools(&self) -> Result<Vec<McpToolInfo>> {
        let index = self.mcp_tool_index.read().await;
        if index.is_empty() {
            return Ok(Vec::new());
        }

        let mut mcp_tools = Vec::new();
        for (provider, tools) in index.iter() {
            for tool_name in tools {
                let canonical_name = format!("mcp::{provider}::{tool_name}");
                if let Some(registration) = self.inventory.get_registration(&canonical_name) {
                    mcp_tools.push(McpToolInfo {
                        name: tool_name.clone(),
                        description: registration.metadata().description().unwrap_or("").to_string(),
                        provider: provider.clone(),
                        input_schema: registration.parameter_schema().cloned().unwrap_or(Value::Null),
                    });
                }
            }
        }

        Ok(mcp_tools)
    }

    /// Check if an MCP tool exists.
    pub async fn has_mcp_tool(&self, tool_name: &str) -> bool {
        self.mcp_reverse_index.read().await.contains_key(tool_name)
    }

    /// Execute an MCP tool.
    pub async fn execute_mcp_tool(&self, tool_name: &str, args: Value) -> Result<Value> {
        let client_opt = self.mcp_client.read().clone();
        if let Some(mcp_client) = client_opt {
            mcp_client.execute_mcp_tool(tool_name, &args).await
        } else {
            Err(anyhow!("MCP client not available"))
        }
    }

    pub(super) async fn resolve_mcp_tool_alias(&self, tool_name: &str) -> Option<String> {
        let normalized = normalize_mcp_tool_identifier(tool_name);
        if normalized.is_empty() {
            return None;
        }

        let index = self.mcp_tool_index.read().await;
        for tools in index.values() {
            for tool in tools {
                if normalize_mcp_tool_identifier(tool) == normalized {
                    return Some(tool.clone());
                }
            }
        }

        None
    }

    /// Refresh MCP tools (reconnect to providers and update tool lists).
    pub async fn refresh_mcp_tools(&self) -> Result<()> {
        let mcp_client_opt = self.mcp_client.read().clone();
        if let Some(mcp_client) = mcp_client_opt {
            debug!("Refreshing MCP tools for {} providers", mcp_client.get_status().provider_count);

            let mut tools: Option<Vec<McpToolInfo>> = None;
            let mut last_err: Option<anyhow::Error> = None;
            for attempt in 0..3 {
                match mcp_client.list_mcp_tools().await {
                    Ok(list) => {
                        tools = Some(list);
                        break;
                    }
                    Err(err) => {
                        last_err = Some(err);
                        let jitter = (attempt * 37) % 80;
                        let pow = 2_u64.saturating_pow(attempt.min(4) as u32); // cap exponent
                        let backoff = Duration::from_millis(200 * pow + jitter).min(Duration::from_secs(3));
                        warn!(
                            attempt = attempt + 1,
                            delay_ms = %backoff.as_millis(),
                            "Failed to list MCP tools, retrying with backoff"
                        );
                        tokio::time::sleep(backoff).await;
                    }
                }
            }

            let tools = match tools {
                Some(list) => list,
                None => {
                    let error_for_log = last_err
                        .as_ref()
                        .map(|error| error.to_string())
                        .unwrap_or_else(|| "unknown MCP error".to_string());
                    warn!(
                        error = %error_for_log,
                        "Failed to refresh MCP tools after retries; keeping existing cache"
                    );
                    let category = last_err
                        .as_ref()
                        .map(classify_anyhow_error)
                        .unwrap_or(ErrorCategory::ExecutionError);
                    self.mcp_circuit_breaker.record_failure_category(category);
                    return Ok(());
                }
            };
            let existing_tools: Vec<String> = {
                let index = self.mcp_tool_index.read().await;
                index
                    .iter()
                    .flat_map(|(provider, names)| names.iter().map(move |name| format!("mcp::{provider}::{name}")))
                    .collect()
            };
            for canonical_name in existing_tools {
                if let Err(err) = self.inventory.remove_tool(&canonical_name) {
                    warn!(
                        tool = %canonical_name,
                        error = %err,
                        "failed to remove stale MCP proxy tool"
                    );
                }
            }

            let mut provider_map: FxHashMap<String, Vec<String>> = FxHashMap::default();
            let mut seen_tools = FxHashSet::default();

            for tool in &tools {
                let canonical_name = format!("mcp::{}::{}", tool.provider, tool.name);
                if !seen_tools.insert(canonical_name) {
                    continue;
                }
                let registration = match build_mcp_registration(Arc::clone(&mcp_client), &tool.provider, tool, None) {
                    Ok(registration) => registration,
                    Err(error) => {
                        warn!(%error, "Rejected MCP proxy metadata");
                        continue;
                    }
                };
                if let Err(error) = self.inventory.register_tool(registration) {
                    warn!(%error, "Failed to register MCP proxy tool");
                    continue;
                }
                provider_map.entry(tool.provider.clone()).or_default().push(tool.name.clone());
            }

            for tools in provider_map.values_mut() {
                tools.sort();
                tools.dedup();
            }

            *self.mcp_tool_index.write().await = provider_map;
            {
                let mut reverse_index = self.mcp_reverse_index.write().await;
                reverse_index.clear();
                let index = self.mcp_tool_index.read().await;
                for (provider, tools) in index.iter() {
                    for tool in tools {
                        reverse_index.insert(tool.clone(), provider.clone());
                    }
                }
            }

            let mcp_index = self.mcp_tool_index.read().await;
            let std_index: hashbrown::HashMap<String, Vec<String>> =
                mcp_index.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            let allowlist = {
                let gateway = self.policy_gateway.clone();
                gateway.update_mcp_tools(&std_index).await?
            };
            if let Some(allowlist) = allowlist {
                mcp_client.update_allowlist(allowlist);
            }

            *self.cached_available_tools.write() = None;
            self.rebuild_tool_assembly().await;
            self.tool_catalog_state.note_explicit_refresh("mcp_tool_refresh");
            self.sync_policy_catalog().await;
            // MP-3: Record success in circuit breaker
            self.mcp_circuit_breaker.record_success();
            Ok(())
        } else {
            debug!("No MCP client configured, nothing to refresh");
            Ok(())
        }
    }
}
