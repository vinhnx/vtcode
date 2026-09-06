# VT Code Execution Policy

This document describes what commands and operations the VT Code agent can execute without prompting, and which require confirmation.

## Summary

The execution policy is designed to allow engineers typical software development workflows while maintaining safety boundaries. Common development tools work automatically, while dangerous operations require confirmation.

## Auto-Allowed Commands

### Version Control (Git)
- **Read operations**: `status`, `log`, `show`, `diff`, `branch`, `tag`, `remote`
- **Tree inspection**: `ls-tree`, `ls-files`, `cat-file`, `rev-parse`, `describe`
- **Additional inspection**: `blame`, `grep`, `shortlog`, `format-patch`
- **Safe writes**: `add`, `commit`, `reset`, `checkout`, `switch`, `restore`, `merge`, `stash` (select ops)
- **Blocked**: `push --force`, `clean`, `rebase`, `cherry-pick`, `filter-branch`

### Build Tools (Cargo)
- **Safe operations**: `build`, `check`, `test`, `doc`, `clippy`, `fmt`, `run`, `bench`
- **Additional safe**: `tree`, `metadata`, `search`, `cache`, `expand`
- **Blocked**: `clean`, `install`, `uninstall`, `publish`, `yank`

### Languages
- **Python** (`python`, `python3`): Script execution, module runs with `-m`
- **Node.js** (`node`): Script execution
- **NPM** (`npm`): Install, test, build, run, start, list, view, search
- **Blocked for NPM**: `publish`, `unpublish`

### File Operations
- **Read**: `cat`, `head`, `tail`, `ls`, `grep`, `find`, `rg` (ripgrep)
- **Write**: `sed` (with workspace validation)
- **Copy**: `cp` (with workspace validation)
- **Count**: `wc`

### System Info
- `pwd`, `whoami`, `hostname`, `uname`, `date`, `echo`, `which`, `printenv`

## Tool Policies

Canonical public tool names in the default profile are `exec_command`,
`write_stdin`, and `apply_patch`. The advanced VT Code profile may also expose
`code_search` for bounded code search. Plain text search and file inspection
use shell commands inside `exec_command.cmd`.

| Tool | Policy | Notes |
|------|--------|-------|
| `exec_command` | **Prompt** | Public shell command surface |
| `write_stdin` | **Prompt** | Live-session continuation surface |
| `apply_patch` | **Prompt** | Patch edits |
| `code_search` | Allow | Advanced-profile bounded code search |
| `request_user_input` | Allow | Interactive clarification surface |

### Recovery After Policy Denial

When `exec_command` is denied, treat that denial as a routing signal, not a
retryable transient:

1. Do not repeat the same shell inspection.
2. If the task needs syntax-aware search and the advanced profile is available, use `code_search`.
3. If the task still requires shell access, state that the change was **not applied** and recommend a user-approved mode change such as `/mode auto`.

Example pivot:

- Denied: `exec_command` running `rg -n "foo" README.md`
- Recovery for a focused code query: advanced `code_search` with `query` and narrower filters
- Final fallback when shell access remains necessary: `Not applied; exec was denied by policy; next action: switch to /mode auto or approve the command.`

## Key Safety Features

1. **Workspace Boundary**: All file operations are confined to `WORKSPACE_DIR`; cannot escape or touch system files
2. **Command Whitelisting**: Only specific commands are allowed; unknown commands are blocked
3. **Argument Validation**: Common flags are validated (e.g., git force-push is blocked)
4. **Confirmation Required**: Destructive operations still require user confirmation
5. **Two-Layer Control**: 
   - Tool-level: Which tools can be used
   - Command-level: Which specific commands and flags are permitted

## Dangerous Operations (Blocked)

- `rm -rf` (recursively remove)
- `sudo` (privilege escalation)
- `kubectl` (Kubernetes operations)
- `chmod`, `chown` (permission changes)
- Git force-push, clean, rebase, cherry-pick
- Cargo install, publish, clean
- NPM publish, unpublish
- File deletion (`delete_file` requires confirmation)
- Apply complex patches (`apply_patch` requires confirmation)

## Use Cases

### Typical Engineer Workflow
```
 git status, git diff, git log, git checkout
 cargo test, cargo build, cargo check
 npm install, npm test, npm run build
 python scripts/setup.py
 Editing files, reading logs, viewing diffs
```

### Blocked Without Confirmation
```
 Deleting files (requires confirmation)
 Applying complex patches (requires confirmation)
 Git force-push or history rewrites
 Publishing to registries
```

## Configuration

Policies are defined in:
- **Core defaults**: `crates/codegen/vtcode-config/src/core/tools.rs` (tool policies)
- **Command validation**: `crates/codegen/vtcode-core/src/exec_policy/mod.rs` (command whitelisting)
- **User overrides**: `vtcode.toml` in project root or the canonical user config directory

Override examples in `vtcode.toml`:
```toml
[tools.policies]
apply_patch = "allow"  # Allow patches without prompt
exec_command = "ask"    # Prompt before shell commands
write_stdin = "ask"     # Prompt before live-session input
code_search = "allow"   # Allow advanced bounded literal search
```

## Cache-Friendly Execution Guidance

Token efficiency is a correctness concern, not just a cost concern: every token of
harness payload is context the model cannot spend on the task. The defaults below
are designed to keep the first-request overhead low and per-turn growth bounded.

### Defaults that keep the prefix small

- **MCP tools defer by default.** Any MCP tool in the catalog is flagged
  `defer_loading` rather than sent eagerly, regardless of tool count. MCP schemas
  are the dominant source of token inflation; the model discovers them on demand.
- **Client-local deferral is the default.** Providers without a hosted tool search
  (e.g. Gemini) omit deferred schemas from the wire payload and append a compact,
  cache-stable discoverability summary to the system prompt. Set
  `tools.client_tool_search = false` to opt back into the eager catalog.
- **Subagents use a lightweight profile.** A delegated child agent defaults to
  `system_prompt_mode = minimal` and `tool_documentation_mode = minimal`, and does
  not inherit the parent's MCP servers unless explicitly requested. This prevents
  replaying the full parent bootstrap on every child turn.
- **Tool-result clearing is on by default.** Old tool results are stripped from
  context once it grows past `trigger_tokens` (default 100k), keeping only the most
  recent `keep_tool_uses` (default 3) results.
- **Builtin tool count is capped.** The number of LLM-exposed builtin tools stays
  within a small cap; new tools must consolidate, defer, or deliberately raise the
  cap. Builtin tool schemas in `progressive` mode fit in a ~3k-token envelope.
- **Startup token-overhead warnings.** At session start (unless `--quiet`),
  VT Code logs non-fatal `tracing::warn!` messages when the config is likely to
  inflate per-request cost: more than 8 configured MCP servers,
  `tools.client_tool_search = false` while MCP servers are configured,
  `system_prompt_mode = "specialized"`, `tool_documentation_mode = "full"`,
  `tool_result_clearing` disabled, `auto_compaction_enabled` disabled, or
  `max_system_prompt_tokens` set very low (< 4000). The actual composed system
  prompt is also measured at startup and flagged if it exceeds the configured
  budget. These surface the "what am I actually sending" question before you pay
  for it.

### Authoring guidance

- Keep instruction files (AGENTS.md / CLAUDE.md) focused; they ride on every request.
- Put universal user-facing behavior in the compiled runtime-guidance section; keep authored instruction files for project-specific maps and maintainer workflows. Authored files are context, not a security boundary.
- Prefer delegating large searches to subagents with a narrow, explicit tool set
  rather than fanning out broad orchestration.
- When adding a tool, keep its description between 40 and 1200 characters and include
  a verb cue and any side-effect/constraint cue so the model selects it accurately
  without padding the prompt.
- Pin the cache breakpoint: keep the system prefix stable across turns. Dynamic
  per-turn content (timestamps, volatile workspace state) belongs in trailing
  sections, not the cached prefix.

### Auditing token cost

A first-request budget guard rail is enforced by tests:

- `crates/codegen/vtcode-core/src/tools/registry/builtins.rs::emitted_model_tool_schema_fits_within_first_request_budget`
  asserts builtin tool schemas stay within the budget in `progressive` mode.
- `crates/codegen/vtcode-core/src/tools/handlers/session_tool_catalog.rs` tests assert MCP tools
  defer (small or large catalog) and that the client-local policy defers small MCP
  catalogs.

Run them with:

```bash
cargo nextest run -p vtcode-core emitted_model_tool_schema_fits_within_first_request_budget
cargo nextest run -p vtcode-core -E 'test(session_tool_catalog)'
```

### Model pricing and fallback policy

`vtcode-llm::usage_cost` owns normalized provider usage and the shared cost calculation.
Raw cost prices all input tokens at the input rate and is used for USD budget enforcement;
cache-aware effective cost is for display. Each turn is priced under the route that
served it and added to the session total; switching to a cheaper model never reprices
earlier spend. A turn without usage or pricing leaves the complete total unknown. Cache creation can make effective cost higher
than raw cost. Missing, negative, or non-finite pricing is not a zero-price estimate.
A configured USD budget blocks an unpriced route before inference or automatic compaction,
with a `turn.blocked` diagnostic and recoverable failed outcome. Explicitly removing the
USD budget permits an unpriced route; the reported cost remains unknown.

Model capability tier and lightweight fallback come from `docs/models.json` fields
`is_pro` and `lightweight_model`. Fallback stays within the selected provider and never
returns the same model. Missing metadata gives no tier assertion or speculative fallback.
Legacy built-in variants without matching catalog rows currently include
`CopilotGPT52Codex`, `CopilotGPT54`, `CopilotClaudeSonnet46`, `EvolinkDeepseekV4Pro`,
`MoonshotKimiK3`, `MoonshotKimiK27Code`, `PoolsideLagunaM1`, and `PoolsideLagunaS21`.
These need explicit catalog metadata before they can participate in automatic fallback.
Pricing is route-specific: OpenAI and OpenRouter Astra catalog routes are priced;
Merge Gateway's Astra route currently has no pricing and therefore blocks with a USD cap.

Deterministic cross-family normalization and route-pricing checks run without paid calls:
`cargo nextest run -p vtcode-llm -E 'test(usage_cost)'`.

### Terminal input and redraw ownership

The core TUI derives a single input owner: overlay, composer, or runtime. Activity
changes while an overlay is open update the state restored on close; busy runtime
states cannot accidentally re-enable the composer. Transcript cache validity is
explicit, so revision zero and resize invalidation remain distinct.

Terminal redraw ticks coalesce to one pending notification. Keyboard events remain
ordered and PTY data continues through its existing ordered command channel; redraw
coalescing never drops output bytes. Built-in theme checks cover every semantic
foreground against its background at WCAG AA 4.5:1, including inherited tool output.
Reasoning uses italics without terminal dimming, whose contrast varies by terminal.
