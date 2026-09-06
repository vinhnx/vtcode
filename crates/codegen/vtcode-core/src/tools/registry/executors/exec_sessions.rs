use super::exec_support::*;
use super::{
    ExecRunBackendKind, ExecSettlementMode, ToolRegistry, acquire_executor_rate_limit, annotate_exec_run_response,
};
use crate::config::constants::tools;
use crate::tools::registry::ToolTimeoutCategory;
use crate::tools::types::VTCodeExecSession;
use crate::zsh_exec_bridge::ZshExecBridgeSession;
use anyhow::{Context, Result, anyhow};
use hashbrown::HashMap;
use serde_json::{Value, json};
use std::future::Future;
use std::time::{Duration, Instant};

struct ResolvedExecSession {
    metadata: VTCodeExecSession,
    command_display: String,
}

impl ResolvedExecSession {
    fn new(metadata: VTCodeExecSession) -> Self {
        let command_display = build_exec_session_command_display(&metadata);
        Self { metadata, command_display }
    }

    fn should_settle_noninteractive(&self, exec_settlement_mode: ExecSettlementMode) -> bool {
        exec_settlement_mode.settle_noninteractive() && self.metadata.backend == "pipe"
    }
}

impl ToolRegistry {
    pub(super) async fn execute_command_session_run_pty(
        &self,
        args: Value,
        retain_completed_session: bool,
    ) -> Result<Value> {
        let request = self
            .prepare_exec_run_request(
                &args,
                ExecRunBackendKind::Pty,
                "command execution requires a 'command' value",
                "PTY command cannot be empty",
            )
            .await?;
        let mut session_env = HashMap::new();
        let mut zsh_exec_bridge = None;
        if self.pty_config().shell_zsh_fork {
            let wrapper_executable =
                std::env::current_exe().context("resolve current executable for zsh exec bridge")?;
            let bridge = ZshExecBridgeSession::spawn(request.confirm).context("initialize zsh exec bridge session")?;
            session_env = bridge.env_vars(&wrapper_executable);
            zsh_exec_bridge = Some(bridge);
        }
        session_env.extend(request.env_overrides);
        let session_metadata = self
            .exec_sessions
            .create_pty_session_with_sandbox(
                request.session_id.clone().into(),
                request.prepared_command.command,
                request.working_dir_path,
                crate::tools::pty::PtySize {
                    rows: request.rows.unwrap_or(self.pty_config().default_rows),
                    cols: request.cols.unwrap_or(self.pty_config().default_cols),
                    pixel_width: 0,
                    pixel_height: 0,
                },
                session_env,
                zsh_exec_bridge,
                request.sandbox_active,
            )
            .await
            .context("Maximum PTY sessions reached; cannot start new session")?;
        self.increment_active_pty_sessions();

        let capture = self
            .wait_for_exec_yield(session_metadata.id.as_str(), request.yield_duration, Some(tools::UNIFIED_EXEC), true)
            .await;

        self.finalize_exec_run_response(
            &session_metadata,
            &request.prepared_command.requested_command_display,
            &request.output_config,
            request.is_git_diff,
            retain_completed_session,
            capture,
        )
        .await
    }

    pub(super) async fn execute_run_pipe_cmd(
        &self,
        args: Value,
        exec_settlement_mode: ExecSettlementMode,
    ) -> Result<Value> {
        let request = self
            .prepare_exec_run_request(
                &args,
                ExecRunBackendKind::Pipe,
                "exec_command run requires a 'command' value",
                "Command cannot be empty",
            )
            .await?;
        let session_env = self.build_pipe_session_env(&request.shell_program, request.env_overrides);
        let session_metadata = self
            .exec_sessions
            .create_pipe_session_with_sandbox(
                request.session_id.clone().into(),
                request.prepared_command.command,
                request.working_dir_path,
                session_env,
                request.sandbox_active,
            )
            .await?;

        let capture = self
            .capture_exec_session_output(
                session_metadata.id.as_str(),
                request.yield_duration,
                Some(tools::UNIFIED_EXEC),
                exec_settlement_mode.settle_noninteractive(),
            )
            .await?;

        self.finalize_exec_run_response(
            &session_metadata,
            &request.prepared_command.requested_command_display,
            &request.output_config,
            request.is_git_diff,
            false,
            capture,
        )
        .await
    }

    pub(super) async fn execute_command_session_write(&self, args: Value) -> Result<Value> {
        self.execute_command_session_write_for_tool(args, tools::UNIFIED_EXEC).await
    }

    pub(crate) async fn execute_command_session_write_for_tool(
        &self,
        args: Value,
        progress_tool_name: &'static str,
    ) -> Result<Value> {
        acquire_executor_rate_limit("write_stdin:write", 3.0)?;

        let payload = exec_session_payload(&args, "command session write requires a JSON object")?;
        let session = self
            .resolve_exec_session(
                &args,
                "command session write requires a JSON object",
                "session_id is required for command session write",
                "command session write",
            )
            .await?;
        let input = crate::tools::command_args::interactive_input_text(&args)
            .ok_or_else(|| anyhow!("input is required for command session write"))?;
        enforce_exec_input_line_policy(input)?;

        let yield_time_ms = clamp_exec_yield_ms(payload.get("yield_time_ms").and_then(Value::as_u64), 250);
        let max_tokens = max_output_tokens_from_payload(payload)
            .unwrap_or(crate::config::constants::defaults::DEFAULT_PTY_OUTPUT_MAX_TOKENS);

        self.exec_sessions
            .send_input_to_session(session.metadata.id.as_str(), input.as_bytes(), false)
            .await?;

        self.build_passthrough_exec_response(
            &session,
            Duration::from_millis(yield_time_ms),
            false,
            Some(max_tokens),
            progress_tool_name,
        )
        .await
    }

    pub(super) async fn execute_command_session_poll_internal(
        &self,
        args: Value,
        exec_settlement_mode: ExecSettlementMode,
    ) -> Result<Value> {
        self.execute_command_session_poll_for_tool(args, exec_settlement_mode, tools::UNIFIED_EXEC)
            .await
    }

    pub(crate) async fn execute_command_session_poll_for_tool(
        &self,
        args: Value,
        exec_settlement_mode: ExecSettlementMode,
        progress_tool_name: &'static str,
    ) -> Result<Value> {
        acquire_executor_rate_limit("write_stdin:poll", 4.0)?;

        let payload = exec_session_payload(&args, "command session read requires a JSON object")?;
        let session = self
            .resolve_exec_session(
                &args,
                "command session read requires a JSON object",
                "session_id is required for command session read",
                "command session read",
            )
            .await?;
        let yield_time_ms = clamp_exec_yield_ms(payload.get("yield_time_ms").and_then(Value::as_u64), 1000);
        let max_tokens = max_output_tokens_from_payload(payload);

        self.build_passthrough_exec_response(
            &session,
            Duration::from_millis(yield_time_ms),
            session.should_settle_noninteractive(exec_settlement_mode),
            max_tokens,
            progress_tool_name,
        )
        .await
    }

    pub(crate) async fn execute_command_session_wait(&self, args: Value) -> Result<Value> {
        acquire_executor_rate_limit("write_stdin:wait", 1.0)?;

        let payload = exec_session_payload(&args, "command session wait requires a JSON object")?;
        let session = self
            .resolve_exec_session(
                &args,
                "command session wait requires a JSON object",
                "session_id is required for command session wait",
                "command session wait",
            )
            .await?;
        let ceiling = self
            .timeout_policy()
            .ceiling_for(ToolTimeoutCategory::LongRunningCommand)
            .unwrap_or(Duration::from_secs(3_600));
        let requested_seconds = payload
            .get("wait_timeout_seconds")
            .or_else(|| payload.get("timeout_seconds"))
            .and_then(Value::as_u64)
            .map(Duration::from_secs)
            .unwrap_or(ceiling);
        let deadline = requested_seconds.min(ceiling);
        let max_tokens = max_output_tokens_from_payload(payload);
        let capture = self
            .wait_for_exec_yield(session.metadata.id.as_str(), deadline, Some(tools::WRITE_STDIN), true)
            .await;
        let mut response =
            build_exec_passthrough_response(&session.metadata, &session.command_display, &capture, max_tokens);
        response["waited_seconds"] = json!(capture.duration.as_secs_f64());
        response["wait_deadline_seconds"] = json!(deadline.as_secs());
        let spool_safe_to_prune = self
            .attach_pipe_output_metadata(&mut response, session.metadata.id.as_str(), capture.exit_code.is_some())
            .await?;
        self.prune_session_if_exited(session.metadata.id.as_str(), capture.exit_code, false, spool_safe_to_prune)
            .await?;
        Ok(response)
    }

    pub(super) async fn execute_command_session_continue_internal(
        &self,
        args: Value,
        exec_settlement_mode: ExecSettlementMode,
    ) -> Result<Value> {
        if crate::tools::command_args::interactive_input_text(&args).is_some() {
            self.execute_command_session_write(args).await
        } else {
            self.execute_command_session_poll_internal(args, exec_settlement_mode).await
        }
    }

    pub(super) async fn execute_command_session_inspect(&self, args: Value) -> Result<Value> {
        let payload = exec_session_payload(&args, "inspect requires a JSON object")?;
        let query = payload
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let literal = payload.get("literal").and_then(Value::as_bool).unwrap_or(false);
        let max_matches = clamp_max_matches(payload.get("max_matches").and_then(Value::as_u64));
        let head_lines =
            clamp_inspect_lines(payload.get("head_lines").and_then(Value::as_u64), DEFAULT_INSPECT_HEAD_LINES);
        let tail_lines =
            clamp_inspect_lines(payload.get("tail_lines").and_then(Value::as_u64), DEFAULT_INSPECT_TAIL_LINES);

        let source_session_id = crate::tools::command_args::session_id_text(&args).map(str::to_string);
        let source_spool_path = payload
            .get("spool_path")
            .and_then(Value::as_str)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

        let (content, spool_truncated) = if let Some(spool_path) = source_spool_path.as_deref() {
            let resolved = resolve_workspace_scoped_path_resolved(self.inventory.workspace_root(), spool_path).await?;
            read_bounded_spool_file(&resolved, EXEC_INSPECT_SPOOL_MAX_BYTES).await?
        } else if let Some(session_id) = source_session_id.as_deref() {
            let session_id = validate_exec_session_id(session_id, "inspect")?;
            let yield_time_ms = clamp_peek_yield_ms(payload.get("yield_time_ms").and_then(Value::as_u64));
            let capture = self
                .wait_for_exec_yield(session_id, Duration::from_millis(yield_time_ms), None, false)
                .await;
            (filter_pty_output(&strip_ansi(&capture.output)), false)
        } else {
            return Err(anyhow!("inspect requires either `session_id` or `spool_path`"));
        };

        let (output, matched_count, truncated) = if let Some(query) = query {
            let (filtered, count, is_truncated) = filter_lines(&content, query, literal, max_matches)?;
            (filtered, count, is_truncated || spool_truncated)
        } else {
            let (preview, is_truncated) = build_head_tail_preview(&content, head_lines, tail_lines);
            (preview, 0, is_truncated || spool_truncated)
        };

        let mut response = json!({
            "success": true,
            "output": output,
            "matched_count": matched_count,
            "truncated": truncated,
            "content_type": "exec_inspect"
        });
        if let Some(session_id) = source_session_id {
            response["session_id"] = json!(session_id);
        }
        if let Some(spool_path) = source_spool_path {
            response["spool_path"] = json!(spool_path);
        }

        Ok(response)
    }

    pub(super) async fn execute_command_session_list(&self) -> Result<Value> {
        let sessions = self.exec_sessions.list_sessions().await;
        Ok(json!({ "success": true, "sessions": sessions }))
    }

    pub(super) async fn execute_command_session_close(&self, args: Value) -> Result<Value> {
        let sid = resolve_exec_session_id(
            &args,
            "command session close requires a JSON object",
            "session_id is required for command session close",
            "command session close",
        )?;

        let session_metadata = self.close_exec_session(&sid).await?;

        Ok(json!({
            "success": true,
            "session_id": sid,
            "backend": session_metadata.backend
        }))
    }

    fn build_pipe_session_env(
        &self,
        shell_program: &str,
        extra_env: HashMap<String, String>,
    ) -> HashMap<String, String> {
        let mut env: HashMap<String, String> = std::env::vars().collect();
        env.insert("WORKSPACE_DIR".to_string(), self.workspace_root().display().to_string());
        env.insert("PAGER".to_string(), "cat".to_string());
        env.insert("GIT_PAGER".to_string(), "cat".to_string());
        env.insert("NO_COLOR".to_string(), "1".to_string());
        env.insert("CARGO_TERM_COLOR".to_string(), "never".to_string());
        if !shell_program.trim().is_empty() {
            env.insert("SHELL".to_string(), shell_program.to_string());
        }
        env.extend(extra_env);
        env
    }

    fn exec_session_metadata<'a>(
        &'a self,
        session_id: &'a str,
    ) -> impl Future<Output = Result<VTCodeExecSession>> + 'a {
        self.exec_sessions.snapshot_session(session_id)
    }

    fn read_exec_session_output<'a>(
        &'a self,
        session_id: &'a str,
        drain: bool,
    ) -> impl Future<Output = Result<Option<String>>> + 'a {
        self.exec_sessions.read_session_output(session_id, drain)
    }

    pub(crate) fn exec_session_completed<'a>(
        &'a self,
        session_id: &'a str,
    ) -> impl Future<Output = Result<Option<i32>>> + 'a {
        self.exec_sessions.is_session_completed(session_id)
    }

    fn exec_session_activity_receiver<'a>(
        &'a self,
        session_id: &'a str,
    ) -> impl Future<Output = Result<Option<tokio::sync::watch::Receiver<u64>>>> + 'a {
        self.exec_sessions.activity_receiver(session_id)
    }

    fn exec_session_output_drained<'a>(&'a self, session_id: &'a str) -> impl Future<Output = Result<bool>> + 'a {
        self.exec_sessions.is_output_drained(session_id)
    }

    async fn attach_pipe_output_metadata(
        &self,
        response: &mut Value,
        session_id: &str,
        session_exited: bool,
    ) -> Result<bool> {
        if let Some(stats) = self.exec_sessions.output_stats(session_id).await? {
            let safe_to_prune = !stats.spool_available || stats.spool_complete;
            let spooler_config = self.output_spooler.config();
            attach_spool_metadata(
                response,
                &stats,
                session_exited,
                spooler_config.enabled,
                spooler_config.threshold_bytes,
            );
            return Ok(safe_to_prune);
        }
        Ok(true)
    }

    fn handle_closed_exec_session(&self, session_metadata: &VTCodeExecSession) {
        if is_pty_exec_session(session_metadata) {
            self.decrement_active_pty_sessions();
        }
    }

    async fn prune_completed_exec_session(&self, session_id: &str) -> Result<()> {
        if let Some(session_metadata) = self.exec_sessions.prune_exited_session(session_id).await? {
            self.handle_closed_exec_session(&session_metadata);
        }
        Ok(())
    }

    pub(crate) async fn close_exec_session(&self, session_id: &str) -> Result<VTCodeExecSession> {
        let session_metadata = self.exec_sessions.close_session(session_id).await?;
        self.handle_closed_exec_session(&session_metadata);
        Ok(session_metadata)
    }

    async fn wait_for_exec_yield(
        &self,
        session_id: &str,
        yield_duration: Duration,
        tool_name: Option<&str>,
        drain_output: bool,
    ) -> PtyEphemeralCapture {
        let mut output = String::new();
        let mut peeked_bytes = 0usize;
        let start = Instant::now();
        let poll_interval = Duration::from_millis(50);
        let mut activity_rx = self.exec_session_activity_receiver(session_id).await.ok().flatten();

        let progress_callback = self.progress_callback();
        let mut last_ui_update = Instant::now();
        let ui_update_interval = Duration::from_millis(100);
        let mut pending_lines = String::new();

        loop {
            let observed_activity = activity_rx.as_mut().map(|receiver| *receiver.borrow_and_update());

            if let Ok(Some(code)) = self.exec_session_completed(session_id).await {
                if let Ok(Some(final_output)) =
                    self.next_exec_session_output(session_id, drain_output, &mut peeked_bytes).await
                {
                    append_bounded_capture(&mut output, &final_output);

                    if let Some(tool_name) = tool_name
                        && let Some(ref callback) = progress_callback
                    {
                        pending_lines.push_str(&final_output);
                        if !pending_lines.is_empty() {
                            callback(tool_name, &pending_lines);
                        }
                    }
                }
                let quiet_window = Duration::from_millis(200);
                let drain_deadline = Instant::now() + Duration::from_millis(1000);
                let mut last_output_at = Instant::now();
                while Instant::now() < drain_deadline {
                    match self.next_exec_session_output(session_id, drain_output, &mut peeked_bytes).await {
                        Ok(Some(extra_output)) => {
                            append_bounded_capture(&mut output, &extra_output);
                            if let Some(tool_name) = tool_name
                                && let Some(ref callback) = progress_callback
                            {
                                pending_lines.push_str(&extra_output);
                                if !pending_lines.is_empty() {
                                    callback(tool_name, &pending_lines);
                                    pending_lines.clear();
                                }
                            }
                            last_output_at = Instant::now();
                        }
                        Ok(None) | Err(_) => {
                            let output_drained = self.exec_session_output_drained(session_id).await.unwrap_or(true);
                            if output_drained && Instant::now().duration_since(last_output_at) >= quiet_window {
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(15)).await;
                        }
                    }
                }
                return PtyEphemeralCapture {
                    output,
                    exit_code: Some(code),
                    duration: start.elapsed(),
                };
            }

            if let Ok(Some(new_output)) =
                self.next_exec_session_output(session_id, drain_output, &mut peeked_bytes).await
            {
                append_bounded_capture(&mut output, &new_output);
                if tool_name.is_some() {
                    pending_lines.push_str(&new_output);
                }

                if let Some(tool_name) = tool_name
                    && let Some(ref callback) = progress_callback
                {
                    let now = Instant::now();
                    if (now.duration_since(last_ui_update) >= ui_update_interval || pending_lines.contains('\n'))
                        && !pending_lines.is_empty()
                    {
                        callback(tool_name, &pending_lines);
                        pending_lines.clear();
                        last_ui_update = now;
                    }
                }
            }

            if start.elapsed() >= yield_duration {
                let output_drained = self.exec_session_output_drained(session_id).await.unwrap_or(false);
                if output_drained {
                    let exit_grace_deadline = Instant::now() + Duration::from_millis(250);
                    while Instant::now() < exit_grace_deadline {
                        if let Ok(Some(code)) = self.exec_session_completed(session_id).await {
                            if let Some(tool_name) = tool_name
                                && let Some(ref callback) = progress_callback
                                && !pending_lines.is_empty()
                            {
                                callback(tool_name, &pending_lines);
                            }
                            return PtyEphemeralCapture {
                                output,
                                exit_code: Some(code),
                                duration: start.elapsed(),
                            };
                        }
                        tokio::time::sleep(Duration::from_millis(15)).await;
                    }
                }
                if let Some(tool_name) = tool_name
                    && let Some(ref callback) = progress_callback
                    && !pending_lines.is_empty()
                {
                    callback(tool_name, &pending_lines);
                }
                return PtyEphemeralCapture { output, exit_code: None, duration: start.elapsed() };
            }

            if let Some(observed_version) = observed_activity
                && let Some(activity_rx) = activity_rx.as_mut()
            {
                if *activity_rx.borrow() != observed_version {
                    continue;
                }

                tokio::select! {
                    _ = activity_rx.changed() => {}
                    _ = tokio::time::sleep(poll_interval) => {}
                }
            } else {
                tokio::time::sleep(poll_interval).await;
            }
        }
    }

    async fn capture_exec_session_output(
        &self,
        session_id: &str,
        yield_duration: Duration,
        tool_name: Option<&str>,
        settle_until_terminal: bool,
    ) -> Result<PtyEphemeralCapture> {
        if !settle_until_terminal {
            return Ok(self.wait_for_exec_yield(session_id, yield_duration, tool_name, true).await);
        }

        let start = Instant::now();
        let mut output = String::new();

        loop {
            let capture = self.wait_for_exec_yield(session_id, yield_duration, tool_name, true).await;
            append_bounded_capture(&mut output, &capture.output);

            if let Some(exit_code) = capture.exit_code {
                return Ok(PtyEphemeralCapture {
                    output,
                    exit_code: Some(exit_code),
                    duration: start.elapsed(),
                });
            }

            self.exec_session_metadata(session_id)
                .await
                .with_context(|| format!("exec session '{session_id}' disappeared during settlement"))?;
        }
    }

    async fn next_exec_session_output(
        &self,
        session_id: &str,
        drain_output: bool,
        peeked_bytes: &mut usize,
    ) -> Result<Option<String>> {
        let Some(output) = self.read_exec_session_output(session_id, drain_output).await? else {
            return Ok(None);
        };
        if drain_output {
            return Ok(Some(output));
        }
        if output.len() <= *peeked_bytes {
            return Ok(None);
        }

        let next = output
            .get(*peeked_bytes..)
            .ok_or_else(|| anyhow!("exec session '{session_id}' output boundary became invalid"))?
            .to_string();
        *peeked_bytes = output.len();
        if next.is_empty() { Ok(None) } else { Ok(Some(next)) }
    }

    async fn finalize_exec_run_response(
        &self,
        session_metadata: &VTCodeExecSession,
        command_display: &str,
        output_config: &ExecRunOutputConfig,
        is_git_diff: bool,
        retain_completed_session: bool,
        capture: PtyEphemeralCapture,
    ) -> Result<Value> {
        let mut response = build_exec_filtered_response(
            session_metadata,
            command_display,
            &capture,
            output_config,
            Some(session_metadata.id.as_str()),
        )?;

        let spool_safe_to_prune = self
            .attach_pipe_output_metadata(&mut response, session_metadata.id.as_str(), capture.exit_code.is_some())
            .await?;
        self.prune_session_if_exited(
            session_metadata.id.as_str(),
            capture.exit_code,
            retain_completed_session,
            spool_safe_to_prune,
        )
        .await?;
        annotate_exec_run_response(&mut response, is_git_diff);

        Ok(response)
    }

    async fn build_passthrough_exec_response(
        &self,
        session: &ResolvedExecSession,
        yield_duration: Duration,
        settle_until_terminal: bool,
        max_tokens: Option<usize>,
        progress_tool_name: &'static str,
    ) -> Result<Value> {
        let capture = self
            .capture_exec_session_output(
                session.metadata.id.as_str(),
                yield_duration,
                Some(progress_tool_name),
                settle_until_terminal,
            )
            .await?;
        let mut response =
            build_exec_passthrough_response(&session.metadata, &session.command_display, &capture, max_tokens);

        let spool_safe_to_prune = self
            .attach_pipe_output_metadata(&mut response, session.metadata.id.as_str(), capture.exit_code.is_some())
            .await?;
        self.prune_session_if_exited(session.metadata.id.as_str(), capture.exit_code, false, spool_safe_to_prune)
            .await?;

        Ok(response)
    }

    async fn prune_session_if_exited(
        &self,
        session_id: &str,
        exit_code: Option<i32>,
        retain_completed_session: bool,
        spool_safe_to_prune: bool,
    ) -> Result<()> {
        if exit_code.is_some() && !retain_completed_session {
            if !spool_safe_to_prune {
                // The response observed a healthy but incomplete spool. Keep
                // the exited session so a later wait can expose its completed
                // reference, even if the writer finishes before this check.
                return Ok(());
            }
            // Do not abort the output task while it is still flushing the
            // complete, sanitized spool. A later explicit wait can finish
            // the checkpoint and safely prune the exited session.
            if !self.exec_session_output_drained(session_id).await.unwrap_or(false) {
                return Ok(());
            }
            self.prune_completed_exec_session(session_id).await?;
        }
        Ok(())
    }

    async fn resolve_exec_session(
        &self,
        args: &Value,
        object_error: &str,
        session_id_error: &str,
        validation_context: &str,
    ) -> Result<ResolvedExecSession> {
        let session_id = resolve_exec_session_id(args, object_error, session_id_error, validation_context)?;
        let metadata = self.exec_session_metadata(&session_id).await?;
        Ok(ResolvedExecSession::new(metadata))
    }
}

fn attach_spool_metadata(
    response: &mut Value,
    stats: &crate::tools::exec_session::PipeOutputStats,
    session_exited: bool,
    spooling_enabled: bool,
    spool_threshold_bytes: usize,
) {
    response["total_output_bytes"] = json!(stats.total_bytes);
    response["output_truncated"] = json!(stats.truncated);

    let output_exceeds_threshold = stats.total_bytes > spool_threshold_bytes as u64;
    let response_capped = ["truncated", "query_truncated"]
        .into_iter()
        .any(|field| response.get(field).and_then(Value::as_bool).unwrap_or(false));
    if !spooling_enabled || !(output_exceeds_threshold || stats.truncated || response_capped) {
        return;
    }

    if stats.spool_available && (stats.spool_complete || !session_exited) {
        response["spool_path"] = json!(stats.spool_path);
        response["output_spooled"] = json!(true);
        response["spool_complete"] = json!(stats.spool_complete);
    } else if stats.spool_available {
        response["spool_complete"] = json!(false);
        response["spool_pending"] = json!(true);
    }
}

fn exec_session_payload<'a>(args: &'a Value, object_error: &str) -> Result<&'a serde_json::Map<String, Value>> {
    args.as_object().ok_or_else(|| anyhow!("{object_error}"))
}

fn resolve_exec_session_id(
    args: &Value,
    object_error: &str,
    session_id_error: &str,
    validation_context: &str,
) -> Result<String> {
    let _payload = exec_session_payload(args, object_error)?;
    let raw_sid = crate::tools::command_args::session_id_text(args).ok_or_else(|| anyhow!("{session_id_error}"))?;
    Ok(validate_exec_session_id(raw_sid, validation_context)?.to_string())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::attach_spool_metadata;
    use crate::tools::exec_session::PipeOutputStats;

    #[test]
    fn incomplete_spool_is_not_advertised_as_a_reusable_reference() {
        let stats = PipeOutputStats {
            total_bytes: 42,
            truncated: true,
            spool_path: ".vtcode/context/tool_outputs/run-1.txt".to_string(),
            spool_available: true,
            spool_complete: false,
        };
        let mut response = json!({});

        attach_spool_metadata(&mut response, &stats, true, true, 32);

        assert_eq!(response["total_output_bytes"], 42);
        assert_eq!(response["output_truncated"], true);
        assert_eq!(response["spool_complete"], false);
        assert_eq!(response["spool_pending"], true);
        assert!(response.get("spool_path").is_none());
        assert!(response.get("output_spooled").is_none());
    }

    #[test]
    fn active_partial_spool_is_advertised_as_a_readable_snapshot() {
        let stats = PipeOutputStats {
            total_bytes: 42,
            truncated: true,
            spool_path: ".vtcode/context/tool_outputs/run-1.txt".to_string(),
            spool_available: true,
            spool_complete: false,
        };
        let mut response = json!({});

        attach_spool_metadata(&mut response, &stats, false, true, 32);

        assert_eq!(response["spool_path"], stats.spool_path);
        assert_eq!(response["output_spooled"], true);
        assert_eq!(response["spool_complete"], false);
        assert!(response.get("spool_pending").is_none());
    }

    #[test]
    fn complete_spool_is_advertised_with_a_reference() {
        let stats = PipeOutputStats {
            total_bytes: 42,
            truncated: false,
            spool_path: ".vtcode/context/tool_outputs/run-1.txt".to_string(),
            spool_available: true,
            spool_complete: true,
        };
        let mut response = json!({});

        attach_spool_metadata(&mut response, &stats, true, true, 32);

        assert_eq!(response["spool_path"], stats.spool_path);
        assert_eq!(response["output_spooled"], true);
        assert_eq!(response["spool_complete"], true);
        assert!(response.get("spool_pending").is_none());
    }

    #[test]
    fn small_output_keeps_lossless_spool_internal() {
        let stats = PipeOutputStats {
            total_bytes: 42,
            truncated: false,
            spool_path: ".vtcode/context/tool_outputs/run-1.txt".to_string(),
            spool_available: true,
            spool_complete: true,
        };
        let mut response = json!({});

        attach_spool_metadata(&mut response, &stats, true, true, 64);

        assert_eq!(response["total_output_bytes"], 42);
        assert_eq!(response["output_truncated"], false);
        assert!(response.get("spool_path").is_none());
        assert!(response.get("output_spooled").is_none());
        assert!(response.get("spool_complete").is_none());
        assert!(response.get("spool_pending").is_none());
    }

    #[test]
    fn disabled_dynamic_spooling_keeps_diagnostics_without_spool_metadata() {
        let stats = PipeOutputStats {
            total_bytes: 8_192,
            truncated: true,
            spool_path: ".vtcode/context/tool_outputs/run-1.txt".to_string(),
            spool_available: true,
            spool_complete: true,
        };
        let mut response = json!({});

        attach_spool_metadata(&mut response, &stats, true, false, 64);

        assert_eq!(response["total_output_bytes"], 8_192);
        assert_eq!(response["output_truncated"], true);
        assert!(response.get("spool_path").is_none());
        assert!(response.get("output_spooled").is_none());
        assert!(response.get("spool_pending").is_none());
    }

    #[test]
    fn response_cap_advertises_spool_even_below_threshold() {
        let stats = PipeOutputStats {
            total_bytes: 42,
            truncated: false,
            spool_path: ".vtcode/context/tool_outputs/run-1.txt".to_string(),
            spool_available: true,
            spool_complete: true,
        };
        let mut response = json!({"truncated": true});

        attach_spool_metadata(&mut response, &stats, true, true, 64);

        assert_eq!(response["spool_path"], stats.spool_path);
        assert_eq!(response["output_spooled"], true);
        assert_eq!(response["spool_complete"], true);
    }

    #[test]
    fn query_cap_advertises_spool_even_below_threshold() {
        let stats = PipeOutputStats {
            total_bytes: 42,
            truncated: false,
            spool_path: ".vtcode/context/tool_outputs/run-1.txt".to_string(),
            spool_available: true,
            spool_complete: true,
        };
        let mut response = json!({"query_truncated": true});

        attach_spool_metadata(&mut response, &stats, true, true, 64);

        assert_eq!(response["spool_path"], stats.spool_path);
        assert_eq!(response["output_spooled"], true);
        assert_eq!(response["spool_complete"], true);
    }
}
