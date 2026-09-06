use super::task::{EvalRunResult, RunOutcome};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalMetric {
    pub(crate) pass_at_k: f64,
    pub(crate) pass_power_k: f64,
    pub(crate) pass_all_k: f64,
    pub(crate) k: u32,
    pub(crate) total_runs: u32,
    pub(crate) passed_runs: u32,
    pub(crate) task_id: String,
}

pub fn compute_metric(task_id: &str, results: &[EvalRunResult]) -> EvalMetric {
    compute_metric_with_k(task_id, results, 1).unwrap_or_else(|_| empty_metric(task_id))
}

/// Compute the combinatorial pass@k and independent reliability pass^k metrics.
pub fn compute_metric_with_k(task_id: &str, results: &[EvalRunResult], k: u32) -> anyhow::Result<EvalMetric> {
    let total = results.len() as u32;
    anyhow::ensure!(total > 0, "cannot compute evaluation metrics without attempts");
    anyhow::ensure!(k > 0 && k <= total, "metric k must be between 1 and the number of attempts");
    let passed = results.iter().filter(|r| r.outcome == RunOutcome::Pass).count() as u32;
    Ok(EvalMetric {
        pass_at_k: pass_at_k_counts(total, passed, k),
        pass_power_k: (passed as f64 / total as f64).powi(k as i32),
        pass_all_k: if passed == total { 1.0 } else { 0.0 },
        k,
        total_runs: total,
        passed_runs: passed,
        task_id: task_id.into(),
    })
}

fn empty_metric(task_id: &str) -> EvalMetric {
    EvalMetric {
        pass_at_k: 0.0,
        pass_power_k: 0.0,
        pass_all_k: 0.0,
        k: 0,
        total_runs: 0,
        passed_runs: 0,
        task_id: task_id.into(),
    }
}

pub fn aggregate_metrics(metrics: &[EvalMetric]) -> EvalMetric {
    if metrics.is_empty() {
        return EvalMetric {
            pass_at_k: 0.0,
            pass_power_k: 0.0,
            pass_all_k: 0.0,
            k: 0,
            total_runs: 0,
            passed_runs: 0,
            task_id: "aggregate".into(),
        };
    }
    let total_runs: u32 = metrics.iter().map(|m| m.total_runs).sum();
    let passed_runs: u32 = metrics.iter().map(|m| m.passed_runs).sum();
    let pass_at_k = metrics.iter().map(|metric| metric.pass_at_k).sum::<f64>() / metrics.len() as f64;
    let pass_power_k = metrics.iter().map(|metric| metric.pass_power_k).sum::<f64>() / metrics.len() as f64;
    let pass_all_k = if metrics.iter().all(|m| m.pass_all_k > 0.0) {
        1.0
    } else {
        0.0
    };
    EvalMetric {
        pass_at_k,
        pass_power_k,
        pass_all_k,
        k: metrics.iter().map(|metric| metric.k).max().unwrap_or(0),
        total_runs,
        passed_runs,
        task_id: "aggregate".into(),
    }
}

pub fn pass_at_k(results: &[EvalRunResult]) -> f64 {
    pass_at_k_with_k(results, 1).unwrap_or(0.0)
}

/// Compute combinatorial pass@k from sampled attempts.
pub fn pass_at_k_with_k(results: &[EvalRunResult], k: u32) -> anyhow::Result<f64> {
    let total = u32::try_from(results.len()).map_err(|_| anyhow::anyhow!("too many evaluation attempts"))?;
    let passed = u32::try_from(results.iter().filter(|result| result.outcome == RunOutcome::Pass).count())
        .map_err(|_| anyhow::anyhow!("too many passed attempts"))?;
    anyhow::ensure!(total > 0 && k > 0 && k <= total, "pass@k requires 1 <= k <= attempts");
    Ok(pass_at_k_counts(total, passed, k))
}

/// Compute independent reliability pass^k as `(passed / attempts)^k`.
pub fn pass_power_k(results: &[EvalRunResult], k: u32) -> anyhow::Result<f64> {
    let total = results.len() as u32;
    anyhow::ensure!(total > 0 && k > 0, "pass^k requires at least one attempt and k");
    let passed = results.iter().filter(|result| result.outcome == RunOutcome::Pass).count() as f64;
    Ok((passed / total as f64).powi(k as i32))
}

fn pass_at_k_counts(total: u32, passed: u32, k: u32) -> f64 {
    1.0 - combinations(total.saturating_sub(passed), k) / combinations(total, k)
}

fn combinations(n: u32, k: u32) -> f64 {
    if k > n {
        return 0.0;
    }
    let k = k.min(n - k);
    let mut result = 1.0;
    for i in 1..=k {
        result *= f64::from(n - k + i) / f64::from(i);
    }
    result
}

pub fn pass_all_k(results: &[EvalRunResult]) -> f64 {
    if results.is_empty() {
        return 0.0;
    }
    if results.iter().all(|r| r.outcome == RunOutcome::Pass) {
        1.0
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{EvalRunResult, RunOutcome};

    fn r(outcome: RunOutcome) -> EvalRunResult {
        EvalRunResult {
            task_id: "t".into(),
            outcome,
            error_message: None,
            duration_secs: 0.0,
            attempt: 1,
            cost_usd: None,
            transcript_path: None,
            trace_summary: None,
        }
    }

    #[test]
    fn compute_metric_pass_at_k() {
        let results = vec![r(RunOutcome::Pass), r(RunOutcome::Fail), r(RunOutcome::Error)];
        let m = compute_metric("t", &results);
        assert_eq!(m.total_runs, 3);
        assert_eq!(m.passed_runs, 1);
        assert!((m.pass_at_k - 1.0 / 3.0).abs() < 1e-9);
        assert_eq!(m.pass_all_k, 0.0);
    }

    #[test]
    fn compute_metric_all_pass() {
        let results = vec![r(RunOutcome::Pass), r(RunOutcome::Pass)];
        let m = compute_metric("t", &results);
        assert_eq!(m.pass_all_k, 1.0);
        assert!((m.pass_at_k - 1.0).abs() < 1e-9);
    }

    #[test]
    fn compute_metric_empty() {
        let m = compute_metric("t", &[]);
        assert_eq!(m.total_runs, 0);
        assert_eq!(m.pass_at_k, 0.0);
    }

    #[test]
    fn aggregate_combines_runs() {
        let a = EvalMetric {
            pass_at_k: 1.0,
            pass_power_k: 1.0,
            pass_all_k: 1.0,
            k: 1,
            total_runs: 1,
            passed_runs: 1,
            task_id: "a".into(),
        };
        let b = EvalMetric {
            pass_at_k: 0.0,
            pass_power_k: 0.0,
            pass_all_k: 0.0,
            k: 1,
            total_runs: 1,
            passed_runs: 0,
            task_id: "b".into(),
        };
        let agg = aggregate_metrics(&[a, b]);
        assert_eq!(agg.total_runs, 2);
        assert_eq!(agg.passed_runs, 1);
        assert!((agg.pass_at_k - 0.5).abs() < 1e-9);
        assert_eq!(agg.pass_all_k, 0.0);
    }

    #[test]
    fn aggregate_empty() {
        let agg = aggregate_metrics(&[]);
        assert_eq!(agg.total_runs, 0);
        assert_eq!(agg.pass_at_k, 0.0);
    }

    #[test]
    fn combinatorial_pass_at_k_and_independent_pass_power_are_distinct() {
        let results = vec![
            r(RunOutcome::Pass),
            r(RunOutcome::Fail),
            r(RunOutcome::Pass),
            r(RunOutcome::Fail),
        ];
        let metric = compute_metric_with_k("t", &results, 2).expect("metric");
        assert!((metric.pass_at_k - (5.0 / 6.0)).abs() < 1e-9);
        assert!((metric.pass_power_k - 0.25).abs() < 1e-9);
        assert!((pass_at_k_with_k(&results, 2).expect("pass@k") - 5.0 / 6.0).abs() < 1e-9);
    }

    #[test]
    fn metric_rejects_zero_attempts_and_invalid_k() {
        assert!(compute_metric_with_k("t", &[], 1).is_err());
        let results = vec![r(RunOutcome::Pass)];
        assert!(compute_metric_with_k("t", &results, 0).is_err());
        assert!(pass_at_k_with_k(&results, 2).is_err());
    }
}
