use crate::audit::PermissionDecision;
use crate::config::CommandsConfig;
use crate::tools::command_cache::PermissionCache;
use crate::tools::command_resolver::CommandResolver;
use regex::Regex;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::warn;

#[derive(Clone)]
pub struct CommandPolicyEvaluator {
    allow_prefixes: Vec<String>,
    deny_prefixes: Vec<String>,
    allow_regexes: Vec<Regex>,
    deny_regexes: Vec<Regex>,
    allow_glob_regexes: Vec<Regex>,
    deny_glob_regexes: Vec<Regex>,
    allow_regexes_empty: bool,
    allow_globs_empty: bool,
    // NEW: Command resolution and caching for improved security visibility
    resolver: Arc<Mutex<CommandResolver>>,
    cache: Arc<Mutex<PermissionCache>>,
}

impl CommandPolicyEvaluator {
    pub fn from_config(config: &CommandsConfig) -> Self {
        let allow_prefixes = crate::utils::merge_env_patterns(&config.allow_list, "VTCODE_COMMANDS_ALLOW_LIST");
        let deny_prefixes = crate::utils::merge_env_patterns(&config.deny_list, "VTCODE_COMMANDS_DENY_LIST");

        let allow_regex_patterns = crate::utils::merge_env_patterns(&config.allow_regex, "VTCODE_COMMANDS_ALLOW_REGEX");
        let deny_regex_patterns = crate::utils::merge_env_patterns(&config.deny_regex, "VTCODE_COMMANDS_DENY_REGEX");

        let allow_glob_patterns = crate::utils::merge_env_patterns(&config.allow_glob, "VTCODE_COMMANDS_ALLOW_GLOB");
        let deny_glob_patterns = crate::utils::merge_env_patterns(&config.deny_glob, "VTCODE_COMMANDS_DENY_GLOB");

        let allow_regexes = compile_regexes(&allow_regex_patterns);
        let deny_regexes = compile_regexes(&deny_regex_patterns);
        let allow_glob_regexes = compile_globs(&allow_glob_patterns);
        let deny_glob_regexes = compile_globs(&deny_glob_patterns);

        Self {
            allow_prefixes,
            deny_prefixes,
            allow_regexes,
            deny_regexes,
            allow_glob_regexes,
            deny_glob_regexes,
            allow_regexes_empty: allow_regex_patterns.is_empty(),
            allow_globs_empty: allow_glob_patterns.is_empty(),
            resolver: Arc::new(Mutex::new(CommandResolver::new())),
            cache: Arc::new(Mutex::new(PermissionCache::new())),
        }
    }

    fn cached_decision(&self, command_text: &str) -> Option<bool> {
        self.cache
            .lock()
            .unwrap_or_else(|poisoned| {
                warn!("command_policy: permission cache mutex poisoned; recovering");
                poisoned.into_inner()
            })
            .get(command_text)
    }

    fn resolve_path(&self, command_text: &str) -> Option<PathBuf> {
        self.resolver
            .lock()
            .unwrap_or_else(|poisoned| {
                warn!("command_policy: command resolver mutex poisoned; recovering");
                poisoned.into_inner()
            })
            .resolve(command_text)
            .resolved_path
            .clone()
    }

    fn cache_decision(&self, command_text: &str, allowed: bool, reason: &str) {
        let mut cache = self.cache.lock().unwrap_or_else(|poisoned| {
            warn!("command_policy: permission cache mutex poisoned; recovering");
            poisoned.into_inner()
        });
        cache.put(command_text, allowed, reason);
    }

    pub fn allows(&self, command: &[String]) -> bool {
        if command.is_empty() {
            return false;
        }
        let command_text = command.join(" ");
        self.allows_text(&command_text)
    }

    pub fn allows_text(&self, command_text: &str) -> bool {
        let cmd = command_text.trim();
        if cmd.is_empty() {
            return false;
        }

        let segments = policy_segments(cmd);

        // Deny takes precedence. Every shell segment is evaluated so chained,
        // piped, or list-joined commands cannot ride on an earlier match.
        if segments.iter().any(|segment| {
            self.matches_prefix(segment, &self.deny_prefixes)
                || Self::matches_any(&self.deny_regexes, segment)
                || Self::matches_any(&self.deny_glob_regexes, segment)
        }) {
            return false;
        }

        // If no allow rules defined, allow by default
        if self.allow_prefixes.is_empty() && self.allow_regexes_empty && self.allow_globs_empty {
            return true;
        }

        // Check allow rules: each segment must independently match
        segments.iter().all(|segment| {
            self.matches_prefix(segment, &self.allow_prefixes)
                || Self::matches_any(&self.allow_regexes, segment)
                || Self::matches_any(&self.allow_glob_regexes, segment)
        })
    }

    /// Enhanced async evaluation with command resolution and caching
    /// Returns (allowed, resolved_path, reason, decision)
    pub fn evaluate_with_resolution(&self, command_text: &str) -> (bool, Option<PathBuf>, String, PermissionDecision) {
        let cmd = command_text.trim();

        // Check cache first
        if let Some(allowed) = self.cached_decision(cmd) {
            let reason = if allowed {
                "Cached allow decision"
            } else {
                "Cached deny decision"
            };
            return (allowed, None, reason.to_string(), PermissionDecision::Cached);
        }

        // Resolve command to actual path
        let resolved_path = self.resolve_path(cmd);

        // Evaluate policy
        let allowed = self.allows_text(cmd);

        // Determine reason - use static strings where possible to avoid allocations
        let reason = if allowed {
            if self.matches_prefix(cmd, &self.allow_prefixes) {
                format!("allow_list match: {cmd}")
            } else if Self::matches_any(&self.allow_glob_regexes, cmd) {
                "allow_glob match".to_string()
            } else {
                "allow_regex match".to_string()
            }
        } else if self.matches_prefix(cmd, &self.deny_prefixes) {
            format!("deny_list match: {cmd}")
        } else if Self::matches_any(&self.deny_glob_regexes, cmd) {
            "deny_glob match".to_string()
        } else {
            "deny_regex match".to_string()
        };

        // Cache the result
        self.cache_decision(cmd, allowed, &reason);

        let decision = if allowed {
            PermissionDecision::Allowed
        } else {
            PermissionDecision::Denied
        };

        (allowed, resolved_path, reason, decision)
    }

    fn matches_prefix(&self, value: &str, prefixes: &[String]) -> bool {
        prefixes
            .iter()
            .filter(|pattern| !pattern.is_empty())
            .any(|pattern| value.starts_with(pattern))
    }
    fn matches_any(regexes: &[Regex], value: &str) -> bool {
        regexes.iter().any(|re| re.is_match(value))
    }
}

/// Split a command string into its shell segments so prefix/regex policy
/// rules apply to every chained, piped, or list-joined command rather than
/// only to the first. Falls back to the whole string when the parser cannot
/// handle the syntax, preserving the previous whole-string behavior.
fn policy_segments(command_text: &str) -> Vec<String> {
    match crate::command_safety::shell_parser::parse_shell_commands(command_text) {
        Ok(segments) if !segments.is_empty() => segments.into_iter().map(|argv| argv.join(" ")).collect(),
        _ => vec![command_text.to_string()],
    }
}

fn compile_regexes(patterns: &[String]) -> Vec<Regex> {
    patterns
        .iter()
        .filter_map(|pattern| {
            Regex::new(pattern)
                .map_err(|error| {
                    warn!(%error, %pattern, "Ignoring invalid command regex pattern");
                    error
                })
                .ok()
        })
        .collect()
}

fn compile_globs(patterns: &[String]) -> Vec<Regex> {
    patterns
        .iter()
        .filter_map(|pattern| {
            let escaped = regex::escape(pattern);
            let glob_regex = format!("^{}$", escaped.replace(r"\*", ".*").replace(r"\?", "."));
            Regex::new(&glob_regex)
                .map_err(|error| {
                    warn!(%error, pattern = %pattern, "Ignoring invalid command glob pattern");
                    error
                })
                .ok()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CommandsConfig;

    #[test]
    fn glob_allows_cargo_commands() {
        let mut config = CommandsConfig::default();
        config.allow_list.clear();
        config.allow_regex.clear();
        config.allow_glob = vec!["cargo *".to_string()];
        let evaluator = CommandPolicyEvaluator::from_config(&config);
        assert!(evaluator.allows_text("cargo fmt"));
        assert!(evaluator.allows(&["cargo".into(), "check".into()]));
    }

    #[test]
    fn glob_supports_question_mark() {
        let mut config = CommandsConfig::default();
        config.allow_list.clear();
        config.allow_regex.clear();
        config.allow_glob = vec!["go test ./pkg/?".to_string()];
        let evaluator = CommandPolicyEvaluator::from_config(&config);
        assert!(evaluator.allows_text("go test ./pkg/a"));
        assert!(!evaluator.allows_text("go test ./pkg/ab"));
    }

    #[test]
    fn glob_allows_node_ecosystem_commands() {
        let mut config = CommandsConfig::default();
        config.allow_list.clear();
        config.allow_regex.clear();
        config.allow_glob = vec!["npm *".to_string(), "bun *".to_string()];
        let evaluator = CommandPolicyEvaluator::from_config(&config);
        assert!(evaluator.allows_text("npm install"));
        assert!(evaluator.allows_text("npm run build"));
        assert!(evaluator.allows_text("bun install"));
        assert!(evaluator.allows_text("bun run check"));
    }

    #[test]
    fn allow_list_allows_exact_git_and_cargo_commands() {
        let mut config = CommandsConfig::default();
        // Clear default allow_list to reduce noise
        config.allow_list.clear();
        config.allow_list.push("git".to_string());
        config.allow_list.push("cargo".to_string());
        let evaluator = CommandPolicyEvaluator::from_config(&config);
        assert!(evaluator.allows_text("git"));
        assert!(evaluator.allows_text("cargo"));
        assert!(evaluator.allows(&["git".into()]));
        assert!(evaluator.allows(&["cargo".into()]));
    }
}
