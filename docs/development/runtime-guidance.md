# Runtime Guidance and Project Instructions

VT Code has two distinct prompt sources:

| Source | Loaded from | Purpose | Trust boundary |
| --- | --- | --- | --- |
| Compiled runtime guidance | `crates/codegen/vtcode-core/src/prompts/runtime_guidance.rs` | Small, universal user-facing behavior included in Default, Minimal, Lightweight, and Specialized profiles | Part of the application runtime |
| Project instruction map | User/workspace `AGENTS.md`, `CLAUDE.md`, and `.vtcode/rules/` | Project conventions, local architecture, and maintainer workflows | User-controlled context, never a security boundary |

The compiled section is deterministic, cached with the static profile, and
kept below its approximate 256-token cap. It must not read, embed, or generate
content from repository instruction files. Profile-specific operating details
remain in the prompt builder; correctness-critical behavior belongs in runtime
policy, schemas, tests, or lints.

## User-facing progress contract

For non-trivial or tool-using work, the compiled guidance asks the model to
announce the next phase in one brief line before its first tool call. During
long or multi-step work it should post one or two concise sentences only when
the phase or next action changes, then close with a standalone recap of what it
found, changed, and verified, plus what comes next. These are user-facing
status updates, not a transcript of every call or hidden chain-of-thought.

Compact transcript mode may collapse successful command bodies while retaining
complete output in Transcript Review. The model must not rerun commands merely
to reveal hidden output; material findings belong in a visible progress update
or the final reply. The provider-neutral collapsed-output disclosure reinforces
this after each affected tool result.

## Continuity and long-running work

### Runtime observability and cancellation

`AgentRuntime` keeps `ThreadEvent` authoritative while tracing bounded
diagnostics for model latency, turn latency, input/output token usage, output
and reasoning byte counts, tool-call count, finish reason, cancellation, and
timeouts. These fields are diagnostic only and must not be persisted as a
parallel lifecycle schema. A provider error or timeout fails the turn; an
interrupted stream is never reported as successful completion. Open tool calls
are closed with a terminal failed status, while partial streamed text remains
partial and is not promoted to a successful final answer. Follow-up steering
received during streaming is retained for the next turn.

Request segment fingerprints include the provider route, model, context capacity,
effective reasoning tag, tool/parallel/cache capabilities, and current tool catalog
epoch. Repeated requests reuse immutable prompt and ordered tool bytes; capability
changes invalidate the segment identity. Shell profile, environment, and harness
limits belong to the frozen segment prefix. Provider cache routing keys include
the fingerprint where the transport supports explicit keys. Local fingerprint
reuse is not proof of a provider cache hit: use returned usage cache-hit metrics
to measure that separately for each provider.

Tool documentation density is resolved separately from tool authorization. Both
request paths use Minimal guidance at 32,000 context tokens or below, or when the
Default prompt exceeds the configured system-prompt token or monetary budget;
otherwise they retain Default guidance. Missing pricing does not imply cheap
execution. Parallel-call hints require the active provider's parallel-tool
capability. Both profiles preserve inspection, verification, and terminal-owned
WebMCP permission guidance.

The runtime keeps prompt additions small and cache-stable while preserving the
newest working context. Automatic compaction uses a non-configurable continuity
tail target of approximately 20,000 estimated tokens. It retains complete
user/assistant/tool protocol groups verbatim, removes an incomplete trailing tool
call, and summarizes only the older prefix. Unless an explicit harness threshold
is configured, the effective hard threshold is the resolved model capacity,
bounded by the provider route and a positive `context.max_context_tokens` safety
ceiling, minus the next request's output reservation. The default safety ceiling
is zero (automatic). Known request output limits take precedence; otherwise
4,096 tokens are reserved. Explicit thresholds may lower this boundary but
cannot bypass it. Prompt and tool overhead count toward pressure; per-turn
tracing records the context denominator. A derived soft boundary marks
compaction pending for the next outer turn boundary; the effective prompt
threshold compacts before the next model request. Provider-native compaction results are normalized
through the same tail rules, with local fallback when the provider does not
return a usable tail.

Long-running command sessions have an explicit `wait` action. A wait deadline
returns a bounded in-progress result without killing the process, so the model
does not need to spend repeated turns issuing 30-second polls. Full command
output is written to the tool-output spool; responses expose only a bounded
preview and its spool metadata. A spool reference is emitted only after its
file is open and has not reported a write failure; `spool_complete` distinguishes
an active readable partial snapshot from a fully drained output stream. Exited
sessions with an unfinished spool retain the session and defer the reference
until a later wait can safely observe the complete file.

The provider-facing history also has a 32 KiB aggregate tool-preview budget per
turn. After exhaustion, new payload bodies are replaced by bounded metadata,
but scalar control signals such as success, exit code, completion status,
verification requirements, and retryability remain visible. The metadata tells
the agent not to repeat equivalent calls merely to recover hidden output, and
checkpoint diagnostics record how many previews were suppressed.
Diagnostics also report requested, admitted, and derived unadmitted tool-call
counts so budget or policy rejections cannot disappear from turn accounting.
Read-only results reused by same-turn caches, cross-turn target caches, or
bounded history replay all increment the same reuse counter. Request assembly
also collapses legacy duplicate output-disclosure notices to one current marker.

Interactive follow-ups are durable steering intents. Each queued intent has a
UUID, the session envelope stores at most 16 pending intents and a 64-ID applied
window, and the intent is acknowledged only after its tagged user message is
durably checkpointed. Recovery compares IDs in the envelope with tagged history,
not just instruction text, so duplicate text remains meaningful.

Even when `.vtcode/prompts/system.md` replaces the static base prompt, the
compiled section is reattached after prompt layers are resolved. This keeps the
universal baseline present without treating workspace prompt content as a
security boundary.

The dynamic instruction pipeline remains enabled by default. It discovers user
and workspace sources in precedence order, loads nested files for the active
directory, applies path-scoped rules and exclusions, and appends the resulting
project appendix separately from the compiled base prompt. `AGENTS.md` files
therefore remain useful maintainer maps without becoming an implicit source of
universal VT Code behavior.

When changing this boundary, run:

```bash
cargo nextest run -p vtcode-core
cargo check --locked
./scripts/check-dev.sh --changed
```

Release archives are independently allowlisted to contain the binary, man
page, and shell completions only. They must never include `AGENTS.md` or other
workspace guidance.
