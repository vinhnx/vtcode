//! Child process spawning with sandbox-aware environment handling.
//!
//! Implements patterns from the Codex sandbox model:
//! - Environment variable sanitization (remove sensitive vars)
//! - Parent death signal (PR_SET_PDEATHSIG on Linux)
//! - Sandbox identification markers for downstream tools

use hashbrown::HashMap;
use std::path::Path;

/// Environment variables that should be filtered from sandboxed processes.
///
/// Following the field guide: "Completely clear the environment and rebuild it
/// with only the variables you actually want."
pub const FILTERED_ENV_VARS: &[&str] = &[
    // API keys and tokens
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "GEMINI_API_KEY",
    "XAI_API_KEY",
    "DEEPSEEK_API_KEY",
    "META_API_KEY",
    "MODEL_API_KEY",
    "OPENROUTER_API_KEY",
    "GROQ_API_KEY",
    "MISTRAL_API_KEY",
    "COHERE_API_KEY",
    "AZURE_OPENAI_API_KEY",
    "HUGGINGFACE_API_KEY",
    "HF_TOKEN",
    // Cloud provider credentials
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "GOOGLE_CLOUD_PROJECT",
    "AZURE_CLIENT_ID",
    "AZURE_CLIENT_SECRET",
    "AZURE_TENANT_ID",
    "AZURE_SUBSCRIPTION_ID",
    // GitHub tokens
    "GITHUB_TOKEN",
    "GH_TOKEN",
    "GITHUB_PAT",
    // NPM/Package registry tokens
    "NPM_TOKEN",
    "NPM_AUTH_TOKEN",
    "CARGO_REGISTRY_TOKEN",
    "PYPI_TOKEN",
    // Database credentials
    "DATABASE_URL",
    "DB_PASSWORD",
    "PGPASSWORD",
    "MYSQL_PWD",
    "REDIS_PASSWORD",
    "MONGO_PASSWORD",
    // SSH/GPG
    "SSH_AUTH_SOCK",
    "GPG_AGENT_INFO",
    // Dynamic linker vars (security risk)
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "LD_AUDIT",
    "LD_DEBUG",
    "LD_PROFILE",
    "DYLD_INSERT_LIBRARIES",
    "DYLD_LIBRARY_PATH",
    "DYLD_FRAMEWORK_PATH",
    "DYLD_FALLBACK_LIBRARY_PATH",
    // Other sensitive vars
    "VAULT_TOKEN",
    "CONSUL_HTTP_TOKEN",
    "DOCKER_AUTH_CONFIG",
    "KUBECONFIG",
    "KUBE_TOKEN",
    "SLACK_TOKEN",
    "SLACK_BOT_TOKEN",
    "DISCORD_TOKEN",
    "TELEGRAM_BOT_TOKEN",
];

/// Environment variables that should always be preserved.
pub const PRESERVED_ENV_VARS: &[&str] = &[
    // Basic shell environment
    "PATH",
    "SystemRoot",
    "SYSTEMROOT",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "USERPROFILE",
    "HOME",
    "USER",
    "SHELL",
    "TERM",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TZ",
    // XDG directories
    "XDG_CONFIG_HOME",
    "XDG_CONFIG_DIRS",
    "XDG_DATA_HOME",
    "XDG_DATA_DIRS",
    "XDG_BIN_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
    "XDG_RUNTIME_DIR",
    // VT Code path overrides
    "VTCODE_CONFIG",
    "VTCODE_CONFIG_PATH",
    "VTCODE_DATA",
    "VTCODE_HOME",
    // External Codex compatibility root
    "CODEX_HOME",
    // Editor preferences (not sensitive)
    "EDITOR",
    "VISUAL",
    "PAGER",
    "GIT_PAGER",
    "LESS",
    "COLUMNS",
    "LINES",
    "WORKSPACE_DIR",
    "CLICOLOR",
    "CLICOLOR_FORCE",
    "LS_COLORS",
    "CARGO_TERM_COLOR",
    // Build tool paths
    "CARGO_HOME",
    "RUSTUP_HOME",
    "GOPATH",
    "GOROOT",
    "JAVA_HOME",
    "PYTHON",
    "PYTHONPATH",
    "NODE_PATH",
    // Terminal capabilities
    "COLORTERM",
    "FORCE_COLOR",
    "NO_COLOR",
    "CLICOLOR",
    "CLICOLOR_FORCE",
    // Temp directories
    "TMPDIR",
    "TEMP",
    "TMP",
];

/// Sandbox environment markers set for child processes.
pub const VTCODE_SANDBOX_ACTIVE: &str = "VTCODE_SANDBOX_ACTIVE";
pub const VTCODE_SANDBOX_NETWORK_DISABLED: &str = "VTCODE_SANDBOX_NETWORK_DISABLED";
pub const VTCODE_SANDBOX_TYPE: &str = "VTCODE_SANDBOX_TYPE";
pub const VTCODE_SANDBOX_WRITABLE_ROOTS: &str = "VTCODE_SANDBOX_WRITABLE_ROOTS";

/// Build a sanitized environment for sandboxed child processes.
///
/// Implements the Codex pattern: "Completely clear the environment and rebuild it
/// with only the variables you actually want."
#[expect(
    unused_results,
    reason = "Environment construction intentionally ignores prior values while writing the sanitized snapshot."
)]
pub fn build_sanitized_env(
    current_env: &HashMap<String, String>,
    sandbox_active: bool,
    network_disabled: bool,
    sandbox_type: &str,
    writable_roots: &[&Path],
) -> HashMap<String, String> {
    let mut sanitized = HashMap::new();

    // Copy only preserved environment variables
    for key in PRESERVED_ENV_VARS {
        let value = current_env.get(*key).or_else(|| {
            cfg!(windows)
                .then(|| current_env.iter().find(|(name, _)| name.eq_ignore_ascii_case(key)))
                .flatten()
                .map(|(_, value)| value)
        });
        if let Some(value) = value {
            sanitized.insert(key.to_string(), value.clone());
        }
    }

    // Add sandbox markers so downstream tools know what's happening
    if sandbox_active {
        sanitized.insert(VTCODE_SANDBOX_ACTIVE.to_string(), "1".to_string());
        sanitized.insert(VTCODE_SANDBOX_TYPE.to_string(), sandbox_type.to_string());

        if network_disabled {
            sanitized.insert(VTCODE_SANDBOX_NETWORK_DISABLED.to_string(), "1".to_string());
        }

        if !writable_roots.is_empty() {
            let roots: Vec<String> = writable_roots.iter().map(|p| p.display().to_string()).collect();
            sanitized.insert(VTCODE_SANDBOX_WRITABLE_ROOTS.to_string(), roots.join(":"));
        }
    }

    sanitized
}

/// Check if an environment variable should be filtered.
pub fn should_filter_env_var(key: &str) -> bool {
    let key = key.to_ascii_uppercase();
    FILTERED_ENV_VARS.contains(&key.as_str())
        || key.starts_with("AWS_")
        || key.starts_with("AZURE_")
        || key.starts_with("GOOGLE_")
        || key.starts_with("GCP_")
        || key.starts_with("CLOUDSDK_")
        || key.starts_with("LD_")
        || key.starts_with("DYLD_")
        || matches!(key.as_str(), "TOKEN" | "SECRET" | "PASSWORD" | "PASS" | "PWD" | "CREDENTIALS")
        || key.ends_with("_TOKEN")
        || key.ends_with("_KEY")
        || key.ends_with("_SECRET")
        || key.ends_with("_PASS")
        || key.ends_with("_PWD")
        || key.ends_with("_PASSWORD")
        || key.ends_with("_CREDENTIALS")
}

/// Filter sensitive environment variables from an existing map.
///
/// Less aggressive than `build_sanitized_env` - preserves most vars but removes known sensitive ones.
pub fn filter_sensitive_env(env: &HashMap<String, String>) -> HashMap<String, String> {
    env.iter()
        .filter(|(k, _)| !should_filter_env_var(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Set up parent death signal on Linux.
///
/// "Ensures sandboxed children die if the main process gets killed -
/// you don't want orphaned processes running around."
///
/// Uses SIGTERM for graceful shutdown. Includes parent PID check to avoid
/// race condition where parent exits between fork and exec.
#[cfg(target_os = "linux")]
pub fn setup_parent_death_signal() -> std::io::Result<()> {
    setup_parent_death_signal_with_check(nix::unistd::getppid())
}

/// Set up parent death signal with explicit parent PID check.
///
/// This variant should be used in pre_exec hooks where the parent PID
/// is captured before spawn to avoid race conditions.
#[cfg(target_os = "linux")]
pub fn setup_parent_death_signal_with_check(expected_parent_pid: nix::unistd::Pid) -> std::io::Result<()> {
    use nix::sys::prctl;
    use nix::sys::signal::{Signal, raise};
    use std::io::Error;

    // Use SIGTERM for graceful shutdown (allows cleanup handlers to run)
    prctl::set_pdeathsig(Some(Signal::SIGTERM))
        .map_err(|e| Error::other(format!("prctl(PR_SET_PDEATHSIG) failed: {e}")))?;

    // Re-check parent PID to catch race condition where parent exited between
    // fork and this prctl call. If parent changed, self-terminate immediately.
    // Signal delivery here is deliberately best-effort: nothing can recover it.
    #[allow(
        clippy::let_underscore_must_use,
        reason = "best-effort self-signal during pdeathsig race"
    )]
    if nix::unistd::getppid() != expected_parent_pid {
        let _ = raise(Signal::SIGTERM);
    }

    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn setup_parent_death_signal() -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_API_KEY_VALUE: &str = "test-openai-key";

    #[test]
    fn test_should_filter_sensitive_vars() {
        assert!(should_filter_env_var("OPENAI_API_KEY"));
        assert!(should_filter_env_var("META_API_KEY"));
        assert!(should_filter_env_var("AWS_SECRET_ACCESS_KEY"));
        assert!(should_filter_env_var("GITHUB_TOKEN"));
        assert!(should_filter_env_var("LD_PRELOAD"));
        assert!(should_filter_env_var("DYLD_INSERT_LIBRARIES"));
        assert!(should_filter_env_var("MY_CUSTOM_TOKEN"));
        assert!(should_filter_env_var("token"));
        assert!(should_filter_env_var("CloudSDK_AUTH_CREDENTIAL_FILE_OVERRIDE"));
        assert!(should_filter_env_var("MY_CUSTOM_PASS"));
        assert!(should_filter_env_var("MYSQL_PWD"));
        assert!(should_filter_env_var("DATABASE_PASSWORD"));

        assert!(!should_filter_env_var("PATH"));
        assert!(!should_filter_env_var("HOME"));
        assert!(!should_filter_env_var("TERM"));
    }

    #[test]
    fn filters_sensitive_names_case_insensitively() {
        assert!(should_filter_env_var("openai_api_key"));
        assert!(should_filter_env_var("Aws_SECRET_ACCESS_KEY"));
        assert!(should_filter_env_var("custom_access_token"));
        assert!(should_filter_env_var("dyld_insert_libraries"));
        assert!(!should_filter_env_var("PROJECT_NAME"));
    }

    #[test]
    fn test_build_sanitized_env() {
        let mut current = HashMap::new();
        drop(current.insert("PATH".to_string(), "/usr/bin".to_string()));
        drop(current.insert("HOME".to_string(), "/home/user".to_string()));
        drop(current.insert("OPENAI_API_KEY".to_string(), TEST_API_KEY_VALUE.to_string()));
        drop(current.insert("RANDOM_VAR".to_string(), "value".to_string()));

        let sanitized = build_sanitized_env(&current, true, true, "MacosSeatbelt", &[]);

        // PATH and HOME should be preserved
        assert_eq!(sanitized.get("PATH"), Some(&"/usr/bin".to_string()));
        assert_eq!(sanitized.get("HOME"), Some(&"/home/user".to_string()));

        // API key should NOT be present (not in preserved list)
        assert!(!sanitized.contains_key("OPENAI_API_KEY"));

        // Random var should NOT be present (not in preserved list)
        assert!(!sanitized.contains_key("RANDOM_VAR"));

        // Sandbox markers should be set
        assert_eq!(sanitized.get(VTCODE_SANDBOX_ACTIVE), Some(&"1".to_string()));
        assert_eq!(sanitized.get(VTCODE_SANDBOX_NETWORK_DISABLED), Some(&"1".to_string()));
        assert_eq!(sanitized.get(VTCODE_SANDBOX_TYPE), Some(&"MacosSeatbelt".to_string()));
    }

    #[test]
    fn preserves_full_xdg_and_vtcode_path_environment_without_secrets() {
        let mut current = HashMap::new();
        for (key, value) in [
            ("XDG_STATE_HOME", "/state"),
            ("XDG_CONFIG_DIRS", "/etc/xdg:/opt/xdg"),
            ("XDG_DATA_DIRS", "/usr/local/share:/usr/share"),
            ("XDG_BIN_HOME", "/home/user/bin"),
            ("VTCODE_CONFIG", "/config"),
            ("VTCODE_CONFIG_PATH", "/config/explicit.toml"),
            ("VTCODE_DATA", "/data"),
            ("VTCODE_HOME", "/legacy"),
            ("CODEX_HOME", "/codex"),
            ("OPENAI_API_KEY", TEST_API_KEY_VALUE),
            ("DATABASE_PASSWORD", "do-not-copy"),
        ] {
            drop(current.insert(key.to_string(), value.to_string()));
        }

        let sanitized = build_sanitized_env(&current, false, false, "test", &[]);

        for (key, value) in [
            ("XDG_STATE_HOME", "/state"),
            ("XDG_CONFIG_DIRS", "/etc/xdg:/opt/xdg"),
            ("XDG_DATA_DIRS", "/usr/local/share:/usr/share"),
            ("XDG_BIN_HOME", "/home/user/bin"),
            ("VTCODE_CONFIG", "/config"),
            ("VTCODE_CONFIG_PATH", "/config/explicit.toml"),
            ("VTCODE_DATA", "/data"),
            ("VTCODE_HOME", "/legacy"),
            ("CODEX_HOME", "/codex"),
        ] {
            assert_eq!(sanitized.get(key), Some(&value.to_string()), "missing {key}");
        }
        assert!(!sanitized.contains_key("OPENAI_API_KEY"));
        assert!(!sanitized.contains_key("DATABASE_PASSWORD"));
    }

    #[test]
    fn test_filter_sensitive_env() {
        let mut env = HashMap::new();
        drop(env.insert("PATH".to_string(), "/usr/bin".to_string()));
        drop(env.insert("OPENAI_API_KEY".to_string(), TEST_API_KEY_VALUE.to_string()));
        drop(env.insert("MY_VAR".to_string(), "value".to_string()));
        drop(env.insert("AWS_ACCESS_KEY_ID".to_string(), "AKIA...".to_string()));
        drop(env.insert("CUSTOM_PASS".to_string(), "let-me-in".to_string()));
        drop(env.insert("SERVICE_PWD".to_string(), "super-secret".to_string()));

        let filtered = filter_sensitive_env(&env);

        assert!(filtered.contains_key("PATH"));
        assert!(filtered.contains_key("MY_VAR"));
        assert!(!filtered.contains_key("OPENAI_API_KEY"));
        assert!(!filtered.contains_key("AWS_ACCESS_KEY_ID"));
        assert!(!filtered.contains_key("CUSTOM_PASS"));
        assert!(!filtered.contains_key("SERVICE_PWD"));
    }

    #[test]
    fn filters_adversarial_credential_and_loader_names() {
        let mut env = HashMap::new();
        for (name, value) in [
            ("MODEL_API_KEY", "model-secret"),
            ("META_API_KEY", "meta-secret"),
            ("INSTRUCTION_SECRET", "prompt-secret"),
            ("AWS_PROFILE", "production"),
            ("LD_AUDIT", "audit.so"),
            ("DYLD_LIBRARY_PATH", "/tmp/injected"),
            ("SAFE_PROJECT_NAME", "vtcode"),
        ] {
            drop(env.insert(name.to_string(), value.to_string()));
        }

        let filtered = filter_sensitive_env(&env);
        let rebuilt = build_sanitized_env(&env, true, false, "test", &[]);

        for sensitive in [
            "MODEL_API_KEY",
            "META_API_KEY",
            "INSTRUCTION_SECRET",
            "AWS_PROFILE",
            "LD_AUDIT",
            "DYLD_LIBRARY_PATH",
        ] {
            assert!(!filtered.contains_key(sensitive), "sensitive variable survived filtering: {sensitive}");
            assert!(!rebuilt.contains_key(sensitive), "sensitive variable survived rebuilding: {sensitive}");
        }
        assert_eq!(filtered.get("SAFE_PROJECT_NAME"), Some(&"vtcode".to_string()));
    }
}
