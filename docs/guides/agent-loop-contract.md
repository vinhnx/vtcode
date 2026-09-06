# Agent Loop Contract

VT Code keeps its existing harness-first runtime, but its external loop contract
now lines up more closely with SDK-style agent runtimes.

This guide describes the public lifecycle semantics shared by interactive runs,
`vtcode exec`, harness logs, and Open Responses extension events.

## Message and Event Mapping

VT Code does not expose Claude-specific SDK structs. The canonical stream stays
`vtcode_exec_events::ThreadEvent`.

The closest concept mapping is:

| Agent SDK concept | VT Code event |
| --- | --- |
| `SystemMessage(init)` | `thread.started` |
| `AssistantMessage` | `item.*` with `agent_message`, `reasoning`, `tool_invocation` |
| Tool-result `UserMessage` | `item.*` with `tool_output` or `command_execution` |
| `StreamEvent` | `item.updated` plus Open Responses stream events |
| `ResultMessage` | `thread.completed` |
| `compact_boundary` | `thread.compact_boundary` |
| Fresh plan execution handoff | `context.reset` |

`turn.started`, `turn.completed`, `turn.failed`, and `turn.blocked` remain VT Code turn
wrappers around the inner item lifecycle. `turn.blocked` is emitted alongside
`turn.failed` for blocked turns with streak/total/caps/last-tool counters so UI
layers get a first-class signal instead of inferring it. Harness `TurnBlocked`,
`BlockedRecoveryStarted`, and `BlockedRecoveryFinished` item events cover the
recovery lifecycle.

### Tool-result ordering and bounded request repair

An assistant message with tool calls is one protocol batch. Every matching tool
result is sent immediately after that assistant message and before any
intervening system, user, or assistant message; batch result order is retained.
Deferred prompt-injection warnings and recovery directives are appended only
after all results in the batch. The warning is rendered to the UI as soon as a
probe flags output, but its model-facing message is queued and deduplicated so
it cannot split the provider batch.

Before a provider request, VT Code keeps a borrowed history view when this
invariant already holds. Otherwise it builds an idempotent request-only view
that drops orphaned, causally early, and duplicate results, adds bounded
cancellation results for missing calls, and groups split results after their
assistant call. Durable session history is unchanged. If the provider returns
the specific unmatched-tool-result `400`, VT Code retries once only when this
wire view changes; a no-op or repeated failure uses the existing resumable
fail-closed handoff rather than issuing repeated identical requests.

### Turn reliability and diagnostics

The runtime records the canonical `turn.*` lifecycle events exactly once for a
turn. Model latency, output/reasoning byte counts, tool-call count, finish
reason, and reported input/output token counts are emitted as structured
diagnostic fields in runtime tracing; these diagnostics do not introduce a
second event contract. Provider errors and timeouts emit `turn.failed` through
the owning run loop and never produce a successful completion.

Cancellation is fail-closed: an interrupted stream returns a cancelled finish
reason, partial assistant text is not treated as a successful final answer, and
tool calls that were opened but did not reach a terminal provider response are
completed as failed. A timeout applies to the provider stream acquisition and
is reported as a failure; it does not silently convert an empty or partial
stream into success. Steering follow-ups remain queued for the next turn.

Failure-like tool results include hard failures, timeouts, and successful process
responses with a non-zero exit status. Non-zero results retain their stdout,
stderr, exit status, partial output, and spool evidence, but count as failures
for metrics, batch summaries, and recovery diagnosis; they do not create a
successful read-only signature. Low-signal grep/no-match behavior remains
unchanged. The existing model-facing tool response may include a bounded
`diagnosis` object:

```json
{"diagnosis":{"observed":"...","likely_cause":"...","next_action":"..."}}
```

Diagnosis uses only the already bounded tool preview and never reopens a spool.
When routing permits, a tool-free lightweight model request returns the same
three bounded fields as strict JSON. Provider failure, timeout, or malformed
output falls back to deterministic evidence-only guidance. Policy,
authentication, permission, circuit-breaker, sandbox, resource, and preflight
failures always use deterministic guidance, so diagnosis cannot recommend
bypassing safeguards. The UI renders an `Info` block and emits an existing
`ReasoningItem` with stage `"diagnosis"` after the failed `ToolOutput`; this
remains visible when native reasoning is hidden. Provider-native reasoning
continues to follow its existing capability and display settings, and raw
chain-of-thought is never exposed.

When the product collapses or bounds a tool result, every provider/model
receives the fixed disclosure after the tool-result user message: `Only you
see that command's output — the user's terminal shows at most a few lines of
it. If the user needs to read any of it, put it in your reply.` Anthropic wire
routes whose selected provider/model capability supports it use
`clear_at: "next_user_message"` and the required beta; unsupported Anthropic
models and gateways promote the same text to their top-level system prompt.
Other providers map it to their native system, history, instructions, or
transcript representation without the Anthropic-only `clear_at` field. One
typed marker remains in canonical history for provider switching and replay.

### User-facing progress updates

The model-facing runtime contract is intentionally separate from provider
native reasoning. For non-trivial tool work, the model should state the next
phase in one brief line before the first call, provide one or two concise
sentences only when the phase or next action changes, and end with a standalone
recap of findings, changes, verification, and next steps. It must not narrate
every tool call or expose hidden chain-of-thought. When compact UI hides
successful output, it should summarize material findings in those visible
updates or the final reply instead of rerunning commands solely to display
output; complete evidence remains available through Transcript Review.

## Terminal Thread Result

VT Code now emits `thread.completed` at the end of a session or exec run.

Fields:

- `thread_id`: stable event-stream thread identifier
- `session_id`: stable VT Code session identifier
- `subtype`: `success`, `error_max_turns`, `error_max_budget_usd`, `error_during_execution`, or `cancelled`
- `outcome_code`: VT Code-specific terminal code
- `result`: final assistant summary text on successful completion only
- `stop_reason`: provider stop reason when available
- `usage`: aggregate token usage for the full thread
- `total_cost_usd`: aggregate estimated cost when pricing metadata exists
- `num_turns`: total turn count

For `vtcode exec`, `outcome_code` comes from `TaskOutcome::code()`. Interactive
sessions preserve the corresponding VT Code session end semantics.

## Canonical event persistence

Interactive and exec runs share one authoritative persistence contract:

| Concern | Contract |
| --- | --- |
| Event type | `vtcode_exec_events::ThreadEvent` is the only runtime event contract. |
| Canonical path | `<workspace>/.vtcode/sessions/<session_id>/events.jsonl`, with `manifest.json` and derived artifacts beside it. |
| Ordering | One dispatch gate feeds canonical persistence and optional exporters in the same order. |
| Backpressure | Canonical events use bounded non-blocking handoffs to a blocking I/O drain; queue saturation fails closed, and accepted events are never silently dropped. |
| Shutdown | The terminal `thread.completed` event is emitted first, then exporters finish and canonical persistence drains before success is reported. |
| Lifecycle status | Sessions become `active` at `thread.started`/`turn.started`; only `thread.completed` makes the manifest terminal. |
| Retention | Closed sessions use the 50-session/30-day defaults. Active sessions, the current session, symlinks, and unrelated files are preserved. |

`agent.harness.event_log_path` and exec `--events` are explicit compatibility
exports. They do not replace the canonical store, and no global
The user state directory's `sessions` harness artifact is created by default. ATIF and Open
Responses files, when enabled by the interactive harness, are derived under
the canonical session's `derived/` directory. Historical global artifacts are
left untouched.

## Compaction Boundary

Whenever VT Code compacts history itself or via a provider-native compaction
path, it emits `thread.compact_boundary`.

Fields:

- `thread_id`
- `trigger`: `manual` or `auto`
- `mode`: `local` or `provider`
- `original_message_count`
- `compacted_message_count`
- `history_artifact_path`: optional archived history path
- `previous_segment_id` and `new_segment_id`: optional cache segment transition
- `previous_prefix_hash` and `new_prefix_hash`: optional immutable prompt-prefix hashes
- `previous_catalog_hash` and `new_catalog_hash`: optional ordered tool-catalog hashes

Each request segment freezes one system prompt, instruction digest, and
deterministically ordered tool catalog. Ordinary turns only append messages.
Compaction, instruction changes, catalog expansion, or primary
model/provider/mode changes start one new segment; the immutable event archive
is retained and local compaction seeds the new segment with its summary and
continuity tail.

This is emitted for manual `/compact` flows, for automatic compaction, and for
automatic local fallback compaction. When Open Responses is enabled, VT Code
surfaces these as VT Code custom extension events without changing the core Open
Responses response model.

## Fresh Plan Execution Context

Selecting “Yes, clear context and implement” is a plan-to-build handoff inside the same user
session. The runtime preserves the approved plan, task tracker, working tree, configuration,
permissions, authentication, and aggregate usage. It clears only the live transcript and other
transient continuation, recovery, cache-lineage, request-segment, and tool-budget state, then
starts a normal build turn with a compact handoff directive.

The successful handoff emits `context.reset` with `trigger`, `plan_preserved`,
`previous_context_usage_percent`, and `tool_budget_reset`. The event is also written to the normal
JSONL/log stream and forwarded by the Open Responses bridge as `vtcode.context_reset`.

### Unified auto-compaction

Auto-compaction is **on by default** (`agent.harness.auto_compaction_enabled`,
default `true`) and is **unified across both runloops**: the core `AgentRunner`
loop and the binary unified runloop both delegate to the shared
`vtcode_core::compaction` orchestrator (`auto_compact_messages`) rather than
maintaining separate compaction logic. It fires at the effective session
ceiling: an explicit `agent.harness.auto_compaction_threshold_tokens` wins,
otherwise VT Code applies the 90% ratio to the smaller of the provider's hard
context capacity and `context.max_context_tokens` (160,000 by default).
Explicit thresholds remain capped by the provider capacity.
Disabling normal auto-compaction does not disable the single bounded recovery
compaction used after a provider rejects a follow-up that follows successful
tool output; that safety path preserves the current request and completed tool
results, and blocks truthfully if it cannot reduce the context.

To preserve conversational continuity, every compacted history keeps:

- a **continuity tail** — approximately 20,000 estimated tokens of the newest
  complete user/assistant/tool protocol groups retained verbatim. Incomplete
  trailing tool calls are dropped, and an oversized individual message is
  represented by a bounded preview/spool reference;
- the structured **session memory envelope** injected at the boundary (see
  Resume and fork continuity).

The soft compaction threshold is 90% of the effective hard threshold. Reaching
it marks compaction pending and defers the work to the next outer turn
boundary. The hard threshold compacts before the next model request. No hidden
summary model call is issued in the middle of an active tool loop.

If a transient provider failure follows successful tool execution, the unified
runloop first compacts only the older prefix, preserving the current request,
tool outputs, and memory envelope. It emits one canonical
`thread.compact_boundary`, then permits one bounded `ToolEnabledRetry` so a
required edit or verification can still run without repeating completed
read-only exploration. A second failure produces a blocked, resumable handoff;
the harness never reports successful completion without confirmation.

### Blocked-turn recovery

Every blocked turn publishes one non-empty deterministic assistant response
through normal conversation history, the renderer, and
`ThreadEvent::ItemCompleted` with `AgentMessage`, then emits the corresponding
`turn.failed` event plus a first-class `turn.blocked` event with fuse counters.
The turn result remains `Blocked`; publishing the handoff does not convert it
to success or emit `turn.completed`. The TUI surfaces the block via a `Blocked`
header badge, `Blocked • continue to retry…` footer hint, transcript banner, and
`ActionRequired` terminal title; `ActivityState::Blocked`/`Recovery` drive those
states while input stays enabled. Blocked-turn spool outputs are pinned until
the blocker resolves so `continue`/`--resume` can still read them.

Blocked responses are reason-specific. A pending-verification response explains
that inspection-only checks, link checks, and `git diff --check` do not clear the
anti-blind checkpoint and directs the operator to run `cargo check --locked` or
the relevant `cargo nextest run`. A context-capacity response explains that
bounded compaction could not reduce the request, retains completed tool outputs,
and directs the operator to resume after reducing context or switching models.
Other blocked reasons use a generic retry handoff. Existing recovery text is
reused when it was already published, so the assistant item is never duplicated.

A lost verification result must not deadlock the anti-blind gate. While the
checkpoint is pending, a verifier-level `Failure`/`Timeout` (for example, the
exec session ended before the verifier's output was captured, reported by
`write_stdin` as a missing session) grants the same bounded fix-up window as a
genuine failed verifier and surfaces a "Verification result lost" directive; the
gate still only clears on a successful standalone verifier re-run. The
`turn.blocked` event also populates `last_tool`, `consecutive_cap`, and
`total_cap` from the blocked-tool-call fuse when it tripped, and transcript
block reasons are truncated (~600 chars) with a pointer to the handoff file,
which retains the full reason.

The fork/branch history builder (`build_summarized_fork_history`) deliberately
omits the continuity tail and produces a minimal resume artifact (envelope +
summary + retained users only).

### Long-running command waits and durable steering

`write_stdin` and `unified_exec` accept an explicit `action: "wait"`. A wait
blocks until the process exits or its requested deadline expires, then returns
one bounded result. A deadline does not terminate an in-progress process; the
returned session ID is reusable for a later wait. Wait time is excluded from
the ordinary per-turn harness wall-clock budget, while cancellation, shutdown,
safety policy, and the configured long-running-command ceiling remain active.

Session input is policy-checked: each submitted line is evaluated against the
same PTY deny list that guards session creation, so a denied interactive
program cannot be launched by typing into an already-running session. The
`unified_exec` `action: "code"` path is likewise subject to the command policy
through its interpreter program (`python3`/`node`); it cannot bypass a policy
that excludes those programs.

Command responses never expose unbounded accumulated `raw_output`. They return
a bounded preview plus total bytes, truncation state, exit state, and a spool
path when the output file is available. `spool_complete` is false while an
active session is still writing and the path is a readable partial snapshot.
When an exited session is still draining, the path is withheld and
`spool_pending` is set until a later wait completes the output spool.
Producer-marked spooled command responses reuse the command preview policy and
cap the model-visible preview at the smaller of the requested budget and 6 KiB.
Inspection commands preserve head and tail context; verification and mutation
commands preserve the tail. The complete spool, byte counts, reference
metadata, and failure or recovery diagnostics remain available without
reopening the spool while the response is constructed.

Across a turn, provider-visible tool previews are capped at 32 KiB. The
budget is enforced twice: at the tool-registry output boundary (which charges
each response's payload bodies and truncates or strips them, marked with
`preview_budget_exhausted`) and again by the unified runloop when responses
enter provider-facing history. Once that
aggregate budget is exhausted, VTCode retains bounded outcome and control
metadata while omitting payload bodies. A successful verifier therefore stays
authoritative without encouraging duplicate reads or checks. Blocker live
pointers are cleared only by the session that created them; archived blocker
files remain self-contained, append a durable resolution marker before pointer
cleanup, and do not claim ownership of the workspace-global task tracker. An
ordinary user exit after a completed non-fallback turn is reported as successful
thread completion; an exit that terminates active work remains cancellation.

Workspace-aware tool responses and execution summaries render paths inside the
active workspace relative to that workspace (for example,
`.vtcode/tasks/current_task.md`). Paths outside the workspace keep their
absolute form so diagnostics do not hide external locations. The same display
rule is used for generated planning artifacts and handles canonical workspace
paths reached through symlinks; planning lifecycle events reuse the same
display value, and it does not change the path used for I/O.

Steering follow-ups are persisted as UUID-tagged intents. The schema-v3 session
envelope keeps at most 16 pending intents and a 64-ID applied window. Recovery
replays only pending IDs absent from both the applied window and tagged user
history; the public `FollowUpInput(String)` message shape remains unchanged.

## Budget and Limits

`agent.harness.max_budget_usd` is the shared budget setting for interactive and
exec sessions.

- VT Code estimates cost from aggregate usage via `ModelResolver::estimate_cost`.
- If pricing metadata is unavailable for the active model, VT Code does not
  enforce the budget.
- In that case `total_cost_usd` stays `null` and VT Code emits one warning.

Turn limits still surface through `thread.completed.subtype = "error_max_turns"`.

## Hooks

VT Code now supports `hooks.lifecycle.pre_compact`.

`pre_compact` runs before VT Code records a compaction boundary. Its payload
includes:

- `session_id`
- `cwd`
- `hook_event_name = "PreCompact"`
- `trigger`
- `mode`
- `original_message_count`
- `compacted_message_count`
- `history_artifact_path`
- `transcript_path`

`session_start` with source `compact` remains supported for compatibility, but
`pre_compact` is the first-class hook for compaction-aware automation.

## Orient Phase

Every session should begin by gathering orientation context from external artifacts. This follows the long-running harness pattern: the agent reads the progress ledger, harness artifacts, loop memory, and git log to understand the current state before acting.

The orient phase produces an `OrientationContext` (see `crates/codegen/vtcode-core/src/core/agent/bootstrap.rs`) that includes:

- Progress ledger summary (goal, completion ratio, confidence, stall status)
- Harness artifact summaries (spec, contract, sprint contract, evaluation, outcome verification)
- Recent git log (last 5 commits)
- Loop memory notes and decisions from previous iterations
- Handoff context from a previous agent, if any

This context is injected as a `[Orientation Context]` section in the system prompt, using summaries and references rather than full content to keep the context lean.

## Handoff Protocol

When one agent hands off to another, it produces a `HandoffRequest` (see `crates/codegen/vtcode-core/src/core/agent/handoff.rs`) that includes:

- **State summary**: what was accomplished, what remains
- **Boundary status**: explicit list of features/deliverables with Done/InProgress/NotStarted/Blocked status
- **Modified files**: files changed in this session
- **Test results**: last test run outcome with actual output
- **Open decisions**: unresolved questions for the next agent
- **Known issues**: bugs, limitations, tech debt the next agent should know
- **Next actions**: recommended next steps
- **Task context**: the original task description

The handoff prompt is rendered as a structured markdown section that the next agent can parse without re-exploring the codebase. This prevents the "inheriting a collaborator's mess" problem: the boundary status makes explicit what is done vs. what was left incomplete.

## Related Controls

These VT Code settings line up with common agent-loop controls:

- Tool allow and deny rules: `[permissions].allow`, `[permissions].deny`, tool policy config
- Permission policy: workspace trust, human-in-the-loop settings, granular agent rules, and full automation allow-lists
- Effort: provider/model reasoning settings
- Tool discovery: MCP and tool catalog flows
- Resume and fork continuity: session archives, thread bootstrap, and compaction envelopes

## Context Reset

Context reset is a context engineering technique **distinct from compaction**.
While compaction preserves conversational continuity within the same task,
context reset deliberately discards conversation history so a fresh agent can
reorient from durable artifacts only.

### When It Triggers

Configured via `agent.harness.context_reset_mode`:

| Mode | Trigger | Use Case |
|------|---------|----------|
| `off` (default) | Never | Normal operation |
| `on_stall` | `context_reset_stall_threshold` consecutive stalled turns | Long-horizon tasks where the agent gets stuck |
| `on_compaction` | After every auto-compaction | Clear noise accumulated before compaction |

### What Happens

When a reset triggers:

1. A `ContextResetManifest` is written to `.vtcode/tasks/current_context_reset.md`
   recording the trigger reason, stall count, and timestamp.
2. The next session starts with **only** `OrientationContext` — no conversation
   history is carried forward.
3. The orient phase reads the manifest and prepends a `### Context Reset` banner:
   "This session starts from a clean context. Reorient from the artifacts below."

### Artifacts That Survive a Reset

All durable artifacts persist across a reset:

- Progress ledger (`crates/codegen/vtcode-memory/src/progress.rs`)
- Harness artifacts (spec, contract, feature list, evaluation, sprint contract)
- Loop memory (notes, decisions)
- Git log and working tree state
- Compaction summary

The comparison with compaction is summarised in [Context Reset](#context-reset).

## Loop Engineering Additions

The subagent layer now supports loop-engineering primitives:

- **Worktree isolation**: set `isolation = "worktree"` on an agent spec to run the child in a git worktree under `.vtcode/worktrees/`. The child's file mutations stay in its own working tree until explicitly merged.
- **Propose/verify separation**: `SubagentController::verify_proposed_change()` spawns a read-only verifier sub-agent that re-reads affected files and approves or rejects the change. The verifier has no shared context with the proposer.
- **Loop run state**: `crates/codegen/vtcode-core/src/loop_state.rs` persists step index, cumulative cost, and status to `.vtcode/state/loop-<id>.json` so a scheduler can resume across invocations.
- **Loop memory**: `crates/codegen/vtcode-core/src/loop_memory.rs` provides an append-only store for agent notes and decisions in `.vtcode/state/notes.md` and `decisions.md`.
- **Cost guardrails**: `CostBudget` in `loop_state.rs` tracks token/cost/step limits and reports `BudgetStatus` (Ok/TokenLimitReached/CostLimitReached/StepLimitReached).

See [Loop Engineering](../loop-engineering.md) for the full design.
