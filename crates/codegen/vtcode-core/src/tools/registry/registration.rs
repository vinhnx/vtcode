use super::ToolRegistry;
use crate::config::types::CapabilityLevel;
use crate::tool_policy::ToolPolicy;
use crate::tools::tool_intent::ToolBehavior;
use crate::tools::traits::Tool;
use futures::future::BoxFuture;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

pub type ToolExecutorFn = for<'a> fn(&'a ToolRegistry, Value) -> BoxFuture<'a, anyhow::Result<Value>>;
pub type NativeCgpToolFactory = Arc<
    dyn for<'a> Fn(&'a ToolRegistration, PathBuf, super::cgp_facade::CgpRuntimeMode) -> Arc<dyn Tool> + Send + Sync,
>;

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolCatalogSource {
    Builtin,
    Mcp,
    #[default]
    Dynamic,
}

/// Trusted registration metadata, never inferred from model-facing tool names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolNetworkAccess {
    Local,
    Network,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Default)]
pub struct ToolRegistrationSpec {
    network_access: ToolNetworkAccess,
    description: Option<String>,
    parameter_schema: Option<Value>,
    config_schema: Option<Value>,
    state_schema: Option<Value>,
    prompt_path: Option<String>,
    default_permission: Option<ToolPolicy>,
    allowlist: Vec<String>,
    denylist: Vec<String>,
    aliases: Vec<String>,
    server_hint: Option<String>,
    behavior: Option<ToolBehavior>,
}

impl ToolRegistrationSpec {
    pub fn network_access(&self) -> ToolNetworkAccess {
        self.network_access
    }
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn with_parameter_schema(mut self, schema: Value) -> Self {
        self.parameter_schema = Some(schema);
        self
    }

    pub fn with_config_schema(mut self, schema: Value) -> Self {
        self.config_schema = Some(schema);
        self
    }

    pub fn with_state_schema(mut self, schema: Value) -> Self {
        self.state_schema = Some(schema);
        self
    }

    pub fn with_prompt_path(mut self, path: impl Into<String>) -> Self {
        self.prompt_path = Some(path.into());
        self
    }

    pub fn with_permission(mut self, permission: ToolPolicy) -> Self {
        self.default_permission = Some(permission);
        self
    }

    pub fn with_allowlist(mut self, patterns: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.allowlist.extend(patterns.into_iter().map(Into::into));
        self
    }

    pub fn with_denylist(mut self, patterns: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.denylist.extend(patterns.into_iter().map(Into::into));
        self
    }

    pub fn with_aliases(mut self, aliases: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.aliases.extend(aliases.into_iter().map(Into::into));
        self
    }

    pub fn with_server_hint(mut self, hint: impl Into<String>) -> Self {
        self.server_hint = Some(hint.into());
        self
    }

    pub fn with_behavior(mut self, behavior: ToolBehavior) -> Self {
        self.behavior = Some(behavior);
        self
    }

    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    pub fn parameter_schema(&self) -> Option<&Value> {
        self.parameter_schema.as_ref()
    }

    pub fn config_schema(&self) -> Option<&Value> {
        self.config_schema.as_ref()
    }

    pub fn state_schema(&self) -> Option<&Value> {
        self.state_schema.as_ref()
    }

    pub fn prompt_path(&self) -> Option<&str> {
        self.prompt_path.as_deref()
    }

    pub fn default_permission(&self) -> Option<ToolPolicy> {
        self.default_permission.clone()
    }

    pub fn allowlist(&self) -> &[String] {
        &self.allowlist
    }

    pub fn denylist(&self) -> &[String] {
        &self.denylist
    }

    pub fn aliases(&self) -> &[String] {
        &self.aliases
    }

    pub fn server_hint(&self) -> Option<&str> {
        self.server_hint.as_deref()
    }

    pub fn behavior(&self) -> Option<ToolBehavior> {
        self.behavior
    }
}

#[derive(Clone)]
pub enum ToolHandler {
    RegistryFn(ToolExecutorFn),
    TraitObject(Arc<dyn Tool>),
}

impl fmt::Debug for ToolHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolHandler::RegistryFn(_) => write!(f, "ToolHandler::RegistryFn"),
            ToolHandler::TraitObject(_) => write!(f, "ToolHandler::TraitObject"),
        }
    }
}

#[derive(Clone)]
pub struct ToolRegistration {
    name: Arc<str>,
    capability: CapabilityLevel,
    catalog_source: ToolCatalogSource,
    uses_pty: bool,
    expose_in_llm: bool,
    deprecated: bool,
    deprecation_message: Option<String>,
    cgp_wrapped: bool,
    handler: ToolHandler,
    metadata: ToolRegistrationSpec,
    native_cgp_factory: Option<NativeCgpToolFactory>,
}

impl fmt::Debug for ToolRegistration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolRegistration")
            .field("name", &self.name)
            .field("capability", &self.capability)
            .field("catalog_source", &self.catalog_source)
            .field("uses_pty", &self.uses_pty)
            .field("expose_in_llm", &self.expose_in_llm)
            .field("deprecated", &self.deprecated)
            .field("deprecation_message", &self.deprecation_message)
            .field("cgp_wrapped", &self.cgp_wrapped)
            .field("handler", &self.handler)
            .field("metadata", &self.metadata)
            .field("has_native_cgp_factory", &self.native_cgp_factory.is_some())
            .finish()
    }
}

impl ToolRegistration {
    pub fn with_network_access(mut self, access: ToolNetworkAccess) -> Self {
        self.metadata.network_access = access;
        self
    }
    pub fn new(
        name: impl Into<Arc<str>>,
        capability: CapabilityLevel,
        uses_pty: bool,
        executor: ToolExecutorFn,
    ) -> Self {
        Self {
            name: name.into(),
            capability,
            catalog_source: ToolCatalogSource::Dynamic,
            uses_pty,
            expose_in_llm: true,
            deprecated: false,
            deprecation_message: None,
            cgp_wrapped: false,
            handler: ToolHandler::RegistryFn(executor),
            metadata: ToolRegistrationSpec::default(),
            native_cgp_factory: None,
        }
    }

    pub fn from_tool(name: impl Into<Arc<str>>, capability: CapabilityLevel, tool: Arc<dyn Tool>) -> Self {
        let mut metadata = ToolRegistrationSpec::default().with_description(tool.description());
        if let Some(schema) = tool.parameter_schema() {
            metadata = metadata.with_parameter_schema(schema);
        }
        if let Some(schema) = tool.config_schema() {
            metadata = metadata.with_config_schema(schema);
        }
        if let Some(schema) = tool.state_schema() {
            metadata = metadata.with_state_schema(schema);
        }
        if let Some(path) = tool.prompt_path() {
            metadata = metadata.with_prompt_path(path.into_owned());
        }
        metadata = metadata.with_permission(tool.default_permission());
        if let Some(patterns) = tool.allow_patterns() {
            metadata = metadata.with_allowlist(patterns.iter().copied());
        }
        if let Some(patterns) = tool.deny_patterns() {
            metadata = metadata.with_denylist(patterns.iter().copied());
        }

        Self::from_tool_with_metadata(name, capability, tool, metadata)
    }

    pub fn from_tool_with_metadata(
        name: impl Into<Arc<str>>,
        capability: CapabilityLevel,
        tool: Arc<dyn Tool>,
        metadata: ToolRegistrationSpec,
    ) -> Self {
        Self {
            name: name.into(),
            capability,
            catalog_source: ToolCatalogSource::Dynamic,
            uses_pty: false,
            expose_in_llm: true,
            deprecated: false,
            deprecation_message: None,
            cgp_wrapped: false,
            handler: ToolHandler::TraitObject(tool),
            metadata,
            native_cgp_factory: None,
        }
    }

    pub fn from_tool_instance<T>(name: impl Into<Arc<str>>, capability: CapabilityLevel, tool: T) -> Self
    where
        T: Tool + 'static,
    {
        Self::from_tool(name, capability, Arc::new(tool))
    }

    /// Register a tool wrapped with a CGP runtime context.
    ///
    /// Wraps an existing `Arc<dyn Tool>` in a CGP `ToolFacade` with the
    /// specified runtime context's approval, metadata, sandbox, logging,
    /// cache, and retry providers.
    ///
    /// Prefer `wrap_native_tool_interactive()` or `wrap_native_tool_ci()` when
    /// the caller still owns the concrete tool instance. Use this bridge when the
    /// tool already has genuine shared ownership.
    ///
    /// # Example
    /// ```rust,ignore
    /// use vtcode_core::components::{InteractiveCtx, ToolBridgeCtx, wrap_tool_interactive};
    ///
    /// let tool: Arc<dyn Tool> = Arc::new(MyTool);
    /// let reg = ToolRegistration::from_cgp_tool(
    ///     "my_tool",
    ///     CapabilityLevel::Basic,
    ///     wrap_tool_interactive(tool, workspace_root),
    /// );
    /// ```
    pub fn from_cgp_tool<Ctx>(
        name: impl Into<Arc<str>>,
        capability: CapabilityLevel,
        facade: crate::components::ToolFacade<Ctx>,
    ) -> Self
    where
        crate::components::ToolFacade<Ctx>: Tool + 'static,
    {
        Self::from_tool_instance(name, capability, facade).with_cgp_wrapped(true)
    }

    pub fn with_llm_visibility(mut self, expose: bool) -> Self {
        self.expose_in_llm = expose;
        self
    }

    pub fn with_catalog_source(mut self, catalog_source: ToolCatalogSource) -> Self {
        self.catalog_source = catalog_source;
        self
    }

    pub fn with_pty(mut self, uses_pty: bool) -> Self {
        self.uses_pty = uses_pty;
        self
    }

    pub fn with_deprecated(mut self, deprecated: bool) -> Self {
        self.deprecated = deprecated;
        self
    }

    pub fn with_deprecation_message(mut self, message: impl Into<String>) -> Self {
        self.deprecation_message = Some(message.into());
        self
    }

    pub fn with_cgp_wrapped(mut self, wrapped: bool) -> Self {
        self.cgp_wrapped = wrapped;
        self
    }

    pub fn with_handler(mut self, handler: ToolHandler) -> Self {
        self.handler = handler;
        self
    }

    pub fn with_native_cgp_factory(mut self, factory: NativeCgpToolFactory) -> Self {
        self.native_cgp_factory = Some(factory);
        self
    }

    pub fn name(&self) -> &str {
        self.name.as_ref()
    }

    /// Returns the capability level of this tool.
    pub fn capability(&self) -> CapabilityLevel {
        self.capability
    }

    /// Returns the catalog source of this tool.
    pub fn catalog_source(&self) -> ToolCatalogSource {
        self.catalog_source
    }

    /// Returns whether this tool uses a PTY.
    pub fn uses_pty(&self) -> bool {
        self.uses_pty
    }

    /// Returns whether this tool is visible to the LLM.
    pub fn expose_in_llm(&self) -> bool {
        self.expose_in_llm
    }

    /// Returns whether this tool is deprecated.
    pub fn is_deprecated(&self) -> bool {
        self.deprecated
    }

    /// Returns the deprecation message if this tool is deprecated.
    pub fn deprecation_message(&self) -> Option<&str> {
        self.deprecation_message.as_deref()
    }

    /// Returns whether this tool is CGP-wrapped.
    pub fn is_cgp_wrapped(&self) -> bool {
        self.cgp_wrapped
    }

    /// Returns the tool handler implementation.
    pub fn handler(&self) -> ToolHandler {
        self.handler.clone()
    }

    /// Returns the native CGP factory if set.
    pub fn native_cgp_factory(&self) -> Option<NativeCgpToolFactory> {
        self.native_cgp_factory.clone()
    }

    /// Returns a reference to the tool registration metadata.
    pub fn metadata(&self) -> &ToolRegistrationSpec {
        &self.metadata
    }

    /// Returns the parameter JSON schema (delegates to metadata).
    pub fn parameter_schema(&self) -> Option<&Value> {
        self.metadata.parameter_schema()
    }

    /// Returns the configuration JSON schema (delegates to metadata).
    pub fn config_schema(&self) -> Option<&Value> {
        self.metadata.config_schema()
    }

    /// Returns the state JSON schema (delegates to metadata).
    pub fn state_schema(&self) -> Option<&Value> {
        self.metadata.state_schema()
    }

    /// Returns the prompt file path (delegates to metadata).
    pub fn prompt_path(&self) -> Option<&str> {
        self.metadata.prompt_path()
    }

    /// Returns the default permission policy (delegates to metadata).
    pub fn default_permission(&self) -> Option<ToolPolicy> {
        self.metadata.default_permission()
    }

    /// Replaces the tool registration metadata.
    pub fn with_metadata(mut self, metadata: ToolRegistrationSpec) -> Self {
        self.metadata = metadata;
        self
    }

    /// Sets the tool description (delegates to metadata).
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.metadata = self.metadata.with_description(description);
        self
    }

    /// Sets the prompt file path (delegates to metadata).
    pub fn with_prompt_path(mut self, path: impl Into<String>) -> Self {
        self.metadata = self.metadata.with_prompt_path(path);
        self
    }

    /// Sets the parameter JSON schema (delegates to metadata).
    pub fn with_parameter_schema(mut self, schema: Value) -> Self {
        self.metadata = self.metadata.with_parameter_schema(schema);
        self
    }

    /// Sets the configuration JSON schema (delegates to metadata).
    pub fn with_config_schema(mut self, schema: Value) -> Self {
        self.metadata = self.metadata.with_config_schema(schema);
        self
    }

    /// Sets the state JSON schema (delegates to metadata).
    pub fn with_state_schema(mut self, schema: Value) -> Self {
        self.metadata = self.metadata.with_state_schema(schema);
        self
    }

    /// Sets the default permission policy (delegates to metadata).
    pub fn with_permission(mut self, permission: ToolPolicy) -> Self {
        self.metadata = self.metadata.with_permission(permission);
        self
    }

    /// Adds glob patterns to the allowlist (delegates to metadata).
    pub fn with_allowlist(mut self, patterns: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.metadata = self.metadata.with_allowlist(patterns);
        self
    }

    /// Adds glob patterns to the denylist (delegates to metadata).
    pub fn with_denylist(mut self, patterns: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.metadata = self.metadata.with_denylist(patterns);
        self
    }

    /// Adds alternative names for the tool (delegates to metadata).
    pub fn with_aliases(mut self, aliases: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.metadata = self.metadata.with_aliases(aliases);
        self
    }

    /// Sets the MCP server hint (delegates to metadata).
    pub fn with_server_hint(mut self, hint: impl Into<String>) -> Self {
        self.metadata = self.metadata.with_server_hint(hint);
        self
    }

    /// Sets the tool behavior classification (delegates to metadata).
    pub fn with_behavior(mut self, behavior: ToolBehavior) -> Self {
        self.metadata = self.metadata.with_behavior(behavior);
        self
    }
}
