# vtcode-eval

Agent evaluation framework for VT Code — defines eval tasks, runs them through
an agent executor, grades outcomes, and reports pass@k / pass^k metrics split by
capability and regression categories.

The crate is deliberately small and I/O-free at its core: `run_suite` orchestrates
the loop of tasks × attempts, computes per-task metrics, and assembles a report.
Everything that touches the filesystem, config, or the concrete agent runner is
pushed behind the `EvalExecutor` trait, so the harness is fully unit-testable with
an in-memory fake executor.

## Layout

| Module | Responsibility |
|---|---|
| `task` | Data model: `EvalTask`, `EvalCategory`, `RunOutcome`, `EvalRunResult` |
| `suite` | `EvalSuite` — a named set of tasks with an `attempts` count |
| `metric` | `EvalMetric` and combinatorial `pass@k`, independent `pass^k`, and `aggregate_metrics` |
| `executor` | `EvalExecutor` trait + bounded `run_suite_with_options` orchestration |
| `environment` | `EnvironmentProbe` checks: `CommandProbe`, `FileExistsProbe`, `GitCleanProbe` |
| `report` | `EvalReport` / `SuiteReport` / `TaskReport` + `to_markdown` renderer |
| `trace_analyzer` | Privacy-preserving JSONL summaries for DeepSeek and VT Code harness traces |

## Concepts

- **`EvalTask`** — a prompt plus `verify_commands` and an optional `timeout_secs`.
  `category` is `Capability` or `Regression`.
- **`RunOutcome`** — `Pass`, `Fail`, or `Error` for a single task attempt.
- **`EvalMetric`** — combinatorial `pass_at_k`, independent reliability `pass_power_k`
  (`(passed / attempts)^k`), and `pass_all_k`, averaged per task. It also carries
  the selected `k` and raw `passed_runs` / `total_runs`.
- **`EvalExecutor`** — the trait boundary. Implementors own "run this task" semantics
  (drive the agent, apply environment probes, grade the result). `run_suite` schedules
  attempts with a default concurrency of two; `run_suite_with_options` makes the
  bound and metric `k` explicit.

`HarnessTraceSummary` provides aggregate-only trace facts: turns, bounded
tool/error counts, latency, output byte totals, repetition, and token/cache
usage. Raw prompts, arguments, file contents, and output text are not retained.
Use `analyze_jsonl_file` for buffered file analysis or `analyze_jsonl_reader`
when the caller already owns a stream.

## Usage

```rust
use vtcode_eval::{EvalExecutor, run_suite, EvalSuite};

// Implement EvalExecutor to drive your agent + grade outcomes, then:
let report = run_suite(&my_executor, &suite).await?;
println!("{}", report.to_markdown());
```

Trace analysis can be kept out of the agent hot path and run against a persisted
JSONL session:

```rust
use vtcode_eval::analyze_jsonl_file;

let summary = analyze_jsonl_file("session.jsonl")?;
println!("{} tool calls, {} input tokens", summary.tool_calls, summary.token_usage.input_tokens);
```

The analyzer recognizes both DeepSeek-style records and serialized
`vtcode-exec-events::ThreadEvent` shapes. Thread-level aggregate usage is used
only when per-turn usage is absent, preventing double counting.

## Notes

- `run_suite` performs no file I/O or trust checks; the caller owns configuration,
  while the scheduler rejects `attempts == 0` and invalid metric `k` values.
- Reports sum known per-attempt costs and count unknown-pricing attempts separately;
  unknown cost is never silently treated as a known zero.
- Environment verification (`EnvironmentProbe`) is a separate concern from outcome
  grading — executor implementations decide whether and how to apply probes before
  returning a `RunOutcome`.
