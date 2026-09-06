//! Tool execution entrypoints for ToolRegistry.

use anyhow::{Context, Result, anyhow};
use hashbrown::HashMap;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::borrow::Cow;
use std::cell::RefCell;
use std::future::Future;
use std::time::{Duration, Instant};
use tokio::task::Id as TokioTaskId;
use tracing::{trace, warn};
use vtcode_commons::ErrorCategory;

use crate::config::constants::tools;
use crate::core::agent::harness_kernel::PreparedToolCall;
use crate::core::memory_pool::SizeRecommendation;
use crate::mcp::McpToolExecutor;
use crate::retry::RetryPolicyCoreExt;
use crate::tool_policy::ToolExecutionDecision;
use crate::tools::error_messages::agent_execution;
use crate::tools::invocation::ToolInvocationId;
use crate::tools::mcp::{legacy_mcp_tool_name, parse_canonical_mcp_tool_name};
use crate::tools::request_response::{ToolCallRequest, ToolCallResponse};
use crate::tools::safety_gateway::{SafetyContext, SafetyDecision, SafetyError as GatewaySafetyError};
use crate::tools::tool_intent;
use crate::tools::unified_error::UnifiedErrorKind;
use crate::tools::unified_error::UnifiedToolError;
use crate::ui::search::fuzzy_match;

use super::assembly::public_tool_name_candidates;
use super::execution_kernel;
use super::normalize_tool_output;
use super::{
    ExecSettlementMode, ExecutionPolicySnapshot, ToolErrorType, ToolExecutionError, ToolExecutionOutcome,
    ToolExecutionRecord, ToolExecutionRequest, ToolHandler, ToolRegistry, ToolTimeoutCategory,
};
use vtcode_config::constants::execution::{LOOP_THROTTLE_MAX_MS, LOOP_THROTTLE_REGISTRY_BASE_MS};

const REENTRANCY_STACK_DEPTH_LIMIT: usize = 64;
/// When a read-only tool call has been repeated this many times, stop returning
/// cached results and return a hard error instead.  Must be greater than
/// MIN_READONLY_IDENTICAL_LIMIT (currently 2).
const LOOP_HARD_BLOCK_REPEAT_COUNT: usize = 5;

fn requests_unsandboxed_shell_permissions(tool_name: &str, args: &Value) -> bool {
    if !tool_intent::is_command_run_tool_call(tool_name, args) {
        return false;
    }

    matches!(
        args.get("sandbox_permissions").and_then(Value::as_str),
        Some(value) if value.eq_ignore_ascii_case("require_escalated") || value.eq_ignore_ascii_case("bypass_sandbox")
    )
}
// Tools should never recursively re-enter themselves in a single task.
// Keeping this at 1 blocks the first re-entry (A -> ... -> A) to fail fast
// on alias/self-recursion bugs with minimal extra work.
const REENTRANCY_PER_TOOL_LIMIT: usize = 1;

/// Extract the file paths a non-readonly tool call is about to mutate.
///
/// Returns an empty Vec for tool calls that aren't path-mutating (in which
/// case no cache invalidation is needed). Delegates to the canonical
/// `apply_patch::mutation_target_paths`, which covers singular `path`
/// fields, `destination`/`destination_path` (move/copy targets), and
/// `items`/`paths`/`files` arrays.
fn mutated_target_paths(tool_name: &str, args: &Value) -> Vec<String> {
    crate::tools::apply_patch::mutation_target_paths(tool_name, args)
        .into_iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}

/// Returns `true` for shell/command tools that can mutate files without
/// exposing a target path in their arguments (e.g. `sed -i`, `cargo build`,
/// `make`). When such a command is classified as mutating we cannot know which
/// files changed, so cached reads must be invalidated conservatively.
fn is_pathless_mutating_command(tool_name: &str) -> bool {
    matches!(tool_name, tools::UNIFIED_EXEC | tools::EXEC_COMMAND | tools::EXEC_PTY_CMD | tools::WRITE_STDIN)
}

fn structured_tool_output_error(value: &Value) -> Option<String> {
    let obj = value.as_object()?;
    if obj.get("success").and_then(Value::as_bool) == Some(false) {
        return obj
            .get("error")
            .map(tool_error_value_to_string)
            .or_else(|| Some("tool reported success=false".to_string()));
    }

    obj.get("error").map(tool_error_value_to_string)
}

fn tool_error_value_to_string(value: &Value) -> String {
    if let Some(message) = value.as_str() {
        return message.to_string();
    }
    if let Some(message) = value.get("message").and_then(Value::as_str) {
        return message.to_string();
    }
    value.to_string()
}

/// Global reentrancy stacks for tokio tasks.
///
/// Uses `parking_lot::Mutex` for lower overhead on short critical sections.
/// Each entry/exit is a single Vec push/pop under a task ID key.
///
/// If contention becomes an issue under high concurrency, consider:
/// - Using a concurrent hash map (e.g., `dashmap`)
/// - Using task-local storage via `tokio::task_local!`
/// - Partitioning the map by task ID hash to reduce contention
static TOOL_REENTRANCY_STACKS: Lazy<Mutex<HashMap<TokioTaskId, Vec<String>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
thread_local! {
    static THREAD_REENTRANCY_STACK: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn lock_reentrancy_stacks() -> parking_lot::MutexGuard<'static, HashMap<TokioTaskId, Vec<String>>> {
    TOOL_REENTRANCY_STACKS.lock()
}

#[derive(Debug)]
struct ReentrancyViolation {
    stack_depth: usize,
    tool_reentry_count: usize,
    stack_trace: String,
}

enum ReentrancyContext {
    Task(TokioTaskId),
    Thread,
}

struct ToolReentrancyGuard {
    context: Option<ReentrancyContext>,
}

impl ToolReentrancyGuard {
    fn enter(tool_name: &str) -> std::result::Result<Self, ReentrancyViolation> {
        if let Some(task_id) = tokio::task::try_id() {
            let mut stacks = lock_reentrancy_stacks();
            let stack = stacks.entry(task_id).or_default();
            let stack_depth = stack.len();
            let tool_reentry_count = stack.iter().filter(|active_tool| active_tool.as_str() == tool_name).count();

            if stack_depth >= REENTRANCY_STACK_DEPTH_LIMIT || tool_reentry_count >= REENTRANCY_PER_TOOL_LIMIT {
                let stack_trace = if stack.is_empty() {
                    "<empty>".to_string()
                } else {
                    stack.join(" -> ")
                };
                return Err(ReentrancyViolation { stack_depth, tool_reentry_count, stack_trace });
            }

            stack.push(tool_name.to_string());
            return Ok(Self { context: Some(ReentrancyContext::Task(task_id)) });
        }

        let violation = THREAD_REENTRANCY_STACK.with(|stack_cell| {
            let mut stack = stack_cell.borrow_mut();
            let stack_depth = stack.len();
            let tool_reentry_count = stack.iter().filter(|active_tool| active_tool.as_str() == tool_name).count();

            if stack_depth >= REENTRANCY_STACK_DEPTH_LIMIT || tool_reentry_count >= REENTRANCY_PER_TOOL_LIMIT {
                let stack_trace = if stack.is_empty() {
                    "<empty>".to_string()
                } else {
                    stack.join(" -> ")
                };
                Some(ReentrancyViolation { stack_depth, tool_reentry_count, stack_trace })
            } else {
                stack.push(tool_name.to_string());
                None
            }
        });

        if let Some(violation) = violation {
            return Err(violation);
        }

        Ok(Self { context: Some(ReentrancyContext::Thread) })
    }
}

impl Drop for ToolReentrancyGuard {
    fn drop(&mut self) {
        let Some(context) = self.context.take() else {
            return;
        };

        match context {
            ReentrancyContext::Task(task_id) => {
                let mut stacks = lock_reentrancy_stacks();
                let should_remove = if let Some(stack) = stacks.get_mut(&task_id) {
                    let _ = stack.pop();
                    stack.is_empty()
                } else {
                    false
                };
                if should_remove {
                    stacks.remove(&task_id);
                }
            }
            ReentrancyContext::Thread => {
                THREAD_REENTRANCY_STACK.with(|stack_cell| {
                    let _ = stack_cell.borrow_mut().pop();
                });
            }
        }
    }
}

impl ToolRegistry {
    fn annotate_timeout_error_payload(
        payload: &mut Value,
        timeout_category: &str,
        timeout_ms: u64,
        circuit_breaker: bool,
    ) {
        if let Some(obj) = payload.get_mut("error").and_then(|value| value.as_object_mut()) {
            obj.insert("timeout_category".into(), Value::String(timeout_category.to_string()));
            obj.insert("timeout_ms".into(), Value::from(timeout_ms));
            obj.insert("circuit_breaker".into(), Value::Bool(circuit_breaker));
        }
    }

    fn safety_denial_error(
        &self,
        tool_name: &str,
        reason: &str,
        violation: Option<GatewaySafetyError>,
        retry_after: Option<Duration>,
    ) -> ToolExecutionError {
        let mut error = ToolExecutionError::policy_violation(
            tool_name.to_string(),
            format!("Safety gateway denied execution: {reason}"),
        );

        match violation {
            Some(GatewaySafetyError::RateLimitExceeded { .. }) => {
                error.error_type = ToolErrorType::NetworkError;
                error.category = ErrorCategory::RateLimit;
                error.retryable = true;
                error.is_recoverable = true;
            }
            Some(GatewaySafetyError::TurnLimitReached { .. })
            | Some(GatewaySafetyError::SessionLimitReached { .. }) => {
                error.error_type = ToolErrorType::ExecutionError;
                error.category = ErrorCategory::ResourceExhausted;
                error.retryable = false;
                error.is_recoverable = false;
            }
            Some(GatewaySafetyError::PlanningPolicyViolation(_)) => {
                error.error_type = ToolErrorType::PolicyViolation;
                error.category = ErrorCategory::PlanningPolicyViolation;
                error.retryable = false;
                error.is_recoverable = true;
            }
            Some(GatewaySafetyError::CommandPolicyDenied(_))
            | Some(GatewaySafetyError::DotfileProtectionViolation(_))
            | None => {}
        }

        if let Some(delay) = retry_after {
            error.retry_after_ms = Some(delay.as_millis() as u64);
        }
        error.circuit_breaker_impact = error.category.should_trip_circuit_breaker();
        error.recovery_suggestions = error.category.recovery_suggestions();
        error
    }

    pub fn safety_gateway(&self) -> std::sync::Arc<crate::tools::safety_gateway::SafetyGateway> {
        std::sync::Arc::clone(&self.safety_gateway)
    }

    async fn check_safety_for_request(
        &self,
        tool_name: &str,
        args: &Value,
        invocation_id: Option<String>,
    ) -> Option<ToolExecutionError> {
        let context = SafetyContext::new(self.harness_context_snapshot().session_id);
        let invocation_id = invocation_id
            .and_then(|id| ToolInvocationId::parse(&id).ok())
            .unwrap_or_default();
        let safety_result = self
            .safety_gateway
            .check_and_record_with_id(&context, tool_name, args, Some(invocation_id))
            .await;

        match safety_result.decision {
            SafetyDecision::Allow | SafetyDecision::NeedsApproval(_) => None,
            SafetyDecision::Deny(reason) => Some(
                self.safety_denial_error(tool_name, &reason, safety_result.violation, safety_result.retry_after)
                    .with_surface("tool_registry"),
            ),
        }
    }

    /// Inline-delegating wrapper that returns the inner future directly to
    /// avoid an extra coroutine state machine (audit section 16).
    pub fn execute_public_tool_request(
        &self,
        request: ToolExecutionRequest,
    ) -> impl Future<Output = ToolExecutionOutcome> + '_ {
        self.execute_tool_request_internal(request)
    }

    pub async fn execute_prepared_public_tool_request(
        &self,
        prepared: &PreparedToolCall,
        policy: ExecutionPolicySnapshot,
    ) -> ToolExecutionOutcome {
        let request = ToolExecutionRequest::new(prepared.canonical_name.clone(), prepared.effective_args.clone())
            .with_policy(
                policy
                    .with_prevalidated(prepared.already_preflighted)
                    .with_safety_prevalidated(false),
            );
        self.execute_tool_request_internal(request).await
    }

    async fn execute_tool_request_internal(&self, request: ToolExecutionRequest) -> ToolExecutionOutcome {
        let tool_name = &request.tool_name;
        let policy = request.policy.clone();

        if requests_unsandboxed_shell_permissions(tool_name, &request.args) {
            let message = format!(
                "sandbox_permissions in `{tool_name}` requires an enforced operator approval decision before unsandboxed execution"
            );
            let error = ToolExecutionError::new(tool_name.clone(), ToolErrorType::PolicyViolation, message)
                .with_tool_call_context(tool_name, &request.args)
                .with_surface("tool_registry");
            return ToolExecutionOutcome::failure(tool_name.clone(), 1, error);
        }

        let mut retry_policy = crate::retry::RetryPolicy::from_retries(
            policy.max_retries as u32,
            policy.retry_base_delay,
            policy.retry_max_delay,
            policy.retry_multiplier,
        );
        retry_policy.jitter = policy.retry_jitter.clamp(0.0, 1.0);

        let max_attempts = retry_policy.max_attempts.max(1);
        let mut attempt_index: u32 = 0;
        let mut last_error: Option<ToolExecutionError> = None;

        while attempt_index < max_attempts {
            if !policy.safety_prevalidated
                && let Some(safety_error) = self
                    .check_safety_for_request(tool_name, &request.args, policy.invocation_id.clone())
                    .await
            {
                let decorated = safety_error
                    .with_tool_call_context(tool_name, &request.args)
                    .with_attempt(attempt_index + 1)
                    .with_surface("tool_registry");
                if let Some(terminal) = Self::classify_and_step(
                    decorated,
                    &retry_policy,
                    tool_name,
                    &mut attempt_index,
                    max_attempts,
                    &mut last_error,
                )
                .await
                {
                    return ToolExecutionOutcome::failure(tool_name, attempt_index + 1, terminal);
                }
                continue;
            }

            let result = self
                .execute_public_tool_ref_dispatch(
                    tool_name,
                    &request.args,
                    policy.prevalidated,
                    execution_kernel::DispatchMode::Harness,
                    policy.exec_settlement_mode,
                )
                .await;

            match result {
                Ok(output) => {
                    if let Some(structured_error) = ToolExecutionError::from_tool_output(&output) {
                        let decorated = structured_error
                            .with_tool_call_context(tool_name, &request.args)
                            .with_attempt(attempt_index + 1)
                            .with_surface("tool_registry");
                        if let Some(terminal) = Self::classify_and_step(
                            decorated,
                            &retry_policy,
                            tool_name,
                            &mut attempt_index,
                            max_attempts,
                            &mut last_error,
                        )
                        .await
                        {
                            return ToolExecutionOutcome::failure(tool_name, attempt_index + 1, terminal);
                        }
                        continue;
                    }

                    return ToolExecutionOutcome::success(tool_name, attempt_index + 1, output);
                }
                Err(error) => {
                    let mut base = ToolExecutionError::from_anyhow(
                        tool_name,
                        &error,
                        attempt_index,
                        false,
                        false,
                        Some("tool_registry"),
                    );
                    let lower_message = base.message.to_ascii_lowercase();
                    let lower_original = base.original_error.as_deref().unwrap_or_default().to_ascii_lowercase();
                    if lower_message.contains("circuit breaker") || lower_original.contains("circuit breaker") {
                        base.category = ErrorCategory::CircuitOpen;
                        base.retryable = true;
                        base.is_recoverable = true;
                        if base.retry_delay_ms.is_none() {
                            base.retry_delay_ms = Some(policy.retry_base_delay.as_millis() as u64);
                        }
                    }

                    if let Some(terminal) = Self::classify_and_step(
                        base,
                        &retry_policy,
                        tool_name,
                        &mut attempt_index,
                        max_attempts,
                        &mut last_error,
                    )
                    .await
                    {
                        return ToolExecutionOutcome::failure(tool_name, attempt_index + 1, terminal);
                    }
                    continue;
                }
            }
        }

        ToolExecutionOutcome::failure(
            tool_name,
            max_attempts,
            last_error.unwrap_or_else(|| {
                ToolExecutionError::new(
                    tool_name,
                    ToolErrorType::ExecutionError,
                    format!("Tool '{}' failed after {} attempts with no structured error", tool_name, max_attempts),
                )
                .with_surface("tool_registry")
            }),
        )
    }

    /// Apply the retry policy to a `ToolExecutionError` and either schedule
    /// the next attempt (sleep + bump index, return `None`) or report a
    /// terminal failure (return `Some(structured)` for the caller to surface).
    ///
    /// Consolidates the three identical retry/sleep/continue blocks that
    /// previously lived inline in `execute_tool_request_internal`.
    async fn classify_and_step(
        decorated: ToolExecutionError,
        retry_policy: &crate::retry::RetryPolicy,
        tool_name: &str,
        attempt_index: &mut u32,
        max_attempts: u32,
        last_error: &mut Option<ToolExecutionError>,
    ) -> Option<ToolExecutionError> {
        let structured = retry_policy.apply_to_tool_execution_error(decorated, *attempt_index, Some(tool_name));
        let retry_delay = structured.retry_after().or_else(|| structured.retry_delay());
        if structured.retryable
            && *attempt_index + 1 < max_attempts
            && let Some(delay) = retry_delay
        {
            *last_error = Some(structured);
            tokio::time::sleep(delay).await;
            *attempt_index = attempt_index.saturating_add(1);
            return None;
        }
        Some(structured)
    }

    async fn should_skip_loop_detection_for_exec_continuation(&self, tool_name: &str, args: &Value) -> bool {
        if tool_name == tools::WRITE_STDIN {
            return matches!(
                crate::tools::command_args::write_stdin_dispatch(args),
                Ok(crate::tools::command_args::WriteStdinDispatch::Poll
                    | crate::tools::command_args::WriteStdinDispatch::Wait,)
            );
        }

        if tool_name != tools::UNIFIED_EXEC {
            return false;
        }

        if !tool_intent::command_session_action_in(args, &["poll", "continue"]) {
            return false;
        }
        if tool_intent::command_session_action_is(args, "continue")
            && crate::tools::command_args::interactive_input_text(args).is_some()
        {
            return false;
        }

        let Some(session_id) = crate::tools::command_args::session_id_text(args) else {
            return false;
        };

        matches!(self.exec_session_completed(session_id).await, Ok(None))
    }

    async fn public_tool_catalog_for_error(&self, requested_name: &str) -> (Vec<String>, Vec<String>) {
        let mut tool_names = self.available_tools().await;
        tool_names.sort_unstable();
        tool_names.dedup();

        let requested_candidates = public_tool_name_candidates(requested_name);
        let mut similar_tools = Vec::new();

        if let Ok(resolved) = self.resolve_public_tool_name_sync(requested_name)
            && tool_names.iter().any(|tool| tool == &resolved)
        {
            similar_tools.push(resolved);
        }

        for tool in &tool_names {
            if similar_tools.len() >= 3 {
                break;
            }

            if similar_tools.iter().any(|candidate| candidate == tool) {
                continue;
            }

            if requested_candidates.iter().any(|candidate| fuzzy_match(candidate, tool)) {
                similar_tools.push(tool.clone());
            }
        }

        (tool_names, similar_tools)
    }

    pub fn preflight_validate_call(&self, name: &str, args: &Value) -> Result<super::ToolPreflightOutcome> {
        execution_kernel::preflight_validate_call(self, name, args)
    }

    /// Preflight a tool call from the harness path, allowing dispatch to
    /// internal (model-hidden) tool registrations such as read_file/write_file.
    /// Direct model-originated entry should use `preflight_validate_call`.
    pub fn preflight_validate_harness_call(&self, name: &str, args: &Value) -> Result<super::ToolPreflightOutcome> {
        execution_kernel::preflight_validate_call_with_mode(self, name, args, execution_kernel::DispatchMode::Harness)
    }

    pub fn admit_public_tool_call(&self, name: &str, args: &Value) -> Result<PreparedToolCall> {
        let preflight = self.preflight_validate_harness_call(name, args)?;
        Ok(PreparedToolCall::new(
            preflight.normalized_tool_name,
            preflight.readonly_classification,
            preflight.parallel_safe_after_preflight,
            preflight.effective_args,
        ))
    }

    pub async fn execute_tool(&self, name: &str, args: Value) -> Result<Value> {
        self.execute_tool_ref(name, &args).await
    }

    /// Execute a model-originated tool call through the public routing assembly.
    pub async fn execute_public_tool_ref(&self, name: &str, args: &Value) -> Result<Value> {
        self.execute_public_tool_ref_internal(name, args, false).await
    }

    /// Reference-taking version of execute_tool to avoid cloning by callers
    /// that already have access to an existing `Value`.
    pub async fn execute_tool_ref(&self, name: &str, args: &Value) -> Result<Value> {
        self.execute_tool_ref_internal(name, args, false, ExecSettlementMode::Manual)
            .await
    }

    /// Reference-taking execution entrypoint for calls that were already preflight-validated.
    ///
    /// This avoids re-running argument/schema/path/command preflight in hot paths
    /// where validation already happened in the runloop.
    pub async fn execute_tool_ref_prevalidated(&self, name: &str, args: &Value) -> Result<Value> {
        self.execute_tool_ref_internal(name, args, true, ExecSettlementMode::Manual)
            .await
    }

    /// Prevalidated model-originated execution that still routes through the public assembly.
    pub async fn execute_public_tool_ref_prevalidated(&self, name: &str, args: &Value) -> Result<Value> {
        self.execute_public_tool_ref_prevalidated_with_mode(name, args, ExecSettlementMode::Manual)
            .await
    }

    #[doc(hidden)]
    pub async fn execute_public_tool_ref_prevalidated_with_mode(
        &self,
        name: &str,
        args: &Value,
        exec_settlement_mode: ExecSettlementMode,
    ) -> Result<Value> {
        self.execute_public_tool_ref_internal_with_mode(name, args, true, exec_settlement_mode)
            .await
    }

    pub async fn execute_prepared_public_tool_ref_with_mode(
        &self,
        prepared: &PreparedToolCall,
        exec_settlement_mode: ExecSettlementMode,
    ) -> Result<Value> {
        // Prepared calls come from the harness admission gate
        // (`admit_public_tool_call`), which is the only producer of
        // `PreparedToolCall`, so internal (model-hidden) dispatch is authorized.
        self.execute_public_tool_ref_dispatch(
            prepared.canonical_name.as_str(),
            &prepared.effective_args,
            prepared.already_preflighted,
            execution_kernel::DispatchMode::Harness,
            exec_settlement_mode,
        )
        .await
    }

    async fn execute_public_tool_ref_internal(&self, name: &str, args: &Value, prevalidated: bool) -> Result<Value> {
        self.execute_public_tool_ref_dispatch(
            name,
            args,
            prevalidated,
            execution_kernel::DispatchMode::ModelPublic,
            ExecSettlementMode::Manual,
        )
        .await
    }

    async fn execute_public_tool_ref_internal_with_mode(
        &self,
        name: &str,
        args: &Value,
        prevalidated: bool,
        exec_settlement_mode: ExecSettlementMode,
    ) -> Result<Value> {
        self.execute_public_tool_ref_dispatch(
            name,
            args,
            prevalidated,
            execution_kernel::DispatchMode::ModelPublic,
            exec_settlement_mode,
        )
        .await
    }

    /// Core public-routing execution entrypoint.
    ///
    /// `dispatch_mode` is the authority to fall back to internal (model-hidden)
    /// tool registrations and is fully independent of `prevalidated` (a pure
    /// performance flag that skips re-running preflight). Only callers that went
    /// through the harness admission gate (`admit_public_tool_call`) pass
    /// [`execution_kernel::DispatchMode::Harness`]; direct model-originated entry always passes
    /// [`execution_kernel::DispatchMode::ModelPublic`], so a stray `prevalidated=true` can never by
    /// itself widen the dispatchable surface.
    async fn execute_public_tool_ref_dispatch(
        &self,
        name: &str,
        args: &Value,
        prevalidated: bool,
        dispatch_mode: execution_kernel::DispatchMode,
        exec_settlement_mode: ExecSettlementMode,
    ) -> Result<Value> {
        let routed_name = execution_kernel::resolve_dispatch_target(self, name, dispatch_mode)
            .map_err(|err| anyhow!(err.to_string()))?;
        let effective_args = execution_kernel::remap_public_file_operation_alias_args(name, routed_name.as_str(), args)
            .or_else(|| execution_kernel::remap_consolidated_action_alias_args(name, routed_name.as_str(), args));
        self.execute_tool_ref_internal(
            routed_name.as_str(),
            effective_args.as_ref().unwrap_or(args),
            prevalidated,
            exec_settlement_mode,
        )
        .await
    }

    async fn execute_tool_ref_internal(
        &self,
        name: &str,
        args: &Value,
        prevalidated: bool,
        exec_settlement_mode: ExecSettlementMode,
    ) -> Result<Value> {
        // PERFORMANCE OPTIMIZATION: Use memory pool for string allocations if enabled
        let _pool_guard = if self.optimization_config.memory_pool.enabled {
            Some(self.memory_pool.get_string())
        } else {
            None
        };

        // PERFORMANCE OPTIMIZATION: Auto-tune memory pool based on usage patterns
        if self.optimization_config.memory_pool.enabled {
            let recommendation = self.memory_pool.auto_tune(&self.optimization_config.memory_pool);

            // Log recommendation if significant changes are suggested
            if !matches!(
                (
                    recommendation.string_size_recommendation,
                    recommendation.value_size_recommendation,
                    recommendation.vec_size_recommendation
                ),
                (SizeRecommendation::Maintain, SizeRecommendation::Maintain, SizeRecommendation::Maintain)
            ) {
                tracing::debug!(
                    "Memory pool tuning recommendation: string={:?}, value={:?}, vec={:?}, allocations_avoided={}",
                    recommendation.string_size_recommendation,
                    recommendation.value_size_recommendation,
                    recommendation.vec_size_recommendation,
                    recommendation.total_allocations_avoided
                );
            }
        }

        // Look up the canonical tool name by trying to resolve the alias
        // The inventory's registration_for() handles alias resolution
        let (tool_name, tool_name_owned, display_name) =
            if let Some(registration) = self.inventory.registration_for(name) {
                let canonical = registration.name().to_string();
                let display = if canonical == name {
                    canonical.clone()
                } else {
                    format!("{name} (alias for {canonical})")
                };
                (canonical.clone(), canonical.clone(), display)
            } else {
                // If not found in registration, use the name as-is (for potential MCP tools or error handling)
                let tool_name_owned = name.to_string();
                let display_name = tool_name_owned.clone();
                (tool_name_owned.clone(), tool_name_owned, display_name)
            };

        // PERFORMANCE OPTIMIZATION: Check hot cache for tool lookup using the canonical name.
        // This must happen AFTER alias resolution so that aliased tools resolve to their
        // canonical cache entry on the first hit. Without this, the cache lookup uses the
        // raw alias string while insertion uses the canonical name, making aliased tools
        // perpetually miss the cache.
        let cached_tool = if self.optimization_config.tool_registry.use_optimized_registry {
            let cache = self.hot_tool_cache.read();
            cache.peek(&tool_name).cloned()
        } else {
            None
        };

        // PERFORMANCE OPTIMIZATION: Update hot cache with resolved tool if optimizations enabled
        if let Some(tool_arc) = cached_tool.as_ref()
            && self.optimization_config.tool_registry.use_optimized_registry
            && tool_name != name
        {
            // Cache the canonical name too for faster future lookups
            self.hot_tool_cache.write().put(tool_name.clone(), tool_arc.clone());
        }

        let parameter_schema = self
            .inventory
            .registration_for(&tool_name)
            .and_then(|registration| registration.parameter_schema().cloned());
        let normalized_args = execution_kernel::normalize_tool_args(&tool_name, args, parameter_schema.as_ref())?;
        let max_output_tokens = crate::tools::output_limits::max_output_tokens(normalized_args.as_ref())?;
        let handler_args = if crate::tools::output_limits::handler_accepts_output_metadata(parameter_schema.as_ref()) {
            Cow::Borrowed(normalized_args.as_ref())
        } else {
            Cow::Owned(crate::tools::output_limits::args_without_output_metadata(normalized_args.as_ref()))
        };
        let args = handler_args.as_ref();
        let requested_name = name.to_string();

        // Clone args once at the start for error recording paths (clone only here)
        let args_for_recording = args.clone();
        // Capture harness context snapshot for structured telemetry and history
        let context_snapshot = self.harness_context_snapshot();
        let record_failure = |tool_name: String,
                              is_mcp_tool: bool,
                              mcp_provider: Option<String>,
                              args: Value,
                              error_msg: String,
                              timeout_category: Option<String>,
                              base_timeout_ms: Option<u64>,
                              adaptive_timeout_ms: Option<u64>,
                              effective_timeout_ms: Option<u64>,
                              circuit_breaker: bool| {
            self.execution_history.add_record(ToolExecutionRecord::failure(
                tool_name,
                requested_name.clone(),
                is_mcp_tool,
                mcp_provider,
                args,
                error_msg,
                context_snapshot.clone(),
                timeout_category,
                base_timeout_ms,
                adaptive_timeout_ms,
                effective_timeout_ms,
                circuit_breaker,
            ));
        };

        let _reentrancy_guard = match ToolReentrancyGuard::enter(&tool_name) {
            Ok(guard) => guard,
            Err(violation) => {
                let reentry_count = violation.tool_reentry_count + 1;
                let error_message = format!(
                    "REENTRANCY GUARD: Tool '{}' was blocked to prevent recursive execution.\n\n\
                     ACTION REQUIRED: DO NOT retry this same tool call without changing control flow.\n\
                     Current stack depth: {}. Re-entry count for this tool in the current task: {}.\n\
                     Stack trace: {}",
                    display_name, violation.stack_depth, reentry_count, violation.stack_trace
                );
                let error = ToolExecutionError::new(
                    tool_name_owned.clone(),
                    ToolErrorType::PolicyViolation,
                    error_message.clone(),
                );
                let mut payload = error.to_json_value();
                if let Some(obj) = payload.as_object_mut() {
                    obj.insert("reentrant_call_blocked".into(), json!(true));
                    obj.insert("stack_depth".into(), json!(violation.stack_depth));
                    obj.insert("reentry_count".into(), json!(reentry_count));
                    obj.insert("tool".into(), json!(display_name));
                    obj.insert("stack_trace".into(), json!(violation.stack_trace));
                }
                record_failure(
                    tool_name_owned.clone(),
                    false,
                    None,
                    args_for_recording.clone(),
                    error_message.clone(),
                    None,
                    None,
                    None,
                    None,
                    false,
                );
                return Err(anyhow!(error_message).context("tool reentrancy blocked"));
            }
        };

        // Classify the tool intent once and reuse it for the read-only
        // classification and the planning-workflow enforcement below, instead
        // of recomputing it on every tool call.
        let intent = tool_intent::classify_tool_intent(&tool_name, args);
        let readonly_classification = if prevalidated {
            #[cfg(debug_assertions)]
            {
                if let Err(err) = execution_kernel::preflight_validate_resolved_call(self, &tool_name, args)
                    && !agent_execution::is_planning_active_denial(&err.to_string())
                {
                    debug_assert!(false, "prevalidated execution received invalid call for '{tool_name}': {err}");
                }
            }
            !intent.mutating
        } else {
            match execution_kernel::preflight_validate_resolved_call(self, &tool_name, args) {
                Ok(outcome) => outcome.readonly_classification,
                Err(err) => {
                    let err_msg = err.to_string();
                    record_failure(
                        tool_name_owned.clone(),
                        false,
                        None,
                        args_for_recording.clone(),
                        err_msg,
                        None,
                        None,
                        None,
                        None,
                        false,
                    );
                    return Err(err);
                }
            }
        };

        if readonly_classification {
            trace!(tool = %tool_name, "Validation classified tool as read-only");
        }

        // Defense-in-depth: prevalidated fast path skips full preflight, but planning workflow
        // mutating-tool enforcement remains a hard safety invariant. Reuse the already-computed
        // `intent` so we don't reclassify on the hot path.
        if self.is_planning_active() && !self.is_planning_active_allowed_with_intent(&tool_name, args, &intent) {
            let error_msg = agent_execution::planning_workflow_denial_message(&display_name);
            record_failure(
                tool_name_owned.clone(),
                false,
                None,
                args_for_recording.clone(),
                error_msg.clone(),
                None,
                None,
                None,
                None,
                false,
            );
            return Err(anyhow!(error_msg).context(agent_execution::PLANNING_DENIED_CONTEXT));
        }

        let shared_circuit_breaker = self.shared_circuit_breaker();
        if let Some(breaker) = shared_circuit_breaker.as_ref()
            && !breaker.allow_request_for_tool(&tool_name)
        {
            let diagnostics = breaker.get_diagnostics(&tool_name);
            let retry_after = diagnostics
                .remaining_backoff
                .map(|backoff| format!(" retry_after={}s.", backoff.as_secs()))
                .unwrap_or_default();
            let error_msg = format!(
                "Tool '{display_name}' is temporarily disabled due to high failure rate (Circuit Breaker OPEN).{retry_after}"
            );
            self.execution_history.add_record(
                ToolExecutionRecord::failure(
                    tool_name_owned.clone(),
                    requested_name.clone(),
                    false,
                    None,
                    args_for_recording.clone(),
                    error_msg.clone(),
                    context_snapshot.clone(),
                    None,
                    None,
                    None,
                    None,
                    true,
                )
                .with_circuit_breaker_state(format!("{:?}", diagnostics.status))
                .with_retry_after(diagnostics.remaining_backoff),
            );
            return Err(anyhow!(error_msg).context("tool denied by circuit breaker"));
        }

        let timeout_category = self.timeout_category_for_args(&tool_name, args).await;

        if let Some(backoff) = self.should_circuit_break(timeout_category) {
            warn!(
                tool = %tool_name,
                category = %timeout_category.label(),
                delay_ms = %backoff.as_millis(),
                "Circuit breaker active for tool category; backing off before execution"
            );
            tokio::time::sleep(backoff).await;
        }

        let execution_span = tracing::debug_span!(
            "tool_execution",
            tool = %tool_name,
            requested = %name,
            session_id = %context_snapshot.session_id,
            task_id = %context_snapshot.task_id.as_deref().unwrap_or("")
        );
        let _span_guard = execution_span.enter();

        trace!(
            tool = %tool_name,
            session_id = %context_snapshot.session_id,
            task_id = %context_snapshot.task_id.as_deref().unwrap_or(""),
            "Executing tool with harness context"
        );

        if tool_name != name {
            trace!(
                requested = %name,
                canonical = %tool_name,
                "Resolved tool alias to canonical name"
            );
        }

        let base_timeout_ms = self
            .timeout_policy
            .read()
            .ceiling_for(timeout_category)
            .map(|d| d.as_millis() as u64);
        let adaptive_timeout_ms = self
            .resiliency
            .lock()
            .adaptive_timeout_ceiling
            .get(&timeout_category)
            .filter(|d| d.as_millis() > 0)
            .map(|d| d.as_millis() as u64);
        let timeout_category_label = Some(timeout_category.label().to_string());

        if let Some(rate_limit) = self.execution_history.rate_limit_per_minute() {
            let calls_last_minute = self.execution_history.calls_in_window(Duration::from_secs(60));
            if calls_last_minute >= rate_limit {
                warn!(
                    tool = %tool_name_owned,
                    requested = %requested_name,
                    calls_last_minute,
                    rate_limit,
                    "Execution history rate-limit threshold exceeded (observability-only)"
                );
            }
        }

        let skip_loop_detection = self.should_skip_loop_detection_for_exec_continuation(&tool_name, args).await;
        if skip_loop_detection {
            trace!(
                tool = %tool_name,
                "Skipping identical-call loop detection for stateful exec continuation"
            );
        }

        // FAST REUSE: Read-only inspection calls are often repeated verbatim within a
        // single turn (e.g. `diff a b | wc -l`, `find ... | grep`). Reuse the most recent
        // successful result immediately instead of paying for another round-trip and
        // another spool file. This is gated by a short TTL and the read-only classification
        // so mutating calls never take this path.
        //
        // Verification commands (`cargo check`, `cargo nextest run`, `cargo fmt
        // --check`, and pure `&&` chains thereof) are read-only by intent but
        // must always re-execute: reusing a stale success would clear the
        // anti-blind-editing gate without verifying the current worktree, and
        // reusing a stale failure would keep the gate pending after a fix.
        let is_verification_command =
            matches!(tool_intent::classify_shell_activity(&tool_name, args), tool_intent::ShellActivity::Verification);
        if readonly_classification && !is_verification_command && !skip_loop_detection {
            let fast_reuse_max_age = Duration::from_secs(60);
            let fast_reused = self
                .execution_history
                .find_recent_spooled_result(&tool_name, args, fast_reuse_max_age)
                .or_else(|| {
                    self.execution_history
                        .find_recent_successful_result(&tool_name, args, fast_reuse_max_age)
                });
            if let Some(mut reused_value) = fast_reused {
                if let Some(obj) = reused_value.as_object_mut() {
                    obj.insert("reused_recent_result".into(), json!(true));
                    obj.insert("tool".into(), json!(display_name));
                    let reused_spooled = obj.get("spool_path").and_then(|v| v.as_str()).is_some();
                    let note = if reused_spooled {
                        "Reusing a recent spooled output for this identical read-only call. Continue from the spool file instead of re-running the tool."
                    } else {
                        "Reusing a recent successful output for this identical read-only call."
                    };
                    obj.insert("reused_result_note".into(), json!(note));
                }
                // Record a synthetic "reused" entry so subsequent
                // `detect_loop` / `find_recent_*` queries see this call in the
                // history.  Previously the fast-reuse path returned the
                // cached payload without recording, which meant `detect_loop`
                // could not account for the reused call — leading to
                // undercounting on the very next turn and silent cache
                // poisoning.
                //
                // We pass `is_mcp_tool = false` and `mcp_provider = None`
                // because the fast-reuse gate has already filtered for
                // read-only calls and we don't have those locals in scope at
                // this point in the function.  The cached record itself
                // (added when the original call ran) carries the true MCP
                // metadata; the synthetic record exists only to make
                // `detect_loop` see this call in the rolling window.
                self.execution_history.add_record(ToolExecutionRecord::success(
                    tool_name.clone(),
                    requested_name.clone(),
                    false,
                    None,
                    args_for_recording.clone(),
                    reused_value.clone(),
                    context_snapshot.clone(),
                    timeout_category_label.clone(),
                    base_timeout_ms,
                    adaptive_timeout_ms,
                    None,
                    false,
                ));
                trace!(
                    tool = %tool_name,
                    "Fast-reusing recent successful read-only result"
                );
                return Ok(reused_value);
            }
        }

        // LOOP DETECTION: Check if we're calling the same tool repeatedly with identical params
        let loop_limit = if skip_loop_detection {
            0
        } else {
            self.execution_history.loop_limit_for(&tool_name, args)
        };
        let loop_result = if skip_loop_detection {
            crate::tools::registry::execution_history::LoopDetectionResult {
                detected: false,
                repeat_count: 0,
                tool_name: tool_name.clone(),
            }
        } else {
            self.execution_history.detect_loop(&tool_name, args)
        };
        if loop_result.detected && loop_result.repeat_count > 1 {
            let delay_ms = (LOOP_THROTTLE_REGISTRY_BASE_MS * loop_result.repeat_count as u64).min(LOOP_THROTTLE_MAX_MS);
            if delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
        }
        if loop_limit > 0 && loop_result.detected {
            warn!(
                tool = %tool_name,
                repeats = loop_result.repeat_count,
                "Loop detected: agent calling same tool with identical parameters {} times",
                loop_result.repeat_count
            );
            if loop_result.repeat_count >= loop_limit {
                // Hard block: when the model has been repeating the same call far
                // beyond the limit, stop returning cached results and return an
                // error.  Returning a cached result at high repeat counts causes
                // the model to see "success" and keep retrying.
                let hard_block = loop_result.repeat_count >= LOOP_HARD_BLOCK_REPEAT_COUNT;

                if readonly_classification && !hard_block {
                    let reuse_max_age = Duration::from_secs(120);
                    let reused = self
                        .execution_history
                        .find_recent_spooled_result(&tool_name, args, reuse_max_age)
                        .or_else(|| {
                            self.execution_history
                                .find_recent_successful_result(&tool_name, args, reuse_max_age)
                        });
                    if let Some(mut reused_value) = reused {
                        if let Some(obj) = reused_value.as_object_mut() {
                            obj.insert("reused_recent_result".into(), json!(true));
                            obj.insert("loop_detected".into(), json!(true));
                            obj.insert("repeat_count".into(), json!(loop_result.repeat_count));
                            obj.insert("limit".into(), json!(loop_limit));
                            obj.insert("tool".into(), json!(display_name));
                            let reused_spooled = obj.get("spool_path").and_then(|v| v.as_str()).is_some();
                            let note = if reused_spooled {
                                "Loop detected: this identical read-only call has been repeated. The full output is in the spool file. STOP making this same tool call -- use the spool file or conversation history. Calling this tool again will be blocked."
                            } else {
                                "Loop detected: this identical read-only call has been repeated with no new information. STOP -- the result is already available in your conversation history. Do NOT call this tool again with the same arguments."
                            };
                            obj.insert("loop_detected_note".into(), json!(note));
                        }
                        return Ok(reused_value);
                    }
                }

                let delay_ms =
                    (LOOP_THROTTLE_REGISTRY_BASE_MS * loop_result.repeat_count as u64).min(LOOP_THROTTLE_MAX_MS);
                if delay_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                }

                let error = ToolExecutionError::new(
                    tool_name_owned.clone(),
                    ToolErrorType::PolicyViolation,
                    agent_execution::loop_detection_block_message(&display_name, loop_result.repeat_count as u64, None),
                );
                let mut payload = error.to_json_value();
                if let Some(obj) = payload.as_object_mut() {
                    obj.insert("loop_detected".into(), json!(true));
                    obj.insert("repeat_count".into(), json!(loop_result.repeat_count));
                    obj.insert("limit".into(), json!(loop_limit));
                    obj.insert("tool".into(), json!(display_name));
                    obj.insert(
                        "next_action".into(),
                        json!("STOP calling this tool. Use the data already in your conversation history. If you need different information, use a DIFFERENT tool or approach."),
                    );
                }

                record_failure(
                    tool_name_owned,
                    false,
                    None,
                    args_for_recording,
                    "Tool call blocked due to repeated identical invocations".to_string(),
                    timeout_category_label.clone(),
                    base_timeout_ms,
                    adaptive_timeout_ms,
                    None,
                    false,
                );

                return Ok(payload);
            }
        }

        let full_auto_denied = {
            let gateway = self.policy_gateway.clone();
            let tool_name_ref = &tool_name;
            async move { gateway.is_denied_in_full_auto(tool_name_ref).await }
        };
        let full_auto_denied = full_auto_denied.await;
        if full_auto_denied {
            let _error = ToolExecutionError::new(
                tool_name_owned.clone(),
                ToolErrorType::PolicyViolation,
                format!("Tool '{display_name}' is not permitted while full-auto permission review is active"),
            );

            record_failure(
                tool_name_owned.clone(),
                false,
                None,
                args_for_recording.clone(),
                "Tool execution denied by policy".to_string(),
                timeout_category_label.clone(),
                base_timeout_ms,
                adaptive_timeout_ms,
                None,
                false,
            );

            return Err(anyhow!("Tool '{display_name}' is not permitted while full-auto permission review is active")
                .context("tool denied by full-auto allowlist"));
        }

        let skip_policy_prompt = self.policy_gateway.take_preapproved(&tool_name).await;

        let decision = if skip_policy_prompt {
            ToolExecutionDecision::Allowed
        } else {
            self.policy_gateway.should_execute_tool(&tool_name).await?
        };

        if !decision.is_allowed() {
            let error_msg = match decision {
                ToolExecutionDecision::DeniedWithFeedback(feedback) => {
                    format!("Tool '{display_name}' denied by user: {feedback}")
                }
                _ => format!("Tool '{display_name}' execution denied by policy"),
            };

            let _error =
                ToolExecutionError::new(tool_name_owned.clone(), ToolErrorType::PolicyViolation, error_msg.clone());

            record_failure(
                tool_name_owned.clone(),
                false,
                None,
                args_for_recording.clone(),
                error_msg.clone(),
                timeout_category_label.clone(),
                base_timeout_ms,
                adaptive_timeout_ms,
                None,
                false,
            );

            return Err(anyhow!("{error_msg}").context("tool denied by policy"));
        }

        let gateway = self.policy_gateway.clone();
        let constrained_result = gateway.apply_policy_constraints(&tool_name, args).await;
        let args = match constrained_result {
            Ok(processed_args) => processed_args,
            Err(err) => {
                let error = ToolExecutionError::with_original_error(
                    tool_name_owned.clone(),
                    ToolErrorType::InvalidParameters,
                    "Failed to apply policy constraints".to_string(),
                    err.to_string(),
                );

                record_failure(
                    tool_name_owned,
                    false,
                    None,
                    args_for_recording,
                    format!("Failed to apply policy constraints: {err}"),
                    timeout_category_label.clone(),
                    base_timeout_ms,
                    adaptive_timeout_ms,
                    None,
                    false,
                );

                return Err(anyhow!(error.to_json_value()).context("tool denied by policy constraints"));
            }
        };

        // First, check if we need a PTY session by checking if the tool exists and needs PTY
        let mut needs_pty = false;
        let mut tool_exists = false;
        let mut is_mcp_tool = false;
        let mut mcp_provider: Option<String> = None;
        let mut mcp_tool_name: Option<String> = None;
        let mut mcp_lookup_error: Option<anyhow::Error> = None;

        // Check if it's a standard tool first
        if let Some(registration) = self.inventory.registration_for(&tool_name) {
            needs_pty = registration.uses_pty();
            tool_exists = true;
        }
        // If not a standard tool, check if it's an MCP tool
        if let Some((provider, remote_tool)) = parse_canonical_mcp_tool_name(&tool_name) {
            needs_pty = true;
            tool_exists = true;
            is_mcp_tool = true;
            mcp_provider = Some(provider.to_string());
            mcp_tool_name = Some(remote_tool.to_string());
        }

        let mcp_client_opt = self.mcp_client.read().clone();
        if !is_mcp_tool && let Some(mcp_client) = mcp_client_opt {
            let mut resolved_mcp_name = legacy_mcp_tool_name(name)
                .map(str::to_string)
                .unwrap_or_else(|| tool_name_owned.clone());

            if let Some(alias_target) = self.resolve_mcp_tool_alias(&resolved_mcp_name).await
                && alias_target != resolved_mcp_name
            {
                trace!(
                    requested = %resolved_mcp_name,
                    resolved = %alias_target,
                    "Resolved MCP tool alias"
                );
                resolved_mcp_name = alias_target;
            }

            match mcp_client.has_mcp_tool(&resolved_mcp_name).await {
                Ok(true) => {
                    needs_pty = true;
                    tool_exists = true;
                    is_mcp_tool = true;
                    mcp_provider = self.find_mcp_provider(&resolved_mcp_name).await;
                    mcp_tool_name = Some(resolved_mcp_name);
                }
                Ok(false) => {
                    // Don't modify tool_exists here - keep the result from standard tool check.
                    // Setting tool_exists = false would incorrectly override a valid standard tool.
                }
                Err(err) => {
                    warn!("Error checking MCP tool '{}': {}", resolved_mcp_name, err);
                    mcp_lookup_error = Some(err);
                }
            }
        }

        // If tool doesn't exist in either registry, return an error
        if !tool_exists {
            if let Some(err) = mcp_lookup_error {
                let error = ToolExecutionError::with_original_error(
                    tool_name_owned.clone(),
                    ToolErrorType::ExecutionError,
                    format!("Failed to resolve MCP tool '{display_name}': {err}"),
                    err.to_string(),
                );

                record_failure(
                    tool_name_owned,
                    is_mcp_tool,
                    mcp_provider.clone(),
                    args_for_recording,
                    format!("Failed to resolve MCP tool '{display_name}': {err}"),
                    timeout_category_label.clone(),
                    base_timeout_ms,
                    adaptive_timeout_ms,
                    None,
                    false,
                );

                return Ok(error.to_json_value());
            }

            let (all_tool_names, similar_tools) = self.public_tool_catalog_for_error(name).await;
            let suggestion = if !similar_tools.is_empty() {
                format!(" Did you mean: {}?", similar_tools.join(", "))
            } else {
                String::new()
            };
            let available_tool_list = all_tool_names.join(", ");
            let message = format!("Unknown tool: {display_name}. Available tools: {available_tool_list}.{suggestion}");
            let error = ToolExecutionError::new(tool_name_owned.clone(), ToolErrorType::ToolNotFound, message.clone());

            record_failure(
                tool_name_owned,
                is_mcp_tool,
                mcp_provider.clone(),
                args_for_recording,
                message,
                timeout_category_label.clone(),
                base_timeout_ms,
                adaptive_timeout_ms,
                None,
                false,
            );

            return Ok(error.to_json_value());
        }

        // MP-3: Circuit breaker check for MCP tools
        if is_mcp_tool && !self.mcp_circuit_breaker.allow_request() {
            let diag = self.mcp_circuit_breaker.diagnostics();
            let error = ToolExecutionError::new(
                tool_name_owned.clone(),
                ToolErrorType::ExecutionError,
                format!("MCP circuit breaker {:?}; skipping execution", diag.status),
            );
            let payload = json!({
                "error": error.to_json_value(),
                "circuit_breaker_state": format!("{:?}", diag.status),
                "consecutive_failures": diag.consecutive_failures,
                "note": "MCP provider circuit breaker open; execution skipped",
                "last_failed_at_ago_ms": diag.last_failure_time
                    .map(|ts| ts.elapsed().as_millis() as u64),
                "current_timeout_seconds": diag.current_timeout.as_secs(),
                "mcp_provider": mcp_provider,
            });
            warn!(
                tool = %tool_name_owned,
                payload = %payload,
                "Skipping MCP tool execution due to circuit breaker"
            );
            self.execution_history.add_record(
                ToolExecutionRecord::failure(
                    tool_name_owned,
                    requested_name.clone(),
                    is_mcp_tool,
                    mcp_provider.clone(),
                    args_for_recording,
                    format!("MCP circuit breaker {:?}; execution skipped", diag.status),
                    context_snapshot.clone(),
                    timeout_category_label.clone(),
                    base_timeout_ms,
                    adaptive_timeout_ms,
                    None,
                    false,
                )
                .with_circuit_breaker_state(format!("{:?}", diag.status))
                .with_retry_after(diag.retry_after),
            );
            return Ok(payload);
        }

        trace!(
            tool = %tool_name,
            requested = %name,
            is_mcp = is_mcp_tool,
            uses_pty = needs_pty,
            alias = %if tool_name == name { "" } else { name },
            mcp_provider = %mcp_provider.as_deref().unwrap_or(""),
            "Resolved tool route"
        );

        // Start PTY session if needed (using RAII guard for automatic cleanup)
        let _pty_guard = if needs_pty {
            match self.start_pty_session() {
                Ok(guard) => Some(guard),
                Err(err) => {
                    let error = ToolExecutionError::with_original_error(
                        tool_name_owned.clone(),
                        ToolErrorType::ExecutionError,
                        "Failed to start PTY session".to_string(),
                        err.to_string(),
                    );

                    record_failure(
                        tool_name_owned,
                        is_mcp_tool,
                        mcp_provider.clone(),
                        args_for_recording,
                        "Failed to start PTY session".to_string(),
                        timeout_category_label.clone(),
                        base_timeout_ms,
                        adaptive_timeout_ms,
                        None,
                        false,
                    );

                    return Ok(error.to_json_value());
                }
            }
        } else {
            None
        };

        // Execute the appropriate tool based on its type
        // The _pty_guard will automatically decrement the session count when dropped
        let execution_started_at = Instant::now();
        // Explicit command waits enforce their own deadline inside the command
        // session executor. Do not wrap them in a second equal deadline here:
        // response draining and settlement need a little time after the child
        // wait returns, and an outer timeout must not terminate a reusable
        // in-progress session.
        let effective_timeout = if timeout_category == ToolTimeoutCategory::LongRunningCommand {
            None
        } else {
            self.effective_timeout(timeout_category)
        };
        let effective_timeout_ms = effective_timeout.map(|d| d.as_millis() as u64);

        let fail_open = self.optimization_config.tool_registry.middleware_fail_open;
        let middleware_req = ToolCallRequest {
            id: requested_name.clone(),
            tool_name: tool_name.as_str().into(),
            args: args.clone(),
            metadata: None,
        };
        if let Err(err) = self.middleware.before_execute_opt(&middleware_req, fail_open).await {
            if !fail_open {
                let error_msg = format!("Middleware denied execution: {err}");
                record_failure(
                    tool_name_owned.clone(),
                    is_mcp_tool,
                    mcp_provider.clone(),
                    args_for_recording.clone(),
                    error_msg.clone(),
                    timeout_category_label.clone(),
                    base_timeout_ms,
                    adaptive_timeout_ms,
                    None,
                    false,
                );
                return Err(anyhow!(error_msg).context("tool denied by middleware"));
            }
        }

        let exec_future = async {
            if is_mcp_tool {
                let mcp_name = mcp_tool_name
                    .as_deref()
                    .context("MCP tool routing inconsistency: resolved MCP tool name missing")?;
                self.execute_mcp_tool(mcp_name, args).await
            } else if exec_settlement_mode.settle_noninteractive()
                && matches!(tool_name.as_str(), tools::UNIFIED_EXEC | tools::EXEC_COMMAND | tools::EXEC_PTY_CMD)
            {
                let exec_args = match tool_name.as_str() {
                    tools::EXEC_COMMAND => super::executors::normalize_command_session_run_alias_args(&args, false)?,
                    tools::EXEC_PTY_CMD => super::executors::normalize_command_session_run_alias_args(&args, true)?,
                    _ => args.clone(),
                };
                if self.optimization_config.memory_pool.enabled {
                    let _execution_guard = self.memory_pool.get_value();
                    let _string_guard = self.memory_pool.get_string();
                    let _vec_guard = self.memory_pool.get_vec();
                    self.execute_command_session_internal(exec_args, exec_settlement_mode).await
                } else {
                    self.execute_command_session_internal(exec_args, exec_settlement_mode).await
                }
            } else if exec_settlement_mode.settle_noninteractive() && tool_name == tools::WRITE_STDIN {
                let (exec_args, dispatch) = super::executors::normalize_write_stdin_args(&args)?;
                match dispatch {
                    crate::tools::command_args::WriteStdinDispatch::Write => {
                        self.execute_command_session_write_for_tool(exec_args, tools::WRITE_STDIN).await
                    }
                    crate::tools::command_args::WriteStdinDispatch::Poll => {
                        self.execute_command_session_poll_for_tool(exec_args, exec_settlement_mode, tools::WRITE_STDIN)
                            .await
                    }
                    crate::tools::command_args::WriteStdinDispatch::Wait => {
                        self.execute_command_session_wait(exec_args).await
                    }
                }
            } else if let Some(registration) = self.inventory.registration_for(&tool_name) {
                // Log deprecation warning if tool is deprecated
                if registration.is_deprecated() {
                    if let Some(msg) = registration.deprecation_message() {
                        warn!("Tool '{}' is deprecated: {}", tool_name, msg);
                    } else {
                        warn!("Tool '{}' is deprecated and may be removed in a future version", tool_name);
                    }
                }

                let handler = registration.handler();
                match handler {
                    ToolHandler::RegistryFn(executor) => {
                        // PERFORMANCE OPTIMIZATION: Use memory pool for tool execution if enabled
                        if self.optimization_config.memory_pool.enabled {
                            let _execution_guard = self.memory_pool.get_value();
                            let _string_guard = self.memory_pool.get_string();
                            let _vec_guard = self.memory_pool.get_vec();
                            executor(self, args).await
                        } else {
                            executor(self, args).await
                        }
                    }
                    ToolHandler::TraitObject(tool) => {
                        // PERFORMANCE OPTIMIZATION: Use cached tool if available and optimizations enabled
                        if self.optimization_config.tool_registry.use_optimized_registry {
                            if let Some(cached_tool) = cached_tool.as_ref() {
                                // Use cached tool instance to avoid registry lookup overhead
                                cached_tool.execute(args).await
                            } else {
                                // Cache the tool for future use
                                self.hot_tool_cache.write().put(tool_name.clone(), tool.clone());
                                tool.execute(args).await
                            }
                        } else {
                            tool.execute(args).await
                        }
                    }
                }
            } else {
                // This should theoretically never happen since we checked tool_exists above
                // Generate helpful error message with available tools
                let (tool_names, similar_tools) = self.public_tool_catalog_for_error(&requested_name).await;
                let available_tool_list = tool_names.join(", ");

                let suggestion = if !similar_tools.is_empty() {
                    format!(" Did you mean: {}?", similar_tools.join(", "))
                } else {
                    String::new()
                };

                let error_msg = format!(
                    "Tool '{display_name}' not found in registry. Available tools: {available_tool_list}.{suggestion}"
                );

                let error =
                    ToolExecutionError::new(tool_name_owned.clone(), ToolErrorType::ToolNotFound, error_msg.clone());

                record_failure(
                    tool_name_owned.clone(),
                    is_mcp_tool,
                    mcp_provider.clone(),
                    args_for_recording.clone(),
                    error_msg,
                    timeout_category_label.clone(),
                    base_timeout_ms,
                    adaptive_timeout_ms,
                    effective_timeout_ms,
                    false,
                );

                Ok(error.to_json_value())
            }
        };

        let result = if let Some(limit) = effective_timeout {
            trace!(
                tool = %tool_name_owned,
                category = %timeout_category.label(),
                timeout_ms = %limit.as_millis(),
                "Executing tool with effective timeout"
            );
            match tokio::time::timeout(limit, exec_future).await {
                Ok(res) => res,
                Err(_) => {
                    let timeout_ms = limit.as_millis() as u64;
                    let tripped = self.record_tool_failure(timeout_category);
                    if tripped {
                        warn!(
                            tool = %tool_name_owned,
                            category = %timeout_category.label(),
                            "Tool circuit breaker tripped after consecutive timeout failures"
                        );
                    }
                    let retry_after = self.should_circuit_break(timeout_category);

                    let mut timeout_error = ToolExecutionError::new(
                        tool_name_owned.clone(),
                        ToolErrorType::Timeout,
                        format!(
                            "Operation '{}' exceeded the {} timeout ceiling ({}s)",
                            tool_name_owned,
                            timeout_category.label(),
                            limit.as_secs()
                        ),
                    )
                    .with_tool_call_context(&tool_name_owned, &args_for_recording)
                    .with_surface("tool_registry")
                    .with_debug_metadata("timeout_category", timeout_category.label())
                    .with_debug_metadata("timeout_ms", timeout_ms.to_string());

                    if tool_name_owned == tools::UNIFIED_EXEC {
                        timeout_error.recovery_suggestions = vec![
                            Cow::Borrowed("Use write_stdin with empty chars to poll command progress"),
                            Cow::Borrowed("Use exec_command with a fresh command if the original session is stale"),
                            Cow::Borrowed("Ask for manual cleanup if a stale session is still active"),
                        ];
                    }

                    if let Some(delay) = retry_after {
                        timeout_error.retry_after_ms = Some(delay.as_millis().min(u128::from(u64::MAX)) as u64);
                    }

                    let mut timeout_payload = timeout_error.to_json_value();
                    Self::annotate_timeout_error_payload(
                        &mut timeout_payload,
                        timeout_category.label(),
                        timeout_ms,
                        tripped,
                    );

                    if let Some(breaker) = shared_circuit_breaker.as_ref() {
                        breaker.record_failure_category_for_tool(&tool_name_owned, ErrorCategory::Timeout);
                    }
                    if is_mcp_tool {
                        self.mcp_circuit_breaker.record_failure_category(ErrorCategory::Timeout);
                    }
                    record_failure(
                        tool_name_owned,
                        is_mcp_tool,
                        mcp_provider,
                        args_for_recording,
                        timeout_error.user_message(),
                        timeout_category_label.clone(),
                        base_timeout_ms,
                        adaptive_timeout_ms,
                        Some(timeout_ms),
                        tripped,
                    );
                    return Ok(timeout_payload);
                }
            }
        } else {
            exec_future.await
        };

        // PTY session will be automatically cleaned up when _pty_guard is dropped

        // Handle the execution result and record it

        match result {
            Ok(value) => {
                if let Some(breaker) = shared_circuit_breaker.as_ref() {
                    breaker.record_success_for_tool(&tool_name_owned);
                }
                if is_mcp_tool {
                    self.mcp_circuit_breaker.record_success();
                }
                self.reset_tool_failure(timeout_category);
                let should_decay = {
                    let mut state = self.resiliency.lock();
                    let success_streak = state.adaptive_tuning.success_streak;
                    if let Some(counter) = state.success_trackers.get_mut(&timeout_category) {
                        *counter = counter.saturating_add(1);
                        let counter_val = *counter;
                        if counter_val >= success_streak {
                            *counter = 0;
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                };
                if should_decay {
                    self.decay_adaptive_timeout(timeout_category);
                }
                self.record_tool_latency(timeout_category, execution_started_at.elapsed());
                // Dynamic context discovery: spool large outputs to files
                let mut value = value;
                if tool_intent::is_spool_file_read_command(&tool_name_owned, &args_for_recording) {
                    if let Some(output) = value.as_object_mut() {
                        output.insert("no_spool".to_string(), json!(true));
                    } else {
                        // `process_tool_output` can spool scalar/array values
                        // based on their serialized size. Wrap an unusual
                        // scalar result before that boundary so a safe spool
                        // inspection can never create a nested spool reference.
                        value = json!({"output": value, "no_spool": true});
                    }
                }
                let processed_value = self
                    .process_tool_output(&tool_name_owned, value, is_mcp_tool, max_output_tokens)
                    .await;
                let mut normalized_value = normalize_tool_output(processed_value);
                if tool_name_owned == tools::CODE_SEARCH
                    && let Some(output) = normalized_value.as_object_mut()
                {
                    output.remove("success");
                }
                let structured_error = structured_tool_output_error(&normalized_value);

                if !readonly_classification {
                    // Invalidate only the cache records whose read target could
                    // overlap the mutated file(s).  Wiping the entire history
                    // (previous behavior) defeated cross-turn dedup: any write
                    // tool call would discard unrelated read-only cache hits,
                    // forcing the model to re-read files whose contents hadn't
                    // changed at all.
                    let targets = mutated_target_paths(&tool_name_owned, &args_for_recording);
                    if targets.is_empty() && is_pathless_mutating_command(&tool_name_owned) {
                        // A mutating shell command with no identifiable target
                        // (e.g. `sed -i`, `cargo build`) could have touched any
                        // file. Conservatively drop every cached read so no
                        // record serves stale content.
                        self.execution_history.invalidate_all_reads();
                    } else {
                        for target in targets {
                            self.execution_history.invalidate_for_path(&target);
                        }
                    }
                }

                if let Some(error_msg) = structured_error {
                    self.execution_history.add_record(ToolExecutionRecord::failure(
                        tool_name_owned,
                        requested_name,
                        is_mcp_tool,
                        mcp_provider,
                        args_for_recording,
                        error_msg,
                        context_snapshot.clone(),
                        timeout_category_label.clone(),
                        base_timeout_ms,
                        adaptive_timeout_ms,
                        effective_timeout_ms,
                        false,
                    ));
                } else {
                    self.execution_history.add_record(ToolExecutionRecord::success(
                        tool_name_owned,
                        requested_name,
                        is_mcp_tool,
                        mcp_provider,
                        args_for_recording,
                        normalized_value.clone(),
                        context_snapshot.clone(),
                        timeout_category_label.clone(),
                        base_timeout_ms,
                        adaptive_timeout_ms,
                        effective_timeout_ms,
                        false,
                    ));
                }

                let _ = self
                    .middleware
                    .after_execute(
                        &middleware_req,
                        &ToolCallResponse {
                            id: middleware_req.id.clone(),
                            success: true,
                            result: Some(normalized_value.clone()),
                            error: None,
                            duration_ms: Some(execution_started_at.elapsed().as_millis() as u64),
                            cache_hit: None,
                        },
                    )
                    .await;

                Ok(normalized_value)
            }
            Err(err) => {
                // Reentrancy violations must surface as hard errors rather
                // than wrapped Ok(error_object) so nested callers see the
                // failure and can react appropriately.
                if err.to_string().contains("tool reentrancy blocked") {
                    return Err(err);
                }

                let error = ToolExecutionError::from_anyhow(
                    tool_name_owned.clone(),
                    &err,
                    0,
                    false,
                    false,
                    Some("tool_registry"),
                )
                .with_tool_call_context(&tool_name_owned, &args_for_recording);
                let error_category = error.category;
                if let Some(breaker) = shared_circuit_breaker.as_ref() {
                    breaker.record_failure_category_for_tool(&tool_name_owned, error_category);
                }
                if is_mcp_tool {
                    self.mcp_circuit_breaker.record_failure_category(error_category);
                }

                let tripped = if error_category.should_trip_circuit_breaker() {
                    let tripped = self.record_tool_failure(timeout_category);
                    if tripped {
                        warn!(
                            tool = %tool_name_owned,
                            category = %timeout_category.label(),
                            "Tool circuit breaker tripped after consecutive failures"
                        );
                    }
                    tripped
                } else {
                    false
                };

                let mut payload = error.to_json_value();
                Self::annotate_timeout_error_payload(
                    &mut payload,
                    timeout_category.label(),
                    effective_timeout_ms.unwrap_or(0),
                    tripped,
                );

                record_failure(
                    tool_name_owned,
                    is_mcp_tool,
                    mcp_provider,
                    args_for_recording,
                    format!("Tool execution failed: {err}"),
                    timeout_category_label.clone(),
                    base_timeout_ms,
                    adaptive_timeout_ms,
                    effective_timeout_ms,
                    tripped,
                );

                let _ = self
                    .middleware
                    .on_error(
                        &middleware_req,
                        &UnifiedToolError::new(
                            UnifiedErrorKind::from(vtcode_commons::classify_anyhow_error(&err)),
                            err.to_string(),
                        ),
                    )
                    .await;

                Ok(payload)
            }
        }
    }
}
