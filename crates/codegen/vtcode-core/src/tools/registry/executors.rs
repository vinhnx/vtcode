use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, anyhow, bail};
#[cfg(test)]
use cargo_failure_diagnostics::{
    CargoTestCommandKind, attach_exec_recovery_guidance, attach_failure_diagnostics_metadata,
    cargo_selector_error_diagnostics, cargo_test_failure_diagnostics, cargo_test_rerun_hint,
};
use chrono;
use exec_support::*;
use futures::future::BoxFuture;
use hashbrown::HashMap;
use sandbox_runtime::*;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::{ExecSettlementMode, ToolRegistry};
use crate::config::constants::tools;
use crate::tools::file_tracker::FileTracker;
use crate::tools::registry::unified_actions::CommandSessionAction;
use crate::tools::{native_memory, tool_intent};

mod cargo_failure_diagnostics;
mod exec_command;
mod exec_output;
mod exec_sessions;
mod exec_support;
mod patch_pipeline;
mod sandbox_runtime;
mod search_introspection;
mod subagents;

pub use sandbox_runtime::sandbox_policy_from_runtime_config;

#[derive(Clone, Copy)]
enum ExecRunBackendKind {
    Pty,
    Pipe,
}

struct PreparedExecRunRequest {
    prepared_command: PreparedExecCommand,
    working_dir_path: PathBuf,
    output_config: ExecRunOutputConfig,
    yield_duration: Duration,
    session_id: String,
    shell_program: String,
    env_overrides: HashMap<String, String>,
    is_git_diff: bool,
    confirm: bool,
    rows: Option<u16>,
    cols: Option<u16>,
    sandbox_active: bool,
}

struct ResolvedExecSandboxRequest {
    working_dir_path: PathBuf,
    sandbox_permissions: crate::sandboxing::SandboxPermissions,
    additional_permissions: Option<crate::sandboxing::AdditionalPermissions>,
}

fn set_payload_default(payload: &mut serde_json::Map<String, Value>, key: &str, value: Value) {
    payload.entry(key.to_string()).or_insert(value);
}

pub(super) fn normalize_command_session_run_alias_args(args: &Value, tty: bool) -> Result<Value> {
    let mut args = crate::tools::command_args::normalize_shell_args(args).map_err(|error| anyhow!(error))?;
    if let Some(payload) = args.as_object_mut() {
        set_payload_default(payload, "action", json!("run"));
        if tty {
            set_payload_default(payload, "tty", json!(true));
        }
    }
    Ok(args)
}

fn with_command_session_action_default(mut args: Value, action: &'static str) -> Value {
    if let Some(payload) = args.as_object_mut() {
        set_payload_default(payload, "action", json!(action));
    }
    args
}

pub(super) fn normalize_write_stdin_args(
    args: &Value,
) -> Result<(Value, crate::tools::command_args::WriteStdinDispatch)> {
    let dispatch = crate::tools::command_args::write_stdin_dispatch(args).map_err(|error| anyhow!(error))?;
    let mut args = crate::tools::command_args::normalize_shell_args(args).map_err(|error| anyhow!(error))?;
    let payload = args
        .as_object_mut()
        .ok_or_else(|| anyhow!("write_stdin requires a JSON object"))?;
    payload.insert("action".to_string(), json!(dispatch.command_session_action()));
    if dispatch == crate::tools::command_args::WriteStdinDispatch::Poll {
        payload.remove("input");
    }
    Ok((args, dispatch))
}

fn annotate_exec_run_response(response: &mut Value, is_git_diff: bool) {
    if is_git_diff {
        response["no_spool"] = json!(true);
        response["content_type"] = json!("git_diff");
    }
}

fn acquire_executor_rate_limit(bucket: &str, multiplier: f64) -> Result<()> {
    let mut guard = crate::tools::rate_limiter::PER_TOOL_RATE_LIMITER
        .lock()
        .map_err(|err| anyhow!("per-tool rate limiter poisoned: {err}"))?;
    guard
        .try_acquire_for_scaled(bucket, multiplier)
        .map_err(|e| anyhow!("tool rate limit exceeded for {bucket}: {e}"))
}

fn parse_action<T>(action_str: &str) -> Result<T>
where
    T: DeserializeOwned,
{
    serde_json::from_value(json!(action_str)).with_context(|| format!("Invalid action: {action_str}"))
}

/// Generate an executor that delegates to a cloned tool instance from the inventory.
macro_rules! delegate_to_tool {
    ($name:ident, $tool_accessor:ident, $method:ident) => {
        pub(super) fn $name(&self, args: Value) -> BoxFuture<'_, Result<Value>> {
            let tool = self.inventory.$tool_accessor().clone();
            Box::pin(async move { tool.$method(args).await })
        }
    };
}

/// Generate an executor that delegates to an async method on `self`.
macro_rules! delegate_to_self {
    ($name:ident, $method:ident) => {
        pub(super) fn $name(&self, args: Value) -> BoxFuture<'_, Result<Value>> {
            Box::pin(async move { self.$method(args).await })
        }
    };
}

impl ToolRegistry {
    /// Unified `cron` executor: dispatches on `action` (create | list | delete).
    /// For legacy alias calls that omit `action`, the action is inferred from
    /// the argument shape: `prompt` implies create, `id` implies delete,
    /// otherwise list.
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "Legacy executor aliases remain available for compatibility tests."
        )
    )]
    pub(super) fn cron_executor(&self, args: Value) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move {
            let action = args
                .get("action")
                .and_then(Value::as_str)
                .map(str::to_ascii_lowercase)
                .unwrap_or_else(|| {
                    if args.get("prompt").is_some() {
                        "create".to_string()
                    } else if args.get("id").is_some() {
                        "delete".to_string()
                    } else {
                        "list".to_string()
                    }
                });
            match action.as_str() {
                "create" => self.cron_create_executor(args).await,
                "list" => self.cron_list_executor(args).await,
                "delete" => self.cron_delete_executor(args).await,
                other => bail!(
                    "cron: unknown action '{other}'. Use action='create' (schedule a prompt), 'list', or 'delete' (requires id)."
                ),
            }
        })
    }

    fn cron_create_executor(&self, args: Value) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move {
            let prompt = args
                .get("prompt")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow!("cron_create requires a non-empty prompt"))?
                .to_string();
            let name = args.get("name").and_then(Value::as_str).map(ToOwned::to_owned);
            let cron = args.get("cron").and_then(Value::as_str);
            let delay_minutes = args.get("delay_minutes").and_then(Value::as_u64);
            let run_at = args.get("run_at").and_then(Value::as_str);

            let schedule = match (cron, delay_minutes, run_at) {
                (Some(expression), None, None) => crate::scheduler::ScheduleSpec::cron5(expression)?,
                (None, Some(minutes), None) => crate::scheduler::ScheduleSpec::fixed_interval(Duration::from_secs(
                    minutes.checked_mul(60).ok_or_else(|| anyhow!("delay_minutes is too large"))?,
                ))?,
                (None, None, Some(raw)) => crate::scheduler::ScheduleSpec::one_shot(
                    crate::scheduler::parse_local_datetime(raw, chrono::Local::now())?,
                ),
                _ => bail!("Choose exactly one of cron, delay_minutes, or run_at"),
            };

            let summary = self
                .create_session_prompt_task(name, prompt, schedule, chrono::Utc::now())
                .await?;
            serde_json::to_value(summary).context("Failed to serialize cron_create response")
        })
    }

    fn cron_list_executor(&self, _args: Value) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move {
            Ok(json!({
                "tasks": self.list_session_tasks().await,
            }))
        })
    }

    fn cron_delete_executor(&self, args: Value) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move {
            let id = args
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow!("cron_delete requires id"))?;
            let deleted = self.delete_session_task(id).await;
            Ok(json!({
                "deleted": deleted.is_some(),
                "task": deleted,
            }))
        })
    }

    pub(super) fn memory_executor(&self, args: Value) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move {
            let workspace_root = self.workspace_root_owned();
            native_memory::execute_with_persistent_memory_config(
                &workspace_root,
                self.persistent_memory_config.as_ref(),
                self.persistent_memory_enabled,
                args,
            )
            .await
        })
    }

    pub async fn shell_run_approval_reason(
        &self,
        tool_name: &str,
        tool_args: Option<&Value>,
    ) -> Result<Option<String>> {
        let resolved_tool_name = self
            .resolve_public_tool_name_sync(tool_name)
            .unwrap_or_else(|_| tool_name.to_string());
        let Some(payload) = shell_run_payload(&resolved_tool_name, tool_args) else {
            return Ok(None);
        };

        let (requested_command, _) = parse_command_parts(
            payload,
            "shell run request requires a command",
            "shell run request command cannot be empty",
        )?;
        let sandbox_request = self.resolve_exec_sandbox_request(payload).await?;
        let sandbox_config = self.sandbox_config();
        let plan = build_shell_execution_plan(
            &sandbox_config,
            self.workspace_root(),
            &requested_command,
            sandbox_request.sandbox_permissions,
            sandbox_request.additional_permissions.as_ref(),
        )?;

        Ok(plan.approval_reason)
    }

    pub(super) fn code_search_executor(&self, args: Value) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move { self.execute_code_search(args).await })
    }

    async fn prepare_exec_run_request(
        &self,
        args: &Value,
        backend: ExecRunBackendKind,
        missing_error: &str,
        empty_error: &str,
    ) -> Result<PreparedExecRunRequest> {
        acquire_executor_rate_limit("exec_command:run", 2.0)?;

        let payload = args
            .as_object()
            .ok_or_else(|| anyhow!("command execution requires a JSON object"))?;

        let (command, auto_raw_command) = parse_command_parts(payload, missing_error, empty_error)?;
        let shell_program = match backend {
            ExecRunBackendKind::Pty => resolve_shell_preference_with_zsh_fork(
                payload.get("shell").and_then(|value| value.as_str()),
                self.pty_config(),
            )?,
            ExecRunBackendKind::Pipe => {
                resolve_shell_preference(payload.get("shell").and_then(|value| value.as_str()), self.pty_config())
            }
        };
        let login_shell = payload.get("login").and_then(|value| value.as_bool()).unwrap_or(false);
        let confirm = payload.get("confirm").and_then(|value| value.as_bool()).unwrap_or(false);

        let mut prepared_command =
            prepare_exec_command(payload, &shell_program, login_shell, command, auto_raw_command);
        let is_git_diff = is_git_diff_command(&prepared_command.requested_command);

        if !self.inventory.command_policy_allows(&prepared_command.requested_command) {
            return Err(anyhow!(
                "command '{}' is not permitted by the execution policy",
                prepared_command.requested_command_display
            ));
        }

        let sandbox_request = self.resolve_exec_sandbox_request(payload).await?;
        if sandbox_request.sandbox_permissions.requires_escalated_permissions() {
            // Fail closed: the plan's escalation approval_reason is exposed
            // for approval queries but no enforced approval flow consumes it
            // on this run path, so a model-supplied escalation must not
            // execute unsandboxed on its own justification string.
            return Err(anyhow!(
                "sandbox_permissions '{:?}' requires an enforced operator approval decision, which is not connected to this execution path; rerun without sandbox_permissions or wire the approval flow before escalating",
                sandbox_request.sandbox_permissions
            ));
        }
        let output_config = exec_run_output_config(payload, &prepared_command.display_command);

        enforce_pty_command_policy(&prepared_command.display_command, confirm)?;
        let sandbox_config = self.sandbox_config();
        let sandbox_plan = build_shell_execution_plan(
            &sandbox_config,
            self.workspace_root(),
            &prepared_command.requested_command,
            sandbox_request.sandbox_permissions,
            sandbox_request.additional_permissions.as_ref(),
        )?;
        let sandbox_active = sandbox_plan.sandbox_policy.is_some();
        prepared_command.command = apply_runtime_sandbox_to_command(
            prepared_command.command,
            &prepared_command.requested_command,
            &sandbox_config,
            self.workspace_root(),
            &sandbox_request.working_dir_path,
            sandbox_request.sandbox_permissions,
            sandbox_request.additional_permissions.as_ref(),
        )?;

        let rows = match backend {
            ExecRunBackendKind::Pty => {
                Some(parse_pty_dimension("rows", payload.get("rows"), self.pty_config().default_rows)?)
            }
            ExecRunBackendKind::Pipe => None,
        };
        let cols = match backend {
            ExecRunBackendKind::Pty => {
                Some(parse_pty_dimension("cols", payload.get("cols"), self.pty_config().default_cols)?)
            }
            ExecRunBackendKind::Pipe => None,
        };

        Ok(PreparedExecRunRequest {
            prepared_command,
            working_dir_path: sandbox_request.working_dir_path,
            output_config,
            yield_duration: Duration::from_millis(clamp_exec_yield_ms(
                payload.get("yield_time_ms").and_then(Value::as_u64),
                10_000,
            )),
            session_id: resolve_exec_run_session_id(payload)?,
            shell_program,
            env_overrides: parse_exec_env_overrides(payload)?,
            is_git_diff,
            confirm,
            rows,
            cols,
            sandbox_active,
        })
    }

    pub(super) async fn execute_command_session(&self, args: Value) -> Result<Value> {
        self.execute_command_session_internal(args, ExecSettlementMode::Manual).await
    }

    pub(super) async fn execute_harness_command_session_terminal_run_raw(&self, args: Value) -> Result<Value> {
        let args = normalize_command_session_run_alias_args(&args, true)?;
        self.execute_command_session_run_pty(args, true).await
    }

    fn dispatch_command_session_alias(&self, args: Value) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move { self.execute_command_session(args).await.map(super::normalize_tool_output) })
    }

    fn dispatch_command_session_run_alias(&self, args: Value, tty: bool) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move {
            let args = normalize_command_session_run_alias_args(&args, tty)?;
            self.execute_command_session(args).await.map(super::normalize_tool_output)
        })
    }

    fn dispatch_command_session_action_alias(&self, args: Value, action: &'static str) -> BoxFuture<'_, Result<Value>> {
        self.dispatch_command_session_alias(with_command_session_action_default(args, action))
    }

    pub(super) async fn execute_command_session_internal(
        &self,
        args: Value,
        exec_settlement_mode: ExecSettlementMode,
    ) -> Result<Value> {
        let args = crate::tools::command_args::normalize_shell_args(&args).map_err(|error| anyhow!(error))?;

        let action_str =
            tool_intent::command_session_action(&args).ok_or_else(|| missing_command_session_action_error(&args))?;
        let action: CommandSessionAction = parse_action(action_str)?;

        match action {
            CommandSessionAction::Run => self.execute_command_session_run_internal(args, exec_settlement_mode).await,
            CommandSessionAction::Write => self.execute_command_session_write(args).await,
            CommandSessionAction::Poll => self.execute_command_session_poll_internal(args, exec_settlement_mode).await,
            CommandSessionAction::Wait => self.execute_command_session_wait(args).await,
            CommandSessionAction::Continue => {
                self.execute_command_session_continue_internal(args, exec_settlement_mode).await
            }
            CommandSessionAction::Inspect => self.execute_command_session_inspect(args).await,
            CommandSessionAction::List => self.execute_command_session_list().await,
            CommandSessionAction::Close => self.execute_command_session_close(args).await,
            CommandSessionAction::Code => self.execute_code(args).await,
        }
    }

    async fn execute_command_session_run_internal(
        &self,
        args: Value,
        exec_settlement_mode: ExecSettlementMode,
    ) -> Result<Value> {
        let tty = args.get("tty").and_then(Value::as_bool).unwrap_or(false);
        if tty {
            self.execute_command_session_run_pty(args, false).await
        } else {
            self.execute_run_pipe_cmd(args, exec_settlement_mode).await
        }
    }

    async fn execute_code_search(&self, args: Value) -> Result<Value> {
        let request = serde_json::from_value(args).context("invalid code_search request")?;
        let response = crate::tools::code_search::execute(self.workspace_root(), request).await?;
        serde_json::to_value(response).context("failed to serialise code_search response")
    }

    async fn execute_code(&self, args: Value) -> Result<Value> {
        let code = args
            .get("command")
            .or_else(|| args.get("code"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing code/command in execute_code"))?;

        let language = code_language_from_args(&args);

        let track_files = args.get("track_files").and_then(|v| v.as_bool()).unwrap_or(false);

        let mcp_client = self.mcp_client().ok_or_else(|| anyhow!("MCP client not available"))?;

        let workspace_root = self.workspace_root_owned();
        // Expose built-in tools to the snippet as callable library functions
        // (curated, non-recursive subset) when a weak self-reference was
        // installed at session bootstrap.
        let builtin_executor = self.builtin_executor_for_code();
        let executor =
            crate::exec::code_executor::CodeExecutor::new(language, mcp_client.clone(), workspace_root.clone())
                .with_builtin_executor(builtin_executor);
        let execution_start = SystemTime::now();

        let result = executor.execute(code).await?;

        let mut response = json!(result);

        if track_files {
            let tracker = FileTracker::new(workspace_root);
            match tracker.detect_new_files(execution_start).await {
                Ok(changes) => {
                    response["generated_files"] = json!({
                        "count": changes.len(),
                        "files": changes,
                        "summary": tracker.generate_file_summary(&changes),
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "FileTracker failed to detect new files after code execution"
                    );
                }
            }
        }

        Ok(response)
    }

    async fn execute_apply_patch(&self, args: Value) -> Result<Value> {
        let (patch_args, patch_input_bytes, patch_base64) = self.prepare_apply_patch_args(args)?;
        let context = self.harness_context_snapshot();
        tracing::debug!(
            tool = tools::APPLY_PATCH,
            payload_bytes = serialized_payload_size_bytes(&patch_args),
            patch_input_bytes,
            patch_base64,
            patch_decoded_bytes = patch_args
                .get("input")
                .and_then(|v| v.as_str())
                .map(|s| s.len())
                .unwrap_or(0),
            session_id = %context.session_id,
            task_id = %context.task_id.as_deref().unwrap_or(""),
            "Prepared apply_patch payload"
        );

        self.execute_apply_patch_internal(patch_args).await
    }

    fn prepare_apply_patch_args(&self, args: Value) -> Result<(Value, usize, bool)> {
        let patch_input = crate::tools::apply_patch::decode_apply_patch_input(&args)?
            .ok_or_else(|| anyhow!("Missing patch input {}", crate::tools::error_helpers::PATCH_PARAMETER_HINT))?;
        let patch_input_bytes = patch_input.source_bytes;
        let patch_base64 = patch_input.was_base64;

        // Guard against a bare-string `args` (some callers pass the patch text
        // directly as a JSON string rather than an object). Indexing a
        // `Value::String` with `["input"]` panics in serde_json, so normalize
        // to an object first.
        let mut patch_args = if args.is_object() { args } else { json!({}) };
        patch_args["input"] = json!(patch_input.text);
        Ok((patch_args, patch_input_bytes, patch_base64))
    }

    async fn resolve_exec_sandbox_request(
        &self,
        payload: &serde_json::Map<String, Value>,
    ) -> Result<ResolvedExecSandboxRequest> {
        let working_dir_path = self.pty_manager().resolve_working_dir(shell_working_dir_value(payload)).await?;
        let sandbox_config = self.sandbox_config();
        let (sandbox_permissions, additional_permissions) =
            parse_requested_sandbox_permissions(payload, self.workspace_root(), &working_dir_path, &sandbox_config)
                .await?;

        Ok(ResolvedExecSandboxRequest {
            working_dir_path,
            sandbox_permissions,
            additional_permissions,
        })
    }

    // ============================================================
    // SPECIALIZED EXECUTORS (Hidden from LLM, used by unified tools)
    // ============================================================

    // File operation executors -- delegate to the file_ops_tool from inventory
    delegate_to_tool!(read_file_executor, file_ops_tool, read_file);
    delegate_to_tool!(write_file_executor, file_ops_tool, write_file);

    // Self-delegating executors -- forward to async methods on ToolRegistry
    delegate_to_self!(list_files_executor, list_files);
    delegate_to_self!(edit_file_executor, edit_file);
    delegate_to_self!(get_errors_executor, execute_get_errors);
    delegate_to_self!(search_tools_executor, execute_search_tools);
    delegate_to_self!(mcp_search_tools_executor, execute_mcp_search_tools);
    delegate_to_self!(mcp_get_tool_details_executor, execute_mcp_get_tool_details);
    delegate_to_self!(mcp_list_servers_executor, execute_mcp_list_servers);
    delegate_to_self!(mcp_connect_server_executor, execute_mcp_connect_server);
    delegate_to_self!(mcp_disconnect_server_executor, execute_mcp_disconnect_server);
    delegate_to_self!(apply_patch_executor, execute_apply_patch);

    /// Unified `mcp` executor: dispatches on `action`
    /// (search_tools | get_tool_details | list_servers | connect | disconnect).
    /// For legacy alias calls that omit `action`, the action is inferred from
    /// the argument shape: `query` implies search_tools, `name` implies
    /// get_tool_details, otherwise list_servers. `connect`/`disconnect` require
    /// an explicit `action` since the schema marks it required, and are
    /// evaluated under the action-qualified policy keys `mcp:connect` /
    /// `mcp:disconnect` so they keep their Prompt confirmation even though
    /// `mcp` itself is `ToolPolicy::Allow`.
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "Legacy executor aliases remain available for compatibility tests."
        )
    )]
    pub(super) fn mcp_executor(&self, args: Value) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move {
            let action = args
                .get("action")
                .and_then(Value::as_str)
                .map(str::to_ascii_lowercase)
                .unwrap_or_else(|| {
                    if args.get("query").is_some() {
                        "search_tools".to_string()
                    } else if args.get("name").is_some() {
                        "get_tool_details".to_string()
                    } else {
                        "list_servers".to_string()
                    }
                });
            match action.as_str() {
                "search_tools" => self.mcp_search_tools_executor(args).await,
                "get_tool_details" => self.mcp_get_tool_details_executor(args).await,
                "list_servers" => self.mcp_list_servers_executor(args).await,
                "connect" => self.mcp_connect_server_executor(args).await,
                "disconnect" => self.mcp_disconnect_server_executor(args).await,
                other => Err(anyhow!(
                    "mcp: unknown action '{other}'. Use action='search_tools' (query), 'get_tool_details' (name), 'list_servers', 'connect' (name), or 'disconnect' (name)."
                )),
            }
        })
    }

    // PTY executors -- distinct signatures, kept explicit.
    // `run_pty_cmd` and `create_pty_session` are intentional aliases: both
    // create a PTY session and run a command. The separate names exist for
    // backward compatibility with existing tool registrations.
    pub(super) fn run_pty_cmd_executor(&self, args: Value) -> BoxFuture<'_, Result<Value>> {
        self.dispatch_command_session_run_alias(args, true)
    }

    pub(super) fn exec_command_executor(&self, args: Value) -> BoxFuture<'_, Result<Value>> {
        self.dispatch_command_session_run_alias(args, false)
    }

    pub(super) fn write_stdin_executor(&self, args: Value) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move {
            let (args, dispatch) = normalize_write_stdin_args(&args)?;

            let response = match dispatch {
                crate::tools::command_args::WriteStdinDispatch::Write => {
                    self.execute_command_session_write_for_tool(args, tools::WRITE_STDIN).await
                }
                crate::tools::command_args::WriteStdinDispatch::Poll => {
                    self.execute_command_session_poll_for_tool(args, ExecSettlementMode::Manual, tools::WRITE_STDIN)
                        .await
                }
                crate::tools::command_args::WriteStdinDispatch::Wait => self.execute_command_session_wait(args).await,
            }?;
            Ok(response)
        })
    }

    pub(super) fn send_pty_input_executor(&self, args: Value) -> BoxFuture<'_, Result<Value>> {
        self.dispatch_command_session_action_alias(args, "write")
    }

    pub(super) fn read_pty_session_executor(&self, args: Value) -> BoxFuture<'_, Result<Value>> {
        self.dispatch_command_session_action_alias(args, "poll")
    }

    pub(super) fn create_pty_session_executor(&self, args: Value) -> BoxFuture<'_, Result<Value>> {
        self.dispatch_command_session_run_alias(args, true)
    }

    pub(super) fn list_pty_sessions_executor(&self, _args: Value) -> BoxFuture<'_, Result<Value>> {
        self.dispatch_command_session_alias(json!({"action": "list"}))
    }

    pub(super) fn close_pty_session_executor(&self, args: Value) -> BoxFuture<'_, Result<Value>> {
        self.dispatch_command_session_action_alias(args, "close")
    }

    // ============================================================
    // INTERNAL IMPLEMENTATIONS
    // ============================================================
}

#[cfg(test)]
mod execute_code_tests {
    use serde_json::json;

    use super::code_language_from_args;
    use crate::exec::code_executor::Language;

    #[test]
    fn code_language_uses_language_field_instead_of_action() {
        assert_eq!(
            code_language_from_args(&json!({
                "action": "code",
                "language": "javascript",
            })),
            Language::JavaScript
        );
        assert_eq!(
            code_language_from_args(&json!({
                "action": "code",
                "lang": "js",
            })),
            Language::JavaScript
        );
        assert_eq!(
            code_language_from_args(&json!({
                "action": "code",
            })),
            Language::Python3
        );
    }
}

#[cfg(test)]
mod subagent_tool_output_tests {
    use serde_json::json;
    use tempfile::TempDir;

    use super::sanitize_subagent_tool_output_paths;

    #[test]
    fn strips_transcript_paths_outside_workspace() {
        let temp = TempDir::new().expect("tempdir");
        let mut value = json!({
            "completed": true,
            "entry": {
                "id": "agent-1",
                "transcript_path": "/Users/example/.vtcode/sessions/agent-1.json",
            }
        });

        sanitize_subagent_tool_output_paths(temp.path(), &mut value);

        assert!(value["entry"].get("transcript_path").is_none());
    }

    #[test]
    fn keeps_transcript_paths_inside_workspace() {
        let temp = TempDir::new().expect("tempdir");
        let transcript_path = temp.path().join(".vtcode/context/subagents/agent-1.json");
        let mut value = json!({
            "id": "agent-1",
            "transcript_path": transcript_path,
        });

        sanitize_subagent_tool_output_paths(temp.path(), &mut value);

        assert_eq!(value["transcript_path"].as_str(), transcript_path.to_str());
    }
}

#[cfg(test)]
mod shell_preference_tests {
    use super::{resolve_shell_preference, resolve_shell_preference_with_zsh_fork};
    use crate::config::PtyConfig;
    use crate::tools::shell::resolve_fallback_shell;

    #[test]
    fn explicit_shell_overrides_config_preference() {
        let config = PtyConfig {
            preferred_shell: Some("/bin/bash".to_string()),
            ..Default::default()
        };

        let resolved = resolve_shell_preference(Some(" /bin/zsh "), &config);
        assert_eq!(resolved, "/bin/zsh");
    }

    #[test]
    fn config_preferred_shell_used_when_explicit_missing() {
        let config = PtyConfig {
            preferred_shell: Some("zsh".to_string()),
            ..Default::default()
        };

        let resolved = resolve_shell_preference(None, &config);
        assert_eq!(resolved, "zsh");
    }

    #[test]
    fn blank_explicit_shell_falls_back_to_config_preference() {
        let config = PtyConfig {
            preferred_shell: Some("bash".to_string()),
            ..Default::default()
        };

        let resolved = resolve_shell_preference(Some("   "), &config);
        assert_eq!(resolved, "bash");
    }

    #[test]
    fn blank_config_shell_falls_back_to_default_resolver() {
        let config = PtyConfig {
            preferred_shell: Some("   ".to_string()),
            ..Default::default()
        };

        let resolved = resolve_shell_preference(None, &config);
        assert_eq!(resolved, resolve_fallback_shell());
    }

    #[test]
    fn missing_preferences_fall_back_to_default_resolver() {
        let config = PtyConfig::default();
        let resolved = resolve_shell_preference(None, &config);
        assert_eq!(resolved, resolve_fallback_shell());
    }

    #[test]
    fn zsh_fork_disabled_uses_standard_shell_resolution() -> anyhow::Result<()> {
        let config = PtyConfig {
            preferred_shell: Some("/bin/bash".to_string()),
            ..Default::default()
        };
        let resolved = resolve_shell_preference_with_zsh_fork(None, &config)?;
        assert_eq!(resolved, "/bin/bash");
        Ok(())
    }

    #[test]
    fn zsh_fork_missing_path_returns_error() {
        let config = PtyConfig {
            shell_zsh_fork: true,
            zsh_path: None,
            ..PtyConfig::default()
        };
        resolve_shell_preference_with_zsh_fork(Some("/bin/bash"), &config).unwrap_err();
    }

    #[cfg(unix)]
    #[test]
    fn zsh_fork_ignores_explicit_shell_and_uses_configured_path() -> anyhow::Result<()> {
        let zsh = tempfile::NamedTempFile::new()?;
        let expected = zsh.path().to_string_lossy().to_string();
        let config = PtyConfig {
            shell_zsh_fork: true,
            zsh_path: Some(expected.clone()),
            ..PtyConfig::default()
        };
        let resolved = resolve_shell_preference_with_zsh_fork(Some("/bin/bash"), &config)?;
        assert_eq!(resolved, expected);
        Ok(())
    }
}

#[cfg(test)]
mod token_efficiency_tests {
    use super::*;

    #[test]
    fn test_suggests_limit_for_cat() {
        assert_eq!(suggest_max_tokens_for_command("cat file.txt"), Some(250));
        assert_eq!(suggest_max_tokens_for_command("cat /path/to/file.rs"), Some(250));
        assert_eq!(suggest_max_tokens_for_command("CAT file.txt"), Some(250)); // case insensitive
    }

    #[test]
    fn test_suggests_limit_for_bat() {
        assert_eq!(suggest_max_tokens_for_command("bat file.rs"), Some(250));
    }

    #[test]
    fn test_no_limit_when_already_limited() {
        assert_eq!(suggest_max_tokens_for_command("cat file.txt | head"), None);
        assert_eq!(suggest_max_tokens_for_command("head -n 50 file.txt"), None);
        assert_eq!(suggest_max_tokens_for_command("tail -n 20 file.txt"), None);
    }

    #[test]
    fn test_no_limit_for_other_commands() {
        assert_eq!(suggest_max_tokens_for_command("ls -la"), None);
        assert_eq!(suggest_max_tokens_for_command("grep pattern file"), None);
        assert_eq!(suggest_max_tokens_for_command("echo hello"), None);
    }
}

#[cfg(test)]
mod pty_output_filter_tests {
    use super::filter_pty_output;

    #[test]
    fn normalizes_crlf_sequences() {
        let raw = "a\r\nb\rc\n";
        assert_eq!(filter_pty_output(raw), "a\nb\nc\n");
    }
}

#[cfg(test)]
mod pty_context_tests {
    use serde_json::json;

    use super::{
        ExecOutputPreview, PtyEphemeralCapture, attach_exec_response_context, attach_pty_continuation,
        build_exec_response, build_exec_session_command_display,
    };
    use crate::tools::types::VTCodeExecSession;

    #[test]
    fn build_exec_session_command_display_unwraps_shell_c_argument() {
        let session = VTCodeExecSession {
            id: "run-123".to_string().into(),
            backend: "pty".to_string(),
            command: "zsh".to_string(),
            args: vec!["-l".to_string(), "-c".to_string(), "cargo check".to_string()],
            working_dir: Some(".".to_string()),
            rows: Some(24),
            cols: Some(80),
            child_pid: None,
            started_at: None,
            lifecycle_state: None,
            exit_code: None,
        };

        assert_eq!(build_exec_session_command_display(&session), "cargo check");
    }

    #[test]
    fn attach_exec_response_context_sets_expected_keys() {
        let mut response = json!({ "output": "ok" });
        let session = VTCodeExecSession {
            id: "run-123".to_string().into(),
            backend: "pty".to_string(),
            command: "zsh".to_string(),
            args: vec!["-l".to_string(), "-c".to_string(), "cargo check".to_string()],
            working_dir: Some(".".to_string()),
            rows: Some(30),
            cols: Some(120),
            child_pid: None,
            started_at: None,
            lifecycle_state: None,
            exit_code: None,
        };

        attach_exec_response_context(&mut response, &session, "cargo check", false);

        assert_eq!(response["session_id"], "run-123");
        assert_eq!(response["command"], "cargo check");
        assert_eq!(response["working_directory"], ".");
        assert_eq!(response["backend"], "pty");
        assert_eq!(response["rows"], 30);
        assert_eq!(response["cols"], 120);
        assert_eq!(response["is_exited"], false);
    }

    #[test]
    fn attach_pty_continuation_compacts_next_continue_args() {
        let mut response = json!({ "output": "ok" });
        attach_pty_continuation(&mut response, "run-123");

        assert!(response.get("follow_up_prompt").is_none());
        assert!(response.get("next_poll_args").is_none());
        assert_eq!(response["next_continue_args"], json!({ "session_id": "run-123" }));
        assert!(response.get("preferred_next_action").is_none());
    }

    #[test]
    fn attach_pty_continuation_keeps_payload_compact() {
        let mut response = json!({ "output": "ok" });
        attach_pty_continuation(&mut response, "run-123");

        assert!(response.get("follow_up_prompt").is_none());
        assert!(response.get("next_poll_args").is_none());
        assert_eq!(response["next_continue_args"], json!({ "session_id": "run-123" }));
    }

    #[test]
    fn build_exec_response_skips_continuation_after_exit() {
        let session = VTCodeExecSession {
            id: "run-123".to_string().into(),
            backend: "pipe".to_string(),
            command: "cargo".to_string(),
            args: vec!["check".to_string()],
            working_dir: Some(".".to_string()),
            rows: None,
            cols: None,
            child_pid: None,
            started_at: None,
            lifecycle_state: None,
            exit_code: None,
        };
        let capture = PtyEphemeralCapture {
            output: "first\nsecond\n".to_string(),
            exit_code: Some(0),
            duration: std::time::Duration::from_millis(25),
        };

        let response = build_exec_response(
            &session,
            "cargo check",
            &capture,
            ExecOutputPreview {
                raw_output: "first\nsecond\n".to_string(),
                output: "first\n[Output truncated]".to_string(),
                truncated: true,
            },
            None,
            false,
            None,
        );

        assert_eq!(response["exit_code"], 0);
        assert!(response.get("next_continue_args").is_none());
    }

    #[test]
    fn build_exec_response_steers_still_running_to_wait_action() {
        let session = VTCodeExecSession {
            id: "run-abc".to_string().into(),
            backend: "pipe".to_string(),
            command: "cargo".to_string(),
            args: vec!["build".to_string()],
            working_dir: Some(".".to_string()),
            rows: None,
            cols: None,
            child_pid: None,
            started_at: None,
            lifecycle_state: None,
            exit_code: None,
        };
        let capture = PtyEphemeralCapture {
            output: "   Compiling vtcode-core\n".to_string(),
            exit_code: None,
            duration: std::time::Duration::from_secs(10),
        };

        let response = build_exec_response(
            &session,
            "cargo build",
            &capture,
            ExecOutputPreview {
                raw_output: "   Compiling vtcode-core\n".to_string(),
                output: "   Compiling vtcode-core\n".to_string(),
                truncated: false,
            },
            None,
            false,
            Some("run-abc"),
        );

        // The poll-oriented continuation is still attached for incremental peeks.
        assert_eq!(response["next_continue_args"], json!({ "session_id": "run-abc" }));
        // The no-burn wait action is pre-filled and ready to reuse.
        assert_eq!(response["next_wait_args"]["session_id"], "run-abc");
        assert_eq!(response["next_wait_args"]["action"], "wait");
        assert_eq!(response["next_wait_args"]["wait_timeout_seconds"], 600);
        // The hint ranks wait ahead of polling and names the tool to call.
        let hint = response["next_action_hint"].as_str().expect("hint present");
        assert!(hint.contains("write_stdin"));
        assert!(hint.contains("next_wait_args"));
        assert!(hint.contains("no model round-trips"));
        assert!(hint.contains("next_continue_args"));
        assert_eq!(response["is_exited"], false);
        assert_eq!(response["process_id"], "run-abc");
    }
}

#[cfg(test)]
mod git_diff_tests {
    use super::is_git_diff_command;

    #[test]
    fn detects_git_diff() {
        let cmd = vec!["git".to_string(), "diff".to_string()];
        assert!(is_git_diff_command(&cmd));
    }

    #[test]
    fn detects_git_diff_with_flags() {
        let cmd = vec![
            "git".to_string(),
            "-c".to_string(),
            "color.ui=always".to_string(),
            "diff".to_string(),
            "--stat".to_string(),
        ];
        assert!(is_git_diff_command(&cmd));
    }

    #[test]
    fn detects_git_diff_with_path() {
        let cmd = vec!["/usr/bin/git".to_string(), "diff".to_string()];
        assert!(is_git_diff_command(&cmd));
    }

    #[test]
    fn ignores_other_git_commands() {
        let cmd = vec!["git".to_string(), "status".to_string()];
        assert!(!is_git_diff_command(&cmd));
    }
}

#[cfg(test)]
mod unified_action_error_tests {
    use std::time::Duration;

    use serde_json::json;

    use super::{
        CargoTestCommandKind, ExecOutputPreview, PtyEphemeralCapture, attach_exec_recovery_guidance,
        attach_failure_diagnostics_metadata, build_exec_output_preview, build_exec_response, build_head_tail_preview,
        cargo_selector_error_diagnostics, cargo_test_failure_diagnostics, cargo_test_rerun_hint, clamp_inspect_lines,
        clamp_max_matches, extract_run_session_id_from_read_file_error, extract_run_session_id_from_tool_output_path,
        filter_lines, missing_command_session_action_error, resolve_exec_run_session_id, summarized_arg_keys,
    };
    use crate::tools::types::VTCodeExecSession;

    #[test]
    fn summarized_arg_keys_reports_shape_for_non_object_payloads() {
        assert_eq!(summarized_arg_keys(&json!(null)), "<null>");
        assert_eq!(summarized_arg_keys(&json!(["a", "b"])), "<array>");
        assert_eq!(summarized_arg_keys(&json!("x")), "<string>");
    }

    #[test]
    fn exec_command_missing_action_error_includes_received_keys() {
        let err = missing_command_session_action_error(&json!({
            "foo": "bar",
            "session_id": "123"
        }));
        let text = err.to_string();
        assert!(text.contains("Missing command session action"));
        assert!(text.contains("foo"));
        assert!(text.contains("session_id"));
    }

    #[test]
    fn extracts_run_session_id_from_tool_output_path() {
        assert_eq!(
            extract_run_session_id_from_tool_output_path(".vtcode/context/tool_outputs/run-abc123.txt"),
            Some("run-abc123".to_string())
        );
        assert_eq!(
            extract_run_session_id_from_tool_output_path(".vtcode/context/tool_outputs/not-a-session.txt"),
            None
        );
    }

    #[test]
    fn extracts_run_session_id_from_read_file_error() {
        let error = "Use exec_command with session_id=\"run-zz9\" instead of read_file.";
        assert_eq!(extract_run_session_id_from_read_file_error(error), Some("run-zz9".to_string()));
        assert_eq!(extract_run_session_id_from_read_file_error("no session"), None);
    }

    #[test]
    fn resolve_exec_run_session_id_prefers_requested_session_id() {
        let payload = json!({ "session_id": " check_sh " });
        let payload = payload.as_object().expect("object");

        assert_eq!(resolve_exec_run_session_id(payload).expect("requested session id"), "check_sh");
    }

    #[test]
    fn resolve_exec_run_session_id_generates_default_when_missing() {
        let payload = json!({});
        let payload = payload.as_object().expect("object");
        let session_id = resolve_exec_run_session_id(payload).expect("generated session id");

        assert!(session_id.starts_with("run-"));
    }

    #[test]
    fn resolve_exec_run_session_id_rejects_invalid_values() {
        let payload = json!({ "session_id": "bad id" });
        let payload = payload.as_object().expect("object");
        let err = resolve_exec_run_session_id(payload).expect_err("invalid session id");

        assert!(err.to_string().contains("Invalid session_id"));
    }

    #[test]
    fn inspect_helpers_clamp_limits() {
        assert_eq!(clamp_inspect_lines(Some(0), 30), 0);
        assert_eq!(clamp_inspect_lines(Some(9_999), 30), 5_000);
        assert_eq!(clamp_max_matches(None), 200);
        assert_eq!(clamp_max_matches(Some(0)), 1);
        assert_eq!(clamp_max_matches(Some(50_000)), 10_000);
    }

    #[test]
    fn inspect_helpers_build_head_tail_preview() {
        let content = "l1\nl2\nl3\nl4\nl5\nl6";
        let (preview, truncated) = build_head_tail_preview(content, 2, 2);
        assert!(truncated);
        assert!(preview.contains("l1"));
        assert!(preview.contains("l2"));
        assert!(preview.contains("l5"));
        assert!(preview.contains("l6"));
    }

    #[test]
    fn inspect_helpers_filter_lines_literal() {
        let (output, matched, truncated) = filter_lines("alpha\nbeta\nalpha2", "alpha", true, 1).expect("filter");
        assert_eq!(matched, 2);
        assert!(truncated);
        assert!(output.contains("1: alpha"));
    }

    #[test]
    fn exec_output_preview_truncates_on_utf8_boundaries() {
        let (preview, truncated) = build_exec_output_preview("a🙂b", 1);

        assert!(truncated);
        assert_eq!(preview, "a\n[Output truncated]");
        std::str::from_utf8(preview.as_bytes()).unwrap();
    }

    #[test]
    fn exec_recovery_guidance_sets_command_not_found_metadata() {
        let session = VTCodeExecSession {
            id: "run-123".to_string().into(),
            backend: "pipe".to_string(),
            command: "zsh".to_string(),
            args: vec!["-c".to_string(), "pip install pymupdf".to_string()],
            working_dir: Some(".".to_string()),
            rows: None,
            cols: None,
            child_pid: None,
            started_at: None,
            lifecycle_state: None,
            exit_code: None,
        };
        let capture = PtyEphemeralCapture {
            output: String::new(),
            exit_code: Some(127),
            duration: Duration::from_millis(42),
        };

        let response = build_exec_response(
            &session,
            "pip install pymupdf",
            &capture,
            ExecOutputPreview {
                raw_output: "bash: pip: command not found".to_string(),
                output: "bash: pip: command not found".to_string(),
                truncated: false,
            },
            None,
            false,
            None,
        );

        assert_eq!(response["output"], "bash: pip: command not found");
        assert_eq!(response["exit_code"], 127);
        assert_eq!(response["session_id"], "run-123");
        assert_eq!(response["command"], "pip install pymupdf");
        assert_eq!(response["critical_note"], "Command `pip` was not found in PATH.");
        assert_eq!(
            response["next_action"],
            "Check the command name or install the missing binary, then rerun the command."
        );
    }

    #[test]
    fn exec_recovery_guidance_ignores_non_command_not_found_exit_codes() {
        let mut response = json!({});
        attach_exec_recovery_guidance(&mut response, "cargo test", Some(1));
        assert!(response.get("critical_note").is_none());
        assert!(response.get("next_action").is_none());
    }

    #[test]
    fn cargo_selector_error_diagnostics_classifies_missing_test_target() {
        let output = "error: no test target named `exec_only_policy_skips_when_full_auto_is_disabled` in `vtcode-core` package\n";

        let diagnostics = cargo_selector_error_diagnostics(
            CargoTestCommandKind::Nextest,
            "cargo nextest run --test exec_only_policy_skips_when_full_auto_is_disabled -p vtcode-core --no-capture",
            output,
        )
        .expect("selector diagnostics");

        assert_eq!(diagnostics["kind"], "cargo_test_selector_error");
        assert_eq!(diagnostics["package"], "vtcode-core");
        assert_eq!(diagnostics["requested_test_target"], "exec_only_policy_skips_when_full_auto_is_disabled");
        assert_eq!(diagnostics["selector_error"], true);
        assert_eq!(
            diagnostics["validation_hint"],
            "cargo test -p vtcode-core --lib -- --list | rg 'exec_only_policy_skips_when_full_auto_is_disabled'"
        );
        assert_eq!(
            diagnostics["rerun_hint"],
            "cargo nextest run -p vtcode-core exec_only_policy_skips_when_full_auto_is_disabled"
        );
    }

    #[test]
    fn cargo_test_failure_diagnostics_extracts_unit_test_failure_details() {
        let output = r#"────────────
    Nextest run ID 18fffe01-0ef9-4113-9a81-2344a7cc3c16 with nextest profile: default
        FAIL [   0.216s] ( 363/2669) vtcode-core core::agent::runner::tests::exec_only_policy_skips_when_full_auto_is_disabled
    stderr ───
    thread 'core::agent::runner::tests::exec_only_policy_skips_when_full_auto_is_disabled' (382951) panicked at crates/codegen/vtcode-core/src/core/agent/runner/tests.rs:692:10:
    task result: Invalid request: QueuedProvider has no queued responses
"#;

        let diagnostics = cargo_test_failure_diagnostics("cargo nextest run -p vtcode-core", output, Some(100))
            .expect("failure diagnostics");

        assert_eq!(diagnostics["kind"], "cargo_test_failure");
        assert_eq!(diagnostics["package"], "vtcode-core");
        assert_eq!(diagnostics["binary_kind"], "unit");
        assert_eq!(
            diagnostics["test_fqname"],
            "core::agent::runner::tests::exec_only_policy_skips_when_full_auto_is_disabled"
        );
        assert_eq!(diagnostics["panic"], "task result: Invalid request: QueuedProvider has no queued responses");
        assert_eq!(diagnostics["source_file"], "crates/codegen/vtcode-core/src/core/agent/runner/tests.rs");
        assert_eq!(diagnostics["source_line"], 692);
        assert_eq!(
            diagnostics["rerun_hint"],
            cargo_test_rerun_hint(
                CargoTestCommandKind::Nextest,
                "vtcode-core",
                "unit",
                "core::agent::runner::tests::exec_only_policy_skips_when_full_auto_is_disabled",
            )
        );
    }

    #[test]
    fn build_exec_response_attaches_cargo_failure_diagnostics() {
        let session = VTCodeExecSession {
            id: "run-123".to_string().into(),
            backend: "pipe".to_string(),
            command: "cargo".to_string(),
            args: vec![
                "nextest".to_string(),
                "run".to_string(),
                "-p".to_string(),
                "vtcode-core".to_string(),
            ],
            working_dir: Some(".".to_string()),
            rows: None,
            cols: None,
            child_pid: None,
            started_at: None,
            lifecycle_state: None,
            exit_code: None,
        };
        let raw_output = r#"
        FAIL [   0.216s] ( 363/2669) vtcode-core core::agent::runner::tests::exec_only_policy_skips_when_full_auto_is_disabled
    thread 'core::agent::runner::tests::exec_only_policy_skips_when_full_auto_is_disabled' (382951) panicked at crates/codegen/vtcode-core/src/core/agent/runner/tests.rs:692:10:
    task result: Invalid request: QueuedProvider has no queued responses
"#;
        let capture = PtyEphemeralCapture {
            output: raw_output.to_string(),
            exit_code: Some(100),
            duration: Duration::from_millis(42),
        };

        let response = build_exec_response(
            &session,
            "cargo nextest run -p vtcode-core",
            &capture,
            ExecOutputPreview {
                raw_output: raw_output.to_string(),
                output: raw_output.to_string(),
                truncated: false,
            },
            None,
            false,
            None,
        );

        assert_eq!(
            response["failure_diagnostics"]["test_fqname"],
            "core::agent::runner::tests::exec_only_policy_skips_when_full_auto_is_disabled"
        );
        assert_eq!(response["package"], "vtcode-core");
        assert_eq!(response["binary_kind"], "unit");
        assert_eq!(response["source_file"], "crates/codegen/vtcode-core/src/core/agent/runner/tests.rs");
        assert_eq!(response["source_line"], 692);
        assert_eq!(
            response["rerun_hint"],
            "cargo nextest run -p vtcode-core core::agent::runner::tests::exec_only_policy_skips_when_full_auto_is_disabled"
        );
        assert_eq!(
            response["next_action"],
            "Rerun the failing test directly with: cargo nextest run -p vtcode-core core::agent::runner::tests::exec_only_policy_skips_when_full_auto_is_disabled"
        );
    }

    #[test]
    fn attach_failure_diagnostics_metadata_promotes_selector_hints() {
        let mut response = json!({
            "success": true,
            "command": "cargo nextest run --test bad -p vtcode-core"
        });
        let diagnostics = json!({
            "kind": "cargo_test_selector_error",
            "package": "vtcode-core",
            "binary_kind": "test_target_selector",
            "requested_test_target": "bad",
            "selector_error": true,
            "validation_hint": "cargo test -p vtcode-core --lib -- --list | rg 'bad'",
            "rerun_hint": "cargo nextest run -p vtcode-core bad",
            "critical_note": "selector mismatch",
            "next_action": "validate first"
        });

        attach_failure_diagnostics_metadata(&mut response, &diagnostics);

        assert_eq!(response["package"], "vtcode-core");
        assert_eq!(response["binary_kind"], "test_target_selector");
        assert_eq!(response["selector_error"], true);
        assert_eq!(response["validation_hint"], "cargo test -p vtcode-core --lib -- --list | rg 'bad'");
        assert_eq!(response["rerun_hint"], "cargo nextest run -p vtcode-core bad");
        assert_eq!(response["critical_note"], "selector mismatch");
        assert_eq!(response["next_action"], "validate first");
        assert_eq!(response["failure_diagnostics"]["kind"], "cargo_test_selector_error");
    }
}

#[cfg(test)]
#[path = "executors/sandbox_runtime_tests.rs"]
mod sandbox_runtime_tests;

#[cfg(test)]
mod mcp_action_dispatch_tests {
    use serde_json::json;

    use super::ToolRegistry;

    /// `mcp_executor` must dispatch `action='connect'`/`'disconnect'` to
    /// `mcp_connect_server_executor`/`mcp_disconnect_server_executor` rather
    /// than falling through to the unknown-action branch. A bare
    /// `ToolRegistry::new` has no MCP client configured, so both calls fail
    /// at the `mcp_client()` lookup inside the delegated executor -- but the
    /// error text proves the dispatch reached the right function.
    #[tokio::test]
    async fn mcp_executor_dispatches_connect_and_disconnect_actions() {
        let temp = tempfile::tempdir().expect("tempdir");
        let registry = ToolRegistry::new(temp.path().to_path_buf()).await;

        let connect_err = registry
            .mcp_executor(json!({"action": "connect", "name": "example"}))
            .await
            .expect_err("connect without an active mcp client should fail");
        let connect_text = connect_err.to_string();
        assert!(!connect_text.contains("unknown action"));
        assert!(connect_text.contains("MCP client not available"));

        let disconnect_err = registry
            .mcp_executor(json!({"action": "disconnect", "name": "example"}))
            .await
            .expect_err("disconnect without an active mcp client should fail");
        let disconnect_text = disconnect_err.to_string();
        assert!(!disconnect_text.contains("unknown action"));
        assert!(disconnect_text.contains("MCP client not available"));
    }

    #[tokio::test]
    async fn mcp_executor_rejects_unknown_action_with_guidance() {
        let temp = tempfile::tempdir().expect("tempdir");
        let registry = ToolRegistry::new(temp.path().to_path_buf()).await;

        let err = registry
            .mcp_executor(json!({"action": "bogus"}))
            .await
            .expect_err("unknown mcp action should error");
        let text = err.to_string();
        assert!(text.contains("unknown action 'bogus'"));
        assert!(text.contains("connect"));
        assert!(text.contains("disconnect"));
    }
}
