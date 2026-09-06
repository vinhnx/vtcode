//! Executor trait boundary and pure eval orchestration.
//!
//! The [`EvalExecutor`] trait isolates the harness orchestration from the
//! concrete agent runner. This is the "interface guard rail": `run_suite`
//! depends only on the trait, so it can be unit-tested with a fake executor
//! and the production executor implementation can be swapped
//! without touching the orchestration logic.

use anyhow::Result;
use async_trait::async_trait;
use futures::{StreamExt, stream};

use crate::task::EvalTask;
use crate::{
    EvalReport, EvalRunResult, EvalSuite, SuiteReport, aggregate_metrics, build_task_report, compute_metric_with_k,
};

/// Controls bounded eval scheduling and the sampling value used by metrics.
#[derive(Debug, Clone, Copy)]
pub struct EvalRunOptions {
    /// Maximum number of attempts executing concurrently.
    pub concurrency: usize,
    /// Sampling value for pass@k/pass^k. Defaults to the suite attempt count.
    pub metric_k: Option<u32>,
}

impl Default for EvalRunOptions {
    fn default() -> Self {
        Self { concurrency: 2, metric_k: None }
    }
}

/// Executes a single eval task attempt and returns the verified outcome.
///
/// Implementations own the full "execute this task" semantics — running the
/// agent and applying any environment verification (probes). Keeping this
/// behind a trait decouples the orchestration in [`run_suite`] from the
/// concrete runner, which makes the harness independently testable.
#[async_trait]
pub trait EvalExecutor: Send + Sync {
    /// Run one attempt of `task` and return the outcome.
    async fn execute_task(&self, task: &EvalTask) -> Result<EvalRunResult>;

    /// Run a numbered attempt. Existing executors remain source-compatible;
    /// production executors override this to isolate each attempt.
    async fn execute_task_attempt(&self, task: &EvalTask, attempt: u32) -> Result<EvalRunResult> {
        let mut result = self.execute_task(task).await?;
        result.attempt = attempt;
        Ok(result)
    }
}

/// Pure orchestration core: loop tasks × attempts through the executor,
/// compute per-task metrics, and assemble the report.
///
/// This function performs no file I/O, no config reads, and no trust checks —
/// those belong to the caller. It depends only on [`EvalExecutor`], which makes
/// it fully unit-testable with an in-memory fake (see `executor::tests`).
pub async fn run_suite(executor: &dyn EvalExecutor, suite: &EvalSuite) -> Result<EvalReport> {
    run_suite_with_options(executor, suite, EvalRunOptions::default()).await
}

/// Execute a suite with bounded concurrency and deterministic report order.
pub async fn run_suite_with_options(
    executor: &dyn EvalExecutor,
    suite: &EvalSuite,
    options: EvalRunOptions,
) -> Result<EvalReport> {
    anyhow::ensure!(suite.attempts >= 1, "evaluation suite attempts must be at least one");
    let metric_k = options.metric_k.unwrap_or(suite.attempts);
    anyhow::ensure!(metric_k >= 1 && metric_k <= suite.attempts, "evaluation metric k must be between 1 and attempts");
    let concurrency = options.concurrency.max(1);

    let jobs =
        suite.tasks.iter().enumerate().flat_map(|(task_index, task)| {
            (1..=suite.attempts).map(move |attempt| (task_index, task.clone(), attempt))
        });
    // Drain every in-flight attempt before propagating an error. A production
    // executor may own an isolated worktree or another external resource that
    // is cleaned up after its future completes; `try_collect` would drop the
    // remaining futures on the first error and strand those resources.
    let mut results = stream::iter(jobs)
        .map(|(task_index, task, attempt)| async move {
            let result = executor.execute_task_attempt(&task, attempt).await?;
            Ok::<_, anyhow::Error>((task_index, attempt, result))
        })
        .buffer_unordered(concurrency)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
    for (task_index, expected_attempt, result) in &results {
        let expected_task = suite
            .tasks
            .get(*task_index)
            .ok_or_else(|| anyhow::anyhow!("executor returned an unknown task index {task_index}"))?;
        anyhow::ensure!(
            result.task_id == expected_task.id,
            "executor returned task id '{}' for task '{}' attempt {}",
            result.task_id,
            expected_task.id,
            expected_attempt
        );
        anyhow::ensure!(
            result.attempt == *expected_attempt,
            "executor returned attempt {} for task '{}' attempt {}",
            result.attempt,
            expected_task.id,
            expected_attempt
        );
    }
    results.sort_by_key(|(task_index, attempt, _)| (*task_index, *attempt));

    let mut all_task_reports = Vec::new();
    let mut all_cost_usd = 0.0;
    let mut known_cost_runs = 0u32;
    let mut unpriced_runs = 0u32;
    let mut duration_secs = 0.0;
    let mut trace_summary: Option<crate::trace_analyzer::HarnessTraceSummary> = None;

    for (task_index, task) in suite.tasks.iter().enumerate() {
        let run_results: Vec<EvalRunResult> = results
            .iter()
            .filter(|(index, _, _)| *index == task_index)
            .map(|(_, _, result)| result.clone())
            .collect();
        for result in &run_results {
            duration_secs += result.duration_secs;
            if let Some(cost) = result.cost_usd.filter(|value| value.is_finite() && *value >= 0.0) {
                all_cost_usd += cost;
                known_cost_runs = known_cost_runs.saturating_add(1);
            } else {
                unpriced_runs = unpriced_runs.saturating_add(1);
            }
            if let Some(summary) = &result.trace_summary {
                trace_summary.get_or_insert_with(Default::default).merge(summary);
            }
        }
        let metric = compute_metric_with_k(&task.id, &run_results, metric_k)?;
        all_task_reports.push(build_task_report(&task.id, &task.name, task.category, metric));
    }

    let all_metrics: Vec<_> = all_task_reports.iter().map(|r| r.metric.clone()).collect();
    let cap_metrics: Vec<_> = all_task_reports
        .iter()
        .filter(|r| r.category == "Capability")
        .map(|r| r.metric.clone())
        .collect();
    let reg_metrics: Vec<_> = all_task_reports
        .iter()
        .filter(|r| r.category == "Regression")
        .map(|r| r.metric.clone())
        .collect();

    Ok(EvalReport {
        generated_at: chrono::Utc::now().to_rfc3339(),
        suites: vec![SuiteReport {
            suite_id: suite.id.clone(),
            suite_name: suite.name.clone(),
            task_reports: all_task_reports,
            aggregate: aggregate_metrics(&all_metrics),
            capability_metrics: aggregate_metrics(&cap_metrics),
            regression_metrics: aggregate_metrics(&reg_metrics),
            cost_usd: (known_cost_runs > 0).then_some(all_cost_usd),
            unpriced_runs,
            duration_secs,
            trace_summary,
        }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{EvalCategory, EvalRunResult, EvalTask, RunOutcome};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::time::{Duration, sleep};

    /// In-memory fake executor for isolating `run_suite`.
    struct FakeExecutor {
        outcomes: Vec<RunOutcome>,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl EvalExecutor for FakeExecutor {
        async fn execute_task(&self, _task: &EvalTask) -> Result<EvalRunResult> {
            let i = self.calls.fetch_add(1, Ordering::SeqCst);
            let outcome = self.outcomes[i % self.outcomes.len()];
            Ok(EvalRunResult {
                task_id: _task.id.clone(),
                outcome,
                error_message: None,
                duration_secs: 0.0,
                attempt: (i + 1) as u32,
                cost_usd: None,
                transcript_path: None,
                trace_summary: None,
            })
        }
    }

    fn suite(attempts: u32) -> EvalSuite {
        EvalSuite {
            id: "s1".into(),
            name: "demo".into(),
            tasks: vec![
                EvalTask {
                    id: "t1".into(),
                    name: "t1".into(),
                    category: EvalCategory::Capability,
                    prompt: "p".into(),
                    verify_commands: vec![],
                    timeout_secs: None,
                },
                EvalTask {
                    id: "t2".into(),
                    name: "t2".into(),
                    category: EvalCategory::Regression,
                    prompt: "p".into(),
                    verify_commands: vec![],
                    timeout_secs: None,
                },
            ],
            attempts,
        }
    }

    #[tokio::test]
    async fn run_suite_aggregates_capability_and_regression() {
        // t1 gets [Pass, Fail]; t2 gets [Pass, Pass]
        let exec = FakeExecutor {
            outcomes: vec![RunOutcome::Pass, RunOutcome::Fail, RunOutcome::Pass, RunOutcome::Pass],
            calls: AtomicUsize::new(0),
        };
        let report = run_suite(&exec, &suite(2)).await.unwrap();
        let s = &report.suites[0];
        // With k=attempts, t1's two-sample pass@2 is 1.0 and t2 is 1.0.
        assert_eq!(s.aggregate.passed_runs, 3);
        assert_eq!(s.aggregate.total_runs, 4);
        assert!((s.capability_metrics.pass_at_k - 1.0).abs() < 1e-9);
        assert!((s.regression_metrics.pass_at_k - 1.0).abs() < 1e-9);
        assert!(s.capability_metrics.pass_all_k < 1.0);
        assert_eq!(s.regression_metrics.pass_all_k, 1.0);
    }

    struct BoundedExecutor {
        active: AtomicUsize,
        max_active: AtomicUsize,
    }

    #[async_trait]
    impl EvalExecutor for BoundedExecutor {
        async fn execute_task(&self, task: &EvalTask) -> Result<EvalRunResult> {
            self.execute_task_attempt(task, 1).await
        }

        async fn execute_task_attempt(&self, task: &EvalTask, attempt: u32) -> Result<EvalRunResult> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            sleep(Duration::from_millis(5)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(EvalRunResult {
                task_id: task.id.clone(),
                outcome: RunOutcome::Pass,
                error_message: None,
                duration_secs: 0.005,
                attempt,
                cost_usd: Some(0.0),
                transcript_path: None,
                trace_summary: None,
            })
        }
    }

    #[tokio::test]
    async fn run_suite_bounds_attempts_and_keeps_report_order() {
        let executor = Arc::new(BoundedExecutor {
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
        });
        let report =
            run_suite_with_options(executor.as_ref(), &suite(4), EvalRunOptions { concurrency: 2, metric_k: Some(2) })
                .await
                .expect("bounded suite should complete");

        assert!(executor.max_active.load(Ordering::SeqCst) <= 2);
        let task_reports = &report.suites[0].task_reports;
        assert_eq!(
            task_reports
                .iter()
                .map(|report| report.metric.task_id.as_str())
                .collect::<Vec<_>>(),
            ["t1", "t2"]
        );
        assert_eq!(report.suites[0].unpriced_runs, 0);
        assert_eq!(report.suites[0].cost_usd, Some(0.0));
    }

    struct IdentityViolatingExecutor;

    #[async_trait]
    impl EvalExecutor for IdentityViolatingExecutor {
        async fn execute_task(&self, task: &EvalTask) -> Result<EvalRunResult> {
            Ok(EvalRunResult {
                task_id: task.id.clone(),
                outcome: RunOutcome::Pass,
                error_message: None,
                duration_secs: 0.0,
                attempt: 1,
                cost_usd: None,
                transcript_path: None,
                trace_summary: None,
            })
        }

        async fn execute_task_attempt(&self, task: &EvalTask, _attempt: u32) -> Result<EvalRunResult> {
            let mut result = self.execute_task(task).await?;
            result.task_id = "wrong-task".into();
            result.attempt = 99;
            Ok(result)
        }
    }

    #[tokio::test]
    async fn run_suite_rejects_executor_identity_mismatch() {
        let error = run_suite(&IdentityViolatingExecutor, &suite(1))
            .await
            .expect_err("executor identity mismatch must fail the suite");
        assert!(error.to_string().contains("executor returned task id"));
    }
}
