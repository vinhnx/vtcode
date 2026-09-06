//! Codex-compatible ToolHandler trait and types
//!
//! This module implements the handler pattern from OpenAI's Codex project,
//! providing a more modular and composable approach to tool execution.
//!
//! Key patterns from Codex:
//! - `ToolHandler` trait with kind/matches_kind/is_mutating/handle methods
//! - `ToolKind` enum for categorizing tool types
//! - `ToolPayload` for typed tool arguments
//! - `ToolOutput` for structured tool results
//! - `ToolInvocation` for execution context

use crate::config::constants::tools;
use hashbrown::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub use vtcode_utility_tool_specs::{
    AdditionalProperties, FreeformTool, FreeformToolFormat, JsonSchema, ResponsesApiTool,
};

/// Tool kind classification (from Codex)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToolKind {
    /// Standard function call tool
    Function,
    /// MCP (Model Context Protocol) tool
    Mcp,
    /// Custom/freeform tool (e.g., apply_patch with custom format)
    Custom,
}

/// Payload types for tool invocations (from Codex)
#[derive(Clone, Debug)]
pub enum ToolPayload {
    /// Standard function call with JSON arguments
    Function { arguments: String },
    /// Custom tool with freeform input (e.g., apply_patch)
    Custom { input: String },
    /// MCP tool call
    Mcp { arguments: Option<Value> },
    /// Local shell execution
    LocalShell { params: ShellToolCallParams },
}

/// Shell command parameters (from Codex)
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ShellToolCallParams {
    pub command: Vec<String>,
    pub workdir: Option<String>,
    pub timeout_ms: Option<u64>,
    pub sandbox_permissions: Option<SandboxPermissions>,
    pub justification: Option<String>,
}

// Re-export the canonical SandboxPermissions from the sandboxing module.
pub use crate::sandboxing::SandboxPermissions;

/// Tool output types (from Codex)
#[derive(Clone, Debug)]
pub enum ToolOutput {
    /// Function call result
    Function {
        content: String,
        content_items: Option<Vec<ContentItem>>,
        success: Option<bool>,
    },
    /// MCP tool result
    Mcp { result: McpToolResult },
}

impl ToolOutput {
    /// Create a simple function output with just content
    pub fn simple(content: impl Into<String>) -> Self {
        Self::Function {
            content: content.into(),
            content_items: None,
            success: Some(true),
        }
    }

    /// Create a function output with success status
    pub fn with_success(content: impl Into<String>, success: bool) -> Self {
        Self::Function {
            content: content.into(),
            content_items: None,
            success: Some(success),
        }
    }

    /// Create an error output
    pub fn error(message: impl Into<String>) -> Self {
        Self::Function {
            content: message.into(),
            content_items: None,
            success: Some(false),
        }
    }

    /// Get the content string if this is a Function output
    pub fn content(&self) -> Option<&str> {
        match self {
            Self::Function { content, .. } => Some(content),
            Self::Mcp { result } => result.content.first().and_then(|c| c.as_text()),
        }
    }

    /// Check if the output indicates success
    pub fn is_success(&self) -> bool {
        match self {
            Self::Function { success, .. } => success.unwrap_or(true),
            Self::Mcp { result } => !result.is_error.unwrap_or(false),
        }
    }
}

/// Content item for multi-part responses (from Codex)
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentItem {
    Text { text: String },
    Image { data: String, mime_type: String },
    Resource { uri: String, mime_type: Option<String> },
}

impl ContentItem {
    pub fn as_text(&self) -> Option<&str> {
        match self {
            ContentItem::Text { text } => Some(text),
            _ => None,
        }
    }
}

/// MCP tool result (from Codex)
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpToolResult {
    pub content: Vec<ContentItem>,
    pub is_error: Option<bool>,
}

/// Context for tool invocation (from Codex)
pub struct ToolInvocation {
    pub session: Arc<dyn ToolSession>,
    pub turn: Arc<TurnContext>,
    pub tracker: Option<SharedDiffTracker>,
    pub call_id: String,
    pub tool_name: String,
    pub payload: ToolPayload,
}

/// Shared diff tracker type alias
pub type SharedDiffTracker = Arc<tokio::sync::Mutex<DiffTracker>>;

/// Lightweight wrapper used to preserve policy fields as structured values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Constrained<T> {
    value: T,
}

impl<T> Constrained<T> {
    pub fn allow_any(initial_value: T) -> Self {
        Self { value: initial_value }
    }

    pub fn get(&self) -> &T {
        &self.value
    }
}

// Deref is for ergonomic read-only access to the inner value.
// The private field + `get()` pattern remains for intentional
// construction barriers; Deref enables transparent use in contexts
// that expect &T (e.g., matching on policy enums).
impl<T> std::ops::Deref for Constrained<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl<T: Copy> Constrained<T> {
    pub fn value(&self) -> T {
        self.value
    }
}

impl<T: Default> Default for Constrained<T> {
    fn default() -> Self {
        Self::allow_any(T::default())
    }
}

/// Session trait for tool execution context
#[async_trait]
pub trait ToolSession: Send + Sync {
    /// Get the current working directory
    fn cwd(&self) -> &PathBuf;

    /// Get workspace root
    fn workspace_root(&self) -> &PathBuf;

    /// Record a warning message
    async fn record_warning(&self, message: String);

    /// Get user's configured shell
    fn user_shell(&self) -> &str;
}

/// Turn context for tool execution
#[derive(Clone, Debug)]
pub struct TurnContext {
    pub cwd: PathBuf,
    pub turn_id: String,
    pub sub_id: Option<String>,
    pub shell_environment_policy: ShellEnvironmentPolicy,
    pub approval_policy: Constrained<ApprovalPolicy>,
    pub codex_linux_sandbox_exe: Option<PathBuf>,
    /// Sandbox policy from Codex (for orchestrator integration)
    pub sandbox_policy: Constrained<super::sandboxing::SandboxConfig>,
}

impl TurnContext {
    /// Resolve a path relative to the current working directory
    pub fn resolve_path(&self, path: Option<String>) -> PathBuf {
        self.resolve_path_ref(path.as_deref())
    }

    /// Resolve a path reference relative to the current working directory
    pub fn resolve_path_ref(&self, path: Option<&str>) -> PathBuf {
        match path {
            Some(p) => {
                let path = PathBuf::from(p);
                if path.is_absolute() { path } else { self.cwd.join(path) }
            }
            None => self.cwd.clone(),
        }
    }
}

/// Shell environment policy
#[derive(Clone, Debug, Default)]
pub enum ShellEnvironmentPolicy {
    #[default]
    Inherit,
    Clean,
    Custom(HashMap<String, String>),
}

/// Approval policy for tool execution
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ApprovalPolicy {
    #[default]
    Never,
    OnMutation,
    Always,
}

/// Diff tracker for file changes
#[derive(Default, Debug)]
pub struct DiffTracker {
    pub changes: HashMap<PathBuf, FileChange>,
}

impl DiffTracker {
    pub fn on_patch_begin(&mut self, changes: &HashMap<PathBuf, FileChange>) {
        self.changes.extend(changes.clone());
    }

    pub fn on_patch_end(&mut self, success: bool) {
        if !success {
            self.changes.clear();
        }
    }
}

/// File change types (from Codex protocol)
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FileChange {
    Add { content: String },
    Delete,
    Update { old_content: String, new_content: String },
    Rename { new_path: PathBuf, content: Option<String> },
}

/// Error type for tool execution (from Codex)
#[derive(Debug, thiserror::Error)]
pub enum ToolCallError {
    /// Error that should be sent back to the model
    #[error("Tool error: {0}")]
    RespondToModel(String),

    /// Internal error that should not be sent to the model
    #[error("Internal error: {0}")]
    Internal(#[from] anyhow::Error),

    /// Tool was rejected by approval policy
    #[error("Tool rejected: {0}")]
    Rejected(String),

    /// Tool timed out
    #[error("Tool timed out after {0}ms")]
    Timeout(u64),
}

impl ToolCallError {
    /// Create an error to respond to the model
    pub fn respond(message: impl Into<String>) -> Self {
        Self::RespondToModel(message.into())
    }
}

impl From<super::sandboxing::ToolError> for ToolCallError {
    fn from(err: super::sandboxing::ToolError) -> Self {
        match err {
            super::sandboxing::ToolError::Rejected(msg) => ToolCallError::Rejected(msg),
            super::sandboxing::ToolError::Codex(e) => ToolCallError::Internal(e),
            super::sandboxing::ToolError::SandboxDenied(msg) => {
                ToolCallError::Rejected(format!("Sandbox denied: {msg}"))
            }
            super::sandboxing::ToolError::Timeout(ms) => ToolCallError::Timeout(ms),
        }
    }
}

/// Core trait for tool handlers (from Codex)
///
/// This trait provides a modular approach to tool execution, separating
/// concerns like kind matching, mutation detection, and actual execution.
#[async_trait]
pub trait ToolHandler: Send + Sync {
    /// Get the kind of tool this handler supports
    fn kind(&self) -> ToolKind;

    /// Check if the handler can process the given payload type
    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(
            (self.kind(), payload),
            (ToolKind::Function, ToolPayload::Function { .. })
                | (ToolKind::Mcp, ToolPayload::Mcp { .. })
                | (ToolKind::Custom, ToolPayload::Custom { .. })
        )
    }

    /// Check if this invocation would mutate state
    ///
    /// Used for approval policies - read-only tools can often be auto-approved
    async fn is_mutating(&self, _invocation: &ToolInvocation) -> bool {
        false
    }

    /// Execute the tool and return the output
    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, ToolCallError>;
}

/// Tool spec types (from Codex)
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolSpec {
    Function(ResponsesApiTool),
    Freeform(FreeformTool),
    WebSearch {},
    LocalShell {},
}

impl ToolSpec {
    pub fn name(&self) -> &str {
        match self {
            ToolSpec::Function(tool) => &tool.name,
            ToolSpec::Freeform(tool) => &tool.name,
            ToolSpec::WebSearch {} => tools::WEB_SEARCH,
            ToolSpec::LocalShell {} => "local_shell",
        }
    }
}

/// Configured tool spec with parallel execution support
#[derive(Clone, Debug)]
pub struct ConfiguredToolSpec {
    pub spec: ToolSpec,
    pub supports_parallel_tool_calls: bool,
}

impl ConfiguredToolSpec {
    pub fn new(spec: ToolSpec, supports_parallel: bool) -> Self {
        Self {
            spec,
            supports_parallel_tool_calls: supports_parallel,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_output_simple() {
        let output = ToolOutput::simple("Hello, world!");
        assert!(output.is_success());
        assert_eq!(output.content(), Some("Hello, world!"));
    }

    #[test]
    fn test_tool_output_error() {
        let output = ToolOutput::error("Something went wrong");
        assert!(!output.is_success());
        assert_eq!(output.content(), Some("Something went wrong"));
    }

    #[test]
    fn test_sandbox_permissions_default() {
        let perms = SandboxPermissions::default();
        assert_eq!(perms, SandboxPermissions::UseDefault);
    }

    #[test]
    fn test_turn_context_resolve_path_absolute() {
        let ctx = TurnContext {
            cwd: PathBuf::from("/workspace"),
            turn_id: "test".to_string(),
            sub_id: None,
            shell_environment_policy: ShellEnvironmentPolicy::default(),
            approval_policy: Constrained::allow_any(ApprovalPolicy::default()),
            codex_linux_sandbox_exe: None,
            sandbox_policy: Constrained::allow_any(Default::default()),
        };

        let resolved = ctx.resolve_path(Some("/absolute/path".to_string()));
        assert_eq!(resolved, PathBuf::from("/absolute/path"));
    }

    #[test]
    fn test_turn_context_resolve_path_relative() {
        let ctx = TurnContext {
            cwd: PathBuf::from("/workspace"),
            turn_id: "test".to_string(),
            sub_id: None,
            shell_environment_policy: ShellEnvironmentPolicy::default(),
            approval_policy: Constrained::allow_any(ApprovalPolicy::default()),
            codex_linux_sandbox_exe: None,
            sandbox_policy: Constrained::allow_any(Default::default()),
        };

        let resolved = ctx.resolve_path(Some("relative/path".to_string()));
        assert_eq!(resolved, PathBuf::from("/workspace/relative/path"));
    }
}
