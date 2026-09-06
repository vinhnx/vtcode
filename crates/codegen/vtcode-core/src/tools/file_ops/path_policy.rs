use super::FileOpsTool;
use crate::tools::jaro_winkler_similarity;
use anyhow::{Context, Result, anyhow};
use ignore::DirEntry;
use std::cmp::Ordering;
use std::future::Future;
use std::path::{Path, PathBuf};
use vtcode_commons::walk::{build_default_walker, is_excluded_dir};
use vtcode_commons::workspace_relative_display;

const MAX_PATH_SUGGESTIONS: usize = 3;
const MAX_PATH_SUGGESTION_SCAN: usize = 20_000;
const MIN_PATH_SUGGESTION_SCORE: f32 = 0.78;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PathSuggestionKind {
    Any,
    File,
}

impl PathSuggestionKind {
    fn matches(self, entry: &DirEntry) -> bool {
        match self {
            Self::Any => true,
            Self::File => entry.file_type().is_some_and(|ft| ft.is_file()),
        }
    }
}

fn normalize_path_for_suggestion(path: &str) -> String {
    path.replace('\\', "/").trim_matches('/').to_ascii_lowercase()
}

fn suggestion_basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn suggestion_score(requested_path: &str, candidate_path: &str) -> f32 {
    let requested_name = suggestion_basename(requested_path);
    let candidate_name = suggestion_basename(candidate_path);

    let full_score = jaro_winkler_similarity(requested_path, candidate_path);
    let name_score = if requested_name.is_empty() || candidate_name.is_empty() {
        0.0
    } else {
        jaro_winkler_similarity(requested_name, candidate_name)
    };

    let mut score = full_score.max(name_score * 0.85);

    if !requested_name.is_empty() && requested_name == candidate_name {
        score += 0.20;
    } else if !requested_name.is_empty()
        && (candidate_name.contains(requested_name) || requested_name.contains(candidate_name))
    {
        score += 0.06;
    }

    if candidate_path.ends_with(requested_path) || requested_path.ends_with(candidate_path) {
        score += 0.12;
    }

    score.min(1.0)
}

impl FileOpsTool {
    pub(super) fn canonical_workspace_root(&self) -> &PathBuf {
        &self.canonical_workspace_root
    }

    pub(super) fn workspace_relative_display(&self, path: &Path) -> String {
        workspace_relative_display(&self.workspace_root, path)
    }

    fn absolute_candidate(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.workspace_root.join(path)
        }
    }

    pub(super) async fn normalize_and_validate_user_path(&self, path: &str) -> Result<PathBuf> {
        self.normalize_and_validate_candidate(Path::new(path), path).await
    }

    pub(super) async fn normalize_and_validate_candidate(
        &self,
        path: &Path,
        original_display: &str,
    ) -> Result<PathBuf> {
        use crate::utils::path::normalize_path;
        let absolute = self.absolute_candidate(path);
        let normalized = normalize_path(&absolute);
        let normalized_root = normalize_path(&self.workspace_root);
        let canonical_root = normalize_path(self.canonical_workspace_root());

        let lexical_in_workspace = normalized.starts_with(&normalized_root);
        let lexical_in_canonical_workspace = normalized.starts_with(&canonical_root);

        // Callers may retain a path through an equivalent filesystem alias
        // (for example `/var` versus macOS's `/private/var`) after the
        // registry has canonicalized its workspace root. Resolve that alias
        // before applying containment; an outside path still fails the
        // canonical-root check below.
        if !lexical_in_workspace && !lexical_in_canonical_workspace {
            let canonical = self.canonicalize_allow_missing(&normalized).await?;
            if !canonical.starts_with(&canonical_root) {
                return Err(anyhow!("Error: Path '{original_display}' resolves outside the workspace."));
            }
            return Ok(canonical);
        }

        // Symlink-aware containment: validate every path component so a
        // symlink committed inside the workspace cannot resolve outside it.
        // The lexical tier above gives precise error messages for plain
        // traversal; this tier closes the escape-by-symlink case.
        if lexical_in_workspace {
            vtcode_commons::paths::ensure_path_within_workspace_resolved(&normalized, &self.workspace_root)
                .await
                .with_context(|| format!("Error: Path '{original_display}' is not accessible inside the workspace"))?;
        }

        let canonical = self.canonicalize_allow_missing(&normalized).await?;
        if !canonical.starts_with(&canonical_root) {
            return Err(anyhow!("Error: Path '{original_display}' resolves outside the workspace."));
        }
        Ok(canonical)
    }

    fn canonicalize_allow_missing<'a>(&'a self, normalized: &'a Path) -> impl Future<Output = Result<PathBuf>> + 'a {
        crate::utils::path::canonicalize_allow_missing(normalized)
    }

    pub(super) async fn resolve_file_path(&self, path: &str) -> Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        let requested = PathBuf::from(path);

        if requested.is_absolute() {
            paths.push(requested);
            return Ok(paths);
        }

        // Try exact path first
        paths.push(self.workspace_root.join(path));

        // If it's just a filename, try common directories that exist in most projects
        if !path.contains('/') && !path.contains('\\') {
            // Generic source directories found in most projects
            paths.push(self.workspace_root.join("src").join(path));
            paths.push(self.workspace_root.join("lib").join(path));
            paths.push(self.workspace_root.join("bin").join(path));
            paths.push(self.workspace_root.join("app").join(path));
            paths.push(self.workspace_root.join("source").join(path));
            paths.push(self.workspace_root.join("sources").join(path));
            paths.push(self.workspace_root.join("include").join(path));
            paths.push(self.workspace_root.join("docs").join(path));
            paths.push(self.workspace_root.join("doc").join(path));
            paths.push(self.workspace_root.join("examples").join(path));
            paths.push(self.workspace_root.join("example").join(path));
            paths.push(self.workspace_root.join("tests").join(path));
            paths.push(self.workspace_root.join("test").join(path));
        }

        // Try case-insensitive variants for filenames
        if !path.contains('/') && !path.contains('\\') {
            let path_lower = path.to_lowercase();
            if let Ok(mut entries) = tokio::fs::read_dir(&self.workspace_root).await {
                loop {
                    let entry: tokio::fs::DirEntry = match entries.next_entry().await {
                        Ok(Some(e)) => e,
                        _ => break,
                    };
                    let name = entry.file_name();
                    if let Ok(name_str) = name.into_string() {
                        if name_str.to_lowercase() == path_lower {
                            paths.push(entry.path());
                        }
                    }
                }
            }
        }

        Ok(paths)
    }

    pub(super) async fn missing_path_suggestion_suffix(
        &self,
        requested_path: &str,
        kind: PathSuggestionKind,
    ) -> String {
        let suggestions = self.suggest_workspace_paths(requested_path, kind).await;
        if suggestions.is_empty() {
            String::new()
        } else {
            format!(" Did you mean: {}?", suggestions.join(", "))
        }
    }

    async fn suggest_workspace_paths(&self, requested_path: &str, kind: PathSuggestionKind) -> Vec<String> {
        let requested_path = normalize_path_for_suggestion(requested_path);
        if requested_path.is_empty() || requested_path == "." {
            return Vec::new();
        }

        let mut scored_paths = Vec::with_capacity(MAX_PATH_SUGGESTIONS * 2);
        let mut scanned = 0usize;

        let walker = build_default_walker(&self.workspace_root)
            .filter_entry(|entry| !is_excluded_dir(entry))
            .build();

        for entry in walker {
            let Ok(entry) = entry else {
                continue;
            };
            if entry.depth() == 0 || !kind.matches(&entry) {
                continue;
            }

            scanned += 1;
            if scanned > MAX_PATH_SUGGESTION_SCAN {
                break;
            }

            let display_path = self.workspace_relative_display(entry.path());
            let normalized_candidate = normalize_path_for_suggestion(&display_path);
            if normalized_candidate.is_empty() || normalized_candidate == requested_path {
                continue;
            }

            let score = suggestion_score(&requested_path, &normalized_candidate);
            if score < MIN_PATH_SUGGESTION_SCORE {
                continue;
            }

            scored_paths.push((score, display_path));
        }

        scored_paths.sort_by(|left, right| {
            right
                .0
                .partial_cmp(&left.0)
                .unwrap_or(Ordering::Equal)
                .then_with(|| left.1.cmp(&right.1))
        });
        scored_paths.dedup_by(|left, right| left.1 == right.1);

        scored_paths
            .into_iter()
            .take(MAX_PATH_SUGGESTIONS)
            .map(|(_, path)| path)
            .collect()
    }

    /// Public helper to normalize and validate a user-provided path against the workspace root.
    /// Inline-delegating wrapper that returns the inner future directly to
    /// avoid an extra coroutine state machine (audit section 16).
    pub fn normalize_user_path<'a>(&'a self, path: &'a str) -> impl Future<Output = Result<PathBuf>> + 'a {
        self.normalize_and_validate_user_path(path)
    }
}

#[cfg(test)]
mod tests {
    use super::super::FileOpsTool;
    use crate::tools::grep_file::GrepSearchManager;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn make_tool(workspace: &TempDir) -> FileOpsTool {
        let grep_manager = Arc::new(GrepSearchManager::new(workspace.path().to_path_buf()));
        FileOpsTool::new(workspace.path().to_path_buf(), grep_manager)
    }

    fn canonical_root(workspace: &TempDir) -> PathBuf {
        dunce::canonicalize(workspace.path()).expect("canonicalize workspace root")
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_inside_workspace_pointing_outside_is_rejected() {
        let temp_dir = TempDir::new().expect("workspace tempdir");
        let outside = TempDir::new().expect("outside tempdir");
        fs::create_dir_all(temp_dir.path().join("sub")).expect("create sub");
        fs::write(outside.path().join("secret.txt"), "top secret").expect("write outside");

        std::os::unix::fs::symlink(outside.path(), temp_dir.path().join("sub/link")).expect("create symlink");

        let file_ops = make_tool(&temp_dir);

        let result = file_ops.normalize_user_path("sub/link/secret.txt").await;
        assert!(result.is_err(), "symlink escape must be rejected, got {result:?}");
    }

    #[tokio::test]
    async fn plain_paths_inside_workspace_still_validate() {
        let temp_dir = TempDir::new().expect("workspace tempdir");
        fs::create_dir_all(temp_dir.path().join("sub")).expect("create sub");
        fs::write(temp_dir.path().join("sub/file.txt"), "ok").expect("write file");
        let root = canonical_root(&temp_dir);

        let file_ops = make_tool(&temp_dir);

        let resolved = file_ops
            .normalize_user_path("sub/file.txt")
            .await
            .expect("existing in-workspace path must validate");
        assert!(resolved.starts_with(&root));

        // Missing files inside the workspace remain allowed (create flows).
        let created = file_ops
            .normalize_user_path("sub/new-file.txt")
            .await
            .expect("missing in-workspace path must validate");
        assert!(created.starts_with(&root));

        // Plain traversal stays rejected.
        assert!(file_ops.normalize_user_path("../outside.txt").await.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn canonical_alias_of_workspace_root_is_accepted() {
        let temp_dir = TempDir::new().expect("workspace tempdir");
        let canonical_root = canonical_root(&temp_dir);
        if canonical_root == temp_dir.path() {
            // No alias on this platform layout; nothing to test.
            return;
        }
        fs::create_dir_all(temp_dir.path().join("sub")).expect("create sub");
        fs::write(temp_dir.path().join("sub/file.txt"), "ok").expect("write file");

        let file_ops = make_tool(&temp_dir);

        // An absolute path written through the canonical (resolved) alias of
        // the workspace root must be accepted: it resolves inside the
        // workspace even though it does not start with the raw root.
        let alias_path = canonical_root.join("sub/file.txt");
        let resolved = file_ops
            .normalize_user_path(&alias_path.to_string_lossy())
            .await
            .expect("canonical alias path must validate");
        assert!(resolved.starts_with(&canonical_root));

        // A missing file through the alias stays allowed (create flows).
        let created = file_ops
            .normalize_user_path(&canonical_root.join("sub/new.txt").to_string_lossy())
            .await
            .expect("missing path via alias must validate");
        assert!(created.starts_with(&canonical_root));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn canonical_alias_escape_is_still_rejected() {
        let temp_dir = TempDir::new().expect("workspace tempdir");
        let outside = TempDir::new().expect("outside tempdir");
        let canonical_root = canonical_root(&temp_dir);
        if canonical_root == temp_dir.path() {
            return;
        }

        let file_ops = make_tool(&temp_dir);
        let outside_path = outside.path().join("secret.txt");
        fs::write(&outside_path, "top secret").expect("write outside");

        let result = file_ops.normalize_user_path(&outside_path.to_string_lossy()).await;
        assert!(result.is_err(), "outside canonical path must be rejected");
    }
}
