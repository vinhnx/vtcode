//! Eval suite runner for `vtcode exec eval`.
//!
//! The orchestration core lives in `vtcode_eval::run_suite`, which depends
//! only on the [`vtcode_eval::EvalExecutor`] trait. This file wires the
//! production executor ([`AgentRunnerExecutor`]) to that trait, applies the
//! trust/automation guardrails, and handles I/O.

use crate::startup::require_full_auto_workspace_trust;
use anyhow::{Context, Result, bail};
use std::path::Path;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use vtcode_core::cli::input_hardening::validate_agent_safe_text;
use vtcode_core::config::VTCodeConfig;
use vtcode_core::config::models::ModelId;
use vtcode_core::config::types::AgentConfig as CoreAgentConfig;
use vtcode_core::core::agent::runner::{AgentRunner, RunnerSettings};
use vtcode_core::core::agent::task::{ContextItem, Task, TaskOutcome};
use vtcode_core::core::agent::types::AgentType;
use vtcode_core::core::threads::ThreadBootstrap;
use vtcode_core::exec::events::ThreadEvent;
use vtcode_eval::{
    EvalRunOptions, EvalRunResult, EvalSuite, EvalTask, HarnessTraceSummary, RunOutcome, analyze_jsonl,
    environment::{CommandProbe, EnvironmentProbe},
    run_suite_with_options,
};

use super::ExecCommandKind;
use super::run::task_spec;

/// Handle the `vtcode exec eval --suite <path>` command.
pub(crate) async fn handle_eval_command(
    config: &CoreAgentConfig,
    vt_cfg: &VTCodeConfig,
    suite_path: &Path,
    output_path: Option<&Path>,
) -> Result<()> {
    let suite_json = tokio::fs::read_to_string(suite_path)
        .await
        .with_context(|| format!("read eval suite from {}", suite_path.display()))?;
    let suite: EvalSuite = serde_json::from_str(&suite_json)
        .with_context(|| format!("parse eval suite JSON from {}", suite_path.display()))?;

    if suite.attempts < 1 {
        bail!("eval suite requires attempts >= 1");
    }

    eprintln!("Running eval suite: {} ({} tasks, {} attempts each)", suite.name, suite.tasks.len(), suite.attempts);

    // H1: require full-auto workspace trust
    require_full_auto_workspace_trust(config.workspace.as_path(), "eval runs", "eval").await?;

    // H2: require full-auto enabled
    if !vt_cfg.automation.full_auto.enabled {
        bail!("Automation is disabled in configuration. Enable [automation.full_auto] to run eval.");
    }

    let allowed_tools = vt_cfg.automation.full_auto.allowed_tools.clone();
    let executor = AgentRunnerExecutor::new(config, vt_cfg, &allowed_tools);

    let report = run_suite_with_options(&executor, &suite, EvalRunOptions::default()).await?;

    let markdown = report.to_markdown();
    if let Some(path) = output_path {
        tokio::fs::write(path, &markdown)
            .await
            .with_context(|| format!("write report to {}", path.display()))?;
        eprintln!("\nReport written to {}", path.display());
    } else {
        println!("{markdown}");
    }
    Ok(())
}

/// Production executor: runs each task through the agent runner and applies
/// environment probes to verify the claimed outcome.
struct AgentRunnerExecutor {
    config: CoreAgentConfig,
    vt_cfg: VTCodeConfig,
    allowed_tools: Vec<String>,
    workspace_root: PathBuf,
}

impl AgentRunnerExecutor {
    fn new(config: &CoreAgentConfig, vt_cfg: &VTCodeConfig, allowed_tools: &[String]) -> Self {
        Self {
            config: config.clone(),
            vt_cfg: vt_cfg.clone(),
            allowed_tools: allowed_tools.to_vec(),
            workspace_root: config.workspace.clone(),
        }
    }
}

#[async_trait::async_trait]
impl vtcode_eval::EvalExecutor for AgentRunnerExecutor {
    async fn execute_task(&self, eval_task: &EvalTask) -> Result<EvalRunResult> {
        Ok(run_eval_task(
            &self.config,
            &self.vt_cfg,
            eval_task,
            &self.allowed_tools,
            &self.config.workspace,
            &self.workspace_root,
            1,
        )
        .await)
    }

    async fn execute_task_attempt(&self, eval_task: &EvalTask, attempt: u32) -> Result<EvalRunResult> {
        let workspace_root = self.workspace_root.clone();
        let worktree_name = format!("eval-{}-{}-{}", uuid::Uuid::new_v4().simple(), eval_task.id, attempt);
        let worktree_path = tokio::task::spawn_blocking(move || {
            vtcode_core::git::WorktreeManager::new(workspace_root).create_from_current(&worktree_name)
        })
        .await
        .context("eval worktree creation task failed")??;
        let mut config = self.config.clone();
        config.workspace = worktree_path.clone();
        let mut result = run_eval_task(
            &config,
            &self.vt_cfg,
            eval_task,
            &self.allowed_tools,
            &worktree_path,
            &self.workspace_root,
            attempt,
        )
        .await;
        result.attempt = attempt;
        let cleanup_name = worktree_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned();
        let root = self.workspace_root.clone();
        match tokio::task::spawn_blocking(move || vtcode_core::git::WorktreeManager::new(root).remove(&cleanup_name))
            .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                result.outcome = RunOutcome::Error;
                result.error_message = Some(format!("eval worktree cleanup failed: {error:#}"));
            }
            Err(error) => {
                result.outcome = RunOutcome::Error;
                result.error_message = Some(format!("eval worktree cleanup task failed: {error}"));
            }
        }
        Ok(result)
    }
}

/// Run a single eval task attempt through the agent runner + probes.
async fn run_eval_task(
    config: &CoreAgentConfig,
    vt_cfg: &VTCodeConfig,
    eval_task: &EvalTask,
    allowed_tools: &[String],
    workspace: &Path,
    trace_root: &Path,
    attempt: u32,
) -> EvalRunResult {
    let start = Instant::now();
    let session_id = format!("eval-{}-attempt-{attempt}", eval_task.id);

    if let Err(e) = validate_agent_safe_text("eval_task.prompt", &eval_task.prompt) {
        return eval_error(&eval_task.id, start, attempt, format!("Prompt validation failed: {e}"));
    }

    let model_id = match ModelId::from_config(
        &config.model,
        &config.provider,
        &vt_cfg.provider_overrides,
        &vt_cfg.custom_providers,
    ) {
        Ok(id) => id,
        Err(e) => return eval_error(&eval_task.id, start, attempt, format!("Model not recognized: {e}")),
    };

    let runner_result = AgentRunner::new_with_bootstrap(
        AgentType::Single,
        model_id,
        config.api_key.clone(),
        workspace.to_path_buf(),
        session_id.clone(),
        RunnerSettings {
            reasoning_effort: Some(config.reasoning_effort),
            verbosity: None,
        },
        None,
        ThreadBootstrap::new(None),
        Some(vt_cfg.clone()),
        config.openai_chatgpt_auth.clone(),
    )
    .await;

    let mut runner = match runner_result {
        Ok(r) => r,
        Err(e) => return eval_error(&eval_task.id, start, attempt, format!("Runner creation failed: {e}")),
    };

    runner.enable_full_auto(allowed_tools).await;
    runner.set_quiet(true);

    let ts = task_spec(&ExecCommandKind::Eval { suite_path: PathBuf::new(), output_path: None }, false);
    let task = Task {
        id: eval_task.id.clone(),
        title: eval_task.name.clone(),
        description: eval_task.prompt.clone(),
        instructions: Some(ts.instructions.to_string()),
    };

    let exec_result = if let Some(timeout_secs) = eval_task.timeout_secs {
        match tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            runner.execute_task_with_retry(&task, &[] as &[ContextItem], 1),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(vtcode_core::error::VtCodeError::execution(
                vtcode_core::error::ErrorCode::Timeout,
                format!("evaluation task exceeded timeout of {timeout_secs}s"),
            )),
        }
    } else {
        runner.execute_task_with_retry(&task, &[] as &[ContextItem], 1).await
    };
    let duration_secs = start.elapsed().as_secs_f64();
    let trace_events = exec_result
        .as_ref()
        .ok()
        .map(|result| result.thread_events.as_slice())
        .unwrap_or(&[]);
    let (transcript_path, trace_summary) =
        match persist_eval_trace(trace_root, &eval_task.id, attempt, trace_events).await {
            Ok((path, summary)) => (Some(path), Some(summary)),
            Err(error) => {
                tracing::warn!(task_id = %eval_task.id, attempt, error = %error, "failed to persist eval trace");
                (None, None)
            }
        };
    let cost_usd = exec_result
        .as_ref()
        .ok()
        .and_then(|result| result.total_cost_usd)
        .filter(|cost| cost.is_finite() && *cost >= 0.0);

    let exec_outcome = match &exec_result {
        Ok(result) if matches!(result.outcome, TaskOutcome::Success | TaskOutcome::StoppedNoAction) => RunOutcome::Pass,
        Ok(_) => RunOutcome::Fail,
        Err(_) => RunOutcome::Error,
    };

    if exec_outcome == RunOutcome::Pass {
        let probes = build_probes(eval_task);
        if !probes.is_empty() && !probes.iter().all(|p| p.check(workspace)) {
            return EvalRunResult {
                task_id: eval_task.id.clone(),
                outcome: RunOutcome::Fail,
                transcript_path,
                cost_usd,
                duration_secs,
                attempt,
                error_message: Some("Environment probes failed after agent claimed success".into()),
                trace_summary,
            };
        }
    }

    EvalRunResult {
        task_id: eval_task.id.clone(),
        outcome: exec_outcome,
        transcript_path,
        cost_usd,
        duration_secs,
        attempt,
        error_message: None,
        trace_summary,
    }
}

/// M1: DRY helper for error EvalRunResult construction.
fn eval_error(task_id: &str, start: Instant, attempt: u32, message: String) -> EvalRunResult {
    EvalRunResult {
        task_id: task_id.into(),
        outcome: RunOutcome::Error,
        transcript_path: None,
        cost_usd: None,
        duration_secs: start.elapsed().as_secs_f64(),
        attempt,
        error_message: Some(message),
        trace_summary: None,
    }
}

async fn persist_eval_trace(
    trace_root: &Path,
    task_id: &str,
    attempt: u32,
    events: &[ThreadEvent],
) -> Result<(String, HarnessTraceSummary)> {
    let trusted_root = vtcode_commons::canonicalize(trace_root)
        .with_context(|| format!("resolve eval trace root {}", trace_root.display()))?;

    let task_component = task_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    let task_component = if task_component.is_empty() {
        "task"
    } else {
        &task_component
    };
    let file_name = format!("{task_component}-attempt-{attempt}-{}.jsonl", uuid::Uuid::new_v4().simple());
    let mut jsonl = String::new();
    for event in events {
        let line = serde_json::to_string(event).context("serialize eval trace event")?;
        jsonl.push_str(&line);
        jsonl.push('\n');
    }
    let relative_path = Path::new(".vtcode").join("eval").join("traces").join(&file_name);
    let write_root = trusted_root.clone();
    let write_path = relative_path.clone();
    let contents = jsonl.as_bytes().to_vec();
    tokio::task::spawn_blocking(move || {
        vtcode_commons::fs::bound_file::write_file_beneath(&write_root, &write_path, &contents)
    })
    .await
    .context("eval trace write task panicked")?
    .with_context(|| format!("write eval trace {}", trusted_root.join(&relative_path).display()))?;
    let path = trusted_root.join(&relative_path);
    let summary = analyze_jsonl(&jsonl).context("analyze persisted eval trace")?;
    Ok((path.to_string_lossy().into_owned(), summary))
}

/// Build environment probes from an eval task's verify commands.
fn build_probes(eval_task: &EvalTask) -> Vec<Box<dyn EnvironmentProbe>> {
    eval_task
        .verify_commands
        .iter()
        .filter_map(|cmd| {
            let parts: Vec<&str> = cmd.split_whitespace().collect();
            if parts.is_empty() {
                return None;
            }
            let command = parts[0].to_string();
            let args: Vec<String> = parts[1..].iter().copied().map(str::to_string).collect();
            Some(Box::new(CommandProbe::new(command, args)) as Box<dyn EnvironmentProbe>)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vtcode_eval::EvalTask;

    #[test]
    fn build_probes_parses_command_and_args() {
        let task = EvalTask {
            id: "t".into(),
            name: "t".into(),
            category: vtcode_eval::EvalCategory::Capability,
            prompt: "p".into(),
            verify_commands: vec!["cargo test --all".into(), "".into(), "git status".into()],
            timeout_secs: None,
        };
        let probes = build_probes(&task);
        // Empty command is filtered out -> 2 probes.
        assert_eq!(probes.len(), 2);
    }

    #[test]
    fn build_probes_empty_when_no_commands() {
        let task = EvalTask {
            id: "t".into(),
            name: "t".into(),
            category: vtcode_eval::EvalCategory::Capability,
            prompt: "p".into(),
            verify_commands: vec![],
            timeout_secs: None,
        };
        assert!(build_probes(&task).is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn persist_eval_trace_rejects_symlinked_trace_components() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let outside = tempfile::tempdir().expect("outside tempdir");
        std::fs::create_dir_all(workspace.path().join(".vtcode")).expect("create metadata directory");
        symlink(outside.path(), workspace.path().join(".vtcode").join("eval")).expect("create eval symlink");

        let result = persist_eval_trace(workspace.path(), "task", 1, &[]).await;
        assert!(result.is_err(), "trace persistence must reject symlinked eval directories");
        assert!(
            std::fs::read_dir(outside.path())
                .expect("read outside directory")
                .next()
                .is_none()
        );

        std::fs::remove_file(workspace.path().join(".vtcode").join("eval")).expect("remove eval symlink");
        std::fs::remove_dir(workspace.path().join(".vtcode")).expect("remove metadata directory");
        symlink(outside.path(), workspace.path().join(".vtcode")).expect("create metadata symlink");
        let result = persist_eval_trace(workspace.path(), "task", 1, &[]).await;
        assert!(result.is_err(), "trace persistence must reject symlinked metadata directories");
        assert!(
            std::fs::read_dir(outside.path())
                .expect("read outside directory")
                .next()
                .is_none()
        );
    }
}
