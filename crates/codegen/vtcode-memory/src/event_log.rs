//! Append-only per-session `ThreadEvent` log plus index and manifest.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::File;
use std::io::{BufRead, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use vtcode_commons::VtCodePaths;
use vtcode_exec_events::{EVENT_SCHEMA_VERSION, ThreadEvent, VersionedThreadEvent};

use crate::error::SessionStoreError;
use crate::manifest::{ManifestStore, PendingCapRewrite};
use crate::session_dir;

/// Default maximum number of events retained per session before the oldest
/// completed turns are evicted.
pub const DEFAULT_MAX_EVENTS: usize = 10_000;

/// Maximum serialized event bytes retained before an append forces a write.
/// Turn boundaries and reads still flush immediately.
const MAX_WRITE_BUFFER_BYTES: usize = 64 * 1024;

/// Callback used to persist a summary of events before they are evicted.
///
/// The callback runs after the event bytes have been flushed and decoded, but
/// before the canonical log is rewritten. A failure leaves the original log
/// and in-memory index untouched, so retention never silently discards
/// history.
pub type EvictionSummaryHook = Arc<dyn Fn(&[ThreadEvent]) -> Result<(), SessionStoreError> + Send + Sync>;

/// Minimal envelope used while rebuilding the turn index.
///
/// The index only needs the event discriminator. Deserializing a complete
/// [`VersionedThreadEvent`] here would allocate every nested tool argument,
/// output, and thread item even though none of that payload is retained.
#[derive(Debug, Deserialize)]
struct VersionedEventKind<'a> {
    #[serde(rename = "schema_version", borrow)]
    _schema_version: &'a str,
    #[serde(borrow)]
    event: EventKind<'a>,
}

#[derive(Debug, Deserialize)]
struct EventKind<'a> {
    #[serde(rename = "type", borrow)]
    kind: &'a str,
}

/// Zero-clone serialization envelope for `ThreadEvent`.
///
/// Produces JSON byte-identical to `VersionedThreadEvent` but borrows the
/// event by reference instead of cloning it. `append` is called for every
/// runtime event, and `ThreadEvent` can carry large tool outputs / thread
/// items — cloning just to feed `serde_json::to_string` was pure waste.
#[derive(Serialize)]
struct BorrowedVersionedEvent<'a> {
    schema_version: &'a str,
    event: &'a ThreadEvent,
}

/// Turn-lifecycle discriminator extracted from either a `ThreadEvent` (at
/// append time) or a raw `&str` kind (during scan).  This is the single
/// representation that both code paths feed into
/// [`LogState::apply_lifecycle_event`], eliminating a duplicated state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleKind {
    ThreadStarted,
    ThreadCompleted,
    TurnStarted,
    TurnCompleted,
    TurnFailed,
    Other,
}

impl LifecycleKind {
    /// Discriminate from a runtime `ThreadEvent` at append time.
    #[inline]
    fn from_event(event: &ThreadEvent) -> Self {
        match event {
            ThreadEvent::ThreadStarted(_) => Self::ThreadStarted,
            ThreadEvent::ThreadCompleted(_) => Self::ThreadCompleted,
            ThreadEvent::TurnStarted(_) => Self::TurnStarted,
            ThreadEvent::TurnCompleted(_) => Self::TurnCompleted,
            ThreadEvent::TurnFailed(_) => Self::TurnFailed,
            _ => Self::Other,
        }
    }

    /// Discriminate from a raw event-type string at scan time.
    #[inline]
    fn from_kind(kind: &str) -> Self {
        match kind {
            "thread.started" => Self::ThreadStarted,
            "thread.completed" => Self::ThreadCompleted,
            "turn.started" => Self::TurnStarted,
            "turn.completed" => Self::TurnCompleted,
            "turn.failed" => Self::TurnFailed,
            _ => Self::Other,
        }
    }
}

/// In-memory state protected by a mutex (cheap; appends are infrequent relative
/// to model inference).
struct LogState {
    manifest: SessionManifest,
    index: TurnIndex,
    /// Whether we are currently inside a turn (between TurnStarted and
    /// TurnCompleted/TurnFailed). Used to update the last index entry's
    /// offsets as intermediate events arrive.
    in_turn: bool,
    /// Running byte offset of the next append. Avoids a `stat` syscall per
    /// event (the previous implementation re-statted the file twice on every
    /// `append`); initialized from the file length on `open`.
    next_offset: u64,
    /// Buffered pending writes to batch syscalls. Events are appended here
    /// and flushed to disk at turn boundaries or before read operations.
    write_buf: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
struct CapEvictionPlan {
    truncate_offset: u64,
    evicted_event_count: u64,
    evicted_turn_count: usize,
}

impl LogState {
    fn new(session_id: &str) -> Self {
        Self {
            manifest: SessionManifest::new(session_id),
            index: TurnIndex::default(),
            in_turn: false,
            next_offset: 0,
            write_buf: Vec::with_capacity(65536),
        }
    }

    /// Serialize `event` directly into the reusable write buffer with rollback
    /// on failure.
    ///
    /// This encapsulates the invariant that `write_buf` never contains a
    /// partial JSON document: if `serde_json::to_writer` fails mid-write the
    /// buffer is truncated back to its pre-serialization boundary.  Returns
    /// the `(start, end)` byte offsets of the serialized event so the caller
    /// can feed them to [`Self::apply_lifecycle_event`].
    fn serialize_event(&mut self, event: &ThreadEvent) -> Result<(u64, u64), SessionStoreError> {
        let start = self.next_offset;
        let buf_len_before = self.write_buf.len();
        if let Err(err) = serde_json::to_writer(
            &mut self.write_buf,
            &BorrowedVersionedEvent { schema_version: EVENT_SCHEMA_VERSION, event },
        ) {
            self.write_buf.truncate(buf_len_before);
            return Err(err.into());
        }
        self.write_buf.push(b'\n');
        let written = self.write_buf.len() - buf_len_before;
        let end = start + written as u64;
        self.next_offset = end;
        Ok((start, end))
    }

    /// Update the in-memory turn index and manifest counters for a single
    /// event.
    ///
    /// This is the single implementation of the turn-lifecycle state machine;
    /// both the append path (via [`LifecycleKind::from_event`]) and the scan
    /// path (via [`LifecycleKind::from_kind`]) route through here, eliminating
    /// a previously duplicated match block.
    ///
    /// Returns `true` when the event closes a turn boundary
    /// (`TurnCompleted` / `TurnFailed`) so the caller can persist metadata
    /// at the appropriate time (append persists immediately; scan persists
    /// once after the full scan).
    fn apply_lifecycle_event(&mut self, kind: LifecycleKind, start: u64, end: u64) -> bool {
        let is_boundary = match kind {
            LifecycleKind::ThreadStarted => {
                self.manifest.status = "active".to_string();
                false
            }
            LifecycleKind::ThreadCompleted => {
                self.manifest.status = "completed".to_string();
                true
            }
            LifecycleKind::TurnStarted => {
                self.manifest.status = "active".to_string();
                self.in_turn = true;
                let n = self.manifest.turn_count + 1;
                self.index.entries.push_back(TurnIndexEntry {
                    turn_number: n,
                    start_offset: start,
                    end_offset: end,
                    event_count: 1,
                    ts: now_rfc3339(),
                });
                false
            }
            LifecycleKind::TurnCompleted | LifecycleKind::TurnFailed => {
                if self.in_turn {
                    if let Some(entry) = self.index.entries.back_mut() {
                        entry.end_offset = end;
                        entry.event_count += 1;
                        // `turn_number` remains monotonic when older completed
                        // turns have been evicted. The manifest is the source
                        // for the next ordinal, so never replace it with the
                        // retained index length.
                        self.manifest.turn_count = self.manifest.turn_count.max(entry.turn_number);
                    }
                    self.in_turn = false;
                }
                true
            }
            LifecycleKind::Other => {
                if self.in_turn
                    && let Some(entry) = self.index.entries.back_mut()
                {
                    entry.end_offset = end;
                    entry.event_count += 1;
                }
                false
            }
        };
        // Persist the open-turn marker alongside the manifest. The marker is
        // deliberately optional for backwards compatibility: an older
        // manifest without it forces a scan on reopen so the state can be
        // reconstructed from the canonical event log.
        self.manifest.in_turn = Some(self.in_turn);
        is_boundary
    }

    /// Plan a cap-enforcement eviction: pop the oldest completed turns from
    /// the index until `event_count` is within `max_events`.
    ///
    /// Returns the byte offset at which the file should be rewritten and the
    /// counts needed to apply the eviction after successful I/O. Returns
    /// `None` when no eviction is needed.
    fn plan_cap_eviction(&self, max_events: usize) -> Option<CapEvictionPlan> {
        if max_events == 0 || self.manifest.event_count <= max_events as u64 {
            return None;
        }
        let mut evicted_event_count = 0u64;
        let mut truncate_offset = 0u64;
        let mut evicted_turn_count = 0;
        for (entry_index, oldest) in self.index.entries.iter().enumerate() {
            // Never evict the active turn. Its entry has a provisional end
            // offset and will be closed by a later completion/failure event;
            // removing it here would make that event unindexed and lose the
            // in-flight turn from reconstruction. If the completed history
            // alone cannot bring the log under the cap, retain the active
            // turn until it reaches a terminal boundary.
            if self.in_turn && entry_index + 1 == self.index.entries.len() {
                break;
            }
            if self.manifest.event_count.saturating_sub(evicted_event_count) <= max_events as u64 {
                break;
            }
            truncate_offset = oldest.end_offset;
            evicted_event_count += oldest.event_count;
            evicted_turn_count += 1;
        }
        if truncate_offset == 0 || evicted_turn_count == 0 {
            None
        } else {
            Some(CapEvictionPlan {
                truncate_offset,
                evicted_event_count,
                evicted_turn_count,
            })
        }
    }

    fn apply_cap_eviction(&mut self, plan: CapEvictionPlan, next_offset: u64) {
        for _ in 0..plan.evicted_turn_count {
            let _ = self.index.entries.pop_front();
        }
        for entry in &mut self.index.entries {
            entry.start_offset = entry.start_offset.saturating_sub(plan.truncate_offset);
            entry.end_offset = entry.end_offset.saturating_sub(plan.truncate_offset);
        }
        self.next_offset = next_offset;
        self.manifest.event_count = self.manifest.event_count.saturating_sub(plan.evicted_event_count);
        self.manifest.retained_turn_base = Some(self.retained_turn_base_after_eviction(0));
    }

    fn retained_turn_base_after_eviction(&self, evicted_turn_count: usize) -> u64 {
        self.index
            .entries
            .get(evicted_turn_count)
            .map(|entry| entry.turn_number)
            .or_else(|| self.manifest.turn_count.checked_add(1))
            .unwrap_or(1)
            .max(1)
    }
}

/// State and file handle shared by every owner of one session in this process.
///
/// A keyed operation lock alone is insufficient: two independently opened
/// handles could still carry stale turn counters and overwrite each other's
/// metadata after taking the lock. Sharing the mutable state and append file
/// makes the lock a true session boundary while preserving the value-type
/// `SessionEventLog` API.
struct SessionShared {
    file: Mutex<Option<File>>,
    state: Mutex<LogState>,
    eviction_lock: Mutex<()>,
    initialized: AtomicBool,
}

/// Return the process-wide shared state for one session's canonical event file.
///
/// The weak registry avoids retaining closed sessions forever while still
/// making repeated `open` calls converge on one file handle and turn state.
fn shared_session(events_path: &Path, session_id: &str) -> Result<Arc<SessionShared>, SessionStoreError> {
    static SESSION_SHARED: OnceLock<Mutex<HashMap<PathBuf, Weak<SessionShared>>>> = OnceLock::new();

    let key = events_path
        .parent()
        .and_then(|parent| vtcode_commons::paths::canonicalize(parent).ok())
        .and_then(|parent| events_path.file_name().map(|name| parent.join(name)))
        .unwrap_or_else(|| events_path.to_path_buf());
    let registry = SESSION_SHARED.get_or_init(|| Mutex::new(HashMap::new()));
    let mut shared_by_path = match registry.lock() {
        Ok(locks) => locks,
        Err(poisoned) => poisoned.into_inner(),
    };
    // Do not retain dead weak entries for every session ever opened by a
    // long-running process. The registry is only an in-process coordination
    // aid, so removing entries with no live owners is safe.
    shared_by_path.retain(|_, shared| shared.strong_count() > 0);
    if let Some(shared) = shared_by_path.get(&key).and_then(Weak::upgrade) {
        return Ok(shared);
    }

    let file = VtCodePaths::open_private_append_file(events_path)
        .map_err(|error| SessionStoreError::io(events_path.to_path_buf(), std::io::Error::other(error)))?;
    let shared = Arc::new(SessionShared {
        file: Mutex::new(Some(file)),
        state: Mutex::new(LogState::new(session_id)),
        eviction_lock: Mutex::new(()),
        initialized: AtomicBool::new(false),
    });
    shared_by_path.insert(key, Arc::downgrade(&shared));
    Ok(shared)
}

/// Canonical append-only event log for a single session.
///
/// All session history is reconstructable from this log. Live conversation
/// state is never read back into context from here; the log is only consumed
/// for revert, compaction, analytics, and long-term-learning queries.
pub struct SessionEventLog {
    events_path: PathBuf,
    manifest_store: ManifestStore,
    shared: Arc<SessionShared>,
    max_events: usize,
    eviction_summary_hook: EvictionSummaryHook,
}

impl SessionEventLog {
    /// Open the log for `session_id`, creating the session directory tree and
    /// rebuilding the index from `events.jsonl` if it already exists.
    pub(crate) fn open(workspace: &Path, session_id: &str, max_events: usize) -> Result<Self, SessionStoreError> {
        let dir = session_dir(workspace, session_id);
        let hook = default_eviction_summary_hook(dir.join(crate::DERIVED_DIR), session_id.to_string());
        Self::open_with_eviction_summary(workspace, session_id, max_events, hook)
    }

    /// Open a log with an explicit eviction-summary callback.
    ///
    /// This is useful for hosts that keep derived memory in another store and
    /// for deterministic failure-path tests. The callback must persist its
    /// summary before returning `Ok(())`.
    pub fn open_with_eviction_summary(
        workspace: &Path,
        session_id: &str,
        max_events: usize,
        eviction_summary_hook: EvictionSummaryHook,
    ) -> Result<Self, SessionStoreError> {
        let dir = session_dir(workspace, session_id);
        crate::ensure_private_directory(&crate::sessions_root(workspace))?;
        crate::ensure_private_directory(&dir)?;
        crate::ensure_private_directory(&dir.join(crate::DERIVED_DIR))?;
        crate::ensure_private_directory(&dir.join("index"))?;
        let events_path = dir.join("events.jsonl");
        let manifest_store = ManifestStore::new(dir.clone());
        let pending_rewrite = manifest_store.load_pending_cap_rewrite()?;
        let shared = shared_session(&events_path, session_id)?;
        let log = Self {
            events_path: events_path.clone(),
            manifest_store,
            shared,
            max_events,
            eviction_summary_hook,
        };
        let _eviction_guard = log.shared.eviction_lock.lock().map_err(poison)?;
        // Try the fast path: read the persisted manifest + index and skip
        // the O(n) scan when they are present and consistent.
        if !log.shared.initialized.load(Ordering::Acquire) {
            let manifest_opt = log.manifest_store.load_manifest()?;
            let index_opt = log.manifest_store.load_turn_index()?;
            let file_len = log.event_file_metadata_len()?;
            let pending_rewrite_matches_file = pending_rewrite.as_ref().is_some_and(|pending| {
                pending.new_file_len == file_len && pending.new_file_len < pending.previous_file_len
            });
            match (&manifest_opt, &index_opt) {
                (Some(manifest), Some(index))
                    if !pending_rewrite_matches_file
                        && manifest.in_turn.is_some()
                        && manifest.persisted_file_len == Some(file_len)
                        && index.is_valid_for_file(file_len)
                        && index.is_consistent_with_manifest(manifest) =>
                {
                    let mut st = log.shared.state.lock().map_err(poison)?;
                    st.in_turn = manifest.in_turn.unwrap_or(false);
                    st.manifest = manifest.clone();
                    st.index = index.clone();
                    st.next_offset = file_len;
                }
                _ => {
                    let scan_turn_base = infer_scan_turn_base(
                        manifest_opt.as_ref(),
                        index_opt.as_ref(),
                        pending_rewrite.as_ref().filter(|_| pending_rewrite_matches_file),
                        file_len,
                    );
                    {
                        let mut st = log.shared.state.lock().map_err(poison)?;
                        if let Some(previous) = manifest_opt.as_ref() {
                            st.manifest = previous.clone();
                        }
                        // The canonical event file is authoritative after any
                        // stale/corrupt metadata. Preserve the ordinal of the
                        // first retained turn while rebuilding all counters.
                        st.manifest.turn_count = scan_turn_base.saturating_sub(1);
                        st.manifest.event_count = 0;
                        st.manifest.status = "active".to_string();
                        st.manifest.in_turn = Some(false);
                        st.manifest.retained_turn_base = Some(scan_turn_base);
                        st.index = TurnIndex::default();
                        st.in_turn = false;
                    }
                    log.scan()?;
                    let mut st = log.shared.state.lock().map_err(poison)?;
                    st.next_offset = file_len;
                    log.persist_meta_locked(&mut st)?;
                }
            }
            if pending_rewrite.is_some() {
                // A marker whose file length did not match either side of the
                // rewrite is stale, while a matching marker has now been
                // incorporated into the repaired metadata. In both cases it
                // is safe to remove it after the open path has persisted the
                // authoritative state.
                log.manifest_store.clear_pending_cap_rewrite()?;
            }
            log.shared.initialized.store(true, Ordering::Release);
        }
        drop(_eviction_guard);
        Ok(log)
    }

    /// Append an event to the log and update the in-memory index/manifest.
    pub fn append(&self, event: &ThreadEvent) -> Result<(), SessionStoreError> {
        let _eviction_guard = self.shared.eviction_lock.lock().map_err(poison)?;
        let mut st = self.shared.state.lock().map_err(poison)?;

        // Serialize into the write buffer with rollback on failure — the
        // invariant that `write_buf` never contains partial JSON is
        // encapsulated in `serialize_event`.
        let (start, end) = st.serialize_event(event)?;

        st.manifest.event_count += 1;
        st.manifest.updated_at = now_rfc3339();

        // Route through the single turn-lifecycle state machine.  When the
        // event closes a turn, persist metadata immediately so a reopen
        // after a mid-turn crash sees a consistent index.
        let is_turn_boundary = st.apply_lifecycle_event(LifecycleKind::from_event(event), start, end);
        if is_turn_boundary {
            self.persist_meta_locked(&mut st)?;
        }

        if st.write_buf.len() >= MAX_WRITE_BUFFER_BYTES {
            // Persist metadata with the bounded byte flush so a reopen after
            // a mid-turn crash does not trust an index that predates these
            // already-written events.
            self.persist_meta_locked(&mut st)?;
        }
        drop(st);
        self.enforce_event_cap()
    }

    /// Enforce the per-session event cap by evicting the oldest completed
    /// turns when the log exceeds [`Self::max_events`]. Returns `Ok(())` even
    /// when no truncation is needed or the cap is disabled (`max_events == 0`).
    fn enforce_event_cap(&self) -> Result<(), SessionStoreError> {
        let mut st = self.shared.state.lock().map_err(poison)?;

        // `plan_cap_eviction` encapsulates the index arithmetic and returns
        // `None` when the cap is disabled or not yet exceeded.
        let Some(plan) = st.plan_cap_eviction(self.max_events) else {
            return Ok(());
        };

        // Keep ordinary appends in memory until a turn boundary or an
        // explicit read. Cap enforcement is the one append-time path that
        // needs the complete on-disk file before rewriting it.
        self.flush_write_buf_locked(&mut st)?;

        let (evicted, remaining, previous_file_len) = {
            let mut file_slot = self.shared.file.lock().map_err(poison)?;
            let file = file_slot.as_mut().ok_or_else(|| self.event_file_unavailable())?;
            let file_len = file
                .metadata()
                .map_err(|error| SessionStoreError::io(&self.events_path, error))?
                .len();
            if plan.truncate_offset > file_len {
                return Err(SessionStoreError::io(
                    &self.events_path,
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "cap offset exceeds event log length"),
                ));
            }
            file.seek(SeekFrom::Start(0))
                .map_err(|e| SessionStoreError::io(&self.events_path, e))?;
            let mut evicted = vec![
                0u8;
                usize::try_from(plan.truncate_offset).map_err(|error| {
                    SessionStoreError::io(
                        &self.events_path,
                        std::io::Error::new(std::io::ErrorKind::InvalidData, error),
                    )
                })?
            ];
            file.read_exact(&mut evicted)
                .map_err(|e| SessionStoreError::io(&self.events_path, e))?;
            file.seek(SeekFrom::Start(plan.truncate_offset))
                .map_err(|e| SessionStoreError::io(&self.events_path, e))?;
            let mut remaining = Vec::new();
            file.read_to_end(&mut remaining)
                .map_err(|e| SessionStoreError::io(&self.events_path, e))?;
            (evicted, remaining, file_len)
        };
        let retained_turn_base = st.retained_turn_base_after_eviction(plan.evicted_turn_count);
        drop(st);

        // The turn index counts only records inside indexed turns. The bytes
        // removed by a cap rewrite may also contain valid session-level
        // records (for example `thread.started`) before the first turn, so
        // reconcile the manifest against the actual persisted prefix rather
        // than the turn-only estimate from `plan_cap_eviction`.
        let mut plan = plan;
        plan.evicted_event_count = count_persisted_event_records(&evicted);
        let evicted_events = decode_events(&evicted);
        (self.eviction_summary_hook)(&evicted_events)?;

        let new_file_len = u64::try_from(remaining.len()).map_err(|error| {
            SessionStoreError::io(&self.events_path, std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        })?;
        self.manifest_store.write_pending_cap_rewrite(&PendingCapRewrite {
            previous_file_len,
            new_file_len,
            retained_turn_base,
        })?;
        let next_offset = self.replace_event_file_contents(&remaining)?;
        let mut st = self.shared.state.lock().map_err(poison)?;
        st.apply_cap_eviction(plan, next_offset);
        // The rewrite changed byte offsets and retained counts; persist the
        // derived metadata before exposing the append as successful.
        self.persist_meta_locked(&mut st)?;
        self.manifest_store.clear_pending_cap_rewrite()?;
        Ok(())
    }

    /// Reconstruct every event belonging to `turn`.
    pub(crate) fn reconstruct_turn(&self, turn: u64) -> Result<Vec<ThreadEvent>, SessionStoreError> {
        // Keep the index snapshot and byte-range read together with cap
        // rewriting. Otherwise an eviction can replace the file between these
        // steps and leave the snapshot offsets pointing into unrelated events.
        let _eviction_guard = self.shared.eviction_lock.lock().map_err(poison)?;
        let entry = {
            let st = self.shared.state.lock().map_err(poison)?;
            st.index
                .entries
                .iter()
                .find(|e| e.turn_number == turn)
                .cloned()
                .ok_or(SessionStoreError::TurnNotFound { session: st.manifest.session_id.clone(), turn })?
        };
        {
            let mut st = self.shared.state.lock().map_err(poison)?;
            self.flush_write_buf_locked(&mut st)?;
        }
        let buf = {
            let mut file_slot = self.shared.file.lock().map_err(poison)?;
            let file = file_slot.as_mut().ok_or_else(|| self.event_file_unavailable())?;
            file.seek(SeekFrom::Start(entry.start_offset))
                .map_err(|e| SessionStoreError::io(&self.events_path, e))?;
            let len = usize::try_from(entry.end_offset.checked_sub(entry.start_offset).ok_or_else(|| {
                SessionStoreError::io(
                    &self.events_path,
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "turn index offsets are out of order"),
                )
            })?)
            .map_err(|error| {
                SessionStoreError::io(&self.events_path, std::io::Error::new(std::io::ErrorKind::InvalidData, error))
            })?;
            let mut buf = vec![0u8; len];
            file.read_exact(&mut buf)
                .map_err(|e| SessionStoreError::io(&self.events_path, e))?;
            buf
        };
        let text = String::from_utf8_lossy(&buf);
        let mut events = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // The index scan only validates the event envelope (plus the
            // lifecycle shape) so it can rebuild cheaply. A line accepted by
            // the scan can therefore still fail full decoding here; skip it
            // instead of failing the whole reconstruction (revert, compaction,
            // and analytics must not break on a single malformed record).
            let v: VersionedThreadEvent = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            events.push(v.into_event());
        }
        Ok(events)
    }

    /// Number of turns recorded.
    #[must_use]
    pub(crate) fn turn_count(&self) -> u64 {
        self.shared.state.lock().map_err(poison).map_or(0, |s| s.manifest.turn_count)
    }

    /// Number of events recorded.
    #[must_use]
    pub fn event_count(&self) -> u64 {
        self.shared.state.lock().map_err(poison).map_or(0, |s| s.manifest.event_count)
    }

    /// Flush pending event bytes and metadata to the session store.
    pub fn flush(&self) -> Result<(), SessionStoreError> {
        let _eviction_guard = self.shared.eviction_lock.lock().map_err(poison)?;
        let mut st = self.shared.state.lock().map_err(poison)?;
        self.persist_meta_locked(&mut st)
    }

    /// Snapshot of the session manifest.
    #[must_use]
    pub fn manifest(&self) -> SessionManifest {
        self.shared
            .state
            .lock()
            .map_err(poison)
            .map(|s| s.manifest.clone())
            .unwrap_or_else(|_| SessionManifest::new(""))
    }

    /// Snapshot of the turn index.
    #[must_use]
    pub fn turn_index(&self) -> TurnIndex {
        self.shared
            .state
            .lock()
            .map_err(poison)
            .map(|s| s.index.clone())
            .unwrap_or_default()
    }

    /// Flush metadata for callers that explicitly close a log handle.
    ///
    /// Terminal status is intentionally controlled only by a persisted
    /// `thread.completed` event. This method does not synthesize lifecycle
    /// state for callers that merely release a store handle.
    pub(crate) fn complete(&self) -> Result<(), SessionStoreError> {
        let _eviction_guard = self.shared.eviction_lock.lock().map_err(poison)?;
        let mut st = self.shared.state.lock().map_err(poison)?;
        st.manifest.updated_at = now_rfc3339();
        self.persist_meta_locked(&mut st)
    }

    /// Rebuild index + manifest by scanning `events.jsonl` (authoritative).
    ///
    /// Reads the file line-by-line via `BufReader` to avoid loading the entire
    /// log into memory. Long-lived sessions can otherwise produce multi-megabyte
    /// logs that spike memory on every reopen.
    fn scan(&self) -> Result<(), SessionStoreError> {
        let mut st = self.shared.state.lock().map_err(poison)?;
        let file = self
            .shared
            .file
            .lock()
            .map_err(poison)?
            .as_ref()
            .ok_or_else(|| self.event_file_unavailable())?
            .try_clone()
            .map_err(|e| SessionStoreError::io(&self.events_path, e))?;
        let mut reader = std::io::BufReader::new(file);
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|e| SessionStoreError::io(&self.events_path, e))?;
        let mut buf = Vec::new();
        let mut pos = 0u64;
        let mut first_ts: Option<String> = None;
        loop {
            buf.clear();
            let n = reader
                .read_until(b'\n', &mut buf)
                .map_err(|e| SessionStoreError::io(&self.events_path, e))?;
            if n == 0 {
                break;
            }
            let line_end = pos + n as u64;
            let trimmed = std::str::from_utf8(&buf).unwrap_or("").trim();
            if !trimmed.is_empty()
                && let Ok(v) = serde_json::from_str::<VersionedEventKind<'_>>(trimmed)
            {
                let kind = v.event.kind;
                if requires_full_lifecycle_validation(kind) && !valid_lifecycle_payload(kind, trimmed) {
                    pos = line_end;
                    continue;
                }
                st.manifest.event_count += 1;
                // `thread.started` is not part of the turn lifecycle — it
                // only seeds `created_at` on the first occurrence.
                if kind == "thread.started" && first_ts.is_none() {
                    first_ts = Some(now_rfc3339());
                }
                // Route turn-lifecycle events through the same state machine
                // as `append`, eliminating a previously duplicated match block.
                st.apply_lifecycle_event(LifecycleKind::from_kind(kind), pos, line_end);
            }
            pos = line_end;
        }
        // Keep the open-turn state reconstructed from the canonical event log.
        // This lets a reopened session continue a turn that was flushed before
        // its completion event was written.
        st.manifest.in_turn = Some(st.in_turn);
        if let Some(ts) = first_ts
            && st.manifest.created_at.is_empty()
        {
            st.manifest.created_at = ts;
        }
        Ok(())
    }

    fn persist_meta_locked(&self, st: &mut LogState) -> Result<(), SessionStoreError> {
        self.flush_write_buf_locked(st)?;
        // The manifest is only eligible for the fast reopen path when it
        // describes the complete on-disk event file. Drop intentionally
        // flushes bytes without metadata, so a length mismatch safely forces
        // the authoritative scan on the next open.
        st.manifest.persisted_file_len = Some(st.next_offset);
        // Publish the derived index first. If a process stops between these
        // two atomic renames, the older manifest still carries a stale file
        // length and forces a scan instead of allowing the new manifest to
        // pair with an older, apparently valid index.
        self.manifest_store.write_turn_index(&st.index)?;
        self.manifest_store.write_manifest(&st.manifest)?;
        Ok(())
    }

    /// Flush the in-memory write buffer to the underlying file.
    fn flush_write_buf_locked(&self, st: &mut LogState) -> Result<(), SessionStoreError> {
        if st.write_buf.is_empty() {
            return Ok(());
        }
        let mut file_slot = self.shared.file.lock().map_err(poison)?;
        let file = file_slot.as_mut().ok_or_else(|| self.event_file_unavailable())?;
        let previous_len = file.metadata().map_err(|e| SessionStoreError::io(&self.events_path, e))?.len();
        if let Err(error) = file.write_all(&st.write_buf) {
            if file.set_len(previous_len).is_err() {
                st.write_buf.clear();
            }
            return Err(SessionStoreError::io(&self.events_path, error));
        }
        if let Err(error) = file.sync_data() {
            st.write_buf.clear();
            return Err(SessionStoreError::io(&self.events_path, error));
        }
        st.write_buf.clear();
        Ok(())
    }

    fn event_file_metadata_len(&self) -> Result<u64, SessionStoreError> {
        let file_slot = self.shared.file.lock().map_err(poison)?;
        file_slot
            .as_ref()
            .ok_or_else(|| self.event_file_unavailable())?
            .metadata()
            .map(|metadata| metadata.len())
            .map_err(|error| SessionStoreError::io(&self.events_path, error))
    }

    fn open_event_file(&self) -> Result<File, SessionStoreError> {
        VtCodePaths::open_private_append_file(&self.events_path)
            .map_err(|error| SessionStoreError::io(&self.events_path, std::io::Error::other(error)))
    }

    fn replace_event_file_contents(&self, contents: &[u8]) -> Result<u64, SessionStoreError> {
        let old_file = {
            let mut file_slot = self.shared.file.lock().map_err(poison)?;
            file_slot.take().ok_or_else(|| self.event_file_unavailable())?
        };
        drop(old_file);

        if let Err(error) = VtCodePaths::write_private_file_atomic(&self.events_path, contents)
            .map_err(|error| SessionStoreError::io(&self.events_path, std::io::Error::other(error)))
        {
            let restored = self.open_event_file();
            if let Ok(file) = restored {
                let mut file_slot = self.shared.file.lock().map_err(poison)?;
                *file_slot = Some(file);
                return Err(error);
            }
            return Err(SessionStoreError::io(
                &self.events_path,
                std::io::Error::other(format!("{error}; failed to restore event log handle")),
            ));
        }

        let replacement = self.open_event_file()?;
        let next_offset = replacement
            .metadata()
            .map_err(|error| SessionStoreError::io(&self.events_path, error))?
            .len();
        let mut file_slot = self.shared.file.lock().map_err(poison)?;
        *file_slot = Some(replacement);
        Ok(next_offset)
    }

    fn event_file_unavailable(&self) -> SessionStoreError {
        SessionStoreError::io(&self.events_path, std::io::Error::other("event log file is unavailable"))
    }
}

/// Recover the ordinal of the first retained turn when metadata is stale.
///
/// A cap rewrite atomically replaces the event file before it publishes the
/// shortened index and manifest. If the process crashes in that interval,
/// the old index still records the pre-rewrite offsets. The difference between
/// its persisted length and the current file length is exactly the removed
/// prefix, so the first old index entry at that boundary supplies the retained
/// turn base. Other stale metadata falls back to the durable base field.
fn infer_scan_turn_base(
    manifest: Option<&SessionManifest>,
    index: Option<&TurnIndex>,
    pending: Option<&PendingCapRewrite>,
    file_len: u64,
) -> u64 {
    let marker_base = pending
        .filter(|pending| pending.new_file_len == file_len && pending.new_file_len < pending.previous_file_len)
        .map(|pending| pending.retained_turn_base);
    let rewritten_base = manifest.and_then(|manifest| {
        let previous_len = manifest.persisted_file_len?;
        if previous_len <= file_len {
            return None;
        }
        let removed_prefix = previous_len - file_len;
        let index = index.filter(|index| index.is_valid_for_file(previous_len))?;
        let first_retained = index
            .entries
            .iter()
            .find(|entry| entry.start_offset >= removed_prefix && entry.end_offset <= previous_len)
            .map(|entry| entry.turn_number);
        first_retained.or_else(|| {
            // If no indexed turn starts in the shortened file, the rewrite
            // evicted every previously indexed turn. Preserve the next
            // ordinal from the stale manifest so a subsequent append cannot
            // reuse an already-observed turn number.
            (removed_prefix >= previous_len).then(|| manifest.turn_count.saturating_add(1))
        })
    });

    let legacy_index_base = index
        .filter(|index| index.is_valid_for_file(file_len))
        .and_then(|index| index.entries.front().map(|entry| entry.turn_number));

    marker_base
        .or(rewritten_base)
        .or_else(|| manifest.and_then(|manifest| manifest.retained_turn_base))
        .or(legacy_index_base)
        .unwrap_or(1)
        .max(1)
}

fn requires_full_lifecycle_validation(kind: &str) -> bool {
    matches!(kind, "thread.started" | "thread.completed" | "turn.started" | "turn.completed" | "turn.failed")
}

fn valid_lifecycle_payload(kind: &str, line: &str) -> bool {
    if serde_json::from_str::<VersionedThreadEvent>(line).is_err() {
        return false;
    }
    if kind != "turn.completed" {
        return true;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return false;
    };
    value
        .get("event")
        .and_then(|event| event.get("usage"))
        .is_some_and(serde_json::Value::is_object)
}

fn decode_events(bytes: &[u8]) -> Vec<ThreadEvent> {
    bytes
        .split(|byte| *byte == b'\n')
        .filter_map(|line| {
            let line = std::str::from_utf8(line).ok()?.trim();
            if line.is_empty() {
                return None;
            }
            serde_json::from_str::<VersionedThreadEvent>(line)
                .ok()
                .map(VersionedThreadEvent::into_event)
        })
        .collect()
}

/// Count records that the authoritative scan would include in the manifest.
/// This deliberately parses the lightweight event envelope instead of using
/// the turn index: a cap rewrite can remove session-level records that never
/// belong to an indexed turn.
fn count_persisted_event_records(bytes: &[u8]) -> u64 {
    bytes
        .split(|byte| *byte == b'\n')
        .filter_map(|line| std::str::from_utf8(line).ok())
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| {
            let Ok(value) = serde_json::from_str::<VersionedEventKind<'_>>(line) else {
                return false;
            };
            let kind = value.event.kind;
            !requires_full_lifecycle_validation(kind) || valid_lifecycle_payload(kind, line)
        })
        .count() as u64
}

#[derive(Debug, Serialize)]
struct EvictionSummary {
    session_id: String,
    evicted_event_count: usize,
    event_types: BTreeMap<String, u64>,
    created_at: String,
}

fn default_eviction_summary_hook(derived_dir: PathBuf, session_id: String) -> EvictionSummaryHook {
    Arc::new(move |events| {
        let mut event_types = BTreeMap::new();
        for event in events {
            let kind = serde_json::to_value(event)
                .ok()
                .and_then(|value| value.get("type").and_then(|value| value.as_str()).map(str::to_owned))
                .unwrap_or_else(|| "unknown".to_owned());
            *event_types.entry(kind).or_insert(0) += 1;
        }
        let summary = EvictionSummary {
            session_id: session_id.clone(),
            evicted_event_count: events.len(),
            event_types,
            created_at: now_rfc3339(),
        };
        let path = derived_dir.join(format!("eviction-summary-{}.json", uuid::Uuid::new_v4().simple()));
        let bytes = serde_json::to_vec(&summary)?;
        VtCodePaths::write_private_file_atomic(&path, &bytes)
            .map_err(|error| SessionStoreError::io(path, std::io::Error::other(error)))
    })
}

impl Drop for SessionEventLog {
    fn drop(&mut self) {
        if let Ok(_eviction_guard) = self.shared.eviction_lock.lock()
            && let Ok(mut st) = self.shared.state.lock()
        {
            // The fallible `flush` method is the authoritative shutdown path;
            // Drop only provides a best-effort byte flush for callers that do
            // not explicitly close the log. Rewriting metadata here could
            // overwrite a manifest update made by another owner after the
            // last append.
            let _ = self.flush_write_buf_locked(&mut st);
        }
    }
}

/// Locate the next newline at or after `from`, returning a past-the-end index.
fn poison<T>(_e: std::sync::PoisonError<T>) -> SessionStoreError {
    SessionStoreError::Io {
        path: PathBuf::new(),
        source: std::io::Error::other("session store lock poisoned"),
    }
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

/// Session-level metadata persisted to `manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionManifest {
    /// Stable session identifier (directory name).
    pub session_id: String,
    /// Layout schema version (`SESSION_STORE_SCHEMA_VERSION`).
    schema_version: u32,
    /// RFC3339 creation timestamp.
    pub created_at: String,
    /// RFC3339 last-update timestamp.
    pub updated_at: String,
    /// Number of completed turns.
    pub turn_count: u64,
    /// Total number of events recorded.
    pub event_count: u64,
    /// Lifecycle status (`active` | `completed`).
    pub status: String,
    /// Whether the canonical log currently ends inside an open turn.
    ///
    /// This is optional on read so manifests written before open-turn
    /// persistence was introduced trigger a safe event-log scan instead of
    /// silently losing lifecycle state.
    #[serde(default)]
    in_turn: Option<bool>,
    /// Byte length covered by the persisted manifest and turn index.
    ///
    /// This is optional for compatibility with manifests written before the
    /// fast-path freshness guard existed; those manifests are rebuilt from the
    /// canonical event log on reopen.
    #[serde(default)]
    persisted_file_len: Option<u64>,
    /// Ordinal of the first turn retained in the canonical log.
    ///
    /// Cap eviction removes completed turns but must keep later turn numbers
    /// monotonic. The field lets an authoritative scan restore those ordinals
    /// even when the derived index is stale or missing.
    #[serde(default)]
    retained_turn_base: Option<u64>,
}

impl SessionManifest {
    /// Create a fresh manifest for a session.
    #[must_use]
    pub(crate) fn new(session_id: &str) -> Self {
        let ts = now_rfc3339();
        Self {
            session_id: session_id.to_string(),
            schema_version: crate::SESSION_STORE_SCHEMA_VERSION,
            created_at: ts.clone(),
            updated_at: ts,
            turn_count: 0,
            event_count: 0,
            status: "active".to_string(),
            in_turn: Some(false),
            persisted_file_len: Some(0),
            retained_turn_base: Some(1),
        }
    }
}

/// Byte-offset index of a single turn within `events.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TurnIndexEntry {
    /// Turn ordinal (1-based).
    turn_number: u64,
    /// Byte offset of the turn's first event.
    start_offset: u64,
    /// Byte offset just past the turn's last event.
    end_offset: u64,
    /// Number of events in the turn.
    event_count: u64,
    /// RFC3339 timestamp of turn start.
    ts: String,
}

/// Ordered index of all turns in a session.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TurnIndex {
    /// Turn entries in ordinal order.
    entries: VecDeque<TurnIndexEntry>,
}

impl TurnIndex {
    /// Number of indexed turns.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the index is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn is_valid_for_file(&self, file_len: u64) -> bool {
        let mut previous_end = 0u64;
        self.entries.iter().all(|entry| {
            let valid = entry.event_count > 0
                && entry.start_offset >= previous_end
                && entry.start_offset <= entry.end_offset
                && entry.end_offset <= file_len;
            if valid {
                previous_end = entry.end_offset;
            }
            valid
        })
    }

    fn is_consistent_with_manifest(&self, manifest: &SessionManifest) -> bool {
        let expected_last_turn = if manifest.in_turn == Some(true) {
            manifest.turn_count.saturating_add(1)
        } else {
            manifest.turn_count
        };
        let entries_are_contiguous = self
            .entries
            .iter()
            .map(|entry| entry.turn_number)
            .try_fold(None::<u64>, |previous, turn_number| {
                if previous.is_some_and(|previous| turn_number != previous.saturating_add(1)) {
                    return Err(());
                }
                Ok(Some(turn_number))
            })
            .is_ok();
        if !entries_are_contiguous {
            return false;
        }

        let expected_first_turn = manifest.retained_turn_base.unwrap_or(1).max(1);
        match (self.entries.front(), self.entries.back()) {
            (Some(first), Some(last)) => {
                first.turn_number == expected_first_turn && last.turn_number == expected_last_turn
            }
            (None, None) => {
                expected_last_turn == 0
                    || manifest
                        .retained_turn_base
                        .is_some_and(|retained_turn_base| retained_turn_base > manifest.turn_count)
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod borrowed_envelope_tests {
    use super::{BorrowedVersionedEvent, EVENT_SCHEMA_VERSION};
    use vtcode_exec_events::{
        ThreadEvent, ThreadStartedEvent, TurnCompletedEvent, TurnStartedEvent, Usage, VersionedThreadEvent,
    };

    /// The borrowed envelope must produce JSON byte-identical to
    /// `VersionedThreadEvent::new(event.clone())`. This guards against drift if
    /// either the envelope or the canonical wrapper is modified.
    #[test]
    fn borrowed_envelope_matches_versioned_envelope() {
        for event in [
            ThreadEvent::ThreadStarted(ThreadStartedEvent { thread_id: "thread".to_string() }),
            ThreadEvent::TurnStarted(TurnStartedEvent::default()),
            ThreadEvent::TurnCompleted(TurnCompletedEvent { usage: Usage::default() }),
        ] {
            let canonical =
                serde_json::to_string(&VersionedThreadEvent::new(event.clone())).expect("canonical serialize");
            let borrowed = serde_json::to_string(&BorrowedVersionedEvent {
                schema_version: EVENT_SCHEMA_VERSION,
                event: &event,
            })
            .expect("borrowed serialize");
            assert_eq!(canonical, borrowed, "JSON differs for {event:?}");
        }
    }
}

#[cfg(test)]
mod lifecycle_state_machine_tests {
    use super::{LifecycleKind, LogState};
    use vtcode_exec_events::{
        ThreadCompletedEvent, ThreadCompletionSubtype, ThreadEvent, ThreadStartedEvent, TurnCompletedEvent,
        TurnFailedEvent, TurnStartedEvent, Usage,
    };

    fn fresh_state() -> LogState {
        LogState::new("test-session")
    }

    #[test]
    fn lifecycle_kind_from_event_covers_all_variants() {
        assert_eq!(
            LifecycleKind::from_event(&ThreadEvent::TurnStarted(TurnStartedEvent::default())),
            LifecycleKind::TurnStarted
        );
        assert_eq!(
            LifecycleKind::from_event(&ThreadEvent::TurnCompleted(TurnCompletedEvent { usage: Usage::default() })),
            LifecycleKind::TurnCompleted
        );
        assert_eq!(
            LifecycleKind::from_event(&ThreadEvent::TurnFailed(TurnFailedEvent {
                message: "err".to_string(),
                usage: None,
            })),
            LifecycleKind::TurnFailed
        );
        assert_eq!(
            LifecycleKind::from_event(&ThreadEvent::ThreadStarted(ThreadStartedEvent { thread_id: "x".to_string() })),
            LifecycleKind::ThreadStarted
        );
        assert_eq!(
            LifecycleKind::from_event(&ThreadEvent::ThreadCompleted(Box::new(ThreadCompletedEvent {
                thread_id: "x".to_string(),
                session_id: "x".to_string(),
                subtype: ThreadCompletionSubtype::Success,
                outcome_code: "completed".to_string(),
                result: None,
                stop_reason: None,
                usage: Usage::default(),
                total_cost_usd: None,
                num_turns: 1,
            }))),
            LifecycleKind::ThreadCompleted
        );
        assert_eq!(LifecycleKind::from_kind("thread.started"), LifecycleKind::ThreadStarted);
        assert_eq!(LifecycleKind::from_kind("thread.completed"), LifecycleKind::ThreadCompleted);
    }

    #[test]
    fn lifecycle_kind_from_str_matches_event_discriminator() {
        assert_eq!(LifecycleKind::from_kind("turn.started"), LifecycleKind::TurnStarted);
        assert_eq!(LifecycleKind::from_kind("turn.completed"), LifecycleKind::TurnCompleted);
        assert_eq!(LifecycleKind::from_kind("turn.failed"), LifecycleKind::TurnFailed);
        assert_eq!(LifecycleKind::from_kind("tool.called"), LifecycleKind::Other);
        assert_eq!(LifecycleKind::from_kind("thread.started"), LifecycleKind::ThreadStarted);
        assert_eq!(LifecycleKind::from_kind("thread.completed"), LifecycleKind::ThreadCompleted);
    }

    #[test]
    fn turn_started_pushes_index_entry_and_sets_in_turn() {
        let mut st = fresh_state();
        st.manifest.status = "completed".to_string();
        let is_boundary = st.apply_lifecycle_event(LifecycleKind::TurnStarted, 0, 100);
        assert!(!is_boundary, "TurnStarted is not a turn boundary");
        assert!(st.in_turn);
        assert_eq!(st.manifest.status, "active");
        assert_eq!(st.index.entries.len(), 1);
        let entry = &st.index.entries[0];
        assert_eq!(entry.turn_number, 1);
        assert_eq!(entry.start_offset, 0);
        assert_eq!(entry.end_offset, 100);
        assert_eq!(entry.event_count, 1);
    }

    #[test]
    fn intermediate_events_extend_current_turn() {
        let mut st = fresh_state();
        st.apply_lifecycle_event(LifecycleKind::TurnStarted, 0, 100);
        // Simulate two intermediate events.
        let is_b1 = st.apply_lifecycle_event(LifecycleKind::Other, 100, 200);
        let is_b2 = st.apply_lifecycle_event(LifecycleKind::Other, 200, 300);
        assert!(!is_b1 && !is_b2);
        assert!(st.in_turn);
        assert_eq!(st.index.entries.len(), 1);
        let entry = &st.index.entries[0];
        assert_eq!(entry.end_offset, 300);
        assert_eq!(entry.event_count, 3);
    }

    #[test]
    fn turn_completed_closes_turn_and_returns_boundary() {
        let mut st = fresh_state();
        st.apply_lifecycle_event(LifecycleKind::TurnStarted, 0, 100);
        st.apply_lifecycle_event(LifecycleKind::Other, 100, 200);
        let is_boundary = st.apply_lifecycle_event(LifecycleKind::TurnCompleted, 200, 300);
        assert!(is_boundary);
        assert!(!st.in_turn);
        assert_eq!(st.manifest.turn_count, 1);
        assert_eq!(st.manifest.status, "active");
        st.apply_lifecycle_event(LifecycleKind::ThreadCompleted, 300, 400);
        assert_eq!(st.manifest.status, "completed");
        let entry = &st.index.entries[0];
        assert_eq!(entry.end_offset, 300);
        assert_eq!(entry.event_count, 3);
    }

    #[test]
    fn turn_failed_closes_turn_without_terminal_thread_status() {
        let mut st = fresh_state();
        st.apply_lifecycle_event(LifecycleKind::TurnStarted, 0, 100);
        let is_boundary = st.apply_lifecycle_event(LifecycleKind::TurnFailed, 100, 200);
        assert!(is_boundary);
        assert!(!st.in_turn);
        assert_eq!(st.manifest.turn_count, 1);
        assert_eq!(st.manifest.status, "active");
        st.apply_lifecycle_event(LifecycleKind::ThreadCompleted, 200, 300);
        assert_eq!(st.manifest.status, "completed");
    }

    #[test]
    fn turn_completed_without_turn_started_is_idempotent() {
        let mut st = fresh_state();
        // Receiving TurnCompleted without a preceding TurnStarted should not
        // panic or corrupt the index; terminal status remains active until the
        // thread lifecycle itself completes.
        let is_boundary = st.apply_lifecycle_event(LifecycleKind::TurnCompleted, 0, 100);
        assert!(is_boundary);
        assert!(!st.in_turn);
        assert_eq!(st.manifest.turn_count, 0, "no turn was started");
        assert_eq!(st.manifest.status, "active");
        st.apply_lifecycle_event(LifecycleKind::ThreadCompleted, 100, 200);
        assert_eq!(st.manifest.status, "completed");
        assert!(st.index.entries.is_empty());
    }

    #[test]
    fn multiple_turns_get_incrementing_ordinals() {
        let mut st = fresh_state();
        for n in 1..=3 {
            st.apply_lifecycle_event(LifecycleKind::TurnStarted, n * 100, n * 100 + 50);
            st.apply_lifecycle_event(LifecycleKind::TurnCompleted, n * 100 + 50, n * 100 + 100);
        }
        assert_eq!(st.index.entries.len(), 3);
        for (i, entry) in st.index.entries.iter().enumerate() {
            assert_eq!(entry.turn_number, (i + 1) as u64);
        }
        assert_eq!(st.manifest.turn_count, 3);
    }
}

#[cfg(test)]
mod cap_eviction_tests {
    use super::{LogState, TurnIndexEntry};

    /// Build a `LogState` with `turns` fake turns, each having `events_per_turn`
    /// events, starting at byte offset 0.
    fn state_with_turns(turns: usize, events_per_turn: u64) -> LogState {
        let mut st = LogState::new("cap-test");
        st.manifest.event_count = (turns as u64) * events_per_turn;
        let mut offset = 0u64;
        for n in 1..=turns {
            st.index.entries.push_back(TurnIndexEntry {
                turn_number: n as u64,
                start_offset: offset,
                end_offset: offset + events_per_turn * 10,
                event_count: events_per_turn,
                ts: "2026-01-01T00:00:00Z".to_string(),
            });
            offset += events_per_turn * 10;
        }
        st
    }

    #[test]
    fn no_eviction_when_under_cap() {
        let st = state_with_turns(3, 2); // 6 events
        assert!(st.plan_cap_eviction(10).is_none());
        assert_eq!(st.index.entries.len(), 3, "no turns should be evicted");
    }

    #[test]
    fn no_eviction_when_cap_disabled() {
        let st = state_with_turns(5, 2); // 10 events
        assert!(st.plan_cap_eviction(0).is_none());
        assert_eq!(st.index.entries.len(), 5);
    }

    #[test]
    fn evicts_oldest_turns_to_meet_cap() {
        // 5 turns × 2 events = 10 events; cap = 6 → need to evict 2 turns (4 events).
        let st = state_with_turns(5, 2);
        let plan = st.plan_cap_eviction(6).expect("eviction planned");
        assert_eq!(plan.evicted_event_count, 4, "should evict 4 events (2 turns)");
        assert_eq!(st.index.entries.len(), 5, "planning must not mutate state");
        // Truncate offset is the end of the last evicted turn.
        assert_eq!(plan.truncate_offset, 40); // 2 turns × 20 bytes each

        // Applying the plan leaves turns 3, 4, 5.
        let mut st = st;
        st.apply_cap_eviction(plan, 60);
        assert_eq!(st.index.entries.len(), 3, "should keep 3 turns");
        assert_eq!(st.index.entries[0].turn_number, 3);
        assert_eq!(st.index.entries[2].turn_number, 5);
    }

    #[test]
    fn evicts_all_turns_when_cap_smaller_than_one_turn() {
        // 3 turns × 5 events = 15 events; cap = 3 → evict turns until ≤ 3 remain.
        // Each turn has 5 events, so evicting 2 turns leaves 5 (>3), evicting
        // 3 turns leaves 0.
        let st = state_with_turns(3, 5);
        let plan = st.plan_cap_eviction(3).expect("eviction planned");
        assert_eq!(plan.evicted_event_count, 15, "all events evicted");
        let mut st = st;
        st.apply_cap_eviction(plan, 0);
        assert_eq!(st.index.entries.len(), 0);
    }
}
