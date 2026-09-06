pub mod environment;
pub mod executor;
pub mod metric;
pub mod report;
pub mod suite;
pub mod task;
pub mod trace_analyzer;

pub use environment::{CommandProbe, EnvironmentProbe, FileExistsProbe, GitCleanProbe};
pub use executor::{EvalExecutor, EvalRunOptions, run_suite, run_suite_with_options};
pub use metric::{
    EvalMetric, aggregate_metrics, compute_metric, compute_metric_with_k, pass_all_k, pass_at_k, pass_at_k_with_k,
    pass_power_k,
};
pub use report::{EvalReport, SuiteReport, TaskReport, build_task_report};
pub use suite::EvalSuite;
pub use task::{EvalCategory, EvalRunResult, EvalTask, RunOutcome};
pub use trace_analyzer::{
    HarnessTraceSummary, LatencyStatistics, TokenUsage, analyze_jsonl, analyze_jsonl_file, analyze_jsonl_reader,
};
