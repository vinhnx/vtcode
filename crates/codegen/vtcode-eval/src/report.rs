use crate::trace_analyzer::HarnessTraceSummary;
use crate::{EvalMetric, task::EvalCategory};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct TaskReport {
    task_id: String,
    pub(crate) category: String,
    pub(crate) metric: EvalMetric,
}

#[derive(Debug, Clone, Serialize)]
pub struct SuiteReport {
    pub(crate) suite_id: String,
    pub(crate) suite_name: String,
    pub(crate) task_reports: Vec<TaskReport>,
    pub(crate) aggregate: EvalMetric,
    pub(crate) capability_metrics: EvalMetric,
    pub(crate) regression_metrics: EvalMetric,
    /// Sum of known per-attempt costs. Unknown-pricing attempts are counted
    /// separately instead of being treated as zero-cost successes.
    pub(crate) cost_usd: Option<f64>,
    pub(crate) unpriced_runs: u32,
    pub(crate) duration_secs: f64,
    /// Aggregate privacy-preserving trace facts joined by task and attempt.
    pub(crate) trace_summary: Option<HarnessTraceSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvalReport {
    pub(crate) generated_at: String,
    pub(crate) suites: Vec<SuiteReport>,
}

impl EvalReport {
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# Eval Report\n\n");
        for s in &self.suites {
            out.push_str(&format!("## {}\n\n", s.suite_name));
            out.push_str(&format!(
                "- Aggregate: pass@k={:.1}%; pass^k={:.1}%\n",
                s.aggregate.pass_at_k * 100.0,
                s.aggregate.pass_power_k * 100.0
            ));
            if let Some(cost) = s.cost_usd {
                out.push_str(&format!("- Cost (known): ${cost:.6}; unpriced attempts: {}\n", s.unpriced_runs));
            } else if s.unpriced_runs > 0 {
                out.push_str(&format!("- Cost (known): unavailable; unpriced attempts: {}\n", s.unpriced_runs));
            }
            if let Some(trace) = &s.trace_summary {
                if let Some(mean_ms) = trace.latency.mean_ms {
                    out.push_str(&format!(
                        "- Trace: {} turns, {} tool calls, mean latency {:.1} ms\n",
                        trace.turns, trace.tool_calls, mean_ms
                    ));
                } else {
                    out.push_str(&format!("- Trace: {} turns, {} tool calls\n", trace.turns, trace.tool_calls));
                }
            }
            out.push_str("| Task | Category | pass@k | pass^k | passed/total |\n");
            out.push_str("|------|----------|--------|--------|-------------|\n");
            for t in &s.task_reports {
                out.push_str(&format!(
                    "| {} | {} | {:.1}% | {:.1}% | {}/{} |\n",
                    t.task_id,
                    t.category.as_str(),
                    t.metric.pass_at_k * 100.0,
                    t.metric.pass_power_k * 100.0,
                    t.metric.passed_runs,
                    t.metric.total_runs
                ));
            }
            out.push('\n');
        }
        out
    }
}

pub fn build_task_report(task_id: &str, _name: &str, category: EvalCategory, metric: EvalMetric) -> TaskReport {
    TaskReport {
        task_id: task_id.into(),
        category: category.label().into(),
        metric,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metric::EvalMetric;
    use crate::task::EvalCategory;

    #[test]
    fn to_markdown_renders_tasks_and_aggregate() {
        let report = EvalReport {
            generated_at: "2026-01-01".into(),
            suites: vec![SuiteReport {
                suite_id: "s1".into(),
                suite_name: "demo".into(),
                task_reports: vec![TaskReport {
                    task_id: "t1".into(),
                    category: "Capability".into(),
                    metric: EvalMetric {
                        pass_at_k: 0.5,
                        pass_power_k: 0.25,
                        pass_all_k: 0.0,
                        k: 1,
                        total_runs: 2,
                        passed_runs: 1,
                        task_id: "t1".into(),
                    },
                }],
                aggregate: EvalMetric {
                    pass_at_k: 0.5,
                    pass_power_k: 0.25,
                    pass_all_k: 0.0,
                    k: 1,
                    total_runs: 2,
                    passed_runs: 1,
                    task_id: "aggregate".into(),
                },
                capability_metrics: EvalMetric {
                    pass_at_k: 0.5,
                    pass_power_k: 0.25,
                    pass_all_k: 0.0,
                    k: 1,
                    total_runs: 2,
                    passed_runs: 1,
                    task_id: "cap".into(),
                },
                regression_metrics: EvalMetric {
                    pass_at_k: 0.0,
                    pass_power_k: 0.0,
                    pass_all_k: 0.0,
                    k: 0,
                    total_runs: 0,
                    passed_runs: 0,
                    task_id: "reg".into(),
                },
                cost_usd: Some(0.0),
                unpriced_runs: 0,
                duration_secs: 0.0,
                trace_summary: None,
            }],
        };
        let md = report.to_markdown();
        assert!(md.contains("# Eval Report"));
        assert!(md.contains("demo"));
        assert!(md.contains("t1"));
        assert!(md.contains("Capability"));
        assert!(md.contains("pass^k"));
    }

    #[test]
    fn build_task_report_maps_category() {
        let tr = build_task_report(
            "t1",
            "name",
            EvalCategory::Regression,
            EvalMetric {
                pass_at_k: 1.0,
                pass_power_k: 1.0,
                pass_all_k: 1.0,
                k: 1,
                total_runs: 1,
                passed_runs: 1,
                task_id: "t1".into(),
            },
        );
        assert_eq!(tr.task_id, "t1");
        assert_eq!(tr.category, "Regression");
    }
}
