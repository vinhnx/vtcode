use crate::error::{Result, WebmcpError};
use crate::protocol::FileChange;
use crate::runtime::{
    AppliedChange, CheckResult, FileSnapshot, PatchProposal, RuntimeAdapter, RuntimeStatus, TurnResult, WorkspaceFile,
};
use async_trait::async_trait;
use hashbrown::HashMap as SandboxEnvironment;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;
use vtcode_commons::diff::{DiffHunk, DiffLineKind, DiffOptions, compute_diff};
use vtcode_commons::exclusions::{SENSITIVE_FILES, is_sensitive_file};
use vtcode_safety::sandboxing::{
    CommandSpec, ExecExpiration, SandboxManager, SandboxPolicy, SensitivePath, default_sensitive_paths,
};

const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CHECK_OUTPUT_BYTES: usize = 512 * 1024;
const MAX_DIRECTORY_DEPTH: usize = 64;
const MAX_VISITED_DIRECTORIES: usize = 16_384;
const MAX_CHECK_SENSITIVE_PATHS: usize = 4096;
const MAX_STORED_PROPOSALS: usize = 64;
const MAX_STORED_PROPOSAL_BYTES: usize = 128 * 1024 * 1024;
const IGNORED_DIRECTORY_NAMES: &[&str] = &[
    ".cargo",
    ".cache",
    ".codegraph",
    ".git",
    ".mypy_cache",
    ".opencode",
    ".pytest_cache",
    ".ruff_cache",
    ".superpowers",
    ".vscode",
    ".worktrees",
    ".vtcode",
    "__pycache__",
    "dist",
    "node_modules",
    "target",
];
const SENSITIVE_DIRECTORY_NAMES: &[&str] = &[
    ".aws",
    ".azure",
    ".config",
    ".docker",
    ".gnupg",
    ".kube",
    ".pki",
    ".secrets",
    ".ssh",
    ".terraform.d",
];

/// Bounds applied by the headless filesystem adapter.
#[derive(Debug, Clone, Copy)]
pub struct FilesystemLimits {
    /// Maximum number of files returned by a listing.
    pub max_files: usize,
    /// Maximum size of one UTF-8 file.
    pub max_file_bytes: usize,
    /// Maximum total bytes read by one listing or one proposal.
    pub max_total_bytes: usize,
    /// Maximum files in one proposal.
    pub max_changes: usize,
    /// Maximum proposal content bytes per file.
    pub max_change_bytes: usize,
}

impl Default for FilesystemLimits {
    fn default() -> Self {
        Self {
            max_files: 8192,
            max_file_bytes: 2 * 1024 * 1024,
            max_total_bytes: 64 * 1024 * 1024,
            max_changes: 32,
            max_change_bytes: 2 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone)]
struct StoredProposal {
    proposal: PatchProposal,
    before: Vec<FileSnapshot>,
    size_bytes: usize,
}

#[derive(Debug, Clone)]
struct StoredChange {
    change_id: String,
    before: Vec<FileSnapshot>,
    after: Vec<FileSnapshot>,
}

#[derive(Debug, Default)]
struct FilesystemState {
    proposals: std::collections::HashMap<String, StoredProposal>,
    proposal_bytes: usize,
    last_change: Option<StoredChange>,
}

/// A safe headless adapter rooted at one canonical workspace directory.
#[derive(Clone)]
pub struct FilesystemWorkspace {
    root: Arc<PathBuf>,
    root_dir: Arc<std::fs::File>,
    allowed_roots: Arc<Vec<PathBuf>>,
    limits: FilesystemLimits,
    mutations_allowed: bool,
    checks_allowed: bool,
    allowed_commands: Arc<HashSet<String>>,
    state: Arc<Mutex<FilesystemState>>,
    mutation_lock: Arc<AsyncMutex<()>>,
}

impl std::fmt::Debug for FilesystemWorkspace {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FilesystemWorkspace")
            .field("root", &self.root)
            .field("allowed_roots", &self.allowed_roots)
            .field("limits", &self.limits)
            .field("mutations_allowed", &self.mutations_allowed)
            .field("checks_allowed", &self.checks_allowed)
            .finish_non_exhaustive()
    }
}

impl FilesystemWorkspace {
    /// Construct an adapter. The current adapter exposes one canonical root;
    /// an empty allowlist means only the supplied root is visible.
    pub async fn new<I>(root: impl AsRef<Path>, allowed_roots: I, mutations_allowed: bool) -> Result<Self>
    where
        I: IntoIterator<Item = PathBuf>,
    {
        let root = vtcode_commons::canonicalize_async(root.as_ref().to_path_buf()).await?;
        if !tokio::fs::metadata(&root).await?.is_dir() {
            return Err(WebmcpError::PathRejected("workspace root is not a directory".to_string()));
        }

        let allowed_roots = allowed_roots.into_iter().collect::<Vec<_>>();
        if allowed_roots.len() > 1 {
            return Err(WebmcpError::InvalidRequest(
                "headless WebMCP currently supports one workspace root".to_string(),
            ));
        }
        let roots = if allowed_roots.is_empty() {
            vec![root.clone()]
        } else {
            let mut roots = Vec::with_capacity(allowed_roots.len());
            for allowed_root in allowed_roots {
                let canonical = vtcode_commons::canonicalize_async(allowed_root).await?;
                if !tokio::fs::metadata(&canonical).await?.is_dir() {
                    return Err(WebmcpError::PathRejected("allowed root is not a directory".to_string()));
                }
                roots.push(canonical);
            }
            roots
        };
        if !roots.iter().any(|allowed| root.starts_with(allowed)) {
            return Err(WebmcpError::PathRejected("workspace root is not in the allowed roots".to_string()));
        }
        let root_dir = open_root_directory(&root)?;
        if !root_dir.metadata()?.is_dir() {
            return Err(WebmcpError::PathRejected("workspace root is not a directory".to_string()));
        }

        Ok(Self {
            root: Arc::new(root),
            root_dir: Arc::new(root_dir),
            allowed_roots: Arc::new(roots),
            limits: FilesystemLimits::default(),
            mutations_allowed,
            checks_allowed: mutations_allowed,
            allowed_commands: Arc::new(["cargo"].into_iter().map(str::to_string).collect()),
            state: Arc::new(Mutex::new(FilesystemState::default())),
            mutation_lock: Arc::new(AsyncMutex::new(())),
        })
    }

    /// Replace the default workspace limits.
    pub fn with_limits(mut self, limits: FilesystemLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Replace the allowlisted check executables.
    pub fn with_allowed_commands<I, S>(mut self, commands: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allowed_commands = Arc::new(commands.into_iter().map(Into::into).collect());
        self
    }

    /// Configure whether the runtime may execute the allowlisted checks.
    pub fn with_checks_allowed(mut self, checks_allowed: bool) -> Self {
        self.checks_allowed = checks_allowed;
        self
    }

    /// Return a still-current proposal for an active runtime turn handoff.
    ///
    /// Rechecking the snapshots here prevents a browser proposal from being
    /// handed to the agent after an external edit occurred between proposal
    /// creation and turn submission.
    pub async fn proposal_for_turn(&self, proposal_id: &str) -> Result<PatchProposal> {
        let _mutation_guard = self.mutation_lock.lock().await;
        let stored = {
            let state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            state.proposals.get(proposal_id).cloned().ok_or(WebmcpError::ProposalNotFound)?
        };
        for expected in &stored.before {
            let current = self.read_snapshot(&expected.path).await?;
            if current.digest != expected.digest {
                return Err(WebmcpError::Conflict {
                    path: expected.path.clone(),
                    expected: expected.digest.clone(),
                    actual: current.digest,
                });
            }
        }
        Ok(stored.proposal)
    }

    /// Returns the canonical primary workspace path.
    pub fn root(&self) -> &Path {
        self.root.as_ref()
    }

    async fn read_snapshot(&self, path: &str) -> Result<FileSnapshot> {
        let root = self.root.clone();
        let root_dir = self.root_dir.clone();
        let path = path.to_string();
        let limits = self.limits;
        tokio::task::spawn_blocking(move || read_snapshot_blocking(&root, &root_dir, &path, limits))
            .await
            .map_err(|error| WebmcpError::Adapter(format!("file read task failed: {error}")))?
    }

    async fn replace_snapshot_if_current(&self, update: &FileSnapshot, expected: &FileSnapshot) -> Result<()> {
        let root = self.root.clone();
        let root_dir = self.root_dir.clone();
        let update = update.clone();
        let expected = expected.clone();
        tokio::task::spawn_blocking(move || replace_snapshot_if_current_blocking(&root, &root_dir, &update, &expected))
            .await
            .map_err(|error| WebmcpError::Adapter(format!("compare-and-replace task failed: {error}")))?
    }

    async fn apply_snapshots_if_current(&self, updates: &[FileSnapshot], expected: &[FileSnapshot]) -> Result<()> {
        if updates.len() != expected.len() {
            return Err(WebmcpError::Adapter("snapshot compare-and-swap lengths do not match".to_string()));
        }
        let mut applied = Vec::with_capacity(updates.len());
        for (index, (update, expected_snapshot)) in updates.iter().zip(expected).enumerate() {
            if let Err(error) = self.replace_snapshot_if_current(update, expected_snapshot).await {
                let rollback_error = self
                    .rollback_snapshots(&applied, expected.get(..index).unwrap_or(&[]))
                    .await
                    .err();
                return Err(rollback_error.unwrap_or(error));
            }
            applied.push(update.clone());
        }
        Ok(())
    }

    async fn apply_proposal_inner(&self, proposal_id: &str) -> Result<AppliedChange> {
        let _mutation_guard = self.mutation_lock.lock().await;
        let stored = {
            let state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            state.proposals.get(proposal_id).cloned().ok_or(WebmcpError::ProposalNotFound)?
        };

        let after = stored
            .proposal
            .changes
            .iter()
            .map(|change| FileSnapshot {
                path: change.path.clone(),
                content: change.content.clone(),
                digest: digest_text(&change.content),
            })
            .collect::<Vec<_>>();
        self.apply_snapshots_if_current(&after, &stored.before).await?;

        let change_id = Uuid::new_v4().simple().to_string();
        let result = AppliedChange {
            change_id: change_id.clone(),
            paths: after.iter().map(|snapshot| snapshot.path.clone()).collect(),
        };
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(removed) = state.proposals.remove(proposal_id) {
            state.proposal_bytes = state.proposal_bytes.saturating_sub(removed.size_bytes);
        }
        state.last_change = Some(StoredChange { change_id, before: stored.before, after });
        Ok(result)
    }

    async fn revert_last_change_inner(&self, change_id: &str) -> Result<AppliedChange> {
        let _mutation_guard = self.mutation_lock.lock().await;
        let stored = {
            let state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            state.last_change.clone().ok_or(WebmcpError::ChangeNotFound)?
        };
        if stored.change_id != change_id {
            return Err(WebmcpError::ChangeNotFound);
        }
        self.apply_snapshots_if_current(&stored.before, &stored.after).await?;
        let reverted = AppliedChange {
            change_id: stored.change_id.clone(),
            paths: stored.before.iter().map(|snapshot| snapshot.path.clone()).collect(),
        };
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        state.last_change = None;
        Ok(reverted)
    }

    async fn rollback_snapshots(&self, applied: &[FileSnapshot], originals: &[FileSnapshot]) -> Result<()> {
        let mut failed = false;
        for (applied_snapshot, original) in applied.iter().zip(originals).rev() {
            if let Err(error) = self.replace_snapshot_if_current(original, applied_snapshot).await {
                tracing::error!(
                    path = %original.path,
                    error = %error,
                    "failed to roll back a partially applied WebMCP change"
                );
                failed = true;
            }
        }
        if failed { Err(WebmcpError::PartialApply) } else { Ok(()) }
    }

    fn proposal_diff(before: &[FileSnapshot], changes: &[FileChange]) -> String {
        let mut diff = String::new();
        for (snapshot, change) in before.iter().zip(changes) {
            let bundle = compute_diff(
                &snapshot.content,
                &change.content,
                DiffOptions { context_lines: 3, ..DiffOptions::default() },
                |hunks, _| format_unified_hunks(hunks),
            );
            if bundle.is_empty {
                continue;
            }
            diff.push_str("--- a/");
            diff.push_str(&snapshot.path);
            diff.push('\n');
            diff.push_str("+++ b/");
            diff.push_str(&change.path);
            diff.push('\n');
            diff.push_str(&bundle.formatted);
        }
        diff
    }
}

fn format_unified_hunks(hunks: &[DiffHunk]) -> String {
    let mut output = String::new();
    for hunk in hunks {
        output.push_str("@@ -");
        output.push_str(&format_diff_range(hunk.old_start, hunk.old_lines));
        output.push_str(" +");
        output.push_str(&format_diff_range(hunk.new_start, hunk.new_lines));
        output.push_str(" @@\n");
        for line in &hunk.lines {
            let prefix = match line.kind {
                DiffLineKind::Context => ' ',
                DiffLineKind::Addition => '+',
                DiffLineKind::Deletion => '-',
            };
            output.push(prefix);
            let has_line_terminator = if let Some(content) = line.text.strip_suffix("\r\n") {
                output.push_str(content);
                output.push('\n');
                true
            } else if let Some(content) = line.text.strip_suffix('\n') {
                output.push_str(content);
                output.push('\n');
                true
            } else if let Some(content) = line.text.strip_suffix('\r') {
                output.push_str(content);
                output.push('\n');
                true
            } else {
                output.push_str(&line.text);
                output.push('\n');
                false
            };
            if !has_line_terminator {
                output.push_str(r"\ No newline at end of file");
                output.push('\n');
            }
        }
    }
    output
}

fn format_diff_range(start: usize, count: usize) -> String {
    if count == 0 {
        return format!("{},0", start.saturating_sub(1));
    }
    if count == 1 {
        return start.to_string();
    }
    format!("{start},{count}")
}

#[async_trait]
impl RuntimeAdapter for FilesystemWorkspace {
    async fn status(&self) -> Result<RuntimeStatus> {
        Ok(RuntimeStatus {
            workspace_root: self.root.display().to_string(),
            connected: true,
            turns_available: false,
            mutations_allowed: self.mutations_allowed,
            checks_allowed: self.checks_allowed,
            approval_authority: if self.mutations_allowed {
                "headless full-auto allowlist".into()
            } else {
                "headless policy (mutations disabled)".into()
            },
        })
    }

    async fn list_files(&self) -> Result<Vec<WorkspaceFile>> {
        let root = self.root.clone();
        let root_dir = self.root_dir.clone();
        let limits = self.limits;
        tokio::task::spawn_blocking(move || list_files_blocking(&root, &root_dir, limits))
            .await
            .map_err(|error| WebmcpError::Adapter(format!("file listing task failed: {error}")))?
    }

    async fn read_file(&self, path: &str) -> Result<FileSnapshot> {
        self.read_snapshot(path).await
    }

    async fn propose_changes(&self, changes: Vec<FileChange>) -> Result<PatchProposal> {
        let _mutation_guard = self.mutation_lock.lock().await;
        if changes.is_empty() || changes.len() > self.limits.max_changes {
            return Err(WebmcpError::LimitExceeded);
        }
        {
            let state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.proposals.len() >= MAX_STORED_PROPOSALS {
                return Err(WebmcpError::LimitExceeded);
            }
        }
        let mut paths = HashSet::with_capacity(changes.len());
        let mut before = Vec::with_capacity(changes.len());
        let mut proposal_bytes = 0usize;
        for change in &changes {
            if change.content.len() > self.limits.max_change_bytes || !paths.insert(change.path.clone()) {
                return Err(WebmcpError::LimitExceeded);
            }
            let snapshot = self.read_snapshot(&change.path).await?;
            if snapshot.digest != change.base_digest {
                return Err(WebmcpError::Conflict {
                    path: change.path.clone(),
                    expected: change.base_digest.clone(),
                    actual: snapshot.digest,
                });
            }
            proposal_bytes = proposal_bytes
                .checked_add(snapshot.content.len())
                .and_then(|bytes| bytes.checked_add(change.content.len()))
                .ok_or(WebmcpError::LimitExceeded)?;
            if proposal_bytes > self.limits.max_total_bytes {
                return Err(WebmcpError::LimitExceeded);
            }
            before.push(snapshot);
        }

        let unified_diff = Self::proposal_diff(&before, &changes);
        if unified_diff.len() > self.limits.max_total_bytes {
            return Err(WebmcpError::LimitExceeded);
        }
        let stored_size_bytes = proposal_bytes
            .checked_add(unified_diff.len())
            .ok_or(WebmcpError::LimitExceeded)?;
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if state
            .proposal_bytes
            .checked_add(stored_size_bytes)
            .is_none_or(|bytes| bytes > MAX_STORED_PROPOSAL_BYTES)
        {
            return Err(WebmcpError::LimitExceeded);
        }
        let proposal = PatchProposal {
            proposal_id: Uuid::new_v4().simple().to_string(),
            unified_diff,
            changes,
        };
        state.proposal_bytes += stored_size_bytes;
        drop(state.proposals.insert(
            proposal.proposal_id.clone(),
            StoredProposal {
                proposal: proposal.clone(),
                before,
                size_bytes: stored_size_bytes,
            },
        ));
        Ok(proposal)
    }

    async fn apply_proposal(&self, proposal_id: &str) -> Result<AppliedChange> {
        if !self.mutations_allowed {
            return Err(WebmcpError::ApprovalRequired);
        }
        let workspace = self.clone();
        let proposal_id = proposal_id.to_string();
        tokio::spawn(async move { workspace.apply_proposal_inner(&proposal_id).await })
            .await
            .map_err(|error| WebmcpError::Adapter(format!("WebMCP apply task failed: {error}")))?
    }

    async fn run_checks(&self, command: &str) -> Result<CheckResult> {
        let args = parse_safe_command(command, &self.allowed_commands)?;
        if !self.checks_allowed {
            return Err(WebmcpError::ApprovalRequired);
        }
        let _mutation_guard = self.mutation_lock.lock().await;
        let (program, arguments) = args
            .split_first()
            .ok_or_else(|| WebmcpError::InvalidRequest("check command cannot be empty".to_string()))?;
        let executable = resolve_check_executable(program, self.root.as_ref())?;
        // Capture host toolchain locations before replacing HOME with the
        // workspace sandbox HOME. This keeps checks reproducible without
        // allowing the child to inherit the caller's complete environment.
        let host_home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from);
        let mut environment = SandboxEnvironment::new();
        let _ = environment.insert("PATH".to_string(), trusted_executable_path(&executable)?);
        let _ = environment.insert("HOME".to_string(), self.root.display().to_string());
        let _ = environment.insert("CARGO_NET_OFFLINE".to_string(), "true".to_string());
        let _ = environment.insert("CARGO_TERM_COLOR".to_string(), "never".to_string());
        let cargo_home = host_home
            .as_ref()
            .and_then(|home| trusted_toolchain_directory(&home.join(".cargo"), self.root.as_ref()));
        if let Some(cargo_home) = cargo_home {
            let _ = environment.insert("CARGO_HOME".to_string(), cargo_home.to_string_lossy().into_owned());
        }
        let rustup_home = host_home
            .as_ref()
            .and_then(|home| trusted_toolchain_directory(&home.join(".rustup"), self.root.as_ref()));
        if let Some(rustup_home) = rustup_home {
            let _ = environment.insert("RUSTUP_HOME".to_string(), rustup_home.to_string_lossy().into_owned());
        }
        let spec = CommandSpec::new(executable)
            .with_args(arguments.iter().cloned())
            .with_cwd(self.root.as_ref().to_path_buf())
            .with_env(environment)
            .with_expiration(ExecExpiration::Timeout(CHECK_TIMEOUT));
        let sandbox_executable = std::env::var_os("VTCODE_LINUX_SANDBOX_EXECUTABLE").map(PathBuf::from);
        let check_policy = check_sandbox_policy(self.root.as_ref())
            .map_err(|error| WebmcpError::Adapter(format!("failed to build WebMCP check sandbox: {error}")))?;
        let exec_env = SandboxManager::new()
            .transform(spec, &check_policy, self.root.as_ref(), sandbox_executable.as_deref())
            .map_err(|error| WebmcpError::Adapter(format!("failed to sandbox WebMCP check: {error}")))?;
        let mut child = Command::new(exec_env.program)
            .args(exec_env.args)
            .current_dir(exec_env.cwd)
            .env_clear()
            .envs(exec_env.env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| WebmcpError::Adapter("check process stdout was not captured".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| WebmcpError::Adapter("check process stderr was not captured".to_string()))?;
        let output = Box::pin(tokio::time::timeout(CHECK_TIMEOUT, async {
            let (status, stdout, stderr) =
                tokio::join!(child.wait(), read_process_output(stdout), read_process_output(stderr));
            Ok::<_, WebmcpError>((status?, stdout?, stderr?))
        }))
        .await;
        let (status, stdout, stderr) = match output {
            Ok(result) => result?,
            Err(_elapsed) => {
                drop(child.kill().await);
                drop(child.wait().await);
                return Err(WebmcpError::Timeout(CHECK_TIMEOUT));
            }
        };
        Ok(CheckResult {
            command: command.to_string(),
            exit_code: status.code(),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    }

    async fn revert_last_change(&self, change_id: &str) -> Result<AppliedChange> {
        if !self.mutations_allowed {
            return Err(WebmcpError::ApprovalRequired);
        }
        let workspace = self.clone();
        let change_id = change_id.to_string();
        tokio::spawn(async move { workspace.revert_last_change_inner(&change_id).await })
            .await
            .map_err(|error| WebmcpError::Adapter(format!("WebMCP revert task failed: {error}")))?
    }

    async fn request_turn(&self, prompt: &str, _proposal_id: Option<&str>) -> Result<TurnResult> {
        if prompt.trim().is_empty() {
            return Err(WebmcpError::InvalidRequest("agent turn prompt cannot be empty".to_string()));
        }
        if prompt.len() > 16 * 1024 {
            return Err(WebmcpError::LimitExceeded);
        }
        Err(WebmcpError::Unsupported(
            "agent turns require an active VT Code runtime; start `vtcode chat` and run `/webmcp pair <origin>` in that same session. The standalone `vtcode webmcp serve` command exposes workspace operations only".to_string(),
        ))
    }
}

fn validate_relative_path(path: &str) -> Result<PathBuf> {
    if path.is_empty() || path.len() > 4096 || path.contains('\0') {
        return Err(WebmcpError::PathRejected(path.to_string()));
    }
    let path = Path::new(path);
    if path.is_absolute() {
        return Err(WebmcpError::PathRejected(path.display().to_string()));
    }
    let mut validated = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => validated.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(WebmcpError::PathRejected(path.display().to_string()));
            }
        }
    }
    if validated.as_os_str().is_empty() {
        return Err(WebmcpError::PathRejected(path.display().to_string()));
    }
    Ok(validated)
}

fn reject_sensitive_relative_path(relative: &Path, original: &str) -> Result<()> {
    if is_sensitive_relative_path(relative) {
        return Err(WebmcpError::PathRejected(format!("sensitive workspace path is not exposed: {original}")));
    }
    Ok(())
}

fn is_sensitive_relative_path(relative: &Path) -> bool {
    relative.components().any(|component| {
        let Component::Normal(name) = component else {
            return false;
        };
        let Some(name) = name.to_str() else {
            return true;
        };
        is_sensitive_file(name)
            || SENSITIVE_DIRECTORY_NAMES
                .iter()
                .any(|sensitive| name.eq_ignore_ascii_case(sensitive))
    })
}

#[cfg(unix)]
fn has_multiple_hard_links(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    metadata.nlink() > 1
}

#[cfg(not(unix))]
const fn has_multiple_hard_links(_metadata: &std::fs::Metadata) -> bool {
    false
}

fn reject_hard_links(metadata: &std::fs::Metadata, path: &str) -> Result<()> {
    if has_multiple_hard_links(metadata) {
        return Err(WebmcpError::PathRejected(format!("hard-linked file is not allowed: {path}")));
    }
    Ok(())
}

fn digest_text(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    let mut encoded = String::with_capacity(7 + digest.len() * 2);
    encoded.push_str("sha256:");
    for byte in digest {
        encoded.push_str(&format!("{byte:02x}"));
    }
    encoded
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "solaris"))))]
fn open_root_directory(path: &Path) -> std::io::Result<std::fs::File> {
    use nix::fcntl::{OFlag, openat};
    use nix::sys::stat::Mode;
    use std::os::unix::fs::OpenOptionsExt;

    let mut current = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/")?;
    for component in path.components() {
        let Component::Normal(name) = component else {
            if matches!(component, Component::RootDir | Component::CurDir) {
                continue;
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "canonical workspace root contains an unsupported path component",
            ));
        };
        let directory = openat(
            &current,
            name,
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
            Mode::empty(),
        )
        .map_err(std::io::Error::from)?;
        current = std::fs::File::from(directory);
    }
    Ok(current)
}

#[cfg(not(all(unix, not(any(target_os = "redox", target_os = "solaris")))))]
fn open_root_directory(_path: &Path) -> Result<std::fs::File> {
    Err(WebmcpError::Unsupported(
        "WebMCP filesystem access requires directory-handle file operations on this platform".to_string(),
    ))
}

fn read_snapshot_blocking(
    root: &Path,
    root_dir: &std::fs::File,
    path: &str,
    limits: FilesystemLimits,
) -> Result<FileSnapshot> {
    let relative = validate_relative_path(path)?;
    reject_sensitive_relative_path(&relative, path)?;
    let file = open_workspace_file(root, root_dir, &relative, false)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(WebmcpError::PathRejected(path.to_string()));
    }
    reject_hard_links(&metadata, path)?;
    let max_file_bytes = u64::try_from(limits.max_file_bytes).unwrap_or(u64::MAX);
    if metadata.len() > max_file_bytes {
        return Err(WebmcpError::LimitExceeded);
    }
    let mut file = file;
    let content = read_bounded_content(&mut file, limits.max_file_bytes)?;
    let digest = digest_text(&content);
    Ok(FileSnapshot { path: path.to_string(), content, digest })
}

fn read_bounded_content<R>(reader: &mut R, max_file_bytes: usize) -> Result<String>
where
    R: Read,
{
    let max_file_bytes_u64 = u64::try_from(max_file_bytes).unwrap_or(u64::MAX);
    let mut bytes = Vec::with_capacity(max_file_bytes.min(64 * 1024));
    let _bytes_read = reader.take(max_file_bytes_u64.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() > max_file_bytes {
        return Err(WebmcpError::LimitExceeded);
    }
    String::from_utf8(bytes).map_err(|_error| WebmcpError::Adapter("file is not valid UTF-8".to_string()))
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "solaris"))))]
fn open_workspace_file(_root: &Path, root_dir: &std::fs::File, relative: &Path, write: bool) -> Result<std::fs::File> {
    use nix::fcntl::{OFlag, openat};
    use nix::sys::stat::Mode;

    let (parent, name) = open_parent_directory(root_dir, relative)?;
    let access = if write { OFlag::O_RDWR } else { OFlag::O_RDONLY };
    let file = openat(&parent, name.as_os_str(), access | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW, Mode::empty())
        .map_err(|error| map_secure_open_error(relative, error))?;
    Ok(std::fs::File::from(file))
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "solaris"))))]
fn open_parent_directory(root_dir: &std::fs::File, relative: &Path) -> Result<(std::fs::File, std::ffi::OsString)> {
    use nix::fcntl::{OFlag, openat};
    use nix::sys::stat::Mode;

    let mut components = relative.components();
    let Some(Component::Normal(name)) = components.next_back() else {
        return Err(WebmcpError::PathRejected(relative.display().to_string()));
    };
    let mut current = root_dir.try_clone()?;
    for component in components {
        let Component::Normal(name) = component else {
            return Err(WebmcpError::PathRejected(relative.display().to_string()));
        };
        let directory = openat(
            &current,
            name,
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|error| map_secure_open_error(relative, error))?;
        current = std::fs::File::from(directory);
    }
    Ok((current, name.to_os_string()))
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "solaris"))))]
fn map_secure_open_error(relative: &Path, error: nix::errno::Errno) -> WebmcpError {
    if matches!(error, nix::errno::Errno::ELOOP | nix::errno::Errno::ENOTDIR) {
        WebmcpError::PathRejected(format!("symlink path is not allowed: {}", relative.display()))
    } else {
        WebmcpError::Io(error.into())
    }
}

#[cfg(not(all(unix, not(any(target_os = "redox", target_os = "solaris")))))]
fn open_workspace_file(
    _root: &Path,
    _root_dir: &std::fs::File,
    _relative: &Path,
    _write: bool,
) -> Result<std::fs::File> {
    Err(WebmcpError::Unsupported(
        "WebMCP filesystem access requires directory-handle file operations on this platform".to_string(),
    ))
}

fn replace_snapshot_if_current_blocking(
    root: &Path,
    root_dir: &std::fs::File,
    update: &FileSnapshot,
    expected: &FileSnapshot,
) -> Result<()> {
    let relative = validate_relative_path(&expected.path)?;
    reject_sensitive_relative_path(&relative, &expected.path)?;
    if update.path != expected.path {
        return Err(WebmcpError::Adapter("compare-and-replace paths do not match".to_string()));
    }
    if digest_text(&update.content) != update.digest {
        return Err(WebmcpError::Adapter("snapshot digest does not match its content".to_string()));
    }
    let file = open_workspace_file(root, root_dir, &relative, true)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(WebmcpError::PathRejected(expected.path.clone()));
    }
    reject_hard_links(&metadata, &expected.path)?;
    replace_open_file(file, update, expected)
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "solaris"))))]
fn replace_open_file(file: std::fs::File, update: &FileSnapshot, expected: &FileSnapshot) -> Result<()> {
    use nix::fcntl::{Flock, FlockArg};

    let mut file =
        Flock::lock(file, FlockArg::LockExclusiveNonblock).map_err(|(_, error)| WebmcpError::Io(error.into()))?;
    let metadata_before = file.metadata()?;
    if !metadata_before.is_file() {
        return Err(WebmcpError::PathRejected(expected.path.clone()));
    }
    reject_hard_links(&metadata_before, &expected.path)?;
    let expected_size = u64::try_from(expected.content.len()).unwrap_or(u64::MAX);
    if metadata_before.len() > expected_size {
        return Err(WebmcpError::Conflict {
            path: expected.path.clone(),
            expected: expected.digest.clone(),
            actual: format!("size:{}", metadata_before.len()),
        });
    }
    let _ = file.seek(SeekFrom::Start(0))?;
    let current = match read_bounded_content(&mut *file, expected.content.len()) {
        Ok(current) => current,
        Err(WebmcpError::LimitExceeded) => {
            return Err(WebmcpError::Conflict {
                path: expected.path.clone(),
                expected: expected.digest.clone(),
                actual: format!("size:>{expected_size}"),
            });
        }
        Err(error) => return Err(error),
    };
    let metadata_after_read = file.metadata()?;
    if metadata_after_read.len() > expected_size || metadata_after_read.len() != metadata_before.len() {
        return Err(WebmcpError::Conflict {
            path: expected.path.clone(),
            expected: expected.digest.clone(),
            actual: format!("size:{}", metadata_after_read.len()),
        });
    }
    if metadata_after_read.modified().ok() != metadata_before.modified().ok() {
        return Err(WebmcpError::Conflict {
            path: expected.path.clone(),
            expected: expected.digest.clone(),
            actual: "metadata-changed".to_string(),
        });
    }
    let actual = digest_text(&current);
    if actual != expected.digest {
        return Err(WebmcpError::Conflict {
            path: expected.path.clone(),
            expected: expected.digest.clone(),
            actual,
        });
    }
    let _ = file.seek(SeekFrom::Start(0))?;
    file.set_len(0)?;
    file.write_all(update.content.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

#[cfg(not(all(unix, not(any(target_os = "redox", target_os = "solaris")))))]
fn replace_open_file(_file: std::fs::File, _update: &FileSnapshot, _expected: &FileSnapshot) -> Result<()> {
    Err(WebmcpError::Unsupported(
        "WebMCP compare-and-replace requires a platform with directory-handle file operations".to_string(),
    ))
}

fn list_files_blocking(root: &Path, root_dir: &std::fs::File, limits: FilesystemLimits) -> Result<Vec<WorkspaceFile>> {
    let mut files = Vec::new();
    let mut total_bytes = 0usize;
    let mut visited_directories = 0usize;
    visit_directory(root, root_dir, root, 0, limits, &mut total_bytes, &mut visited_directories, &mut files)?;
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

fn visit_directory(
    root: &Path,
    root_dir: &std::fs::File,
    directory: &Path,
    depth: usize,
    limits: FilesystemLimits,
    total_bytes: &mut usize,
    visited_directories: &mut usize,
    files: &mut Vec<WorkspaceFile>,
) -> Result<()> {
    if depth > MAX_DIRECTORY_DEPTH {
        return Err(WebmcpError::LimitExceeded);
    }
    *visited_directories = visited_directories.saturating_add(1);
    if *visited_directories > MAX_VISITED_DIRECTORIES {
        return Err(WebmcpError::LimitExceeded);
    }
    let entries = std::fs::read_dir(directory)?;
    for entry in entries {
        if files.len() >= limits.max_files {
            return Err(WebmcpError::LimitExceeded);
        }
        let entry = entry?;
        let path = entry.path();
        let relative_path = path
            .strip_prefix(root)
            .map_err(|_error| WebmcpError::PathRejected(path.display().to_string()))?;
        if is_sensitive_relative_path(relative_path) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            if entry
                .file_name()
                .to_str()
                .is_some_and(|name| IGNORED_DIRECTORY_NAMES.contains(&name))
            {
                continue;
            }
            visit_directory(root, root_dir, &path, depth + 1, limits, total_bytes, visited_directories, files)?;
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        if has_multiple_hard_links(&metadata) {
            continue;
        }
        let max_file_bytes = u64::try_from(limits.max_file_bytes).unwrap_or(u64::MAX);
        if metadata.len() > max_file_bytes {
            continue;
        }
        let file = open_workspace_file(root, root_dir, relative_path, false)?;
        let opened_metadata = file.metadata()?;
        if !opened_metadata.is_file()
            || has_multiple_hard_links(&opened_metadata)
            || opened_metadata.len() > max_file_bytes
        {
            continue;
        }
        let mut reader = file.take(max_file_bytes.saturating_add(1));
        let mut bytes = Vec::with_capacity(limits.max_file_bytes.min(64 * 1024));
        let _bytes_read = reader.read_to_end(&mut bytes)?;
        if bytes.len() > limits.max_file_bytes {
            continue;
        }
        let Ok(content) = String::from_utf8(bytes) else {
            continue;
        };
        *total_bytes = total_bytes.saturating_add(content.len());
        if *total_bytes > limits.max_total_bytes {
            return Err(WebmcpError::LimitExceeded);
        }
        let relative = relative_path.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/");
        files.push(WorkspaceFile {
            path: relative,
            size_bytes: content.len() as u64,
            digest: digest_text(&content),
        });
    }
    Ok(())
}

fn trusted_toolchain_directory(path: &Path, workspace: &Path) -> Option<PathBuf> {
    let canonical = vtcode_commons::canonicalize(path).ok()?;
    (canonical.is_dir() && !canonical.starts_with(workspace)).then_some(canonical)
}

fn resolve_check_executable(program: &str, workspace: &Path) -> Result<PathBuf> {
    let executable_name = if cfg!(windows) {
        format!("{program}.exe")
    } else {
        program.to_string()
    };
    let mut candidates = Vec::new();

    #[cfg(unix)]
    {
        if program == "cargo" {
            if let Some(home) = std::env::var_os("HOME") {
                candidates.push(PathBuf::from(home).join(".cargo/bin").join(&executable_name));
            }
        }
        for directory in ["/usr/local/bin", "/opt/homebrew/bin", "/usr/bin", "/bin"] {
            candidates.push(PathBuf::from(directory).join(&executable_name));
        }
    }

    #[cfg(windows)]
    {
        if program == "cargo"
            && let Some(profile) = std::env::var_os("USERPROFILE")
        {
            candidates.push(PathBuf::from(profile).join(".cargo/bin").join(&executable_name));
        }
        if let Some(system_root) = std::env::var_os("SystemRoot") {
            candidates.push(PathBuf::from(system_root).join("System32").join(&executable_name));
        }
    }

    for candidate in candidates {
        let Ok(canonical) = vtcode_commons::canonicalize(&candidate) else {
            continue;
        };
        if canonical.starts_with(workspace) {
            continue;
        }
        let Ok(metadata) = std::fs::metadata(&canonical) else {
            continue;
        };
        if metadata.is_file() && is_executable_file(&metadata) {
            return Ok(canonical);
        }
    }

    Err(WebmcpError::InvalidRequest(format!(
        "allowlisted check executable is not installed in a trusted location: {program}"
    )))
}

fn is_executable_file(metadata: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        metadata.is_file()
    }
}

fn trusted_executable_path(executable: &Path) -> Result<String> {
    let mut directories = Vec::new();
    if let Some(parent) = executable.parent() {
        directories.push(parent.to_path_buf());
    }
    #[cfg(unix)]
    directories.extend([
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ]);
    #[cfg(windows)]
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        directories.push(PathBuf::from(system_root).join("System32"));
    }
    directories.dedup();
    std::env::join_paths(directories)
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|error| WebmcpError::Adapter(format!("failed to build trusted check PATH: {error}")))
}

fn sensitive_path_for_policy(path: &Path) -> Result<SensitivePath> {
    let path_string = path.to_str().ok_or_else(|| {
        WebmcpError::Adapter(format!("cannot sandbox a non-UTF-8 sensitive path: {}", path.display()))
    })?;
    if path_string
        .chars()
        .any(|character| character == '"' || character == '\\' || character.is_control())
    {
        return Err(WebmcpError::Adapter(format!(
            "cannot sandbox a sensitive path containing an unsafe character: {path_string}"
        )));
    }
    Ok(SensitivePath::new(path_string))
}

fn collect_workspace_sensitive_paths(root: &Path) -> Result<Vec<SensitivePath>> {
    let mut sensitive_paths = Vec::new();
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    let mut visited_directories = 0usize;

    while let Some((directory, depth)) = pending.pop() {
        if depth > MAX_DIRECTORY_DEPTH {
            return Err(WebmcpError::LimitExceeded);
        }
        visited_directories = visited_directories.saturating_add(1);
        if visited_directories > MAX_VISITED_DIRECTORIES {
            return Err(WebmcpError::LimitExceeded);
        }

        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            let path = entry.path();
            let relative_path = path
                .strip_prefix(root)
                .map_err(|_error| WebmcpError::PathRejected(path.display().to_string()))?;
            if is_sensitive_relative_path(relative_path) {
                if sensitive_paths.len() >= MAX_CHECK_SENSITIVE_PATHS {
                    return Err(WebmcpError::LimitExceeded);
                }
                sensitive_paths.push(sensitive_path_for_policy(&path)?);
                continue;
            }

            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                pending.push((path, depth + 1));
            }
        }
    }

    Ok(sensitive_paths)
}

fn check_sandbox_policy(root: &Path) -> Result<SandboxPolicy> {
    let mut sensitive_paths = default_sensitive_paths();
    for name in SENSITIVE_FILES.iter().copied().chain(SENSITIVE_DIRECTORY_NAMES.iter().copied()) {
        sensitive_paths.push(sensitive_path_for_policy(&root.join(name))?);
    }
    sensitive_paths.extend(collect_workspace_sensitive_paths(root)?);
    Ok(SandboxPolicy::workspace_write_with_sensitive_paths(vec![root.to_path_buf()], sensitive_paths))
}

fn parse_safe_command(command: &str, allowed_commands: &HashSet<String>) -> Result<Vec<String>> {
    if command.len() > 512
        || command
            .chars()
            .any(|character| matches!(character, ';' | '|' | '&' | '$' | '`' | '>' | '<' | '\n' | '\r'))
    {
        return Err(WebmcpError::InvalidRequest("check command contains shell syntax".to_string()));
    }
    let args = shell_words::split(command)
        .map_err(|error| WebmcpError::InvalidRequest(format!("invalid check command: {error}")))?;
    let Some(program) = args.first() else {
        return Err(WebmcpError::InvalidRequest("check command cannot be empty".to_string()));
    };
    if Path::new(program).components().count() != 1 || !allowed_commands.contains(program) {
        return Err(WebmcpError::InvalidRequest("check executable is not allowlisted".to_string()));
    }
    if args.iter().any(|argument| argument.contains('\0')) {
        return Err(WebmcpError::InvalidRequest("check command contains an invalid argument".to_string()));
    }
    match program.as_str() {
        "cargo"
            if args.get(1).is_some_and(|subcommand| subcommand == "check")
                && args.iter().skip(2).all(|argument| {
                    matches!(
                        argument.as_str(),
                        "--locked" | "--offline" | "--workspace" | "--all-targets" | "--all-features"
                    )
                }) =>
        {
            Ok(args)
        }
        "printf" => Ok(args),
        _ => Err(WebmcpError::InvalidRequest(
            "only the bounded cargo check and printf commands are supported".to_string(),
        )),
    }
}

async fn read_process_output<R>(mut reader: R) -> Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut captured = Vec::with_capacity(MAX_CHECK_OUTPUT_BYTES.min(8192));
    let mut buffer = [0u8; 8192];
    let mut exceeded = false;
    loop {
        let bytes_read = reader.read(&mut buffer).await?;
        if bytes_read == 0 {
            break;
        }
        let remaining = MAX_CHECK_OUTPUT_BYTES.saturating_sub(captured.len());
        exceeded |= bytes_read > remaining;
        if remaining > 0 {
            let chunk = buffer
                .get(..bytes_read.min(remaining))
                .ok_or_else(|| WebmcpError::Adapter("check output read exceeded its buffer".to_string()))?;
            captured.extend_from_slice(chunk);
        }
    }
    if exceeded {
        Err(WebmcpError::LimitExceeded)
    } else {
        Ok(captured)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tokio::io::{AsyncWriteExt, duplex};

    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    #[cfg(windows)]
    use std::os::windows::fs::symlink_file as symlink;

    async fn workspace(mutations_allowed: bool) -> (TempDir, FilesystemWorkspace) {
        let temp = TempDir::new().expect("temp dir");
        tokio::fs::write(temp.path().join("main.js"), "console.log('old');\n")
            .await
            .expect("seed");
        let adapter = FilesystemWorkspace::new(temp.path(), [], mutations_allowed)
            .await
            .expect("adapter");
        (temp, adapter)
    }

    #[tokio::test]
    async fn proposal_apply_check_and_revert_validate_current_digests() {
        let (_temp, adapter) = workspace(true).await;
        let adapter = adapter.with_allowed_commands(["cargo", "printf"]);
        let snapshot = adapter.read_file("main.js").await.expect("read");
        let proposal = adapter
            .propose_changes(vec![FileChange {
                path: "main.js".to_string(),
                base_digest: snapshot.digest,
                content: "console.log('new');\n".to_string(),
            }])
            .await
            .expect("propose");
        let applied = adapter.apply_proposal(&proposal.proposal_id).await.expect("apply");
        let result = adapter.run_checks("printf ok").await.expect("check");
        assert!(
            result.exit_code == Some(0)
                || (result.exit_code == Some(71) && result.stderr.contains("sandbox_apply: Operation not permitted")),
            "unexpected check result: {result:?}"
        );
        assert_eq!(adapter.read_file("main.js").await.expect("read").content, "console.log('new');\n");
        let _ = adapter.revert_last_change(&applied.change_id).await.expect("revert");
        assert_eq!(adapter.read_file("main.js").await.expect("read").content, "console.log('old');\n");
    }

    #[test]
    fn proposal_diff_contains_context_and_correct_file_ranges() {
        let before = FileSnapshot {
            path: "src/main.js".to_string(),
            content: (1..=12).map(|line| format!("line-{line}\n")).collect(),
            digest: String::new(),
        };
        let after = (1..=12)
            .map(|line| match line {
                2 => "changed-2\n".to_string(),
                10 => "changed-10\n".to_string(),
                _ => format!("line-{line}\n"),
            })
            .collect::<String>();
        let diff = FilesystemWorkspace::proposal_diff(
            &[before],
            &[FileChange {
                path: "src/main.js".to_string(),
                base_digest: String::new(),
                content: after,
            }],
        );

        assert_eq!(
            diff,
            "--- a/src/main.js\n+++ b/src/main.js\n@@ -1,5 +1,5 @@\n line-1\n-line-2\n+changed-2\n line-3\n line-4\n line-5\n@@ -7,6 +7,6 @@\n line-7\n line-8\n line-9\n-line-10\n+changed-10\n line-11\n line-12\n"
        );
    }

    #[test]
    fn proposal_diff_handles_empty_files_and_missing_final_newlines() {
        let empty_before = FileSnapshot {
            path: "new.txt".to_string(),
            content: String::new(),
            digest: String::new(),
        };
        let empty_diff = FilesystemWorkspace::proposal_diff(
            &[empty_before],
            &[FileChange {
                path: "new.txt".to_string(),
                base_digest: String::new(),
                content: "first\nsecond\n".to_string(),
            }],
        );
        assert_eq!(empty_diff, "--- a/new.txt\n+++ b/new.txt\n@@ -0,0 +1,2 @@\n+first\n+second\n");

        let no_newline_before = FileSnapshot {
            path: "line.txt".to_string(),
            content: "old".to_string(),
            digest: String::new(),
        };
        let no_newline_diff = FilesystemWorkspace::proposal_diff(
            &[no_newline_before],
            &[FileChange {
                path: "line.txt".to_string(),
                base_digest: String::new(),
                content: "new".to_string(),
            }],
        );
        assert_eq!(
            no_newline_diff,
            "--- a/line.txt\n+++ b/line.txt\n@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n"
        );

        let cr_diff = FilesystemWorkspace::proposal_diff(
            &[FileSnapshot {
                path: "cr.txt".to_string(),
                content: "one\rtwo\r".to_string(),
                digest: String::new(),
            }],
            &[FileChange {
                path: "cr.txt".to_string(),
                base_digest: String::new(),
                content: "one\rchanged\r".to_string(),
            }],
        );
        assert_eq!(cr_diff, "--- a/cr.txt\n+++ b/cr.txt\n@@ -1,2 +1,2 @@\n one\n-two\n+changed\n");
    }

    #[tokio::test]
    async fn proposal_for_turn_rechecks_the_staged_snapshot() {
        let (temp, adapter) = workspace(false).await;
        let snapshot = adapter.read_file("main.js").await.expect("snapshot");
        let proposal = adapter
            .propose_changes(vec![FileChange {
                path: "main.js".to_string(),
                base_digest: snapshot.digest,
                content: "console.log('new');\n".to_string(),
            }])
            .await
            .expect("proposal");

        let handed_off = adapter
            .proposal_for_turn(&proposal.proposal_id)
            .await
            .expect("current proposal");
        assert_eq!(handed_off.unified_diff, proposal.unified_diff);

        tokio::fs::write(temp.path().join("main.js"), "external\n")
            .await
            .expect("external edit");
        assert!(matches!(
            adapter.proposal_for_turn(&proposal.proposal_id).await,
            Err(WebmcpError::Conflict { path, .. }) if path == "main.js"
        ));
    }

    #[tokio::test]
    async fn headless_adapter_does_not_fake_agent_turns() {
        let (_temp, adapter) = workspace(false).await;
        let status = adapter.status().await.expect("status");
        assert!(!status.turns_available);
        assert!(matches!(
            adapter.request_turn("review the draft", None).await,
            Err(WebmcpError::Unsupported(message)) if message.contains("active VT Code runtime")
        ));
    }

    #[tokio::test]
    async fn listing_skips_dependency_and_generated_directories() {
        let temp = TempDir::new().expect("temp dir");
        tokio::fs::write(temp.path().join("visible.txt"), "visible")
            .await
            .expect("visible file");
        tokio::fs::write(temp.path().join(".env"), "TOKEN=secret")
            .await
            .expect("dotenv file");
        tokio::fs::write(temp.path().join(".npmrc"), "//registry.example/:_authToken=secret")
            .await
            .expect("credential file");
        tokio::fs::create_dir_all(temp.path().join(".ssh"))
            .await
            .expect("credential directory");
        tokio::fs::write(temp.path().join(".ssh/id_ed25519"), "private key")
            .await
            .expect("private key");
        tokio::fs::create_dir_all(temp.path().join(".env.secrets"))
            .await
            .expect("dotenv directory");
        tokio::fs::write(temp.path().join(".env.secrets/token.txt"), "secret")
            .await
            .expect("dotenv secret");
        for directory in ["node_modules", "target", ".git", ".worktrees", "dist"] {
            let directory = temp.path().join(directory);
            tokio::fs::create_dir_all(&directory).await.expect("directory");
            tokio::fs::write(directory.join("hidden.txt"), "hidden")
                .await
                .expect("hidden file");
        }

        let adapter = FilesystemWorkspace::new(temp.path(), [], false).await.expect("adapter");
        let files = adapter.list_files().await.expect("list files");
        assert_eq!(files.iter().map(|file| file.path.as_str()).collect::<Vec<_>>(), vec!["visible.txt"]);
        assert!(matches!(adapter.read_file(".env").await, Err(WebmcpError::PathRejected(_))));
        assert!(matches!(adapter.read_file(".npmrc").await, Err(WebmcpError::PathRejected(_))));
        assert!(matches!(adapter.read_file(".ssh/id_ed25519").await, Err(WebmcpError::PathRejected(_))));
        assert!(matches!(adapter.read_file(".env.secrets/token.txt").await, Err(WebmcpError::PathRejected(_))));
    }

    #[test]
    fn webmcp_check_sandbox_blocks_case_variants_of_sensitive_files() {
        let temp = TempDir::new().expect("workspace");
        for name in ["ID_ECDSA", ".ENV", "credentials.JSON"] {
            std::fs::write(temp.path().join(name), "secret").expect("sensitive file");
        }
        let nested = temp.path().join("project");
        std::fs::create_dir_all(nested.join(".ssh")).expect("nested credential directory");
        std::fs::write(nested.join(".ssh/id_ed25519"), "private key").expect("nested private key");
        std::fs::write(nested.join(".env"), "TOKEN=secret").expect("nested dotenv file");

        let policy = check_sandbox_policy(temp.path()).expect("sandbox policy");

        for path in [
            temp.path().join("ID_ECDSA"),
            temp.path().join(".ENV"),
            temp.path().join("credentials.JSON"),
            nested.join(".ssh/id_ed25519"),
            nested.join(".env"),
        ] {
            assert!(!policy.is_path_readable(&path), "check sandbox read allowed: {path:?}");
            assert!(!policy.is_path_writable(&path, temp.path()), "check sandbox write allowed: {path:?}");
        }
    }

    #[cfg(any(unix, windows))]
    #[tokio::test]
    async fn stale_and_traversal_requests_fail_closed() {
        let (temp, adapter) = workspace(false).await;
        let stale = FileChange {
            path: "main.js".to_string(),
            base_digest: "sha256:stale".to_string(),
            content: "new".to_string(),
        };
        assert!(matches!(adapter.propose_changes(vec![stale]).await, Err(WebmcpError::Conflict { .. })));
        assert!(matches!(adapter.read_file("../outside").await, Err(WebmcpError::PathRejected(_))));
        assert!(matches!(
            adapter.run_checks("printf safe; echo injected").await,
            Err(WebmcpError::InvalidRequest(_))
        ));
        assert!(matches!(adapter.run_checks("printf safe").await, Err(WebmcpError::InvalidRequest(_))));
        assert!(matches!(adapter.run_checks("cargo run").await, Err(WebmcpError::InvalidRequest(_))));
        assert!(matches!(adapter.run_checks("npm run check").await, Err(WebmcpError::InvalidRequest(_))));
        assert!(matches!(adapter.run_checks("python3 -c 'print(1)'").await, Err(WebmcpError::InvalidRequest(_))));
        assert!(matches!(adapter.run_checks("env").await, Err(WebmcpError::InvalidRequest(_))));
        assert!(matches!(adapter.run_checks("cargo check").await, Err(WebmcpError::ApprovalRequired)));

        let (_checks_temp, checks_disabled) = workspace(true).await;
        let checks_disabled = checks_disabled.with_checks_allowed(false);
        assert!(matches!(checks_disabled.run_checks("cargo check").await, Err(WebmcpError::ApprovalRequired)));

        let outside = temp.path().join("outside.txt");
        tokio::fs::write(&outside, "secret").await.expect("outside");
        symlink(&outside, temp.path().join("link.txt")).expect("symlink");
        assert!(matches!(adapter.read_file("link.txt").await, Err(WebmcpError::PathRejected(_))));
        assert!(matches!(adapter.apply_proposal("missing").await, Err(WebmcpError::ApprovalRequired)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn nested_symlink_components_are_not_read() {
        let (_temp, adapter) = workspace(false).await;
        let outside = TempDir::new().expect("outside temp dir");
        tokio::fs::write(outside.path().join("secret.txt"), "secret")
            .await
            .expect("outside file");
        symlink(outside.path(), adapter.root().join("linked")).expect("directory symlink");

        let result = adapter.read_file("linked/secret.txt").await;
        assert!(matches!(&result, Err(WebmcpError::PathRejected(_))), "result={result:?}");
    }

    #[tokio::test]
    async fn invalid_explicit_allowed_roots_are_rejected() {
        let temp = TempDir::new().expect("temp dir");
        let file_root = temp.path().join("not-a-directory");
        tokio::fs::write(&file_root, "file").await.expect("seed file");
        assert!(matches!(
            FilesystemWorkspace::new(temp.path(), [file_root], false).await,
            Err(WebmcpError::PathRejected(_))
        ));
    }

    #[tokio::test]
    async fn multiple_allowed_roots_are_rejected_until_root_selection_exists() {
        let temp = TempDir::new().expect("temp dir");
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        tokio::fs::create_dir_all(&first).await.expect("first root");
        tokio::fs::create_dir_all(&second).await.expect("second root");
        assert!(matches!(
            FilesystemWorkspace::new(&first, [first.clone(), second], false).await,
            Err(WebmcpError::InvalidRequest(_))
        ));
    }

    #[tokio::test]
    async fn reads_and_check_output_are_bounded() {
        let (_temp, adapter) = workspace(true).await;
        let limited = adapter.with_limits(FilesystemLimits { max_file_bytes: 4, ..FilesystemLimits::default() });
        assert!(matches!(limited.read_file("main.js").await, Err(WebmcpError::LimitExceeded)));

        let (mut writer, reader) = duplex(8192);
        let writer_task = tokio::spawn(async move {
            writer
                .write_all(&vec![b'x'; MAX_CHECK_OUTPUT_BYTES + 1])
                .await
                .expect("write test output");
        });
        assert!(matches!(read_process_output(reader).await, Err(WebmcpError::LimitExceeded)));
        writer_task.await.expect("test output writer");
    }

    #[tokio::test]
    async fn rollback_does_not_overwrite_an_external_change() {
        let (_temp, adapter) = workspace(true).await;
        let original = adapter.read_file("main.js").await.expect("original");
        let applied = FileSnapshot {
            path: original.path.clone(),
            content: "applied\n".to_string(),
            digest: digest_text("applied\n"),
        };
        let proposal = adapter
            .propose_changes(vec![FileChange {
                path: applied.path.clone(),
                base_digest: original.digest.clone(),
                content: applied.content.clone(),
            }])
            .await
            .expect("proposal");
        let _ = adapter.apply_proposal(&proposal.proposal_id).await.expect("apply snapshot");
        tokio::fs::write(adapter.root().join("main.js"), "external\n")
            .await
            .expect("external change");

        assert!(matches!(
            adapter
                .rollback_snapshots(std::slice::from_ref(&applied), std::slice::from_ref(&original))
                .await,
            Err(WebmcpError::PartialApply)
        ));

        assert_eq!(adapter.read_file("main.js").await.expect("current").content, "external\n");
    }

    #[tokio::test]
    async fn apply_rejects_an_external_change_after_proposal() {
        let (_temp, adapter) = workspace(true).await;
        let original = adapter.read_file("main.js").await.expect("original");
        let proposal = adapter
            .propose_changes(vec![FileChange {
                path: original.path.clone(),
                base_digest: original.digest.clone(),
                content: "proposed\n".to_string(),
            }])
            .await
            .expect("proposal");
        tokio::fs::write(adapter.root().join("main.js"), "external\n")
            .await
            .expect("external change");

        assert!(matches!(
            adapter.apply_proposal(&proposal.proposal_id).await,
            Err(WebmcpError::Conflict { path, .. }) if path == "main.js"
        ));
        assert_eq!(adapter.read_file("main.js").await.expect("current").content, "external\n");
    }

    #[tokio::test]
    async fn apply_reports_a_grown_file_as_a_conflict() {
        let (_temp, adapter) = workspace(true).await;
        let original = adapter.read_file("main.js").await.expect("original");
        let proposal = adapter
            .propose_changes(vec![FileChange {
                path: original.path.clone(),
                base_digest: original.digest.clone(),
                content: "proposed\n".to_string(),
            }])
            .await
            .expect("proposal");
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(adapter.root().join("main.js"))
            .await
            .expect("external append");
        file.write_all(b"external append\n").await.expect("append");
        file.flush().await.expect("flush");

        let result = adapter.apply_proposal(&proposal.proposal_id).await;
        assert!(matches!(result, Err(WebmcpError::Conflict { actual, .. }) if actual.starts_with("size:")));
        assert!(
            tokio::fs::read_to_string(adapter.root().join("main.js"))
                .await
                .expect("current")
                .ends_with("external append\n")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn apply_rejects_a_path_replaced_with_a_symlink() {
        let (_temp, adapter) = workspace(true).await;
        let original = adapter.read_file("main.js").await.expect("original");
        let proposal = adapter
            .propose_changes(vec![FileChange {
                path: original.path.clone(),
                base_digest: original.digest,
                content: "proposed\n".to_string(),
            }])
            .await
            .expect("proposal");
        let outside = TempDir::new().expect("outside temp dir");
        let outside_file = outside.path().join("outside.txt");
        tokio::fs::write(&outside_file, "outside\n").await.expect("outside file");
        tokio::fs::remove_file(adapter.root().join("main.js"))
            .await
            .expect("remove workspace file");
        symlink(&outside_file, adapter.root().join("main.js")).expect("replacement symlink");

        assert!(matches!(adapter.apply_proposal(&proposal.proposal_id).await, Err(WebmcpError::PathRejected(_))));
        assert_eq!(tokio::fs::read_to_string(outside_file).await.expect("outside content"), "outside\n");
    }
}
