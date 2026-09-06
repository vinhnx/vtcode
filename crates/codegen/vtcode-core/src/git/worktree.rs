//! Git worktree management for loop isolation.
//!
//! A `WorktreeManager` creates, lists, and removes git worktrees under
//! `{workspace}/.vtcode/worktrees/`. Each parallel loop run gets its own
//! worktree so concurrent agents cannot collide on the working tree.

use anyhow::{Context, Result, anyhow};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use vtcode_commons::{canonicalize, fs::bound_file};

const WORKTREES_DIR_NAME: &str = "worktrees";

/// Build a Git command whose cwd is bound to an already validated directory
/// handle on Unix. The non-Unix implementation retains the validated path as
/// the child cwd because those platforms do not expose a portable descriptor
/// based `fchdir` primitive; callers still validate every path before this
/// helper is reached.
#[cfg(unix)]
fn git_command_at(directory: &Path) -> Result<(std::fs::File, Command)> {
    let handle = bound_file::open_directory_handle(directory)
        .with_context(|| format!("open handle-bound Git directory {}", directory.display()))?;
    let mut command = Command::new("git");
    bound_file::set_command_working_directory(&mut command, &handle)
        .with_context(|| format!("bind Git cwd to {}", directory.display()))?;
    Ok((handle, command))
}

#[cfg(not(unix))]
fn git_command_at(directory: &Path) -> Result<((), Command)> {
    let metadata = std::fs::symlink_metadata(directory)
        .with_context(|| format!("inspect validated Git directory {}", directory.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(anyhow!("Git directory is not a regular directory: {}", directory.display()));
    }

    let mut command = Command::new("git");
    command.current_dir(directory);
    Ok(((), command))
}

// ─── WorktreeInfo ────────────────────────────────────────────────────────────

/// Information about a discovered worktree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeInfo {
    /// The worktree name (directory name under `.vtcode/worktrees/`).
    pub name: String,
    /// Absolute path to the worktree working directory.
    pub path: PathBuf,
    /// The HEAD commit hash of the worktree.
    pub head: Option<String>,
    /// Whether the worktree has uncommitted changes.
    pub is_dirty: bool,
}

// ─── WorktreeManager ─────────────────────────────────────────────────────────

/// Manages git worktrees for loop isolation. Each worktree is an independent
/// checkout of the repository that can be worked on in parallel without
/// interfering with other worktrees or the main working tree.
pub struct WorktreeManager {
    workspace_root: PathBuf,
}

impl WorktreeManager {
    /// Create a new `WorktreeManager` for the given workspace.
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self { workspace_root: workspace_root.into() }
    }

    /// The directory where worktrees are stored.
    pub fn worktrees_dir(&self) -> PathBuf {
        self.workspace_root.join(".vtcode").join(WORKTREES_DIR_NAME)
    }

    /// Create a new worktree with the given name. Returns the path to the
    /// new worktree's working directory.
    ///
    /// The worktree is created under `.vtcode/worktrees/{name}/` on a new
    /// branch named `loop/{name}`.
    pub fn create(&self, name: &str) -> Result<PathBuf> {
        let (_lock, workspace_root) = self.acquire_operation_lock()?;
        self.create_locked(&workspace_root, name)
    }

    fn create_locked(&self, workspace_root: &Path, name: &str) -> Result<PathBuf> {
        let sanitized = sanitize_worktree_name(name);
        if sanitized.is_empty() {
            return Err(anyhow!("Worktree name cannot be empty after sanitization"));
        }

        let worktrees_dir = self.ensure_worktrees_dir_at(workspace_root)?;

        let worktree_path = worktrees_dir.join(&sanitized);
        match std::fs::symlink_metadata(&worktree_path) {
            Ok(_) => return Err(anyhow!("Worktree already exists at {}", worktree_path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("inspect worktree path {}", worktree_path.display()));
            }
        }

        bound_file::validate_directory_beneath(workspace_root, Path::new(".vtcode").join(WORKTREES_DIR_NAME).as_path())
            .with_context(|| format!("revalidate managed worktree directory {}", worktrees_dir.display()))?;

        let branch_name = format!("loop/{sanitized}");

        let (_worktrees_handle, mut command) = git_command_at(&worktrees_dir)?;
        let output = command
            .args(["worktree", "add", "-b", &branch_name, &sanitized])
            .output()
            .context("Failed to run git worktree add")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("git worktree add failed: {}", stderr.trim()));
        }

        bound_file::validate_directory_beneath(
            workspace_root,
            Path::new(".vtcode").join(WORKTREES_DIR_NAME).join(&sanitized).as_path(),
        )
        .with_context(|| format!("verify created worktree {}", worktree_path.display()))?;
        Ok(worktree_path)
    }

    /// Create a worktree and apply the current tracked and untracked changes
    /// from the source checkout. This keeps isolated executions aligned with
    /// the reviewable working tree even when the source changes are not
    /// committed yet.
    pub fn create_from_current(&self, name: &str) -> Result<PathBuf> {
        let (_lock, workspace_root) = self.acquire_operation_lock()?;
        let worktree_path = self.create_locked(&workspace_root, name)?;
        let sanitized = sanitize_worktree_name(name);
        let relative_worktree = Path::new(".vtcode").join(WORKTREES_DIR_NAME).join(&sanitized);
        bound_file::validate_directory_beneath(&workspace_root, &relative_worktree)
            .with_context(|| format!("revalidate created worktree {}", worktree_path.display()))?;
        let result = self.apply_current_snapshot(&workspace_root, &worktree_path);
        if let Err(error) = result {
            if let Err(cleanup_error) = self.remove_locked(&workspace_root, &sanitized) {
                tracing::debug!(
                    error = %cleanup_error,
                    worktree = %sanitized,
                    "failed to clean up worktree after snapshot failure"
                );
            }
            return Err(error);
        }
        Ok(worktree_path)
    }

    fn apply_current_snapshot(&self, workspace_root: &Path, worktree_path: &Path) -> Result<()> {
        let relative_worktree = worktree_path
            .strip_prefix(workspace_root)
            .with_context(|| format!("worktree path escapes workspace: {}", worktree_path.display()))?;
        bound_file::validate_directory_beneath(workspace_root, relative_worktree)
            .with_context(|| format!("validate snapshot worktree {}", worktree_path.display()))?;

        let (_workspace_handle, mut command) = git_command_at(workspace_root)?;
        let diff = command
            .args(["diff", "--binary", "HEAD", "--"])
            .output()
            .context("Failed to capture current git diff for worktree snapshot")?;
        if !diff.status.success() {
            return Err(anyhow!(
                "git diff failed while preparing worktree snapshot: {}",
                String::from_utf8_lossy(&diff.stderr).trim()
            ));
        }

        if !diff.stdout.is_empty() {
            // Re-open the no-follow directory chain directly before handing
            // the path to Git. The subsequent untracked-file copy remains
            // handle-bound on Unix, so a replaced destination cannot redirect
            // snapshot contents outside the workspace.
            bound_file::validate_directory_beneath(workspace_root, relative_worktree)
                .with_context(|| format!("revalidate snapshot worktree {}", worktree_path.display()))?;
            let (_worktree_handle, mut command) = git_command_at(worktree_path)?;
            let mut child = command
                .args(["apply", "--whitespace=nowarn", "-"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .context("Failed to apply current git diff to worktree snapshot")?;
            child
                .stdin
                .take()
                .context("git apply stdin was unavailable")?
                .write_all(&diff.stdout)
                .context("Failed to write git diff to worktree snapshot")?;
            let output = child
                .wait_with_output()
                .context("Failed to finish git apply for worktree snapshot")?;
            if !output.status.success() {
                return Err(anyhow!(
                    "git apply failed while preparing worktree snapshot: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }

            bound_file::validate_directory_beneath(workspace_root, relative_worktree)
                .with_context(|| format!("verify snapshot worktree {}", worktree_path.display()))?;
        }

        self.copy_untracked_snapshot_files(workspace_root, worktree_path)
    }

    #[cfg(unix)]
    fn copy_untracked_snapshot_files(&self, workspace_root: &Path, worktree_path: &Path) -> Result<()> {
        let relative_worktree = worktree_path
            .strip_prefix(workspace_root)
            .with_context(|| format!("worktree path escapes workspace: {}", worktree_path.display()))?;
        bound_file::validate_directory_beneath(workspace_root, relative_worktree)
            .with_context(|| format!("validate snapshot destination {}", worktree_path.display()))?;

        let (_workspace_handle, mut command) = git_command_at(workspace_root)?;
        let output = command
            .args(["ls-files", "--others", "--exclude-standard", "-z"])
            .output()
            .context("Failed to enumerate untracked files for worktree snapshot")?;
        if !output.status.success() {
            return Err(anyhow!(
                "git ls-files failed while preparing worktree snapshot: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }

        for relative_bytes in output.stdout.split(|byte| *byte == 0).filter(|path| !path.is_empty()) {
            let relative = std::str::from_utf8(relative_bytes).context("untracked file path is not valid UTF-8")?;
            let relative_path = Path::new(relative);
            if relative_path.is_absolute()
                || relative_path
                    .components()
                    .any(|component| matches!(component, std::path::Component::ParentDir))
            {
                return Err(anyhow!("untracked file path escapes the workspace: {relative}"));
            }

            let source = workspace_root.join(relative_path);
            let metadata = std::fs::symlink_metadata(&source)
                .with_context(|| format!("stat untracked snapshot file {}", source.display()))?;
            let destination_relative = relative_worktree.join(relative_path);
            if metadata.file_type().is_symlink() {
                let target = std::fs::read_link(&source)
                    .with_context(|| format!("read untracked symlink {}", source.display()))?;
                bound_file::create_symlink_beneath(workspace_root, &destination_relative, &target)
                    .with_context(|| format!("copy untracked symlink {}", source.display()))?;
            } else if metadata.is_file() {
                bound_file::copy_file_beneath(workspace_root, relative_path, workspace_root, &destination_relative)
                    .with_context(|| format!("copy untracked file {}", source.display()))?;
            }
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn copy_untracked_snapshot_files(&self, _workspace_root: &Path, _worktree_path: &Path) -> Result<()> {
        Err(anyhow!("handle-bound worktree snapshots are unavailable on this platform"))
    }

    /// List all worktrees managed by this instance (under `.vtcode/worktrees/`).
    pub fn list(&self) -> Result<Vec<WorktreeInfo>> {
        let (_lock, workspace_root) = self.acquire_operation_lock()?;
        self.list_locked(&workspace_root)
    }

    fn list_locked(&self, workspace_root: &Path) -> Result<Vec<WorktreeInfo>> {
        let Some(worktrees_dir) = self.validate_worktrees_dir_at(workspace_root)? else {
            return Ok(Vec::new());
        };

        let (_workspace_handle, mut command) = git_command_at(workspace_root)?;
        let output = command
            .args(["worktree", "list", "--porcelain"])
            .output()
            .context("Failed to run git worktree list")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("git worktree list failed: {}", stderr.trim()));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut worktrees = Vec::new();
        let mut current_path: Option<PathBuf> = None;
        let mut current_head: Option<String> = None;

        /// Build a WorktreeInfo if the path is under the managed directory.
        fn try_build_info(path: PathBuf, head: &mut Option<String>, managed_dir: &Path) -> Option<WorktreeInfo> {
            if !path.starts_with(managed_dir) {
                return None;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            Some(WorktreeInfo { name, path, head: head.take(), is_dirty: false })
        }

        for line in stdout.lines() {
            if let Some(rest) = line.strip_prefix("worktree ") {
                // If we have a pending entry, check if it's managed
                if let Some(path) = current_path.take() {
                    if let Some(info) = try_build_info(path, &mut current_head, &worktrees_dir) {
                        worktrees.push(info);
                    }
                }
                current_path = Some(PathBuf::from(rest));
                current_head = None;
            } else if let Some(rest) = line.strip_prefix("HEAD ") {
                current_head = Some(rest.to_string());
            } else if line == "detached" {
                // Detached HEAD state, head already set
            }
        }

        // Handle last entry
        if let Some(path) = current_path {
            if let Some(info) = try_build_info(path, &mut current_head, &worktrees_dir) {
                worktrees.push(info);
            }
        }

        // Check dirty status for each worktree
        for wt in &mut worktrees {
            let relative = wt
                .path
                .strip_prefix(workspace_root)
                .with_context(|| format!("worktree path escapes workspace: {}", wt.path.display()))?;
            bound_file::validate_directory_beneath(workspace_root, relative)
                .with_context(|| format!("revalidate listed worktree {}", wt.path.display()))?;
            let (_worktree_handle, mut command) =
                git_command_at(&wt.path).with_context(|| format!("open listed worktree {}", wt.path.display()))?;
            let output = command
                .args(["status", "--porcelain"])
                .output()
                .with_context(|| format!("inspect worktree status {}", wt.path.display()))?;
            if !output.status.success() {
                return Err(anyhow!(
                    "git status failed for {}: {}",
                    wt.path.display(),
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
            wt.is_dirty = !output.stdout.is_empty();
        }

        Ok(worktrees)
    }

    /// Remove a worktree by name. Runs `git worktree remove` with `--force`
    /// to handle worktrees with uncommitted changes.
    pub fn remove(&self, name: &str) -> Result<()> {
        let (_lock, workspace_root) = self.acquire_operation_lock()?;
        self.remove_locked(&workspace_root, name)
    }

    fn remove_locked(&self, workspace_root: &Path, name: &str) -> Result<()> {
        let sanitized = sanitize_worktree_name(name);
        if sanitized.is_empty() {
            return Err(anyhow!("Worktree name cannot be empty after sanitization"));
        }
        let Some(worktrees_dir) = self.validate_worktrees_dir_at(workspace_root)? else {
            return Err(anyhow!(
                "Worktree '{}' does not exist at {}",
                name,
                workspace_root
                    .join(".vtcode")
                    .join(WORKTREES_DIR_NAME)
                    .join(&sanitized)
                    .display()
            ));
        };
        let worktree_path = worktrees_dir.join(&sanitized);
        let relative_worktree = Path::new(".vtcode").join(WORKTREES_DIR_NAME).join(&sanitized);
        if let Err(error) = bound_file::validate_directory_beneath(workspace_root, &relative_worktree) {
            if error.kind() == std::io::ErrorKind::NotFound {
                return Err(anyhow!("Worktree '{}' does not exist at {}", name, worktree_path.display()));
            }
            return Err(error).with_context(|| format!("validate worktree path {}", worktree_path.display()));
        }

        bound_file::validate_directory_beneath(workspace_root, Path::new(".vtcode").join(WORKTREES_DIR_NAME).as_path())
            .with_context(|| format!("revalidate managed worktree directory {}", worktrees_dir.display()))?;

        let (_worktrees_handle, mut command) = git_command_at(&worktrees_dir)?;
        let output = command
            .args(["worktree", "remove", "--force", &sanitized])
            .output()
            .context("Failed to run git worktree remove")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("git worktree remove failed: {}", stderr.trim()));
        }

        // Clean up the orphan branch created by `create()`.
        let branch = format!("loop/{sanitized}");
        let (_workspace_handle, mut command) = git_command_at(workspace_root)?;
        let branch_output = command.args(["branch", "-D", &branch]).output();
        match branch_output {
            Ok(o) if !o.status.success() => {
                // Branch may not exist if the worktree was created externally;
                // log but do not fail the removal.
                tracing::debug!(
                    branch = %branch,
                    stderr = %String::from_utf8_lossy(&o.stderr),
                    "Could not delete orphan branch (may not exist)"
                );
            }
            Err(e) => {
                tracing::debug!(error = %e, "Failed to spawn git branch -D");
            }
            _ => {}
        }

        Ok(())
    }

    fn ensure_worktrees_dir_at(&self, workspace_root: &Path) -> Result<PathBuf> {
        let relative = Path::new(".vtcode").join(WORKTREES_DIR_NAME);
        bound_file::ensure_directory_beneath(workspace_root, &relative).with_context(|| {
            format!("create managed worktree directory {}", workspace_root.join(&relative).display())
        })?;
        Ok(workspace_root.join(relative))
    }

    fn acquire_operation_lock(&self) -> Result<(std::fs::File, PathBuf)> {
        let workspace_root = canonicalize(&self.workspace_root)
            .with_context(|| format!("canonicalize workspace root {}", self.workspace_root.display()))?;
        bound_file::ensure_directory_beneath(&workspace_root, Path::new(".vtcode"))
            .with_context(|| format!("create metadata directory {}", workspace_root.join(".vtcode").display()))?;
        let lock_path = Path::new(".vtcode").join("worktrees.lock");
        let lock = bound_file::open_lock_file_beneath(&workspace_root, &lock_path)
            .with_context(|| format!("open worktree operation lock {}", workspace_root.join(&lock_path).display()))?;
        lock.lock_exclusive()
            .with_context(|| format!("lock worktree operation file {}", workspace_root.join(&lock_path).display()))?;
        Ok((lock, workspace_root))
    }

    fn validate_worktrees_dir_at(&self, workspace_root: &Path) -> Result<Option<PathBuf>> {
        let relative = Path::new(".vtcode").join(WORKTREES_DIR_NAME);
        match bound_file::validate_directory_beneath(workspace_root, &relative) {
            Ok(()) => Ok(Some(workspace_root.join(relative))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| {
                format!("validate managed worktree directory {}", workspace_root.join(&relative).display())
            }),
        }
    }

    /// Remove all worktrees managed by this instance.
    pub fn remove_all(&self) -> Result<usize> {
        let (_lock, workspace_root) = self.acquire_operation_lock()?;
        let worktrees = self.list_locked(&workspace_root)?;
        let count = worktrees.len();
        for wt in &worktrees {
            self.remove_locked(&workspace_root, &wt.name)?;
        }
        Ok(count)
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn sanitize_worktree_name(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use vtcode_commons::canonicalize;

    #[test]
    fn sanitize_worktree_name_basic() {
        assert_eq!(sanitize_worktree_name("my-loop"), "my-loop");
        assert_eq!(sanitize_worktree_name("my_loop"), "my_loop");
        assert_eq!(sanitize_worktree_name("loop123"), "loop123");
    }

    #[test]
    fn sanitize_worktree_name_replaces_special_chars() {
        assert_eq!(sanitize_worktree_name("my loop"), "my-loop");
        assert_eq!(sanitize_worktree_name("path/to/thing"), "path-to-thing");
        assert_eq!(sanitize_worktree_name("a@b#c"), "a-b-c");
    }

    #[test]
    fn sanitize_worktree_name_trims_dashes() {
        assert_eq!(sanitize_worktree_name("--test--"), "test");
        assert_eq!(sanitize_worktree_name("  spaces  "), "spaces");
    }

    #[test]
    fn sanitize_worktree_name_empty_after_sanitize() {
        assert_eq!(sanitize_worktree_name("///"), "");
        assert_eq!(sanitize_worktree_name(""), "");
    }

    #[test]
    fn worktree_manager_worktrees_dir() {
        let mgr = WorktreeManager::new("/tmp/workspace");
        assert_eq!(mgr.worktrees_dir(), PathBuf::from("/tmp/workspace/.vtcode/worktrees"));
    }

    // Integration tests below exercise the real `git worktree` CLI against a
    // throwaway git repo, fulfilling the plan's B1 "create/list/remove against a
    // temp git repo" verification requirement (previously only sanitization was
    // covered).

    use std::process::Command as ProcCommand;

    /// Build a `WorktreeManager` from a canonicalized repo root.
    ///
    /// `git worktree list --porcelain` returns canonical (symlink-resolved)
    /// paths, so the manager must be constructed from a canonical root or its
    /// `starts_with(managed_dir)` filter would miss worktrees when the caller
    /// passes a symlinked path (e.g. macOS `/tmp` -> `/private/tmp`).
    fn manager_for(repo: &TempDir) -> WorktreeManager {
        WorktreeManager::new(canonicalize(repo.path()).expect("canonicalize repo"))
    }

    fn init_temp_git_repo() -> TempDir {
        let dir = TempDir::new().expect("temp dir");
        let run = |args: &[&str]| {
            let status = ProcCommand::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .expect("spawn git");
            assert!(status.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&status.stderr));
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "test@vtcode.dev"]);
        run(&["config", "user.name", "vtcode-test"]);
        std::fs::write(dir.path().join("README.md"), "seed\n").expect("write seed");
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "seed"]);
        dir
    }

    #[cfg(unix)]
    #[test]
    fn create_then_list_returns_managed_worktree() {
        let repo = init_temp_git_repo();
        let mgr = manager_for(&repo);

        let path = mgr.create("loop-a").expect("create");
        assert!(path.exists(), "worktree directory must exist after create");

        let worktrees = mgr.list().expect("list");
        let found = worktrees.iter().find(|w| w.name == "loop-a");
        assert!(found.is_some(), "created worktree should be listed");
        assert_eq!(found.unwrap().path, path);
    }

    #[cfg(unix)]
    #[test]
    fn create_is_idempotency_safe() {
        let repo = init_temp_git_repo();
        let mgr = manager_for(&repo);
        mgr.create("loop-b").expect("create first");

        let err = mgr.create("loop-b");
        assert!(err.is_err(), "re-creating an existing worktree must fail");
    }

    #[cfg(unix)]
    #[test]
    fn create_from_current_carries_tracked_and_untracked_changes() {
        let repo = init_temp_git_repo();
        std::fs::write(repo.path().join("README.md"), "working-tree\n").expect("modify tracked file");
        std::fs::create_dir_all(repo.path().join("nested")).expect("create untracked directory");
        std::fs::write(repo.path().join("nested").join("fixture.txt"), "untracked\n").expect("write untracked file");
        let mgr = manager_for(&repo);

        let path = mgr.create_from_current("loop-snapshot").expect("create snapshot");
        assert_eq!(std::fs::read_to_string(path.join("README.md")).expect("read tracked snapshot"), "working-tree\n");
        assert_eq!(
            std::fs::read_to_string(path.join("nested").join("fixture.txt")).expect("read untracked snapshot"),
            "untracked\n"
        );
        mgr.remove("loop-snapshot").expect("remove snapshot");
    }

    #[cfg(unix)]
    #[test]
    fn remove_deletes_worktree_and_orphan_branch() {
        let repo = init_temp_git_repo();
        let mgr = manager_for(&repo);
        mgr.create("loop-c").expect("create");

        mgr.remove("loop-c").expect("remove");
        assert!(
            mgr.list().expect("list").iter().all(|w| w.name != "loop-c"),
            "worktree should no longer be listed after removal"
        );

        // The orphan `loop/{name}` branch created by `create()` must be GC'd too.
        let branch_check = ProcCommand::new("git")
            .args(["rev-parse", "--verify", "loop/loop-c"])
            .current_dir(repo.path())
            .output()
            .expect("check branch");
        assert!(!branch_check.status.success(), "orphan branch should be deleted on remove");
    }

    #[cfg(unix)]
    #[test]
    fn remove_missing_worktree_errors() {
        let repo = init_temp_git_repo();
        let mgr = manager_for(&repo);
        assert!(mgr.remove("does-not-exist").is_err(), "removing a non-existent worktree must error");
    }

    #[cfg(unix)]
    #[test]
    fn remove_all_clears_every_managed_worktree() {
        let repo = init_temp_git_repo();
        let mgr = manager_for(&repo);
        mgr.create("loop-x").expect("create x");
        mgr.create("loop-y").expect("create y");

        let removed = mgr.remove_all().expect("remove_all");
        assert_eq!(removed, 2);
        assert!(mgr.list().expect("list").is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn managed_directory_symlink_is_rejected() {
        use std::os::unix::fs::symlink;

        let repo = init_temp_git_repo();
        let outside = TempDir::new().expect("outside temp dir");
        std::fs::create_dir_all(repo.path().join(".vtcode")).expect("create metadata directory");
        symlink(outside.path(), repo.path().join(".vtcode").join(WORKTREES_DIR_NAME))
            .expect("create managed directory symlink");
        let mgr = manager_for(&repo);

        assert!(mgr.create("loop-symlink").is_err());
        assert!(mgr.list().is_err());
        assert!(
            std::fs::read_dir(outside.path())
                .expect("read outside directory")
                .next()
                .is_none()
        );
    }
}
