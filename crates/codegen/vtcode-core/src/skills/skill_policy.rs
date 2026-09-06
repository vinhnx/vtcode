//! Skill-scoped tool and sandbox policy.

use crate::llm::provider::ToolDefinition;
use crate::sandboxing::{AdditionalPermissions, SandboxPermissions};
use crate::skills::types::{Skill, SkillNetworkPolicy};
use crate::tools::registry::{ToolErrorType, ToolExecutionError};
use crate::tools::tool_intent;
use anyhow::{Result, anyhow};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// Network-capable tool names that should be filtered based on skill network policy.
const NETWORK_TOOLS: &[&str] = &[
    "http",
    "fetch",
    "browser",
    "web_search",
    "web_fetch",
    "defuddle_fetch",
    "read_web_page",
    "curl",
];

/// Execution-time allowlist derived from the definitions presented to a skill sub-call.
///
/// Tool definitions are model-facing data, so they are not themselves an authorization
/// boundary. This scope is checked immediately before a sub-call reaches the registry.
#[derive(Debug, Default)]
pub(super) struct SkillToolScope {
    allowed_tool_names: BTreeSet<String>,
}

impl SkillToolScope {
    pub(super) fn from_definitions(definitions: &[ToolDefinition]) -> Self {
        Self {
            allowed_tool_names: definitions.iter().map(|tool| tool.function_name().to_string()).collect(),
        }
    }

    pub(super) fn permits(&self, tool_name: &str) -> bool {
        self.allowed_tool_names.contains(tool_name)
    }

    pub(super) fn denied_error(&self, skill: &Skill, tool_name: &str) -> ToolExecutionError {
        ToolExecutionError::new(
            tool_name,
            ToolErrorType::ToolNotFound,
            format!("Tool '{tool_name}' is not available to skill '{}'", skill.name()),
        )
    }
}

fn is_function_network_tool(tool: &ToolDefinition) -> bool {
    tool.function.as_ref().is_some_and(|function| {
        let name = function.name.to_ascii_lowercase();
        NETWORK_TOOLS.contains(&name.as_str())
    })
}

fn is_native_web_search_tool(tool: &ToolDefinition) -> bool {
    matches!(tool.tool_type.as_str(), "web_search" | "google_search") || tool.tool_type.starts_with("web_search_")
}

fn is_gemini_native_network_tool(tool: &ToolDefinition) -> bool {
    matches!(tool.tool_type.as_str(), "google_maps" | "url_context")
}

fn is_network_capable_tool(tool: &ToolDefinition) -> bool {
    is_native_web_search_tool(tool) || is_gemini_native_network_tool(tool) || is_function_network_tool(tool)
}

fn json_string_array(config: &Map<String, Value>, key: &str) -> Result<Option<Vec<String>>> {
    let Some(value) = config.get(key) else {
        return Ok(None);
    };
    let Value::Array(values) = value else {
        return Err(anyhow!("{key} must be an array of strings"));
    };

    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| anyhow!("{key} must contain only strings"))
        })
        .collect::<Result<Vec<_>>>()
        .map(Some)
}

fn set_json_string_array(config: &mut Map<String, Value>, key: &str, values: Vec<String>) {
    if values.is_empty() {
        config.remove(key);
        return;
    }

    config.insert(key.to_string(), Value::Array(values.into_iter().map(Value::String).collect()));
}

fn intersect_domains(existing: Option<Vec<String>>, requested: &[String]) -> Vec<String> {
    match existing {
        Some(existing) => existing
            .into_iter()
            .filter(|domain| requested.iter().any(|candidate| candidate == domain))
            .collect(),
        None => requested.to_vec(),
    }
}

fn union_domains(existing: Option<Vec<String>>, requested: &[String]) -> Vec<String> {
    let mut merged = existing.unwrap_or_default();
    for domain in requested {
        if !merged.iter().any(|candidate| candidate == domain) {
            merged.push(domain.clone());
        }
    }
    merged
}

fn apply_web_search_policy(
    skill: &Skill,
    tool: &ToolDefinition,
    policy: &SkillNetworkPolicy,
) -> Option<ToolDefinition> {
    let mut updated = tool.clone();
    let existing_config = match updated.web_search.take() {
        Some(Value::Object(config)) => config,
        Some(_) => {
            warn!(
                skill = skill.name(),
                tool_type = %tool.tool_type,
                "Dropping network tool because web search policy could not be encoded"
            );
            return None;
        }
        None => Map::new(),
    };

    let existing_allowed = match json_string_array(&existing_config, "allowed_domains") {
        Ok(value) => value,
        Err(error) => {
            warn!(
                skill = skill.name(),
                tool_type = %tool.tool_type,
                error = %error,
                "Dropping network tool because web search policy could not be encoded"
            );
            return None;
        }
    };
    let existing_blocked = match json_string_array(&existing_config, "blocked_domains") {
        Ok(value) => value,
        Err(error) => {
            warn!(
                skill = skill.name(),
                tool_type = %tool.tool_type,
                error = %error,
                "Dropping network tool because web search policy could not be encoded"
            );
            return None;
        }
    };
    let merged_allowed = if policy.allowed_domains.is_empty() {
        existing_allowed.unwrap_or_default()
    } else {
        intersect_domains(existing_allowed, &policy.allowed_domains)
    };
    let merged_blocked = if policy.denied_domains.is_empty() {
        existing_blocked.unwrap_or_default()
    } else {
        union_domains(existing_blocked, &policy.denied_domains)
    };

    if updated.is_anthropic_web_search() && !merged_allowed.is_empty() && !merged_blocked.is_empty() {
        warn!(
            skill = skill.name(),
            tool_type = %tool.tool_type,
            "Dropping anthropic web search tool because allowlist and denylist cannot both be enforced"
        );
        return None;
    }

    let mut config = existing_config;
    set_json_string_array(&mut config, "allowed_domains", merged_allowed);
    set_json_string_array(&mut config, "blocked_domains", merged_blocked);
    updated.web_search = Some(Value::Object(config));

    if let Err(error) = updated.validate() {
        warn!(
            skill = skill.name(),
            tool_type = %tool.tool_type,
            error = %error,
            "Dropping network tool because the enforced web search policy is invalid"
        );
        return None;
    }

    Some(updated)
}

/// Filter available tools based on a skill's network policy.
///
/// - If a skill has no network policy, network-capable tools are removed.
/// - If a skill has a policy, it is encoded for native web-search tools.
/// - If a policy cannot be encoded safely, the tool is removed.
pub fn filter_tools_for_skill(skill: &Skill, tools: Vec<ToolDefinition>) -> Vec<ToolDefinition> {
    let network_policy = &skill.manifest.network_policy;

    match network_policy {
        None => tools
            .into_iter()
            .filter(|tool| {
                let is_network = is_network_capable_tool(tool);
                if is_network {
                    debug!(
                        tool = tool.function_name(),
                        "Filtered network tool for skill '{}' (no network policy)",
                        skill.name()
                    );
                }
                !is_network
            })
            .collect(),
        Some(policy) => tools
            .into_iter()
            .filter_map(|tool| {
                if !is_network_capable_tool(&tool) {
                    return Some(tool);
                }

                if is_native_web_search_tool(&tool) {
                    return apply_web_search_policy(skill, &tool, policy);
                }

                if is_gemini_native_network_tool(&tool) {
                    info!(
                        skill = skill.name(),
                        tool = tool.function_name(),
                        "Dropping Gemini native network tool because skill domain policy cannot be enforced safely"
                    );
                    return None;
                }

                info!(
                    skill = skill.name(),
                    tool = tool.function_name(),
                    "Dropping network tool because skill policy cannot be enforced for function-style tools"
                );
                None
            })
            .collect(),
    }
}

/// Apply trusted registry metadata before exposing function tools to a skill.
/// Domain restrictions cannot currently be enforced on arbitrary function tools.
pub(super) fn filter_registered_tools_for_skill(
    skill: &Skill,
    tools: Vec<ToolDefinition>,
    registry: &crate::tools::registry::ToolRegistry,
) -> Vec<ToolDefinition> {
    filter_tools_for_skill(skill, tools)
        .into_iter()
        .filter(|tool| tool.function.is_none() || skill_function_tool_permitted(registry, tool.function_name()))
        .collect()
}

pub(super) fn skill_function_tool_permitted(registry: &crate::tools::registry::ToolRegistry, name: &str) -> bool {
    registry.tool_network_access(name) == crate::tools::registry::ToolNetworkAccess::Local
}

fn skill_additional_permissions(skill: &Skill) -> Option<AdditionalPermissions> {
    let file_system = skill.manifest.permissions.as_ref()?.file_system.as_ref()?;
    let fs_read = resolve_skill_permission_paths(skill.path.as_path(), &file_system.read);
    let fs_write = resolve_skill_permission_paths(skill.path.as_path(), &file_system.write);
    let permissions = AdditionalPermissions { fs_read, fs_write };
    (!permissions.is_empty()).then_some(permissions)
}

fn resolve_skill_permission_paths(skill_root: &Path, paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut resolved = Vec::with_capacity(paths.len());
    let mut seen = BTreeSet::new();

    for path in paths {
        if path.as_os_str().is_empty() {
            continue;
        }

        let absolute = if path.is_absolute() {
            path.clone()
        } else {
            skill_root.join(path)
        };
        let normalized = crate::utils::path::normalize_path(&absolute);
        if seen.insert(normalized.clone()) {
            resolved.push(normalized);
        }
    }

    resolved
}

fn merge_permission_paths(existing: &[PathBuf], extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut merged = Vec::with_capacity(existing.len() + extra.len());
    let mut seen = BTreeSet::new();

    for path in existing.iter().chain(extra.iter()) {
        if seen.insert(path.clone()) {
            merged.push(path.clone());
        }
    }

    merged
}

fn merge_additional_permissions(
    existing: &AdditionalPermissions,
    extra: &AdditionalPermissions,
) -> AdditionalPermissions {
    AdditionalPermissions {
        fs_read: merge_permission_paths(&existing.fs_read, &extra.fs_read),
        fs_write: merge_permission_paths(&existing.fs_write, &extra.fs_write),
    }
}

pub(super) fn merge_skill_command_permissions(skill: &Skill, tool_name: &str, tool_args: Value) -> Value {
    if !tool_intent::is_command_run_tool_call(tool_name, &tool_args) {
        return tool_args;
    }

    let Some(skill_permissions) = skill_additional_permissions(skill) else {
        return tool_args;
    };

    let mut args = match tool_args {
        Value::Object(args) => args,
        other => return other,
    };

    let sandbox_permissions = match args.get("sandbox_permissions") {
        Some(value) => match serde_json::from_value::<SandboxPermissions>(value.clone()) {
            Ok(value) => value,
            Err(_) => return Value::Object(args),
        },
        None => SandboxPermissions::UseDefault,
    };

    if matches!(sandbox_permissions, SandboxPermissions::RequireEscalated | SandboxPermissions::BypassSandbox) {
        return Value::Object(args);
    }

    let existing_permissions = match args.get("additional_permissions") {
        Some(value) => match serde_json::from_value::<AdditionalPermissions>(value.clone()) {
            Ok(value) => value,
            Err(_) => return Value::Object(args),
        },
        None => AdditionalPermissions::default(),
    };

    let merged_permissions = merge_additional_permissions(&existing_permissions, &skill_permissions);
    args.insert(
        "sandbox_permissions".to_string(),
        serde_json::to_value(SandboxPermissions::WithAdditionalPermissions)
            .expect("sandbox permissions should serialize"),
    );
    args.insert(
        "additional_permissions".to_string(),
        serde_json::to_value(&merged_permissions).expect("additional permissions should serialize"),
    );
    debug!("Applied skill-scoped sandbox permissions for '{}' to tool '{}'", skill.name(), tool_name);

    Value::Object(args)
}
