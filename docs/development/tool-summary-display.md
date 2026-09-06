# Tool Summary Display

VT Code has two independent tool-display settings:

- `ui.tool_output_mode` controls how command and tool result bodies are truncated or spooled.
- `ui.tool_display_mode` controls the transition summaries shown before those bodies. It defaults to `compact`; contiguous successful command/PTY calls share a compact activity row while non-command tools retain their compact tree formatting.

```toml
[ui]
tool_display_mode = "compact"
```

Compact mode keeps one concise activity row for each contiguous run of
successful command/PTY calls. A single command can show its command and hidden
line count, for example `• Ran cargo check · … +12 lines`; consecutive calls
collapse to `• Ran 2 commands`. The review suffix includes the configured
shortcut and a styled `click to expand` affordance. In expanded
mode, running PTY blocks show a bounded live tail; after completion, successful
bodies collapse to the activity row.
Failures, non-zero exits, cancellations, warnings, stderr diagnostics,
meaningful diffs, and useful artifacts retain bounded inline context. Complete
output is available in the session-local Transcript Review opened with the
configured review shortcut, including outside fullscreen, where it is ordered with user messages, assistant
responses, reasoning, and other status entries. Rich review rendering is the
default; `r` switches to ANSI-free raw text. Plan updates, non-command tools,
and explicit result bodies end a command group. The review hint follows the
configured primary review binding and is omitted when that action is unbound.
Successful file mutations also end a command group and remain visible as a
glanceable `• Edited path (+N -M)` (or created/deleted) row followed by the
numbered diff preview; the complete tool result remains available to the
agent and Transcript Review.

Failure-like results (hard failures, timeouts, and non-zero command exits) also
show a bounded `Info` diagnosis with `Observed`, `Likely cause`, and `Next
action`. It is derived from the existing bounded preview, while complete raw
output remains available in Transcript Review. Deterministic guidance is used
for policy, permission, authentication, circuit-breaker, sandbox, resource,
and preflight failures; provider-native reasoning settings do not expose raw
diagnosis-provider reasoning.

When the UI collapses or bounds a tool result, every provider/model receives
this exact disclosure after the tool-result user message:

```text
Only you see that command's output — the user's terminal shows at most a few lines of it. If the user needs to read any of it, put it in your reply.
```

Anthropic wire routes whose selected provider/model capability supports it use
the native `clear_at: "next_user_message"` field and the required
`mid-conversation-system-clear-at-2026-08-21` beta. Anthropic-shaped routes
without that capability promote the same text to their top-level system
prompt. All remaining provider routes receive it through their native system,
history, instructions, or transcript mapping, without the Anthropic-only
`clear_at` field. VT Code keeps one typed marker in canonical session history
so provider switching and replay preserve the disclosure. This tells the model
when it must quote or summarize output for the user; it does not expose raw
provider reasoning or replace the complete output retained by Transcript
Review.

In compact mode, PTY commands keep their complete capture and grouped completion row without emitting a transient live PTY block. Progress remains available through the active status/spinner, while warnings, failures, diffs, stderr, and meaningful artifacts stay inline. Expanded mode preserves the bounded live tail.

Model-facing progress guidance complements these UI summaries: for non-trivial
tool work, the model announces the next phase in one brief line, gives one or
two concise sentences when the phase or next action changes, and ends with a
standalone recap. It should summarize material findings instead of rerunning a
command whose successful body is available through Transcript Review.

The runtime mode can be changed for the current session with `Alt+T`. This action is rebindable through the existing keybinding configuration. Use `/config` to cycle `ui.tool_display_mode` and persist the choice to `vtcode.toml`.

Explicit `expanded` mode preserves the existing per-call summary and live-output layout.

## Model-visible tool output budget

Tool-result previews copied into provider-facing history share a 32 KiB
aggregate budget per turn. The existing per-result spool limit still applies;
when the aggregate budget is exhausted, later results expose only bounded
metadata such as the tool name, spool path, byte count, completion state, and a
short note. Complete output remains in the internal spool and current-session
tool-output viewer, so this budget affects recovery diagnostics rather than
Transcript Review.

Static, bounded reads of `.vtcode/context/tool_outputs/` using `cat`, `sed`,
`tail`, or bounded `rg` pipelines are marked `no_spool` before generic output
processing. Dynamic paths, redirects, writes, in-place edits, and malformed
commands remain fail-closed; they cannot opt out of normal spooling.

## Guardrails quick reference

| Signal | Expected behavior | Where it is defined |
| --- | --- | --- |
| `preview_budget_exhausted` | Trust preserved metadata, run one `&&` verifier, then synthesize; never repeat an equivalent call | `generate_tool_guidelines` in `crates/codegen/vtcode-core/src/prompts/guidelines.rs` |
| `turn.blocked` | Resumable stop with streak and counter metadata; one tool-free synthesis, then checkpoint resume | `ThreadEvent::TurnBlocked` in `crates/common/vtcode-exec-events`, `docs/guides/agent-loop-contract.md` |
| Mid-execution replan | Keep scopes, add falsifiers, continue the run on existing `plan.delta` events | `docs/guides/planning-workflow.md` |

The combined developer narrative lives in [Preview Budget, Blocked Turns, and Replans](./preview-budget-blocked-replan.md). The executable regression for all three rows is `crates/codegen/vtcode-eval/evals/preview-budget-blocked-replan.json`.
