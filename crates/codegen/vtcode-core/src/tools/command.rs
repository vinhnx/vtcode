//! Command execution tool

use super::types::*;
use crate::command_safety::UnifiedCommandEvaluator;
use crate::command_safety::command_might_be_dangerous;
use crate::command_safety::unified::EvaluationReason;
use crate::config::CommandsConfig;
use crate::exec_policy::command_validation::{sanitize_working_dir, validate_command};
use crate::tools::command_policy::CommandPolicyEvaluator;
use crate::tools::path_env;
use crate::tools::shell::resolve_fallback_shell;
use anyhow::{Result, anyhow};
#[cfg(test)]
use hashbrown::HashMap;
#[cfg(test)]
use std::ffi::OsString;
use std::path::PathBuf;

/// Known-safe shell binaries that can be used as shell overrides.
/// This list is intentionally restrictive to prevent prompt-injected LLMs
/// from specifying arbitrary executables as the shell.
const SAFE_SHELLS: &[&str] = &[
    "/bin/sh",
    "/bin/bash",
    "/bin/zsh",
    "/usr/bin/sh",
    "/usr/bin/bash",
    "/usr/bin/zsh",
    "/bin/dash",
    "/usr/bin/dash",
    "sh",
    "bash",
    "zsh",
    "dash",
];

/// Validate that a shell override is one of the known-safe shells.
///
/// Returns `Ok(shell)` if the shell is safe, or an error if it's not in the allowed list.
fn validate_shell_override(shell: &str) -> Result<String> {
    let trimmed = shell.trim();

    // Check if it's in the safe list (exact match or basename match)
    for safe_shell in SAFE_SHELLS {
        if trimmed == *safe_shell {
            return Ok(trimmed.to_string());
        }
        // Also match basename (e.g., "/usr/local/bin/bash" matches "bash")
        if let Some(basename) = PathBuf::from(trimmed).file_name().and_then(|n| n.to_str()) {
            if basename == *safe_shell {
                return Ok(trimmed.to_string());
            }
        }
    }

    Err(anyhow!(
        "shell '{}' is not in the allowed list. \
         Allowed shells: {}",
        trimmed,
        SAFE_SHELLS.join(", ")
    ))
}

/// Command execution tool for non-PTY process handling with policy enforcement
#[derive(Clone)]
pub struct CommandTool {
    workspace_root: PathBuf,
    policy: CommandPolicyEvaluator,
    /// Unified command evaluator combining policy and safety rules
    unified_evaluator: UnifiedCommandEvaluator,
    extra_path_entries: Vec<PathBuf>,
}

impl CommandTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self::with_commands_config(workspace_root, CommandsConfig::default())
    }

    pub fn with_commands_config(workspace_root: PathBuf, commands_config: CommandsConfig) -> Self {
        // Note: We use the workspace_root directly here. Full validation happens
        // in prepare_invocation which is async.
        let policy = CommandPolicyEvaluator::from_config(&commands_config);
        let unified_evaluator = UnifiedCommandEvaluator::new();
        let extra_path_entries =
            path_env::compute_extra_search_paths(&commands_config.extra_path_entries, &workspace_root);
        Self {
            workspace_root,
            policy,
            unified_evaluator,
            extra_path_entries,
        }
    }

    pub fn update_commands_config(&mut self, commands_config: &CommandsConfig) {
        self.policy = CommandPolicyEvaluator::from_config(commands_config);
        self.unified_evaluator = UnifiedCommandEvaluator::new();
        self.extra_path_entries =
            path_env::compute_extra_search_paths(&commands_config.extra_path_entries, &self.workspace_root);
    }

    /// Check the configured command policy for an argv request.
    pub fn policy_allows(&self, command: &[String]) -> bool {
        self.policy.allows(command)
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Legacy invocation preparation remains available to test-only compatibility paths."
        )
    )]
    async fn prepare_invocation(&self, input: &EnhancedTerminalInput) -> Result<CommandInvocation> {
        let command = &input.command;
        if command.is_empty() {
            return Err(anyhow!("Command cannot be empty"));
        }

        let program = &command[0];
        // Validate that the executable is non-empty after trimming
        if program.trim().is_empty() {
            return Err(anyhow!("Command executable cannot be empty"));
        }
        if program.contains(char::is_whitespace) {
            return Err(anyhow!("Program name cannot contain whitespace: {program}"));
        }

        let working_dir = sanitize_working_dir(&self.workspace_root, input.working_dir.as_deref()).await?;

        // Unified command evaluation: combines safety rules + policy rules
        let confirm_ok = input.confirm.unwrap_or(false);
        let risky_command = is_risky_command(command);
        if risky_command && !confirm_ok {
            return Err(anyhow!(
                "Command appears destructive. Do not self-approve: surface it to the operator, and only retry with `confirm: true` after the operator explicitly approves this exact command."
            ));
        }

        let policy_allowed = self.policy.allows(command);

        // Use unified evaluator with policy layer
        let eval_result = self
            .unified_evaluator
            .evaluate_with_policy(command, policy_allowed, "config policy")
            .await?;

        if !eval_result.allowed {
            if !policy_allowed {
                return Err(anyhow!("command '{program}' is not permitted by the execution policy"));
            }
            // If unified evaluator denied, still allow explicitly confirmed risky commands
            // when they are permitted by the configured policy.
            let allow_confirmed_risky = risky_command
                && confirm_ok
                && policy_allowed
                && matches!(eval_result.primary_reason, EvaluationReason::DangerousCommand(_));
            if !allow_confirmed_risky {
                // If unified evaluator denied, forward to validator for custom checks
                validate_command(command, &self.workspace_root, &working_dir, confirm_ok).await?;
            }
        }

        if risky_command && confirm_ok {
            // Record audit for the explicitly confirmed destructive command
            log_audit_for_command(&format_command(command), "Confirmed destructive operation by agent");
        }

        // If the program name includes a path separator or is absolute, execute it directly as provided
        // (unless the caller explicitly requested a shell override). Otherwise, always use the
        // user's login shell in `-lc` mode so PATH and environment are initialized consistently.
        let resolved_invocation = if program.contains(std::path::MAIN_SEPARATOR) || program.contains('/') {
            // Program provided as absolute/relative path: run directly
            CommandInvocation {
                program: program.to_owned(),
                args: command[1..].to_vec(),
                display: input.raw_command.clone().unwrap_or_else(|| format_command(command)),
            }
        } else {
            // Honor explicit shell override provided in the input. If the caller set `login` to
            // false, use `-c` (no login). Otherwise use `-lc` to force login shell semantics.
            let shell = if let Some(ref shell_override) = input.shell {
                if !shell_override.trim().is_empty() {
                    // Validate the shell override against the safe list
                    validate_shell_override(shell_override)?
                } else {
                    resolve_fallback_shell()
                }
            } else {
                resolve_fallback_shell()
            };
            let use_login = input.login.unwrap_or(true);
            let full_command = format_command(command);
            CommandInvocation {
                program: shell,
                args: vec![
                    if use_login { "-lc".to_owned() } else { "-c".to_owned() },
                    full_command.clone(),
                ],
                display: full_command,
            }
        };

        Ok(resolved_invocation)
    }

    /// Validate command arguments without executing them (test/helper)
    #[cfg(test)]
    async fn validate_args(&self, input: &EnhancedTerminalInput) -> Result<()> {
        self.prepare_invocation(input).await.map(|_| ())
    }
}

// NOTE: Tool and ModeTool trait implementations removed since CommandTool
// is no longer registered as a public tool (RUN_COMMAND was deprecated).
// CommandTool is kept for internal command preparation in the PTY system.

#[derive(Debug, Clone)]
#[allow(dead_code, reason = "Intentional compatibility, platform, or test-only suppression.")]
pub(crate) struct CommandInvocation {
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
    pub(crate) display: String,
}

fn format_command(command: &[String]) -> String {
    command
        .iter()
        .map(|part| quote_argument_posix(part))
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_risky_command(command: &[String]) -> bool {
    if command.is_empty() {
        return false;
    }

    // Centralized detection for dangerous command patterns (git/rm/mkfs/dd/etc.).
    if command_might_be_dangerous(command) {
        return true;
    }

    let program = command[0].as_str();
    let args = &command[1..];

    // Supplemental checks outside centralized detection coverage.
    if program == "rm" && args.iter().any(|a| a == "/") {
        return true;
    }

    if program == "docker" && args.iter().any(|a| a == "run" && args.iter().any(|b| b == "--privileged")) {
        return true;
    }

    program == "kubectl" // kubectl operations can be destructive; require confirmation
}

fn log_audit_for_command(_command: &str, _reason: &str) {
    // Audit logging removed - kept as no-op for backwards compatibility
}

fn quote_argument_posix(arg: &str) -> String {
    if arg.is_empty() {
        return "''".to_owned();
    }

    if arg.chars().all(|ch| ch.is_ascii_alphanumeric() || "-_./:@".contains(ch)) {
        return arg.to_owned();
    }

    let mut quoted = String::from("'");
    for ch in arg.chars() {
        if ch == '\'' {
            quoted.push_str("'\"'\"'");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::path_env;
    use tempfile::tempdir;

    fn make_tool() -> CommandTool {
        let cwd = std::env::current_dir().expect("current dir");
        CommandTool::new(cwd)
    }

    fn make_input(command: Vec<&str>) -> EnhancedTerminalInput {
        EnhancedTerminalInput {
            command: command.into_iter().map(String::from).collect(),
            working_dir: None,
            timeout_secs: None,
            mode: None,
            response_format: None,
            raw_command: None,
            shell: None,
            login: None,
            confirm: None,
            max_tokens: None,
        }
    }

    #[test]
    fn formats_command_for_display() {
        let parts = vec!["echo".to_string(), "hello world".to_string()];
        assert_eq!(format_command(&parts), "echo 'hello world'");
    }

    #[tokio::test]
    async fn prepare_invocation_allows_policy_command() {
        let tool = make_tool();
        let input = make_input(vec!["ls"]);
        let invocation = tool.prepare_invocation(&input).await.expect("invocation");
        let shell = resolve_fallback_shell();
        assert_eq!(invocation.program, shell);
        assert_eq!(invocation.args, vec!["-lc".to_owned(), "ls".to_owned()]);
        assert_eq!(invocation.display, "ls");
    }

    #[tokio::test]
    async fn prepare_invocation_allows_cargo_via_policy() {
        let tool = make_tool();
        let input = make_input(vec!["cargo", "check"]);
        let invocation = tool.prepare_invocation(&input).await.expect("cargo check should be allowed");
        let shell = resolve_fallback_shell();
        assert_eq!(invocation.program, shell);
        assert_eq!(invocation.args, vec!["-lc".to_owned(), "cargo check".to_owned()]);
        assert_eq!(invocation.display, "cargo check");
    }

    #[tokio::test]
    async fn prepare_invocation_rejects_command_not_in_policy() {
        let tool = make_tool();
        let input = make_input(vec!["custom-tool"]);
        let error = tool
            .prepare_invocation(&input)
            .await
            .expect_err("custom-tool should be blocked");
        assert!(error.to_string().contains("is not permitted by the execution policy"));
    }

    #[tokio::test]
    async fn prepare_invocation_requires_confirm_for_git_reset_hard() {
        let tool = make_tool();
        let input = make_input(vec!["git", "reset", "--hard"]);
        // No explicit confirm set - should error
        let error = tool
            .prepare_invocation(&input)
            .await
            .expect_err("git reset --hard should require confirmation");
        assert!(error.to_string().contains("Do not self-approve"));
    }

    #[tokio::test]
    async fn prepare_invocation_allows_git_reset_with_confirm() {
        let tool = make_tool();
        let mut input = make_input(vec!["git", "reset", "--hard"]);
        input.confirm = Some(true);
        let invocation = tool
            .prepare_invocation(&input)
            .await
            .expect("git reset --hard should be allowed when confirm=true");
        assert!(invocation.display.contains("git reset"));
    }

    #[tokio::test]
    async fn prepare_invocation_respects_custom_allow_list() {
        let cwd = std::env::current_dir().expect("current dir");
        let mut config = CommandsConfig::default();
        config.allow_list.push("my-build".to_owned());
        let tool = CommandTool::with_commands_config(cwd, config);
        let input = make_input(vec!["my-build"]);
        let invocation = tool
            .prepare_invocation(&input)
            .await
            .expect("custom allow list should enable command");
        let shell = resolve_fallback_shell();
        assert_eq!(invocation.program, shell);
        assert_eq!(invocation.args, vec!["-lc".to_owned(), "my-build".to_owned()]);
    }

    #[tokio::test]
    async fn prepare_invocation_respects_shell_override_and_login_false() {
        let cwd = std::env::current_dir().expect("current dir");
        let tool = CommandTool::new(cwd);
        let mut input = make_input(vec!["ls"]);
        input.shell = Some("/bin/sh".to_string());
        input.login = Some(false);
        let invocation = tool.prepare_invocation(&input).await.expect("invocation");
        assert_eq!(invocation.program, "/bin/sh".to_owned());
        assert_eq!(invocation.args, vec!["-c".to_owned(), "ls".to_owned()]);
    }

    #[test]
    fn resolve_program_path_respects_os_path_separator() {
        let noise_dir = tempdir().expect("noise tempdir");
        let target_dir = tempdir().expect("target tempdir");
        let fake_tool_path = target_dir.path().join("fake-tool");
        std::fs::write(&fake_tool_path, b"#!/bin/sh\n").expect("write fake tool");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&fake_tool_path).expect("metadata").permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&fake_tool_path, perms).expect("set perms");
        }

        let custom_paths = vec![noise_dir.path().to_path_buf(), target_dir.path().to_path_buf()];
        let resolved = path_env::resolve_program_path_from_paths("fake-tool", custom_paths.into_iter());
        let expected = fake_tool_path.to_string_lossy().into_owned();
        assert_eq!(resolved, Some(expected));
    }

    #[tokio::test]
    async fn prepare_invocation_respects_custom_deny_list() {
        let cwd = std::env::current_dir().expect("current dir");
        let mut config = CommandsConfig::default();
        config.deny_list.push("cargo".to_string());
        let tool = CommandTool::with_commands_config(cwd, config);
        let input = make_input(vec!["cargo", "check"]);
        let error = tool.prepare_invocation(&input).await.expect_err("deny list should block cargo");
        assert!(error.to_string().contains("is not permitted"));
    }

    #[tokio::test]
    async fn prepare_invocation_uses_shell_for_command_execution() {
        let tool = make_tool();
        let input = make_input(vec!["cargo", "check"]);
        let invocation = tool.prepare_invocation(&input).await.expect("invocation");
        let shell = resolve_fallback_shell();
        assert_eq!(invocation.program, shell);
        assert_eq!(invocation.args, vec!["-lc".to_owned(), "cargo check".to_owned()]);
        assert_eq!(invocation.display, "cargo check");
    }

    #[tokio::test]
    async fn prepare_invocation_uses_extra_path_entries() {
        let cwd = std::env::current_dir().expect("current dir");
        let temp_dir = tempdir().expect("tempdir");
        let binary_path = temp_dir.path().join("fake-extra");
        std::fs::write(&binary_path, b"#!/bin/sh\n").expect("write fake binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&binary_path).expect("metadata").permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&binary_path, perms).expect("set perms");
        }

        let mut config = CommandsConfig::default();
        config.allow_list.push("fake-extra".to_owned());
        config.extra_path_entries = vec![binary_path.parent().expect("parent").to_string_lossy().into_owned()];

        let tool = CommandTool::with_commands_config(cwd, config);
        let input = make_input(vec!["fake-extra"]);
        let invocation = tool.prepare_invocation(&input).await.expect("extra path should allow command");
        let shell = resolve_fallback_shell();
        assert_eq!(invocation.program, shell);
        assert_eq!(invocation.args, vec!["-lc".to_owned(), "fake-extra".to_owned()]);
        assert_eq!(tool.extra_path_entries, vec![binary_path.parent().expect("parent").to_path_buf()]);
    }

    #[tokio::test]
    async fn working_dir_escape_is_rejected() {
        let tool = make_tool();
        let mut input = make_input(vec!["ls"]);
        input.working_dir = Some("../".into());
        let error = tool
            .prepare_invocation(&input)
            .await
            .expect_err("working dir escape should fail");
        assert!(error.to_string().contains("working directory '../' escapes the workspace root"));
    }

    #[tokio::test]
    async fn prepare_invocation_rejects_empty_command() {
        let tool = make_tool();
        let input = make_input(vec![]);
        let error = tool
            .prepare_invocation(&input)
            .await
            .expect_err("empty command should be rejected");
        assert!(error.to_string().contains("Command cannot be empty"));
    }

    #[tokio::test]
    async fn prepare_invocation_rejects_empty_executable() {
        let tool = make_tool();
        let input = make_input(vec!["", "arg1"]);
        let error = tool
            .prepare_invocation(&input)
            .await
            .expect_err("empty executable should be rejected");
        assert!(error.to_string().contains("Command executable cannot be empty"));
    }

    #[tokio::test]
    async fn prepare_invocation_rejects_whitespace_only_executable() {
        let tool = make_tool();
        let input = make_input(vec!["   ", "arg1"]);
        let error = tool
            .prepare_invocation(&input)
            .await
            .expect_err("whitespace-only executable should be rejected");
        assert!(error.to_string().contains("Command executable cannot be empty"));
    }

    #[tokio::test]
    async fn validate_args_rejects_empty_command() {
        let tool = make_tool();
        let args = make_input(vec![]);
        let error = tool
            .validate_args(&args)
            .await
            .expect_err("empty command should fail validation");
        assert!(error.to_string().contains("Command cannot be empty"));
    }

    #[tokio::test]
    async fn validate_args_rejects_empty_executable() {
        let tool = make_tool();
        let args = make_input(vec!["", "arg1"]);
        let error = tool
            .validate_args(&args)
            .await
            .expect_err("empty executable should fail validation");
        assert!(error.to_string().contains("Command executable cannot be empty"));
    }

    #[tokio::test]
    async fn validate_args_accepts_valid_command() {
        let tool = make_tool();
        let args = make_input(vec!["ls", "-la"]);
        tool.validate_args(&args).await.expect("valid command should pass validation");
    }

    #[test]
    fn environment_variables_are_inherited_from_parent() {
        // Verify that the environment setup includes inherited parent process variables.
        // This test documents the fix for the cargo fmt issue where PATH and other
        // critical environment variables were not being passed to subprocesses.
        // See: crates/codegen/vtcode-core/src/tools/command.rs:execute_terminal_command()

        // The fix uses std::env::vars_os().collect() which inherits all parent variables
        let env: HashMap<OsString, OsString> = std::env::vars_os().collect();

        // Verify critical system variables are present
        assert!(
            env.contains_key(&OsString::from("PATH")),
            "PATH environment variable must be inherited for command resolution"
        );
    }
}
