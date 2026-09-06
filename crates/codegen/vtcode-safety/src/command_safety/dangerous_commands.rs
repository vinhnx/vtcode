#![expect(
    clippy::indexing_slicing,
    reason = "Dangerous-command matching uses validated token lengths and fixed ASCII option prefixes."
)]

//! Detection of dangerous commands that should never be executed.
//!
//! This module implements hardcoded detection for commands that are inherently
//! destructive or dangerous, regardless of their options.
//!
//! Examples:
//! - `rm -rf /` (destructive)
//! - `git reset --hard` (destructive)
//! - `dd if=/dev/zero of=/dev/sda` (very destructive)
//! - `sudo rm` (privilege escalation + destruction)

/// Checks if a command appears dangerous to execute.
/// Returns true if the command should be blocked before execution.
pub fn command_might_be_dangerous(command: &[String]) -> bool {
    // PowerShell's encoded-command form hides the script from every
    // platform-neutral parser. Treat it as dangerous before policy or shell
    // evaluation can classify the base64 payload as an ordinary argument.
    if is_encoded_powershell_invocation(command) {
        return true;
    }

    #[cfg(windows)]
    {
        if crate::command_safety::windows::is_dangerous_command_windows(command) {
            return true;
        }
    }

    if is_dangerous_to_call_with_exec(command) {
        return true;
    }

    // Support bash -lc "..." parsing for chained commands
    // If the command is bash -c "..." or similar, parse the script and check each command
    if command.len() >= 3
        && (command[0] == "bash" || command[0] == "sh" || command[0] == "zsh")
        && (command[1] == "-c" || command[1] == "-lc" || command[1] == "-ilc")
    {
        let script = &command[2];
        if let Ok(sub_commands) = crate::command_safety::shell_parser::parse_shell_commands(script) {
            for sub_cmd in sub_commands {
                if command_might_be_dangerous(&sub_cmd) {
                    return true;
                }
            }
        }
    }

    false
}

fn is_encoded_powershell_invocation(command: &[String]) -> bool {
    let Some(executable) = command.first() else {
        return false;
    };
    let executable = extract_command_name(executable).to_ascii_lowercase();
    if !matches!(executable.as_str(), "powershell" | "powershell.exe" | "pwsh" | "pwsh.exe") {
        return false;
    }

    command.iter().skip(1).any(|argument| {
        matches!(argument.to_ascii_lowercase().as_str(), "-encodedcommand" | "-encoded" | "-enc" | "-e")
    })
}

/// Git global options that take a value (skip these and their values when finding subcommand)
fn is_git_global_option_with_value(arg: &str) -> bool {
    matches!(
        arg,
        "-C" | "-c" | "--config-env" | "--exec-path" | "--git-dir" | "--namespace" | "--super-prefix" | "--work-tree"
    )
}

/// Git global options with inline values (e.g., --git-dir=/path)
fn is_git_global_option_with_inline_value(arg: &str) -> bool {
    matches!(
        arg,
        s if s.starts_with("--config-env=")
            || s.starts_with("--exec-path=")
            || s.starts_with("--git-dir=")
            || s.starts_with("--namespace=")
        || s.starts_with("--super-prefix=")
        || s.starts_with("--work-tree=")
    ) || ((arg.starts_with("-C") || arg.starts_with("-c")) && arg.len() > 2)
}

/// Returns whether a git global option can redirect repository, config, or
/// helper lookup and therefore must not be treated as an inspection flag.
pub fn git_global_option_requires_prompt(arg: &str) -> bool {
    matches!(
        arg,
        "-C" | "-c" | "--config-env" | "--exec-path" | "--git-dir" | "--namespace" | "--super-prefix" | "--work-tree"
    ) || matches!(
        arg,
        s if (s.starts_with("-C") && s.len() > 2)
            || (s.starts_with("-c") && s.len() > 2)
            || s.starts_with("--config-env=")
            || s.starts_with("--exec-path=")
            || s.starts_with("--git-dir=")
            || s.starts_with("--namespace=")
            || s.starts_with("--super-prefix=")
            || s.starts_with("--work-tree=")
    )
}

/// Find the first matching git subcommand, skipping known global options that
/// may appear before it (e.g., `-C`, `-c`, `--git-dir`).
///
/// Shared with `is_safe_command` to avoid git-global-option bypasses.
pub(crate) fn find_git_subcommand<'a>(command: &'a [String], subcommands: &[&str]) -> Option<(usize, &'a str)> {
    let cmd0 = command.first().map(String::as_str)?;
    if !cmd0.ends_with("git") {
        return None;
    }

    let mut skip_next = false;
    for (idx, arg) in command.iter().enumerate().skip(1) {
        if skip_next {
            skip_next = false;
            continue;
        }

        let arg = arg.as_str();

        if is_git_global_option_with_inline_value(arg) {
            continue;
        }

        if is_git_global_option_with_value(arg) {
            skip_next = true;
            continue;
        }

        if arg == "--" || arg.starts_with('-') {
            continue;
        }

        if subcommands.contains(&arg) {
            return Some((idx, arg));
        }

        // In git, the first non-option token is the subcommand. If it isn't
        // one of the subcommands we're looking for, we must stop scanning to
        // avoid misclassifying later positional args (e.g., branch names).
        return None;
    }

    None
}

/// Check if a short flag group contains a specific character (e.g., -fdx contains 'f')
fn short_flag_group_contains(arg: &str, target: char) -> bool {
    arg.starts_with('-') && !arg.starts_with("--") && arg.chars().skip(1).any(|c| c == target)
}

/// Check if git branch command is a delete operation
fn git_branch_is_delete(branch_args: &[String]) -> bool {
    // Git allows stacking short flags (for example, `-dv` or `-vd`). Treat any
    // short-flag group containing `d`/`D` as a delete flag.
    branch_args.iter().map(String::as_str).any(|arg| {
        matches!(arg, "-d" | "-D" | "--delete")
            || arg.starts_with("--delete=")
            || short_flag_group_contains(arg, 'd')
            || short_flag_group_contains(arg, 'D')
    })
}

/// Check if git push command is dangerous (force, delete, or dangerous refspec)
fn git_push_is_dangerous(push_args: &[String]) -> bool {
    push_args.iter().map(String::as_str).any(|arg| {
        matches!(arg, "--force" | "--force-with-lease" | "--force-if-includes" | "--delete" | "-f" | "-d")
            || arg.starts_with("--force-with-lease=")
            || arg.starts_with("--force-if-includes=")
            || arg.starts_with("--delete=")
            || short_flag_group_contains(arg, 'f')
            || short_flag_group_contains(arg, 'd')
            || git_push_refspec_is_dangerous(arg)
    })
}

/// Check if a refspec is dangerous (+refspec forces updates, :refspec deletes)
fn git_push_refspec_is_dangerous(arg: &str) -> bool {
    // `+<refspec>` forces updates and `:<dst>` deletes remote refs.
    (arg.starts_with('+') || arg.starts_with(':')) && arg.len() > 1
}

/// Check if git clean command uses force flag
fn git_clean_is_force(clean_args: &[String]) -> bool {
    clean_args.iter().map(String::as_str).any(|arg| {
        matches!(arg, "--force" | "-f") || arg.starts_with("--force=") || short_flag_group_contains(arg, 'f')
    })
}

/// Check if a command is a dangerous git subcommand (without the "git" prefix)
/// This handles commands parsed from shell scripts where the binary name may be omitted
fn is_dangerous_git_subcommand(command: &[String]) -> bool {
    if command.is_empty() {
        return false;
    }

    let first_arg = command[0].as_str();

    // Check if first arg is a git subcommand
    match first_arg {
        "reset" | "rm" => true,
        "branch" => git_branch_is_delete(&command[1..]),
        "push" => git_push_is_dangerous(&command[1..]),
        "clean" => git_clean_is_force(&command[1..]),
        // Handle global options that appear before subcommand (e.g., -C, -c)
        // These would be from shell parser extracting partial commands
        opt if opt.starts_with('-') => {
            // Try to find the subcommand after global options
            if let Some((idx, subcommand)) =
                find_git_subcommand_from_args(command, &["reset", "rm", "branch", "push", "clean"])
            {
                match subcommand {
                    "reset" | "rm" => true,
                    "branch" => git_branch_is_delete(&command[idx + 1..]),
                    "push" => git_push_is_dangerous(&command[idx + 1..]),
                    "clean" => git_clean_is_force(&command[idx + 1..]),
                    _ => false,
                }
            } else {
                false
            }
        }
        _ => false,
    }
}

/// Find git subcommand from a list of args (without the "git" binary name)
fn find_git_subcommand_from_args<'a>(args: &'a [String], subcommands: &[&str]) -> Option<(usize, &'a str)> {
    let mut skip_next = false;
    for (idx, arg) in args.iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }

        let arg = arg.as_str();

        if is_git_global_option_with_inline_value(arg) {
            continue;
        }

        if is_git_global_option_with_value(arg) {
            skip_next = true;
            continue;
        }

        if arg == "--" || arg.starts_with('-') {
            continue;
        }

        if subcommands.contains(&arg) {
            return Some((idx, arg));
        }

        // First non-option token that isn't a subcommand we're looking for
        return None;
    }

    None
}

/// Core dangerous command detection for Unix/Linux/macOS
fn is_dangerous_to_call_with_exec(command: &[String]) -> bool {
    if command.is_empty() {
        return false;
    }

    let cmd0 = command.first().map(String::as_str);
    let base_cmd = extract_command_name(cmd0.unwrap_or(""));

    match base_cmd {
        // ──── Git ────
        "git" => {
            let Some((subcommand_idx, subcommand)) =
                find_git_subcommand(command, &["reset", "rm", "branch", "push", "clean"])
            else {
                return false;
            };

            match subcommand {
                "reset" | "rm" => true,
                "branch" => git_branch_is_delete(&command[subcommand_idx + 1..]),
                "push" => git_push_is_dangerous(&command[subcommand_idx + 1..]),
                "clean" => git_clean_is_force(&command[subcommand_idx + 1..]),
                other => {
                    debug_assert!(false, "unexpected git subcommand from matcher: {other}");
                    false
                }
            }
        }

        // ──── Rm ────
        "rm" => matches!(command.get(1).map(String::as_str), Some("-f" | "-rf" | "-fr" | "-r")),

        // ──── Destructive system commands ────
        _ if base_cmd == "mkfs" || base_cmd.starts_with("mkfs.") => true,
        "dd" | "shutdown" | "reboot" | "init" => true,

        // ──── Fork bomb ────
        _ if base_cmd.ends_with(':') && command.len() >= 2 => command[1] == "(){:|:&};:",

        // ──── Sudo: check the wrapped command ────
        "sudo" => {
            if command.len() > 1 {
                is_dangerous_to_call_with_exec(&command[1..])
            } else {
                false
            }
        }

        // ──── Git subcommands without "git" prefix (from shell parsing) ────
        _ => is_dangerous_git_subcommand(command),
    }
}

/// Extract base command name from full path
fn extract_command_name(cmd: &str) -> &str {
    std::path::Path::new(cmd)
        .file_name()
        .and_then(|osstr| osstr.to_str())
        .unwrap_or(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vec_str(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn git_reset_is_dangerous() {
        let cmd = vec!["git".to_string(), "reset".to_string()];
        assert!(is_dangerous_to_call_with_exec(&cmd));
    }

    #[test]
    fn git_reset_hard_is_dangerous() {
        let cmd = vec!["git".to_string(), "reset".to_string(), "--hard".to_string()];
        assert!(is_dangerous_to_call_with_exec(&cmd));
    }

    #[test]
    fn git_status_is_safe() {
        let cmd = vec!["git".to_string(), "status".to_string()];
        assert!(!is_dangerous_to_call_with_exec(&cmd));
    }

    #[test]
    fn git_log_is_safe() {
        let cmd = vec!["git".to_string(), "log".to_string()];
        assert!(!is_dangerous_to_call_with_exec(&cmd));
    }

    #[test]
    fn rm_f_is_dangerous() {
        let cmd = vec!["rm".to_string(), "-f".to_string(), "file.txt".to_string()];
        assert!(is_dangerous_to_call_with_exec(&cmd));
    }

    #[test]
    fn rm_rf_is_dangerous() {
        let cmd = vec!["rm".to_string(), "-rf".to_string(), "/".to_string()];
        assert!(is_dangerous_to_call_with_exec(&cmd));
    }

    #[test]
    fn rm_without_flags_is_safe() {
        let cmd = vec!["rm".to_string()];
        assert!(!is_dangerous_to_call_with_exec(&cmd));
    }

    #[test]
    fn mkfs_is_dangerous() {
        let cmd = vec!["mkfs".to_string()];
        assert!(is_dangerous_to_call_with_exec(&cmd));
    }

    #[test]
    fn mkfs_variants_are_dangerous() {
        let cmd = vec!["mkfs.ext4".to_string(), "/dev/sda1".to_string()];
        assert!(is_dangerous_to_call_with_exec(&cmd));
    }

    #[test]
    fn dd_is_dangerous() {
        let cmd = vec!["dd".to_string(), "if=/dev/zero".to_string()];
        assert!(is_dangerous_to_call_with_exec(&cmd));
    }

    #[test]
    fn shutdown_is_dangerous() {
        let cmd = vec!["shutdown".to_string()];
        assert!(is_dangerous_to_call_with_exec(&cmd));
    }

    #[test]
    fn sudo_git_reset_is_dangerous() {
        let cmd = vec![
            "sudo".to_string(),
            "git".to_string(),
            "reset".to_string(),
            "--hard".to_string(),
        ];
        assert!(is_dangerous_to_call_with_exec(&cmd));
    }

    #[test]
    fn sudo_git_status_is_safe() {
        let cmd = vec!["sudo".to_string(), "git".to_string(), "status".to_string()];
        assert!(!is_dangerous_to_call_with_exec(&cmd));
    }

    #[test]
    fn absolute_path_git_reset_is_dangerous() {
        let cmd = vec!["/usr/bin/git".to_string(), "reset".to_string()];
        assert!(is_dangerous_to_call_with_exec(&cmd));
    }

    #[test]
    fn empty_command_is_safe() {
        let cmd: Vec<String> = vec![];
        assert!(!is_dangerous_to_call_with_exec(&cmd));
    }

    #[test]
    fn command_might_be_dangerous_detects_git_reset() {
        let cmd = vec!["git".to_string(), "reset".to_string()];
        assert!(command_might_be_dangerous(&cmd));
    }

    #[test]
    fn command_might_be_dangerous_allows_git_status() {
        let cmd = vec!["git".to_string(), "status".to_string()];
        assert!(!command_might_be_dangerous(&cmd));
    }

    // ──── Git Branch Delete Tests ────

    #[test]
    fn git_branch_delete_is_dangerous() {
        assert!(command_might_be_dangerous(&vec_str(&["git", "branch", "-d", "feature",])));
        assert!(command_might_be_dangerous(&vec_str(&["git", "branch", "-D", "feature",])));
        // Test shell script parsing separately
        let script = "git branch --delete feature";
        if let Ok(sub_commands) = crate::command_safety::shell_parser::parse_shell_commands(script) {
            for sub_cmd in sub_commands {
                assert!(command_might_be_dangerous(&sub_cmd), "sub-command should be dangerous: {sub_cmd:?}");
            }
        }
    }

    #[test]
    fn git_branch_delete_with_stacked_short_flags_is_dangerous() {
        assert!(command_might_be_dangerous(&vec_str(&["git", "branch", "-dv", "feature",])));
        assert!(command_might_be_dangerous(&vec_str(&["git", "branch", "-vd", "feature",])));
        assert!(command_might_be_dangerous(&vec_str(&["git", "branch", "-vD", "feature",])));
        assert!(command_might_be_dangerous(&vec_str(&["git", "branch", "-Dvv", "feature",])));
    }

    #[test]
    fn git_branch_delete_with_global_options_is_dangerous() {
        assert!(command_might_be_dangerous(&vec_str(&["git", "-C", ".", "branch", "-d", "feature",])));
        assert!(command_might_be_dangerous(&vec_str(&["git", "-c", "color.ui=false", "branch", "-D", "feature",])));
        // Test shell script parsing separately
        let script = "git -C . branch -d feature";
        if let Ok(sub_commands) = crate::command_safety::shell_parser::parse_shell_commands(script) {
            for sub_cmd in sub_commands {
                assert!(command_might_be_dangerous(&sub_cmd), "sub-command should be dangerous: {sub_cmd:?}");
            }
        }
    }

    #[test]
    fn git_checkout_reset_is_not_dangerous() {
        // The first non-option token is "checkout", so later positional args
        // like branch names must not be treated as subcommands.
        assert!(!command_might_be_dangerous(&vec_str(&["git", "checkout", "reset",])));
    }

    // ──── Git Push Dangerous Tests ────

    #[test]
    fn git_push_force_is_dangerous() {
        assert!(command_might_be_dangerous(&vec_str(&["git", "push", "--force", "origin", "main",])));
        assert!(command_might_be_dangerous(&vec_str(&["git", "push", "-f", "origin", "main",])));
        assert!(command_might_be_dangerous(&vec_str(&[
            "git",
            "-C",
            ".",
            "push",
            "--force-with-lease",
            "origin",
            "main",
        ])));
    }

    #[test]
    fn git_push_plus_refspec_is_dangerous() {
        assert!(command_might_be_dangerous(&vec_str(&["git", "push", "origin", "+main",])));
        assert!(command_might_be_dangerous(&vec_str(
            &["git", "push", "origin", "+refs/heads/main:refs/heads/main",]
        )));
    }

    #[test]
    fn git_push_delete_flag_is_dangerous() {
        assert!(command_might_be_dangerous(&vec_str(&["git", "push", "--delete", "origin", "feature",])));
        assert!(command_might_be_dangerous(&vec_str(&["git", "push", "-d", "origin", "feature",])));
    }

    #[test]
    fn git_push_delete_refspec_is_dangerous() {
        assert!(command_might_be_dangerous(&vec_str(&["git", "push", "origin", ":feature",])));
        // Test shell script parsing separately
        let script = "git push origin :feature";
        if let Ok(sub_commands) = crate::command_safety::shell_parser::parse_shell_commands(script) {
            for sub_cmd in sub_commands {
                assert!(command_might_be_dangerous(&sub_cmd), "sub-command should be dangerous: {sub_cmd:?}");
            }
        }
    }

    #[test]
    fn git_push_without_force_is_not_dangerous() {
        assert!(!command_might_be_dangerous(&vec_str(&["git", "push", "origin", "main",])));
    }

    // ──── Git Clean Tests ────

    #[test]
    fn git_clean_force_is_dangerous_even_when_f_is_not_first_flag() {
        assert!(command_might_be_dangerous(&vec_str(&["git", "clean", "-fdx",])));
        assert!(command_might_be_dangerous(&vec_str(&["git", "clean", "-xdf",])));
        assert!(command_might_be_dangerous(&vec_str(&["git", "clean", "--force",])));
    }
}
