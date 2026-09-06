use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::SessionManifest;
use crate::TurnIndex;
use crate::error::SessionStoreError;

/// Durable intent record for an event-file cap rewrite.
///
/// The event file is published before the derived manifest and turn index. If
/// the process stops in that interval, this marker preserves the retained turn
/// ordinal even when the stale index cannot be trusted during recovery.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PendingCapRewrite {
    pub(crate) previous_file_len: u64,
    pub(crate) new_file_len: u64,
    pub(crate) retained_turn_base: u64,
}

/// Manifest persistence helpers.
///
/// Separated from `event_log` so the hot append path does not carry
/// serialization concerns, and so `open` can cheaply probe the manifest
/// before deciding whether to run the O(n) scan.
pub struct ManifestStore {
    session_dir: std::path::PathBuf,
}

impl ManifestStore {
    /// Create a new manifest store for the given session directory.
    pub(crate) fn new(session_dir: std::path::PathBuf) -> Self {
        Self { session_dir }
    }

    /// Path to `manifest.json` inside the session directory.
    fn manifest_path(&self) -> std::path::PathBuf {
        self.session_dir.join("manifest.json")
    }

    /// Path to `index/turns.json` inside the session directory.
    fn turns_path(&self) -> std::path::PathBuf {
        self.session_dir.join("index").join("turns.json")
    }

    /// Path to the cap-rewrite intent marker.
    fn pending_cap_rewrite_path(&self) -> std::path::PathBuf {
        self.session_dir.join("index").join("pending-cap-rewrite.json")
    }

    /// Load the manifest if it exists and is parseable.
    ///
    /// Returns `Ok(None)` when the file is missing (fresh session) or
    /// malformed, rather than erroring — the caller can fall back to scanning
    /// the event log.
    pub(crate) fn load_manifest(&self) -> Result<Option<SessionManifest>, SessionStoreError> {
        let path = self.manifest_path();
        let Some(bytes) = read_optional_private_file(&path)? else {
            return Ok(None);
        };
        Ok(serde_json::from_slice(&bytes).ok())
    }

    /// Load the turn index if it exists and is parseable.
    ///
    /// Returns `Ok(None)` when the file is missing or malformed.
    pub(crate) fn load_turn_index(&self) -> Result<Option<TurnIndex>, SessionStoreError> {
        let path = self.turns_path();
        let Some(bytes) = read_optional_private_file(&path)? else {
            return Ok(None);
        };
        Ok(serde_json::from_slice(&bytes).ok())
    }

    /// Load a pending cap-rewrite marker when it is present and valid.
    pub(crate) fn load_pending_cap_rewrite(&self) -> Result<Option<PendingCapRewrite>, SessionStoreError> {
        let path = self.pending_cap_rewrite_path();
        let Some(bytes) = read_optional_private_file(&path)? else {
            return Ok(None);
        };
        Ok(serde_json::from_slice(&bytes).ok())
    }

    /// Persist a cap-rewrite marker before replacing the canonical event file.
    pub(crate) fn write_pending_cap_rewrite(&self, pending: &PendingCapRewrite) -> Result<(), SessionStoreError> {
        let path = self.pending_cap_rewrite_path();
        let bytes = serde_json::to_vec(pending)?;
        vtcode_commons::VtCodePaths::write_private_file_atomic(&path, &bytes)
            .map_err(|error| SessionStoreError::io(path, std::io::Error::other(error)))
    }

    /// Remove a completed cap-rewrite marker.
    pub(crate) fn clear_pending_cap_rewrite(&self) -> Result<(), SessionStoreError> {
        let path = self.pending_cap_rewrite_path();
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(SessionStoreError::io(path, error)),
        }
    }
    /// Atomically write the manifest. Parent directories must already exist.
    pub(crate) fn write_manifest(&self, manifest: &SessionManifest) -> Result<(), SessionStoreError> {
        let path = self.manifest_path();
        let bytes = serde_json::to_vec(manifest)?;
        vtcode_commons::VtCodePaths::write_private_file_atomic(&path, &bytes)
            .map_err(|error| SessionStoreError::io(path.clone(), std::io::Error::other(error)))?;
        crate::query::invalidate_manifest_cache(&path);
        Ok(())
    }

    /// Atomically write the turn index. Parent directories must already exist.
    pub(crate) fn write_turn_index(&self, index: &TurnIndex) -> Result<(), SessionStoreError> {
        let path = self.turns_path();
        let bytes = serde_json::to_vec(index)?;
        vtcode_commons::VtCodePaths::write_private_file_atomic(&path, &bytes)
            .map_err(|error| SessionStoreError::io(path, std::io::Error::other(error)))
    }
}

fn read_optional_private_file(path: &Path) -> Result<Option<Vec<u8>>, SessionStoreError> {
    match fs::symlink_metadata(path) {
        Ok(_) => vtcode_commons::VtCodePaths::read_file_no_follow(path)
            .map(Some)
            .map_err(|error| SessionStoreError::io(path.to_path_buf(), std::io::Error::other(error))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(SessionStoreError::io(path.to_path_buf(), error)),
    }
}
