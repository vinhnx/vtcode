# vtcode-memory

[Root AGENTS.md](../../../AGENTS.md) | Per-session event log and derived state.

## Conventions

- `events.jsonl` is canonical; `derived/` and `index/` are regenerated views — never persist session history elsewhere.
- `progress.rs` hosts the `GoalTracker` state machine and compaction-safe `ProgressLedger` view.
- Append-only: do not mutate historical events; new facts go through `append`.
- Off the hot path: never read the log back into agent context; use derived queries for revert/compaction/analytics.
- Public API uses `anyhow::Result<T>` + `.context()`; no `unwrap`/`expect` in non-test code.
- Keep ordinary appends buffered; flush at turn boundaries, reads, cap rewrites, and close.
- Return persistence errors to callers; do not silently discard cap-enforcement failures.
- Retention may remove only validated direct child session directories; preserve active manifests and reject manifest-controlled paths or symlink entries.
- `event_log.rs`: turn-lifecycle state machine is `LogState::apply_lifecycle_event` (single impl shared by `append` and `scan` via `LifecycleKind`). Serialization+rollback is `LogState::serialize_event`. Cap eviction planning is `LogState::plan_cap_eviction` (I/O stays in `enforce_event_cap`). Do not duplicate these state transitions inline.
- Session event bytes are synced before derived metadata; publish the turn index before the manifest, leave the pending cap-rewrite marker until both are durable, and rescan when metadata is malformed, inconsistent, or offsets exceed the canonical log.
- Session directories are `0700` and session files are `0600`; preserve the symlink-safe `vtcode-commons` filesystem primitives.
- `query::search_memory` uses BM25 (`k1=1.2`, `b=0.75`) with deterministic chunk-id ties; invalidate the manifest LRU when atomic manifests change.
- Cap eviction invokes its summary hook before replacing `events.jsonl`; a failed summary keeps the canonical events intact.

## Dependencies

- `vtcode-commons` owns symlink-safe private directories/files and atomic writes.
- `vtcode-exec-events` owns the `ThreadEvent` / `VersionedThreadEvent` contract; never reinvent event types.
- `walkdir` handles directory-size and GC walks; `chrono`, `serde`, and `serde_json` handle persistence metadata.
- `uuid` supports verifier-id generation for the goal tracker.

## Testing

- Use `cargo nextest run -p vtcode-memory` and cover ordering, reopen/index reconstruction, retention, and write boundaries.
- Index rebuild reads the versioned envelope and `event.type`, with targeted full-shape validation for lifecycle events so malformed records cannot create phantom turns; broader decoding belongs to turn reconstruction.
