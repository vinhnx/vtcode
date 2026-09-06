# vtcode-bash-runner

[Root AGENTS.md](../AGENTS.md) | Cross-platform command runner with workspace-safe operations.

## Modules

`executor` CommandExecutor trait + backends | `runner` BashRunner | `policy` CommandPolicy + WorkspaceGuardPolicy | `pipe` async process spawning | `process` handles | `process_group` kill/cleanup | `background` long-running tasks | `stream` utilities

## Rules

- `CommandExecutor` trait = primary abstraction for new backends.
- `CommandPolicy` trait = execution gate. `WorkspaceGuardPolicy` enforces boundaries.
- Feature flags: `dry-run`, `pure-rust`, `exec-events`, `serde-errors`.
- `process_group` uses safe `nix` wrappers (`Pid::from_raw`, `signal::killpg`, `setpgid` in `pre_exec`); there is no `unsafe` here. Unsafe env mutation is centralized in `vtcode-commons::env_lock`, serialized by a process-wide mutex.

## Testing

`cargo nextest run -p vtcode-bash-runner` | pipe tests: `cargo nextest run -p vtcode-bash-runner -E 'binary(/pipe_tests/)'` | use `AllowAllPolicy` unless testing policy.

## Gotchas

- `BashRunner::new()` canonicalizes root — bails if missing.
- Authorization resolves paths freshly; never cache symlink targets across operations. OS sandboxing or bound filesystem handles remain necessary against concurrent replacement.
- Unsafe env mutation (`set_var`/`remove_var`) is centralized in `vtcode-commons::env_lock`, serialized by a process-wide mutex, single-threaded startup only.
- `policy` containment delegates to `vtcode_commons::paths::ensure_path_within_workspace` — `..`-traversal paths are rejected (intentionally stricter than the old `starts_with`).
- Pipe spooling opts into `SpawnedProcess::reliable_output_rx`, a bounded lossless stream; legacy broadcast subscribers must remain independent of that backpressure path.
- `wait_with_output` bounds post-exit draining even when a descendant inherits the pipe; never turn that drain back into an unbounded wait.
