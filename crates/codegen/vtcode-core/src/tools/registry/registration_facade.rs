//! Registration-related ToolRegistry helpers.

use anyhow::Result;

use super::{ToolRegistration, ToolRegistry};

impl ToolRegistry {
    /// Resolve trusted network metadata through the same alias inventory as execution.
    pub fn tool_network_access(&self, name: &str) -> super::ToolNetworkAccess {
        self.inventory
            .registration_for(name)
            .map(|registration| registration.metadata().network_access())
            .unwrap_or_default()
    }
    /// Register or replace a tool with the registry.
    ///
    /// # Arguments
    /// * `registration` - The tool registration to add
    ///
    /// # Returns
    /// `Result<()>` indicating success. A duplicate canonical tool name replaces
    /// the previous registration in place; alias conflicts remain errors.
    pub async fn register_tool(&self, registration: ToolRegistration) -> Result<()> {
        let registration = if let Some(mode) = self.current_cgp_mode() {
            if registration.is_cgp_wrapped() {
                registration
            } else if let Some(handler) = self.cgp_handler_for_registration(&registration, mode) {
                registration.with_handler(handler).with_cgp_wrapped(true)
            } else {
                registration
            }
        } else {
            registration
        };
        self.inventory.register_tool(registration)?;
        // Invalidate cache
        *self.cached_available_tools.write() = None;
        self.rebuild_tool_assembly().await;
        self.tool_catalog_state.note_explicit_refresh("tool_registration");
        self.sync_policy_catalog().await;
        Ok(())
    }

    /// Unregister a tool from the registry.
    pub async fn unregister_tool(&self, name: &str) -> Result<bool> {
        let removed = self.inventory.remove_tool(name)?.is_some();
        if removed {
            *self.cached_available_tools.write() = None;
            self.rebuild_tool_assembly().await;
            self.tool_catalog_state.note_explicit_refresh("tool_unregistration");
            self.sync_policy_catalog().await;
        }
        Ok(removed)
    }
}
