# Full Automation

`--full-auto` lets VT Code run non-interactively on explicitly allow-listed tools. It is an execution and permission layer on top of the active primary agent, not a primary agent of its own. Use it only when you fully trust the workspace configuration and have reviewed the safeguards below.

`--full-auto` is intentionally separate from normal session permissions:

- Normal sessions use primary agents plus granular permission rules.
- Agent specs use `permissions.default` plus `allow`, `ask`, `auto`, and `deny` rule buckets.
- The `permissions.auto` bucket sends matching calls to classifier-backed review instead of treating them as unrestricted.
- `--full-auto` uses the explicit `[automation.full_auto]` allow-list as a hard gate. Tools outside the allow-list are denied; promptable outcomes inside the allow-list are routed through automatic permission review after explicit deny and policy checks instead of asking.
- `--dangerously-skip-permissions` auto-approves promptable actions while still respecting explicit denies and policy blocks.

Primary-agent selection still works normally. If you explicitly select or configure a primary agent, including `duck`, full-auto runs on top of that agent. If no primary agent is explicitly selected or configured, VT Code selects the effective `auto` primary agent. If full-auto needs that defaulted `auto` agent and no effective `auto` exists, startup fails fast.

## Activation Checklist

1. **Update `vtcode.toml`**
    - Enable the feature: `automation.full_auto.enabled = true`.
    - Configure the tool allow-list to match your risk tolerance.
    - Keep `require_profile_ack = true` so a profile file is required.
2. **Create the acknowledgement profile**
    - Place the file referenced by `automation.full_auto.profile_path` in your workspace.
    - Document acceptable behaviour, escalation procedures, and any workspace-specific hazards.
3. **Review tool policies**
    - Full automation still honours existing tool policies; denied tools remain blocked.
    - Tools not included in the allow-list will be rejected automatically.
    - Allow-listed promptable actions use automatic permission review after deny and policy checks.
4. **Launch the agent**
    - Run `vtcode --full-auto` with any other CLI flags you need.

## Runtime Behaviour

- VT Code displays the active allow-list at session start.
- Full-auto does not grant tools outside `[automation.full_auto].allowed_tools`.
- Explicit denies and policy blocks are honoured before full-auto review.
- Promptable allow-listed actions are reviewed automatically instead of interrupting for user input.
- Non allow-listed tools are rejected before execution, and their attempts are logged.
- If the acknowledgement profile is missing while required, the CLI aborts before launching.

## Customising The Allow-List

```toml
[automation.full_auto]
enabled = true
require_profile_ack = true
profile_path = "automation/full_auto_profile.toml"
allowed_tools = [
    "exec_command",
    "write_stdin",
    "apply_patch",
    "code_search",
]
```

Tips:

- Use the constants listed in `vtcode_core::config::constants::tools` to avoid typos.
- Include `"*"` only when the workspace is fully isolated.
- Combine with granular agent permissions if you need per-tool constraints in normal interactive sessions.
- Treat the list as a hard execution boundary for full-auto: outside the list is denied, and inside the list still passes deny and policy checks before automatic review.

## Propose/Verify Sub-agent

When `automation.full_auto.verify_mutations` is enabled, every call to `SubagentController::verify_proposed_change()` spawns a fresh read-only verifier sub-agent that re-reads the affected files and approves or rejects the change before it is committed. The verifier runs with no shared context from the proposer, preventing confirmation bias.

```toml
[automation.full_auto]
enabled = true
verify_mutations = true
```

The verifier is gated behind this flag because it roughly doubles the token cost on mutating calls. Default: `false`.

When enabled, the propose/verify cycle works as follows:

1. The proposer sub-agent makes a change (file edit, shell command, etc.).
2. The caller invokes `verify_proposed_change()` with a diff description and affected file paths.
3. A verifier sub-agent (falling back to the `explorer` agent if no `verifier` agent is defined) reads the files and returns `VerificationResult { approved, issues, reasoning }`.
4. If rejected, the caller can retry the mutation up to N times.

See [Loop Engineering](../loop-engineering.md) for the full design.

## Orchestrated Harness

For longer unattended builds, prefer enabling the planner/evaluator harness instead of relying on a single uninterrupted build loop:

```toml
[agent.harness]
orchestration_mode = "plan_build_evaluate"
max_revision_rounds = 2
```

When enabled, `vtcode exec --full-auto` writes a small set of working artefacts under `.vtcode/tasks/`:

- `current_spec.md`: high-level execution spec
- `current_contract.md`: observable done criteria and verification contract
- `current_task.md`: tracker state
- `current_evaluation.md`: evaluator output after a completion attempt

This keeps long-running work resumable and makes evaluator-driven revision rounds explicit instead of relying on the generator to judge itself.

Each revision round follows a fixed LLM-call lifecycle:

1. **Planner** expands the task into a spec, contract, feature list, and tracker.
2. **Build** executes the tracker (may issue multiple turns, including tool calls, until it signals completion).
3. **Evaluator** judges the candidate and returns a verdict.
4. On rejection, a **Replanner** rewrites the spec/contract/tracker from evaluator feedback.
5. The **Build** phase runs again (its own completion-signaling turn) before the **Evaluator** re-runs.

A round is exhausted once `max_revision_rounds` replans have been attempted; the run then writes a blocked-handoff artifact instead of silently accepting the candidate. Because the build phase must complete (produce a completion-signaling response) before re-evaluation, every revision round consumes at least one additional build turn. The eval test harness uses a role-aware mock (`RoleQueuedProvider`) that matches responses to the calling sub-agent by system prompt and returns a completion default for any build turn it did not explicitly queue, so tests declare *what each role says* rather than the exact wire-order of calls.

## Regression Testing

For long-running automation, use the `vtcode-eval` crate to build a regression
eval suite that verifies the agent still handles previously-passing tasks after
model or harness changes.

### Running Eval Suites

Use the `vtcode exec eval` CLI command to run an eval suite:

```bash
vtcode exec eval --suite my-suite.json --output report.md
```

The suite JSON file contains an `EvalSuite` with tasks, each specifying a prompt,
setup/verify commands, and category (capability or regression). The runner
executes each task through the agent with a bounded default concurrency of two,
verifies outcomes with environment probes (command exit codes), computes
combinatorial pass@k and independent pass^k metrics per task, and outputs a
deterministically ordered markdown report. Known per-attempt cost is aggregated;
unknown pricing is reported separately rather than treated as free.

See [vtcode-eval](../../vtcode-eval/) for the framework and metric definitions.

## Profile File Recommendations

The profile file is a simple acknowledgement document. Suggested content:

- Operator name and timestamp approving unattended execution.
- Workspace-specific limitations, such as directories that must not be modified.
- Contact or escalation details if automation encounters unexpected failures.
- Rollback procedures or monitoring steps to follow afterwards.

Keeping this file under version control provides a clear audit trail for when full automation was used and under which guardrails.
