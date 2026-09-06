===

Areas that are complex
The most intricate parts are likely:

     1. core_tui/session.rs
        Main state transitions, layout, rendering, and interaction coordination.
     2. Transcript rendering and caching
        Reflow, scroll behavior, tool blocks, PTY output, overlays, and cache
        invalidation interact heavily.
     3. Input ownership
        Normal input, popups, approval prompts, search, and fullscreen review each have
        different routing rules.
     4. Async integration
        Terminal events, agent events, PTY events, and redraw requests must be
        coordinated without blocking the runtime.
     5. Theme and contrast behavior
        Theme changes affect normal text, accents, syntax highlighting, status colors,
        overlays, and accessibility requirements.

==> improve

===

diagnose and improve vtcode harness based on the session run log.

/Users/vinhnguyenxuan/Developer/learn-by-doing/vtcode/.vtcode/checkpoints/turn_1032.json /Users/vinhnguyenxuan/Developer/learn-by-doing/vtcode/.vtcode/checkpoints/turn_1031.json

Diagnosis (from checkpoint evidence)
Turns analyzed: turn_1030.json (108,346 in-tok, 12 tools, 56s), turn_1031.json (32,280 in-tok, 4 tools, 33.7s), turn_1032.json (16,149 in-tok, 0 tools, 14.3s).
#: 1
Finding: Prompt cache never warm
Evidence: cached_input_tokens: 0, cache_creation_tokens: 0 on all turns — turn
1030 paid full price for 108K input tokens
─────────────────────────────────────────────────────────────────────────────────
#: 2
Finding: Preview budget exhaustion returns zero visibility
Evidence: Even trivial/empty commands returned preview_budget_exhausted with
empty output; spool_path: null, byte_count: 7751 = result dropped entirely (
neither inline nor spooled)
─────────────────────────────────────────────────────────────────────────────────
#: 3
Finding: completion_state: "unknown" on successful (exit-0) exec results
Evidence: Diagnostics can't distinguish clean completion from timeout
─────────────────────────────────────────────────────────────────────────────────
#: 4
Finding: model_visible_output_bytes ≪ raw_spooled_bytes
Evidence: Turn 1031: 19,325 visible vs 44,129 spooled (~44% of evidence reached
the model)
─────────────────────────────────────────────────────────────────────────────────
#: 5
Finding: Low-signal detector misses duplicate listings
Evidence: low_signal_tool_calls: 0 despite 3 overlapping find invocations in one
turn
─────────────────────────────────────────────────────────────────────────────────
#: 6
Finding: Diagnostics schema instability
Evidence: Turn 1030 has elapsed_ms: null, requested_tool_calls: null — no trend
analysis possible across turns
─────────────────────────────────────────────────────────────────────────────────
#: 7
Finding: files array always empty (file_count: 0) even for file-reading turns
Evidence: Session replay can't show touched files

===

====

===

In vtcode + gpt-6-astra (1.05M ctx / 922k max in / 128k out / $10 in / $50 out / reasoning.effort: low,medium,high,xhigh,max):
Astra is built for exactly this: long multi-step coding, computer-use, multi-agent delegation, with fewer tokens/task than GPT-5.6 Sol. Gotchas for VT Code: >272k in = 2x in + 1.5x out, no temperature/top_p, use Responses API, and it over-asks + over-tests + over-obeys AGENTS.md/SKILL.md by default. 0. Wire it up (once)

===

1. 1M-context architecture audit
   reasoning.effort=xhigh. Use file_search + code_search, not full-file dumps.

Audit crates/codegen/vtcode-core/core/agent/, tools/, llm/, prompts/sections + guidelines.rs:22, runtime_guidance.rs:7 against docs/harness/ARCHITECTURAL_INVARIANTS.md, docs/guides/agent-loop-contract.md.

Find: duplicate types vs ThreadEvent, prompt bloat >256 tokens, spool/preview budget leaks, sync fs in async paths, policy bypasses. Return: 10 findings ranked by blast radius with file:line + minimal patch via apply_patch.

Notable findings:
The 32 KiB per-turn aggregate preview budget does not exist — no byte ledger is ever incremented or consulted; every call independently gets a 40 KiB default (200 KiB if requested). Enforcing it needs a per-turn ledger in ToolRegistry (registry/mod.rs:182, registry/spool_processing.rs:24).
ThreadEvent::TurnBlocked and ContextReset are never emitted anywhere (events/mod.rs:540 turn_blocked has zero callers; the Open Responses bridge handles both, vtcode-llm/src/open_responses/bridge.rs:140,189 — dead paths). Blocked turns instead emit only the BlockedHandoffWritten harness event (runner/execute_helpers.rs:32).
tools/handlers/tool_handler.rs:297 ToolEvent ("from Codex") is a parallel item-lifecycle enum used only for tracing; file_change_completed (events/mod.rs:820) is dead code that hardcodes PatchChangeKind::Update.
unified_exec action:"code" spawns python3/node with no policy check or sandbox plan (executors.rs:421,444); write_stdin has no re-check of the PTY deny list on submitted lines.
Sync-fs probes in the per-call dedup path (registry/execution_history.rs:300-313) and 3× canonicalize per planning tool call (registry/planning_workflow_checks.rs:138-154) — cold-ish but per-call syscalls.
Dead prompt code: SectionKind enum in prompts/sections.rs:13-60 (duplicate of the private one in system.rs) and the never-wired few_shot.rs 800-token budget.

===

3. Harness loop tuner for Astra behavior
   VT Code prompts live in crates/codegen/vtcode-core/src/prompts/guidelines.rs + runtime_guidance.rs. Astra needs explicit autonomy/delegation/testing calibration.

Propose new RUNTIME_GUIDANCE_SECTION + Active Tools lines that: 1) force subagent parallelization, 2) forbid approval pauses for read-only/reversible, 3) limit verification to one verifier unless failed. Keep <256 tokens, deterministic, idempotent via ensure_runtime_guidance. Include golden/compactness test updates.

===

4. Sandbox / safety red-team (use Astra cyber strength, fenced)
   reasoning.effort=max. You are defensive auditor only. No weaponization, no exfil.

Target: vtcode-bash-runner sandbox launch, vtcode-core/tools/registry/executors/, mcp/plugin_providers.rs ./-canonicalization, WebMCP edits, exec_policy vs command_safety layers.

Try: command injection, path/symlink escape, env leakage, spool recursion (.vtcode/context/tool_outputs/ must use no_spool), fail-open on saturation. For each: PoC test in scripts/tests/ or Rust unit test, fail-closed fix, add adversarial regression. Verify: cargo nextest run -p vtcode-core -E 'binary(/pty_tests/)', -p vtcode-bash-runner -E 'binary(/pipe_tests/)'.

===

5. Rust quality / perf sweep
   reasoning.effort=medium. Follow AGENTS.md: surgical, preserve APIs, CompactString, Cow<'static,str>, Arc<Vec<Message>> via messages_mut(), tokio fs or spawn_blocking for scans, ast-grep for shape (not rg), hawk for dead code.

Run: cargo clippy -- -D warnings, ./scripts/hawk.sh --deny, ast-grep scan. Fix only main logic, no test churn. Verify with ./scripts/check-dev.sh --lints.

===

6. Docs/eval closer (VT Code definition of done)
   Every major feature must update: docs/development/ guide + quick-ref row, prompts/guidelines.rs + vtcode-utility-tool-specs schema if tool surface changed, ThreadEvent if runtime contract changed. Check AGENTS.md links still resolve.

Generate missing docs + vtcode-eval regression (pass@k, env-verified outcome) + nextest test. Plain language, no "delve/leverage/Bottom Line:", paragraphs over lists unless parallel/sequential.

===

7. Cheap long-runner pattern
   Use low for edits, configuration_update to xhigh/max only for hard segments to preserve cache. Batch/Flex at 50% for sweeps. Chunk under 272k to avoid 2x pricing.

=====

More prompts (8-17) 8. Cache-stability + token-bloat fix:
reasoning.effort=xhigh. Audit prompt caching: stable_system_prefix_hash in core/agent/hash_utils.rs:39-55 strips Active-Tools/Catalog/Context but not [Harness Limits] or ## Environment/Shell Profile. cache_key in prompts/system.rs:576-606 omits prompt_context. system_prompt_budget in system.rs:305-462 is warn-only trim-off by default, estimator len/4. Fix to keep PROMPT_CACHE hits, include digests in envelope instruction_digest, enable safe trim, add hit-rate assert via Usage.cache_hit_rate. Verify: cargo nextest run -p vtcode-core prompts::

9. Compaction vs reset unification:
   Compaction preserves+new segment (compaction_checkpoint.rs:25-130, compaction/mod.rs:21-43) vs context_reset.rs discards to .vtcode/tasks/current_context_reset.md. Session clear wipes all in session/mod.rs:487-505. Auto trigger 90% min(provider,session) in compaction/memory_envelope.rs:1338-1349. Unify to single manifest referencing ThreadCompactBoundary + ContextReset events, test stall->reset->orient roundtrip. Keep ThreadEvent schema 0.14.0 compatible.

10. Parallel fan-out:
    can_parallelize=readonly&&preflight in tool_batching.rs:81-83 over-serializes; duplicate exec_command forced sequential; guidance is one line in guidelines.rs:80-82. Widen parallel_safe_after_preflight for pure reads (read_file/batch.rs), add few-shot parallel example, surface fan-out in telemetry. Verify max_parallel_tool_calls honored in tool_exec.rs:417,750.

11. Failure taxonomy collapse:
    Unify harness_kernel.ExecutionFailure, cargo_failure_diagnostics, Reasoning stage diagnosis, ErrorRecoveryState+circuit, HarnessEventKind::ToolRetry/ErrorRecovered into ErrorCategory+ToolOutcome path. Always emit HarnessEventItem{attempt,error_category,duration_ms}. No new ThreadEvent variants.

12. Spool hardening:
    Spool 8192B in output_spooler.rs:34 vs 32KiB/turn budget vs OUTPUT_PREVIEW_CHARS_PER_TOKEN=4. Reducers only cover read_file/unified_exec. Anti-recursion depends on caller no_spool, is_tool_output_spool_path is substring match. Enforce no_spool at gateway via canonical containment, append-only+digest reads via SpooledOutputReference only, add recursion/poisoning tests.

13. Shell injection + redirection blindspot:
    reasoning.effort=max, defensive only. executor.rs:166-169 sh -c + shell_handler.rs:86-92 join(" ") + preflight skip(1) in sandbox_runtime.rs:674-729 misses > ~/.ssh/authorized_keys, python -c, $BIN/curl, sudo unwrap. Enforce argv-only exec unless validate_command_safety+redirection-aware preflight pass, deny > to sensitive, expand interpreter -c/-e list. Add regression tests, run pty_tests+pipe_tests.

14. Env leakage + containment TOCTOU:
    manager.rs:56-62 uses denylist filter_sensitive_env not allowlist build_sanitized_env; PYTHONPATH/NODE_PATH/RUSTFLAGS/NODE_OPTIONS/GIT_SSH_COMMAND leak. WorkspaceGuardPolicy lexical vs ensure_path_within_workspace_resolved, skill_additional_permissions normalize only. Switch restrictive to allowlist, scope PYTHONPATH to workspace, fail-closed canonicalize, add symlink-swap harness.

15. Skill/MCP trust:
    NETWORK_TOOLS misses exec/shell egress in skill_policy.rs:14-44, silent UseDefault->WithAdditional merge, SKILL.md unfenced, SkillToolScope checked once. MCP parse_mcp_tool verbatim no size/name cap. Treat shell as network unless BlockAll, require human approval for out-of-workspace paths, fence skill/MCP descriptions + injection probe, cap schema bytes, namespace names, rate-limit list_changed.

16. Eval + memory fix:
    metric.rs:13-25 fake pass@k, suite attempts:0 deserializes, executor sequential cost-blind, search_memory substring count, eviction truncates without summarize. Implement true pass@k/pass^k, attempts>=1 guard, parallel run_suite with cost_usd aggregation, wire eviction->grounded_facts summarizer, BM25+recency search. Add gpt-6-astra capability suite.

17. Astra routing/cost/budget:
    GPT6Astra exists in table.rs:109-115 + openai.rs:85 + merge_gateway.rs:21 but pricing None => estimate_session_costs returns None => max_budget_usd skipped in runner/execute.rs:761-799. Duplicate estimate in model_resolver.rs:286-301 vs usage_cost.rs:105-136. Unify on usage_cost, audit models.json for 3 Astra IDs, fail-closed on pricing None when budget set, document raw=enforcement vs effective=display.

===

1. tui/core*tui/session.rs:1-407 — state transitions / layout
   Split across session/state.rs:1-958, impl_init.rs, impl_layout.rs, impl_render.rs, driver.rs, action.rs, config.rs. Gotcha: transcript_area is source of truth for scroll/hit-test, ActivityState is global busy/idle, bottom-half rect shared by overlays + clipping.
   reasoning.effort=xhigh. Map Session lifecycle in tui/core_tui/session.rs + session/state.rs + session/driver.rs + session/action.rs.
   Output: state table (field, mutated where, dirty/redraw path, modal/timeline interaction) + top 5 transition bugs (e.g. mark_dirty missed, overlay vs transcript_area clipping, ActivityState drift). Keep transcript_area as scroll source, no explicit rect to apply_view_rows. Patch surgically, verify with cargo nextest run -p vtcode-ui.
   Decompose Session god-object: propose extract of init/layout/render/scroll/style from impl*\*.rs into small interfaces without breaking task_panel.rs helpers or SessionWidget in tui/ui/tui/widgets/. Keep 4-space, anyhow::Result+context. Show before/after file:line.
2. Transcript rendering + cache
   session/transcript.rs:1-585 (TranscriptReflowCache, revision, invalidate_message), reflow/blocks.rs|formatting.rs|helpers.rs, wrapping.rs, tool_renderer.rs, message_renderer.rs, widgets/transcript.rs. Gotchas: info/warning/error blocks invalidate from first line, each Info summary line is boundary, Alt+T must invalidate caches, PTY • prefix keeps explicit status color, blank line above/below tool blocks, hit regions rebuilt after reflow.
   reasoning.effort=max. Audit TranscriptReflowCache: set_width/invalidate_content/needs_reflow/update_message in session/transcript.rs + reflow/ + wrapping.rs + text_utils.rs hanging-prefix.
   Find: stale revision on grouped reflow, width-change over-invalidation, scroll-anchor loss, PTY live vs complete capture leak, compact review hint hit-region drift. Fix with bounded cache + targeted invalidate_message, add test in session/tests/transcript_rendering.rs + diff_overlay.rs. Measure large-transcript reflow time before/after.
   Repro PTY/scroll bugs end-to-end: compact vs expanded PTY, Ctrl+T Transcript Review (rich/raw via r), drag_autoscroll, overlay_list scroll-through. Use computer-use to drive terminal resize + wheel outside modal_list_area (must pass to transcript). Output failing case as nextest + bounded live lines, complete captures behind viewer.
3. Input ownership
   session/input_manager.rs:1-1234 (TextArea wrapper, max_histories 50), input.rs, impl_input.rs, textarea_bridge.rs, modal/state.rs|render.rs|layout.rs, reverse_search.rs, queue.rs. Gotchas: bridge prompts = deferred-event queue prompt-only, transient overlays own input, slash parsing terminal-only, toggle_tool_display_mode Alt+T before legacy text-edit, Ctrl+C copy-swallow, fullscreen Ctrl+T dual meaning.
   Build input-routing matrix for: normal, popup/modal, approval, reverse-search, fullscreen review, queue_inputs. Trace in input_manager.rs + impl_input.rs + modal/ + reverse_search.rs + queue.rs. Find focus leaks, key swallowing (Alt+T, Ctrl+C/T), mouse ownership outside modal_list_area. Unify to explicit owner enum + guard, add tests in tests/vim.rs,input_navigation.rs,queue_inputs.rs,overlay_list.rs.
4. Async integration
   runner/events.rs:1-278 (TerminalEvent::Tick/Crossterm, EventChannels pause/resume, last_input_elapsed_ms), runner/drive.rs|surface.rs|terminal_io.rs|signal.rs|terminal_modes.rs, session/events.rs|impl_events.rs, panic_hook/. Gotchas: terminal-op lock for render/finalize, alt-screen teardown clears before leave, panic-hook only after successful mutation, active-PTY counter = global loading observer with footer fallback in state.rs.
   reasoning.effort=xhigh. Audit event fan-in: Tick adaptive rate via last_input_elapsed_ms, agent/PTY/redraw coordination in runner/drive.rs + runner/events.rs + session/impl_events.rs. Find: blocking recv, unbounded mpsc queue (clear_queue drain), missed pause/resume, signal teardown race, redraw starvation. Fix with bounded coalesced diagnostics, non-blocking handoff, terminal-operation lock proof. Verify with harness PTY tests: cargo nextest run -p vtcode-core -E 'binary(/pty_tests/)'.
5. Theme + contrast
   theme/registry.rs:52-923, scheme.rs, runtime.rs, color_math.rs, tests.rs:58-80. Requirement: all built-ins WCAG AA 4.5:1, cargo nextest run -p vtcode-ui -E 'test(theme)'. Catppuccin-latte special-case saturating_sub(32).
   For every theme in all_theme_definitions: validate foreground/primary/secondary/user/response vs background via contrast_ratio in theme/tests.rs. Theme changes must propagate to normal/accent/syntax/status/overlay. Fix latte-style failures by darkening, not lightening bg. Add snapshot test in tui/core_tui/widgets/snapshots/ for tool success/failure/warning • prefix + syntax fallback when shell highlight yields no distinct tokens.
   Run order: 1 -> 2 -> 3 -> 4 -> 5. Delegate 2+5 in parallel subagents — no shared files.

===

Engine: gpt-6-astra for execution quality. Target: whole vtcode harness, model-agnostic.
Rules: No if model=="gpt-6-astra" branches. Use ResolvedModel capability (context_window/pricing/supports_reasoning_effort), Provider trait, ModelCatalogEntry. Test on >=3 families (OpenAI + Claude + Gemini/local). Preserve ThreadEvent contract, 4-space, anyhow::Result+context, CompactString, ./scripts/check-dev.sh --changed + cargo nextest run (never cargo test).
User instructions > AGENTS.md > SKILL.md. Bias to reviewable action, delegate parallel subagents, one verifier unless failed.
P1 — Capability-driven budgets, not 1M assumption:
Replace hardcoded 160k cap + 90% compaction rule with effective_budget=min(ResolvedModel::context_window(), context.max_context_tokens, session safety). Fix in compaction/memory_envelope.rs + vtcode-config/src/context.rs + core/agent/runner/task_setup.rs. Log denominator per turn. Test with small (32k dynamic in model_resolver.rs:606) vs 1M models. Verify auto-compact triggers correctly for both.
P2 — Reasoning-effort mapping without fidelity loss:
Audit rig_adapter.rs:81-139 where XHigh/Max->high on some providers + provider_trait.rs:24 capability flag. Introduce central ReasoningEffortMapper: query supports_reasoning_effort, degrade explicitly (Max->XHigh->High) with TurnBlocked/harness diagnostic, never silent. Remove duplicate estimate_cost in model_resolver.rs:286 vs usage_cost.rs. Test matrix all ReasoningEffortLevel x OpenAI/Anthropic/Gemini.
P3 — Prompt cache stable across models:
In core/agent/hash_utils.rs + prompts/system.rs:576-606: prefix hash must include capability digest (model id, reasoning tag, tool catalog epoch) not model name string. Keep ShellProfile/Environment/Harness-Limits frozen per segment. Assert PROMPT_CACHE hit via Usage.cache_hit_rate. Must improve hits for all providers, not just Astra Responses caching.
P4 — Tool guidelines per capability level:
Extend prompts/guidelines.rs:22 generate_tool_guidelines(CapabilityLevel) to emit terse vs verbose variants based on ResolvedModel context + cost, not Astra verbosity. Small-context models get Minimal profile, large get Default. Parallel-call hint only if provider supports parallel tool calls. Snapshot test both.
P5 — Cost + fallback unified:
Unify model_resolver estimate + usage_cost raw=enforcement/effective=display. If pricing None + max_budget_usd set -> fail-closed or explicit allow-unpriced, not silent skip in runner/execute.rs. Fix manual is_pro_variant + hardcoded fallback in capabilities.rs:154 to catalog-driven. Aggregate cost_usd in vtcode-eval report. Test gpt-6-astra via 3 routes + missing-pricing case.
P6 — Safety/sandbox + spool harness-wide:
Defensive only. Fix sh -c join, redirection-blind preflight, env denylist->allowlist, lexical vs resolved TOCTOU, spool substring check, WebMCP CARGO_HOME trust, skill network miss, MCP schema verbatim — at SafetyGateway/registry layer so all models inherit. No model-specific bypass. Add adversarial nextest + pty_tests/pipe_tests.
P7 — TUI complexity (session/transcript/input/async/theme):
In vtcode-ui/src/tui/core_tui/session.rs + transcript.rs:28-62 + input_manager.rs + runner/events.rs + theme/tests.rs:58-80: fix transition table, reflow revision invalidation, input owner enum, Tick/PTY/redraw coalescing, WCAG 4.5:1 for all themes. Must work headless + all terminals, not Astra computer-use only. Verify cargo nextest run -p vtcode-ui -E 'test(theme)' + transcript_rendering + overlay_list tests.
P8 — Eval/memory generalizer:
Fix vtcode-eval/metric.rs true pass@k/pass^k, attempts>=1 guard, parallel run_suite with cost/latency join to trace_analyzer. Fix vtcode-memory eviction->summarize hook + BM25 search + LRU invalidate. Add cross-model regression suite (Astra executes, Claude/Gemini must also pass).
Run order: P3 -> P1/P2/P5 -> P4 -> P6 -> P7 -> P8. Each PR must show check-dev.sh --changed + 3-model evidence.
