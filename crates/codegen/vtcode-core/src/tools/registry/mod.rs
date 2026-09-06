//! Tool registry and function declarations

mod approval_recorder;
mod assembly;
mod availability_facade;
mod builder;
mod builtins;
mod cache;
mod catalog_facade;
mod cgp_facade;
mod circuit_breaker;
mod commands_facade;
mod config_helpers;
mod distributed;
mod dual_output;
mod error;
mod execution_facade;
mod execution_history;
mod execution_kernel;
mod execution_request;
mod execution_stages;
mod executors;
pub mod file_helpers;
mod file_monitor_facade;
mod harness;
mod harness_facade;
mod history_facade;
pub mod interfaces;
mod inventory;
mod inventory_facade;
mod justification;
mod justification_extractor;
pub mod labels;
mod maintenance;
mod mcp_facade;
mod mcp_helpers;
mod metrics_facade;
mod optimization_facade;
mod output_processing;
mod pack;
mod pack_impls;
mod planning_workflow_checks;
mod planning_workflow_facade;
mod policy;
mod policy_facade;
mod progress_facade;
mod pty;
mod pty_facade;
mod registration;
mod registration_facade;
mod resiliency;
mod resiliency_facade;
mod risk_scorer;
mod runtime_config_facade;
mod sandbox_facade;
mod scheduler_facade;
mod search_runtime_facade;
mod shell_policy;
mod shell_policy_facade;
mod spool_processing;
mod spooler_facade;
mod subagent_facade;
mod telemetry;
mod timeout;
mod timeout_category;
mod timeout_facade;
mod tool_catalog_facade;
mod tool_executor_impl;
mod tool_search_index;
mod trait_impls;
mod unified_actions;
mod utils;

pub use approval_recorder::ApprovalRecorder;
pub use cgp_facade::CgpRuntimeMode;
pub use cgp_facade::native_cgp_tool_factory;
pub use cgp_facade::wrap_registered_native_tool;
pub use error::{ToolErrorType, ToolExecutionError};
pub use execution_history::{
    HarnessContextSnapshot, ToolExecutionHistory, ToolExecutionRecord, ToolTaskTelemetrySnapshot,
};
pub use execution_kernel::ToolPreflightOutcome;
pub use execution_request::{ExecSettlementMode, ExecutionPolicySnapshot, ToolExecutionOutcome, ToolExecutionRequest};
pub use executors::sandbox_policy_from_runtime_config;
pub use harness::HarnessContext;
pub use justification::{ApprovalPattern, JustificationManager, ToolJustification};
pub use justification_extractor::JustificationExtractor;
pub use pty::{PtySessionGuard, PtySessionManager};
pub use registration::{
    NativeCgpToolFactory, ToolCatalogSource, ToolExecutorFn, ToolHandler, ToolRegistration,
    ToolRegistrationSpec as ToolMetadata,
};
pub use resiliency::{ResiliencyContext, ToolFailureTracker};
pub use risk_scorer::{RiskLevel, ToolRiskContext, ToolRiskScorer, ToolSource, WorkspaceTrust};
pub use shell_policy::ShellPolicyChecker;
pub use telemetry::ToolTelemetryEvent;
pub use timeout::{AdaptiveTimeoutTuning, ToolLatencyStats, ToolTimeoutCategory, ToolTimeoutPolicy};
pub use tool_catalog_facade::{SessionToolCatalogState, ToolGroup, tool_groups};

// Re-export trait interfaces for external consumers.
pub use interfaces::{
    McpBridge, PtySessionControl, SharedRegistry, ToolCatalog, ToolMetrics, ToolRegistryApi, ToolResilience,
    ToolSecurity,
};

use assembly::ToolAssembly;
use inventory::ToolInventory;
use policy::ToolPolicyGateway;
use utils::normalize_tool_output;

use crate::tool_policy::ToolPolicy;
use crate::tools::exec_session::ExecSessionManager;
use crate::tools::handlers::PlanningWorkflowState;
pub(super) use crate::tools::pty::PtyManager;
use crate::tools::result::ToolResult as SplitToolResult;
use crate::tools::safety_gateway::SafetyGateway;
use indexmap::IndexMap;
use parking_lot::Mutex; // Use parking_lot for better performance
use rustc_hash::FxHashMap;
use std::sync::{Arc, Weak};

use crate::exec::code_executor::{BuiltinToolExecutor, BuiltinToolInfo};
use crate::mcp::McpClient;
use crate::subagents::SubagentController;
use crate::tools::edited_file_monitor::EditedFileMonitor;
use async_trait::async_trait;
use std::sync::RwLock;

pub type SessionModelTools = Arc<tokio::sync::RwLock<Vec<crate::llm::provider::ToolDefinition>>>;

/// Callback for tool progress and output streaming
pub type ToolProgressCallback = Arc<dyn Fn(&str, &str) + Send + Sync>;

use super::traits::Tool;

#[cfg(test)]
use crate::config::types::CapabilityLevel;

/// Default window size for loop detection.
const DEFAULT_LOOP_DETECT_WINDOW: usize = 5;

#[derive(Clone)]
pub struct ToolRegistry {
    inventory: ToolInventory,
    /// Persistent-memory settings captured with the workspace tool snapshot.
    persistent_memory_config: Arc<crate::config::PersistentMemoryConfig>,
    persistent_memory_enabled: bool,
    edited_file_monitor: Arc<EditedFileMonitor>,
    policy_gateway: Arc<ToolPolicyGateway>,
    pty_sessions: PtySessionManager,
    exec_sessions: ExecSessionManager,
    mcp_client: Arc<parking_lot::RwLock<Option<Arc<McpClient>>>>,
    mcp_tool_index: Arc<tokio::sync::RwLock<FxHashMap<String, Vec<String>>>>,
    mcp_reverse_index: Arc<tokio::sync::RwLock<FxHashMap<String, String>>>,
    timeout_policy: Arc<parking_lot::RwLock<ToolTimeoutPolicy>>,
    execution_history: ToolExecutionHistory,
    harness_context: HarnessContext,

    // Mutable runtime state wrapped for concurrent access
    resiliency: Arc<Mutex<ResiliencyContext>>,

    /// MP-3: Circuit breaker for MCP client failures
    mcp_circuit_breaker: Arc<circuit_breaker::McpCircuitBreaker>,
    /// Shared per-tool circuit breaker state used by the runloop.
    shared_circuit_breaker: Arc<RwLock<Option<Arc<crate::tools::circuit_breaker::CircuitBreaker>>>>,
    initialized: Arc<std::sync::atomic::AtomicBool>,
    // Security & Identity
    shell_policy: Arc<RwLock<ShellPolicyChecker>>,
    runtime_sandbox_config: Arc<RwLock<vtcode_config::SandboxConfig>>,
    agent_type: Arc<RwLock<String>>,
    // PTY Session Management
    active_pty_sessions: Arc<RwLock<Option<Arc<std::sync::atomic::AtomicUsize>>>>,

    // Caching
    cached_available_tools: Arc<parking_lot::RwLock<Option<Vec<String>>>>,
    /// Active model-facing profile used by catalogue and policy projections.
    active_tool_profile: Arc<RwLock<crate::config::ToolProfile>>,
    /// Callback for streaming tool output and progress
    progress_callback: Arc<RwLock<Option<ToolProgressCallback>>>,
    // Performance Observability
    /// Total tool calls made in current session
    tool_call_counter: Arc<std::sync::atomic::AtomicU64>,
    /// Total PTY poll iterations (for monitoring CPU usage)
    pty_poll_counter: Arc<std::sync::atomic::AtomicU64>,
    /// Provider-visible preview bytes emitted this turn. Reset at each turn
    /// start by `begin_turn_preview_window()`; enforced in
    /// `output_processing::process_tool_output` against
    /// `TURN_PREVIEW_BUDGET_BYTES`.
    turn_preview_bytes: Arc<std::sync::atomic::AtomicUsize>,
    /// Canonical `.vtcode/plans` directory, resolved once per registry for
    /// the planning-mode hot path (`is_plan_file_operation`).
    canonical_plans_dir: Arc<std::sync::OnceLock<std::path::PathBuf>>,
    /// Shared metrics collector for reliability and execution observability
    metrics: Arc<crate::metrics::MetricsCollector>,

    // PERFORMANCE OPTIMIZATIONS - Actually integrated into the real registry
    /// Memory pool for reducing allocations in hot paths
    memory_pool: Arc<crate::core::memory_pool::MemoryPool>,
    /// Hot cache for frequently accessed tools (reduces HashMap lookups)
    hot_tool_cache: Arc<parking_lot::RwLock<lru::LruCache<String, Arc<dyn Tool>>>>,
    /// Optimization configuration
    optimization_config: vtcode_config::OptimizationConfig,

    /// Composable middleware chain for pre/post tool execution hooks.
    middleware: crate::tools::tool_middleware::MiddlewareChain,

    /// Output spooler for dynamic context discovery (large outputs to files)
    output_spooler: Arc<super::output_spooler::ToolOutputSpooler>,

    /// Shared Planning workflow state (plan file tracking, active flag) for enter/exit tools
    /// and read-only enforcement. This is the single source of truth for planning workflow;
    /// use `is_planning_active()` / `enable_planning()` / `disable_planning()` as accessors.
    planning_workflow_state: PlanningWorkflowState,
    /// Saved policies for tools whose permissions are overridden during planning
    /// workflow entry, so they can be restored on exit without disturbing other
    /// user-configured policies.
    planning_mode_policy_overrides: Arc<parking_lot::RwLock<Option<IndexMap<String, ToolPolicy>>>>,
    /// Canonical safety gateway shared across registry execution surfaces.
    safety_gateway: Arc<SafetyGateway>,
    /// Active CGP runtime mode for wrapping registrations added after startup.
    cgp_runtime_mode: Arc<RwLock<Option<CgpRuntimeMode>>>,
    /// Canonical manifest-driven tool assembly used by routing, catalog projections, and policy sync.
    tool_assembly: Arc<RwLock<ToolAssembly>>,
    /// Registry-owned tool catalog snapshot cache shared by harnesses.
    tool_catalog_state: Arc<SessionToolCatalogState>,
    /// Shared subagent controller when the session enables delegated child agents.
    subagent_controller: Arc<RwLock<Option<Arc<SubagentController>>>>,
    /// Session-scoped scheduled prompts for interactive loops and cron tools.
    session_scheduler: Arc<tokio::sync::Mutex<crate::scheduler::SessionScheduler>>,
    /// Live model-facing tool definitions attached by the runloop.
    session_model_tools: Arc<RwLock<Option<SessionModelTools>>>,
    /// Weak self-reference used by the code executor's built-in tool bridge.
    self_ref: Arc<RwLock<Option<Weak<ToolRegistry>>>>,
}

const BUILTIN_CODE_TOOLS: &[&str] = &[
    crate::config::constants::tools::UNIFIED_FILE,
    crate::config::constants::tools::CODE_SEARCH,
    crate::config::constants::tools::WEB_FETCH,
    crate::config::constants::tools::WEB_SEARCH,
    crate::config::constants::tools::CRON,
    crate::config::constants::tools::MEMORY,
    crate::config::constants::tools::TASK_TRACKER,
];

fn builtin_code_tool_description(name: &str) -> String {
    match name {
        n if n == crate::config::constants::tools::UNIFIED_FILE => "Read, write, edit, move, copy, or delete files.",
        n if n == crate::config::constants::tools::CODE_SEARCH => {
            "Search code by query, with optional path, file_types, result_types, and max_results."
        }
        n if n == crate::config::constants::tools::WEB_FETCH => {
            "Fetch a URL and return an analysed summary or markdown."
        }
        n if n == crate::config::constants::tools::WEB_SEARCH => "Run a web search and return ranked results.",
        n if n == crate::config::constants::tools::CRON => "Manage scheduled prompts.",
        n if n == crate::config::constants::tools::MEMORY => "Read or update persistent project memory.",
        n if n == crate::config::constants::tools::TASK_TRACKER => "Track multi-step task checklists.",
        _ => "Built-in VT Code tool.",
    }
    .to_string()
}

impl ToolRegistry {
    pub fn set_self_ref(&self, registry: Arc<ToolRegistry>) {
        *self.self_ref.write().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::downgrade(&registry));
    }

    fn builtin_executor_for_code(&self) -> Option<Arc<dyn BuiltinToolExecutor>> {
        self.self_ref
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(Weak::upgrade)
            .map(|registry| registry as Arc<dyn BuiltinToolExecutor>)
    }
}

#[async_trait]
impl BuiltinToolExecutor for ToolRegistry {
    async fn execute_builtin_tool(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        if !BUILTIN_CODE_TOOLS.contains(&tool_name) {
            anyhow::bail!("tool '{tool_name}' is not exposed to code snippets");
        }
        self.execute_tool(tool_name, args.clone()).await
    }

    fn list_builtin_tools(&self) -> anyhow::Result<Vec<BuiltinToolInfo>> {
        Ok(BUILTIN_CODE_TOOLS
            .iter()
            .map(|name| BuiltinToolInfo {
                name: (*name).to_string(),
                description: builtin_code_tool_description(name),
            })
            .collect())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPermissionDecision {
    Allow,
    Deny,
    Prompt,
}

#[cfg(test)]
mod tests;
