//! Sandbox manager for transforming commands into sandboxed execution environments.

use std::ffi::OsString;
use std::path::Path;

use super::child_spawn::build_sanitized_env;
use super::exec_env::{CommandSpec, ExecEnv, SandboxType};
#[cfg(target_os = "macos")]
use super::policy::NetworkAllowlistEntry;
use super::policy::SandboxPolicy;

/// Error type for sandbox transformation failures.
#[derive(Debug, thiserror::Error)]
pub enum SandboxTransformError {
    #[error("missing sandbox executable path")]
    MissingSandboxExecutable,

    #[error("sandbox type {0:?} is not available on this platform")]
    UnavailableSandboxType(SandboxType),

    #[error("failed to create sandbox environment: {0}")]
    CreationFailed(String),

    #[error("invalid sandbox policy: {0}")]
    InvalidPolicy(String),
}

/// Manager for sandbox transformation.
///
/// Transforms a `CommandSpec` into an `ExecEnv` by applying the appropriate
/// sandbox wrapper based on the platform and policy.
#[derive(Debug, Default)]
pub struct SandboxManager;

impl SandboxManager {
    /// Create a new sandbox manager.
    pub fn new() -> Self {
        Self
    }

    /// Transform a command specification into a sandboxed execution environment.
    pub fn transform(
        &self,
        spec: CommandSpec,
        policy: &SandboxPolicy,
        sandbox_cwd: &Path,
        sandbox_executable: Option<&Path>,
    ) -> Result<ExecEnv, SandboxTransformError> {
        // Determine the sandbox type based on policy and platform
        let sandbox_type = self.determine_sandbox_type(policy)?;

        // A restrictive sandbox must not inherit secrets or dynamic-loader
        // controls, including values supplied by a caller through `spec.env`.
        // Full-access and externally managed policies intentionally preserve
        // the caller's environment because this manager is not their boundary.
        let spec = if sandbox_type == SandboxType::None {
            spec
        } else {
            let mut spec = spec;
            spec.env = build_sanitized_env(&spec.env, false, false, "", &[]);
            spec
        };

        // If no sandbox needed or full access, return direct execution
        if sandbox_type == SandboxType::None {
            return Ok(ExecEnv {
                program: spec.program.into(),
                args: spec.args,
                cwd: spec.cwd,
                env: spec.env,
                expiration: spec.expiration,
                sandbox_active: false,
                sandbox_type: SandboxType::None,
            });
        }

        // Check sandbox availability
        if !sandbox_type.is_available() {
            return Err(SandboxTransformError::UnavailableSandboxType(sandbox_type));
        }

        // Transform based on sandbox type
        match sandbox_type {
            SandboxType::MacosSeatbelt => self.transform_seatbelt(spec, policy, sandbox_cwd),
            SandboxType::LinuxLandlock => self.transform_landlock(spec, policy, sandbox_cwd, sandbox_executable),
            SandboxType::WindowsRestrictedToken => self.transform_windows(spec, policy, sandbox_cwd),
            SandboxType::None => {
                Err(SandboxTransformError::InvalidPolicy("Cannot transform with SandboxType::None".into()))
            }
        }
    }

    /// Determine the appropriate sandbox type for the given policy.
    fn determine_sandbox_type(&self, policy: &SandboxPolicy) -> Result<SandboxType, SandboxTransformError> {
        match policy {
            SandboxPolicy::DangerFullAccess | SandboxPolicy::ExternalSandbox { .. } => Ok(SandboxType::None),
            SandboxPolicy::ReadOnly { .. } | SandboxPolicy::WorkspaceWrite { .. } => {
                Ok(SandboxType::platform_default())
            }
        }
    }

    /// Transform for macOS Seatbelt sandbox.
    #[cfg(target_os = "macos")]
    fn transform_seatbelt(
        &self,
        spec: CommandSpec,
        policy: &SandboxPolicy,
        sandbox_cwd: &Path,
    ) -> Result<ExecEnv, SandboxTransformError> {
        const SEATBELT_EXECUTABLE: &str = "/usr/bin/sandbox-exec";

        // Build the seatbelt profile
        let profile = self.build_seatbelt_profile(policy, sandbox_cwd)?;

        let mut args = vec!["-p".to_string(), profile, os_string_to_arg(spec.program.clone())];
        args.extend(spec.args);

        Ok(ExecEnv {
            program: SEATBELT_EXECUTABLE.into(),
            args,
            cwd: spec.cwd,
            env: spec.env,
            expiration: spec.expiration,
            sandbox_active: true,
            sandbox_type: SandboxType::MacosSeatbelt,
        })
    }

    #[cfg(not(target_os = "macos"))]
    fn transform_seatbelt(
        &self,
        _spec: CommandSpec,
        _policy: &SandboxPolicy,
        _sandbox_cwd: &Path,
    ) -> Result<ExecEnv, SandboxTransformError> {
        Err(SandboxTransformError::UnavailableSandboxType(SandboxType::MacosSeatbelt))
    }

    /// Build a seatbelt profile string.
    ///
    /// Implements the field guide's recommendations:
    /// - "Default-deny outbound network, then allowlist."
    /// - Block sensitive paths to prevent credential leakage.
    #[cfg(target_os = "macos")]
    fn build_seatbelt_profile(
        &self,
        policy: &SandboxPolicy,
        sandbox_cwd: &Path,
    ) -> Result<String, SandboxTransformError> {
        fn append_network_rules(
            profile: &mut String,
            network_access: bool,
            network_allowlist: &[NetworkAllowlistEntry],
        ) -> Result<(), SandboxTransformError> {
            if !network_allowlist.is_empty() {
                return Err(SandboxTransformError::InvalidPolicy(
                    "macOS Seatbelt cannot enforce hostname network allowlists exactly; refusing to widen access"
                        .to_string(),
                ));
            }

            if !network_access {
                // Keep local unix sockets available even when outbound network is restricted.
                profile.push_str("(allow network* (local unix))\n");
            }
            if network_access {
                profile.push_str("(allow network*)\n");
            }
            Ok(())
        }

        let mut profile = String::from("(version 1)\n");
        profile.push_str("(deny default)\n");
        profile.push_str("(allow process-exec)\n");
        profile.push_str("(allow process-fork)\n");
        profile.push_str("(allow sysctl-read)\n");
        profile.push_str("(allow mach-lookup)\n");
        profile.push_str("(allow ipc-posix-shm-read* (ipc-posix-name-prefix \"apple.cfprefs.\"))\n");
        profile.push_str("(allow mach-lookup (global-name \"com.apple.cfprefsd.daemon\") (global-name \"com.apple.cfprefsd.agent\") (local-name \"com.apple.cfprefsd.agent\"))\n");
        profile.push_str("(allow user-preference-read)\n");

        // Block sensitive paths BEFORE allowing general read access
        // This ensures deny rules take precedence
        let sensitive_paths = policy.sensitive_paths_for_execution(sandbox_cwd);
        for sp in &sensitive_paths {
            let expanded = sp.expand_path();
            let path_str = expanded.display();
            if sp.block_read {
                profile.push_str(&format!("(deny file-read* (subpath \"{path_str}\"))\n"));
            }
            if sp.block_write {
                profile.push_str(&format!("(deny file-write* (subpath \"{path_str}\"))\n"));
            }
        }

        // Allow reading from everywhere (except denied sensitive paths above)
        profile.push_str("(allow file-read*)\n");

        match policy {
            SandboxPolicy::ReadOnly { network_access, network_allowlist } => {
                // Read-only: only allow writing to /dev/null
                profile.push_str("(allow file-write* (literal \"/dev/null\"))\n");
                append_network_rules(&mut profile, *network_access, network_allowlist)?;
            }
            SandboxPolicy::WorkspaceWrite { network_access, network_allowlist, .. } => {
                for root in policy.get_writable_roots_with_cwd(sandbox_cwd) {
                    let path = root.root.display();
                    profile.push_str(&format!("(allow file-write* (subpath \"{path}\"))\n"));
                }
                append_network_rules(&mut profile, *network_access, network_allowlist)?;
            }
            _ => {}
        }

        Ok(profile)
    }

    /// Transform for Linux Landlock sandbox.
    ///
    /// Following the field guide: "Landlock + seccomp is the recommended Linux pattern."
    /// The sandbox helper binary receives both the policy (for Landlock filesystem rules)
    /// and the seccomp profile (for syscall filtering).
    fn transform_landlock(
        &self,
        spec: CommandSpec,
        policy: &SandboxPolicy,
        sandbox_cwd: &Path,
        sandbox_executable: Option<&Path>,
    ) -> Result<ExecEnv, SandboxTransformError> {
        let sandbox_exe = sandbox_executable.ok_or(SandboxTransformError::MissingSandboxExecutable)?;

        // Serialize the policy for the sandbox helper (includes Landlock rules)
        let policy_json = serde_json::to_string(policy)
            .map_err(|e| SandboxTransformError::CreationFailed(format!("failed to serialize sandbox policy: {e}")))?;

        // Serialize seccomp profile separately for explicit syscall filtering
        let seccomp_profile = policy.seccomp_profile();
        let seccomp_json = seccomp_profile
            .to_json()
            .map_err(|e| SandboxTransformError::CreationFailed(format!("failed to serialize seccomp profile: {e}")))?;

        // Serialize resource limits for cgroup/rlimit enforcement
        let resource_limits = policy.resource_limits();
        let limits_json = serde_json::to_string(&resource_limits)
            .map_err(|e| SandboxTransformError::CreationFailed(format!("failed to serialize resource limits: {e}")))?;

        let sandbox_cwd_str = sandbox_cwd.to_string_lossy().to_string();

        let mut args = vec![
            "--sandbox-policy-cwd".to_string(),
            sandbox_cwd_str,
            "--sandbox-policy".to_string(),
            policy_json,
            "--seccomp-profile".to_string(),
            seccomp_json,
            "--resource-limits".to_string(),
            limits_json,
            "--".to_string(),
            os_string_to_arg(spec.program.clone()),
        ];
        args.extend(spec.args);

        Ok(ExecEnv {
            program: sandbox_exe.to_path_buf(),
            args,
            cwd: spec.cwd,
            env: spec.env,
            expiration: spec.expiration,
            sandbox_active: true,
            sandbox_type: SandboxType::LinuxLandlock,
        })
    }

    /// Transform for Windows restricted token sandbox.
    ///
    /// Not yet implemented. Returns `UnavailableSandboxType` so that callers
    /// requesting a restrictive policy on Windows get an explicit error
    /// instead of silently running unsandboxed. The `is_available()` check in
    /// `transform()` normally catches this first, but this guard ensures
    /// fail-closed behavior even if the availability check is bypassed.
    fn transform_windows(
        &self,
        _spec: CommandSpec,
        _policy: &SandboxPolicy,
        _sandbox_cwd: &Path,
    ) -> Result<ExecEnv, SandboxTransformError> {
        Err(SandboxTransformError::UnavailableSandboxType(SandboxType::WindowsRestrictedToken))
    }
}

fn os_string_to_arg(value: OsString) -> String {
    value.into_string().unwrap_or_else(|value| value.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_sandbox_for_full_access() {
        let manager = SandboxManager::new();
        let spec = CommandSpec::new("echo").with_args(vec!["hello"]);
        let policy = SandboxPolicy::full_access();

        let env = manager.transform(spec, &policy, Path::new("/tmp"), None).unwrap();

        assert!(!env.sandbox_active);
        assert_eq!(env.sandbox_type, SandboxType::None);
    }

    #[test]
    fn test_sandbox_type_determination() {
        let manager = SandboxManager::new();

        // Full access = no sandbox
        let result = manager.determine_sandbox_type(&SandboxPolicy::DangerFullAccess);
        assert_eq!(result.unwrap(), SandboxType::None);

        // Read-only = platform default
        let result = manager.determine_sandbox_type(&SandboxPolicy::read_only());
        assert_eq!(result.unwrap(), SandboxType::platform_default());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_profile_includes_default_preferences_policy() {
        let manager = SandboxManager::new();
        let profile = manager
            .build_seatbelt_profile(&SandboxPolicy::read_only(), Path::new("/tmp"))
            .unwrap();

        assert!(profile.contains("(allow ipc-posix-shm-read* (ipc-posix-name-prefix \"apple.cfprefs.\"))"));
        assert!(profile.contains("(global-name \"com.apple.cfprefsd.daemon\")"));
        assert!(profile.contains("(global-name \"com.apple.cfprefsd.agent\")"));
        assert!(profile.contains("(local-name \"com.apple.cfprefsd.agent\")"));
        assert!(profile.contains("(allow user-preference-read)"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_rejects_hostname_allowlist_without_exact_enforcement() {
        let manager = SandboxManager::new();
        let policy = SandboxPolicy::read_only_with_network(vec![NetworkAllowlistEntry::https("api.example.com")]);

        let result = manager.build_seatbelt_profile(&policy, Path::new("/tmp"));

        assert!(matches!(result, Err(SandboxTransformError::InvalidPolicy(message)) if message.contains("hostname")));
    }

    /// Windows restricted-token sandbox is not yet implemented, so
    /// `is_available()` must return `false` on **all** platforms. This
    /// prevents silent pass-through when a restrictive policy is requested.
    #[test]
    fn windows_restricted_token_is_not_available() {
        assert!(!SandboxType::WindowsRestrictedToken.is_available());
    }

    /// `transform_windows` must fail-closed with `UnavailableSandboxType`,
    /// not silently pass the command through unsandboxed.
    #[test]
    fn transform_windows_fails_closed() {
        let manager = SandboxManager::new();
        let spec = CommandSpec::new("echo").with_args(vec!["hello"]);
        let result = manager.transform_windows(spec, &SandboxPolicy::read_only(), Path::new("/tmp"));

        assert!(matches!(
            result,
            Err(SandboxTransformError::UnavailableSandboxType(SandboxType::WindowsRestrictedToken))
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn restrictive_linux_policy_requires_sandbox_helper() {
        let manager = SandboxManager::new();
        let spec = CommandSpec::new("echo").with_args(vec!["hello"]);

        let result = manager.transform(spec, &SandboxPolicy::read_only(), Path::new("/tmp"), None);

        assert!(matches!(result, Err(SandboxTransformError::MissingSandboxExecutable)));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn restrictive_policy_filters_sensitive_environment_overrides() {
        use hashbrown::HashMap;

        let manager = SandboxManager::new();
        let mut env = HashMap::new();
        drop(env.insert("OPENAI_API_KEY".to_string(), "secret-value".to_string()));
        drop(env.insert("LD_PRELOAD".to_string(), "injected.so".to_string()));
        drop(env.insert("SAFE_PROJECT_NAME".to_string(), "vtcode".to_string()));
        drop(env.insert("PATH".to_string(), "/usr/bin:/bin".to_string()));
        drop(env.insert("INTERNAL_AUTH_BLOB".to_string(), "secret".to_string()));
        let spec = CommandSpec::new("echo").with_env(env);
        let sandbox_helper = if cfg!(target_os = "linux") {
            Some(Path::new("/tmp/vtcode-test-sandbox-helper"))
        } else {
            None
        };

        let transformed = manager
            .transform(spec, &SandboxPolicy::read_only(), Path::new("/tmp"), sandbox_helper)
            .unwrap();

        assert!(transformed.sandbox_active);
        assert!(!transformed.env.contains_key("OPENAI_API_KEY"));
        assert!(!transformed.env.contains_key("LD_PRELOAD"));
        assert!(!transformed.env.contains_key("INTERNAL_AUTH_BLOB"));
        assert!(!transformed.env.contains_key("SAFE_PROJECT_NAME"));
        assert_eq!(transformed.env.get("PATH"), Some(&"/usr/bin:/bin".to_string()));
    }
    #[test]
    fn explicit_full_access_preserves_caller_environment() {
        let manager = SandboxManager::new();
        let mut env = hashbrown::HashMap::new();
        drop(env.insert("INTERNAL_AUTH_BLOB".to_string(), "explicit".to_string()));
        let result = manager
            .transform(
                CommandSpec::new("echo").with_env(env.clone()),
                &SandboxPolicy::full_access(),
                Path::new("."),
                None,
            )
            .expect("full access transform");
        assert!(!result.sandbox_active);
        assert_eq!(result.env, env);
    }
}
