//! Tool handlers module (Codex-compatible compatibility layer)
//!
//! This module implements the handler pattern from OpenAI's Codex project,
//! providing compatibility helpers around tool execution.
//!
//! The authoritative public tool surface, name resolution, and catalog assembly
//! live in `crate::tools::registry`. Keep router/adapter changes here scoped to
//! compatibility and handler composition, not public registry policy.
//!
//! ## Key Components
//!
//! - [`tool_handler`]: Core traits and types (ToolHandler, ToolKind, ToolPayload, etc.)
//! - [`sandboxing`]: Approval, sandbox, and runtime traits (from Codex)
//! - [`tool_orchestrator`]: Approval → sandbox → attempt → retry orchestration
//! - [`orchestrator`]: Legacy compatibility shim over the active sandbox/orchestrator modules
//! - [`events`]: Event emission for tool lifecycle (begin, success, failure)
//! - [`router`]: Tool routing and dispatch (ToolRouter, DispatchRegistry, DispatchRegistryBuilder)
//! - [`adapter`]: Bidirectional adapters between ToolHandler and Tool trait
//! - [`turn_diff_tracker`]: Aggregates file diffs across patches in a turn
//! - [`mod@intercept_apply_patch`]: Shell command interception for apply_patch
//!
//! ## Handlers
//!
//! - [`apply_patch_handler`]: Apply patch tool implementation
//! - [`shell_handler`]: Shell command execution
//! - [`read_file`]: File reading with line ranges
//! - [`list_dir_handler`]: Directory listing
//!
//! ## Usage
//!
//! ```rust,ignore
//! use vtcode_core::tools::handlers::{ToolHandler, ToolInvocation, ToolOutput};
//!
//! struct MyHandler;
//!
//! #[async_trait::async_trait]
//! impl ToolHandler for MyHandler {
//!     fn kind(&self) -> ToolKind {
//!         ToolKind::Function
//!     }
//!
//!     async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, ToolCallError> {
//!         Ok(ToolOutput::simple("Done!"))
//!     }
//! }
//! ```

// Core architecture modules
pub mod adapter;
pub mod apply_patch_handler;
pub mod events;
pub mod intercept_apply_patch;
pub mod orchestrator;
pub mod router;
pub mod sandboxing;
pub mod tool_handler;
pub mod tool_orchestrator;
pub mod turn_diff_tracker;

// Handler implementations
pub mod compact;
pub mod list_dir_handler;
pub mod planning_task_tracker;
pub mod planning_workflow;
pub mod read_file;
pub mod session_tool_catalog;
mod session_tool_projection;
pub mod shell_handler;
pub mod task_tracker;
pub mod task_tracking;

// Re-export main types for convenience

// Apply patch handler
pub use apply_patch_handler::{
    ApplyPatchHandler, ApplyPatchRequest as ApplyPatchHandlerRequest, ApplyPatchRuntime, ApplyPatchToolArgs,
    create_apply_patch_freeform_tool, create_apply_patch_json_tool,
};

// Events
pub use events::{
    ExecCommandInput, ExecCommandSource, ParsedCommand, ToolEmitter, ToolEventCtx, ToolEventFailureKind, ToolEventStage,
};

// Intercept apply patch
pub use intercept_apply_patch::{
    ApplyPatchError, ApplyPatchRequest, CODEX_APPLY_PATCH_ARG, intercept_apply_patch,
    maybe_parse_apply_patch_from_command,
};

// List directory handler
pub use list_dir_handler::{DirEntry, ListDirArgs, ListDirHandler, create_list_dir_tool};

// New Sandboxing module (Codex-compatible)
pub use sandboxing::{
    Approvable, ApprovalCtx, ApprovalStore, AskForApproval, BoxFuture, CommandSpec, ExecApprovalRequirement, ExecEnv,
    ExecPolicyAmendment, ExecToolCallOutput, NetworkAccess, RejectConfig, ReviewDecision, SandboxAttempt,
    SandboxConfig, SandboxManager, SandboxOverride, SandboxPolicy, SandboxTransformError, SandboxType, Sandboxable,
    SandboxablePreference, ToolCtx, ToolError, ToolRuntime, default_exec_approval_requirement, execute_env,
    with_cached_approval,
};

// Tool Orchestrator (Codex-compatible)
pub use tool_orchestrator::ToolOrchestrator;

// Turn Diff Tracker with Agent Trace support
pub use turn_diff_tracker::{
    ChangeAttribution, FileChange, FileChangeKind, SharedTurnDiffTracker, TurnDiffTracker, new_shared_tracker,
};

// Shell handler
pub use session_tool_catalog::{
    CatalogToolKind, DeferredToolPolicy, DeferredToolSearchKind, SessionSurface, SessionToolCatalog,
    SessionToolsConfig, ToolCatalogEntry, ToolCatalogSource, ToolModelCapabilities, ToolProfile, ToolSchemaEntry,
    anthropic_native_memory_enabled_for_runtime, deferred_tool_policy_for_runtime,
};
pub use shell_handler::{ShellHandler, create_shell_tool};

// Planning workflow tools
pub use planning_workflow::{PlanningWorkflowState, StartPlanningTool};

// Task tracker (NL2Repo-Bench)
pub use planning_task_tracker::PlanningTaskTrackerTool;
pub use task_tracker::TaskTrackerTool;

// Core tool handler types
pub use router::{DispatchRegistry, DispatchRegistryBuilder, ToolCall, ToolRouter, ToolRouterProvider};
pub use tool_handler::{
    AdditionalProperties, ApprovalPolicy, ConfiguredToolSpec, Constrained, ContentItem, DiffTracker, FreeformTool,
    FreeformToolFormat, JsonSchema, McpToolResult, ResponsesApiTool, SandboxPermissions, SharedDiffTracker,
    ShellEnvironmentPolicy, ShellToolCallParams, ToolCallError, ToolHandler, ToolInvocation, ToolKind, ToolOutput,
    ToolPayload, ToolSession, ToolSpec, TurnContext,
};

// Legacy FileChange re-export for backward compatibility
pub use tool_handler::FileChange as LegacyFileChange;
