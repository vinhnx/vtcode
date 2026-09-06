use super::AgentRunner;
use crate::config::constants::tools;
use crate::core::agent::harness_kernel::PreparedToolCall;
use crate::core::agent::session::AgentSessionState;
use crate::permissions::{
    ResolvedPermissionDecision, build_advertised_permission_requests, build_permission_request,
    evaluate_effective_permissions, evaluate_permissions,
};
use crate::primary_agent::primary_agent_allows_tool;
use crate::tools::file_ops::restore_exact_text_content;
use crate::tools::registry::{ExecutionPolicySnapshot, ToolErrorType, ToolExecutionError};
use crate::tools::{command_args, tool_intent};
use anyhow::{Result, bail};
use serde_json::Value;
use tracing::{info, warn};

fn restore_exact_file_read_output(mut output: Value) -> Value {
    let Some(obj) = output.as_object_mut() else {
        return output;
    };

    let is_text_file_read = obj
        .get("content_kind")
        .and_then(Value::as_str)
        .is_some_and(|value| value == "text")
        && obj.get("path").and_then(Value::as_str).is_some();
    if !is_text_file_read {
        return output;
    }

    let Some(content) = obj.get("content").and_then(Value::as_str) else {
        return output;
    };
    let Some(size_bytes) = obj
        .get("metadata")
        .and_then(|metadata| metadata.get("data"))
        .and_then(|data| data.get("size_bytes"))
        .and_then(Value::as_u64)
    else {
        return output;
    };

    if let Some(exact_content) = restore_exact_text_content(content, size_bytes) {
        obj.insert("content".to_string(), Value::String(exact_content));
    }

    output
}

impl AgentRunner {
    pub(super) async fn resolve_executable_tool_name(&self, tool_name: &str) -> Option<String> {
        let canonical_name = self.tool_registry.resolve_public_tool_name(tool_name).ok()?;

        self.is_tool_exposed(&canonical_name).await.then_some(canonical_name)
    }

    pub(super) fn admit_tool_call(
        &self,
        tool_name: &str,
        args: Value,
        session_state: &mut AgentSessionState,
    ) -> Result<PreparedToolCall> {
        let normalized_args = self.normalize_tool_args(tool_name, args, session_state);
        self.ensure_active_primary_agent_allows_tool_call(tool_name, &normalized_args)?;
        self.tool_registry.admit_public_tool_call(tool_name, &normalized_args)
    }

    fn ensure_active_primary_agent_allows_tool_call(&self, tool_name: &str, args: &Value) -> Result<()> {
        let Some(active_primary_agent) = self.active_primary_agent.as_ref() else {
            return Ok(());
        };

        let normalized_tool_name = self
            .tool_registry
            .resolve_public_tool_name(tool_name)
            .unwrap_or_else(|_| tool_name.to_string());

        if !primary_agent_allows_tool(active_primary_agent, &normalized_tool_name) {
            bail!(
                "Tool '{}' is not permitted by active primary agent '{}'",
                normalized_tool_name,
                active_primary_agent.identity.name
            );
        }

        let current_dir = std::env::current_dir().unwrap_or_else(|_| self._workspace.clone());
        let permission_request =
            build_permission_request(&self._workspace, &current_dir, &normalized_tool_name, Some(args));
        let decision = evaluate_effective_permissions(
            &self.config().permissions,
            &active_primary_agent.permissions,
            &self._workspace,
            &current_dir,
            &permission_request,
        );

        if decision == ResolvedPermissionDecision::Deny {
            bail!(
                "Tool '{}' is denied by active primary agent '{}'",
                normalized_tool_name,
                active_primary_agent.identity.name
            );
        }

        Ok(())
    }

    /// Check if a tool is allowed for this agent
    async fn is_tool_allowed(&self, tool_name: &str) -> bool {
        if self.local_tools_only
            && self.tool_registry.tool_network_access(tool_name) != crate::tools::registry::ToolNetworkAccess::Local
        {
            return false;
        }
        let policy = self.tool_registry.get_tool_policy(tool_name).await;
        matches!(policy, crate::tool_policy::ToolPolicy::Allow | crate::tool_policy::ToolPolicy::Prompt)
    }

    /// Check if a tool is exposed to the active runtime after feature gating.
    pub(super) async fn is_tool_exposed(&self, tool_name: &str) -> bool {
        if !self
            .features()
            .allows_tool_name(tool_name, self.tool_registry.is_planning_active(), false)
        {
            return false;
        }

        self.is_tool_permitted_for_advertisement(tool_name).await
    }

    /// Check policy and permission gates without applying runtime mode filtering.
    pub(super) async fn is_tool_permitted_for_advertisement(&self, tool_name: &str) -> bool {
        if let Some(active_primary_agent) = self.active_primary_agent.as_ref()
            && !primary_agent_allows_tool(active_primary_agent, tool_name)
        {
            return false;
        }

        let current_dir = std::env::current_dir().unwrap_or_else(|_| self._workspace.clone());
        if tool_name == tools::EXEC_COMMAND {
            let bash_probe_args = serde_json::json!({ "cmd": "true" });
            let bash_probe =
                build_permission_request(&self._workspace, &current_dir, tool_name, Some(&bash_probe_args));
            if evaluate_permissions(&self.config().permissions, &self._workspace, &current_dir, &bash_probe).deny {
                return false;
            }
            if let Some(active_primary_agent) = self.active_primary_agent.as_ref()
                && evaluate_effective_permissions(
                    &self.config().permissions,
                    &active_primary_agent.permissions,
                    &self._workspace,
                    &current_dir,
                    &bash_probe,
                ) == ResolvedPermissionDecision::Deny
            {
                return false;
            }
        }
        let advertised_requests = if tool_name == tools::EXEC_COMMAND {
            Vec::new()
        } else {
            build_advertised_permission_requests(&self._workspace, &current_dir, tool_name)
        };
        if advertised_requests.iter().any(|request| {
            evaluate_permissions(&self.config().permissions, &self._workspace, &current_dir, request).deny
        }) {
            return false;
        }

        if let Some(active_primary_agent) = self.active_primary_agent.as_ref() {
            let permission_denied = advertised_requests.iter().any(|permission_request| {
                evaluate_effective_permissions(
                    &self.config().permissions,
                    &active_primary_agent.permissions,
                    &self._workspace,
                    &current_dir,
                    permission_request,
                ) == ResolvedPermissionDecision::Deny
            });
            if permission_denied {
                return false;
            }
        }

        if self.tool_registry.is_denied_in_full_auto(tool_name).await {
            return false;
        }

        self.is_tool_allowed(tool_name).await
    }

    /// Validate if a tool name is safe, registered, and allowed by policy
    #[inline]
    pub(super) async fn is_valid_tool(&self, tool_name: &str) -> bool {
        self.resolve_executable_tool_name(tool_name).await.is_some()
    }

    /// Execute a prepared tool call, returning `(output, attempt_count)`.
    /// The attempt count reflects retries performed inside the tool registry,
    /// so callers can emit retry/recovery observability events.
    pub(super) async fn execute_prepared_tool_internal(
        &self,
        prepared: &PreparedToolCall,
    ) -> std::result::Result<(Value, u32), ToolExecutionError> {
        let resolved_tool_name = prepared.canonical_name.as_str();
        let args = &prepared.effective_args;
        let shell_command = if tool_intent::is_command_run_tool_call(resolved_tool_name, args)
            || (resolved_tool_name == tools::UNIFIED_EXEC && tool_intent::command_session_action(args).is_none())
        {
            command_args::command_text(args).ok().flatten()
        } else {
            None
        };

        // Enforce per-agent shell policies for shell-executed commands.
        if let Some(cmd_text) = shell_command {
            let cfg = self.config();

            let agent_prefix = format!("VTCODE_{}_COMMANDS_", self.agent_type.to_string().to_uppercase());

            let deny_regex_patterns = crate::utils::merge_env_patterns(
                &cfg.commands.deny_regex,
                &format!("{}{}", agent_prefix, "DENY_REGEX"),
            );
            let deny_glob_patterns =
                crate::utils::merge_env_patterns(&cfg.commands.deny_glob, &format!("{}{}", agent_prefix, "DENY_GLOB"));

            self.tool_registry
                .check_shell_policy(&cmd_text, &deny_regex_patterns, &deny_glob_patterns)
                .map_err(|err| {
                    ToolExecutionError::policy_violation(
                        resolved_tool_name.to_string(),
                        format!("tool denied by policy: {err}"),
                    )
                    .with_surface("agent_runner")
                })?;

            info!(target = "policy", agent = ?self.agent_type, tool = resolved_tool_name, cmd = %cmd_text, "shell_policy_checked");
        }

        let mut policy = ExecutionPolicySnapshot::default()
            .with_prevalidated(prepared.already_preflighted)
            .with_max_retries(self.config().agent.harness.max_tool_retries as usize);
        policy.retry_jitter = 0.15;

        // Enforce the harness tool wall-clock budget per call. This is the
        // single chokepoint for both parallel and sequential batch paths. On
        // elapsed, surface a `Timeout` error so the call flows through the
        // existing retry/fallback handling — no new state type is introduced.
        // `0` disables the bound and preserves the previous unbounded behaviour.
        let wall_clock_secs = self.config().agent.harness.max_tool_wall_clock_secs;
        let outcome = if wall_clock_secs > 0 {
            let budget = std::time::Duration::from_secs(wall_clock_secs);
            match tokio::time::timeout(
                budget,
                self.tool_registry.execute_prepared_public_tool_request(prepared, policy),
            )
            .await
            {
                Ok(outcome) => outcome,
                Err(_elapsed) => {
                    warn!(
                        agent = ?self.agent_type,
                        tool = resolved_tool_name,
                        budget_secs = wall_clock_secs,
                        "tool execution exceeded the harness wall-clock budget; aborting"
                    );
                    return Err(ToolExecutionError::new(
                        resolved_tool_name.to_string(),
                        ToolErrorType::Timeout,
                        format!("tool execution exceeded the harness wall-clock budget of {wall_clock_secs}s"),
                    )
                    .with_surface("agent_runner"));
                }
            }
        } else {
            self.tool_registry.execute_prepared_public_tool_request(prepared, policy).await
        };
        let attempts = outcome.attempts;
        match (outcome.output, outcome.error) {
            (Some(output), None) => Ok((restore_exact_file_read_output(output), attempts)),
            (_, Some(error)) => Err(error.with_surface("agent_runner")),
            _ => Err(ToolExecutionError::policy_violation(
                resolved_tool_name.to_string(),
                "tool execution failed without output or error",
            )
            .with_surface("agent_runner")),
        }
    }

    /// Internal tool execution, skipping validation.
    /// Use when `is_valid_tool` has already been called by the caller.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Internal validation-bypassing path is exercised only by tests and trusted callers."
        )
    )]
    pub(super) async fn execute_tool_internal(
        &self,
        tool_name: &str,
        args: &Value,
    ) -> std::result::Result<Value, ToolExecutionError> {
        let prepared = self.tool_registry.admit_public_tool_call(tool_name, args).map_err(|error| {
            ToolExecutionError::from_anyhow(tool_name, &error, 0, false, false, Some("agent_runner"))
                .with_tool_call_context(tool_name, args)
        })?;
        self.execute_prepared_tool_internal(&prepared)
            .await
            .map(|(value, _attempts)| value)
    }
}

#[cfg(test)]
mod tests {
    use crate::retry::{RetryPolicy, RetryPolicyCoreExt};
    use std::time::Duration;
    use vtcode_commons::ErrorCategory;

    /// Verify that the canonical retry policy correctly identifies transient
    /// errors as retryable via the `ErrorCategory` classifier.
    #[test]
    fn policy_retries_transient_errors() {
        let policy = RetryPolicy::new(3, Duration::from_millis(200), Duration::from_secs(2), 2.0);

        let network_err = anyhow::anyhow!("network connection dropped");
        let decision = policy.decision_for_anyhow(&network_err, 0, Some("test_tool"));
        assert!(decision.retryable, "network errors should be retryable");
        assert_eq!(decision.category, ErrorCategory::Network);

        let timeout_err = anyhow::anyhow!("operation timed out");
        let decision = policy.decision_for_anyhow(&timeout_err, 0, Some("test_tool"));
        assert!(decision.retryable, "timeout errors should be retryable");
        assert_eq!(decision.category, ErrorCategory::Timeout);

        let rate_limit_err = anyhow::anyhow!("429 Too Many Requests");
        let decision = policy.decision_for_anyhow(&rate_limit_err, 0, Some("test_tool"));
        assert!(decision.retryable, "rate limit errors should be retryable");
    }

    /// Verify that non-retryable errors fail fast without retry.
    #[test]
    fn policy_does_not_retry_permanent_errors() {
        let policy = RetryPolicy::new(3, Duration::from_millis(200), Duration::from_secs(2), 2.0);

        let policy_err = anyhow::anyhow!("tool denied by policy");
        let decision = policy.decision_for_anyhow(&policy_err, 0, Some("test_tool"));
        assert!(!decision.retryable, "policy violations should not be retryable");

        let auth_err = anyhow::anyhow!("invalid api key");
        let decision = policy.decision_for_anyhow(&auth_err, 0, Some("test_tool"));
        assert!(!decision.retryable, "authentication errors should not be retryable");

        let param_err = anyhow::anyhow!("invalid arguments: missing required field");
        let decision = policy.decision_for_anyhow(&param_err, 0, Some("test_tool"));
        assert!(!decision.retryable, "invalid parameter errors should not be retryable");
    }

    /// Verify that retryable decisions include backoff delays.
    #[test]
    fn policy_provides_backoff_delays() {
        let policy = RetryPolicy::new(3, Duration::from_millis(200), Duration::from_secs(2), 2.0);

        let err = anyhow::anyhow!("network connection dropped");

        let d0 = policy.decision_for_anyhow(&err, 0, Some("test_tool"));
        let d1 = policy.decision_for_anyhow(&err, 1, Some("test_tool"));

        assert!(d0.delay.is_some(), "first retry should have a delay");
        assert!(d1.delay.is_some(), "second retry should have a delay");
        assert!(d1.delay.unwrap_or_default() >= d0.delay.unwrap_or_default(), "backoff should increase");
    }
}
