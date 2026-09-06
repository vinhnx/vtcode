#![allow(
    dead_code,
    unused_imports,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
#![expect(
    unused_results,
    clippy::let_underscore_must_use,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::string_slice,
    reason = "The memory store uses compact persisted counters, bounded timestamp conversions, and side-effect-only index maintenance."
)]
//! Unified per-session state store for VT Code.
//!
//! This crate is the single source of truth for an agent session's state,
//! context, and history. Each session is persisted under
//! `.vtcode/sessions/<session_id>/` as:
//!
//! - `events.jsonl` — the canonical append-only [`ThreadEvent`](vtcode_exec_events::ThreadEvent)
//!   log (schema-versioned). Everything else is derived from this.
//! - `manifest.json` — session metadata and counters.
//! - `index/turns.json` — byte-offset index enabling O(1) turn reconstruction.
//! - `derived/` — regenerated views (`trajectory.jsonl`, `memory.json`, …).
//!
//! The store is intentionally append-only and off the agent's hot path: the
//! live conversation stays in memory and is never reloaded from disk into
//! context. Reads happen only for revert, compaction, analytics, and
//! long-term-learning queries.

pub mod error;
pub mod event_log;
/// Manifest and turn-index persistence helpers.
pub mod manifest;
pub mod migration;
pub mod progress;
pub mod query;
pub mod retention;

pub use error::SessionStoreError;
pub use event_log::{
    DEFAULT_MAX_EVENTS, EvictionSummaryHook, SessionEventLog, SessionManifest, TurnIndex, TurnIndexEntry,
};
pub use migration::{MigrationReport, migrate_legacy};
pub use progress::{
    GoalClassifierVerdict, GoalEvent, GoalHistoryEntry, GoalOrchestration, GoalPauseReason, GoalPhase, GoalStatus,
    GoalTracker, Milestone, MilestoneStatus, ProgressLedger, load_progress, progress_path, save_progress,
};
pub use query::{FactRecord, MemorySearchResult, SessionSummary, query_facts, recent_sessions, search_memory};
pub use retention::{RetentionPolicy, apply_retention, apply_retention_preserving, gc_legacy};

use std::path::{Path, PathBuf};

/// Directory (relative to the workspace) holding all per-session stores.
const SESSIONS_DIR: &str = ".vtcode/sessions";

/// Sub-directory inside a session holding regenerated views.
const DERIVED_DIR: &str = "derived";

/// Schema version for the on-disk session store layout.
const SESSION_STORE_SCHEMA_VERSION: u32 = 1;

/// Resolve the sessions root directory for a workspace.
#[must_use]
pub(crate) fn sessions_root(workspace: &Path) -> PathBuf {
    workspace.join(SESSIONS_DIR)
}

/// Resolve the directory for a single session.
#[must_use]
pub(crate) fn session_dir(workspace: &Path, session_id: &str) -> PathBuf {
    sessions_root(workspace).join(sanitize_id(session_id))
}

/// Return the canonical directory for a session.
///
/// Derived exporters and diagnostics must live beneath this directory so the
/// session store remains the single persistence root for interactive and exec
/// sessions.
#[must_use]
pub fn session_directory(workspace: &Path, session_id: &str) -> PathBuf {
    session_dir(workspace, session_id)
}

/// Open (creating if necessary) the event log for a session.
///
/// This is the canonical entry point for recording a session's events. Multiple
/// handles opened for the same session share an `Arc`-backed file and state,
/// allowing concurrent `append` calls from the runloop's event sink to use one
/// coordinated turn index.
pub fn open(workspace: &Path, session_id: &str, max_events: usize) -> Result<SessionEventLog, SessionStoreError> {
    SessionEventLog::open(workspace, session_id, max_events)
}

/// Open a session log with a callback that persists summaries before cap
/// eviction. A callback failure leaves the canonical event log unchanged.
pub fn open_with_eviction_summary(
    workspace: &Path,
    session_id: &str,
    max_events: usize,
    eviction_summary_hook: EvictionSummaryHook,
) -> Result<SessionEventLog, SessionStoreError> {
    SessionEventLog::open_with_eviction_summary(workspace, session_id, max_events, eviction_summary_hook)
}

/// Ensure a session-store directory exists with private permissions.
pub(crate) fn ensure_private_directory(path: &Path) -> Result<(), SessionStoreError> {
    vtcode_commons::VtCodePaths::ensure_user_dir(path).map_err(|error| SessionStoreError::CreateDir {
        path: path.to_path_buf(),
        source: std::io::Error::other(error),
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| SessionStoreError::io(path.to_path_buf(), error))?;
    }

    Ok(())
}

/// Sanitize a session id so it is safe to use as a directory name.
fn sanitize_id(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    for c in id.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    // Strip leading dots to avoid creating hidden directories.
    let out = out.trim_start_matches('.').to_string();
    if out.is_empty() { "session".to_string() } else { out }
}

#[cfg(test)]
mod tests;
