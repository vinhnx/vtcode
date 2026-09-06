# vtcode-safety

[Root AGENTS.md](../AGENTS.md) | Command safety detection, execution policies, and sandboxing. Layer 1 crate — depends on vtcode-commons.

## Module Groups

| Area | Modules |
|---|---|
| Command Safety | `command_safety/` — dangerous command detection, shell parsing |
| Execution Policy | `exec_policy/` — policy management, approval workflows, command validation |
| Sandboxing | `sandboxing/` — sandbox policy, permissions, execution environments |

## Rules

- `exec_policy::manager` imports `command_safety::command_might_be_dangerous` and `sandboxing::SandboxPolicy` — these form a tightly coupled safety subsystem.
- Re-export facades in vtcode-core (`command_safety/mod.rs`, `exec_policy/mod.rs`, `sandboxing/mod.rs`) must stay in sync.
- `SandboxPermissions::normalized_for` is the canonical boundary for additional-permission mode normalization; callers must not duplicate that rule.
- The `BashParser` singleton (`once_cell::Lazy`) is safe across crates — read-after-init pattern.

## Gotchas

- `exec_policy/parser.rs` imports `vtcode_commons::fs::{parse_json_with_context, read_file_with_context}`; `command_validation.rs` imports `paths::{canonicalize_workspace, normalize_path}` and delegates workspace containment to `ensure_path_within_workspace_resolved` (symlink-aware walk lives in commons, tests included).
- `sandboxing/` uses tree-sitter for Bash AST analysis — pinned to specific versions.
- `command_safety::shell_parser` must extract nested simple commands from loops/conditionals so safety checks and approval caching see loop bodies, not just top-level shell syntax; preserve raw and ANSI-quoted arguments in that extraction.
- `command_safety::shell_parser` owns dynamic-shell-syntax detection; `find` expansion must fail closed before preflight or learned approval. Static shell classification permits literal escapes inside double-quoted arguments (e.g., `rg` regexes) but rejects unquoted escapes; keep the scanner quote-aware.
- Sandboxed pipe/PTY and MCP stdio launches rebuild allowlisted env vars after overrides; macOS hostname allowlists reject unenforceable policies, Windows restrictions fail closed, and `SensitivePath` matching is case-insensitive with component-boundary semantics.
- Keep `exec_policy_command_validation` fuzzing and traversal/symlink regression cases aligned with workspace containment or command validation changes.
- Windows-only `command_safety` DB builders (`windows_cmdlet_db.rs`, `windows_com_analyzer.rs`, `windows_registry_filter.rs`) use module-level `#![expect(unused_results)]` — one-shot builders deliberately discard `insert` results; cross-check with `cargo check --target x86_64-pc-windows-msvc` since Linux CI never compiles `#[cfg(windows)]` code.
