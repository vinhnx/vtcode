//! Tool event emitter (from Codex)
//!
//! Handles the tool execution lifecycle:
//! - Diff-tracker updates for apply_patch begin/end
//! - Structured tracing for begin/success/failure stages
//!
//! Lifecycle visibility into the canonical event stream flows through the
//! runloop's `ThreadEvent` recorder (`item.*`), not through this module.

use crate::config::constants::tools;
use crate::types::CompactStr;
use hashbrown::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use super::sandboxing::{ExecToolCallOutput, ToolError};
use super::tool_handler::{DiffTracker, FileChange, ToolCallError, TurnContext};

/// Context for emitting tool events
pub struct ToolEventCtx<'a> {
    pub turn: &'a TurnContext,
    pub call_id: &'a str,
    pub turn_diff_tracker: Option<&'a Arc<tokio::sync::Mutex<DiffTracker>>>,
}

impl<'a> ToolEventCtx<'a> {
    pub fn new(
        turn: &'a TurnContext,
        call_id: &'a str,
        tracker: Option<&'a Arc<tokio::sync::Mutex<DiffTracker>>>,
    ) -> Self {
        Self { turn, call_id, turn_diff_tracker: tracker }
    }
}

/// Event stage during tool execution
#[derive(Clone, Debug)]
pub enum ToolEventStage {
    Begin,
    Success(ExecToolCallOutput),
    Failure(ToolEventFailureKind),
}

/// Failure kind for tool events
#[derive(Clone, Debug)]
pub enum ToolEventFailureKind {
    Output(ExecToolCallOutput),
    Message(String),
    Error(String),
}

/// Command source for tracking where commands originate
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExecCommandSource {
    #[default]
    Agent,
    User,
    CommandSessionStartup,
    CommandSessionWriteStdin,
}

/// Parsed command information
#[derive(Clone, Debug)]
pub struct ParsedCommand {
    pub program: String,
    pub args: Vec<String>,
}

/// Parse command from argv
pub fn parse_command(command: &[String]) -> ParsedCommand {
    let program = command.first().cloned().unwrap_or_default();
    let args = command.get(1..).map(|s| s.to_vec()).unwrap_or_default();
    ParsedCommand { program, args }
}

/// Tool event emitter for different tool types (from Codex)
#[derive(Clone, Debug)]
pub enum ToolEmitter {
    /// Shell command execution
    Shell {
        command: Vec<String>,
        cwd: PathBuf,
        source: ExecCommandSource,
        parsed_cmd: ParsedCommand,
        freeform: bool,
    },
    /// Apply patch operation
    ApplyPatch {
        changes: HashMap<PathBuf, FileChange>,
        auto_approved: bool,
    },
    /// Unified command-session execution
    CommandSession {
        command: Vec<String>,
        cwd: PathBuf,
        source: ExecCommandSource,
        interaction_input: Option<String>,
        parsed_cmd: ParsedCommand,
        process_id: Option<String>,
    },
    /// Generic tool execution
    Generic { tool_name: CompactStr },
}

impl ToolEmitter {
    /// Create emitter for shell commands
    pub fn shell(command: Vec<String>, cwd: PathBuf, source: ExecCommandSource, freeform: bool) -> Self {
        let parsed_cmd = parse_command(&command);
        Self::Shell { command, cwd, source, parsed_cmd, freeform }
    }

    /// Create emitter for apply_patch
    pub fn apply_patch(changes: HashMap<PathBuf, FileChange>, auto_approved: bool) -> Self {
        Self::ApplyPatch { changes, auto_approved }
    }

    /// Create emitter for command-session execution.
    pub fn command_session(
        command: &[String],
        cwd: PathBuf,
        source: ExecCommandSource,
        process_id: Option<String>,
    ) -> Self {
        let parsed_cmd = parse_command(command);
        Self::CommandSession {
            command: command.to_vec(),
            cwd,
            source,
            interaction_input: None,
            parsed_cmd,
            process_id,
        }
    }

    /// Create emitter for generic tools
    pub fn generic(tool_name: impl Into<CompactStr>) -> Self {
        Self::Generic { tool_name: tool_name.into() }
    }

    /// Emit event for current stage
    pub async fn emit(&self, ctx: ToolEventCtx<'_>, stage: ToolEventStage) {
        match (self, &stage) {
            // Apply patch begin
            (Self::ApplyPatch { changes, auto_approved }, ToolEventStage::Begin) => {
                // Update diff tracker
                if let Some(tracker) = ctx.turn_diff_tracker {
                    let mut guard = tracker.lock().await;
                    guard.on_patch_begin(changes);
                }

                tracing::debug!(
                    call_id = %ctx.call_id,
                    turn_id = %ctx.turn.turn_id,
                    changes = changes.len(),
                    auto_approved = *auto_approved,
                    "patch apply began"
                );
            }

            // Apply patch success
            (Self::ApplyPatch { changes: _, .. }, ToolEventStage::Success(output)) => {
                self.emit_patch_end(ctx, output.stdout.clone(), output.stderr.clone(), true)
                    .await;
            }

            // Apply patch failure
            (Self::ApplyPatch { .. }, ToolEventStage::Failure(ToolEventFailureKind::Output(output))) => {
                self.emit_patch_end(ctx, output.stdout.clone(), output.stderr.clone(), false)
                    .await;
            }

            (Self::ApplyPatch { .. }, ToolEventStage::Failure(ToolEventFailureKind::Message(msg))) => {
                self.emit_patch_end(ctx, String::new(), msg.clone(), false).await;
            }

            // Shell/CommandSession begin
            (Self::Shell { .. } | Self::CommandSession { .. }, ToolEventStage::Begin) => {
                tracing::debug!(
                    tool = %self.tool_name(),
                    call_id = %ctx.call_id,
                    turn_id = %ctx.turn.turn_id,
                    "Tool execution started"
                );
            }

            // Shell/CommandSession success
            (Self::Shell { .. } | Self::CommandSession { .. }, ToolEventStage::Success(_)) => {
                tracing::debug!(call_id = %ctx.call_id, "Tool execution succeeded");
            }

            // Shell/CommandSession failure
            (Self::Shell { .. } | Self::CommandSession { .. }, ToolEventStage::Failure(kind)) => {
                let error = match kind {
                    ToolEventFailureKind::Output(output) => output.combined_output(),
                    ToolEventFailureKind::Message(msg) => msg.clone(),
                    ToolEventFailureKind::Error(err) => err.clone(),
                };
                tracing::warn!(call_id = %ctx.call_id, error = %error, "Tool execution failed");
            }

            // Generic tool events
            (Self::Generic { tool_name }, ToolEventStage::Begin) => {
                tracing::debug!(
                    tool = %tool_name,
                    call_id = %ctx.call_id,
                    turn_id = %ctx.turn.turn_id,
                    "Tool execution started"
                );
            }

            (Self::Generic { .. }, ToolEventStage::Success(_)) => {
                tracing::debug!(call_id = %ctx.call_id, "Tool execution succeeded");
            }

            (Self::Generic { .. }, ToolEventStage::Failure(kind)) => {
                let error = match kind {
                    ToolEventFailureKind::Output(output) => output.combined_output(),
                    ToolEventFailureKind::Message(msg) => msg.clone(),
                    ToolEventFailureKind::Error(err) => err.clone(),
                };
                tracing::warn!(call_id = %ctx.call_id, error = %error, "Tool execution failed");
            }

            _ => {}
        }
    }

    /// Emit begin event
    pub async fn begin(&self, ctx: ToolEventCtx<'_>) {
        self.emit(ctx, ToolEventStage::Begin).await;
    }

    /// Complete execution and format output for model
    pub async fn finish(
        &self,
        ctx: ToolEventCtx<'_>,
        result: Result<ExecToolCallOutput, ToolError>,
    ) -> Result<String, ToolCallError> {
        match result {
            Ok(output) => {
                self.emit(ctx, ToolEventStage::Success(output.clone())).await;
                Ok(self.format_output_for_model(&output))
            }
            Err(ToolError::Rejected(msg)) => {
                self.emit(ctx, ToolEventStage::Failure(ToolEventFailureKind::Message(msg.clone())))
                    .await;
                Err(ToolCallError::Rejected(msg))
            }
            Err(ToolError::Timeout(ms)) => {
                let msg = format!("Command timed out after {ms}ms");
                self.emit(ctx, ToolEventStage::Failure(ToolEventFailureKind::Message(msg.clone())))
                    .await;
                Err(ToolCallError::Timeout(ms))
            }
            Err(e) => {
                let msg = e.to_string();
                self.emit(ctx, ToolEventStage::Failure(ToolEventFailureKind::Error(msg.clone())))
                    .await;
                Err(ToolCallError::Internal(e.into()))
            }
        }
    }

    /// Format output for model consumption
    fn format_output_for_model(&self, output: &ExecToolCallOutput) -> String {
        let mut result = String::with_capacity(output.stdout.len() + output.stderr.len() + 32);

        if !output.stdout.is_empty() {
            result.push_str(&output.stdout);
        }

        if !output.stderr.is_empty() {
            if !result.is_empty() {
                result.push_str("\n\n[stderr]\n");
            }
            result.push_str(&output.stderr);
        }

        if output.exit_code != 0 {
            if !result.is_empty() {
                result.push('\n');
            }
            let _ = write!(result, "[exit code: {}]", output.exit_code);
        }

        if result.is_empty() {
            result.push_str("[no output]");
        }

        result
    }

    /// Get tool name for this emitter
    fn tool_name(&self) -> CompactStr {
        match self {
            Self::Shell { .. } => CompactStr::from(tools::SHELL),
            Self::ApplyPatch { .. } => CompactStr::from(tools::APPLY_PATCH),
            Self::CommandSession { .. } => CompactStr::from(tools::EXEC_COMMAND),
            Self::Generic { tool_name } => tool_name.clone(),
        }
    }

    /// Emit patch end event
    async fn emit_patch_end(&self, ctx: ToolEventCtx<'_>, _stdout: String, _stderr: String, success: bool) {
        // Update diff tracker
        if let Some(tracker) = ctx.turn_diff_tracker {
            let mut guard = tracker.lock().await;
            guard.on_patch_end(success);
        }

        tracing::debug!(call_id = %ctx.call_id, success, "patch apply finished");
    }
}

/// Input for exec commands
#[derive(Clone, Debug)]
pub struct ExecCommandInput<'a> {
    pub command: &'a [String],
    pub cwd: &'a std::path::Path,
    pub parsed_cmd: &'a ParsedCommand,
    pub source: ExecCommandSource,
    pub timeout_ms: Option<u64>,
    pub justification: Option<&'a str>,
}

impl<'a> ExecCommandInput<'a> {
    pub fn new(
        command: &'a [String],
        cwd: &'a std::path::Path,
        parsed_cmd: &'a ParsedCommand,
        source: ExecCommandSource,
        timeout_ms: Option<u64>,
        justification: Option<&'a str>,
    ) -> Self {
        Self {
            command,
            cwd,
            parsed_cmd,
            source,
            timeout_ms,
            justification,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_command() {
        let cmd = vec!["ls".to_string(), "-la".to_string(), "/tmp".to_string()];
        let parsed = parse_command(&cmd);

        assert_eq!(parsed.program, "ls");
        assert_eq!(parsed.args, vec!["-la", "/tmp"]);
    }

    #[test]
    fn test_parse_command_empty() {
        let cmd: Vec<String> = vec![];
        let parsed = parse_command(&cmd);

        assert_eq!(parsed.program, "");
        assert!(parsed.args.is_empty());
    }

    #[test]
    fn test_emitter_tool_names() {
        let shell = ToolEmitter::shell(vec!["ls".to_string()], PathBuf::new(), ExecCommandSource::Agent, false);
        assert_eq!(shell.tool_name(), "shell");

        let patch = ToolEmitter::apply_patch(HashMap::new(), true);
        assert_eq!(patch.tool_name(), "apply_patch");

        let exec = ToolEmitter::command_session(&["echo".to_string()], PathBuf::new(), ExecCommandSource::Agent, None);
        assert_eq!(exec.tool_name(), "exec_command");

        let generic = ToolEmitter::generic("custom_tool");
        assert_eq!(generic.tool_name(), "custom_tool");
    }

    #[test]
    fn test_format_output_for_model() {
        let emitter = ToolEmitter::generic("test");

        // Success with output
        let output = ExecToolCallOutput {
            stdout: "Hello, world!".to_string(),
            stderr: String::new(),
            exit_code: 0,
        };
        assert_eq!(emitter.format_output_for_model(&output), "Hello, world!");

        // Failure with stderr
        let output = ExecToolCallOutput {
            stdout: String::new(),
            stderr: "Error!".to_string(),
            exit_code: 1,
        };
        assert_eq!(emitter.format_output_for_model(&output), "Error!\n[exit code: 1]");

        // No output
        let output = ExecToolCallOutput::default();
        assert_eq!(emitter.format_output_for_model(&output), "[no output]");
    }
}
