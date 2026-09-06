//! # Tool System Architecture
//!
//! This module provides a modular, composable architecture for VT Code agent tools,
//! implementing a registry-based system for tool discovery, execution, and management.
//!
//! ## Architecture Overview
//!
//! The tool system is designed around several key principles:
//!
//! - **Modularity**: Each tool is a focused, reusable component
//! - **Registry Pattern**: Centralized tool registration and discovery
//! - **Policy-Based Execution**: Configurable execution policies and safety checks
//! - **Type Safety**: Strong typing for tool parameters and results
//! - **Async Support**: Full async/await support for all tool operations
//!
//! ## Core Components
//!
//! ### Tool Registry
//! ```rust,ignore
//! use vtcode_core::tools::{ToolRegistry, ToolRegistration};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let workspace = std::env::current_dir()?;
//!     let mut registry = ToolRegistry::new(workspace);
//!
//!     // Register a custom tool
//!     let tool = ToolRegistration {
//!         name: "my_tool".to_string(),
//!         description: "A custom tool".to_string(),
//!         parameters: serde_json::json!({"type": "object"}),
//!         handler: |args| async move {
//!             Ok(serde_json::json!({"result": "success"}))
//!         },
//!     };
//!
//!     registry.register_tool(tool).await?;
//!     Ok(())
//! }
//! ```
//!
//! ### Tool Categories
//!
//! #### File Operations
//! - **File Operations**: Read, write, create, delete files
//! - **Search Tools**: grep_file with ripgrep for fast regex-based pattern matching, glob patterns, type filtering
//! - **Cache Management**: File caching and performance optimization
//!
//! #### Terminal Integration
//! - **Bash Tools**: Shell command execution
//! - **PTY Support**: Full terminal emulation
//! - **Command Policies**: Safety and execution controls
//!
//! #### Code Analysis
//! ## Tool Execution
//!
//! ```rust,ignore
//! use vtcode_core::tools::ToolRegistry;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let mut registry = ToolRegistry::new(std::env::current_dir()?);
//!
//!     // Execute a tool
//!     let args = serde_json::json!({"path": "."});
//!     let result = registry.execute_tool("list_files", args).await?;
//!
//!     println!("Result: {}", result);
//!     Ok(())
//! }
//! ```
//!
//! ## Safety & Policies
//!
//! The tool system includes comprehensive safety features:
//!
//! - **Path Validation**: All file operations check workspace boundaries
//! - **Command Policies**: Configurable allow/deny lists for terminal commands
//! - **Execution Limits**: Timeout and resource usage controls
//! - **Audit Logging**: Complete trail of tool executions
//!
//! ## Custom Tool Development
//!
//! ```rust,ignore
//! use vtcode_core::tools::traits::Tool;
//! use serde_json::Value;
//!
//! struct MyCustomTool;
//!
//! #[async_trait::async_trait]
//! impl Tool for MyCustomTool {
//!     async fn execute(&self, args: Value) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
//!         // Tool implementation
//!         Ok(serde_json::json!({"status": "completed"}))
//!     }
//!
//!     fn name(&self) -> &str {
//!         "my_custom_tool"
//!     }
//!
//!     fn description(&self) -> &str {
//!         "A custom tool for specific tasks"
//!     }
//!
//!     fn parameters(&self) -> Value {
//!         serde_json::json!({
//!             "type": "object",
//!             "properties": {
//!                 "input": {"type": "string"}
//!             }
//!         })
//!     }
//! }
//! ```
//!
//! Modular tool system for VT Code
//!
//! This module provides a composable architecture for agent tools, breaking down
//! the monolithic implementation into focused, reusable components.

pub mod apply_patch;
pub use apply_patch::mutation_target_paths;
pub mod ast_grep_binary;
pub mod ast_grep_installer;
pub(crate) mod ast_grep_language;
pub mod builder;
pub mod constants;
pub mod error_messages;
pub(crate) mod rate_limit_config;
pub mod request_user_input;

pub mod autonomous_executor;
pub mod cache;
pub(crate) mod code_search;
pub use code_search::normalised_identity as normalised_code_search_identity;
pub use code_search::normalised_loop_identity as normalised_code_search_loop_identity;
pub use code_search::normalised_path as normalised_code_search_path;
pub use code_search::scope_contains_mutated_path as code_search_scope_contains_mutated_path;
pub mod command;
pub mod command_args;
pub mod command_cache;
pub mod command_policy;
pub mod command_resolver;
pub mod continuation;
pub mod edited_file_monitor;
pub mod editing;
pub mod error_helpers;
pub mod exec_session;
pub mod exec_session_id;
pub mod execution_context;
pub mod execution_tracker;
pub mod file_ops;
pub mod file_search_bridge;
pub mod file_search_rpc;
pub mod file_tracker;
pub mod generation_helpers;
mod grep_backend;
pub mod grep_cache;
pub mod grep_file;
pub mod handlers;
pub mod invocation;
pub mod mcp;
pub mod names;
pub mod native_memory;
pub(crate) mod output_limits;
pub mod path_env;
pub mod plugins;
pub mod pty;
pub mod read_limits;
pub mod resilience;

pub use resilience::rate_limiter;
pub mod defuddle;
pub mod outline_search;
pub mod registry;
pub mod result;
pub mod result_cache;
pub mod result_metadata;
pub mod ripgrep_binary;
pub mod ripgrep_installer;
pub mod safety_gateway;
pub mod search_metrics;
pub(crate) mod search_runtime;
pub mod shell;
pub mod shell_snapshot;
pub mod skills;
pub mod summarizers;
#[cfg(feature = "tui")]
pub mod terminal_app;
pub mod tool_effectiveness;
pub mod tool_intent;
pub mod traits;
pub(crate) mod tree_sitter_runtime;
pub mod types;
pub mod validation;
pub mod validation_cache;
pub mod web_fetch;
pub mod web_search;

// Production-grade improvements modules
pub use resilience::adaptive_rate_limiter;
pub mod async_middleware;
pub mod cached_executor;
pub mod lru_cache;
pub mod pattern_detection;
pub mod registry_adapters;
pub mod time_compat;
pub mod tool_middleware;
pub mod workflow_optimizer;
pub use resilience::circuit_breaker;
pub mod health;
pub mod improvement_algorithms;
pub mod improvements_config;
pub mod improvements_errors;
pub mod improvements_registry_ext;
mod install_support;
pub mod optimized_registry;
pub mod output_spooler;
pub mod pattern_engine;
pub mod request_response;
pub mod unified_error;
pub mod untrusted_data;

/// Internal helper IDs for apply_patch and tracker constructors.
pub const CREATE_APPLY_PATCH_FREEFORM_TOOL_ID: &str = "create_apply_patch_freeform_tool";
pub const CREATE_APPLY_PATCH_JSON_TOOL_ID: &str = "create_apply_patch_json_tool";
pub const INTERCEPT_APPLY_PATCH_ID: &str = "intercept_apply_patch";
pub const NEW_SHARED_TRACKER_ID: &str = "new_shared_tracker";

// Re-export main types and traits for backward compatibility
pub use ast_grep_installer::{AstGrepInstallOutcome, AstGrepStatus};
pub use autonomous_executor::{AutonomousExecutor, AutonomousPolicy};
pub use cache::FileCache;
pub use command_cache::PermissionCache;
pub use command_resolver::CommandResolver;
pub use editing::{Patch, PatchError, PatchHunk, PatchLine, PatchOperation};
pub use exec_session_id::ExecSessionId;
pub use execution_context::{ToolExecutionContext, ToolExecutionRecord, ToolPattern};
pub use execution_tracker::{ExecutionRecord, ExecutionStats, ExecutionStatus, ExecutionTracker};
pub use file_search_rpc::{
    FileMatchRpc, FileSearchRpcHandler, ListFilesRequest, ListFilesResponse, RpcError, RpcRequest, RpcResponse,
    SearchFilesRequest, SearchFilesResponse,
};
pub use grep_file::GrepSearchManager;
pub use invocation::{InvocationBuilder, ToolInvocation as UnifiedToolInvocation, ToolInvocationId};
pub use search_runtime::{
    SearchToolBundleStatus, SearchToolReadiness, dominant_workspace_language, search_tool_bundle_status,
};

pub use defuddle::DefuddleTool;
pub use optimized_registry::{CachedToolMetadata as OptimizedToolMetadata, OptimizedToolRegistry};
pub use plugins::{PluginHandle, PluginId, PluginInstaller, PluginManifest, PluginRuntime};
pub use pty::{PtyCommandRequest, PtyCommandResult, PtyManager};
pub use registry::{
    ApprovalPattern, ApprovalRecorder, CgpRuntimeMode, JustificationExtractor, JustificationManager, RiskLevel,
    ToolJustification, ToolRegistration, ToolRegistry, ToolRiskContext, ToolRiskScorer, ToolSource, WorkspaceTrust,
    native_cgp_tool_factory, wrap_registered_native_tool,
};
pub use request_response::{ToolCallRequest, ToolCallResponse};
pub use result::{TokenCounts, ToolMetadata, ToolMetadataBuilder, ToolResult as SplitToolResult};
pub use result_cache::{ToolCacheKey, ToolResultCache};
pub use result_metadata::{EnhancedToolResult, ResultCompleteness, ResultMetadata, ResultScorer, ScorerRegistry};
pub use ripgrep_installer::RipgrepStatus;
pub use safety_gateway::{
    SafetyCheckResult, SafetyContext, SafetyDecision, SafetyError, SafetyGateway, SafetyGatewayConfig, SafetyStats,
    SafetyTrustLevel,
};
pub use search_metrics::{SearchMetric, SearchMetrics, SearchMetricsStats};
pub use shell_snapshot::{
    FileFingerprint, ShellKind, ShellSnapshot, ShellSnapshotManager, SnapshotStats, apply_snapshot_env,
    global_snapshot_manager,
};
pub use tool_effectiveness::{
    AdaptiveToolSelector, ToolEffectiveness, ToolEffectivenessTracker, ToolFailureMode, ToolSelectionContext,
    ToolSelector,
};
pub use traits::{Tool, ToolExecutor};
pub use types::*;
pub use unified_error::{
    DebugContext as UnifiedToolDebugContext, ErrorSeverity as UnifiedErrorSeverity, UnifiedErrorKind, UnifiedToolError,
};
pub use web_fetch::WebFetchTool;
pub use web_search::WebSearchTool;

// Dynamic context discovery
pub use output_spooler::{SpoolResult, SpooledOutputReference, SpoolerConfig, ToolOutputSpooler};

// Production-grade improvements re-exports
pub use async_middleware::{
    AsyncCachingMiddleware, AsyncLoggingMiddleware, AsyncMiddleware, AsyncMiddlewareChain, AsyncRetryMiddleware,
    MiddlewareToolResult, ToolRequest as MiddlewareToolRequest,
};
pub use cached_executor::{CachedToolExecutor, ExecutorStats};
pub use handlers::{
    // Apply patch handler
    ApplyPatchHandler,
    ApplyPatchRequest,
    ApplyPatchRuntime,
    ApplyPatchToolArgs,
    // Orchestrator and sandboxing
    Approvable,
    // Core handler traits and types
    ApprovalPolicy,
    AskForApproval,
    // Turn diff tracker with Agent Trace support
    ChangeAttribution,
    CommandSpec,
    ConfiguredToolSpec,
    ContentItem,
    DiffTracker,
    ExecApprovalRequirement,
    // Event emission
    ExecCommandInput,
    ExecCommandSource,
    ExecEnv,
    ExecPolicyAmendment,
    ExecToolCallOutput,
    FileChange,
    FileChangeKind,
    FreeformTool,
    FreeformToolFormat,
    JsonSchema as ToolJsonSchema,
    McpToolResult,
    ParsedCommand,
    RejectConfig,
    ResponsesApiTool,
    SandboxAttempt,
    SandboxConfig,
    SandboxManager,
    SandboxPermissions,
    SandboxPolicy,
    SandboxTransformError,
    Sandboxable,
    SandboxablePreference,
    SharedDiffTracker,
    SharedTurnDiffTracker,
    ShellEnvironmentPolicy,
    ShellToolCallParams,
    ToolCallError,
    ToolCtx,
    ToolEmitter,
    ToolError,
    ToolEventCtx,
    ToolEventFailureKind,
    ToolEventStage,
    ToolHandler,
    ToolInvocation,
    ToolKind,
    ToolOrchestrator,
    ToolOutput,
    ToolPayload,
    ToolRuntime,
    ToolSession,
    ToolSpec,
    TurnContext,
    TurnDiffTracker,
    create_apply_patch_freeform_tool,
    create_apply_patch_json_tool,
    default_exec_approval_requirement,
    intercept_apply_patch,
    new_shared_tracker,
};
pub use improvement_algorithms::{
    MLScoreComponents, PatternState, TimeDecayedScore, ToolCallRecord, detect_pattern, jaro_winkler_similarity,
};
pub use improvements_config::{
    CacheConfig, ContextConfig, FallbackConfig, ImprovementsConfig, PatternConfig, SimilarityConfig, TimeDecayConfig,
};
pub use improvements_errors::{
    ErrorKind, EventType, ImprovementError, ImprovementEvent, ImprovementResult, ImprovementSeverity,
    ImprovementSeverity as ErrorSeverity, ObservabilityContext, ObservabilitySink,
};
pub use improvements_registry_ext::{ToolMetrics, ToolRegistryImprovement};
pub use lru_cache::{CacheObserver, CacheStats, LruCache};
pub use pattern_detection::{DetectedPattern, PatternDetector};
pub use tool_middleware::{Middleware, MiddlewareChain};
pub use vtcode_utility_tool_specs::parse_tool_input_schema;
pub use workflow_optimizer::{Optimization, OptimizationType, WorkflowOptimizer};
