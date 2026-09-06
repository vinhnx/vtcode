use hashbrown::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use parking_lot::{Mutex, RwLock};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use shell_words::join;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::mpsc as tokio_mpsc;
use tracing::{debug, info, warn};

use super::command_utils::{
    is_long_running_command, is_long_running_command_string, is_sandbox_wrapper_program, is_shell_program,
};
use super::formatting::sanitize_session_id;
use super::manager_utils::{clamp_timeout, exit_status_code, set_command_environment_with_sandbox};

use super::screen_backend::PtyScreenState;
use super::scrollback::PtyScrollback;
use super::session::PtySessionHandle;
use super::types::{PtyCommandRequest, PtyCommandResult};

use once_cell::sync::Lazy;

/// Per-workspace command locks to serialize long-running toolchain commands.
/// Keyed by canonicalized workspace path to prevent lockfile contention.
/// This is more granular than a global lock - different workspaces can run concurrently.
///
/// Entries are held weakly: once the last `Arc` returned by `get_command_lock` is
/// dropped (the command that held it completes), the entry is collected on the
/// next lookup, so the map stays bounded by the number of concurrently-held
/// locks rather than the number of workspaces ever seen.
static WORKSPACE_COMMAND_LOCKS: Lazy<Mutex<HashMap<PathBuf, Weak<tokio::sync::Mutex<()>>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Get or create a command lock for the given workspace root
fn get_command_lock(workspace_root: &Path) -> Arc<tokio::sync::Mutex<()>> {
    let mut locks = WORKSPACE_COMMAND_LOCKS.lock();
    let key = workspace_root.to_path_buf();
    if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    lock
}

/// Grace period to wait for threads to exit after killing the process (ms)
const THREAD_JOIN_GRACE_PERIOD_MS: u64 = 500;

use crate::audit::PermissionAuditLog;
use crate::config::{CommandsConfig, PtyConfig};
use crate::telemetry::perf;
use crate::tools::path_env;
use crate::tools::shell::resolve_fallback_shell;
use crate::tools::types::VTCodePtySession;
use crate::utils::gatekeeper;
use crate::utils::path::ensure_path_within_workspace;
use crate::utils::unicode_monitor::UNICODE_MONITOR;
use vtcode_commons::canonicalize;

mod session_ops;

#[derive(Clone)]
pub struct PtyManager {
    workspace_root: PathBuf,
    config: PtyConfig,
    inner: Arc<PtyState>,
    audit_log: Option<Arc<TokioMutex<PermissionAuditLog>>>,
    extra_paths: Arc<RwLock<Arc<Vec<PathBuf>>>>,
}

#[derive(Default)]
struct PtyState {
    sessions: Mutex<HashMap<String, Arc<PtySessionHandle>>>,
}

impl PtyManager {
    pub fn new(workspace_root: PathBuf, config: PtyConfig) -> Self {
        let resolved_root = canonicalize(&workspace_root).unwrap_or_else(|_| workspace_root.clone());

        let default_paths =
            path_env::compute_extra_search_paths(&CommandsConfig::default().extra_path_entries, &resolved_root);

        Self {
            workspace_root: resolved_root,
            config,
            inner: Arc::new(PtyState::default()),
            audit_log: None,
            extra_paths: Arc::new(RwLock::new(Arc::new(default_paths))),
        }
    }

    pub fn with_audit_log(mut self, audit_log: Arc<TokioMutex<PermissionAuditLog>>) -> Self {
        self.audit_log = Some(audit_log);
        self
    }

    pub fn config(&self) -> &PtyConfig {
        &self.config
    }

    pub fn apply_commands_config(&self, commands_config: &CommandsConfig) {
        let new_paths = path_env::compute_extra_search_paths(&commands_config.extra_path_entries, &self.workspace_root);
        *self.extra_paths.write() = Arc::new(new_paths);
    }

    pub fn describe_working_dir(&self, path: &Path) -> String {
        self.format_working_dir(path)
    }

    pub async fn run_command(&self, mut request: PtyCommandRequest) -> Result<PtyCommandResult> {
        if request.command.is_empty() {
            return Err(anyhow!("PTY command cannot be empty"));
        }

        let mut command = std::mem::take(&mut request.command);
        let program = command.remove(0);
        let args = command;
        let timeout = clamp_timeout(request.timeout);
        let work_dir = std::mem::take(&mut request.working_dir);
        let size = request.size;
        let start = Instant::now();

        let mut tags = HashMap::new();
        tags.insert("subsystem".to_string(), "pty".to_string());
        tags.insert("program".to_string(), program.clone());
        perf::record_value("vtcode.perf.spawn_count", 1.0, tags);

        gatekeeper::check_quarantine_for_program(&program);
        self.ensure_within_workspace(&work_dir)?;
        let workspace_root = self.workspace_root.clone();
        let extra_paths = self.extra_paths.read().clone();
        let max_tokens = request.max_tokens;

        // Determine if this command needs serialization to avoid contention
        let needs_lock = is_long_running_command(&program)
            || (is_shell_program(&program) && args.iter().any(|arg| is_long_running_command_string(arg)));

        // Acquire per-workspace lock if needed to prevent lockfile contention.
        // This prevents "blocking waiting for file lock" errors when the agent
        // triggers multiple long-running commands before previous ones complete.
        // Using per-workspace lock allows concurrent commands in different workspaces.
        let command_lock = if needs_lock {
            Some(get_command_lock(&workspace_root))
        } else {
            None
        };
        let _command_guard = if let Some(ref lock) = command_lock {
            debug!(
                target: "vtcode.pty.command_lock",
                program = %program,
                workspace = %workspace_root.display(),
                "Acquiring per-workspace command lock to serialize long-running invocations"
            );
            Some(lock.lock().await)
        } else {
            None
        };

        let result = tokio::task::spawn_blocking(move || -> Result<PtyCommandResult> {
            let timeout_duration = Duration::from_millis(timeout);

            // Use login shell for command execution to ensure user's PATH and environment
            // is properly initialized from their shell configuration files (~/.bashrc, ~/.zshrc, etc).
            // However, we avoid double-wrapping if the command is already a shell invocation.
            let (exec_program, exec_args, display_program, _use_shell_wrapper) = if (is_shell_program(&program)
                && args.iter().any(|arg| arg == "-c" || arg == "/C"))
                || is_sandbox_wrapper_program(&program, &args)
            {
                // Already a shell command or sandbox wrapper, don't wrap again.
                (program.clone(), args.clone(), program.clone(), false)
            } else {
                let shell = resolve_fallback_shell();
                let full_command = join(std::iter::once(program.clone()).chain(args.iter().cloned()));
                (shell.clone(), vec!["-lc".to_owned(), full_command.clone()], program.clone(), true)
            };

            let mut builder = CommandBuilder::new(exec_program.clone());
            for arg in &exec_args {
                builder.arg(arg);
            }
            builder.cwd(&work_dir);
            let extra_env = HashMap::new();
            let sandbox_active = is_sandbox_wrapper_program(&program, &args);
            set_command_environment_with_sandbox(
                &mut builder,
                &display_program,
                size,
                &workspace_root,
                &extra_paths,
                &extra_env,
                &HashMap::new(),
                sandbox_active,
            );

            let pty_system = native_pty_system();
            let pair = pty_system.openpty(size).context("failed to allocate PTY pair")?;

            let mut child = pair
                .slave
                .spawn_command(builder)
                .with_context(|| format!("failed to spawn PTY command '{display_program}'"))?;
            let child_pid = child.process_id();
            let mut killer = child.clone_killer();
            drop(pair.slave);

            let reader = pair.master.try_clone_reader().context("failed to clone PTY reader")?;

            let (wait_tx, wait_rx) = mpsc::channel();
            let wait_thread = thread::spawn(move || {
                let status = child.wait();
                let _ = wait_tx.send(());
                status
            });

            let reader_thread = thread::spawn(move || -> Result<Vec<u8>> {
                let mut reader = reader;
                let mut buffer = [0u8; 4096];
                let mut collected = Vec::new();

                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(bytes_read) => {
                            collected.extend_from_slice(&buffer[..bytes_read]);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                            continue;
                        }
                        Err(error) => {
                            return Err(error).context("failed to read PTY command output");
                        }
                    }
                }

                Ok(collected)
            });

            let wait_result = match wait_rx.recv_timeout(timeout_duration) {
                Ok(()) => wait_thread
                    .join()
                    .map_err(|panic| anyhow!("PTY command wait thread panicked: {panic:?}"))?,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // Kill the process group and the direct child process handle.
                    // vtcode_bash_runner::graceful_kill_process_group_default now handles
                    // the robust 'more kills' pattern which ensures descendants do not survive.
                    if let Some(pid) = child_pid {
                        let _ = vtcode_bash_runner::graceful_kill_process_group_default(pid);
                    } else {
                        let _ = killer.kill();
                    }

                    // Wait with a grace period - don't hang forever
                    let grace_period = Duration::from_millis(THREAD_JOIN_GRACE_PERIOD_MS);
                    match wait_rx.recv_timeout(grace_period) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                            // Process exited, try to join threads with timeout
                            // If they don't exit within grace period, detach them
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            warn!(
                                target: "vtcode.pty.timeout",
                                timeout_ms = timeout,
                                grace_ms = THREAD_JOIN_GRACE_PERIOD_MS,
                                "PTY command did not exit within grace period after kill, detaching threads"
                            );
                            // Detach threads by dropping handles - they may leak but we don't hang
                            drop(wait_thread);
                            drop(reader_thread);
                            return Err(anyhow!(
                                "PTY command timed out after {timeout} milliseconds and did not respond to kill signal"
                            ));
                        }
                    }

                    // Try to join wait thread (should be quick since process exited)
                    match wait_thread.join() {
                        Ok(result) => {
                            if let Err(error) = result {
                                warn!(
                                    target: "vtcode.pty.timeout",
                                    error = %error,
                                    "PTY command wait error after timeout"
                                );
                            }
                        }
                        Err(panic) => {
                            warn!(
                                target: "vtcode.pty.timeout",
                                "PTY wait thread panicked: {:?}",
                                panic
                            );
                        }
                    }

                    // Try to join reader thread (may take a moment for PTY to close)
                    // Use a thread-local timeout via a parking_lot based approach
                    // For simplicity, just drop the handle if it doesn't complete quickly
                    let reader_handle = thread::spawn(move || reader_thread.join());
                    match reader_handle.join() {
                        Ok(Ok(Ok(_))) => {}
                        Ok(Ok(Err(e))) => {
                            warn!(
                                target: "vtcode.pty.timeout",
                                error = %e,
                                "PTY reader error after timeout"
                            );
                        }
                        Ok(Err(panic)) => {
                            warn!(
                                target: "vtcode.pty.timeout",
                                "PTY reader thread panicked: {:?}",
                                panic
                            );
                        }
                        Err(_) => {
                            warn!(
                                target: "vtcode.pty.timeout",
                                "Failed to join PTY reader thread wrapper"
                            );
                        }
                    }

                    return Err(anyhow!("PTY command timed out after {timeout} milliseconds"));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    // Channel disconnected - process likely crashed
                    // Try to join with grace period
                    let grace_period = Duration::from_millis(THREAD_JOIN_GRACE_PERIOD_MS);

                    // Spawn wrapper thread to allow timeout on join
                    let wait_wrapper = thread::spawn(move || wait_thread.join());
                    thread::sleep(grace_period);
                    if wait_wrapper.is_finished() {
                        match wait_wrapper.join() {
                            Ok(Ok(result)) => {
                                if let Err(error) = result {
                                    return Err(error)
                                        .context("failed to wait for PTY command after channel disconnect");
                                }
                            }
                            Ok(Err(panic)) => {
                                return Err(anyhow!("PTY wait thread panicked: {panic:?}"));
                            }
                            Err(_) => {
                                return Err(anyhow!("PTY wait channel disconnected and thread join failed"));
                            }
                        }
                    } else {
                        warn!(
                            target: "vtcode.pty.disconnect",
                            "PTY wait thread did not exit within grace period, detaching"
                        );
                        drop(reader_thread);
                        return Err(anyhow!("PTY command wait channel disconnected unexpectedly"));
                    }

                    // Also try to get reader output
                    match reader_thread.join() {
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => {
                            warn!(
                                target: "vtcode.pty.disconnect",
                                error = %e,
                                "PTY reader error after channel disconnect"
                            );
                        }
                        Err(panic) => {
                            warn!(
                                target: "vtcode.pty.disconnect",
                                "PTY reader panicked: {:?}",
                                panic
                            );
                        }
                    }

                    return Err(anyhow!("PTY command wait channel disconnected unexpectedly"));
                }
            };

            let status = wait_result.context("failed to wait for PTY command to exit")?;

            let output_bytes = reader_thread
                .join()
                .map_err(|panic| anyhow!("PTY command reader thread panicked: {panic:?}"))?
                .context("failed to read PTY command output")?;
            // Fast zero-copy path for valid UTF-8 (the common case); only
            // allocate a lossy copy when the bytes are actually invalid.
            let mut output =
                String::from_utf8(output_bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
            let exit_code = exit_status_code(status);

            // Apply max_tokens truncation if specified
            if let Some(max_tokens) = max_tokens {
                if max_tokens > 0 {
                    // Simple byte-based truncation
                    if output.len() > max_tokens * 4 {
                        let truncate_point = (max_tokens * 4).min(output.len());
                        output.truncate(truncate_point);
                        output.push_str("\n[... truncated by max_tokens ...]");
                    }
                } else {
                    // Keep original if max_tokens is not valid
                }
            }
            // Keep original if max_tokens is None

            Ok(PtyCommandResult {
                exit_code,
                output,
                duration: start.elapsed(),
                size,
                applied_max_tokens: max_tokens,
            })
        })
        .await
        .context("failed to join PTY command task")??;

        Ok(result)
    }

    pub async fn resolve_working_dir(&self, requested: Option<&str>) -> Result<PathBuf> {
        let requested = match requested {
            Some(dir) if !dir.trim().is_empty() => dir.trim(),
            _ => return Ok(self.workspace_root.clone()),
        };

        let candidate = self.workspace_root.join(requested);
        let normalized = ensure_path_within_workspace(&candidate, &self.workspace_root)
            .map_err(|e| anyhow!("Working directory '{}' escapes the workspace root: {e}", candidate.display()))?;
        // Symlink-aware containment: a symlinked directory inside the
        // workspace must not move the command cwd outside it.
        vtcode_commons::paths::ensure_path_within_workspace_resolved(&normalized, &self.workspace_root)
            .await
            .map_err(|e| anyhow!("Working directory '{}' escapes the workspace root: {e}", candidate.display()))?;
        let metadata = tokio::fs::metadata(&normalized)
            .await
            .with_context(|| format!("Working directory '{}' does not exist", normalized.display()))?;
        if !metadata.is_dir() {
            return Err(anyhow!("Working directory '{}' is not a directory", normalized.display()));
        }
        Ok(normalized)
    }

    pub fn create_session(
        &self,
        session_id: String,
        command: Vec<String>,
        working_dir: PathBuf,
        size: PtySize,
    ) -> Result<VTCodePtySession> {
        self.create_session_with_bridge(session_id, command, working_dir, size, HashMap::new(), None)
    }

    fn create_session_with_bridge(
        &self,
        session_id: String,
        command: Vec<String>,
        working_dir: PathBuf,
        size: PtySize,
        extra_env: HashMap<String, String>,
        zsh_exec_bridge: Option<crate::zsh_exec_bridge::ZshExecBridgeSession>,
    ) -> Result<VTCodePtySession> {
        self.create_session_with_bridge_sandboxed(
            session_id,
            command,
            working_dir,
            size,
            extra_env,
            zsh_exec_bridge,
            HashMap::new(),
            false,
        )
    }

    pub(crate) fn create_session_with_bridge_sandboxed(
        &self,
        session_id: String,
        command: Vec<String>,
        working_dir: PathBuf,
        size: PtySize,
        extra_env: HashMap<String, String>,
        zsh_exec_bridge: Option<crate::zsh_exec_bridge::ZshExecBridgeSession>,
        trusted_env: HashMap<String, String>,
        sandbox_active: bool,
    ) -> Result<VTCodePtySession> {
        if command.is_empty() {
            return Err(anyhow!(
                "PTY session command cannot be empty.\n\
                 This is an internal error - command validation should have caught this earlier.\n\
                 Please report this with the command-session parameters used."
            ));
        }

        // Use entry API to avoid double lookup
        let mut sessions = self.inner.sessions.lock();
        use hashbrown::hash_map::Entry;
        let entry = match sessions.entry(session_id.clone()) {
            Entry::Occupied(_) => {
                return Err(anyhow!("PTY session '{session_id}' already exists"));
            }
            Entry::Vacant(e) => e,
        };

        let mut command_parts = command;
        let program = command_parts.remove(0);
        let args = command_parts;
        let extra_paths = self.extra_paths.read().clone();

        // Use login shell for command execution to ensure user's PATH and environment
        // is properly initialized from their shell configuration files (~/.bashrc, ~/.zshrc, etc).
        // However, we avoid double-wrapping if the command is already a shell invocation.
        let (exec_program, exec_args, display_program) = if (is_shell_program(&program)
            && args.iter().any(|arg| arg == "-c" || arg == "/C"))
            || is_sandbox_wrapper_program(&program, &args)
        {
            // Already a shell command, don't wrap again
            (program.clone(), args.clone(), program.clone())
        } else {
            let shell = resolve_fallback_shell();
            let full_command = join(std::iter::once(program.clone()).chain(args.iter().cloned()));

            // Verify we have a valid command string
            if full_command.is_empty() {
                return Err(anyhow!("Failed to construct command string from program '{program}' and args {args:?}"));
            }

            (shell.clone(), vec!["-lc".to_owned(), full_command.clone()], program.clone())
        };

        let pty_system = native_pty_system();
        let pair = pty_system.openpty(size).context("failed to allocate PTY pair")?;

        let mut builder = CommandBuilder::new(exec_program.clone());
        for arg in &exec_args {
            builder.arg(arg);
        }
        builder.cwd(&working_dir);
        self.ensure_within_workspace(&working_dir)?;
        set_command_environment_with_sandbox(
            &mut builder,
            &display_program,
            size,
            &self.workspace_root,
            &extra_paths,
            &extra_env,
            &trusted_env,
            sandbox_active,
        );

        let child = pair
            .slave
            .spawn_command(builder)
            .with_context(|| format!("failed to spawn PTY session command '{display_program}'"))?;

        // Capture the child process ID for process group management
        let child_pid = child.process_id();

        drop(pair.slave);

        let master = pair.master;
        let mut reader = master.try_clone_reader().context("failed to clone PTY reader")?;
        let writer = master.take_writer().context("failed to take PTY writer")?;

        let screen_state = Arc::new(Mutex::new(PtyScreenState::new(size, self.config.scrollback_lines)));
        let scrollback =
            Arc::new(Mutex::new(PtyScrollback::new(self.config.scrollback_lines, self.config.max_scrollback_bytes)));

        let output_spool_path = self.format_working_dir(
            &self
                .workspace_root
                .join(".vtcode/context/tool_outputs")
                .join(format!("write_stdin_{}.txt", sanitize_session_id(&session_id))),
        );
        let output_spool_file_path = self.workspace_root.join(&output_spool_path);
        let (output_spool_tx, mut output_spool_rx) = tokio_mpsc::channel::<Vec<u8>>(64);
        let output_total_bytes = Arc::new(AtomicU64::new(0));
        let output_total_bytes_for_reader = Arc::clone(&output_total_bytes);
        let output_spool_failed = Arc::new(AtomicBool::new(false));
        let output_spool_failed_for_task = Arc::clone(&output_spool_failed);
        let output_spool_ready = Arc::new(AtomicBool::new(false));
        let output_spool_ready_for_task = Arc::clone(&output_spool_ready);
        let output_spool_finished = Arc::new(AtomicBool::new(false));
        let output_spool_finished_for_task = Arc::clone(&output_spool_finished);

        let output_spool_task = async move {
            let mut spool_file = if let Some(parent) = output_spool_file_path.parent()
                && tokio::fs::create_dir_all(parent).await.is_ok()
            {
                tokio::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&output_spool_file_path)
                    .await
                    .ok()
            } else {
                None
            };

            if spool_file.is_none() {
                output_spool_failed_for_task.store(true, Ordering::Release);
            } else {
                output_spool_ready_for_task.store(true, Ordering::Release);
            }
            let mut spool_redactor = vtcode_commons::sanitizer::StreamingSecretRedactor::default();

            while let Some(chunk) = output_spool_rx.recv().await {
                let Some(file) = spool_file.as_mut() else {
                    continue;
                };
                let text = String::from_utf8_lossy(&chunk);
                let sanitized = spool_redactor.push(&text);
                if !sanitized.is_empty() && file.write_all(sanitized.as_bytes()).await.is_err() {
                    output_spool_failed_for_task.store(true, Ordering::Release);
                    spool_file = None;
                }
            }

            if let Some(file) = spool_file.as_mut() {
                let sanitized = spool_redactor.finish();
                if (!sanitized.is_empty() && file.write_all(sanitized.as_bytes()).await.is_err())
                    || file.flush().await.is_err()
                {
                    output_spool_failed_for_task.store(true, Ordering::Release);
                }
            }
            output_spool_finished_for_task.store(true, Ordering::Release);
        };

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(output_spool_task);
        } else {
            output_spool_failed.store(true, Ordering::Release);
            output_spool_finished.store(true, Ordering::Release);
        }

        debug!(
            session_id = %session_id,
            rows = size.rows,
            cols = size.cols,
            "Created PTY session"
        );
        let screen_state_clone = Arc::clone(&screen_state);
        let scrollback_clone = Arc::clone(&scrollback);
        let reader_completed = Arc::new(AtomicBool::new(false));
        let reader_completed_clone = Arc::clone(&reader_completed);
        let session_name = session_id.clone();
        let output_spool_failed_for_reader = Arc::clone(&output_spool_failed);
        let mut output_spool_tx_for_reader = Some(output_spool_tx);
        // Start unicode monitoring for this session
        UNICODE_MONITOR.start_session();

        let reader_thread = thread::Builder::new()
            .name(format!("vtcode-pty-reader-{session_name}"))
            .spawn(move || {
                let mut buffer = [0u8; 8192]; // Increased buffer size for better performance
                let mut utf8_buffer: Vec<u8> = Vec::with_capacity(8192); // Pre-allocate buffer
                let mut total_bytes = 0usize;
                let mut unicode_detection_hits = 0usize;

                loop {
                    match reader.read(&mut buffer) {
Ok(0) => {
    if !utf8_buffer.is_empty() {
        let mut scrollback = scrollback_clone.lock();
        scrollback.push_utf8(&mut utf8_buffer, true);
    }
    debug!("PTY session '{}' reader reached EOF (processed {} bytes, {} unicode detections)",
           session_name, total_bytes, unicode_detection_hits);
    break;
}
                        Ok(bytes_read) => {
                            let chunk = &buffer[..bytes_read];
                            total_bytes += bytes_read;
                            output_total_bytes_for_reader.fetch_add(bytes_read as u64, Ordering::Relaxed);
                            let send_result = output_spool_tx_for_reader
                                .as_ref()
                                .map(|sender| sender.blocking_send(chunk.to_vec()));
                            if matches!(send_result, Some(Err(_))) {
                                output_spool_failed_for_reader.store(true, Ordering::Release);
                                warn!(
                                    "PTY session '{}' spool receiver closed after {} bytes; retaining screen and scrollback output",
                                    session_name, total_bytes
                                );
                                output_spool_tx_for_reader = None;
                            }

    // Quick unicode detection heuristic
    let likely_unicode = chunk.iter().any(|&b| b >= 0x80);
    if likely_unicode {
        unicode_detection_hits += 1;
    }

    {
        let mut screen_state = screen_state_clone.lock();
        screen_state.process(chunk);
    }

    utf8_buffer.extend_from_slice(chunk);
    {
        let mut scrollback = scrollback_clone.lock();
        scrollback.push_utf8(&mut utf8_buffer, false);
    }

    // Periodic buffer cleanup to prevent excessive memory usage
    if utf8_buffer.capacity() > 32768 && utf8_buffer.len() < 1024 {
        utf8_buffer.shrink_to_fit();
    }
}
Err(error) => {
    warn!("PTY session '{}' reader error: {} (processed {} bytes)",
          session_name, error, total_bytes);
    break;
}
                    }
                }
                debug!("PTY session '{}' reader thread finished (total: {} bytes, unicode detections: {})",
                       session_name, total_bytes, unicode_detection_hits);

                // Signal that the reader thread has finished pushing all data.
                // This must be set BEFORE any further scrollback lock acquisitions
                // so that is_output_drained() sees both pending==false and completed==true
                // atomically under the scrollback lock.
                reader_completed_clone.store(true, Ordering::Release);

                // End unicode monitoring for this session
                UNICODE_MONITOR.end_session();

                // Log unicode statistics if any unicode was detected
                if unicode_detection_hits > 0 {
                    let scrollback = scrollback_clone.lock();
                    let metrics = scrollback.metrics();
                    if metrics.unicode_errors > 0 {
warn!("PTY session '{}' had {} unicode errors during processing",
      session_name, metrics.unicode_errors);
                    }
                    if metrics.total_unicode_chars > 0 {
info!("PTY session '{}' processed {} unicode characters across {} sessions with {} buffer remainder",
      session_name, metrics.total_unicode_chars, metrics.unicode_sessions, metrics.utf8_buffer_size);
                    }
                }
            })
            .context("failed to spawn PTY reader thread")?;

        let metadata = VTCodePtySession {
            id: session_id.clone(),
            command: program,
            args,
            working_dir: Some(self.format_working_dir(&working_dir)),
            rows: size.rows,
            cols: size.cols,
            child_pid,
            started_at: Some(Utc::now()),
            lifecycle_state: Some(crate::tools::types::VTCodeSessionLifecycleState::Running),
            exit_code: None,
            screen_contents: None,
            scrollback: None,
        };

        // Use the entry we obtained earlier to insert without additional lookup
        entry.insert(Arc::new(PtySessionHandle {
            master: Mutex::new(master),
            child: Mutex::new(child),
            child_pid,
            writer: Mutex::new(Some(writer)),
            screen_state,
            scrollback,
            reader_thread: Mutex::new(Some(reader_thread)),
            reader_completed,
            metadata: metadata.clone(),
            last_input: Mutex::new(None),
            _zsh_exec_bridge: zsh_exec_bridge,
            output_total_bytes,
            output_spool_failed,
            output_spool_ready,
            output_spool_finished,
            output_spool_path,
        }));

        Ok(metadata)
    }

    fn format_working_dir(&self, path: &Path) -> String {
        match path.strip_prefix(&self.workspace_root) {
            Ok(relative) if relative.as_os_str().is_empty() => ".".into(),
            Ok(relative) => relative.to_string_lossy().replace("\\", "/"),
            Err(_) => path.to_string_lossy().into_owned(),
        }
    }

    fn ensure_within_workspace(&self, candidate: &Path) -> Result<()> {
        ensure_path_within_workspace(candidate, &self.workspace_root).map(|_| ())
    }
}
