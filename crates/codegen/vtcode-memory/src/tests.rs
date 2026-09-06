//! Tests for the unified session store.

use std::fs;
use std::sync::Arc;

use tempfile::TempDir;
use vtcode_exec_events::{
    ThreadCompletedEvent, ThreadCompletionSubtype, ThreadEvent, ThreadStartedEvent, TurnCompletedEvent,
    TurnStartedEvent, Usage, VersionedThreadEvent,
};

use crate::event_log::{DEFAULT_MAX_EVENTS, SessionEventLog};
use crate::migration::migrate_legacy;
use crate::query::{query_facts, recent_sessions};
use crate::{
    EvictionSummaryHook, SessionStoreError, open, open_with_eviction_summary, retention::apply_retention, sessions_root,
};

fn sample_turn() -> Vec<ThreadEvent> {
    vec![
        ThreadEvent::ThreadStarted(ThreadStartedEvent { thread_id: "thread".to_string() }),
        ThreadEvent::TurnStarted(TurnStartedEvent::default()),
        ThreadEvent::TurnCompleted(TurnCompletedEvent { usage: Usage::default() }),
        ThreadEvent::ThreadCompleted(Box::new(ThreadCompletedEvent {
            thread_id: "thread".to_string(),
            session_id: "session".to_string(),
            subtype: ThreadCompletionSubtype::Success,
            outcome_code: "completed".to_string(),
            result: None,
            stop_reason: None,
            usage: Usage::default(),
            total_cost_usd: None,
            num_turns: 1,
        })),
    ]
}

#[test]
fn append_and_reconstruct_roundtrip() {
    let dir = TempDir::new().expect("tempdir");
    let log = open(dir.path(), "sess-1", DEFAULT_MAX_EVENTS).expect("open");
    for _ in 0..3 {
        for e in &sample_turn() {
            log.append(e).expect("append");
        }
    }
    assert_eq!(log.turn_count(), 3);
    let rebuilt = log.reconstruct_turn(2).expect("reconstruct");
    assert_eq!(rebuilt.len(), 2);
    assert!(matches!(rebuilt[0], ThreadEvent::TurnStarted(_)));
    assert!(matches!(rebuilt[1], ThreadEvent::TurnCompleted(_)));
}

#[test]
fn event_log_batches_appends_until_turn_boundary() {
    let dir = TempDir::new().expect("tempdir");
    let log = open(dir.path(), "sess-buffered", DEFAULT_MAX_EVENTS).expect("open");
    let events_path = sessions_root(dir.path()).join("sess-buffered").join("events.jsonl");

    log.append(&ThreadEvent::ThreadStarted(ThreadStartedEvent { thread_id: "thread".to_string() }))
        .expect("append thread event");
    assert_eq!(fs::metadata(&events_path).expect("metadata").len(), 0);

    log.append(&ThreadEvent::TurnStarted(TurnStartedEvent::default()))
        .expect("append turn start");
    assert_eq!(fs::metadata(&events_path).expect("metadata").len(), 0);

    log.append(&ThreadEvent::TurnCompleted(TurnCompletedEvent { usage: Usage::default() }))
        .expect("append turn completion");
    assert!(fs::metadata(&events_path).expect("metadata").len() > 0);
    assert_eq!(log.reconstruct_turn(1).expect("reconstruct").len(), 2);
}

#[test]
fn large_buffer_flush_persists_manifest_progress() {
    let dir = TempDir::new().expect("tempdir");
    let log = open(dir.path(), "sess-large-buffer", DEFAULT_MAX_EVENTS).expect("open");

    for index in 0..1_000 {
        log.append(&ThreadEvent::ThreadStarted(ThreadStartedEvent {
            thread_id: format!("thread-{index:04}-buffer-boundary"),
        }))
        .expect("append event");
    }

    let manifest_path = sessions_root(dir.path()).join("sess-large-buffer").join("manifest.json");
    let manifest: crate::SessionManifest =
        serde_json::from_str(&fs::read_to_string(manifest_path).expect("read manifest")).expect("parse manifest");
    assert!(manifest.event_count > 0);
    assert!(manifest.event_count < 1_000);
}

#[test]
fn flushing_mid_turn_persists_buffered_metadata_for_reopen() {
    let dir = TempDir::new().expect("tempdir");
    {
        let log = open(dir.path(), "sess-mid-turn", DEFAULT_MAX_EVENTS).expect("open");
        log.append(&ThreadEvent::ThreadStarted(ThreadStartedEvent { thread_id: "thread".to_string() }))
            .expect("append thread event");
        log.append(&ThreadEvent::TurnStarted(TurnStartedEvent::default()))
            .expect("append turn start");
        log.flush().expect("flush mid-turn event log");
    }

    let reopened = open(dir.path(), "sess-mid-turn", DEFAULT_MAX_EVENTS).expect("reopen");
    assert_eq!(reopened.event_count(), 2);
    assert_eq!(reopened.turn_count(), 0);
    assert_eq!(reopened.reconstruct_turn(1).expect("reconstruct open turn").len(), 1);
}

#[test]
fn reopening_after_drop_scans_bytes_flushed_without_metadata() {
    let dir = TempDir::new().expect("tempdir");
    {
        let log = open(dir.path(), "sess-drop-flush", DEFAULT_MAX_EVENTS).expect("open");
        log.append(&ThreadEvent::ThreadStarted(ThreadStartedEvent { thread_id: "thread".to_string() }))
            .expect("append thread event");
        // Drop performs a best-effort byte flush but deliberately leaves the
        // manifest untouched, so reopen must detect the stale file length.
    }

    let reopened = open(dir.path(), "sess-drop-flush", DEFAULT_MAX_EVENTS).expect("reopen");
    assert_eq!(reopened.event_count(), 1);
}

#[test]
fn reopening_mid_turn_preserves_state_for_completion() {
    let dir = TempDir::new().expect("tempdir");
    {
        let log = open(dir.path(), "sess-mid-turn-resume", DEFAULT_MAX_EVENTS).expect("open");
        log.append(&ThreadEvent::TurnStarted(TurnStartedEvent::default()))
            .expect("append turn start");
        log.flush().expect("flush mid-turn event log");
    }

    let reopened = open(dir.path(), "sess-mid-turn-resume", DEFAULT_MAX_EVENTS).expect("reopen");
    reopened
        .append(&ThreadEvent::TurnCompleted(TurnCompletedEvent { usage: Usage::default() }))
        .expect("append turn completion");

    assert_eq!(reopened.turn_count(), 1);
    assert_eq!(reopened.reconstruct_turn(1).expect("reconstruct completed turn").len(), 2);
}

#[test]
fn index_rebuilt_on_reopen() {
    let dir = TempDir::new().expect("tempdir");
    {
        let log = open(dir.path(), "sess-2", DEFAULT_MAX_EVENTS).expect("open");
        for e in &sample_turn() {
            log.append(e).expect("append");
        }
        log.complete().expect("complete");
    }
    // Reopen: scan must rebuild the index from events.jsonl.
    let log = SessionEventLog::open(dir.path(), "sess-2", DEFAULT_MAX_EVENTS).expect("reopen");
    assert_eq!(log.turn_count(), 1);
    let rebuilt = log.reconstruct_turn(1).expect("reconstruct after reopen");
    assert_eq!(rebuilt.len(), 2);
    assert!(log.manifest().status == "completed");
}

#[test]
fn migrate_legacy_imports_history_and_trajectory() {
    let dir = TempDir::new().expect("tempdir");
    let vt = dir.path().join(".vtcode");
    fs::create_dir_all(vt.join("history")).expect("mk history");
    fs::create_dir_all(vt.join("logs")).expect("mk logs");

    let memory = serde_json::json!({
        "session_id": "session-foo",
        "schema_version": 2,
        "summary": "did a thing",
        "grounded_facts": [{"fact": "the widget is blue"}],
    });
    fs::write(
        vt.join("history").join("session-foo.memory.json"),
        serde_json::to_string_pretty(&memory).expect("ser"),
    )
    .expect("write memory");

    fs::write(
        vt.join("logs").join("trajectory-20260101T000000Z.jsonl"),
        "{\"kind\":\"llm_retry_metrics\",\"turn\":1}\n",
    )
    .expect("write traj");

    let report = migrate_legacy(dir.path(), false).expect("migrate");
    assert_eq!(report.sessions_created, 2);
    assert_eq!(report.memory_imported, 1);
    assert_eq!(report.trajectory_imported, 1);

    // Cross-session fact query works off the unified store.
    let facts = query_facts(dir.path(), 10).expect("facts");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].fact, "the widget is blue");

    // Legacy history + logs still present (remove_legacy=false).
    assert!(vt.join("history").exists());
    assert!(vt.join("logs").exists());

    // recent_sessions lists the migrated sessions.
    let sessions = recent_sessions(dir.path(), 10);
    assert_eq!(sessions.len(), 2);
}

#[test]
fn retention_removes_oldest_sessions() {
    let dir = TempDir::new().expect("tempdir");
    // Create 3 old sessions (2020) and 2 recent sessions (today).
    for i in 0..5u64 {
        let log = open(dir.path(), &format!("sess-{i}"), DEFAULT_MAX_EVENTS).expect("open");
        for e in &sample_turn() {
            log.append(e).expect("append");
        }
        log.complete().expect("complete");
        let mpath = sessions_root(dir.path()).join(format!("sess-{i}")).join("manifest.json");
        let mut m: crate::SessionManifest =
            serde_json::from_str(&fs::read_to_string(&mpath).expect("read manifest")).expect("parse");
        // First 3 are old (2020), last 2 keep today's timestamp.
        if i < 3 {
            m.updated_at = format!("2020-01-{:02}T00:00:00Z", i + 1);
            fs::write(&mpath, serde_json::to_string_pretty(&m).expect("ser")).expect("write manifest");
        }
    }

    // max_sessions=4: count-based eviction removes 1 oldest (sess-0).
    // max_age_days=30: age-based eviction removes 2 more old sessions (sess-1, sess-2).
    // Total: 3 removed, 2 recent remain.
    let removed = apply_retention(dir.path(), crate::retention::RetentionPolicy { max_sessions: 4, max_age_days: 30 })
        .expect("retain");
    assert_eq!(removed, 3);
    let remaining = recent_sessions(dir.path(), 100);
    assert_eq!(remaining.len(), 2);
}

#[test]
fn retention_evicts_old_sessions_even_when_under_count_cap() {
    let dir = TempDir::new().expect("tempdir");
    // Create 3 sessions: 1 old (2020) and 2 recent (today).
    for i in 0..3u64 {
        let log = open(dir.path(), &format!("sess-{i}"), DEFAULT_MAX_EVENTS).expect("open");
        for e in &sample_turn() {
            log.append(e).expect("append");
        }
        log.complete().expect("complete");
        if i == 0 {
            let mpath = sessions_root(dir.path()).join("sess-0").join("manifest.json");
            let mut m: crate::SessionManifest =
                serde_json::from_str(&fs::read_to_string(&mpath).expect("read manifest")).expect("parse");
            m.updated_at = "2020-01-01T00:00:00Z".to_string();
            fs::write(&mpath, serde_json::to_string_pretty(&m).expect("ser")).expect("write manifest");
        }
    }

    // max_sessions=10: count cap is not exceeded (3 < 10).
    // max_age_days=30: age-based eviction should still remove sess-0.
    let removed = apply_retention(dir.path(), crate::retention::RetentionPolicy { max_sessions: 10, max_age_days: 30 })
        .expect("retain");
    assert_eq!(removed, 1);
    let remaining = recent_sessions(dir.path(), 100);
    assert_eq!(remaining.len(), 2);
}

#[test]
fn retention_preserves_active_sessions() {
    let dir = TempDir::new().expect("tempdir");
    let log = open(dir.path(), "active-session", DEFAULT_MAX_EVENTS).expect("open");
    log.append(&ThreadEvent::ThreadStarted(ThreadStartedEvent { thread_id: "active".to_string() }))
        .expect("append thread start");
    log.flush().expect("flush active session");

    let session_dir = sessions_root(dir.path()).join("active-session");
    let manifest_path = session_dir.join("manifest.json");
    let mut manifest: crate::SessionManifest =
        serde_json::from_str(&fs::read_to_string(&manifest_path).expect("read manifest")).expect("parse manifest");
    manifest.updated_at = "2020-01-01T00:00:00Z".to_string();
    fs::write(&manifest_path, serde_json::to_string_pretty(&manifest).expect("serialize manifest"))
        .expect("write manifest");

    let removed = apply_retention(dir.path(), crate::retention::RetentionPolicy { max_sessions: 0, max_age_days: 0 })
        .expect("retain");

    assert_eq!(removed, 0);
    assert!(session_dir.exists(), "active sessions must not be evicted");
}

#[test]
fn retention_preserves_explicit_current_session() {
    let dir = TempDir::new().expect("tempdir");
    for session_id in ["current-session", "old-session"] {
        let log = open(dir.path(), session_id, DEFAULT_MAX_EVENTS).expect("open");
        for event in &sample_turn() {
            log.append(event).expect("append lifecycle");
        }
        log.complete().expect("complete");
        let manifest_path = sessions_root(dir.path()).join(session_id).join("manifest.json");
        let mut manifest: crate::SessionManifest =
            serde_json::from_str(&fs::read_to_string(&manifest_path).expect("read manifest")).expect("parse manifest");
        manifest.updated_at = "2020-01-01T00:00:00Z".to_string();
        fs::write(&manifest_path, serde_json::to_string_pretty(&manifest).expect("serialize manifest"))
            .expect("write manifest");
    }

    let removed = crate::retention::apply_retention_preserving(
        dir.path(),
        crate::retention::RetentionPolicy { max_sessions: 0, max_age_days: 0 },
        Some("current-session"),
    )
    .expect("retain");

    assert_eq!(removed, 1);
    assert!(sessions_root(dir.path()).join("current-session").exists());
    assert!(!sessions_root(dir.path()).join("old-session").exists());
}

#[test]
fn retention_ignores_manifest_session_id_for_deletion_path() {
    let dir = TempDir::new().expect("tempdir");
    let outside = dir.path().join("outside");
    fs::create_dir(&outside).expect("create outside");
    fs::write(outside.join("keep.txt"), "preserve").expect("write outside file");

    let log = open(dir.path(), "safe-session", DEFAULT_MAX_EVENTS).expect("open");
    for event in &sample_turn() {
        log.append(event).expect("append lifecycle");
    }
    log.complete().expect("complete");
    let manifest_path = sessions_root(dir.path()).join("safe-session").join("manifest.json");
    let mut manifest: crate::SessionManifest =
        serde_json::from_str(&fs::read_to_string(&manifest_path).expect("read manifest")).expect("parse manifest");
    manifest.session_id = "../outside".to_string();
    manifest.updated_at = "2020-01-01T00:00:00Z".to_string();
    fs::write(&manifest_path, serde_json::to_string(&manifest).expect("serialize manifest")).expect("write manifest");

    assert_eq!(
        apply_retention(dir.path(), crate::retention::RetentionPolicy { max_sessions: 0, max_age_days: 30 })
            .expect("retain"),
        1
    );
    assert!(outside.join("keep.txt").exists());
}

#[cfg(unix)]
#[test]
fn retention_skips_symlink_session_entries() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new().expect("tempdir");
    let target = sessions_root(dir.path()).join("target");
    let log = open(dir.path(), "target", DEFAULT_MAX_EVENTS).expect("open");
    for event in &sample_turn() {
        log.append(event).expect("append lifecycle");
    }
    log.complete().expect("complete");
    let manifest_path = target.join("manifest.json");
    let mut manifest: crate::SessionManifest =
        serde_json::from_str(&fs::read_to_string(&manifest_path).expect("read manifest")).expect("parse manifest");
    manifest.updated_at = "2020-01-01T00:00:00Z".to_string();
    fs::write(&manifest_path, serde_json::to_string(&manifest).expect("serialize manifest")).expect("write manifest");
    let link = sessions_root(dir.path()).join("linked");
    symlink(&target, &link).expect("create symlink");

    apply_retention(dir.path(), crate::retention::RetentionPolicy { max_sessions: 0, max_age_days: 30 })
        .expect("retain");
    assert!(!target.exists());
    assert!(link.exists() || fs::symlink_metadata(&link).is_ok());
}

#[cfg(unix)]
#[test]
fn retention_skips_symlink_sessions_root() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new().expect("tempdir");
    let real_root = dir.path().join("real-sessions");
    let linked_session = real_root.join("linked-session");
    fs::create_dir_all(&linked_session).expect("create linked session");
    fs::write(
        linked_session.join("manifest.json"),
        serde_json::json!({
            "session_id": "linked-session",
            "turn_count": 0,
            "event_count": 0,
            "status": "completed",
            "updated_at": "2020-01-01T00:00:00Z"
        })
        .to_string(),
    )
    .expect("write linked manifest");

    let sessions_parent = dir.path().join(".vtcode");
    fs::create_dir_all(&sessions_parent).expect("create sessions parent");
    symlink(&real_root, sessions_parent.join("sessions")).expect("create sessions root symlink");

    assert_eq!(
        apply_retention(dir.path(), crate::retention::RetentionPolicy { max_sessions: 0, max_age_days: 0 })
            .expect("retain"),
        0
    );
    assert!(linked_session.exists());
}

#[test]
fn manifest_shortcut_skips_scan_on_reopen() {
    let dir = TempDir::new().expect("tempdir");
    // Write a few turns and complete.
    {
        let log = open(dir.path(), "sess-shortcut", DEFAULT_MAX_EVENTS).expect("open");
        for e in &sample_turn() {
            log.append(e).expect("append");
        }
        log.complete().expect("complete");
    }
    // Reopen: the manifest + index should be loaded without scanning.
    let log = SessionEventLog::open(dir.path(), "sess-shortcut", DEFAULT_MAX_EVENTS).expect("reopen");
    assert_eq!(log.turn_count(), 1);
    assert_eq!(log.manifest().status, "completed");
    let rebuilt = log.reconstruct_turn(1).expect("reconstruct");
    assert_eq!(rebuilt.len(), 2);
}

#[test]
fn corrupt_manifest_falls_back_to_event_log_scan() {
    let dir = TempDir::new().expect("tempdir");
    {
        let log = open(dir.path(), "sess-corrupt-manifest", DEFAULT_MAX_EVENTS).expect("open");
        for event in &sample_turn() {
            log.append(event).expect("append");
        }
        log.complete().expect("complete");
    }

    let session_dir = sessions_root(dir.path()).join("sess-corrupt-manifest");
    fs::write(session_dir.join("manifest.json"), b"{\"broken\"").expect("corrupt manifest");

    let reopened = open(dir.path(), "sess-corrupt-manifest", DEFAULT_MAX_EVENTS).expect("recover from manifest");
    assert_eq!(reopened.turn_count(), 1);
    assert_eq!(reopened.reconstruct_turn(1).expect("reconstruct").len(), 2);
    serde_json::from_str::<crate::SessionManifest>(
        &fs::read_to_string(session_dir.join("manifest.json")).expect("read repaired manifest"),
    )
    .expect("manifest should be repaired");
}

#[test]
fn stale_turn_index_offsets_fall_back_to_event_log_scan() {
    let dir = TempDir::new().expect("tempdir");
    {
        let log = open(dir.path(), "sess-stale-index", DEFAULT_MAX_EVENTS).expect("open");
        for event in &sample_turn() {
            log.append(event).expect("append");
        }
        log.complete().expect("complete");
    }

    let index_path = sessions_root(dir.path()).join("sess-stale-index/index/turns.json");
    let mut index: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&index_path).expect("read index")).expect("parse index");
    index["entries"][0]["end_offset"] = serde_json::json!(u64::MAX);
    fs::write(&index_path, serde_json::to_vec(&index).expect("serialize stale index")).expect("write stale index");

    let reopened = open(dir.path(), "sess-stale-index", DEFAULT_MAX_EVENTS).expect("recover from stale index");
    assert_eq!(reopened.turn_count(), 1);
    assert_eq!(reopened.reconstruct_turn(1).expect("reconstruct").len(), 2);
}

#[test]
fn current_manifest_rejects_a_stale_but_well_formed_turn_index() {
    let dir = TempDir::new().expect("tempdir");
    let session_id = "sess-stale-valid-index";
    let session_dir = sessions_root(dir.path()).join(session_id);
    let index_path = session_dir.join("index/turns.json");
    let log = open(dir.path(), session_id, DEFAULT_MAX_EVENTS).expect("open");
    for event in [
        ThreadEvent::TurnStarted(TurnStartedEvent::default()),
        ThreadEvent::TurnCompleted(TurnCompletedEvent { usage: Usage::default() }),
    ] {
        log.append(&event).expect("append first turn");
    }
    log.flush().expect("flush first turn");
    let first_turn_index = fs::read(&index_path).expect("read first index");
    for event in [
        ThreadEvent::TurnStarted(TurnStartedEvent::default()),
        ThreadEvent::TurnCompleted(TurnCompletedEvent { usage: Usage::default() }),
    ] {
        log.append(&event).expect("append second turn");
    }
    log.flush().expect("flush second turn");
    drop(log);

    // A crash after manifest.json was published but before turns.json would
    // leave exactly this pairing: the current file length and manifest with a
    // shorter, still structurally valid index.
    let mut current_index: serde_json::Value =
        serde_json::from_slice(&fs::read(&index_path).expect("read current index")).expect("parse current index");
    let first_index: serde_json::Value = serde_json::from_slice(&first_turn_index).expect("parse first index");
    let second_entry = current_index["entries"][1].clone();
    current_index["entries"] = serde_json::json!([first_index["entries"][0].clone()]);
    fs::write(&index_path, serde_json::to_vec(&current_index).expect("serialize stale index"))
        .expect("write stale index");

    let reopened = open(dir.path(), session_id, DEFAULT_MAX_EVENTS).expect("recover from stale index");
    assert_eq!(reopened.turn_count(), 2);
    assert_eq!(reopened.reconstruct_turn(1).expect("first turn").len(), 2);
    assert_eq!(reopened.reconstruct_turn(2).expect("second turn").len(), 2);

    // An index containing only turn 2 is also structurally valid, but its
    // first ordinal does not match the manifest's retained base. It must be
    // rejected and rebuilt so turn 1 remains addressable.
    drop(reopened);
    current_index["entries"] = serde_json::json!([second_entry]);
    fs::write(&index_path, serde_json::to_vec(&current_index).expect("serialize offset index"))
        .expect("write offset index");

    let reopened = open(dir.path(), session_id, DEFAULT_MAX_EVENTS).expect("recover from offset index");
    assert_eq!(reopened.turn_count(), 2);
    assert_eq!(reopened.reconstruct_turn(1).expect("rebuilt first turn").len(), 2);
    assert_eq!(reopened.reconstruct_turn(2).expect("rebuilt second turn").len(), 2);
}

#[test]
fn legacy_manifest_uses_valid_index_base_when_scanning() {
    let dir = TempDir::new().expect("tempdir");
    let session_id = "sess-legacy-index-base";
    let session_dir = sessions_root(dir.path()).join(session_id);
    let events_path = session_dir.join("events.jsonl");
    let index_path = session_dir.join("index/turns.json");
    {
        let log = open(dir.path(), session_id, 0).expect("open");
        for _ in 0..3 {
            log.append(&ThreadEvent::TurnStarted(TurnStartedEvent::default()))
                .expect("append turn start");
            log.append(&ThreadEvent::TurnCompleted(TurnCompletedEvent { usage: Usage::default() }))
                .expect("append turn completion");
        }
        log.flush().expect("flush");
    }

    let bytes = fs::read(&events_path).expect("read event log");
    let mut index: serde_json::Value =
        serde_json::from_slice(&fs::read(&index_path).expect("read turn index")).expect("parse turn index");
    let third_entry = index["entries"][2].clone();
    let third_start = third_entry["start_offset"].as_u64().expect("third turn start");
    let third_end = third_entry["end_offset"].as_u64().expect("third turn end");
    let retained_len = third_end.saturating_sub(third_start);
    fs::write(
        &events_path,
        &bytes[usize::try_from(third_start).expect("offset fits")..usize::try_from(third_end).expect("offset fits")],
    )
    .expect("write retained turn");

    // Legacy manifests did not persist `retained_turn_base` or `in_turn`.
    // Keep a structurally valid, rebased index so the scan must recover the
    // first retained ordinal from that index instead of restarting at turn 1.
    index["entries"] = serde_json::json!([{
        "turn_number": third_entry["turn_number"],
        "start_offset": 0,
        "end_offset": retained_len,
        "event_count": third_entry["event_count"],
        "ts": third_entry["ts"],
    }]);
    fs::write(&index_path, serde_json::to_vec(&index).expect("serialize legacy index")).expect("write legacy index");
    let manifest_path = session_dir.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("read manifest")).expect("parse manifest");
    manifest.as_object_mut().expect("manifest object").remove("retained_turn_base");
    manifest.as_object_mut().expect("manifest object").remove("in_turn");
    fs::write(&manifest_path, serde_json::to_vec(&manifest).expect("serialize legacy manifest"))
        .expect("write legacy manifest");

    let reopened = open(dir.path(), session_id, 0).expect("reopen legacy session");
    assert_eq!(reopened.turn_count(), 3);
    assert!(reopened.reconstruct_turn(1).is_err(), "evicted ordinals must stay absent");
    assert!(reopened.reconstruct_turn(2).is_err(), "evicted ordinals must stay absent");
    assert_eq!(reopened.reconstruct_turn(3).expect("retained turn").len(), 2);
}

#[test]
fn cap_rewrite_keeps_event_log_appendable_and_reopenable() {
    let dir = TempDir::new().expect("tempdir");
    let log = open(dir.path(), "sess-cap-rewrite", 2).expect("open");
    let turn = || {
        [
            ThreadEvent::TurnStarted(TurnStartedEvent::default()),
            ThreadEvent::TurnCompleted(TurnCompletedEvent { usage: Usage::default() }),
        ]
    };

    for event in turn().into_iter().chain(turn()) {
        log.append(&event).expect("append");
    }

    let events_path = sessions_root(dir.path()).join("sess-cap-rewrite/events.jsonl");
    assert_eq!(fs::read_to_string(&events_path).expect("read compacted log").lines().count(), 2);
    let summaries: Vec<_> = fs::read_dir(sessions_root(dir.path()).join("sess-cap-rewrite/derived"))
        .expect("read summaries")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("eviction-summary-"))
        .collect();
    assert!(!summaries.is_empty(), "evicted history must have a persisted summary");
    assert_eq!(log.reconstruct_turn(2).expect("reconstruct retained turn").len(), 2);

    drop(log);
    let reopened = open(dir.path(), "sess-cap-rewrite", 2).expect("reopen compacted log");
    assert_eq!(reopened.event_count(), 2);
    assert_eq!(reopened.reconstruct_turn(2).expect("reconstruct after reopen").len(), 2);
}

#[test]
fn cap_eviction_counts_session_records_removed_with_oldest_turn() {
    let dir = TempDir::new().expect("tempdir");
    let log = open(dir.path(), "sess-cap-session-record", 2).expect("open");
    log.append(&ThreadEvent::ThreadStarted(ThreadStartedEvent { thread_id: "thread".to_string() }))
        .expect("append thread start");
    for _ in 0..2 {
        log.append(&ThreadEvent::TurnStarted(TurnStartedEvent::default()))
            .expect("append turn start");
        log.append(&ThreadEvent::TurnCompleted(TurnCompletedEvent { usage: Usage::default() }))
            .expect("append turn completion");
    }

    assert_eq!(log.event_count(), 2, "eviction count must include thread.started in the removed prefix");
    assert_eq!(
        fs::read_to_string(sessions_root(dir.path()).join("sess-cap-session-record/events.jsonl"))
            .expect("read compacted event log")
            .lines()
            .count(),
        2
    );
    assert!(log.reconstruct_turn(1).is_err(), "oldest turn must be evicted");
    assert_eq!(log.reconstruct_turn(2).expect("retained turn").len(), 2);
}

#[test]
fn scan_after_cap_rewrite_crash_preserves_retained_turn_ordinals() {
    let dir = TempDir::new().expect("tempdir");
    let events_path = sessions_root(dir.path()).join("sess-cap-crash/events.jsonl");
    {
        let log = open(dir.path(), "sess-cap-crash", 0).expect("open");
        for _ in 0..3 {
            log.append(&ThreadEvent::TurnStarted(TurnStartedEvent::default()))
                .expect("append turn start");
            log.append(&ThreadEvent::TurnCompleted(TurnCompletedEvent { usage: Usage::default() }))
                .expect("append turn completion");
        }
        log.flush().expect("flush before simulated rewrite");
    }

    // Simulate a process dying after the atomic event-file replacement but
    // before the shortened manifest and index are published. The stale index
    // still contains the original offsets and ordinals, which the reopen path
    // must use to recover the first retained turn number.
    let index_path = sessions_root(dir.path()).join("sess-cap-crash/index/turns.json");
    let index: serde_json::Value =
        serde_json::from_slice(&fs::read(&index_path).expect("read turn index")).expect("parse turn index");
    let truncate_offset = index["entries"][0]["end_offset"].as_u64().expect("first turn end offset");
    let bytes = fs::read(&events_path).expect("read event log");
    fs::write(&events_path, &bytes[usize::try_from(truncate_offset).expect("offset fits")..])
        .expect("simulate shortened event log");

    let reopened = open(dir.path(), "sess-cap-crash", 0).expect("reopen after simulated crash");
    assert_eq!(reopened.turn_count(), 3);
    assert!(reopened.reconstruct_turn(1).is_err(), "evicted ordinal must stay absent");
    assert_eq!(reopened.reconstruct_turn(2).expect("retained turn 2").len(), 2);
    assert_eq!(reopened.reconstruct_turn(3).expect("retained turn 3").len(), 2);
}

#[test]
fn scan_after_full_cap_rewrite_crash_preserves_next_turn_ordinal() {
    let dir = TempDir::new().expect("tempdir");
    let events_path = sessions_root(dir.path()).join("sess-cap-crash-empty/events.jsonl");
    {
        let log = open(dir.path(), "sess-cap-crash-empty", 0).expect("open");
        for event in [
            ThreadEvent::TurnStarted(TurnStartedEvent::default()),
            ThreadEvent::TurnCompleted(TurnCompletedEvent { usage: Usage::default() }),
        ] {
            log.append(&event).expect("append");
        }
        log.flush().expect("flush before simulated rewrite");
    }

    // Simulate the atomic replacement after every indexed turn was evicted,
    // before the shortened manifest was published. The stale manifest still
    // carries the last observed turn count and must seed the next ordinal.
    fs::write(&events_path, []).expect("simulate empty rewritten event log");

    let reopened = open(dir.path(), "sess-cap-crash-empty", 0).expect("reopen after simulated crash");
    assert_eq!(reopened.turn_count(), 1);
    reopened
        .append(&ThreadEvent::TurnStarted(TurnStartedEvent::default()))
        .expect("append next turn start");
    reopened
        .append(&ThreadEvent::TurnCompleted(TurnCompletedEvent { usage: Usage::default() }))
        .expect("append next turn completion");
    assert_eq!(reopened.turn_count(), 2);
    assert_eq!(reopened.reconstruct_turn(2).expect("reconstruct next turn").len(), 2);
}

#[test]
fn pending_cap_rewrite_recovers_when_index_is_missing() {
    let dir = TempDir::new().expect("tempdir");
    let session_id = "sess-cap-crash-missing-index";
    let session_dir = sessions_root(dir.path()).join(session_id);
    let events_path = session_dir.join("events.jsonl");
    {
        let log = open(dir.path(), session_id, 0).expect("open");
        for _ in 0..3 {
            log.append(&ThreadEvent::TurnStarted(TurnStartedEvent::default()))
                .expect("append turn start");
            log.append(&ThreadEvent::TurnCompleted(TurnCompletedEvent { usage: Usage::default() }))
                .expect("append turn completion");
        }
        log.flush().expect("flush before simulated rewrite");
    }

    let index_path = session_dir.join("index/turns.json");
    let index: serde_json::Value =
        serde_json::from_slice(&fs::read(&index_path).expect("read turn index")).expect("parse turn index");
    let truncate_offset = index["entries"][0]["end_offset"].as_u64().expect("first turn end offset");
    let bytes = fs::read(&events_path).expect("read event log");
    let new_file_len = bytes
        .len()
        .saturating_sub(usize::try_from(truncate_offset).expect("offset fits"));
    fs::write(
        session_dir.join("index/pending-cap-rewrite.json"),
        serde_json::to_vec(&serde_json::json!({
            "previous_file_len": bytes.len(),
            "new_file_len": new_file_len,
            "retained_turn_base": 2,
        }))
        .expect("serialize rewrite marker"),
    )
    .expect("write rewrite marker");
    fs::write(&events_path, &bytes[usize::try_from(truncate_offset).expect("offset fits")..])
        .expect("simulate shortened event log");
    fs::remove_file(index_path).expect("remove stale index");

    let reopened = open(dir.path(), session_id, 0).expect("reopen after simulated crash");
    assert_eq!(reopened.turn_count(), 3);
    assert!(reopened.reconstruct_turn(1).is_err(), "evicted ordinal must stay absent");
    assert_eq!(reopened.reconstruct_turn(2).expect("retained turn 2").len(), 2);
    assert_eq!(reopened.reconstruct_turn(3).expect("retained turn 3").len(), 2);
    assert!(!session_dir.join("index/pending-cap-rewrite.json").exists());
}

#[test]
fn pending_cap_rewrite_recovers_when_index_is_invalid() {
    let dir = TempDir::new().expect("tempdir");
    let session_id = "sess-cap-crash-invalid-index";
    let session_dir = sessions_root(dir.path()).join(session_id);
    let events_path = session_dir.join("events.jsonl");
    {
        let log = open(dir.path(), session_id, 0).expect("open");
        for _ in 0..3 {
            log.append(&ThreadEvent::TurnStarted(TurnStartedEvent::default()))
                .expect("append turn start");
            log.append(&ThreadEvent::TurnCompleted(TurnCompletedEvent { usage: Usage::default() }))
                .expect("append turn completion");
        }
        log.flush().expect("flush before simulated rewrite");
    }

    let index_path = session_dir.join("index/turns.json");
    let index: serde_json::Value =
        serde_json::from_slice(&fs::read(&index_path).expect("read turn index")).expect("parse turn index");
    let truncate_offset = index["entries"][0]["end_offset"].as_u64().expect("first turn end offset");
    let bytes = fs::read(&events_path).expect("read event log");
    let new_file_len = bytes
        .len()
        .saturating_sub(usize::try_from(truncate_offset).expect("offset fits"));
    fs::write(
        session_dir.join("index/pending-cap-rewrite.json"),
        serde_json::to_vec(&serde_json::json!({
            "previous_file_len": bytes.len(),
            "new_file_len": new_file_len,
            "retained_turn_base": 2,
        }))
        .expect("serialize rewrite marker"),
    )
    .expect("write rewrite marker");
    fs::write(&events_path, &bytes[usize::try_from(truncate_offset).expect("offset fits")..])
        .expect("simulate shortened event log");
    fs::write(index_path, b"{invalid index").expect("corrupt stale index");

    let reopened = open(dir.path(), session_id, 0).expect("reopen after simulated crash");
    assert_eq!(reopened.turn_count(), 3);
    assert!(reopened.reconstruct_turn(1).is_err(), "evicted ordinal must stay absent");
    assert_eq!(reopened.reconstruct_turn(2).expect("retained turn 2").len(), 2);
    assert_eq!(reopened.reconstruct_turn(3).expect("retained turn 3").len(), 2);
    assert!(!session_dir.join("index/pending-cap-rewrite.json").exists());
}

#[test]
fn multiple_open_handles_share_turn_state_and_file_lock() {
    let dir = TempDir::new().expect("tempdir");
    let first = open(dir.path(), "sess-shared-handles", 0).expect("open first handle");
    let second = open(dir.path(), "sess-shared-handles", 0).expect("open second handle");

    for event in [
        ThreadEvent::TurnStarted(TurnStartedEvent::default()),
        ThreadEvent::TurnCompleted(TurnCompletedEvent { usage: Usage::default() }),
    ] {
        first.append(&event).expect("append first turn");
    }
    for event in [
        ThreadEvent::TurnStarted(TurnStartedEvent::default()),
        ThreadEvent::TurnCompleted(TurnCompletedEvent { usage: Usage::default() }),
    ] {
        second.append(&event).expect("append second turn");
    }

    assert_eq!(first.turn_count(), 2);
    assert_eq!(second.turn_count(), 2);
    assert_eq!(first.reconstruct_turn(1).expect("first turn").len(), 2);
    assert_eq!(second.reconstruct_turn(2).expect("second turn").len(), 2);
}

#[test]
fn failed_eviction_summary_keeps_canonical_events() {
    let dir = TempDir::new().expect("tempdir");
    let hook: EvictionSummaryHook =
        Arc::new(|_| Err(SessionStoreError::io("summary", std::io::Error::other("summary backend unavailable"))));
    let log = open_with_eviction_summary(dir.path(), "sess-summary-failure", 2, hook).expect("open");
    for event in [
        ThreadEvent::TurnStarted(TurnStartedEvent::default()),
        ThreadEvent::TurnCompleted(TurnCompletedEvent { usage: Usage::default() }),
        ThreadEvent::TurnStarted(TurnStartedEvent::default()),
    ] {
        let _ = log.append(&event);
    }
    let error = log.append(&ThreadEvent::TurnCompleted(TurnCompletedEvent { usage: Usage::default() }));
    assert!(error.is_err());
    let events_path = sessions_root(dir.path()).join("sess-summary-failure/events.jsonl");
    assert_eq!(fs::read_to_string(events_path).expect("read canonical log").lines().count(), 4);
    assert_eq!(log.event_count(), 4);
}

#[cfg(unix)]
#[test]
fn session_artifacts_use_private_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().expect("tempdir");
    let log = open(dir.path(), "sess-private", DEFAULT_MAX_EVENTS).expect("open");
    for event in &sample_turn() {
        log.append(event).expect("append");
    }
    log.flush().expect("flush");

    let session_dir = sessions_root(dir.path()).join("sess-private");
    let mode = |path: &std::path::Path| fs::metadata(path).expect("metadata").permissions().mode() & 0o777;
    assert_eq!(mode(&session_dir), 0o700);
    assert_eq!(mode(&session_dir.join("derived")), 0o700);
    assert_eq!(mode(&session_dir.join("index")), 0o700);
    assert_eq!(mode(&session_dir.join("events.jsonl")), 0o600);
    assert_eq!(mode(&session_dir.join("manifest.json")), 0o600);
    assert_eq!(mode(&session_dir.join("index/turns.json")), 0o600);
}

#[test]
fn scan_fallback_when_manifest_missing() {
    let dir = TempDir::new().expect("tempdir");
    // Write events directly to events.jsonl without manifest/index.
    let session_dir = dir.path().join(".vtcode/sessions/sess-raw");
    let events_path = session_dir.join("events.jsonl");
    fs::create_dir_all(&session_dir).expect("mkdir");
    let events = [
        VersionedThreadEvent::new(ThreadEvent::ThreadStarted(ThreadStartedEvent { thread_id: "t-1".to_string() })),
        VersionedThreadEvent::new(ThreadEvent::TurnStarted(TurnStartedEvent::default())),
        VersionedThreadEvent::new(ThreadEvent::TurnCompleted(TurnCompletedEvent { usage: Usage::default() })),
    ];
    let lines: Vec<String> = events.iter().map(|v| serde_json::to_string(v).expect("ser")).collect();
    fs::write(&events_path, lines.join("\n") + "\n").expect("write raw events");

    let log = SessionEventLog::open(dir.path(), "sess-raw", DEFAULT_MAX_EVENTS).expect("open");
    assert_eq!(log.turn_count(), 1);
    let rebuilt = log.reconstruct_turn(1).expect("reconstruct");
    assert_eq!(rebuilt.len(), 2);
}

#[test]
fn scan_skips_malformed_lifecycle_payloads() {
    let dir = TempDir::new().expect("tempdir");
    let session_dir = dir.path().join(".vtcode/sessions/sess-invalid");
    let events_path = session_dir.join("events.jsonl");
    fs::create_dir_all(&session_dir).expect("mkdir");

    let valid_events = [
        VersionedThreadEvent::new(ThreadEvent::TurnStarted(TurnStartedEvent::default())),
        VersionedThreadEvent::new(ThreadEvent::TurnCompleted(TurnCompletedEvent { usage: Usage::default() })),
    ];
    let mut lines = vec![
        r#"{"schema_version":"0.11.0","event":{"type":"thread.started","thread_id":123}}"#.to_string(),
        r#"{"schema_version":"0.11.0","event":{"type":"turn.started","token_breakdown":"invalid"}}"#.to_string(),
        r#"{"schema_version":"0.11.0","event":{"type":"turn.completed","usage":"invalid"}}"#.to_string(),
    ];
    lines.extend(
        valid_events
            .iter()
            .map(|event| serde_json::to_string(event).expect("serialize")),
    );
    fs::write(&events_path, lines.join("\n") + "\n").expect("write raw events");

    let log = SessionEventLog::open(dir.path(), "sess-invalid", DEFAULT_MAX_EVENTS).expect("open");
    assert_eq!(log.event_count(), 2);
    assert_eq!(log.turn_count(), 1);
    assert_eq!(log.reconstruct_turn(1).expect("reconstruct").len(), 2);
}
