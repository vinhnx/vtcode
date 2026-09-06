use hashbrown::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

use portable_pty::{CommandBuilder, PtySize};

use super::command_utils::is_shell_program;
use crate::sandboxing::build_sanitized_env;
use crate::tools::path_env;
use crate::tools::shell_snapshot::ShellSnapshot;

pub(super) fn clamp_timeout(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}

pub(super) fn exit_status_code(status: portable_pty::ExitStatus) -> i32 {
    if status.signal().is_some() {
        -1
    } else {
        status.exit_code() as i32
    }
}

pub(super) fn set_command_environment_with_sandbox(
    builder: &mut CommandBuilder,
    program: &str,
    size: PtySize,
    workspace_root: &Path,
    extra_paths: &[PathBuf],
    extra_env: &HashMap<String, String>,
    trusted_env: &HashMap<String, String>,
    sandbox_active: bool,
) {
    // `portable_pty::CommandBuilder` starts with a snapshot of the parent
    // environment. Clear that snapshot before applying the allowlist; merely
    // setting sanitized keys would otherwise leave filtered credentials and
    // loader variables available to the child.
    if sandbox_active {
        builder.env_clear();
    }

    // Start from the caller's environment and apply explicit overrides before
    // the shared sandbox allowlist. This keeps the policy identical for pipe
    // and PTY sessions and prevents arbitrary override names from reaching a
    // restricted child process.
    let mut env_map: HashMap<String, String> = std::env::vars().collect();
    for (key, value) in extra_env {
        env_map.insert(key.clone(), value.clone());
    }
    let env_map = if sandbox_active {
        build_sanitized_env(&env_map, true, false, "pty", &[])
    } else {
        env_map
    };

    // Internal bridge variables are supplied by the trusted PTY manager, not
    // by the command payload. Apply them after sanitization so the bridge
    // remains available without widening the user override allowlist.
    let mut env_map = env_map;
    for (key, value) in trusted_env {
        env_map.insert(key.clone(), value.clone());
    }

    // Ensure HOME is set - this is crucial for proper path expansion in cargo and other tools
    let home_key = "HOME".to_string();
    if let Some(home_dir) = dirs::home_dir() {
        env_map
            .entry(home_key)
            .or_insert_with(|| home_dir.to_string_lossy().into_owned());
    }

    let current_path = env_map.get("PATH").map(OsStr::new);
    if let Some(merged) = path_env::merge_path_env(current_path, extra_paths) {
        env_map.insert("PATH".to_string(), merged.to_string_lossy().into_owned());
    }

    for (key, value) in env_map {
        builder.env(OsString::from(key), OsString::from(value));
    }

    // Override or set specific environment variables for TTY
    builder.env("TERM", "xterm-256color");
    builder.env("PAGER", "cat");
    builder.env("GIT_PAGER", "cat");
    builder.env("LESS", "R");
    builder.env("COLUMNS", size.cols.to_string());
    builder.env("LINES", size.rows.to_string());
    builder.env("WORKSPACE_DIR", workspace_root.as_os_str());

    // Disable automatic color output from ls and other commands
    builder.env("CLICOLOR", "0");
    builder.env("CLICOLOR_FORCE", "0");
    builder.env("LS_COLORS", "");
    builder.env("NO_COLOR", "1");

    // For Rust/Cargo, disable colors at the source
    builder.env("CARGO_TERM_COLOR", "never");

    // Suppress macOS malloc debugging junk that can pollute PTY output
    // This is especially common when running in login shells (-l)
    builder.env_remove("MallocStackLogging");
    builder.env_remove("MallocStackLoggingNoCompact");
    builder.env_remove("MallocStackLoggingDirectory");
    builder.env_remove("MallocErrorAbort");
    builder.env_remove("MallocCheckHeapStart");
    builder.env_remove("MallocCheckHeapEach");
    builder.env_remove("MallocCheckHeapSleep");
    builder.env_remove("MallocCheckHeapAbort");
    builder.env_remove("MallocGuardEdges");
    builder.env_remove("MallocScribble");
    builder.env_remove("MallocDoNotProtectSentinel");
    builder.env_remove("MallocQuiet");

    if is_shell_program(program) {
        builder.env("SHELL", program);
    }
}

/// Set command environment from a shell snapshot for faster startup.
///
/// This uses a pre-captured shell environment instead of inheriting from the
/// parent process, which can speed up command execution by avoiding the need
/// to run login scripts via `-l` flag.
#[expect(
    dead_code,
    reason = "Intentional compatibility, platform, test, or API-shape suppression."
)] // Infrastructure for future snapshot-based PTY execution
fn set_command_environment_from_snapshot(
    builder: &mut CommandBuilder,
    snapshot: &ShellSnapshot,
    program: &str,
    size: PtySize,
    workspace_root: &Path,
    extra_paths: &[PathBuf],
) {
    // Start with the snapshot environment
    for (key, value) in &snapshot.env {
        builder.env(key, value);
    }

    // Merge extra paths into PATH
    let path_key = OsString::from("PATH");
    let current_path = snapshot.env.get("PATH").map(OsString::from);
    let current_path_ref = current_path.as_deref();
    if let Some(merged) = path_env::merge_path_env(current_path_ref, extra_paths) {
        builder.env(path_key, merged);
    }

    // Override or set specific environment variables for TTY
    builder.env("TERM", "xterm-256color");
    builder.env("PAGER", "cat");
    builder.env("GIT_PAGER", "cat");
    builder.env("LESS", "R");
    builder.env("COLUMNS", size.cols.to_string());
    builder.env("LINES", size.rows.to_string());
    builder.env("WORKSPACE_DIR", workspace_root.as_os_str());

    // Disable automatic color output
    builder.env("CLICOLOR", "0");
    builder.env("CLICOLOR_FORCE", "0");
    builder.env("LS_COLORS", "");
    builder.env("NO_COLOR", "1");
    builder.env("CARGO_TERM_COLOR", "never");

    // Suppress macOS malloc debugging
    builder.env_remove("MallocStackLogging");
    builder.env_remove("MallocStackLoggingNoCompact");
    builder.env_remove("MallocStackLoggingDirectory");
    builder.env_remove("MallocErrorAbort");
    builder.env_remove("MallocCheckHeapStart");
    builder.env_remove("MallocCheckHeapEach");
    builder.env_remove("MallocCheckHeapSleep");
    builder.env_remove("MallocCheckHeapAbort");
    builder.env_remove("MallocGuardEdges");
    builder.env_remove("MallocScribble");
    builder.env_remove("MallocDoNotProtectSentinel");
    builder.env_remove("MallocQuiet");

    if is_shell_program(program) {
        builder.env("SHELL", program);
    }
}
