# vtcode-eval

[Root AGENTS.md](../AGENTS.md) | Agent evaluation framework: pass@k / pass^k metrics, capability/regression evals, environment-based outcome verification.

## Module Groups

| Area | Modules |
|---|---|
| Data model | `task` — `EvalTask`, `EvalCategory`, `RunOutcome`, `EvalRunResult` |
| Suite config | `suite` — `EvalSuite` (tasks + attempts + id) |
| Metrics | `metric` — `EvalMetric`, combinatorial `pass@k`, independent `pass^k`, and `aggregate_metrics` |
| Orchestration | `executor` — `EvalExecutor` trait + bounded `run_suite_with_options` (pure, I/O-free) |
| Environment | `environment` — `EnvironmentProbe` + `CommandProbe`, `FileExistsProbe`, `GitCleanProbe` |
| Reporting | `report` — `EvalReport`, `SuiteReport`, `TaskReport`, `to_markdown`, `build_task_report` |
| Trace analysis | `trace_analyzer` — privacy-preserving JSONL aggregate summaries |

## Rules

- `lib.rs` re-exports the public facade: types from `task`/`suite`/`metric`/`report`, `executor::{EvalExecutor, run_suite}`, and `trace_analyzer` summaries.
- `run_suite_with_options` depends only on the `EvalExecutor` trait — no file I/O, config, or trust checks. It bounds concurrent attempts (default two), validates `attempts`/`k`, and sorts results by task then attempt before reporting.
- The four `EvalCategory` strings (`Capability`, `Regression`) are the only valid split keys; `report` filters on `category.label()` serialization.
- `EvalSuite` is defined once in `suite.rs` and re-exported from `lib.rs`. Do not duplicate it in `task.rs`.

## Gotchas

- `attempts >= 1` is NOT enforced by serde (suite.rs test confirms `attempts: 0` deserializes). `run_suite_with_options` and the CLI both reject zero-attempt suites before scheduling.
- `run_suite` uses the default bounded scheduler; use `run_suite_with_options` to set a different concurrency or metric `k`. The executor still owns task execution semantics and environment verification.
- Environment verification (`EnvironmentProbe`) is a separate concern from outcome grading — `EvalExecutor` implementations decide how/whether to apply probes before returning `RunOutcome`.
- `trace_analyzer` retains aggregate facts only; summaries must not include command arguments, file contents, or tool output. Its public facade delegates bounded streaming and metric accounting to private submodules.
