use super::ZedAgent;
use crate::tooling::ToolDescriptor;
use crate::zed::helpers::PrimaryAgentCatalog;
use crate::zed::types::ToolRuntime;
use assert_fs::TempDir;
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::Duration;
use vtcode_config::auth::AuthCredentialsStoreMode;
use vtcode_config::constants::tool_limits;
use vtcode_config::{SubagentDiscoveryInput, discover_subagents};
use vtcode_core::config::constants::tools;
use vtcode_core::config::core::PromptCachingConfig;
use vtcode_core::config::tool_call_delay_for_rate;
use vtcode_core::config::types::{
    AgentConfig as CoreAgentConfig, ModelSelectionSource, ReasoningEffortLevel, UiSurfacePreference,
};
use vtcode_core::config::{AgentClientProtocolZedConfig, CommandsConfig, ToolProfile, ToolsConfig};
use vtcode_core::core::agent::snapshots::{DEFAULT_CHECKPOINTS_ENABLED, DEFAULT_MAX_AGE_DAYS, DEFAULT_MAX_SNAPSHOTS};
use vtcode_core::llm::provider::{MessageRole, ToolDefinition};

async fn build_agent(workspace: &Path) -> ZedAgent {
    build_agent_with_tools_config(workspace, ToolsConfig::default()).await
}

async fn build_agent_with_tools_config(workspace: &Path, tools_config: ToolsConfig) -> ZedAgent {
    let core_config = CoreAgentConfig {
        model: "test-model".to_string(),
        api_key: String::new(),
        provider: "test-provider".to_string(),
        api_key_env: "TEST_API_KEY".to_string(),
        workspace: workspace.to_path_buf(),
        verbose: false,
        quiet: false,
        theme: "test".to_string(),
        reasoning_effort: ReasoningEffortLevel::Low,
        ui_surface: UiSurfacePreference::default(),
        prompt_cache: PromptCachingConfig::default(),
        model_source: ModelSelectionSource::WorkspaceConfig,
        custom_api_keys: BTreeMap::new(),
        checkpointing_enabled: DEFAULT_CHECKPOINTS_ENABLED,
        checkpointing_storage_dir: None,
        checkpointing_max_snapshots: DEFAULT_MAX_SNAPSHOTS,
        checkpointing_max_age_days: Some(DEFAULT_MAX_AGE_DAYS),
        max_conversation_turns: 1000,
        model_behavior: None,
        openai_chatgpt_auth: None,
    };

    let mut zed_config = AgentClientProtocolZedConfig::default();
    zed_config.tools.list_files = true;
    zed_config.tools.read_file = false;

    let mut discovery_input = SubagentDiscoveryInput::new(workspace.to_path_buf());
    discovery_input.include_user_agents = false;
    let discovered = discover_subagents(&discovery_input).expect("discover primary agents");
    let primary_agents = PrimaryAgentCatalog::from_specs_with_default(&discovered.effective, "duck");

    ZedAgent::new(
        core_config,
        false,
        AuthCredentialsStoreMode::default(),
        zed_config,
        tools_config,
        CommandsConfig::default(),
        String::new(),
        Some("Zed".to_string()),
        primary_agents,
    )
    .await
}

#[test]
fn default_zed_tool_config_uses_shared_tool_loop_budget() {
    assert_eq!(ToolsConfig::default().max_tool_loops, tool_limits::DEFAULT_MAX_TOOL_LOOPS);
}

#[test]
fn tool_call_delay_for_rate_ignores_unset_or_zero_limits() {
    assert_eq!(tool_call_delay_for_rate(None), None);
    assert_eq!(tool_call_delay_for_rate(Some(0)), None);
}

#[test]
fn tool_call_delay_for_rate_uses_per_second_interval() {
    assert_eq!(tool_call_delay_for_rate(Some(4)), Some(Duration::from_millis(250)));
}

#[tokio::test]
async fn tool_loop_limit_uses_tools_config() {
    let temp = TempDir::new().unwrap();
    let tools_config = ToolsConfig { max_tool_loops: 2, ..ToolsConfig::default() };
    let agent = build_agent_with_tools_config(temp.path(), tools_config).await;

    assert!(!agent.tool_loop_limit_reached(0));
    assert!(!agent.tool_loop_limit_reached(1));
    assert!(agent.tool_loop_limit_reached(2));
    assert!(agent.tool_loop_limit_message().contains("maximum tool loops (2)"));
}

fn definition_names(definitions: Vec<ToolDefinition>) -> Vec<String> {
    definitions
        .into_iter()
        .map(|definition| definition.function_name().to_string())
        .collect()
}

#[test]
fn parse_terminal_command_rejects_empty_array() {
    let args = json!({ "command": [] });
    let result = ZedAgent::parse_terminal_command(&args);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "command array cannot be empty");
}

#[test]
fn parse_terminal_command_rejects_empty_string() {
    let args = json!({ "command": "" });
    let result = ZedAgent::parse_terminal_command(&args);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "command string cannot be empty");
}

#[test]
fn parse_terminal_command_rejects_whitespace_only_string() {
    let args = json!({ "command": "   " });
    let result = ZedAgent::parse_terminal_command(&args);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "command string cannot be empty");
}

#[test]
fn parse_terminal_command_rejects_empty_executable_in_array() {
    let args = json!({ "command": ["", "arg1", "arg2"] });
    let result = ZedAgent::parse_terminal_command(&args);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "command executable cannot be empty");
}

#[test]
fn parse_terminal_command_rejects_whitespace_only_executable_in_array() {
    let args = json!({ "command": ["  ", "arg1"] });
    let result = ZedAgent::parse_terminal_command(&args);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "command executable cannot be empty");
}

#[test]
fn parse_terminal_command_accepts_valid_array() {
    let args = json!({ "command": ["ls", "-la"] });
    let result = ZedAgent::parse_terminal_command(&args);
    assert!(result.is_ok());
    let cmd = result.unwrap();
    assert_eq!(cmd, vec!["ls", "-la"]);
}

#[test]
fn parse_terminal_command_accepts_valid_string() {
    let args = json!({ "command": "echo test" });
    let result = ZedAgent::parse_terminal_command(&args);
    assert!(result.is_ok());
    let cmd = result.unwrap();
    assert_eq!(cmd, vec!["echo", "test"]);
}

#[test]
fn parse_terminal_command_accepts_cmd_alias() {
    let args = json!({ "cmd": "echo test" });
    let result = ZedAgent::parse_terminal_command(&args);
    assert!(result.is_ok());
    let cmd = result.unwrap();
    assert_eq!(cmd, vec!["echo", "test"]);
}

#[test]
fn parse_terminal_command_rejects_missing_command_field() {
    let args = json!({});
    let result = ZedAgent::parse_terminal_command(&args);
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err(),
        "command execution requires a 'command' field (string/array or indexed command.N entries)"
    );
}

#[test]
fn parse_terminal_command_accepts_indexed_arguments_zero_based() {
    let args = json!({ "command.0": "python", "command.1": "-c", "command.2": "print('hi')" });
    let result = ZedAgent::parse_terminal_command(&args);
    assert!(result.is_ok());
    let cmd = result.unwrap();
    assert_eq!(cmd, vec!["python", "-c", "print('hi')"]);
}

#[test]
fn parse_terminal_command_accepts_indexed_arguments_one_based() {
    let args = json!({ "command.1": "ls", "command.2": "-a" });
    let result = ZedAgent::parse_terminal_command(&args);
    assert!(result.is_ok());
    let cmd = result.unwrap();
    assert_eq!(cmd, vec!["ls", "-a"]);
}

#[test]
fn parse_terminal_command_rejects_non_string_indexed_argument() {
    let args = json!({ "command.0": 1 });
    let result = ZedAgent::parse_terminal_command(&args);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "command array must contain only strings");
}

#[tokio::test]
async fn resolve_terminal_working_dir_accepts_workdir_alias() {
    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;
    let args = json!({ "workdir": "src" });

    let working_dir = agent
        .resolve_terminal_working_dir(&args)
        .expect("workdir alias should resolve")
        .expect("working directory should be present");

    assert_eq!(working_dir, temp.path().join("src"));
}

#[tokio::test]
async fn read_only_primary_agents_hide_local_tools() {
    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;
    let enabled_tools: Vec<_> = agent
        .tool_availability(true, false)
        .into_iter()
        .filter_map(|(tool, runtime)| match runtime {
            ToolRuntime::Enabled => Some(tool),
            ToolRuntime::Disabled => None,
        })
        .collect();

    let duck_names = definition_names(agent.tool_definitions(true, &enabled_tools, "duck").unwrap());
    let plan_names = definition_names(agent.tool_definitions(true, &enabled_tools, "plan").unwrap());
    let build_names = definition_names(agent.tool_definitions(true, &enabled_tools, "build").unwrap());

    assert_eq!(duck_names, vec![tools::LIST_FILES.to_string()]);
    assert_eq!(plan_names, duck_names);
    let removed_tool = format!("switch_{}", "mode");
    assert!(!build_names.contains(&removed_tool));
    assert!(build_names.contains(&tools::LIST_FILES.to_string()));
    assert!(build_names.contains(&tools::EXEC_COMMAND.to_string()));
    assert!(build_names.contains(&tools::WRITE_STDIN.to_string()));
    assert!(build_names.contains(&tools::APPLY_PATCH.to_string()));
    assert!(!build_names.contains(&tools::CODE_SEARCH.to_string()));
}

#[tokio::test]
async fn advanced_global_profile_expands_acp_local_catalogue() {
    let temp = TempDir::new().unwrap();
    let tools_config = ToolsConfig {
        profile: ToolProfile::AdvancedVtCode,
        ..ToolsConfig::default()
    };
    let agent = build_agent_with_tools_config(temp.path(), tools_config).await;
    let local_names = definition_names(agent.acp_tool_registry.definitions_for(&[], true));

    assert!(local_names.contains(&tools::EXEC_COMMAND.to_string()));
    assert!(local_names.contains(&tools::WRITE_STDIN.to_string()));
    assert!(local_names.contains(&tools::APPLY_PATCH.to_string()));
    assert!(local_names.contains(&tools::CODE_SEARCH.to_string()));
}

#[tokio::test]
async fn custom_primary_agent_permissions_control_local_tools() {
    let temp = TempDir::new().unwrap();
    fs::create_dir_all(temp.path().join(".vtcode/agents")).unwrap();
    fs::write(
        temp.path().join(".vtcode/agents/sheller.md"),
        r#"---
name: sheller
description: Shell primary
mode: primary
permissions:
  default: deny
  allow:
    - exec_command
---
Shell prompt."#,
    )
    .unwrap();
    fs::write(
        temp.path().join(".vtcode/agents/reader.md"),
        r#"---
name: reader
description: Reader primary
mode: primary
permissions:
  default: deny
---
Reader prompt."#,
    )
    .unwrap();
    let agent = build_agent(temp.path()).await;
    let enabled_tools: Vec<_> = agent
        .tool_availability(true, false)
        .into_iter()
        .filter_map(|(tool, runtime)| match runtime {
            ToolRuntime::Enabled => Some(tool),
            ToolRuntime::Disabled => None,
        })
        .collect();

    let sheller_names = definition_names(agent.tool_definitions(true, &enabled_tools, "sheller").unwrap());
    let reader_names = definition_names(agent.tool_definitions(true, &enabled_tools, "reader").unwrap());

    assert!(sheller_names.contains(&tools::EXEC_COMMAND.to_string()));
    assert!(!sheller_names.contains(&tools::APPLY_PATCH.to_string()));
    assert!(!reader_names.contains(&tools::EXEC_COMMAND.to_string()));
    assert!(!reader_names.contains(&tools::APPLY_PATCH.to_string()));
}

#[tokio::test]
async fn allows_tool_gates_deny_default_agent_by_tool_category() {
    use vtcode_core::tools::names::canonical_tool_name;

    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;
    let local_names = definition_names(agent.acp_tool_registry.definitions_for(&[], true));
    assert!(!local_names.is_empty(), "expected at least one local tool definition");

    // A `default: deny` agent that only allows `exec_command` must expose
    // exec_command and nothing that maps to a different, unallowed category.
    // This exercises `allows_tool` for every local tool name so name drift that
    // would silently over-permit (an unrecognized name falling through to an
    // always-permitted `Other` request) is caught.
    let catalog =
        PrimaryAgentCatalog::from_specs_with_default(&[deny_default_agent_allowing("exec_command")], "sheller");
    let workspace = temp.path();

    for name in &local_names {
        let canonical = canonical_tool_name(name).to_ascii_lowercase();
        let allowed = catalog.allows_tool("sheller", &canonical, workspace);
        if canonical == tools::EXEC_COMMAND {
            assert!(allowed, "explicitly allowed exec_command must be permitted");
        } else if canonical == tools::CODE_SEARCH || canonical == tools::WRITE_STDIN {
            // Read-only search and session-stdin are intentionally category-Other
            // / read tools whose deny-default handling is asserted elsewhere; skip
            // to avoid over-constraining their policy here.
        } else {
            assert!(
                !allowed,
                "deny-default agent must not expose '{name}' (canonical '{canonical}') \
                 when only exec_command is allowed"
            );
        }
    }
}

fn deny_default_agent_allowing(tool: &str) -> vtcode_config::SubagentSpec {
    use vtcode_config::core::permissions::PermissionDefault;
    let mut spec = vtcode_config::builtin_primary_build_agent();
    spec.name = "sheller".to_string();
    spec.description = "Shell primary".to_string();
    spec.permissions.default = PermissionDefault::Deny;
    spec.permissions.allow = vec![tool.to_string()];
    spec.permissions.ask = Vec::new();
    spec.permissions.auto = Vec::new();
    spec.permissions.deny = Vec::new();
    spec
}

#[tokio::test]
async fn local_tool_execution_uses_registry_request_path() {
    let temp = TempDir::new().unwrap();
    fs::create_dir(temp.path().join("src")).unwrap();
    fs::write(temp.path().join("src").join("sample.txt"), "hello").unwrap();
    let agent = build_agent(temp.path()).await;

    let report = agent
        .execute_local_tool(
            tools::EXEC_COMMAND,
            &json!({
                "cmd": "printf sample.txt",
            }),
            "call-local-list",
        )
        .await;

    assert_eq!(report.status, crate::acp::ToolCallStatus::Completed);
    let payload = report.raw_output.expect("successful tool output");
    assert_eq!(payload["status"], "success");
    assert_eq!(payload["tool"], tools::EXEC_COMMAND);
    assert!(payload["result"].to_string().contains("sample.txt"));
}

#[tokio::test]
async fn local_tool_execution_reports_registry_failure() {
    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;

    let report = agent.execute_local_tool("unknown_tool", &json!({}), "call-local-missing").await;

    assert_eq!(report.status, crate::acp::ToolCallStatus::Failed);
    assert!(report.llm_response.contains("unknown_tool"));
}

#[tokio::test]
async fn local_tool_metadata_uses_core_labels_and_kinds() {
    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;
    let exec_args = json!({
        "cmd": "cargo check",
    });
    let search_args = json!({
        "query": "Widget",
        "path": "src/lib.rs",
    });
    let patch_args = json!({});

    assert_eq!(
        agent
            .acp_tool_registry
            .render_title(ToolDescriptor::Local, tools::EXEC_COMMAND, &exec_args),
        "Run command"
    );
    assert_eq!(
        agent
            .acp_tool_registry
            .tool_kind_for_call(tools::EXEC_COMMAND, Some(&exec_args)),
        crate::acp::ToolKind::Execute
    );
    assert_eq!(
        agent
            .acp_tool_registry
            .render_title(ToolDescriptor::Local, tools::CODE_SEARCH, &search_args),
        "Search code"
    );
    assert_eq!(
        agent
            .acp_tool_registry
            .tool_kind_for_call(tools::CODE_SEARCH, Some(&search_args)),
        crate::acp::ToolKind::Search
    );
    assert_eq!(
        agent
            .acp_tool_registry
            .render_title(ToolDescriptor::Local, tools::APPLY_PATCH, &patch_args),
        "Apply patch"
    );
    assert_eq!(
        agent
            .acp_tool_registry
            .tool_kind_for_call(tools::APPLY_PATCH, Some(&patch_args)),
        crate::acp::ToolKind::Edit
    );
}

#[tokio::test]
async fn resolved_messages_include_custom_primary_agent_prompt() {
    let temp = TempDir::new().unwrap();
    fs::create_dir_all(temp.path().join(".vtcode/agents")).unwrap();
    fs::write(
        temp.path().join(".vtcode/agents/research.md"),
        r#"---
name: research
description: Research primary
mode: primary
permissions:
  default: deny
---
Research primary prompt."#,
    )
    .unwrap();
    let agent = build_agent(temp.path()).await;
    let session_id = agent.register_session();
    let session = agent.session_handle(&session_id).unwrap();
    assert!(agent.update_session_primary_agent(&session, "research".to_string()));

    let messages = agent.resolved_messages(&session);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, MessageRole::System);
    assert_eq!(messages[0].content.as_text(), "Research primary prompt.");
}

#[tokio::test]
async fn resolved_messages_include_primary_agent_prompt() {
    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;
    let session_id = agent.register_session();
    let session = agent.session_handle(&session_id).unwrap();

    let messages = agent.resolved_messages(&session);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, MessageRole::System);
    let prompt = messages[0].content.as_text();
    assert!(prompt.contains("You are the duck agent"));
    assert!(!prompt.contains("Architect mode"));
    assert!(!prompt.contains("Code mode"));
}
