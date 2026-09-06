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

=====

More prompts (8-17) 8. Cache-stability + token-bloat fix:
reasoning.effort=xhigh. Audit prompt caching: stable_system_prefix_hash in core/agent/hash_utils.rs:39-55 strips Active-Tools/Catalog/Context but not [Harness Limits] or ## Environment/Shell Profile. cache_key in prompts/system.rs:576-606 omits prompt_context. system_prompt_budget in system.rs:305-462 is warn-only trim-off by default, estimator len/4. Fix to keep PROMPT_CACHE hits, include digests in envelope instruction_digest, enable safe trim, add hit-rate assert via Usage.cache_hit_rate. Verify: cargo nextest run -p vtcode-core prompts::

1. Compaction vs reset unification:
   Compaction preserves+new segment (compaction_checkpoint.rs:25-130, compaction/mod.rs:21-43) vs context_reset.rs discards to .vtcode/tasks/current_context_reset.md. Session clear wipes all in session/mod.rs:487-505. Auto trigger 90% min(provider,session) in compaction/memory_envelope.rs:1338-1349. Unify to single manifest referencing ThreadCompactBoundary + ContextReset events, test stall->reset->orient roundtrip. Keep ThreadEvent schema 0.14.0 compatible.

2. Parallel fan-out:
   can_parallelize=readonly&&preflight in tool_batching.rs:81-83 over-serializes; duplicate exec_command forced sequential; guidance is one line in guidelines.rs:80-82. Widen parallel_safe_after_preflight for pure reads (read_file/batch.rs), add few-shot parallel example, surface fan-out in telemetry. Verify max_parallel_tool_calls honored in tool_exec.rs:417,750.

3. Failure taxonomy collapse:
   Unify harness_kernel.ExecutionFailure, cargo_failure_diagnostics, Reasoning stage diagnosis, ErrorRecoveryState+circuit, HarnessEventKind::ToolRetry/ErrorRecovered into ErrorCategory+ToolOutcome path. Always emit HarnessEventItem{attempt,error_category,duration_ms}. No new ThreadEvent variants.

4. Spool hardening:
   Spool 8192B in output_spooler.rs:34 vs 32KiB/turn budget vs OUTPUT_PREVIEW_CHARS_PER_TOKEN=4. Reducers only cover read_file/unified_exec. Anti-recursion depends on caller no_spool, is_tool_output_spool_path is substring match. Enforce no_spool at gateway via canonical containment, append-only+digest reads via SpooledOutputReference only, add recursion/poisoning tests.

5. Shell injection + redirection blindspot:
   reasoning.effort=max, defensive only. executor.rs:166-169 sh -c + shell_handler.rs:86-92 join(" ") + preflight skip(1) in sandbox_runtime.rs:674-729 misses > ~/.ssh/authorized_keys, python -c, $BIN/curl, sudo unwrap. Enforce argv-only exec unless validate_command_safety+redirection-aware preflight pass, deny > to sensitive, expand interpreter -c/-e list. Add regression tests, run pty_tests+pipe_tests.

6. Env leakage + containment TOCTOU:
   manager.rs:56-62 uses denylist filter_sensitive_env not allowlist build_sanitized_env; PYTHONPATH/NODE_PATH/RUSTFLAGS/NODE_OPTIONS/GIT_SSH_COMMAND leak. WorkspaceGuardPolicy lexical vs ensure_path_within_workspace_resolved, skill_additional_permissions normalize only. Switch restrictive to allowlist, scope PYTHONPATH to workspace, fail-closed canonicalize, add symlink-swap harness.

7. Skill/MCP trust:
   NETWORK_TOOLS misses exec/shell egress in skill_policy.rs:14-44, silent UseDefault->WithAdditional merge, SKILL.md unfenced, SkillToolScope checked once. MCP parse_mcp_tool verbatim no size/name cap. Treat shell as network unless BlockAll, require human approval for out-of-workspace paths, fence skill/MCP descriptions + injection probe, cap schema bytes, namespace names, rate-limit list_changed.

8. Eval + memory fix:
   metric.rs:13-25 fake pass@k, suite attempts:0 deserializes, executor sequential cost-blind, search_memory substring count, eviction truncates without summarize. Implement true pass@k/pass^k, attempts>=1 guard, parallel run_suite with cost_usd aggregation, wire eviction->grounded_facts summarizer, BM25+recency search. Add gpt-6-astra capability suite.

9. Astra routing/cost/budget:
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

Refined plan (locked decisions applied) 0. Goal + guardrails
Engine gpt-6-astra, whole harness model-agnostic. No if model=="..."; only ResolvedModel::context_window()/pricing()/reasoning_supported(), Provider trait, ModelCatalogEntry.

- ThreadEvent 0.14.0 frozen — no new variants; unify via manifest referencing thread.compact_boundary, context.reset, turn.blocked.
- Style: 4-space, anyhow::Result+with_context, CompactString, surgical diffs, no new top-level harness subsystem.
- Verify each PR: ./scripts/check-dev.sh --changed + cargo nextest run (never cargo test) + 3-family log (OpenAI + Claude + Gemini/local).
- Execution: parallel subagents per workstream, one verifier unless failed.
  Locked: 7 findings → 7 small issues; scope = docs + High fixes; P1 160k default; P5 fail-closed + allow_unpriced=false; P8 run_suite stays sequential.

1. File 7 small issues (no code)
   Restore deleted diagnosis as tracked issues: cold cache, preview_budget_exhausted+spool_path:null, completion_state:unknown on exit-0, 19k/44k visible/spooled gap, low_signal_tool_calls:0 on 3×find, elapsed_ms:null, files:[].
2. P3 cache + P1 budget denominator (first, parallelizable)

- core/agent/hash_utils.rs:19-62,229-251: keep PromptCapabilityIdentity; replace raw model string with capability digest (model_id canonical + ResolvedModel::context_window + reasoning_tag + catalog_epoch + parallel/cache/tools bools) via FNV-1a hash_value; strip [Harness Limits]/Environment/ShellProfile from stable prefix or freeze per segment (request_envelope.rs:12-21,45-108, state.rs:206-282, request_builder.rs:230-253).
- Fix prompts/system.rs:576-609 cache_key: DefaultHasher → StableHasher, require explicit epoch, include digest.
- P1: default context.max_context_tokens=160k (align context.rs:196-198 0 with constants/tool_limits + docs agent-loop-contract.md:204); effective_budget=min(provider, session, safety) in memory_envelope.rs:1276-1362, task_setup.rs:131-135, execute.rs:509-557; log denominator/turn; 90% of min() threshold.
- Tests: same-prompt × 3 families → same stable prefix, distinct digest; Usage::cache_hit_rate (commons/llm.rs:72-84) hit on 2nd turn; 32k vs 1M auto-compact (turn/compaction/tests.rs:2078-2145).

3. P2 effort + P5 cost (parallel subagents)

- P2: reuse reasoning_effort.rs:20-99 ReasoningEffortMapper; refactor rig_adapter.rs:25-144 to query provider_trait.rs:24 supports_reasoning_effort + supported_reasoning_efforts:187-195; degrade Max→XHigh→High with turn.blocked diagnostic, honor allow_reasoning_effort_downgrade=false (agent.rs:99-101). Matrix test all levels × OpenAI/Anthropic/Gemini.
- P5: keep usage_cost.rs:55-102 canonical (raw=enforcement, effective=display); execute.rs:789-840: pricing None + max_budget set + !allow_unpriced → TurnBlocked, else explicit warn; replace defaults.rs:7-17, openai/errors.rs:72-78, lightweight_routing.rs:179-201, orchestrator_retry.rs:150-179 hardcodes with catalog-driven (preferred_lightweight_variant, non_reasoning_variant); aggregate cost_usd in eval/task.rs:49, metric.rs, report.rs.

4. P4 guidelines (small)
   prompts/guidelines.rs:22-94: add overload generate_tool_guidelines(level, ResolvedModel); terse Minimal for small/high-cost, Default for large; parallel hint only if supports_parallel_tool_config. Snapshot both. Files: guidelines.rs, system.rs:319-385, harness_limits.rs:14-50.
5. P6 safety gateway (High fixes)
   All at tools/safety_gateway.rs:197-690 + registry/:

- Spool: output_spooler.rs:34-41,191, spool_processing.rs:43-63, file_ops/read.rs:253-256, tool_reads.rs:28-31 substring → canonical containment, no_spool enforced at gateway, readers via SpooledOutputReference only.
- Env: safety/sandboxing/manager.rs:56-62, exec_session.rs:502-506, child_spawn.rs:15-212 denylist → build_sanitized_env allowlist in restrictive.
- Paths: commons/paths.rs:202-287 resolved variant into bash-runner/policy.rs:36-42, skill_policy.rs:257-278, command_validation.rs:1015.
- Shell: bash-runner/executor.rs:164-169, runner.rs:435-437, shell_handler.rs:86-87 join(" ") → argv-only; wire shell_parser.rs:157-264 redirection + sudo/$BIN/curl/python -c into sandbox_runtime.rs:674-843 preflight.
- WebMCP filesystem.rs:550-561,1033-1118 pin system cargo; skills skill_policy.rs:15-63 expand NETWORK_TOOLS; MCP mcp_tool.rs:12-18, mcp/provider.rs:561-575 size cap + namespace + list_changed limit.
- Tests: adversarial nextest + extend pty_tests.rs, pipe_tests.rs:1-276.

6. P7 TUI targeted + P8 eval/memory (last)

- P7: keep facade (session.rs:30-80 + 20 submodules); fix transition table (activity.rs:4-60), reflow invalidation (transcript.rs:28-311, state.rs:139-147,477-512), introduce InputOwner enum (replace input_enabled bools), keep Tick/PTY coalescing (events.rs:12-211, drive.rs:199-512), WCAG 4.5:1 (theme/tests.rs:58-125). Verify nextest -p vtcode-ui -E 'test(theme)' + transcript_rendering + overlay_list.
- P8: true pass@k=1-C(n-c,k)/C(n,k), pass^k=(c/n)^k + attempts>=1 guard in suite.rs; keep executor.rs:33-44 sequential, parallelize at caller with cost/latency join to trace_analyzer/; eviction→summarize hook, BM25 replace substring (query.rs:141-227), LRU invalidate (query.rs:11-14); cross-model regression suite (Astra executes, Claude/Gemini pass).
