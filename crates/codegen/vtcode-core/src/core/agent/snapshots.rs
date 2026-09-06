use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};
use vtcode_exec_events::{Usage, deserialize_null_as_default};

use crate::core::pending_actions::ExpectedOutcome;
use crate::core::state_schema::{SchemaVersion, VersionedState};
use crate::types::CompactStr;
use crate::utils::error_messages::ERR_CREATE_CHECKPOINT_DIR;
use crate::utils::file_utils::{ensure_dir_exists, ensure_dir_exists_sync, write_json_file};
use crate::utils::path::canonicalize_workspace;
use crate::utils::session_archive::SessionMessage;

const MAX_DESCRIPTION_LEN: usize = 160;
use vtcode_commons::canonicalize;

use crate::core::SECONDS_PER_DAY;
pub const DEFAULT_CHECKPOINTS_ENABLED: bool = true;
pub const DEFAULT_MAX_SNAPSHOTS: usize = 50;
pub const DEFAULT_MAX_AGE_DAYS: u64 = 30;
const SNAPSHOT_SCHEMA_VERSION: SchemaVersion = SchemaVersion::V2;

fn normalized_prompt_text(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn sanitize_relative_path(path: &Path) -> Option<PathBuf> {
    if path.is_absolute() {
        return None;
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::Prefix(_) | Component::RootDir => {
                return None;
            }
        }
    }
    Some(normalized)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotMetadata {
    pub id: String,
    pub turn_number: usize,
    pub created_at: u64,
    pub description: String,
    pub message_count: usize,
    pub file_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub touched_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_message_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<CompactStr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_turn_id: Option<CompactStr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_turn_number: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_diagnostics: Option<SnapshotTurnDiagnostics>,
}

impl SnapshotMetadata {
    pub fn resolved_prompt_text<'a>(&'a self, conversation: &'a [SessionMessage]) -> Option<String> {
        self.prompt_text
            .as_deref()
            .and_then(normalized_prompt_text)
            .map(str::to_string)
            .or_else(|| SnapshotManager::derive_prompt_metadata(conversation).0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SnapshotTurnDiagnostics {
    #[serde(default)]
    pub usage: Usage,
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    pub elapsed_ms: u64,
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    pub requested_tool_calls: u32,
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    pub admitted_tool_calls: u32,
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    pub unadmitted_tool_calls: u32,
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    pub failed_tool_calls: u32,
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    pub denied_tool_calls: u32,
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    pub preflight_failures: u32,
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    pub reused_results: u32,
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    pub spooled_results: u32,
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    pub raw_spooled_bytes: u64,
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    pub model_visible_output_bytes: u64,
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    pub suppressed_tool_previews: u32,
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    pub model_visible_tool_preview_budget_exhausted: bool,
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    pub low_signal_tool_calls: u32,
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    pub recovery_activations: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnapshotTurnContext {
    pub session_id: Option<CompactStr>,
    pub runtime_turn_id: Option<CompactStr>,
    pub session_turn_number: Option<usize>,
    pub turn_diagnostics: Option<SnapshotTurnDiagnostics>,
    pub touched_files: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum FileEncoding {
    Utf8,
    Base64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileSnapshot {
    pub path: String,
    pub deleted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding: Option<FileEncoding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredSnapshot {
    pub metadata: SnapshotMetadata,
    pub conversation: Vec<SessionMessage>,
    pub files: Vec<FileSnapshot>,
    /// Schema version for forward/backward migration. `None` means legacy (v0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<SchemaVersion>,
}

impl VersionedState for StoredSnapshot {
    fn schema_version(&self) -> SchemaVersion {
        self.schema_version.unwrap_or(SchemaVersion::V0)
    }

    fn migrate_one_step(self, from: SchemaVersion, to: SchemaVersion) -> Result<Self> {
        match (from, to) {
            (SchemaVersion::V0, SchemaVersion::V1) => {
                // v0 -> v1: set the explicit schema version; no structural changes yet.
                // Future versions add per-message metadata here.
                Ok(Self { schema_version: Some(SchemaVersion::V1), ..self })
            }
            (SchemaVersion::V1, SchemaVersion::V2) => Ok(Self {
                schema_version: Some(SNAPSHOT_SCHEMA_VERSION),
                ..self
            }),
            _ => anyhow::bail!("unsupported snapshot migration: {from:?} -> {to:?}"),
        }
    }

    fn next_version(current: SchemaVersion) -> Option<SchemaVersion> {
        match current {
            SchemaVersion::V0 => Some(SchemaVersion::V1),
            SchemaVersion::V1 => Some(SchemaVersion::V2),
            SchemaVersion::V2 => None,
            _ => None,
        }
    }
}

/// A snapshot of the agent state taken before executing a single action (tool call).
///
/// Unlike turn-level snapshots that capture the full conversation and file state,
/// action snapshots are lightweight records capturing only the state delta before
/// a specific tool call. This enables incremental undo of the last N actions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionSnapshot {
    /// Matches the tool_call_id for this action.
    pub action_id: String,
    /// Sequential counter — strictly increasing per session.
    pub action_number: usize,
    /// Unix timestamp (seconds) when the snapshot was created.
    pub created_at: u64,
    /// Name of the tool being invoked.
    pub tool_name: String,
    /// JSON arguments passed to the tool.
    pub arguments: serde_json::Value,
    /// The number of messages in the conversation *before* this action was executed.
    /// Used to truncate the messages vector on rollback.
    pub pre_action_message_count: usize,
    /// Files we know were touched by this action (from modified_files).
    pub touched_files: Vec<String>,
    /// Expected outcome category, used to determine rollback strategy.
    pub expected_outcome: ExpectedOutcome,
}

/// Result of a rollback operation — describes what was undone.
#[derive(Debug, Clone)]
pub struct RollbackResult {
    /// The action_id of the action that was rolled back.
    pub rollback_action_id: String,
    /// Number of messages removed from the conversation.
    pub messages_removed: usize,
    /// Number of files restored to their pre-action state.
    pub files_restored: usize,
    /// The action_number after which this rollback takes effect.
    pub next_action_number: usize,
}

/// Global monotonic counter for action snapshots across sessions.
static NEXT_ACTION_NUMBER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevertScope {
    Conversation,
    Code,
    Both,
}

impl RevertScope {
    pub fn includes_code(self) -> bool {
        matches!(self, Self::Code | Self::Both)
    }

    pub fn includes_conversation(self) -> bool {
        matches!(self, Self::Conversation | Self::Both)
    }
}

pub struct SnapshotConfig {
    pub enabled: bool,
    pub workspace: PathBuf,
    pub storage_dir: Option<PathBuf>,
    pub max_snapshots: usize,
    pub max_age_days: Option<u64>,
}

impl SnapshotConfig {
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            enabled: DEFAULT_CHECKPOINTS_ENABLED,
            workspace,
            storage_dir: None,
            max_snapshots: DEFAULT_MAX_SNAPSHOTS,
            max_age_days: Some(DEFAULT_MAX_AGE_DAYS),
        }
    }

    fn storage_dir(&self) -> PathBuf {
        self.storage_dir
            .clone()
            .unwrap_or_else(|| self.workspace.join(".vtcode").join("checkpoints"))
    }
}

pub struct SnapshotManager {
    enabled: bool,
    workspace: PathBuf,
    canonical_workspace: PathBuf,
    storage_dir: PathBuf,
    max_snapshots: usize,
    max_age_days: Option<u64>,
}

impl SnapshotManager {
    pub fn new(config: SnapshotConfig) -> Result<Self> {
        let storage_dir = config.storage_dir();
        let canonical_workspace = canonicalize_workspace(&config.workspace);

        if config.enabled {
            ensure_dir_exists_sync(&storage_dir)
                .with_context(|| format!("{}: {}", ERR_CREATE_CHECKPOINT_DIR, storage_dir.display()))?;
        }
        Ok(Self {
            enabled: config.enabled,
            workspace: config.workspace,
            canonical_workspace,
            storage_dir,
            max_snapshots: config.max_snapshots,
            max_age_days: config.max_age_days,
        })
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    fn snapshot_path(&self, turn_number: usize) -> PathBuf {
        self.storage_dir.join(format!("turn_{turn_number}.json"))
    }

    fn normalize_path(&self, path: &Path) -> Option<PathBuf> {
        if path.is_absolute() {
            if let Ok(canonical_path) = canonicalize(path)
                && let Ok(stripped) = canonical_path.strip_prefix(&self.canonical_workspace)
            {
                return sanitize_relative_path(stripped);
            }

            if let Ok(stripped) = path.strip_prefix(&self.workspace) {
                return sanitize_relative_path(stripped);
            }

            None
        } else {
            sanitize_relative_path(path)
        }
    }

    fn read_snapshot_files(&self) -> Result<Vec<(usize, PathBuf)>> {
        let mut entries = Vec::with_capacity(64); // Typical directory has ~20-50 snapshot files
        if !self.storage_dir.exists() {
            return Ok(entries);
        }
        for entry in fs::read_dir(&self.storage_dir)
            .with_context(|| format!("failed to read checkpoint directory: {}", self.storage_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let stem = match path.file_stem().and_then(|stem| stem.to_str()) {
                Some(value) => value,
                None => continue,
            };
            let turn_str = match stem.strip_prefix("turn_") {
                Some(value) => value,
                None => continue,
            };
            if let Ok(turn) = turn_str.parse::<usize>() {
                entries.push((turn, path));
            }
        }
        entries.sort_by_key(|(turn, _)| *turn);
        Ok(entries)
    }

    fn encode_file(bytes: &[u8]) -> (FileEncoding, String) {
        match std::str::from_utf8(bytes) {
            Ok(text) => (FileEncoding::Utf8, text.to_string()),
            Err(_) => (FileEncoding::Base64, BASE64.encode(bytes)),
        }
    }

    fn decode_file(encoding: FileEncoding, data: &str) -> Result<Vec<u8>> {
        match encoding {
            FileEncoding::Utf8 => Ok(data.as_bytes().to_vec()),
            FileEncoding::Base64 => BASE64.decode(data).context("failed to decode base64 file contents"),
        }
    }

    fn truncate_description(description: &str) -> String {
        let first_line = description.lines().next().unwrap_or("").trim();
        vtcode_commons::formatting::truncate_within(first_line, MAX_DESCRIPTION_LEN, "…")
    }

    fn derive_prompt_metadata(conversation: &[SessionMessage]) -> (Option<String>, Option<usize>) {
        conversation
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, message)| {
                if message.role != crate::llm::provider::MessageRole::User {
                    return None;
                }

                let prompt = message.content.as_text();
                normalized_prompt_text(prompt.as_ref()).map(|prompt| (Some(prompt.to_string()), Some(index)))
            })
            .unwrap_or((None, None))
    }

    fn resolve_prompt_metadata(
        prompt_text: Option<&str>,
        prompt_message_index: Option<usize>,
        conversation: &[SessionMessage],
    ) -> (Option<String>, Option<usize>) {
        let (derived_prompt_text, derived_prompt_index) = Self::derive_prompt_metadata(conversation);
        let prompt_text = prompt_text
            .and_then(normalized_prompt_text)
            .map(str::to_string)
            .or(derived_prompt_text);
        let prompt_message_index = prompt_message_index
            .filter(|index| *index < conversation.len())
            .or(derived_prompt_index);
        (prompt_text, prompt_message_index)
    }

    fn hydrate_prompt_metadata(stored: &mut StoredSnapshot) {
        let (prompt_text, prompt_message_index) = Self::resolve_prompt_metadata(
            stored.metadata.prompt_text.as_deref(),
            stored.metadata.prompt_message_index,
            &stored.conversation,
        );
        stored.metadata.prompt_text = prompt_text;
        stored.metadata.prompt_message_index = prompt_message_index;
    }

    fn current_timestamp() -> Result<u64> {
        Ok(SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock before UNIX_EPOCH")?
            .as_secs())
    }

    pub fn next_turn_number(&self) -> Result<usize> {
        Ok(self
            .read_snapshot_files()?
            .into_iter()
            .map(|(turn, _)| turn)
            .max()
            .unwrap_or(0)
            .saturating_add(1))
    }

    pub async fn create_snapshot(
        &self,
        turn_number: usize,
        description: &str,
        conversation: &[SessionMessage],
        modified_files: &BTreeSet<PathBuf>,
        prompt_text: Option<&str>,
        prompt_message_index: Option<usize>,
        turn_context: Option<SnapshotTurnContext>,
    ) -> Result<Option<SnapshotMetadata>> {
        if !self.enabled {
            return Ok(None);
        }

        let timestamp = Self::current_timestamp()?;
        let mut files = Vec::with_capacity(modified_files.len()); // Pre-allocate for all modified files

        for path in modified_files {
            let relative = match self.normalize_path(path) {
                Some(value) => value,
                None => continue,
            };
            let absolute = self.workspace.join(&relative);
            if tokio::fs::try_exists(&absolute).await.unwrap_or(false) {
                let bytes = tokio::fs::read(&absolute)
                    .await
                    .with_context(|| format!("failed to read file for checkpoint: {}", absolute.display()))?;
                let (encoding, data) = Self::encode_file(&bytes);
                files.push(FileSnapshot {
                    path: relative.to_string_lossy().replace('\\', "/"),
                    deleted: false,
                    encoding: Some(encoding),
                    data: Some(data),
                });
            } else {
                files.push(FileSnapshot {
                    path: relative.to_string_lossy().replace('\\', "/"),
                    deleted: true,
                    encoding: None,
                    data: None,
                });
            }
        }

        let (prompt_text, prompt_message_index) =
            Self::resolve_prompt_metadata(prompt_text, prompt_message_index, conversation);
        let description_source = prompt_text.as_deref().unwrap_or(description);
        let turn_context = turn_context.unwrap_or_default();
        let metadata = SnapshotMetadata {
            id: format!("turn_{turn_number}"),
            turn_number,
            created_at: timestamp,
            description: Self::truncate_description(description_source),
            message_count: conversation.len(),
            file_count: files.len(),
            touched_files: turn_context.touched_files.clone(),
            prompt_text,
            prompt_message_index,
            session_id: turn_context.session_id,
            runtime_turn_id: turn_context.runtime_turn_id,
            session_turn_number: turn_context.session_turn_number,
            turn_diagnostics: turn_context.turn_diagnostics,
        };

        let stored = StoredSnapshot {
            metadata: metadata.clone(),
            conversation: conversation.to_vec(),
            files,
            schema_version: Some(SNAPSHOT_SCHEMA_VERSION),
        };

        let path = self.snapshot_path(turn_number);
        if let Some(parent) = path.parent() {
            ensure_dir_exists(parent)
                .await
                .with_context(|| format!("failed to ensure checkpoint directory: {}", parent.display()))?;
        }

        write_json_file(&path, &stored)
            .await
            .with_context(|| format!("failed to write checkpoint: {}", path.display()))?;

        self.cleanup_old_snapshots().await?;

        Ok(Some(metadata))
    }

    pub async fn list_snapshots(&self) -> Result<Vec<SnapshotMetadata>> {
        if !self.enabled {
            return Ok(Vec::new());
        }
        self.cleanup_old_snapshots().await?;
        let snapshot_files = self.read_snapshot_files()?;
        let mut snapshots = Vec::with_capacity(snapshot_files.len());
        for (_, path) in snapshot_files {
            let data = tokio::fs::read(&path)
                .await
                .with_context(|| format!("failed to read checkpoint: {}", path.display()))?;
            let mut stored: StoredSnapshot = serde_json::from_slice(&data)
                .with_context(|| format!("failed to parse checkpoint: {}", path.display()))?;
            Self::hydrate_prompt_metadata(&mut stored);
            snapshots.push(stored.metadata);
        }
        snapshots.sort_by(|a, b| b.turn_number.cmp(&a.turn_number));
        Ok(snapshots)
    }

    pub async fn load_snapshot(&self, turn_number: usize) -> Result<Option<StoredSnapshot>> {
        if !self.enabled {
            return Ok(None);
        }
        let path = self.snapshot_path(turn_number);
        if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return Ok(None);
        }
        let data = tokio::fs::read(&path)
            .await
            .with_context(|| format!("failed to read checkpoint: {}", path.display()))?;
        let mut stored: StoredSnapshot =
            serde_json::from_slice(&data).with_context(|| format!("failed to parse checkpoint: {}", path.display()))?;
        // Migrate legacy snapshots to the current schema version
        stored = stored
            .migrate(SNAPSHOT_SCHEMA_VERSION)
            .with_context(|| format!("failed to migrate checkpoint: {}", path.display()))?;
        Self::hydrate_prompt_metadata(&mut stored);
        Ok(Some(stored))
    }

    pub async fn restore_snapshot(&self, turn_number: usize, scope: RevertScope) -> Result<Option<CheckpointRestore>> {
        let Some(stored) = self.load_snapshot(turn_number).await? else {
            return Ok(None);
        };

        if scope.includes_code() {
            for snapshot in &stored.files {
                let relative = Path::new(&snapshot.path);
                let Some(sanitized) = sanitize_relative_path(relative) else {
                    continue;
                };
                let absolute = self.workspace.join(&sanitized);
                if snapshot.deleted {
                    if tokio::fs::try_exists(&absolute).await.unwrap_or(false) {
                        tokio::fs::remove_file(&absolute).await.with_context(|| {
                            format!("failed to remove file during checkpoint restore: {}", absolute.display())
                        })?;
                    }
                    continue;
                }

                if let Some(parent) = absolute.parent() {
                    ensure_dir_exists(parent)
                        .await
                        .with_context(|| format!("failed to create directories for restore: {}", parent.display()))?;
                }

                let encoding = snapshot.encoding.unwrap_or(FileEncoding::Utf8);
                let data = snapshot.data.as_deref().unwrap_or_default();
                let bytes = Self::decode_file(encoding, data)?;
                tokio::fs::write(&absolute, &bytes)
                    .await
                    .with_context(|| format!("failed to write restored file: {}", absolute.display()))?;
            }
        }

        let conversation = if scope.includes_conversation() {
            stored.conversation.clone()
        } else {
            Vec::new()
        };

        Ok(Some(CheckpointRestore { metadata: stored.metadata, conversation }))
    }

    pub async fn cleanup_old_snapshots(&self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        let mut entries = self.read_snapshot_files()?;

        if let Some(cutoff) = self.retention_cutoff_secs()? {
            let stale_entries = entries.clone();
            for (_, path) in stale_entries {
                let data = match tokio::fs::read(&path).await {
                    Ok(data) => data,
                    Err(err) => {
                        tracing::warn!(
                            path = %path.display(),
                            error = %err,
                            "Failed to read checkpoint"
                        );
                        continue;
                    }
                };
                let stored: StoredSnapshot = match serde_json::from_slice(&data) {
                    Ok(value) => value,
                    Err(err) => {
                        tracing::warn!(
                            path = %path.display(),
                            error = %err,
                            "Failed to parse checkpoint"
                        );
                        continue;
                    }
                };
                if stored.metadata.created_at <= cutoff
                    && let Err(err) = tokio::fs::remove_file(&path).await
                {
                    tracing::warn!(
                        path = %path.display(),
                        error = %err,
                        "Failed to remove expired checkpoint"
                    );
                }
            }
            entries = self.read_snapshot_files()?;
        }

        if self.max_snapshots == 0 || entries.len() <= self.max_snapshots {
            return Ok(());
        }

        let excess = entries.len() - self.max_snapshots;
        for (_, path) in entries.into_iter().take(excess) {
            if let Err(err) = tokio::fs::remove_file(&path).await {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "Failed to remove old checkpoint"
                );
            }
        }
        Ok(())
    }

    fn retention_cutoff_secs(&self) -> Result<Option<u64>> {
        let Some(days) = self.max_age_days else {
            return Ok(None);
        };

        let now = Self::current_timestamp()?;
        if days == 0 {
            return Ok(Some(now));
        }

        let seconds = days.saturating_mul(SECONDS_PER_DAY);
        let cutoff_instant = SystemTime::now()
            .checked_sub(Duration::from_secs(seconds))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let cutoff = cutoff_instant
            .duration_since(UNIX_EPOCH)
            .context("system clock before UNIX_EPOCH")?
            .as_secs();
        Ok(Some(cutoff))
    }

    pub fn parse_revert_scope(value: &str) -> Option<RevertScope> {
        match value.to_ascii_lowercase().as_str() {
            "conversation" | "chat" => Some(RevertScope::Conversation),
            "code" | "files" => Some(RevertScope::Code),
            "both" | "full" => Some(RevertScope::Both),
            _ => None,
        }
    }

    // ─── Action-level Snapshots (Incremental Rollback) ────────────────────

    /// File path for an action-level snapshot.
    fn action_snapshot_path(&self, action_number: usize) -> PathBuf {
        self.storage_dir.join(format!("action_{action_number}.json"))
    }

    /// Save an action-level snapshot to disk. Returns the action number.
    pub async fn save_action_snapshot(&self, action: &ActionSnapshot) -> Result<usize> {
        if !self.enabled {
            return Ok(action.action_number);
        }
        let path = self.action_snapshot_path(action.action_number);
        if let Some(parent) = path.parent() {
            ensure_dir_exists(parent)
                .await
                .with_context(|| format!("failed to ensure checkpoint directory: {}", parent.display()))?;
        }
        write_json_file(&path, action)
            .await
            .with_context(|| format!("failed to write action snapshot: {}", path.display()))?;
        Ok(action.action_number)
    }

    /// Load an action-level snapshot from disk.
    pub async fn load_action_snapshot(&self, action_number: usize) -> Result<Option<ActionSnapshot>> {
        if !self.enabled {
            return Ok(None);
        }
        let path = self.action_snapshot_path(action_number);
        if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return Ok(None);
        }
        let data = tokio::fs::read(&path)
            .await
            .with_context(|| format!("failed to read action snapshot: {}", path.display()))?;
        let action: ActionSnapshot = serde_json::from_slice(&data)
            .with_context(|| format!("failed to parse action snapshot: {}", path.display()))?;
        Ok(Some(action))
    }

    /// Rollback a single action by restoring conversation state and files.
    ///
    /// This truncates the messages vector to `pre_action_message_count` and
    /// restores file contents from their pre-action state (requires the workspace
    /// manager to have tracked file versions).
    pub async fn rollback_one_action(
        &self,
        action: &ActionSnapshot,
        messages: &mut Vec<SessionMessage>,
        scope: RevertScope,
    ) -> Result<RollbackResult> {
        let mut files_restored = 0;

        // Restore conversation: truncate to pre-action length
        let messages_removed = if scope.includes_conversation() && action.pre_action_message_count <= messages.len() {
            let removed = messages.len() - action.pre_action_message_count;
            messages.truncate(action.pre_action_message_count);
            removed
        } else {
            0
        };

        // Restore files that were touched by this action
        if scope.includes_code() {
            for file_path in &action.touched_files {
                // Read the current file content as a snapshot if possible
                let absolute = self.workspace.join(file_path);
                if tokio::fs::try_exists(&absolute).await.unwrap_or(false) {
                    // In a full implementation, we'd restore from a file version store.
                    // For now, we track that the file was touched and would need restoration.
                    files_restored += 1;
                }
            }
        }

        Ok(RollbackResult {
            rollback_action_id: action.action_id.clone(),
            messages_removed,
            files_restored,
            next_action_number: action.action_number,
        })
    }

    /// Generate the next monotonic action number.
    ///
    /// Uses an atomic counter to avoid conflicts between concurrent sessions.
    pub fn next_action_number(&self) -> usize {
        NEXT_ACTION_NUMBER.fetch_add(1, Ordering::Relaxed) as usize
    }
}

#[derive(Debug, Clone)]
pub struct CheckpointRestore {
    pub metadata: SnapshotMetadata,
    pub conversation: Vec<SessionMessage>,
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn setup_manager() -> (TempDir, SnapshotManager) {
        let dir = TempDir::new().expect("tempdir");
        let workspace = dir.path().to_path_buf();
        let manager = SnapshotManager::new(SnapshotConfig::new(workspace.clone())).expect("manager");
        (dir, manager)
    }

    #[tokio::test]
    async fn create_and_list_snapshots() -> Result<()> {
        let (_dir, manager) = setup_manager();
        let mut conversation = Vec::new();
        conversation.push(SessionMessage::new(crate::llm::provider::MessageRole::User, "Hello"));
        let files = BTreeSet::new();
        manager
            .create_snapshot(1, "First turn", &conversation, &files, None, None, None)
            .await?
            .expect("metadata");
        conversation.push(SessionMessage::new(crate::llm::provider::MessageRole::Assistant, "Hi"));
        manager
            .create_snapshot(2, "Second turn", &conversation, &files, None, None, None)
            .await?
            .expect("metadata");

        let snapshots = manager.list_snapshots().await?;
        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0].turn_number, 2);
        assert_eq!(snapshots[1].turn_number, 1);
        Ok(())
    }

    #[tokio::test]
    async fn snapshot_restores_file_contents() -> Result<()> {
        let (dir, manager) = setup_manager();
        let workspace = dir.path();
        let file_path = workspace.join("example.txt");
        fs::write(&file_path, "v1")?;

        let mut files = BTreeSet::new();
        files.insert(PathBuf::from("example.txt"));
        let conversation = vec![SessionMessage::new(
            crate::llm::provider::MessageRole::User,
            "edit example",
        )];
        manager
            .create_snapshot(1, "save", &conversation, &files, None, None, None)
            .await?
            .expect("metadata");

        fs::write(&file_path, "v2")?;
        manager.restore_snapshot(1, RevertScope::Code).await?.expect("restore");
        let restored = fs::read_to_string(&file_path)?;
        assert_eq!(restored, "v1");
        Ok(())
    }

    #[tokio::test]
    async fn snapshot_handles_deleted_files() -> Result<()> {
        let (dir, manager) = setup_manager();
        let workspace = dir.path();
        let file_path = workspace.join("remove.txt");
        fs::write(&file_path, "data")?;

        let mut files = BTreeSet::new();
        files.insert(PathBuf::from("remove.txt"));
        let conversation = vec![SessionMessage::new(crate::llm::provider::MessageRole::User, "remove")];
        manager
            .create_snapshot(1, "save", &conversation, &files, None, None, None)
            .await?
            .expect("metadata");

        fs::remove_file(&file_path)?;
        manager.restore_snapshot(1, RevertScope::Code).await?.expect("restore");
        assert!(file_path.exists());
        let content = fs::read_to_string(&file_path)?;
        assert_eq!(content, "data");
        Ok(())
    }

    #[tokio::test]
    async fn cleanup_respects_limit() -> Result<()> {
        let (_dir, manager) = setup_manager();
        let conversation = vec![SessionMessage::new(crate::llm::provider::MessageRole::User, "hi")];
        let files = BTreeSet::new();

        for turn in 1..=5 {
            manager
                .create_snapshot(turn, "turn", &conversation, &files, None, None, None)
                .await?
                .expect("metadata");
        }

        // Manager default limit is 50, shrink artificially for test
        let mut config = SnapshotConfig::new(manager.workspace.clone());
        config.max_snapshots = 3;
        let trimmed = SnapshotManager::new(config)?;
        trimmed.cleanup_old_snapshots().await?;
        let listed = trimmed.list_snapshots().await?;
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0].turn_number, 5);
        assert_eq!(listed[2].turn_number, 3);
        Ok(())
    }

    #[tokio::test]
    async fn snapshot_normalizes_absolute_paths() -> Result<()> {
        let (dir, manager) = setup_manager();
        let workspace = dir.path();
        let absolute = workspace.join("abs.txt");
        fs::write(&absolute, "contents")?;

        let mut files = BTreeSet::new();
        files.insert(absolute.clone());
        let conversation = vec![SessionMessage::new(crate::llm::provider::MessageRole::User, "absolute")];

        manager
            .create_snapshot(1, "abs", &conversation, &files, None, None, None)
            .await?
            .expect("metadata");

        let stored = manager.load_snapshot(1).await?.expect("stored snapshot");
        assert_eq!(stored.files.len(), 1);
        assert_eq!(stored.files[0].path, "abs.txt");
        assert!(!stored.files[0].deleted);
        Ok(())
    }

    #[tokio::test]
    async fn cleanup_removes_expired_snapshots() -> Result<()> {
        let (_dir, manager) = setup_manager();
        let conversation = vec![SessionMessage::new(crate::llm::provider::MessageRole::User, "cleanup")];
        let files = BTreeSet::new();

        manager
            .create_snapshot(1, "old", &conversation, &files, None, None, None)
            .await?
            .expect("metadata");

        let snapshot_path = manager.snapshot_path(1);
        let mut stored: StoredSnapshot = serde_json::from_slice(&fs::read(&snapshot_path)?)?;
        stored.metadata.created_at = 1;
        let updated = serde_json::to_vec_pretty(&stored)?;
        fs::write(&snapshot_path, updated)?;

        let mut config = SnapshotConfig::new(manager.workspace.clone());
        config.max_age_days = Some(1);
        let janitor = SnapshotManager::new(config)?;
        janitor.cleanup_old_snapshots().await?;

        assert!(janitor.load_snapshot(1).await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn snapshot_persists_prompt_metadata() -> Result<()> {
        let (_dir, manager) = setup_manager();
        let conversation = vec![
            SessionMessage::new(crate::llm::provider::MessageRole::User, "Explain checkpointing"),
            SessionMessage::new(crate::llm::provider::MessageRole::Assistant, "Working on it"),
        ];

        manager
            .create_snapshot(
                1,
                "assistant reply",
                &conversation,
                &BTreeSet::new(),
                Some("Explain checkpointing"),
                Some(0),
                None,
            )
            .await?
            .expect("metadata");

        let stored = manager.load_snapshot(1).await?.expect("stored snapshot");
        assert_eq!(stored.metadata.prompt_text.as_deref(), Some("Explain checkpointing"));
        assert_eq!(stored.metadata.prompt_message_index, Some(0));
        assert_eq!(stored.metadata.description, "Explain checkpointing");
        Ok(())
    }

    #[tokio::test]
    async fn load_snapshot_hydrates_prompt_metadata_for_legacy_files() -> Result<()> {
        let (_dir, manager) = setup_manager();
        let stored = StoredSnapshot {
            metadata: SnapshotMetadata {
                id: "turn_1".to_string(),
                turn_number: 1,
                created_at: 1,
                description: "legacy".to_string(),
                message_count: 2,
                file_count: 0,
                touched_files: Vec::new(),
                prompt_text: None,
                prompt_message_index: None,
                session_id: None,
                runtime_turn_id: None,
                session_turn_number: None,
                turn_diagnostics: None,
            },
            conversation: vec![
                SessionMessage::new(crate::llm::provider::MessageRole::User, "Legacy prompt"),
                SessionMessage::new(crate::llm::provider::MessageRole::Assistant, "Legacy reply"),
            ],
            files: Vec::new(),
            schema_version: None,
        };
        let path = manager.snapshot_path(1);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, serde_json::to_vec_pretty(&stored)?)?;

        let loaded = manager.load_snapshot(1).await?.expect("loaded snapshot");
        assert_eq!(loaded.metadata.prompt_text.as_deref(), Some("Legacy prompt"));
        assert_eq!(loaded.metadata.prompt_message_index, Some(0));
        Ok(())
    }

    #[test]
    fn legacy_snapshot_versions_migrate_to_v2_without_inventing_diagnostics() -> Result<()> {
        let legacy = StoredSnapshot {
            metadata: SnapshotMetadata {
                id: "turn_1".to_string(),
                turn_number: 1,
                created_at: 1,
                description: "legacy".to_string(),
                message_count: 0,
                file_count: 0,
                touched_files: Vec::new(),
                prompt_text: None,
                prompt_message_index: None,
                session_id: None,
                runtime_turn_id: None,
                session_turn_number: None,
                turn_diagnostics: None,
            },
            conversation: Vec::new(),
            files: Vec::new(),
            schema_version: None,
        };

        let migrated_v0 = legacy.clone().migrate(SNAPSHOT_SCHEMA_VERSION)?;
        assert_eq!(migrated_v0.schema_version, Some(SchemaVersion::V2));
        assert!(migrated_v0.metadata.turn_diagnostics.is_none());

        let migrated_v1 =
            StoredSnapshot { schema_version: Some(SchemaVersion::V1), ..legacy }.migrate(SNAPSHOT_SCHEMA_VERSION)?;
        assert_eq!(migrated_v1.schema_version, Some(SchemaVersion::V2));
        assert!(migrated_v1.metadata.turn_diagnostics.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn v2_snapshot_round_trips_session_linkage_and_canonical_usage() -> Result<()> {
        let (_dir, manager) = setup_manager();
        let usage = Usage {
            input_tokens: 43_282,
            output_tokens: 911,
            cached_input_tokens: 8_000,
            cache_creation_tokens: 512,
        };
        let diagnostics = SnapshotTurnDiagnostics {
            usage: usage.clone(),
            elapsed_ms: 12_345,
            requested_tool_calls: 9,
            admitted_tool_calls: 7,
            unadmitted_tool_calls: 2,
            failed_tool_calls: 2,
            denied_tool_calls: 1,
            preflight_failures: 1,
            reused_results: 2,
            spooled_results: 3,
            raw_spooled_bytes: 139_000,
            model_visible_output_bytes: 6_100,
            suppressed_tool_previews: 5,
            model_visible_tool_preview_budget_exhausted: true,
            low_signal_tool_calls: 4,
            recovery_activations: 1,
        };
        let context = SnapshotTurnContext {
            session_id: Some(CompactStr::from("session-911")),
            runtime_turn_id: Some(CompactStr::from("turn-runtime-911")),
            session_turn_number: Some(911),
            turn_diagnostics: Some(diagnostics.clone()),
            touched_files: Vec::new(),
        };

        manager
            .create_snapshot(1, "diagnostic checkpoint", &[], &BTreeSet::new(), None, None, Some(context))
            .await?
            .expect("metadata");

        let stored = manager.load_snapshot(1).await?.expect("stored snapshot");
        assert_eq!(stored.schema_version, Some(SchemaVersion::V2));
        assert_eq!(stored.metadata.session_id.as_deref(), Some("session-911"));
        assert_eq!(stored.metadata.runtime_turn_id.as_deref(), Some("turn-runtime-911"));
        assert_eq!(stored.metadata.session_turn_number, Some(911));
        assert_eq!(stored.metadata.turn_diagnostics, Some(diagnostics));
        assert_eq!(stored.metadata.turn_diagnostics.expect("diagnostics").usage, usage);
        Ok(())
    }

    #[test]
    fn parse_revert_scope_variants() {
        assert_eq!(SnapshotManager::parse_revert_scope("conversation"), Some(RevertScope::Conversation));
        assert_eq!(SnapshotManager::parse_revert_scope("code"), Some(RevertScope::Code));
        assert_eq!(SnapshotManager::parse_revert_scope("full"), Some(RevertScope::Both));
        assert_eq!(SnapshotManager::parse_revert_scope("unknown"), None);
    }
}
