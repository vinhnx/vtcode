use crate::executor::{CommandCategory, CommandExecutor, CommandInvocation, CommandOutput, ShellKind};
use crate::policy::CommandPolicy;
use anyhow::{Context, Result, anyhow, bail};
use path_clean::PathClean;
use shell_escape::escape;
use std::fs;
use std::path::{Path, PathBuf};
use vtcode_commons::{WorkspacePaths, canonicalize};

pub struct BashRunner<E, P> {
    executor: E,
    policy: P,
    workspace_root: PathBuf,
    working_dir: PathBuf,
    shell_kind: ShellKind,
}

impl<E, P> BashRunner<E, P>
where
    E: CommandExecutor,
    P: CommandPolicy,
{
    pub fn new(workspace_root: PathBuf, executor: E, policy: P) -> Result<Self> {
        if !workspace_root.exists() {
            bail!("workspace root `{}` does not exist", workspace_root.display());
        }

        let canonical_root = canonicalize(&workspace_root)
            .with_context(|| format!("failed to canonicalize `{}`", workspace_root.display()))?;

        Ok(Self {
            executor,
            policy,
            workspace_root: canonical_root.clone(),
            working_dir: canonical_root,
            shell_kind: default_shell_kind(),
        })
    }

    pub fn from_workspace_paths<W>(paths: &W, executor: E, policy: P) -> Result<Self>
    where
        W: WorkspacePaths,
    {
        Self::new(paths.workspace_root().to_path_buf(), executor, policy)
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    fn working_dir(&self) -> &Path {
        &self.working_dir
    }

    pub fn shell_kind(&self) -> ShellKind {
        self.shell_kind
    }

    /// Authorization must resolve the current filesystem, never a cached symlink target.
    fn resolve_canonical_path(&self, path: &Path) -> Result<PathBuf> {
        canonicalize(path).with_context(|| format!("failed to canonicalize `{}`", path.display()))
    }

    pub fn cd(&mut self, path: &str) -> Result<()> {
        let candidate = self.resolve_path(path)?;
        if !candidate.exists() {
            bail!("directory `{}` does not exist", candidate.display());
        }
        if !candidate.is_dir() {
            bail!("path `{}` is not a directory", candidate.display());
        }

        let canonical = self.resolve_canonical_path(&candidate)?;

        self.ensure_within_workspace(&canonical)?;

        let invocation = CommandInvocation::new(
            self.shell_kind,
            format!("cd {}", format_path(self.shell_kind, &canonical)),
            CommandCategory::ChangeDirectory,
            canonical.clone(),
        )
        .with_paths(vec![canonical.clone()]);

        self.policy.check(&invocation)?;
        self.working_dir = canonical;
        Ok(())
    }

    pub fn ls(&self, path: Option<&str>, show_hidden: bool) -> Result<String> {
        let target = path
            .map(|p| self.resolve_existing_path(p))
            .transpose()?
            .unwrap_or_else(|| self.working_dir.clone());

        let command = match self.shell_kind {
            ShellKind::Unix => ShellCommand::new(ShellKind::Unix)
                .verb("ls")
                .flag(if show_hidden { "la" } else { "l" })
                .value(format_path(ShellKind::Unix, &target))
                .build(),
            ShellKind::Windows => ShellCommand::new(ShellKind::Windows)
                .verb("Get-ChildItem")
                .flag_if(show_hidden, "Force")
                .named("Path", format_path(ShellKind::Windows, &target))
                .build(),
        };

        let invocation =
            CommandInvocation::new(self.shell_kind, command, CommandCategory::ListDirectory, self.working_dir.clone())
                .with_paths(vec![target]);

        let output = self.expect_success(invocation)?;
        Ok(output.stdout)
    }

    pub fn pwd(&self) -> Result<String> {
        let command = match self.shell_kind {
            ShellKind::Unix => ShellCommand::new(ShellKind::Unix).verb("pwd").build(),
            ShellKind::Windows => ShellCommand::new(ShellKind::Windows).verb("Get-Location").build(),
        };
        let invocation =
            CommandInvocation::new(self.shell_kind, command, CommandCategory::PrintDirectory, self.working_dir.clone());
        self.policy.check(&invocation)?;
        Ok(self.working_dir.to_string_lossy().into_owned())
    }

    pub fn mkdir(&self, path: &str, parents: bool) -> Result<()> {
        let target = self.resolve_path(path)?;
        self.ensure_mutation_target_within_workspace(&target)?;

        let command = match self.shell_kind {
            ShellKind::Unix => ShellCommand::new(ShellKind::Unix)
                .verb("mkdir")
                .flag_if(parents, "p")
                .value(format_path(ShellKind::Unix, &target))
                .build(),
            ShellKind::Windows => ShellCommand::new(ShellKind::Windows)
                .verb("New-Item")
                .flag("ItemType")
                .value("Directory")
                .flag_if(parents, "Force")
                .named("Path", format_path(ShellKind::Windows, &target))
                .build(),
        };

        let invocation = CommandInvocation::new(
            self.shell_kind,
            command,
            CommandCategory::CreateDirectory,
            self.working_dir.clone(),
        )
        .with_paths(vec![target]);

        self.expect_success(invocation).map(|_| ())
    }

    pub fn rm(&self, path: &str, recursive: bool, force: bool) -> Result<()> {
        let target = self.resolve_path(path)?;
        // rm replaces its target itself, so the target must not be (or
        // resolve through a symlink to) the workspace root. The canonical
        // comparison closes the traversal (`rm ..` from a subdirectory),
        // alias (`/tmp` vs `/private/tmp`) and symlink-to-root cases.
        let target_canonical = if target.exists() {
            Some(self.resolve_canonical_path(&target)?)
        } else {
            None
        };
        if let Some(canonical) = &target_canonical {
            self.ensure_not_workspace_root(canonical)?;
        }
        self.ensure_mutation_target_within_workspace(&target)?;

        let command = match self.shell_kind {
            ShellKind::Unix => ShellCommand::new(ShellKind::Unix)
                .verb("rm")
                .flag_if(recursive, "r")
                .flag_if(force, "f")
                .value(format_path(ShellKind::Unix, &target))
                .build(),
            ShellKind::Windows => ShellCommand::new(ShellKind::Windows)
                .verb("Remove-Item")
                .flag_if(recursive, "Recurse")
                .flag_if(force, "Force")
                .named("Path", format_path(ShellKind::Windows, &target))
                .build(),
        };

        let invocation =
            CommandInvocation::new(self.shell_kind, command, CommandCategory::Remove, self.working_dir.clone())
                .with_paths(vec![target]);

        self.expect_success(invocation).map(|_| ())
    }

    pub fn cp(&self, source: &str, dest: &str, recursive: bool) -> Result<()> {
        let source_path = self.resolve_existing_path(source)?;
        let dest_path = self.resolve_path(dest)?;
        self.ensure_mutation_target_within_workspace(&dest_path)?;

        let command = match self.shell_kind {
            ShellKind::Unix => ShellCommand::new(ShellKind::Unix)
                .verb("cp")
                .flag_if(recursive, "r")
                .value(format_path(ShellKind::Unix, &source_path))
                .value(format_path(ShellKind::Unix, &dest_path))
                .build(),
            ShellKind::Windows => ShellCommand::new(ShellKind::Windows)
                .verb("Copy-Item")
                .named("Path", format_path(ShellKind::Windows, &source_path))
                .named("Destination", format_path(ShellKind::Windows, &dest_path))
                .flag_if(recursive, "Recurse")
                .build(),
        };

        let invocation =
            CommandInvocation::new(self.shell_kind, command, CommandCategory::Copy, self.working_dir.clone())
                .with_paths(vec![source_path, dest_path]);

        self.expect_success(invocation).map(|_| ())
    }

    pub fn mv(&self, source: &str, dest: &str) -> Result<()> {
        let source_path = self.resolve_existing_path(source)?;
        let dest_path = self.resolve_path(dest)?;
        self.ensure_mutation_target_within_workspace(&dest_path)?;

        let command = match self.shell_kind {
            ShellKind::Unix => ShellCommand::new(ShellKind::Unix)
                .verb("mv")
                .value(format_path(ShellKind::Unix, &source_path))
                .value(format_path(ShellKind::Unix, &dest_path))
                .build(),
            ShellKind::Windows => ShellCommand::new(ShellKind::Windows)
                .verb("Move-Item")
                .named("Path", format_path(ShellKind::Windows, &source_path))
                .named("Destination", format_path(ShellKind::Windows, &dest_path))
                .build(),
        };

        let invocation =
            CommandInvocation::new(self.shell_kind, command, CommandCategory::Move, self.working_dir.clone())
                .with_paths(vec![source_path, dest_path]);

        self.expect_success(invocation).map(|_| ())
    }

    pub fn grep(&self, pattern: &str, path: Option<&str>, recursive: bool) -> Result<String> {
        let target = path
            .map(|p| self.resolve_existing_path(p))
            .transpose()?
            .unwrap_or_else(|| self.working_dir.clone());

        let command = match self.shell_kind {
            ShellKind::Unix => ShellCommand::new(ShellKind::Unix)
                .verb("grep")
                .flag("n")
                .flag_if(recursive, "r")
                .value(format_pattern(ShellKind::Unix, pattern))
                .value(format_path(ShellKind::Unix, &target))
                .build(),
            ShellKind::Windows => ShellCommand::new(ShellKind::Windows)
                .verb("Select-String")
                .named("Pattern", format_pattern(ShellKind::Windows, pattern))
                .named("Path", format_path(ShellKind::Windows, &target))
                .value("-SimpleMatch")
                .flag_if(recursive, "Recurse")
                .build(),
        };

        let invocation =
            CommandInvocation::new(self.shell_kind, command, CommandCategory::Search, self.working_dir.clone())
                .with_paths(vec![target]);

        let output = self.execute_invocation(invocation)?;
        if output.status.success() {
            return Ok(output.stdout);
        }

        if output.stdout.trim().is_empty() && output.stderr.trim().is_empty() {
            Ok(String::new())
        } else {
            Err(anyhow!(
                "search command failed: {}",
                if output.stderr.trim().is_empty() {
                    output.stdout
                } else {
                    output.stderr
                }
            ))
        }
    }

    fn execute_invocation(&self, invocation: CommandInvocation) -> Result<CommandOutput> {
        self.policy.check(&invocation)?;
        self.executor.execute(&invocation)
    }

    fn expect_success(&self, invocation: CommandInvocation) -> Result<CommandOutput> {
        let output = self.execute_invocation(invocation.clone())?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(anyhow!(
                "command `{}` failed: {}",
                invocation.command,
                if output.stderr.trim().is_empty() {
                    output.stdout
                } else {
                    output.stderr
                }
            ))
        }
    }

    fn resolve_existing_path(&self, raw: &str) -> Result<PathBuf> {
        let path = self.resolve_path(raw)?;
        if !path.exists() {
            bail!("path `{}` does not exist", path.display());
        }

        let canonical = self.resolve_canonical_path(&path)?;

        self.ensure_within_workspace(&canonical)?;
        Ok(canonical)
    }

    fn resolve_path(&self, raw: &str) -> Result<PathBuf> {
        // An empty/whitespace path joins to the working directory itself
        // (`PathBuf::join("")` returns the base), turning `rm -r -f ""` into
        // a recursive delete of the workspace root. Reject it at the entry
        // point shared by cd/mkdir/rm/cp/mv.
        if raw.trim().is_empty() {
            bail!("path must not be empty");
        }
        let candidate = Path::new(raw);
        let joined = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            self.working_dir.join(candidate)
        };
        Ok(joined.clean())
    }

    fn ensure_mutation_target_within_workspace(&self, candidate: &Path) -> Result<()> {
        if let Ok(metadata) = fs::symlink_metadata(candidate)
            && metadata.file_type().is_symlink()
        {
            let canonical = self.resolve_canonical_path(candidate)?;
            return self.ensure_within_workspace(&canonical);
        }

        if candidate.exists() {
            let canonical = self.resolve_canonical_path(candidate)?;
            self.ensure_within_workspace(&canonical)
        } else {
            let parent = self.canonicalize_existing_parent(candidate)?;
            self.ensure_within_workspace(&parent)
        }
    }

    /// Refuse to operate on the workspace root itself. A recursive delete of
    /// the root erases the entire workspace; the root can be reached
    /// lexically (`rm -r .` from the root, `rm -r ..` from a subdirectory)
    /// or through a symlink/absolute alias, so the check compares the
    /// canonicalized target against the canonical workspace root.
    fn ensure_not_workspace_root(&self, canonical_candidate: &Path) -> Result<()> {
        if canonical_candidate == self.workspace_root {
            bail!("refusing to operate on the workspace root itself (`{}`)", canonical_candidate.display());
        }
        Ok(())
    }

    fn canonicalize_existing_parent(&self, candidate: &Path) -> Result<PathBuf> {
        let mut current = candidate.parent();
        while let Some(path) = current {
            if path.exists() {
                return self.resolve_canonical_path(path);
            }
            current = path.parent();
        }

        Ok(self.working_dir.clone())
    }

    fn ensure_within_workspace(&self, candidate: &Path) -> Result<()> {
        // `workspace_root` is canonicalized in the constructor and candidates
        // arrive canonicalized, so the lexical check is sufficient here.
        vtcode_commons::paths::ensure_path_within_workspace(candidate, &self.workspace_root).map_err(|error| {
            error.context(format!(
                "path `{}` escapes workspace root `{}`",
                candidate.display(),
                self.workspace_root.display()
            ))
        })?;
        Ok(())
    }
}

fn default_shell_kind() -> ShellKind {
    if cfg!(windows) {
        ShellKind::Windows
    } else {
        ShellKind::Unix
    }
}

fn join_command(parts: Vec<String>) -> String {
    parts.into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" ")
}

fn format_path(shell: ShellKind, path: &Path) -> String {
    match shell {
        ShellKind::Unix => escape(path.to_string_lossy()).into_owned(),
        ShellKind::Windows => format!("'{}'", path.to_string_lossy().replace('\'', "''")),
    }
}

fn format_pattern(shell: ShellKind, pattern: &str) -> String {
    match shell {
        ShellKind::Unix => escape(pattern.into()).into_owned(),
        ShellKind::Windows => format!("'{}'", pattern.replace('\'', "''")),
    }
}

/// Fluent builder for shell-aware command strings.
///
/// `ShellKind::Unix` follows POSIX conventions (flags prefixed with `-`,
/// arguments are positional). `ShellKind::Windows` targets PowerShell,
/// which uses named switches in the form `-Name value`.
struct ShellCommand {
    shell: ShellKind,
    parts: Vec<String>,
}

impl ShellCommand {
    fn new(shell: ShellKind) -> Self {
        Self { shell, parts: Vec::new() }
    }

    /// Append the command verb (first token).
    fn verb(mut self, name: &str) -> Self {
        self.parts.push(name.to_string());
        self
    }

    /// Append a `-Name` flag unconditionally.
    fn flag(mut self, name: &str) -> Self {
        self.parts.push(format!("-{name}"));
        self
    }

    /// Append a `-Name` flag only if `condition` holds.
    fn flag_if(mut self, condition: bool, name: &str) -> Self {
        if condition {
            self.parts.push(format!("-{name}"));
        }
        self
    }

    /// Append a named parameter with a value. On Unix, the `name` is ignored
    /// and the value is added as a positional argument. On Windows, the
    /// pair is rendered as `-Name value`.
    fn named(mut self, name: &str, value: impl Into<String>) -> Self {
        let v = value.into();
        let token = match self.shell {
            ShellKind::Unix => v,
            ShellKind::Windows => format!("-{name} {v}"),
        };
        self.parts.push(token);
        self
    }

    /// Append a positional value rendered the same way on both shells.
    fn value(mut self, value: impl Into<String>) -> Self {
        self.parts.push(value.into());
        self
    }

    fn build(self) -> String {
        join_command(self.parts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{CommandInvocation, CommandOutput, CommandStatus};
    use crate::policy::AllowAllPolicy;
    use assert_fs::TempDir;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct RecordingExecutor {
        invocations: Arc<Mutex<Vec<CommandInvocation>>>,
    }

    impl CommandExecutor for RecordingExecutor {
        fn execute(&self, invocation: &CommandInvocation) -> Result<CommandOutput> {
            self.invocations
                .lock()
                .map_err(|e| anyhow!("executor lock poisoned: {e}"))?
                .push(invocation.clone());
            Ok(CommandOutput {
                status: CommandStatus::new(true, Some(0)),
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn cd_updates_working_directory() -> Result<()> {
        let dir = TempDir::new()?;
        let nested = dir.path().join("nested");
        fs::create_dir(&nested)?;
        let runner = BashRunner::new(dir.path().to_path_buf(), RecordingExecutor::default(), AllowAllPolicy);
        let mut runner = runner?;
        runner.cd("nested")?;
        // Canonicalize expected path to match runner's canonical working_dir
        let expected = canonicalize(&nested)?;
        assert_eq!(runner.working_dir(), expected);
        Ok(())
    }

    #[test]
    fn rm_rejects_empty_path_instead_of_targeting_workspace_root() -> Result<()> {
        let dir = TempDir::new()?;
        let executor = RecordingExecutor::default();
        let runner = BashRunner::new(dir.path().to_path_buf(), executor.clone(), AllowAllPolicy)?;

        for empty in ["", "   ", "."] {
            let result = runner.rm(empty, true, true);
            assert!(result.is_err(), "rm({empty:?}) must be rejected");
        }
        assert!(
            executor.invocations.lock().expect("invocations lock").is_empty(),
            "no command must be built for empty paths"
        );
        Ok(())
    }

    #[test]
    fn rm_rejects_workspace_root_via_parent_traversal_and_absolute_alias() -> Result<()> {
        let dir = TempDir::new()?;
        let executor = RecordingExecutor::default();
        let runner = BashRunner::new(dir.path().to_path_buf(), executor.clone(), AllowAllPolicy)?;
        let mut runner = runner;
        let canonical_root = runner.working_dir().to_path_buf();

        // rm .. from a subdirectory resolves to the workspace root.
        fs::create_dir_all(runner.working_dir().join("sub"))?;
        runner.cd("sub")?;
        assert!(runner.rm("..", true, true).is_err(), "rm('..') from sub must be rejected");
        // Absolute path written through the non-canonical alias of the root.
        let alias = dir.path().to_path_buf();
        if alias != canonical_root {
            assert!(
                runner.rm(&alias.to_string_lossy(), true, true).is_err(),
                "rm via non-canonical absolute alias must be rejected"
            );
        }
        // The canonical root itself (rm is run from a subdirectory on purpose).
        assert!(
            runner.rm(&canonical_root.to_string_lossy(), true, true).is_err(),
            "rm on the canonical root must be rejected"
        );
        // A symlink to the workspace root placed inside it.
        let link = runner.working_dir().join("root-link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&canonical_root, &link).expect("create root symlink");
        #[cfg(unix)]
        assert!(
            runner.rm(&link.to_string_lossy(), true, true).is_err(),
            "rm through a symlink to the root must be rejected"
        );
        #[cfg(not(unix))]
        let _ = &link;
        assert!(
            executor.invocations.lock().expect("invocations lock").is_empty(),
            "no command must be built for root targets"
        );
        Ok(())
    }

    #[test]
    fn mkdir_records_invocation() -> Result<()> {
        let dir = TempDir::new()?;
        let executor = RecordingExecutor::default();
        let runner = BashRunner::new(dir.path().to_path_buf(), executor.clone(), AllowAllPolicy);
        runner?.mkdir("new_dir", true)?;
        let invocations = executor
            .invocations
            .lock()
            .map_err(|e| anyhow!("executor lock poisoned: {e}"))?;
        assert_eq!(invocations.len(), 1);
        assert_eq!(invocations[0].category, CommandCategory::CreateDirectory);
        Ok(())
    }
    #[cfg(unix)]
    #[test]
    fn symlink_retargeting_cannot_reuse_an_earlier_authorization() -> Result<()> {
        let root = TempDir::new()?;
        let outside = TempDir::new()?;
        let inside = root.path().join("inside");
        fs::create_dir(&inside)?;
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&inside, &link)?;
        let executor = RecordingExecutor::default();
        let runner = BashRunner::new(root.path().to_path_buf(), executor.clone(), AllowAllPolicy)?;
        runner.ls(Some("link"), false)?;
        fs::remove_file(&link)?;
        std::os::unix::fs::symlink(outside.path(), &link)?;
        assert!(runner.ls(Some("link"), false).is_err());
        assert!(runner.mkdir("link/new-directory", false).is_err());
        assert_eq!(executor.invocations.lock().expect("invocations lock").len(), 1);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn shell_command_keeps_metacharacters_inside_a_single_literal_argument() -> Result<()> {
        let root = TempDir::new()?;
        let executor = RecordingExecutor::default();
        let runner = BashRunner::new(root.path().to_path_buf(), executor.clone(), AllowAllPolicy)?;
        let filename = "literal;$(echo injected)'file";
        runner.mkdir(filename, false)?;
        let invocations = executor.invocations.lock().expect("invocations lock");
        let command = &invocations[0].command;
        let output = std::process::Command::new("sh").arg("-c").arg(command).output()?;
        assert!(output.status.success());
        assert!(runner.workspace_root().join(filename).is_dir());
        Ok(())
    }
}
