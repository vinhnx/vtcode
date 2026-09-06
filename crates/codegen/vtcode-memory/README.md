# vtcode-memory

Unified per-session state store for VT Code — the single source of truth for an
agent session's state, context, and history.

Each session is persisted under `.vtcode/sessions/<session_id>/`:

- `events.jsonl` — the canonical append-only `ThreadEvent` log (schema-versioned
  via `VersionedThreadEvent`). Everything else is derived from this.
- `manifest.json` — session metadata and counters.
- `index/turns.json` — byte-offset index enabling O(1) turn reconstruction.
- `derived/` — regenerated views (`trajectory.jsonl`, `memory.json`, …).

## Design

- **Append-only + off the hot path.** The live conversation stays in memory and
  is never reloaded from disk into context. Reads happen only for revert,
  compaction, analytics, and long-term-learning queries.
- **Single source of truth.** Checkpoints, trajectory metrics, and session
  memory are *derived* from `events.jsonl` rather than persisted independently,
  eliminating the redundant `.vtcode/checkpoints`, `.vtcode/logs`, and
  `.vtcode/history` stores.
- **Bounded growth.** `apply_retention` evicts the oldest/stale sessions so disk
  overhead does not accumulate across a long-lived agent. Event-cap eviction
  persists a derived summary before replacing the canonical log and retains
  events when that summary cannot be persisted.
- **Deterministic search.** Cross-session memory search ranks tokenized facts with
  BM25 (`k1=1.2`, `b=0.75`) and invalidates its manifest LRU after atomic writes.
- **Crash-safe + private.** Event compaction and metadata writes are atomic;
  session directories and artifacts use private permissions.

## Usage

```rust
use vtcode_memory::{open, migrate_legacy, apply_retention, query_facts};

// Record a session's events (call from the runloop's event sink).
let log = open(workspace, session_id)?;
log.append(&event)?;
let turn = log.reconstruct_turn(3)?; // derived view, never into context

// One-off migration of the legacy overlapping stores.
let report = migrate_legacy(workspace, /* remove_legacy */ false)?;

// Bound growth and learn across sessions.
apply_retention(workspace, Default::default())?;
let facts = query_facts(workspace, 100)?;
```

## Modules

- `event_log` — append-only log, turn index, manifest.
- `migration` — import legacy history/trajectory stores.
- `retention` — retention policy + garbage collection.
- `query` — cross-session analytics and long-term-learning queries.
