//! Session memory envelope + local compaction helpers, shared by every
//! compaction path (auto, manual `/compact`, model-switch, recovery, fork).
//!
//! This module was extracted from the binary unified runloop so that both the
//! binary runloop and the `vtcode-core` `AgentRunner` loop use a single
//! compaction path with identical continuity behavior.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::fs as async_fs;

use vtcode_config::context::default_max_context_tokens;
use vtcode_config::loader::VTCodeConfig;

use crate::compaction::CompactionConfig;
use crate::config::constants::tools as tool_names;
use crate::context::history_files::{HistoryFileManager, messages_to_history_messages};
use crate::core::agent::harness_artifacts::{current_task_path, read_evaluation_summary, read_spec_summary};
use crate::core::agent::steering::{
    MAX_APPLIED_FOLLOW_UP_INTENT_IDS, MAX_QUEUED_FOLLOW_UP_INTENTS, QueuedFollowUpIntent,
};
use crate::llm::provider::{LLMProvider, Message, MessageRole};
use crate::llm::utils::truncate_to_token_limit;
use crate::persistent_memory::{GroundedFactRecord, dedup_latest_facts, normalize_whitespace, truncate_for_fact};

pub const MEMORY_ENVELOPE_HEADER: &str = "[Session Memory Envelope]";
pub const MEMORY_ENVELOPE_SUFFIX: &str = ".memory.json";
pub const SESSION_MEMORY_ENVELOPE_SCHEMA_VERSION: u32 = 3;
pub const MEMORY_LIST_LIMIT: usize = 5;
pub const APPLIED_INTENT_WINDOW: usize = MAX_APPLIED_FOLLOW_UP_INTENT_IDS;
pub const DEDUPED_FILE_READ_NOTE: &str = "Older duplicate file read omitted during local compaction; a newer read of the same target slice is retained later in history.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryEnvelopePersistence {
    PersistToDisk,
    InMemoryOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryEnvelopePlacement {
    Start,
    BeforeLastUserOrSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionMemoryEnvelope {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub schema_version: Option<u32>,
    pub summary: String,
    #[serde(default)]
    pub objective: Option<String>,
    pub task_summary: Option<String>,
    pub spec_summary: Option<String>,
    pub evaluation_summary: Option<String>,
    #[serde(default)]
    pub verification_summary: Option<String>,
    #[serde(default)]
    pub constraints: Vec<String>,
    pub grounded_facts: Vec<GroundedFactRecord>,
    pub touched_files: Vec<String>,
    #[serde(default)]
    pub open_questions: Vec<String>,
    #[serde(default)]
    pub verification_todo: Vec<String>,
    #[serde(default)]
    pub delegation_notes: Vec<String>,
    /// Follow-up steering intents accepted but not yet represented by a
    /// tagged user message. The list is bounded to preserve FIFO recovery.
    #[serde(default)]
    pub pending_intents: Vec<QueuedFollowUpIntent>,
    /// Recently applied steering IDs used to avoid replaying an already
    /// durable instruction after restart or compaction.
    #[serde(default)]
    pub applied_intent_ids: Vec<String>,
    pub history_artifact_path: Option<String>,
    pub generated_at: String,
}

impl SessionMemoryEnvelope {
    /// Returns true if this envelope carries the same meaningful content as
    /// `other`. Generated timestamps and history artifact paths are ignored
    /// because they change even when the underlying session state does not.
    pub fn is_content_equivalent_to(&self, other: &SessionMemoryEnvelope) -> bool {
        self.session_id == other.session_id
            && self.schema_version == other.schema_version
            && self.summary == other.summary
            && self.objective == other.objective
            && self.task_summary == other.task_summary
            && self.spec_summary == other.spec_summary
            && self.evaluation_summary == other.evaluation_summary
            && self.verification_summary == other.verification_summary
            && self.constraints == other.constraints
            && self.grounded_facts == other.grounded_facts
            && self.touched_files == other.touched_files
            && self.open_questions == other.open_questions
            && self.verification_todo == other.verification_todo
            && self.delegation_notes == other.delegation_notes
            && self.pending_intents == other.pending_intents
            && self.applied_intent_ids == other.applied_intent_ids
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionMemoryEnvelopeUpdate {
    pub objective: Option<String>,
    pub constraints: Vec<String>,
    pub grounded_facts: Vec<GroundedFactRecord>,
    pub touched_files: Vec<String>,
    pub open_questions: Vec<String>,
    pub verification_todo: Vec<String>,
    pub delegation_notes: Vec<String>,
    /// Replace the durable pending-intent snapshot when supplied.
    pub pending_intents: Option<Vec<QueuedFollowUpIntent>>,
    /// IDs acknowledged since the previous envelope.
    pub applied_intent_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct TaskTrackerSnapshot {
    summary: Option<String>,
    objective: Option<String>,
    verification_summary: Option<String>,
    verification_todo: Vec<String>,
}

fn merge_dedup_push<T, K, F>(prior: &[T], updates: impl IntoIterator<Item = T>, limit: usize, key_fn: F) -> Vec<T>
where
    K: PartialEq,
    F: Fn(&T) -> K,
    T: Clone,
{
    let mut merged = prior.to_vec();
    for item in updates {
        if let Some(idx) = merged.iter().position(|e| key_fn(e) == key_fn(&item)) {
            merged.remove(idx);
        }
        merged.push(item);
    }
    let keep_from = merged.len().saturating_sub(limit);
    merged.into_iter().skip(keep_from).collect()
}

fn merge_touched_files(prior_envelope: Option<&SessionMemoryEnvelope>, touched_files: &[String]) -> Vec<String> {
    let prior = prior_envelope.map(|e| e.touched_files.as_slice()).unwrap_or(&[]);
    merge_dedup_push(prior, touched_files.iter().cloned(), usize::MAX, |s| s.clone())
}

fn merge_recent_strings(prior: &[String], updates: &[String], limit: usize) -> Vec<String> {
    let prior_normalized: Vec<_> = prior
        .iter()
        .map(|v| normalize_whitespace(v))
        .filter(|v| !v.is_empty())
        .collect();
    let updates_normalized: Vec<_> = updates
        .iter()
        .map(|v| normalize_whitespace(v))
        .filter(|v| !v.is_empty())
        .collect();
    merge_dedup_push(&prior_normalized, updates_normalized, limit, |s| s.to_ascii_lowercase())
}

fn merge_applied_intent_ids(prior: &[String], updates: &[String]) -> Vec<String> {
    merge_dedup_push(prior, updates.iter().cloned(), APPLIED_INTENT_WINDOW, |id| id.clone())
}

fn extract_constraints_from_summary(text: Option<&str>) -> Vec<String> {
    text.into_iter()
        .flat_map(|value| value.lines())
        .map(normalize_whitespace)
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            if let Some(rest) = line.strip_prefix("- ") {
                return Some(rest.trim().to_string());
            }
            line.strip_prefix("* ").map(|rest| rest.trim().to_string())
        })
        .take(MEMORY_LIST_LIMIT)
        .collect()
}

pub fn derive_continuity_summary(
    history: &[Message],
    prior_envelope: Option<&SessionMemoryEnvelope>,
    task_snapshot: &TaskTrackerSnapshot,
) -> String {
    let objective = task_snapshot
        .objective
        .as_deref()
        .or_else(|| prior_envelope.and_then(|e| e.objective.as_deref()))
        .filter(|s| !s.is_empty());

    let latest = history
        .iter()
        .rev()
        .filter(|message| message.role == MessageRole::User || message.role == MessageRole::Assistant)
        .find_map(|message| {
            let trimmed = normalize_whitespace(message.content.as_text().as_ref());
            (!trimmed.is_empty()).then_some((message.role.as_generic_str(), truncate_for_fact(&trimmed, 140)))
        });

    match (objective, latest) {
        (Some(obj), Some((role, text))) => {
            format!("Working on: {obj}. Latest {role} action: {text}.")
        }
        (Some(obj), None) => format!("Working on: {obj}. Session continuity preserved."),
        (None, Some((role, text))) => {
            format!("Latest {role} action: {text}.")
        }
        (None, None) => prior_envelope
            .map(|envelope| envelope.summary.clone())
            .unwrap_or_else(|| "Session continuity facts preserved.".to_string()),
    }
}

fn merge_grounded_facts(
    prior_envelope: Option<&SessionMemoryEnvelope>,
    original_history: &[Message],
    updates: &[GroundedFactRecord],
) -> Vec<GroundedFactRecord> {
    let mut merged = prior_envelope
        .map(|envelope| envelope.grounded_facts.clone())
        .unwrap_or_default();

    for fact in dedup_latest_facts(original_history, 5) {
        let normalized = normalize_whitespace(&fact.fact).to_ascii_lowercase();
        if let Some(existing_idx) = merged
            .iter()
            .position(|entry| normalize_whitespace(&entry.fact).to_ascii_lowercase() == normalized)
        {
            merged.remove(existing_idx);
        }
        merged.push(fact.clone());
    }

    for fact in updates {
        let normalized = normalize_whitespace(&fact.fact).to_ascii_lowercase();
        if let Some(existing_idx) = merged
            .iter()
            .position(|entry| normalize_whitespace(&entry.fact).to_ascii_lowercase() == normalized)
        {
            merged.remove(existing_idx);
        }
        merged.push(fact.clone());
    }

    let keep_from = merged.len().saturating_sub(5);
    merged.into_iter().skip(keep_from).collect()
}

#[allow(
    clippy::too_many_arguments,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
pub fn build_session_memory_envelope(
    session_id: &str,
    workspace_root: &Path,
    original_history: &[Message],
    touched_files: &[String],
    summary: String,
    history_artifact_path: Option<&PathBuf>,
    prior_envelope: Option<&SessionMemoryEnvelope>,
    task_snapshot: &TaskTrackerSnapshot,
    envelope_update: Option<&SessionMemoryEnvelopeUpdate>,
) -> SessionMemoryEnvelope {
    let pe = prior_envelope;
    let spec_summary = read_spec_summary(workspace_root).or_else(|| pe.and_then(|e| e.spec_summary.clone()));
    let evaluation_summary =
        read_evaluation_summary(workspace_root).or_else(|| pe.and_then(|e| e.evaluation_summary.clone()));
    let merge = |prior: &[String], updates: &[String]| merge_recent_strings(prior, updates, MEMORY_LIST_LIMIT);
    let constraints = merge(
        pe.map(|e| e.constraints.as_slice()).unwrap_or(&[]),
        &extract_constraints_from_summary(spec_summary.as_deref()),
    );
    let constraints = merge(&constraints, &extract_constraints_from_summary(evaluation_summary.as_deref()));
    let update = envelope_update.cloned().unwrap_or_default();
    let pending_intents = update
        .pending_intents
        .map(|intents| intents.into_iter().take(MAX_QUEUED_FOLLOW_UP_INTENTS).collect())
        .or_else(|| pe.map(|envelope| envelope.pending_intents.clone()))
        .unwrap_or_default();
    let applied_intent_ids = merge_applied_intent_ids(
        pe.map(|envelope| envelope.applied_intent_ids.as_slice()).unwrap_or(&[]),
        &update.applied_intent_ids,
    );

    SessionMemoryEnvelope {
        session_id: session_id.to_string(),
        schema_version: Some(SESSION_MEMORY_ENVELOPE_SCHEMA_VERSION),
        summary,
        objective: update
            .objective
            .or_else(|| pe.and_then(|e| e.objective.clone()).or_else(|| task_snapshot.objective.clone())),
        task_summary: pe
            .and_then(|e| e.task_summary.clone())
            .or_else(|| task_snapshot.summary.clone()),
        spec_summary,
        evaluation_summary,
        verification_summary: task_snapshot
            .verification_summary
            .clone()
            .or_else(|| pe.and_then(|e| e.verification_summary.clone())),
        constraints: merge(&constraints, &update.constraints),
        grounded_facts: merge_grounded_facts(pe, original_history, &update.grounded_facts),
        touched_files: merge_touched_files(
            pe,
            &touched_files.iter().cloned().chain(update.touched_files).collect::<Vec<_>>(),
        ),
        open_questions: merge(pe.map(|e| e.open_questions.as_slice()).unwrap_or(&[]), &update.open_questions),
        verification_todo: merge(
            pe.map(|e| e.verification_todo.as_slice()).unwrap_or(&[]),
            &task_snapshot
                .verification_todo
                .iter()
                .cloned()
                .chain(update.verification_todo)
                .collect::<Vec<_>>(),
        ),
        delegation_notes: merge(pe.map(|e| e.delegation_notes.as_slice()).unwrap_or(&[]), &update.delegation_notes),
        pending_intents,
        applied_intent_ids,
        history_artifact_path: history_artifact_path
            .map(|p| p.display().to_string())
            .or_else(|| pe.and_then(|e| e.history_artifact_path.clone())),
        generated_at: Utc::now().to_rfc3339(),
    }
}

/// Persist the recoverable full-history artifact and build + inject a session
/// memory envelope into the compacted history. Returns the envelope (with the
/// artifact path) when one was produced.
pub fn persist_memory_envelope(
    workspace_root: &Path,
    session_id: &str,
    vt_cfg: Option<&VTCodeConfig>,
    original_history: &[Message],
    touched_files: &[String],
    compacted: &mut Vec<Message>,
    persistence: MemoryEnvelopePersistence,
    placement: MemoryEnvelopePlacement,
    seed_envelope: Option<&SessionMemoryEnvelope>,
) -> anyhow::Result<Option<SessionMemoryEnvelope>> {
    persist_memory_envelope_with_update(
        workspace_root,
        session_id,
        vt_cfg,
        original_history,
        touched_files,
        compacted,
        persistence,
        placement,
        seed_envelope,
        None,
    )
}

/// Variant of [`persist_memory_envelope`] that applies a live session update
/// while constructing the envelope. Keeping the compatibility wrapper above
/// avoids changing synchronous callers that do not track steering state.
#[allow(
    clippy::too_many_arguments,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
pub fn persist_memory_envelope_with_update(
    workspace_root: &Path,
    session_id: &str,
    vt_cfg: Option<&VTCodeConfig>,
    original_history: &[Message],
    touched_files: &[String],
    compacted: &mut Vec<Message>,
    persistence: MemoryEnvelopePersistence,
    placement: MemoryEnvelopePlacement,
    seed_envelope: Option<&SessionMemoryEnvelope>,
    envelope_update: Option<&SessionMemoryEnvelopeUpdate>,
) -> anyhow::Result<Option<SessionMemoryEnvelope>> {
    let should_persist = should_persist_memory_envelope(vt_cfg);
    if original_history.is_empty() || (!should_persist && persistence == MemoryEnvelopePersistence::PersistToDisk) {
        return Ok(None);
    }

    let task_snapshot = read_task_tracker_snapshot(workspace_root);
    let history_artifact_path = if should_persist && persistence == MemoryEnvelopePersistence::PersistToDisk {
        let mut hm = HistoryFileManager::new(workspace_root, session_id);
        let hm2 = messages_to_history_messages(original_history, 0);
        let hr = hm
            .write_history_sync(&hm2, original_history.len(), "compaction", touched_files, &[])
            .context("write compaction history artifact")?;
        Some(hr.file_path)
    } else {
        None
    };
    let loaded = if seed_envelope.is_none() {
        load_latest_memory_envelope(workspace_root, session_id)
    } else {
        None
    };
    let prior = seed_envelope.or(loaded.as_ref());
    let envelope = build_session_memory_envelope(
        session_id,
        workspace_root,
        original_history,
        touched_files,
        extract_compaction_summary(compacted, original_history),
        history_artifact_path.as_ref(),
        prior,
        &task_snapshot,
        envelope_update,
    );

    if let Some(hap) = history_artifact_path.as_ref() {
        write_memory_envelope_to_path(&memory_envelope_path_from_history_path(workspace_root, hap), &envelope)?;
    }
    apply_memory_envelope(compacted, &envelope, placement);
    Ok(Some(envelope))
}

/// Async counterpart to [`persist_memory_envelope`]. Compaction runs on the
/// async agent loop, so filesystem work must not block the runtime while the
/// recoverable history artifact or envelope is being written.
pub async fn persist_memory_envelope_async(
    workspace_root: &Path,
    session_id: &str,
    vt_cfg: Option<&VTCodeConfig>,
    original_history: &[Message],
    touched_files: &[String],
    compacted: &mut Vec<Message>,
    persistence: MemoryEnvelopePersistence,
    placement: MemoryEnvelopePlacement,
    seed_envelope: Option<&SessionMemoryEnvelope>,
) -> anyhow::Result<Option<SessionMemoryEnvelope>> {
    persist_memory_envelope_async_with_update(
        workspace_root,
        session_id,
        vt_cfg,
        original_history,
        touched_files,
        compacted,
        persistence,
        placement,
        seed_envelope,
        None,
    )
    .await
}

/// Async variant of [`persist_memory_envelope_async`] that applies a live
/// session update while constructing the envelope.
#[allow(
    clippy::too_many_arguments,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
pub async fn persist_memory_envelope_async_with_update(
    workspace_root: &Path,
    session_id: &str,
    vt_cfg: Option<&VTCodeConfig>,
    original_history: &[Message],
    touched_files: &[String],
    compacted: &mut Vec<Message>,
    persistence: MemoryEnvelopePersistence,
    placement: MemoryEnvelopePlacement,
    seed_envelope: Option<&SessionMemoryEnvelope>,
    envelope_update: Option<&SessionMemoryEnvelopeUpdate>,
) -> anyhow::Result<Option<SessionMemoryEnvelope>> {
    let should_persist = should_persist_memory_envelope(vt_cfg);
    if original_history.is_empty() || (!should_persist && persistence == MemoryEnvelopePersistence::PersistToDisk) {
        return Ok(None);
    }

    let task_snapshot = read_task_tracker_snapshot_async(workspace_root).await;
    let history_artifact_path = if should_persist && persistence == MemoryEnvelopePersistence::PersistToDisk {
        let mut history_manager = HistoryFileManager::new(workspace_root, session_id);
        let history_messages = messages_to_history_messages(original_history, 0);
        let history_result = history_manager
            .write_history(&history_messages, original_history.len(), "compaction", touched_files, &[])
            .await
            .context("write compaction history artifact")?;
        Some(history_result.file_path)
    } else {
        None
    };

    let loaded = if seed_envelope.is_none() {
        load_latest_memory_envelope_async(workspace_root, session_id).await
    } else {
        None
    };
    let prior = seed_envelope.or(loaded.as_ref());
    let envelope = build_session_memory_envelope(
        session_id,
        workspace_root,
        original_history,
        touched_files,
        extract_compaction_summary(compacted, original_history),
        history_artifact_path.as_ref(),
        prior,
        &task_snapshot,
        envelope_update,
    );

    if let Some(history_path) = history_artifact_path.as_ref() {
        write_memory_envelope_to_path_async(
            &memory_envelope_path_from_history_path(workspace_root, history_path),
            &envelope,
        )
        .await?;
    }
    apply_memory_envelope(compacted, &envelope, placement);
    Ok(Some(envelope))
}

pub fn should_persist_memory_envelope(vt_cfg: Option<&VTCodeConfig>) -> bool {
    vt_cfg.is_some_and(|cfg| cfg.context.dynamic.enabled && cfg.context.dynamic.persist_history)
}

fn memory_envelope_message(envelope: &SessionMemoryEnvelope) -> Message {
    let mut sections = Vec::new();
    sections.push(format!("{}\nSummary:\n{}", MEMORY_ENVELOPE_HEADER, envelope.summary.trim()));

    fn maybe_section(prefix: &str, content: Option<&str>) -> Option<String> {
        content
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| format!("{prefix}\n{s}"))
    }

    fn list_section(prefix: &str, items: &[String]) -> Option<String> {
        (!items.is_empty()).then(|| format!("{prefix}\n- {}", items.join("\n- ")))
    }

    if let Some(s) = maybe_section("Objective", envelope.objective.as_deref()) {
        sections.push(s);
    }
    if let Some(s) = maybe_section("Task Tracker", envelope.task_summary.as_deref()) {
        sections.push(s);
    }
    if let Some(s) = maybe_section("Spec Summary", envelope.spec_summary.as_deref()) {
        sections.push(s);
    }
    if let Some(s) = maybe_section("Evaluation Summary", envelope.evaluation_summary.as_deref()) {
        sections.push(s);
    }
    if let Some(s) = maybe_section("Verification Status", envelope.verification_summary.as_deref()) {
        sections.push(s);
    }
    if let Some(s) = list_section("Constraints", &envelope.constraints) {
        sections.push(s);
    }
    if let Some(s) = list_section("Touched Files", &envelope.touched_files) {
        sections.push(s);
    }

    if !envelope.grounded_facts.is_empty() {
        let facts: Vec<_> = envelope
            .grounded_facts
            .iter()
            .map(|f| format!("[{}] {}", f.source, f.fact.trim()))
            .collect();
        sections.push(format!("Grounded Facts:\n{}", facts.join("\n")));
    }
    if let Some(s) = list_section("Open Questions", &envelope.open_questions) {
        sections.push(s);
    }
    if let Some(s) = list_section("Verification Todo", &envelope.verification_todo) {
        sections.push(s);
    }
    if let Some(s) = list_section("Delegation Notes", &envelope.delegation_notes) {
        sections.push(s);
    }
    if let Some(s) = maybe_section("History Artifact", envelope.history_artifact_path.as_deref()) {
        sections.push(s);
    }

    Message::system(sections.join("\n\n"))
}

fn is_compaction_summary_message(message: &Message) -> bool {
    message.role == MessageRole::System && message.content.as_text().starts_with("Previous conversation summary:\n")
}

pub fn strip_existing_memory_envelope(history: &mut Vec<Message>) {
    history.retain(|message| {
        !(message.role == MessageRole::System && message.content.as_text().starts_with(MEMORY_ENVELOPE_HEADER))
    });
}

fn extract_compaction_summary(compacted: &[Message], original_history: &[Message]) -> String {
    if let Some(summary) = compacted.iter().find_map(|message| {
        if message.role != MessageRole::System {
            return None;
        }

        let text = message.content.as_text();
        let trimmed = text.trim();
        trimmed
            .strip_prefix("Previous conversation summary:\n")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    }) {
        return summary;
    }

    let mut recent = original_history
        .iter()
        .rev()
        .filter_map(|message| {
            let text = message.content.as_text();
            let trimmed = normalize_whitespace(text.as_ref());
            (!trimmed.is_empty()).then_some(format!(
                "{}: {}",
                message.role.as_generic_str(),
                truncate_for_fact(&trimmed, 160)
            ))
        })
        .take(4)
        .collect::<Vec<_>>();
    recent.reverse();

    if recent.is_empty() {
        "Compacted earlier conversation state and preserved continuity facts.".to_string()
    } else {
        format!("Compacted earlier conversation state. Recent preserved context: {}", recent.join(" | "))
    }
}

fn sanitize_session_id(session_id: &str) -> String {
    session_id
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(32)
        .collect()
}

fn memory_envelope_file_matches_session(name: &str, session_id: &str) -> bool {
    let session_prefix = sanitize_session_id(session_id);
    name == format!("{session_prefix}{MEMORY_ENVELOPE_SUFFIX}")
        || (name.starts_with(&format!("{session_prefix}_")) && name.ends_with(MEMORY_ENVELOPE_SUFFIX))
}

fn parse_task_tracker_snapshot(content: &str) -> TaskTrackerSnapshot {
    let title = content
        .lines()
        .find(|line| line.starts_with("# "))
        .map(|line| line.trim_start_matches("# ").trim().to_string());
    let checklist = content
        .lines()
        .filter(|line| line.trim_start().starts_with("- ["))
        .take(5)
        .map(normalize_whitespace)
        .collect::<Vec<_>>();
    let verification_summary = extract_verification_summary(content, &checklist);
    let verification_todo = content
        .lines()
        .filter(|line| line.trim_start().starts_with("- [ ]"))
        .take(MEMORY_LIST_LIMIT)
        .map(normalize_whitespace)
        .collect::<Vec<_>>();
    let summary = match (title.clone(), checklist.is_empty()) {
        (Some(title), false) => Some(format!("{title}: {}", checklist.join(" | "))),
        (Some(title), true) => Some(title),
        (None, false) => Some(checklist.join(" | ")),
        (None, true) => None,
    };

    TaskTrackerSnapshot {
        summary,
        objective: title,
        verification_summary,
        verification_todo,
    }
}

pub fn read_task_tracker_snapshot(workspace_root: &Path) -> TaskTrackerSnapshot {
    let tracker_path = current_task_path(workspace_root);
    fs::read_to_string(&tracker_path)
        .ok()
        .map(|content| parse_task_tracker_snapshot(&content))
        .unwrap_or_default()
}

/// Read the task tracker without blocking the async runtime.
pub async fn read_task_tracker_snapshot_async(workspace_root: &Path) -> TaskTrackerSnapshot {
    let tracker_path = current_task_path(workspace_root);
    async_fs::read_to_string(tracker_path)
        .await
        .ok()
        .map(|content| parse_task_tracker_snapshot(&content))
        .unwrap_or_default()
}

fn extract_verification_summary(content: &str, checklist: &[String]) -> Option<String> {
    let verify_commands = collect_structured_verify_commands(content);
    if !verify_commands.is_empty() {
        return Some(render_bullet_list(&verify_commands));
    }

    let fallback_lines = checklist
        .iter()
        .filter(|line| looks_like_verification_line(line))
        .cloned()
        .collect::<Vec<_>>();
    (!fallback_lines.is_empty()).then(|| fallback_lines.join("\n"))
}

fn collect_structured_verify_commands(content: &str) -> Vec<String> {
    let mut commands = Vec::new();
    let mut in_verify_block = false;

    for line in content.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("verify:") {
            let command = normalize_whitespace(rest);
            if command.is_empty() {
                in_verify_block = true;
            } else {
                commands.push(command);
                in_verify_block = false;
            }
            continue;
        }

        if !in_verify_block {
            continue;
        }

        if trimmed.is_empty() {
            continue;
        }

        if (line.starts_with(' ') || line.starts_with('\t')) && trimmed.starts_with("- ") {
            commands.push(normalize_whitespace(trimmed.trim_start_matches("- ")));
            continue;
        }

        in_verify_block = false;
    }

    commands
}

fn render_bullet_list(items: &[String]) -> String {
    items.iter().map(|item| format!("- {item}")).collect::<Vec<_>>().join("\n")
}

fn looks_like_verification_line(line: &str) -> bool {
    let lowered = line.to_ascii_lowercase();
    [
        "verify",
        "verification",
        "test",
        "lint",
        "cargo check",
        "check-dev.sh",
        "check.sh",
    ]
    .iter()
    .any(|keyword| lowered.contains(keyword))
}

fn memory_envelope_path_from_history_path(workspace_root: &Path, history_path: &Path) -> PathBuf {
    let absolute_history_path = if history_path.is_absolute() {
        history_path.to_path_buf()
    } else {
        workspace_root.join(history_path)
    };

    let file_name = absolute_history_path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| {
            if let Some(stem) = name.strip_suffix(".jsonl") {
                format!("{stem}{MEMORY_ENVELOPE_SUFFIX}")
            } else {
                format!("{name}{MEMORY_ENVELOPE_SUFFIX}")
            }
        })
        .unwrap_or_else(|| format!("session_memory{MEMORY_ENVELOPE_SUFFIX}"));

    let parent = absolute_history_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace_root.join(".vtcode").join("history"));
    parent.join(file_name)
}

pub fn default_memory_envelope_path_for_session(workspace_root: &Path, session_id: &str) -> PathBuf {
    workspace_root
        .join(".vtcode")
        .join("history")
        .join(format!("{}{MEMORY_ENVELOPE_SUFFIX}", sanitize_session_id(session_id)))
}

fn memory_envelope_paths_for_session(workspace_root: &Path, session_id: &str) -> Vec<PathBuf> {
    let history_dir = workspace_root.join(".vtcode").join("history");
    let mut candidates = fs::read_dir(history_dir)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.flatten())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| memory_envelope_file_matches_session(name, session_id))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        let left_modified = fs::metadata(left).and_then(|metadata| metadata.modified()).ok();
        let right_modified = fs::metadata(right).and_then(|metadata| metadata.modified()).ok();
        right_modified
            .cmp(&left_modified)
            .then_with(|| right.file_name().cmp(&left.file_name()))
    });
    candidates
}

pub fn latest_memory_envelope_path_for_session(workspace_root: &Path, session_id: &str) -> Option<PathBuf> {
    memory_envelope_paths_for_session(workspace_root, session_id)
        .into_iter()
        .find(|path| {
            fs::read_to_string(path)
                .ok()
                .and_then(|content| serde_json::from_str::<SessionMemoryEnvelope>(&content).ok())
                .is_some_and(|envelope| envelope.session_id.is_empty() || envelope.session_id == session_id)
        })
}

pub fn load_latest_memory_envelope(workspace_root: &Path, session_id: &str) -> Option<SessionMemoryEnvelope> {
    let path = latest_memory_envelope_path_for_session(workspace_root, session_id)?;
    let content = fs::read_to_string(path).ok()?;
    let envelope: SessionMemoryEnvelope = serde_json::from_str(&content).ok()?;
    if !envelope.session_id.is_empty() && envelope.session_id != session_id {
        return None;
    }
    Some(envelope)
}

async fn memory_envelope_paths_for_session_async(workspace_root: &Path, session_id: &str) -> Vec<PathBuf> {
    let history_dir = workspace_root.join(".vtcode").join("history");
    let mut candidates = Vec::new();
    let Ok(mut entries) = async_fs::read_dir(history_dir).await else {
        return candidates;
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let matches = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| memory_envelope_file_matches_session(name, session_id));
        if matches {
            candidates.push(path);
        }
    }

    let mut modified = Vec::with_capacity(candidates.len());
    for path in candidates {
        let timestamp = async_fs::metadata(&path)
            .await
            .ok()
            .and_then(|metadata| metadata.modified().ok());
        modified.push((timestamp, path));
    }
    modified.sort_by(|(left_time, left_path), (right_time, right_path)| {
        right_time
            .cmp(left_time)
            .then_with(|| right_path.file_name().cmp(&left_path.file_name()))
    });
    modified.into_iter().map(|(_, path)| path).collect()
}

/// Find and deserialize the newest valid envelope without synchronously
/// scanning historical candidates on the runtime thread.
pub async fn latest_memory_envelope_path_for_session_async(workspace_root: &Path, session_id: &str) -> Option<PathBuf> {
    for path in memory_envelope_paths_for_session_async(workspace_root, session_id).await {
        let Ok(content) = async_fs::read_to_string(&path).await else {
            continue;
        };
        let Ok(envelope) = serde_json::from_str::<SessionMemoryEnvelope>(&content) else {
            continue;
        };
        if envelope.session_id.is_empty() || envelope.session_id == session_id {
            return Some(path);
        }
    }
    None
}

pub async fn load_latest_memory_envelope_async(
    workspace_root: &Path,
    session_id: &str,
) -> Option<SessionMemoryEnvelope> {
    let path = latest_memory_envelope_path_for_session_async(workspace_root, session_id).await?;
    let content = async_fs::read_to_string(path).await.ok()?;
    let envelope: SessionMemoryEnvelope = serde_json::from_str(&content).ok()?;
    if !envelope.session_id.is_empty() && envelope.session_id != session_id {
        return None;
    }
    Some(envelope)
}

pub fn insert_memory_envelope_message(
    history: &mut Vec<Message>,
    envelope: &SessionMemoryEnvelope,
    placement: MemoryEnvelopePlacement,
) {
    let message = memory_envelope_message(envelope);
    match placement {
        MemoryEnvelopePlacement::Start => history.insert(0, message),
        MemoryEnvelopePlacement::BeforeLastUserOrSummary => {
            let insert_at = history
                .iter()
                .rposition(|item| item.role == MessageRole::User || is_compaction_summary_message(item))
                .unwrap_or(0);
            history.insert(insert_at, message);
        }
    }
}

fn apply_memory_envelope(
    compacted: &mut Vec<Message>,
    envelope: &SessionMemoryEnvelope,
    placement: MemoryEnvelopePlacement,
) {
    strip_existing_memory_envelope(compacted);
    insert_memory_envelope_message(compacted, envelope, placement);
}

pub fn inject_latest_memory_envelope(workspace_root: &Path, session_id: &str, history: &mut Vec<Message>) -> bool {
    let Some(envelope) = load_latest_memory_envelope(workspace_root, session_id) else {
        return false;
    };

    strip_existing_memory_envelope(history);
    insert_memory_envelope_message(history, &envelope, MemoryEnvelopePlacement::Start);
    true
}

pub fn write_memory_envelope_to_path(path: &Path, envelope: &SessionMemoryEnvelope) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create memory envelope directory {}", parent.display()))?;
    }
    let serialized = serde_json::to_string_pretty(envelope)?;
    let temporary_path = memory_envelope_temporary_path(path);
    fs::write(&temporary_path, serialized).with_context(|| format!("write memory envelope {}", path.display()))?;
    replace_memory_envelope_file(&temporary_path, path)
        .with_context(|| format!("replace memory envelope {}", path.display()))?;
    Ok(())
}

/// Atomically replace an envelope from an async context. The temporary file is
/// created beside the destination so rename remains atomic on the same volume.
pub async fn write_memory_envelope_to_path_async(path: &Path, envelope: &SessionMemoryEnvelope) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        async_fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create memory envelope directory {}", parent.display()))?;
    }
    let serialized = serde_json::to_string_pretty(envelope)?;
    let temporary_path = memory_envelope_temporary_path(path);
    async_fs::write(&temporary_path, serialized)
        .await
        .with_context(|| format!("write memory envelope {}", path.display()))?;
    replace_memory_envelope_file_async(&temporary_path, path)
        .await
        .with_context(|| format!("replace memory envelope {}", path.display()))?;
    Ok(())
}

fn memory_envelope_temporary_path(path: &Path) -> PathBuf {
    let suffix = format!("{}.tmp-{}", std::process::id(), Utc::now().timestamp_nanos_opt().unwrap_or_default());
    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or("session.memory.json");
    path.with_file_name(format!("{file_name}.{suffix}"))
}

fn replace_memory_envelope_file(temporary_path: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    match fs::remove_file(destination) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    fs::rename(temporary_path, destination)
}

async fn replace_memory_envelope_file_async(temporary_path: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    match async_fs::remove_file(destination).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    async_fs::rename(temporary_path, destination).await
}

pub fn has_latest_memory_envelope(workspace_root: &Path, session_id: &str) -> bool {
    latest_memory_envelope_path_for_session(workspace_root, session_id).is_some()
}

// ---------------------------------------------------------------------------
// Local compaction configuration + zero-cost fork history
// ---------------------------------------------------------------------------

pub fn configured_retained_user_messages(vt_cfg: Option<&VTCodeConfig>) -> usize {
    vt_cfg.map(|cfg| cfg.context.dynamic.retained_user_messages).unwrap_or(4)
}

pub fn local_compaction_config(vt_cfg: Option<&VTCodeConfig>, always_summarize: bool) -> CompactionConfig {
    CompactionConfig {
        always_summarize,
        retained_user_messages: configured_retained_user_messages(vt_cfg),
        ..CompactionConfig::default()
    }
}

fn collect_zero_cost_retained_user_messages(
    history: &[Message],
    token_budget: usize,
    max_messages: usize,
) -> Vec<Message> {
    if token_budget == 0 || max_messages == 0 {
        return Vec::new();
    }

    let mut kept = Vec::new();
    let mut remaining = token_budget;

    for message in history.iter().rev() {
        if kept.len() >= max_messages {
            break;
        }
        if message.role != MessageRole::User || message.content.trim().is_empty() {
            continue;
        }

        let estimated = message.estimate_tokens();
        if estimated <= remaining {
            kept.push(message.clone());
            remaining = remaining.saturating_sub(estimated);
            continue;
        }

        if remaining > 4 {
            let truncated = truncate_to_token_limit(message.content.as_text().as_ref(), remaining.saturating_sub(4));
            let trimmed = truncated.trim();
            if !trimmed.is_empty() {
                kept.push(Message::user(trimmed.to_string()));
            }
        }
        break;
    }

    kept.reverse();
    kept
}

pub fn build_zero_cost_summarized_fork_history(
    source_history: &[Message],
    source_envelope: Option<&SessionMemoryEnvelope>,
    retained_user_messages: usize,
) -> Vec<Message> {
    let summary = source_envelope
        .map(|envelope| normalize_whitespace(&envelope.summary))
        .filter(|summary| !summary.is_empty())
        .unwrap_or_else(|| derive_continuity_summary(source_history, source_envelope, &TaskTrackerSnapshot::default()));

    let retained_users = collect_zero_cost_retained_user_messages(
        source_history,
        CompactionConfig::default().retained_user_message_tokens,
        retained_user_messages,
    );

    let mut compacted = Vec::with_capacity(retained_users.len().saturating_add(1));
    compacted.push(Message::system(format!("Previous conversation summary:\n{}", summary.trim())));
    compacted.extend(retained_users);
    compacted
}

// ---------------------------------------------------------------------------
// File-read de-duplication for local compaction
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FileReadDedupKey {
    target: String,
    start_line: Option<u64>,
    end_line: Option<u64>,
    spool_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileReadDedupCandidate {
    key: FileReadDedupKey,
    placeholder_content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileReadToolKind {
    ReadFile,
    UnifiedFileRead,
}

fn is_read_file_tool_name(tool_name: &str) -> bool {
    tool_name == tool_names::READ_FILE || tool_name.ends_with(".read_file")
}

fn collect_file_read_tool_kinds(history: &[Message]) -> HashMap<String, FileReadToolKind> {
    let mut kinds = HashMap::new();
    for message in history {
        let Some(tool_calls) = message.tool_calls.as_ref() else {
            continue;
        };
        for tc in tool_calls {
            let Some(tn) = tc.tool_name() else {
                continue;
            };
            let kind = if is_read_file_tool_name(tn) {
                Some(FileReadToolKind::ReadFile)
            } else if tn == tool_names::UNIFIED_FILE {
                tc.execution_arguments().ok().and_then(|args| {
                    args.get("action")
                        .and_then(Value::as_str)
                        .filter(|a| *a == "read")
                        .map(|_| FileReadToolKind::UnifiedFileRead)
                })
            } else {
                None
            };
            if let Some(k) = kind {
                kinds.insert(tc.id.clone(), k);
            }
        }
    }
    kinds
}

fn normalize_file_read_target(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.replace('\\', "/"))
}

fn build_file_read_dedup_key(payload: &Value) -> Option<FileReadDedupKey> {
    let obj = payload.as_object()?;
    if obj.get("items").is_some()
        || obj.get("error").is_some()
        || obj.get("spool_chunked").and_then(Value::as_bool).unwrap_or(false)
        || obj.get("has_more").and_then(Value::as_bool).unwrap_or(false)
    {
        return None;
    }
    let target = obj
        .get("file_path")
        .and_then(Value::as_str)
        .or_else(|| obj.get("path").and_then(Value::as_str))
        .and_then(normalize_file_read_target)?;
    Some(FileReadDedupKey {
        target,
        start_line: obj.get("start_line").and_then(Value::as_u64),
        end_line: obj.get("end_line").and_then(Value::as_u64),
        spool_path: obj
            .get("spool_path")
            .and_then(Value::as_str)
            .and_then(normalize_file_read_target),
    })
}

fn build_file_read_placeholder_content(payload: &Value, key: &FileReadDedupKey) -> String {
    let mut p = serde_json::Map::new();
    p.insert("deduped_read".into(), Value::Bool(true));
    p.insert("note".into(), Value::String(DEDUPED_FILE_READ_NOTE.to_string()));

    fn maybe_str(p: &mut serde_json::Map<String, Value>, payload: &Value, key: &str) {
        if let Some(s) = payload
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            p.insert(key.into(), Value::String(s.to_string()));
        }
    }

    maybe_str(&mut p, payload, "file_path");
    maybe_str(&mut p, payload, "path");
    if let Some(sl) = key.start_line {
        p.insert("start_line".into(), json!(sl));
    }
    if let Some(el) = key.end_line {
        p.insert("end_line".into(), json!(el));
    }
    if let Some(sp) = key.spool_path.as_deref() {
        p.insert("spool_path".into(), json!(sp));
    }
    Value::Object(p).to_string()
}

fn file_read_dedup_candidate(
    message: &Message,
    tool_kinds: &HashMap<String, FileReadToolKind>,
) -> Option<FileReadDedupCandidate> {
    if message.role != MessageRole::Tool {
        return None;
    }

    let kind = message
        .tool_call_id
        .as_deref()
        .and_then(|tool_call_id| tool_kinds.get(tool_call_id).copied())
        .or_else(|| {
            message
                .origin_tool
                .as_deref()
                .and_then(|tool_name| is_read_file_tool_name(tool_name).then_some(FileReadToolKind::ReadFile))
        })?;

    if !matches!(kind, FileReadToolKind::ReadFile | FileReadToolKind::UnifiedFileRead) {
        return None;
    }

    let payload: Value = serde_json::from_str(message.content.as_text().as_ref()).ok()?;
    let key = build_file_read_dedup_key(&payload)?;

    Some(FileReadDedupCandidate {
        placeholder_content: build_file_read_placeholder_content(&payload, &key),
        key,
    })
}

pub fn dedup_repeated_file_reads_for_local_compaction(history: &[Message]) -> Vec<Message> {
    let tool_kinds = collect_file_read_tool_kinds(history);
    let mut last_idx = HashMap::new();
    let mut candidates = Vec::new();
    for (i, msg) in history.iter().enumerate() {
        let Some(c) = file_read_dedup_candidate(msg, &tool_kinds) else {
            continue;
        };
        last_idx.insert(c.key.clone(), i);
        candidates.push((i, c));
    }
    let mut deduped = history.to_vec();
    let mut changed = false;
    for (idx, c) in candidates {
        if last_idx.get(&c.key).copied() == Some(idx) {
            continue;
        }
        if let Some(msg) = deduped.get_mut(idx) {
            msg.content = c.placeholder_content.into();
            changed = true;
        }
    }
    if changed { deduped } else { history.to_vec() }
}

// ---------------------------------------------------------------------------
// Threshold resolution (shared by every compaction trigger)
// ---------------------------------------------------------------------------

/// Conservative output reservation when the request does not declare a limit.
pub const DEFAULT_OUTPUT_RESERVE_TOKENS: usize = 4096;

/// Resolve a prompt threshold while preserving room for the next response.
pub fn resolve_compaction_threshold(configured_threshold: Option<u64>, context_size: usize) -> Option<u64> {
    resolve_compaction_threshold_with_reserve(configured_threshold, context_size, DEFAULT_OUTPUT_RESERVE_TOKENS)
}

pub fn resolve_compaction_threshold_with_reserve(
    configured_threshold: Option<u64>,
    context_size: usize,
    reserved_output_tokens: usize,
) -> Option<u64> {
    let configured = configured_threshold.filter(|value| *value > 0);
    if context_size == 0 {
        return configured;
    }
    let prompt_budget = context_size.saturating_sub(reserved_output_tokens).max(1) as u64;
    Some(configured.map_or(prompt_budget, |value| value.min(prompt_budget)))
}

/// Explicit trigger overrides can reduce, but never bypass, the session ceiling.
pub fn resolve_effective_compaction_threshold(
    configured_threshold: Option<u64>,
    provider_context_size: usize,
    session_context_budget: usize,
) -> Option<u64> {
    resolve_compaction_threshold(
        configured_threshold,
        effective_session_context_budget(provider_context_size, session_context_budget),
    )
}

/// Zero means unknown/unconfigured; positive limits are intersected.
#[must_use]
pub fn effective_session_context_budget(provider_context_size: usize, session_context_budget: usize) -> usize {
    match (provider_context_size > 0, session_context_budget > 0) {
        (true, true) => provider_context_size.min(session_context_budget),
        (true, false) => provider_context_size,
        (false, true) => session_context_budget,
        (false, false) => 0,
    }
}

/// Resolve catalog/dynamic model capacity and the provider's route-specific ceiling.
#[must_use]
pub fn effective_context_budget(vt_cfg: Option<&VTCodeConfig>, provider: &dyn LLMProvider, model: &str) -> usize {
    use crate::llm::model_resolver::{DynamicModelMeta, ModelResolver};

    let provider_capacity = provider.effective_context_size(model);
    // Resolve through the shared model capability path so catalog entries and
    // discovered model metadata participate in exactly the same calculation as
    // request construction. The provider remains a hard route-specific ceiling
    // because a catalog can describe a larger platform window than this endpoint
    // exposes (or an explicit `ContextWindowProvider` override can narrow it).
    let resolved_capacity = ModelResolver::resolve(
        Some(provider.name()),
        model,
        &[],
        Some(DynamicModelMeta {
            display_name: model.to_owned(),
            description: None,
            context_window: (provider_capacity > 0).then_some(provider_capacity),
        }),
    )
    .and_then(|resolved| resolved.context_window())
    // Custom provider names and dynamic model ids are intentionally not
    // required to exist in the built-in catalog. The provider trait has
    // already resolved the route-specific capacity, so keep that value when
    // catalog resolution cannot identify the route.
    .unwrap_or(provider_capacity);
    let capacity = effective_session_context_budget(resolved_capacity, provider_capacity);
    let session_budget = vt_cfg.map_or_else(default_max_context_tokens, |cfg| cfg.context.max_context_tokens);
    effective_session_context_budget(capacity, session_budget)
}

pub fn effective_compaction_threshold_with_reserve(
    vt_cfg: Option<&VTCodeConfig>,
    provider: &dyn LLMProvider,
    model: &str,
    reserved_output_tokens: usize,
) -> Option<usize> {
    resolve_compaction_threshold_with_reserve(
        vt_cfg.and_then(|cfg| cfg.agent.harness.auto_compaction_threshold_tokens),
        effective_context_budget(vt_cfg, provider, model),
        reserved_output_tokens,
    )
    .and_then(|value| usize::try_from(value).ok())
}

pub fn effective_compaction_threshold(
    vt_cfg: Option<&VTCodeConfig>,
    provider: &dyn LLMProvider,
    model: &str,
) -> Option<usize> {
    let reserve = provider
        .sampling_overrides(model)
        .max_tokens
        .map_or(DEFAULT_OUTPUT_RESERVE_TOKENS, |value| value as usize);
    effective_compaction_threshold_with_reserve(vt_cfg, provider, model, reserve)
}
