use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use hashbrown::HashMap;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, RwLock, watch};
use tokio::task::JoinHandle;
use vtcode_bash_runner::{PipeSpawnOptions, ProcessHandle, spawn_pipe_process_with_options};

use crate::sandboxing::build_sanitized_env;
use crate::tools::ExecSessionId;
use crate::tools::pty::PtySize;
use crate::tools::registry::{PtySessionGuard, PtySessionManager};
use crate::tools::types::VTCodeExecSession;
use crate::utils::path::{canonicalize_workspace, ensure_path_within_workspace};
use crate::zsh_exec_bridge::ZshExecBridgeSession;

const PIPE_OUTPUT_HEAD_BYTES: usize = 8 * 1024;
const PIPE_OUTPUT_TAIL_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PipeOutputStats {
    pub total_bytes: u64,
    pub truncated: bool,
    pub spool_path: String,
    pub spool_available: bool,
    pub spool_complete: bool,
}

#[derive(Default)]
struct PipeOutputBuffer {
    pending: Mutex<PipeOutputWindow>,
    total_bytes: AtomicU64,
    truncated: AtomicBool,
}

#[derive(Default)]
struct PipeOutputWindow {
    head: String,
    tail: String,
    total_bytes: u64,
    truncated: bool,
}

impl PipeOutputBuffer {
    async fn append(&self, chunk: &str, raw_byte_count: usize) {
        let mut pending = self.pending.lock().await;
        pending.total_bytes = pending.total_bytes.saturating_add(raw_byte_count as u64);
        self.total_bytes.fetch_add(raw_byte_count as u64, Ordering::Relaxed);
        if pending.head.len() < PIPE_OUTPUT_HEAD_BYTES {
            let remaining = PIPE_OUTPUT_HEAD_BYTES - pending.head.len();
            let end = chunk.floor_char_boundary(remaining.min(chunk.len()));
            pending.head.push_str(&chunk[..end]);
            if end < chunk.len() {
                pending.truncated = true;
                self.truncated.store(true, Ordering::Relaxed);
            }
        }

        // Operate on `pending.tail` in place. The previous code cloned the tail,
        // mutated the clone, then assigned it back — an O(tail) copy on every
        // output chunk. Since `pending` is already a &mut MutexGuard, in-place
        // mutation is equivalent and avoids the per-chunk allocation.
        pending.tail.push_str(chunk);
        if pending.tail.len() > PIPE_OUTPUT_TAIL_BYTES {
            let start = pending.tail.ceil_char_boundary(pending.tail.len() - PIPE_OUTPUT_TAIL_BYTES);
            pending.tail.drain(..start);
            pending.truncated = true;
            self.truncated.store(true, Ordering::Relaxed);
        }
    }

    async fn peek_pending(&self) -> Option<String> {
        let pending = self.pending.lock().await;
        if pending.total_bytes == 0 {
            None
        } else {
            Some(pending.preview())
        }
    }

    async fn drain_pending(&self) -> Option<String> {
        let mut pending = self.pending.lock().await;
        if pending.total_bytes == 0 {
            None
        } else {
            Some(std::mem::take(&mut *pending).preview())
        }
    }

    async fn stats(&self) -> (u64, bool) {
        (self.total_bytes.load(Ordering::Relaxed), self.truncated.load(Ordering::Relaxed))
    }
}

impl PipeOutputWindow {
    fn preview(&self) -> String {
        if !self.truncated {
            return self.head.clone();
        }
        if self.head == self.tail {
            return self.head.clone();
        }
        format!("{}\n[output preview truncated]\n{}", self.head, self.tail)
    }
}

struct PipeSessionRecord {
    metadata: VTCodeExecSession,
    handle: Arc<ProcessHandle>,
    output: Arc<PipeOutputBuffer>,
    output_task: Mutex<Option<JoinHandle<()>>>,
    exit_task: Mutex<Option<JoinHandle<()>>>,
    activity_tx: watch::Sender<u64>,
    spool: PipeSpoolState,
}

struct PipeSpoolState {
    path: String,
    ready: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
}

impl PipeSessionRecord {
    fn new(
        metadata: VTCodeExecSession,
        handle: Arc<ProcessHandle>,
        output: Arc<PipeOutputBuffer>,
        output_task: JoinHandle<()>,
        exit_task: JoinHandle<()>,
        activity_tx: watch::Sender<u64>,
        spool: PipeSpoolState,
    ) -> Self {
        Self {
            metadata,
            handle,
            output,
            output_task: Mutex::new(Some(output_task)),
            exit_task: Mutex::new(Some(exit_task)),
            activity_tx,
            spool,
        }
    }
}

#[derive(Clone)]
struct PipeSessionManager {
    workspace_root: PathBuf,
    sessions: Arc<RwLock<HashMap<ExecSessionId, Arc<PipeSessionRecord>>>>,
}

impl PipeSessionManager {
    fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root: canonicalize_workspace(&workspace_root),
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn create_session(
        &self,
        session_id: ExecSessionId,
        command: Vec<String>,
        working_dir: PathBuf,
        env: HashMap<String, String>,
    ) -> Result<VTCodeExecSession> {
        if command.is_empty() {
            return Err(anyhow!("exec session command cannot be empty"));
        }
        // Canonicalization does sync fs I/O; keep it off the runtime worker
        // since this runs per spawned exec session.
        let working_dir = tokio::task::spawn_blocking({
            let working_dir = working_dir.clone();
            move || canonicalize_workspace(&working_dir)
        })
        .await
        .unwrap_or(working_dir);
        self.ensure_within_workspace(&working_dir)?;

        // Hold the write lock across check → spawn → insert so two concurrent
        // creates with the same session_id cannot both pass the existence check
        // and spawn; the second insert would silently overwrite the first and
        // leak its spawned process and background tasks.
        let mut sessions = self.sessions.write().await;
        if sessions.contains_key(session_id.as_str()) {
            return Err(anyhow!("exec session '{}' already exists", session_id.as_str()));
        }

        let mut command_parts = command;
        let program = command_parts.remove(0);
        let args = command_parts;

        let opts = PipeSpawnOptions::new(program.clone(), working_dir.clone())
            .args(args.clone())
            .env(env)
            .lossless_output(true);
        let spawned = spawn_pipe_process_with_options(opts)
            .await
            .with_context(|| format!("failed to spawn pipe session '{session_id}'"))?;

        let metadata = VTCodeExecSession {
            id: session_id.clone(),
            backend: "pipe".to_string(),
            command: program,
            args,
            working_dir: Some(self.format_working_dir(&working_dir)),
            rows: None,
            cols: None,
            child_pid: None,
            started_at: Some(Utc::now()),
            lifecycle_state: Some(crate::tools::types::VTCodeSessionLifecycleState::Running),
            exit_code: None,
        };

        let handle = Arc::new(spawned.session);
        let output = Arc::new(PipeOutputBuffer::default());
        let output_clone = Arc::clone(&output);
        let mut output_rx = spawned.reliable_output_rx;
        let output_handle = Arc::clone(&handle);
        let (activity_tx, _) = watch::channel(0u64);
        let output_activity_tx = activity_tx.clone();
        let spool_path = self.format_working_dir(
            &self
                .workspace_root
                .join(".vtcode/context/tool_outputs")
                .join(format!("write_stdin_{session_id}.txt")),
        );
        let spool_file_path = self.workspace_root.join(&spool_path);
        let spool_ready = Arc::new(AtomicBool::new(false));
        let spool_ready_for_task = Arc::clone(&spool_ready);
        let spool_failed = Arc::new(AtomicBool::new(false));
        let spool_failed_for_task = Arc::clone(&spool_failed);
        let spool_finished = Arc::new(AtomicBool::new(false));
        let spool_finished_for_task = Arc::clone(&spool_finished);
        let output_task = tokio::spawn(async move {
            let mut spool_file =
                if tokio::fs::create_dir_all(spool_file_path.parent().unwrap_or_else(|| Path::new(".")))
                    .await
                    .is_ok()
                {
                    tokio::fs::OpenOptions::new()
                        .create(true)
                        .write(true)
                        .truncate(true)
                        .open(&spool_file_path)
                        .await
                        .ok()
                } else {
                    None
                };
            if spool_file.is_none() {
                spool_failed_for_task.store(true, Ordering::Release);
            } else {
                spool_ready_for_task.store(true, Ordering::Release);
            }
            let mut spool_redactor = vtcode_commons::sanitizer::StreamingSecretRedactor::default();
            loop {
                match tokio::time::timeout(tokio::time::Duration::from_millis(15), output_rx.recv()).await {
                    Ok(Some(chunk)) => {
                        // Keep the decoded text as a `Cow<str>` so that the
                        // common case (valid UTF-8) borrows `chunk` with zero
                        // allocation. `.into_owned()` would force a String copy
                        // on every chunk even when the bytes are already valid.
                        let text = String::from_utf8_lossy(&chunk);
                        if let Some(file) = spool_file.as_mut() {
                            let sanitized = spool_redactor.push(&text);
                            if !sanitized.is_empty() && file.write_all(sanitized.as_bytes()).await.is_err() {
                                spool_failed_for_task.store(true, Ordering::Release);
                                spool_file = None;
                            }
                        }
                        output_clone.append(&text, chunk.len()).await;
                        output_activity_tx.send_modify(|version| *version += 1);
                    }
                    Ok(None) => break,
                    Err(_) if output_handle.has_exited() && output_handle.is_output_drained() => {
                        break;
                    }
                    Err(_) => continue,
                }
            }
            if let Some(file) = spool_file.as_mut() {
                let sanitized = spool_redactor.finish();
                if (!sanitized.is_empty() && file.write_all(sanitized.as_bytes()).await.is_err())
                    || file.flush().await.is_err()
                {
                    spool_failed_for_task.store(true, Ordering::Release);
                }
            }
            spool_finished_for_task.store(true, Ordering::Release);
        });
        let exit_rx = spawned.exit_rx;
        let exit_activity_tx = activity_tx.clone();
        let exit_task = tokio::spawn(async move {
            let _ = exit_rx.await;
            exit_activity_tx.send_modify(|version| *version += 1);
        });
        let record = Arc::new(PipeSessionRecord::new(
            metadata.clone(),
            handle,
            output,
            output_task,
            exit_task,
            activity_tx,
            PipeSpoolState {
                path: spool_path,
                ready: spool_ready,
                failed: spool_failed,
                finished: spool_finished,
            },
        ));

        sessions.insert(session_id, record);

        Ok(metadata)
    }

    async fn read_session_output(&self, session_id: &str, drain: bool) -> Result<Option<String>> {
        let record = self.session_record(session_id).await?;
        if drain {
            Ok(record.output.drain_pending().await)
        } else {
            Ok(record.output.peek_pending().await)
        }
    }

    async fn output_stats(&self, session_id: &str) -> Result<PipeOutputStats> {
        let record = self.session_record(session_id).await?;
        let (total_bytes, truncated) = record.output.stats().await;
        let spool_available =
            record.spool.ready.load(Ordering::Acquire) && !record.spool.failed.load(Ordering::Acquire);
        Ok(PipeOutputStats {
            total_bytes,
            truncated,
            spool_path: record.spool.path.clone(),
            spool_available,
            spool_complete: spool_available && record.spool.finished.load(Ordering::Acquire),
        })
    }

    async fn send_input_to_session(&self, session_id: &str, data: &[u8], append_newline: bool) -> Result<usize> {
        let record = self.session_record(session_id).await?;
        record
            .handle
            .write(data.to_vec())
            .await
            .map_err(|e| anyhow!("exec session '{session_id}' is no longer writable: {e}"))?;

        if append_newline {
            record
                .handle
                .write(b"\n".to_vec())
                .await
                .map_err(|e| anyhow!("exec session '{session_id}' is no longer writable: {e}"))?;
        }

        Ok(data.len() + usize::from(append_newline))
    }

    async fn is_session_completed(&self, session_id: &str) -> Result<Option<i32>> {
        let record = self.session_record(session_id).await?;
        if record.handle.has_exited() {
            Ok(record.handle.exit_code())
        } else {
            Ok(None)
        }
    }

    async fn terminate_session(&self, session_id: &str) -> Result<()> {
        let record = self.session_record(session_id).await?;
        record.handle.terminate();
        Ok(())
    }

    async fn close_session(&self, session_id: &str) -> Result<VTCodeExecSession> {
        let record = {
            let mut sessions = self.sessions.write().await;
            sessions
                .remove(session_id)
                .ok_or_else(|| anyhow!("exec session '{session_id}' not found. Copy the exact `session_id` from the original run response `next_wait_args`/`next_continue_args`; do not invent or reuse an older session id. If the session already exited, re-run the command instead of waiting"))?
        };

        record.handle.terminate();
        if let Some(task) = record.output_task.lock().await.take() {
            task.abort();
        }
        if let Some(task) = record.exit_task.lock().await.take() {
            task.abort();
        }

        Ok(record.metadata.clone())
    }

    async fn activity_receiver(&self, session_id: &str) -> Result<watch::Receiver<u64>> {
        let record = self.session_record(session_id).await?;
        Ok(record.activity_tx.subscribe())
    }

    async fn is_output_drained(&self, session_id: &str) -> Result<bool> {
        let record = self.session_record(session_id).await?;
        let output_task = record.output_task.lock().await;
        let output_task_finished = match output_task.as_ref() {
            Some(task) => task.is_finished(),
            None => true,
        };
        Ok(record.handle.is_output_drained() && output_task_finished)
    }

    async fn terminate_all_sessions(&self) -> Result<()> {
        let ids = {
            let sessions = self.sessions.read().await;
            sessions.keys().cloned().collect::<Vec<_>>()
        };

        for session_id in ids {
            self.close_session(&session_id).await?;
        }

        Ok(())
    }

    async fn session_record(&self, session_id: &str) -> Result<Arc<PipeSessionRecord>> {
        let sessions = self.sessions.read().await;
        sessions
            .get(session_id)
            .cloned()
            .ok_or_else(|| anyhow!("exec session '{session_id}' not found. Copy the exact `session_id` from the original run response `next_wait_args`/`next_continue_args`; do not invent or reuse an older session id. If the session already exited, re-run the command instead of waiting"))
    }

    fn ensure_within_workspace(&self, candidate: &Path) -> Result<()> {
        ensure_path_within_workspace(candidate, &self.workspace_root).map(|_| ())
    }

    fn format_working_dir(&self, path: &Path) -> String {
        match path.strip_prefix(&self.workspace_root) {
            Ok(relative) if relative.as_os_str().is_empty() => ".".into(),
            Ok(relative) => relative.to_string_lossy().replace("\\", "/"),
            Err(_) => path.to_string_lossy().into_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecSessionBackend {
    Pipe,
    Pty,
}

struct ExecSessionRecord {
    metadata: VTCodeExecSession,
    backend: ExecSessionBackend,
    _pty_guard: Option<PtySessionGuard>,
}

impl ExecSessionRecord {
    fn new(metadata: VTCodeExecSession, backend: ExecSessionBackend, pty_guard: Option<PtySessionGuard>) -> Self {
        Self { metadata, backend, _pty_guard: pty_guard }
    }
}

#[derive(Clone)]
pub struct ExecSessionManager {
    pipe_sessions: PipeSessionManager,
    pty_sessions: PtySessionManager,
    sessions: Arc<RwLock<HashMap<ExecSessionId, Arc<ExecSessionRecord>>>>,
}

impl ExecSessionManager {
    #[must_use]
    pub fn new(workspace_root: PathBuf, pty_sessions: PtySessionManager) -> Self {
        Self {
            pipe_sessions: PipeSessionManager::new(workspace_root),
            pty_sessions,
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub(crate) async fn create_pipe_session(
        &self,
        session_id: ExecSessionId,
        command: Vec<String>,
        working_dir: PathBuf,
        env: HashMap<String, String>,
    ) -> Result<VTCodeExecSession> {
        self.create_pipe_session_with_sandbox(session_id, command, working_dir, env, false)
            .await
    }

    pub(crate) async fn create_pipe_session_with_sandbox(
        &self,
        session_id: ExecSessionId,
        command: Vec<String>,
        working_dir: PathBuf,
        env: HashMap<String, String>,
        sandbox_active: bool,
    ) -> Result<VTCodeExecSession> {
        self.ensure_session_absent(&session_id).await?;
        let env = if sandbox_active {
            build_sanitized_env(&env, true, false, "exec-session", &[])
        } else {
            env
        };
        let metadata = self.pipe_sessions.create_session(session_id, command, working_dir, env).await?;
        self.insert_session(metadata.clone(), ExecSessionBackend::Pipe, None).await?;
        Ok(metadata)
    }

    pub(crate) async fn create_pty_session(
        &self,
        session_id: ExecSessionId,
        command: Vec<String>,
        working_dir: PathBuf,
        size: PtySize,
        extra_env: HashMap<String, String>,
        zsh_exec_bridge: Option<ZshExecBridgeSession>,
    ) -> Result<VTCodeExecSession> {
        self.create_pty_session_with_sandbox(
            session_id,
            command,
            working_dir,
            size,
            extra_env,
            zsh_exec_bridge,
            HashMap::new(),
            false,
        )
        .await
    }

    pub(crate) async fn create_pty_session_with_sandbox(
        &self,
        session_id: ExecSessionId,
        command: Vec<String>,
        working_dir: PathBuf,
        size: PtySize,
        extra_env: HashMap<String, String>,
        zsh_exec_bridge: Option<ZshExecBridgeSession>,
        trusted_env: HashMap<String, String>,
        sandbox_active: bool,
    ) -> Result<VTCodeExecSession> {
        self.ensure_session_absent(&session_id).await?;
        let pty_guard = self.pty_sessions.start_session()?;
        let metadata = self.pty_sessions.manager().create_session_with_bridge_sandboxed(
            session_id.clone().into(),
            command,
            working_dir,
            size,
            extra_env,
            zsh_exec_bridge,
            trusted_env,
            sandbox_active,
        )?;
        let exec_metadata = VTCodeExecSession::from(metadata);
        self.insert_session(exec_metadata.clone(), ExecSessionBackend::Pty, Some(pty_guard))
            .await?;
        Ok(exec_metadata)
    }

    pub(crate) async fn snapshot_session(&self, session_id: &str) -> Result<VTCodeExecSession> {
        let record = self.session_record(session_id).await?;
        match record.backend {
            ExecSessionBackend::Pipe => self.pipe_sessions.session_record(session_id).await.map(|r| {
                let mut metadata = r.metadata.clone();
                let exit_code = if r.handle.has_exited() {
                    r.handle.exit_code()
                } else {
                    None
                };
                metadata.exit_code = exit_code;
                metadata.lifecycle_state = Some(if exit_code.is_some() {
                    crate::tools::types::VTCodeSessionLifecycleState::Exited
                } else {
                    crate::tools::types::VTCodeSessionLifecycleState::Running
                });
                metadata
            }),
            ExecSessionBackend::Pty => self
                .pty_sessions
                .manager()
                .snapshot_session(session_id)
                .map(VTCodeExecSession::from),
        }
    }

    pub(crate) async fn list_sessions(&self) -> Vec<VTCodeExecSession> {
        let sessions = self.sessions.read().await;
        let mut listed = sessions.values().map(|record| record.metadata.clone()).collect::<Vec<_>>();
        listed.sort_by(|left, right| left.id.cmp(&right.id));
        listed
    }

    pub(crate) async fn read_session_output(&self, session_id: &str, drain: bool) -> Result<Option<String>> {
        let record = self.session_record(session_id).await?;
        match record.backend {
            ExecSessionBackend::Pipe => self.pipe_sessions.read_session_output(session_id, drain).await,
            ExecSessionBackend::Pty => self.pty_sessions.manager().read_session_output(session_id, drain),
        }
    }

    pub(crate) async fn output_stats(&self, session_id: &str) -> Result<Option<PipeOutputStats>> {
        let record = self.session_record(session_id).await?;
        match record.backend {
            ExecSessionBackend::Pipe => self.pipe_sessions.output_stats(session_id).await.map(Some),
            ExecSessionBackend::Pty => self.pty_sessions.manager().output_stats(session_id).map(|stats| {
                stats.map(|stats| PipeOutputStats {
                    total_bytes: stats.total_bytes,
                    truncated: stats.truncated,
                    spool_path: stats.spool_path,
                    spool_available: stats.spool_available,
                    spool_complete: stats.spool_complete,
                })
            }),
        }
    }

    pub(crate) async fn send_input_to_session(
        &self,
        session_id: &str,
        data: &[u8],
        append_newline: bool,
    ) -> Result<usize> {
        let record = self.session_record(session_id).await?;
        match record.backend {
            ExecSessionBackend::Pipe => {
                self.pipe_sessions.send_input_to_session(session_id, data, append_newline).await
            }
            ExecSessionBackend::Pty => {
                self.pty_sessions
                    .manager()
                    .send_input_to_session(session_id, data, append_newline)
            }
        }
    }

    pub(crate) async fn is_session_completed(&self, session_id: &str) -> Result<Option<i32>> {
        let record = self.session_record(session_id).await?;
        match record.backend {
            ExecSessionBackend::Pipe => self.pipe_sessions.is_session_completed(session_id).await,
            ExecSessionBackend::Pty => self.pty_sessions.manager().is_session_completed(session_id),
        }
    }

    pub(crate) async fn activity_receiver(&self, session_id: &str) -> Result<Option<watch::Receiver<u64>>> {
        let record = self.session_record(session_id).await?;
        match record.backend {
            ExecSessionBackend::Pipe => self.pipe_sessions.activity_receiver(session_id).await.map(Some),
            ExecSessionBackend::Pty => Ok(None),
        }
    }

    pub(crate) async fn is_output_drained(&self, session_id: &str) -> Result<bool> {
        let record = self.session_record(session_id).await?;
        match record.backend {
            ExecSessionBackend::Pipe => self.pipe_sessions.is_output_drained(session_id).await,
            ExecSessionBackend::Pty => self.pty_sessions.manager().is_output_drained(session_id),
        }
    }

    pub(crate) async fn terminate_session(&self, session_id: &str) -> Result<()> {
        let record = self.session_record(session_id).await?;
        match record.backend {
            ExecSessionBackend::Pipe => self.pipe_sessions.terminate_session(session_id).await,
            ExecSessionBackend::Pty => self.pty_sessions.manager().terminate_session(session_id),
        }
    }

    pub(crate) async fn close_session(&self, session_id: &str) -> Result<VTCodeExecSession> {
        let record = {
            let mut sessions = self.sessions.write().await;
            sessions
                .remove(session_id)
                .ok_or_else(|| anyhow!("exec session '{session_id}' not found. Copy the exact `session_id` from the original run response `next_wait_args`/`next_continue_args`; do not invent or reuse an older session id. If the session already exited, re-run the command instead of waiting"))?
        };

        let metadata = match record.backend {
            ExecSessionBackend::Pipe => self.pipe_sessions.close_session(session_id).await?,
            ExecSessionBackend::Pty => self
                .pty_sessions
                .manager()
                .close_session(session_id)
                .map(VTCodeExecSession::from)?,
        };

        Ok(metadata)
    }

    pub(crate) async fn prune_exited_session(&self, session_id: &str) -> Result<Option<VTCodeExecSession>> {
        if self.is_session_completed(session_id).await?.is_some() {
            return self.close_session(session_id).await.map(Some);
        }
        Ok(None)
    }

    pub(crate) async fn terminate_all_sessions_async(&self) -> Result<()> {
        let ids = {
            let sessions = self.sessions.read().await;
            sessions.keys().cloned().collect::<Vec<_>>()
        };

        let mut failures = Vec::new();
        for session_id in ids {
            if let Err(err) = self.close_session(&session_id).await {
                failures.push(format!("{session_id}: {err}"));
            }
        }

        if let Err(err) = self.pipe_sessions.terminate_all_sessions().await {
            failures.push(err.to_string());
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(anyhow!("failed to terminate all exec sessions: {}", failures.join("; ")))
        }
    }

    async fn insert_session(
        &self,
        metadata: VTCodeExecSession,
        backend: ExecSessionBackend,
        pty_guard: Option<PtySessionGuard>,
    ) -> Result<()> {
        let mut sessions = self.sessions.write().await;
        use hashbrown::hash_map::Entry;
        match sessions.entry(metadata.id.clone()) {
            Entry::Occupied(_) => Err(anyhow!("exec session '{}' already exists", metadata.id.as_str())),
            Entry::Vacant(entry) => {
                entry.insert(Arc::new(ExecSessionRecord::new(metadata, backend, pty_guard)));
                Ok(())
            }
        }
    }

    async fn ensure_session_absent(&self, session_id: &str) -> Result<()> {
        let sessions = self.sessions.read().await;
        if sessions.contains_key(session_id) {
            return Err(anyhow!("exec session '{session_id}' already exists"));
        }
        Ok(())
    }

    async fn session_record(&self, session_id: &str) -> Result<Arc<ExecSessionRecord>> {
        let sessions = self.sessions.read().await;
        sessions
            .get(session_id)
            .cloned()
            .ok_or_else(|| anyhow!("exec session '{session_id}' not found. Copy the exact `session_id` from the original run response `next_wait_args`/`next_continue_args`; do not invent or reuse an older session id. If the session already exited, re-run the command instead of waiting"))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use hashbrown::HashMap;
    use tempfile::tempdir;
    use tokio::time::{Duration, timeout};

    use super::ExecSessionManager;
    use crate::config::PtyConfig;
    use crate::tools::pty::PtySize;
    use crate::tools::registry::PtySessionManager;
    use crate::utils::path::canonicalize_workspace;

    #[tokio::test]
    #[cfg(all(unix, feature = "tui"))]
    async fn pty_session_limit_holds_until_exec_session_close() -> anyhow::Result<()> {
        let temp_dir = tempdir()?;
        let workspace_root = canonicalize_workspace(temp_dir.path());
        let pty_sessions =
            PtySessionManager::new(workspace_root.clone(), PtyConfig { max_sessions: 1, ..Default::default() });
        let manager = ExecSessionManager::new(workspace_root.clone(), pty_sessions);
        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };

        manager
            .create_pty_session(
                "run-1".to_string().into(),
                vec!["/bin/sh".to_string(), "-c".to_string(), "sleep 1".to_string()],
                workspace_root.clone(),
                size,
                HashMap::new(),
                None,
            )
            .await?;

        let second = manager
            .create_pty_session(
                "run-2".to_string().into(),
                vec!["/bin/sh".to_string(), "-c".to_string(), "sleep 1".to_string()],
                workspace_root.clone(),
                size,
                HashMap::new(),
                None,
            )
            .await;
        assert!(second.is_err());
        assert!(second.unwrap_err().to_string().contains("Maximum PTY sessions"));

        manager.close_session("run-1").await?;
        manager
            .create_pty_session(
                "run-3".to_string().into(),
                vec!["/bin/sh".to_string(), "-c".to_string(), "sleep 1".to_string()],
                workspace_root,
                size,
                HashMap::new(),
                None,
            )
            .await?;
        manager.close_session("run-3").await?;

        Ok(())
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn pipe_session_activity_receiver_notifies_on_output() -> anyhow::Result<()> {
        let temp_dir = tempdir()?;
        let workspace_root = canonicalize_workspace(temp_dir.path());
        let pty_sessions = PtySessionManager::new(workspace_root.clone(), PtyConfig::default());
        let manager = ExecSessionManager::new(workspace_root.clone(), pty_sessions);

        manager
            .create_pipe_session(
                "run-1".to_string().into(),
                vec!["/bin/sh".to_string(), "-c".to_string(), "printf hello".to_string()],
                workspace_root,
                HashMap::new(),
            )
            .await?;

        let mut activity_rx = manager
            .activity_receiver("run-1")
            .await?
            .expect("pipe sessions should expose activity receiver");

        let output = timeout(Duration::from_secs(2), async {
            loop {
                if let Some(output) = manager.read_session_output("run-1", true).await? {
                    return Ok::<String, anyhow::Error>(output);
                }
                activity_rx.changed().await?;
            }
        })
        .await??;
        assert!(output.contains("hello"));

        manager.close_session("run-1").await?;
        Ok(())
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn concurrent_pipe_session_create_with_same_id_creates_exactly_one() -> anyhow::Result<()> {
        let temp_dir = tempdir()?;
        let workspace_root = canonicalize_workspace(temp_dir.path());
        let pty_sessions = PtySessionManager::new(workspace_root.clone(), PtyConfig::default());
        let manager = ExecSessionManager::new(workspace_root.clone(), pty_sessions);

        let (a, b) = tokio::join!(
            manager.create_pipe_session(
                "same-id".to_string().into(),
                vec!["/bin/sh".to_string(), "-c".to_string(), "sleep 5".to_string()],
                workspace_root.clone(),
                HashMap::new(),
            ),
            manager.create_pipe_session(
                "same-id".to_string().into(),
                vec!["/bin/sh".to_string(), "-c".to_string(), "sleep 5".to_string()],
                workspace_root.clone(),
                HashMap::new(),
            ),
        );

        assert_eq!(a.is_ok() as u8 + b.is_ok() as u8, 1, "exactly one concurrent create must win: {a:?} {b:?}");
        let loser = a.err().or_else(|| b.err()).expect("the loser should error");
        assert!(loser.to_string().contains("already exists"), "loser error should report duplicate: {loser}");

        manager.close_session("same-id").await?;
        Ok(())
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn pipe_session_drain_clears_so_old_output_does_not_reappear() -> anyhow::Result<()> {
        let temp_dir = tempdir()?;
        let workspace_root = canonicalize_workspace(temp_dir.path());
        let pty_sessions = PtySessionManager::new(workspace_root.clone(), PtyConfig::default());
        let manager = ExecSessionManager::new(workspace_root.clone(), pty_sessions);

        manager
            .create_pipe_session(
                "drain-clear".to_string().into(),
                vec!["/bin/sh".to_string(), "-c".to_string(), "printf hello".to_string()],
                workspace_root,
                HashMap::new(),
            )
            .await?;

        let mut activity_rx = manager
            .activity_receiver("drain-clear")
            .await?
            .expect("pipe sessions should expose activity receiver");

        timeout(Duration::from_secs(2), activity_rx.changed()).await??;
        let drained = manager
            .read_session_output("drain-clear", true)
            .await?
            .expect("should drain hello");
        assert!(drained.contains("hello"));

        let stale = manager.read_session_output("drain-clear", true).await?;
        assert!(stale.is_none(), "drained output must not reappear: {stale:?}");

        manager.close_session("drain-clear").await?;
        Ok(())
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn pipe_session_returns_new_output_after_drain() -> anyhow::Result<()> {
        let temp_dir = tempdir()?;
        let workspace_root = canonicalize_workspace(temp_dir.path());
        let pty_sessions = PtySessionManager::new(workspace_root.clone(), PtyConfig::default());
        let manager = ExecSessionManager::new(workspace_root.clone(), pty_sessions);

        manager
            .create_pipe_session(
                "drain-resume".to_string().into(),
                vec![
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    "printf one; sleep 1; printf two".to_string(),
                ],
                workspace_root,
                HashMap::new(),
            )
            .await?;

        let mut activity_rx = manager
            .activity_receiver("drain-resume")
            .await?
            .expect("pipe sessions should expose activity receiver");

        timeout(Duration::from_secs(2), activity_rx.changed()).await??;
        let _first = manager.read_session_output("drain-resume", true).await?;

        timeout(Duration::from_secs(3), activity_rx.changed()).await??;
        let second = manager
            .read_session_output("drain-resume", true)
            .await?
            .expect("should drain post-drain output");
        assert!(second.contains("two"), "output produced after a drain must still be returned: {second:?}");

        manager.close_session("drain-resume").await?;
        Ok(())
    }

    #[tokio::test]
    async fn pipe_output_buffer_peek_is_idempotent_and_non_consuming() {
        let buffer = super::PipeOutputBuffer::default();
        buffer.append("hello", 5).await;

        let first = buffer.peek_pending().await;
        let second = buffer.peek_pending().await;
        assert_eq!(first, second);
        assert_eq!(first, Some("hello".to_string()));
    }

    #[tokio::test]
    async fn pipe_output_buffer_drain_returns_exactly_once() {
        let buffer = super::PipeOutputBuffer::default();
        buffer.append("hello", 5).await;

        let first = buffer.drain_pending().await;
        let second = buffer.drain_pending().await;
        assert_eq!(first, Some("hello".to_string()));
        assert_eq!(second, None);
    }

    #[tokio::test]
    async fn pipe_output_buffer_drain_clears_internal_pending_length() {
        let buffer = super::PipeOutputBuffer::default();
        buffer.append("hello", 5).await;

        buffer.drain_pending().await;
        let peek: Option<String> = buffer.peek_pending().await;
        assert!(peek.is_none(), "buffer must be empty after drain: {peek:?}");
    }

    #[tokio::test]
    async fn pipe_output_buffer_append_after_drain_returns_only_fresh_output() {
        let buffer = super::PipeOutputBuffer::default();
        buffer.append("first", 5).await;
        buffer.drain_pending().await;

        buffer.append("second", 6).await;
        let output = buffer.peek_pending().await;
        assert_eq!(output, Some("second".to_string()));
    }

    #[tokio::test]
    async fn pipe_output_buffer_bounds_preview_and_tracks_total_bytes() {
        let buffer = super::PipeOutputBuffer::default();
        let chunk = "x".repeat(super::PIPE_OUTPUT_HEAD_BYTES * 4);
        buffer.append(&chunk, chunk.len()).await;

        let preview = buffer.peek_pending().await.expect("bounded preview");
        assert!(preview.len() <= super::PIPE_OUTPUT_HEAD_BYTES * 3);
        let (total_bytes, truncated) = buffer.stats().await;
        assert_eq!(total_bytes, chunk.len() as u64);
        assert!(truncated);
    }
}
