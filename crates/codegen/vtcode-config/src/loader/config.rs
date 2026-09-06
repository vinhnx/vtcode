use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::str::FromStr;

use crate::acp::AgentClientProtocolConfig;
use crate::codex::{FileOpener, HistoryConfig, TuiConfig};
use crate::constants::defaults::DEFAULT_PRIMARY_AGENT_NAME;
use crate::context::ContextFeaturesConfig;
use crate::core::{
    AgentConfig, AgentPermissionsConfig, AnthropicConfig, AuthConfig, AutomationConfig, CommandsConfig,
    CustomProviderConfig, DotfileProtectionConfig, ModelConfig, OpenAIConfig, PermissionsConfig, PromptCachingConfig,
    ProviderOverrideConfig, SandboxConfig, SecurityConfig, SkillsConfig, ToolsConfig,
};
use crate::debug::DebugConfig;
use crate::defaults::{self, ConfigDefaultsProvider};
use crate::hooks::HooksConfig;
use crate::ide_context::IdeContextConfig;
use crate::mcp::McpClientConfig;
use crate::models::{MiMoAuthMethod, Provider};
use crate::optimization::OptimizationConfig;
use crate::output_styles::OutputStyleConfig;
use crate::root::{ChatConfig, PtyConfig, UiConfig};
use crate::subagents::SubagentRuntimeLimits;
use crate::telemetry::TelemetryConfig;
use crate::timeouts::TimeoutsConfig;
use crate::webmcp::WebmcpConfig;

use crate::loader::syntax_highlighting::SyntaxHighlightingConfig;

/// Provider-specific configuration
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ProviderConfig {
    /// OpenAI provider configuration
    #[serde(default)]
    pub openai: OpenAIConfig,

    /// Anthropic provider configuration
    #[serde(default)]
    pub anthropic: AnthropicConfig,

    /// Xiaomi MiMo auth method: "payg" or "token-plan"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mimo_auth_method: Option<MiMoAuthMethod>,
}

/// Codex-compatible top-level feature flags.
///
/// Maps to `[features]` in `vtcode.toml`.
/// When `memories` is true, VT Code can carry useful context from earlier
/// threads into future work via the persistent memory subsystem.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct FeaturesConfig {
    /// Master toggle for the memories subsystem.
    /// When true, VT Code extracts durable context from completed threads
    /// and injects it into future sessions.
    #[serde(default)]
    pub memories: bool,
}

/// Workspace-level configuration controls.
///
/// Maps to `[workspace]` in `vtcode.toml`.
/// When `use_root_config` is true, only the workspace root `vtcode.toml`
/// is used as the active config layer (system, user, project, and
/// dot-dir layers are discarded).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct WorkspaceConfig {
    /// When true, force the workspace root `vtcode.toml` as the sole
    /// active config layer, discarding system, user, project, and
    /// dot-dir layers.
    #[serde(default)]
    pub(crate) use_root_config: bool,

    /// Include workspace context in messages.
    #[serde(default = "default_true")]
    pub(crate) include_context: bool,

    /// Maximum size of workspace context to include (in bytes).
    #[serde(default)]
    pub(crate) max_context_size: Option<usize>,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            use_root_config: false,
            include_context: true,
            max_context_size: None,
        }
    }
}

fn default_true() -> bool {
    true
}

/// Main configuration structure for VT Code
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VTCodeConfig {
    /// Primary agent selected at startup when no session override is active.
    #[serde(default = "default_primary_agent")]
    pub default_primary_agent: String,

    /// Codex-compatible top-level feature flags (`[features]` table).
    #[serde(default)]
    pub features: FeaturesConfig,

    /// Codex-compatible clickable citation URI scheme.
    #[serde(default)]
    pub file_opener: FileOpener,

    /// External notification command invoked for supported events.
    #[serde(default)]
    pub notify: Vec<String>,

    /// Codex-compatible local history persistence controls.
    #[serde(default)]
    pub history: HistoryConfig,

    /// Codex-compatible TUI settings.
    #[serde(default)]
    pub tui: TuiConfig,

    /// Agent-wide settings
    #[serde(default)]
    pub agent: AgentConfig,

    /// Authentication configuration for OAuth flows
    #[serde(default)]
    pub auth: AuthConfig,

    /// Tool execution policies
    #[serde(default)]
    pub tools: ToolsConfig,

    /// Unix command permissions
    #[serde(default)]
    pub commands: CommandsConfig,

    /// Permission system settings (resolution, audit logging, caching)
    #[serde(default)]
    pub permissions: PermissionsConfig,

    /// Runtime-only agent permission policy supplied by derived child agents.
    #[serde(skip)]
    #[cfg_attr(feature = "schema", schemars(skip))]
    pub runtime_agent_permissions: Option<AgentPermissionsConfig>,

    /// Runtime-only snapshot of lifecycle hook commands sourced from
    /// workspace-controlled configuration layers, populated by `ConfigManager`
    /// at load time. Workspace-controlled hooks require explicit user approval
    /// before execution; see [`WorkspaceLifecycleHooks`](crate::hooks::WorkspaceLifecycleHooks).
    #[serde(skip)]
    #[cfg_attr(feature = "schema", schemars(skip))]
    pub workspace_lifecycle_hooks: Option<crate::hooks::WorkspaceLifecycleHooks>,

    /// Security settings
    #[serde(default)]
    pub security: SecurityConfig,

    /// Sandbox settings for command execution isolation
    #[serde(default)]
    pub sandbox: SandboxConfig,

    /// UI settings
    #[serde(default)]
    pub ui: UiConfig,

    /// Chat settings
    #[serde(default)]
    pub chat: ChatConfig,

    /// PTY settings
    #[serde(default)]
    pub pty: PtyConfig,

    /// Debug and tracing settings
    #[serde(default)]
    pub debug: DebugConfig,

    /// Context features (e.g., Decision Ledger)
    #[serde(default)]
    pub context: ContextFeaturesConfig,

    /// Telemetry configuration (logging, trajectory)
    #[serde(default)]
    pub telemetry: TelemetryConfig,

    /// Performance optimization settings
    #[serde(default)]
    pub optimization: OptimizationConfig,

    /// Syntax highlighting configuration
    #[serde(default)]
    pub syntax_highlighting: SyntaxHighlightingConfig,

    /// Timeout ceilings and UI warning thresholds
    #[serde(default)]
    pub timeouts: TimeoutsConfig,

    /// Automation configuration
    #[serde(default)]
    pub automation: AutomationConfig,

    /// Subagent runtime configuration
    #[serde(default)]
    pub subagents: SubagentRuntimeLimits,

    /// Prompt cache configuration (local + provider integration)
    #[serde(default)]
    pub prompt_cache: PromptCachingConfig,

    /// Model Context Protocol configuration
    #[serde(default)]
    pub mcp: McpClientConfig,

    /// Authenticated browser editor bridge configuration
    #[serde(default)]
    pub webmcp: WebmcpConfig,

    /// Agent Client Protocol configuration
    #[serde(default)]
    pub acp: AgentClientProtocolConfig,

    /// IDE context configuration
    #[serde(default)]
    pub ide_context: IdeContextConfig,

    /// Lifecycle hooks configuration
    #[serde(default)]
    pub hooks: HooksConfig,

    /// Model-specific behavior configuration
    #[serde(default)]
    pub model: ModelConfig,

    /// Provider-specific configuration
    #[serde(default)]
    pub provider: ProviderConfig,

    /// Skills system configuration (Agent Skills spec)
    #[serde(default)]
    pub skills: SkillsConfig,

    /// User-defined OpenAI-compatible provider endpoints.
    /// These entries are editable in `/config` and appear in the model picker
    /// using each entry's `display_name`. Non-empty values from
    /// repository-controlled workspace/project layers are rejected; define
    /// provider endpoints in trusted system/user or explicitly selected config.
    #[serde(default)]
    pub custom_providers: Vec<CustomProviderConfig>,

    /// Built-in provider overrides for model lists and endpoint configuration.
    ///
    /// Maps provider key (e.g., "opencode-zen", "opencode-go") to override
    /// config that extends the provider's hardcoded model list with custom
    /// models, and optionally overrides the base URL or API key env var.
    #[serde(default)]
    pub provider_overrides: BTreeMap<String, ProviderOverrideConfig>,

    /// Restrict which providers may be used.
    ///
    /// When non-empty, only providers listed here are visible in the model
    /// picker, selectable at first-run, and instantiable at runtime.  Empty
    /// (the default) means all built-in and custom providers are available.
    #[serde(default)]
    pub providers_whitelist: Vec<String>,

    /// Output style configuration
    #[serde(default)]
    pub output_style: OutputStyleConfig,

    /// Dotfile protection configuration
    #[serde(default)]
    pub dotfile_protection: DotfileProtectionConfig,

    /// Workspace-level configuration controls
    #[serde(default)]
    pub workspace: WorkspaceConfig,
}

impl Default for VTCodeConfig {
    fn default() -> Self {
        Self {
            default_primary_agent: default_primary_agent(),
            features: FeaturesConfig::default(),
            file_opener: FileOpener::default(),
            notify: Vec::new(),
            history: HistoryConfig::default(),
            tui: TuiConfig::default(),
            agent: AgentConfig::default(),
            auth: AuthConfig::default(),
            tools: ToolsConfig::default(),
            commands: CommandsConfig::default(),
            permissions: PermissionsConfig::default(),
            runtime_agent_permissions: None,
            workspace_lifecycle_hooks: None,
            security: SecurityConfig::default(),
            sandbox: SandboxConfig::default(),
            ui: UiConfig::default(),
            chat: ChatConfig::default(),
            pty: PtyConfig::default(),
            debug: DebugConfig::default(),
            context: ContextFeaturesConfig::default(),
            telemetry: TelemetryConfig::default(),
            optimization: OptimizationConfig::default(),
            syntax_highlighting: SyntaxHighlightingConfig::default(),
            timeouts: TimeoutsConfig::default(),
            automation: AutomationConfig::default(),
            subagents: SubagentRuntimeLimits::default(),
            prompt_cache: PromptCachingConfig::default(),
            mcp: McpClientConfig::default(),
            webmcp: WebmcpConfig::default(),
            acp: AgentClientProtocolConfig::default(),
            ide_context: IdeContextConfig::default(),
            hooks: HooksConfig::default(),
            model: ModelConfig::default(),
            provider: ProviderConfig::default(),
            skills: SkillsConfig::default(),
            custom_providers: Vec::new(),
            provider_overrides: BTreeMap::new(),
            providers_whitelist: Vec::new(),
            output_style: OutputStyleConfig::default(),
            dotfile_protection: DotfileProtectionConfig::default(),
            workspace: WorkspaceConfig::default(),
        }
    }
}

impl VTCodeConfig {
    pub fn validate(&self) -> Result<()> {
        self.syntax_highlighting
            .validate()
            .context("Invalid syntax_highlighting configuration")?;

        self.context.validate().context("Invalid context configuration")?;

        self.hooks.validate().context("Invalid hooks configuration")?;

        self.timeouts.validate().context("Invalid timeouts configuration")?;

        self.prompt_cache.validate().context("Invalid prompt_cache configuration")?;

        self.webmcp.validate().context("Invalid webmcp configuration")?;

        self.agent
            .validate_llm_params()
            .map_err(anyhow::Error::msg)
            .context("Invalid agent configuration")?;

        self.ui
            .keyboard_protocol
            .validate()
            .context("Invalid keyboard_protocol configuration")?;

        self.pty.validate().context("Invalid pty configuration")?;

        // Validate custom providers
        let mut seen_names = std::collections::HashSet::new();
        for cp in &self.custom_providers {
            cp.validate()
                .map_err(|msg| anyhow::anyhow!(msg))
                .context("Invalid custom_providers configuration")?;
            if !seen_names.insert(cp.name.to_lowercase()) {
                anyhow::bail!("custom_providers: duplicate name `{}`", cp.name);
            }
        }

        // Validate provider overrides
        for (provider_name, override_config) in &self.provider_overrides {
            override_config
                .validate(provider_name)
                .map_err(|msg| anyhow::anyhow!(msg))
                .context("Invalid provider_overrides configuration")?;
            // Validate that the provider key matches a known provider
            if Provider::from_str(provider_name).is_err() {
                anyhow::bail!(
                    "provider_overrides: unknown provider `{provider_name}`; \
                     must be one of: gemini, openai, anthropic, copilot, deepseek, meta, \
                     openrouter, ollama, lmstudio, llamacpp, moonshot, zai, minimax, \
                     mimo, mistral, huggingface, opencodezen, opencodego, qwen, \
                     stepfun, evolink, poolside, nvidia, merge-gateway, vercel"
                );
            }
        }

        // Validate providers_whitelist entries
        for entry in &self.providers_whitelist {
            let known = Provider::from_str(entry).is_ok() || self.custom_providers.iter().any(|cp| cp.name == *entry);
            if !known {
                anyhow::bail!(
                    "providers_whitelist: unknown provider `{entry}`; \
                     must be a built-in provider or a name from custom_providers"
                );
            }
        }

        Ok(())
    }

    /// Returns true when the persistent memory subsystem is enabled.
    ///
    /// The top-level `features.memories` flag is the global master switch.
    /// `agent.persistent_memory.enabled` then enables repository-scoped storage.
    #[must_use]
    pub fn persistent_memory_enabled(&self) -> bool {
        self.features.memories && self.agent.persistent_memory.enabled
    }

    /// Returns true when the memories subsystem is enabled for injection.
    ///
    /// Memories are active when the subsystem is enabled and `memories.use_memories`
    /// is true.
    #[must_use]
    pub fn memories_enabled(&self) -> bool {
        self.persistent_memory_enabled() && self.agent.persistent_memory.memories.use_memories
    }

    /// Returns true when completed threads should be stored as memory inputs.
    #[must_use]
    pub fn should_generate_memories(&self) -> bool {
        self.persistent_memory_enabled() && self.agent.persistent_memory.memories.generate_memories
    }

    /// Look up a custom provider by its stable key.
    pub fn custom_provider(&self, name: &str) -> Option<&CustomProviderConfig> {
        let lower = name.to_lowercase();
        self.custom_providers.iter().find(|cp| cp.name.to_lowercase() == lower)
    }

    /// Return the explicitly configured credential key for a provider.
    ///
    /// Custom-provider identities take precedence over built-in provider
    /// overrides. A missing result means callers should use the provider's
    /// built-in default or the agent-wide fallback.
    pub fn configured_api_key_env(&self, name: &str) -> Option<String> {
        if let Some(custom_provider) = self.custom_provider(name) {
            return Some(custom_provider.resolved_api_key_env());
        }

        self.provider_overrides
            .iter()
            .find(|(provider, _)| provider.eq_ignore_ascii_case(name))
            .and_then(|(_, provider_override)| provider_override.api_key_env.clone())
            .filter(|api_key_env| !api_key_env.trim().is_empty())
    }

    /// Get the display name for any provider key, falling back to the raw key
    /// if no custom provider matches.
    pub fn provider_display_name(&self, provider_key: &str) -> String {
        if let Some(cp) = self.custom_provider(provider_key) {
            cp.display_name.clone()
        } else if provider_key.eq_ignore_ascii_case("codex") {
            "Codex".to_string()
        } else if let Ok(p) = FromStr::from_str(provider_key) {
            let p: Provider = p;
            p.label().to_string()
        } else {
            provider_key.to_string()
        }
    }

    #[cfg(feature = "bootstrap")]
    /// Bootstrap project with config + gitignore
    pub fn bootstrap_project<P: AsRef<Path>>(workspace: P, force: bool) -> Result<Vec<String>> {
        Self::bootstrap_project_with_options(workspace, force, false)
    }

    #[cfg(feature = "bootstrap")]
    /// Bootstrap project with config + gitignore, with option to create in home directory
    pub fn bootstrap_project_with_options<P: AsRef<Path>>(
        workspace: P,
        force: bool,
        use_home_dir: bool,
    ) -> Result<Vec<String>> {
        let workspace = workspace.as_ref().to_path_buf();
        defaults::with_config_defaults(|provider| {
            Self::bootstrap_project_with_provider(&workspace, force, use_home_dir, provider)
        })
    }

    #[cfg(feature = "bootstrap")]
    /// Bootstrap project files using the supplied [`ConfigDefaultsProvider`].
    fn bootstrap_project_with_provider<P: AsRef<Path>>(
        workspace: P,
        force: bool,
        use_home_dir: bool,
        defaults_provider: &dyn ConfigDefaultsProvider,
    ) -> Result<Vec<String>> {
        let workspace = workspace.as_ref();
        let config_file_name = defaults_provider.config_file_name().to_string();
        let (config_path, gitignore_path) = crate::loader::bootstrap::determine_bootstrap_targets(
            workspace,
            use_home_dir,
            &config_file_name,
            defaults_provider,
        )?;
        let vtcode_readme_path = workspace.join(".vtcode").join("README.md");
        let ast_grep_config_path = workspace.join("sgconfig.yml");
        let ast_grep_rule_path = workspace.join("rules").join("examples").join("no-console-log.yml");
        let ast_grep_test_path = workspace.join("rule-tests").join("examples").join("no-console-log-test.yml");
        let ast_grep_snapshot_path = workspace
            .join("rule-tests")
            .join("__snapshots__")
            .join("no-console-log-snapshot.yml");

        let home_targets = use_home_dir && config_path != workspace.join(&config_file_name);
        if home_targets {
            crate::loader::bootstrap::ensure_private_parent_dir(&config_path)?;
            crate::loader::bootstrap::ensure_private_parent_dir(&gitignore_path)?;
        } else {
            crate::loader::bootstrap::ensure_parent_dir(&config_path)?;
            crate::loader::bootstrap::ensure_parent_dir(&gitignore_path)?;
        }
        crate::loader::bootstrap::ensure_parent_dir(&vtcode_readme_path)?;
        crate::loader::bootstrap::ensure_parent_dir(&ast_grep_config_path)?;
        crate::loader::bootstrap::ensure_parent_dir(&ast_grep_rule_path)?;
        crate::loader::bootstrap::ensure_parent_dir(&ast_grep_test_path)?;
        crate::loader::bootstrap::ensure_parent_dir(&ast_grep_snapshot_path)?;

        let mut created_files = Vec::new();

        if !config_path.exists() || force {
            let config_content = Self::default_vtcode_toml_template();

            fs::write(&config_path, config_content)
                .with_context(|| format!("Failed to write config file: {}", config_path.display()))?;

            if let Some(file_name) = config_path.file_name().and_then(|name| name.to_str()) {
                created_files.push(file_name.to_string());
            }
        }

        if !gitignore_path.exists() || force {
            let gitignore_content = Self::default_vtcode_gitignore();
            fs::write(&gitignore_path, gitignore_content)
                .with_context(|| format!("Failed to write gitignore file: {}", gitignore_path.display()))?;

            if let Some(file_name) = gitignore_path.file_name().and_then(|name| name.to_str()) {
                created_files.push(file_name.to_string());
            }
        }

        if !vtcode_readme_path.exists() || force {
            let vtcode_readme = Self::default_vtcode_readme_template();
            fs::write(&vtcode_readme_path, vtcode_readme)
                .with_context(|| format!("Failed to write VT Code README: {}", vtcode_readme_path.display()))?;
            created_files.push(".vtcode/README.md".to_string());
        }

        let ast_grep_files = [
            (&ast_grep_config_path, Self::default_ast_grep_config_template(), "sgconfig.yml"),
            (
                &ast_grep_rule_path,
                Self::default_ast_grep_example_rule_template(),
                "rules/examples/no-console-log.yml",
            ),
            (
                &ast_grep_test_path,
                Self::default_ast_grep_example_test_template(),
                "rule-tests/examples/no-console-log-test.yml",
            ),
            (
                &ast_grep_snapshot_path,
                Self::default_ast_grep_example_snapshot_template(),
                "rule-tests/__snapshots__/no-console-log-snapshot.yml",
            ),
        ];

        for (path, contents, label) in ast_grep_files {
            if !path.exists() || force {
                fs::write(path, contents)
                    .with_context(|| format!("Failed to write ast-grep scaffold file: {}", path.display()))?;
                created_files.push(label.to_string());
            }
        }

        Ok(created_files)
    }

    #[cfg(feature = "bootstrap")]
    /// Generate the default `vtcode.toml` template used by bootstrap helpers.
    fn default_vtcode_toml_template() -> String {
        r#"# VT Code Configuration File (Example)
# Getting-started reference; see docs/config/CONFIGURATION_PRECEDENCE.md for override order.
# Copy this file to vtcode.toml and customize as needed.

# Clickable file citation URI scheme ("vscode", "cursor", "windsurf", "vscode-insiders", "none")
file_opener = "none"

# Workspace-level configuration
# [workspace]
# When true, force workspace root vtcode.toml as the sole active config layer
# (system, user, project, and dot-dir layers are discarded)
# use_root_config = false
# include_context = true
# max_context_size = 1048576  # 1MB

# Primary agent selected for new sessions when no explicit session selection is active
default_primary_agent = "build"

# Optional external command invoked after each completed agent turn
notify = []

# User-defined OpenAI-compatible providers
custom_providers = []

# [[custom_providers]]
# name = "mycorp"
# display_name = "MyCorporateName"
# base_url = "https://llm.corp.example/v1"
# api_key_env = "MYCORP_API_KEY"
# model = "gpt-5-mini"
# context_window = 256000

[history]
# Persist local session transcripts to disk
persistence = "file"

# Optional max size budget for each persisted session snapshot
# max_bytes = 104857600

[tui]
# Enable all built-in TUI notifications, disable them, or restrict to specific event types.
# notifications = true
# notifications = ["agent-turn-complete", "approval-requested"]

# Notification transport: "auto", "osc9", or "bel"
# notification_method = "auto"

# Set to false to reduce shimmer/animation effects
# animations = true

# Alternate-screen override: "always" or "never"
# alternate_screen = "never"

# Show onboarding hints on the welcome screen
# show_tooltips = true

# Core agent behavior; see docs/config/CONFIGURATION_PRECEDENCE.md.
[agent]
# Primary LLM provider to use (e.g., "openai", "gemini", "anthropic", "openrouter")
provider = "openai"

# Environment variable containing the API key for the provider
api_key_env = "OPENAI_API_KEY"

# Default model to use when no specific model is specified
default_model = "gpt-5.6-sol"

# Visual theme for the terminal interface
theme = "ciapre-dark"

# Enable TODO planning helper mode for structured task management
todo_planning_mode = true

# UI surface to use ("inline" by default; "auto" and "alternate" are also supported)
ui_surface = "inline"

# Maximum number of conversation turns before rotating context (affects memory usage)
# Lower values reduce memory footprint but may lose context; higher values preserve context but use more memory
max_conversation_turns = 150

# Reasoning effort level ("none", "minimal", "low", "medium", "high", "xhigh", "max") - affects model usage and response speed
reasoning_effort = "none"

# Temperature for main model responses (0.0-1.0)
temperature = 0.7

# Enable self-review loop to check and improve responses (increases API calls)
enable_self_review = false

# Maximum number of review passes when self-review is enabled
max_review_passes = 1

# Enable prompt refinement loop for improved prompt quality (increases processing time)
refine_prompts_enabled = false

# Maximum passes for prompt refinement when enabled
refine_prompts_max_passes = 1

# Optional alternate model for refinement (leave empty to use default)
refine_prompts_model = ""

# Maximum size of project documentation to include in context (in bytes)
project_doc_max_bytes = 16384

# Maximum size of instruction files to process (in bytes)
instruction_max_bytes = 16384

# List of additional instruction files to include in context
instruction_files = []

# Instruction files or globs to exclude from AGENTS/rules discovery
instruction_excludes = []

# Maximum recursive @import depth for instruction and rule files
instruction_import_max_depth = 5

# Durable per-repository memory for main sessions
[agent.persistent_memory]
enabled = false
auto_write = true
# directory_override = "/absolute/user-local/path"
# Startup scan budget for the compact memory summary
startup_line_limit = 200
startup_byte_limit = 25600

# Lightweight model helpers for lower-cost side tasks
[agent.small_model]
enabled = true
model = ""
temperature = 0.3
use_for_large_reads = true
use_for_web_summary = true
use_for_git_history = true
use_for_memory = true

# Inline prompt suggestions for the chat composer
[agent.prompt_suggestions]
# Enable Alt+P ghost-text suggestions in the composer
enabled = true

# Lightweight model for prompt suggestions (leave empty to auto-pick)
model = ""

# Lower values keep suggestions stable and completion-like
temperature = 0.3

# Show a one-time note that LLM-backed suggestions can consume tokens
show_cost_notice = true

# Onboarding configuration - Customize the startup experience
[agent.onboarding]
# Enable the onboarding welcome message on startup
enabled = true

# Custom introduction text shown on startup
intro_text = "Let's get oriented. I preloaded workspace context so we can move fast."

# Include project overview information in welcome
include_project_overview = true

# Include language summary information in welcome
include_language_summary = false

# Include key guideline highlights from AGENTS.md/CLAUDE.md
include_guideline_highlights = true

# Include usage tips in the welcome message
include_usage_tips_in_welcome = false

# Include recommended actions in the welcome message
include_recommended_actions_in_welcome = false

# Maximum number of guideline highlights to show
guideline_highlight_limit = 3

# List of usage tips shown during onboarding
usage_tips = [
    "Describe your current coding goal or ask for a quick status overview.",
    "Reference AGENTS.md/CLAUDE.md guidelines when proposing changes.",
    "Prefer asking for targeted file reads or diffs before editing.",
]

# List of recommended actions shown during onboarding
recommended_actions = [
    "Review the highlighted guidelines and share the task you want to tackle.",
    "Ask for a workspace tour if you need more context.",
]

# Checkpointing configuration for session persistence
[agent.checkpointing]
# Enable automatic session checkpointing
enabled = true

# Maximum number of checkpoints to keep on disk
max_snapshots = 50

# Maximum age of checkpoints to keep (in days)
max_age_days = 30

# Per-turn tool-call budget for the built-in agent harness
[agent.harness]
max_tool_calls_per_turn = 120

# Tool security configuration
[tools]
# Default policy when no specific policy is defined ("allow", "prompt", "deny")
# "allow" - Execute without confirmation
# "prompt" - Ask for confirmation
# "deny" - Block the tool
default_policy = "prompt"

# Maximum number of tool loops allowed per turn
# Set to 0 to disable the limit and let other turn safeguards govern termination.
max_tool_loops = 40

# Maximum number of repeated identical tool calls (prevents stuck loops)
max_repeated_tool_calls = 2

# Maximum consecutive blocked tool calls before force-breaking the turn
# Helps prevent high-CPU churn when calls are repeatedly denied/blocked
max_consecutive_blocked_tool_calls_per_turn = 8

# Maximum sequential spool-chunk reads per turn before nudging targeted extraction/summarization
max_sequential_spool_chunk_reads = 6

# Specific tool policies - Override default policy for individual tools
[tools.policies]
apply_patch = "prompt"            # Apply code patches (requires confirmation)
request_user_input = "allow"      # Ask focused user questions when the task requires it
task_tracker = "prompt"           # Create or update explicit task plans
exec_command = "prompt"           # Run commands; pipe-first by default, set tty=true for PTY/interactive sessions
write_stdin = "prompt"            # Send input to active command sessions
code_search = "allow"             # Advanced bounded literal search with query-led filters

# Command security - Define safe and dangerous command patterns
[commands]
# Commands that are always allowed without confirmation
allow_list = [
    "ls",           # List directory contents
    "pwd",          # Print working directory
    "git status",   # Show git status
    "git diff",     # Show git differences
    "cargo check",  # Check Rust code
    "echo",         # Print text
]

# Commands that are never allowed
deny_list = [
    "rm -rf /",        # Delete root directory (dangerous)
    "rm -rf ~",        # Delete home directory (dangerous)
    "shutdown",        # Shut down system (dangerous)
    "reboot",          # Reboot system (dangerous)
    "sudo *",          # Any sudo command (dangerous)
    ":(){ :|:& };:",   # Fork bomb (dangerous)
]

# Command patterns that are allowed (supports glob patterns)
allow_glob = [
    "git *",        # All git commands
    "cargo *",      # All cargo commands
    "python -m *",  # Python module commands
]

# Command patterns that are denied (supports glob patterns)
deny_glob = [
    "rm *",         # All rm commands
    "sudo *",       # All sudo commands
    "chmod *",      # All chmod commands
    "chown *",      # All chown commands
    "kubectl *",    # All kubectl commands (admin access)
]

# Regular expression patterns for allowed commands (if needed)
allow_regex = []

# Regular expression patterns for denied commands (if needed)
deny_regex = []

# Security configuration - Safety settings for automated operations
[security]
# Require human confirmation for potentially dangerous actions
human_in_the_loop = true

# Require explicit write tool usage for claims about file modifications
require_write_tool_for_claims = true

# Auto-apply patches without prompting (DANGEROUS - disable for safety)
auto_apply_detected_patches = false

# UI configuration - Terminal and display settings
[ui]
# Tool output display mode
# "compact" - Concise tool output
# "full" - Detailed tool output
tool_output_mode = "compact"

# Tool transition summary display mode
# "compact" - Render one compact summary per call and keep live PTY output bounded
# "expanded" - Render the expanded summary layout per call
tool_display_mode = "compact"

# Maximum number of lines to display in tool output (prevents transcript flooding)
# Lines beyond this limit are truncated to a tail preview
tool_output_max_lines = 600

# Maximum bytes threshold for spooling tool output to disk
# Output exceeding this size is written to .vtcode/tool-output/*.log
tool_output_spool_bytes = 200000

# Optional custom directory for spooled tool output logs
# If not set, defaults to .vtcode/tool-output/
# tool_output_spool_dir = "/path/to/custom/spool/dir"

# Allow ANSI escape sequences in tool output (enables colors but may cause layout issues)
allow_tool_ansi = false

# Number of rows to allocate for inline UI viewport
inline_viewport_rows = 16

# Show elapsed time divider after each completed turn
show_turn_timer = false

# Show warning/error/fatal diagnostic lines directly in transcript
# Effective in debug/development builds only
show_diagnostics_in_transcript = false

# Show timeline navigation panel
show_timeline_pane = false

# Runtime notification preferences
[ui.notifications]
# Master toggle for terminal/desktop notifications
enabled = true

# Delivery mode: "terminal", "hybrid", or "desktop"
delivery_mode = "desktop"

# Preferred desktop backend: "auto", "osascript", "notify_rust", or "terminal"
backend = "auto"

# Suppress notifications while terminal is focused
suppress_when_focused = true

# Failure/error notifications
command_failure = false
tool_failure = false
error = true

# Completion notifications
# Legacy master toggle (fallback for split settings when unset)
completion = true
completion_success = false
completion_failure = true

# Human approval/interaction notifications
hitl = true
policy_approval = true
request = false

# Success notifications for tool call results
tool_success = false

# Repeated notification suppression
repeat_window_seconds = 30
max_identical_in_window = 1

# Status line configuration
[ui.status_line]
# Status line mode ("auto", "command", "hidden")
mode = "auto"

# Show current system time (HH:MM:SS) on the status line
show_clock = true

# How often to refresh status line (milliseconds)
refresh_interval_ms = 2000

# Timeout for command execution in status line (milliseconds)
command_timeout_ms = 200

# Terminal title configuration
[ui.terminal_title]
# Ordered terminal title items (empty list disables VT Code-managed titles)
items = ["spinner", "project"]

# Enable Vim-style prompt editing in interactive mode
vim_mode = false

# PTY (Pseudo Terminal) configuration - For interactive command execution
[pty]
# Enable PTY support for interactive commands
enabled = true

# Default number of terminal rows for PTY sessions
default_rows = 24

# Default number of terminal columns for PTY sessions
default_cols = 80

# Maximum number of concurrent PTY sessions
max_sessions = 10

# Command timeout in seconds (prevents hanging commands)
command_timeout_seconds = 300

# Number of recent lines to show in PTY output
stdout_tail_lines = 20

# Total lines to keep in PTY scrollback buffer
scrollback_lines = 400

# Optional preferred shell for PTY sessions (falls back to $SHELL when unset)
# preferred_shell = "/bin/zsh"

# Route shell execution through zsh EXEC_WRAPPER intercept hooks (feature-gated)
shell_zsh_fork = false

# Absolute path to patched zsh used when shell_zsh_fork is enabled
# zsh_path = "/usr/local/bin/zsh"

# Context management configuration - Controls conversation memory
[context]
# Session prompt safety budget used by automatic compaction.
# Set to 0 to derive the effective budget from the resolved model/provider
# capacity; positive values remain an explicit session safety ceiling.
max_context_tokens = 0

# Percentage to trim context to when it gets too large
trim_to_percent = 60

# Number of recent conversation turns to always preserve
preserve_recent_turns = 6

# Decision ledger configuration - Track important decisions
[context.ledger]
# Enable decision tracking and persistence
enabled = true

# Maximum number of decisions to keep in ledger
max_entries = 12

# Include ledger summary in model prompts
include_in_prompt = true

# Preserve ledger during context compression
preserve_in_compression = true

# AI model routing - Intelligent model selection
# Telemetry and analytics
[telemetry]
# Enable trajectory logging for usage analysis
trajectory_enabled = true

# Syntax highlighting configuration
[syntax_highlighting]
# Enable syntax highlighting for code in tool output
enabled = true

# Theme for syntax highlighting
theme = "base16-ocean.dark"

# Cache syntax highlighting themes for performance
cache_themes = true

# Maximum file size for syntax highlighting (in MB)
max_file_size_mb = 10

# Programming languages to enable syntax highlighting for
enabled_languages = [
    "rust",
    "python",
    "javascript",
    "typescript",
    "go",
    "java",
    "bash",
    "sh",
    "shell",
    "zsh",
    "markdown",
    "md",
]

# Timeout for syntax highlighting operations (milliseconds)
highlight_timeout_ms = 1000

# Automation features - Full-auto permission review settings
[automation.full_auto]
# Enable full automation mode (DANGEROUS - requires careful oversight)
enabled = false

# Maximum number of turns before asking for human input
max_turns = 100

# Tools allowed in full automation mode
allowed_tools = [
    "write_file",
    "read_file",
    "list_files",
    "grep_file",
]

# Require profile acknowledgment before using full auto
require_profile_ack = true

# Path to full auto profile configuration
profile_path = "automation/full_auto_profile.toml"

[automation.scheduled_tasks]
# Enable cron tools and durable `vtcode schedule` jobs
enabled = false

# Prompt caching - Cache model responses for efficiency
[prompt_cache]
# Enable prompt caching (reduces API calls for repeated prompts)
enabled = true

# Maximum number of cache entries to keep
max_entries = 1000

# Maximum age of cache entries (in days)
max_age_days = 30

# Enable automatic cache cleanup
enable_auto_cleanup = true

# Minimum quality threshold to keep cache entries
min_quality_threshold = 0.7

# Keep volatile runtime counters at the end of system prompts to improve provider-side prefix cache reuse
# (enabled by default; disable only if you need legacy prompt layout)
cache_friendly_prompt_shaping = true

# Prompt cache configuration for OpenAI
    [prompt_cache.providers.openai]
    enabled = true
    min_prefix_tokens = 1024
    idle_expiration_seconds = 3600
    surface_metrics = true
    # Routing key strategy for OpenAI prompt cache locality.
    # "session" creates one stable key per VT Code conversation.
    prompt_cache_key_mode = "session"
    # Optional: server-side prompt cache retention for OpenAI Responses API
    # Supported values: "in_memory" or "24h" (leave commented out for default behavior)
    # prompt_cache_retention = "24h"

# Prompt cache configuration for Anthropic
[prompt_cache.providers.anthropic]
enabled = true
default_ttl_seconds = 300
extended_ttl_seconds = 3600
max_breakpoints = 4
cache_system_messages = true
cache_user_messages = true

# Prompt cache configuration for Gemini
[prompt_cache.providers.gemini]
enabled = true
mode = "implicit"
min_prefix_tokens = 1024
explicit_ttl_seconds = 3600

# Prompt cache configuration for OpenRouter
[prompt_cache.providers.openrouter]
enabled = true
propagate_provider_capabilities = true
report_savings = true

# Prompt cache configuration for Moonshot
[prompt_cache.providers.moonshot]
enabled = true

# Prompt cache configuration for DeepSeek
[prompt_cache.providers.deepseek]
enabled = true
surface_metrics = true

# Prompt cache configuration for Z.AI
[prompt_cache.providers.zai]
enabled = false

# Model Context Protocol (MCP) - Connect external tools and services
[mcp]
# Enable Model Context Protocol (may impact startup time if services unavailable)
enabled = true
max_concurrent_connections = 5
request_timeout_seconds = 30
retry_attempts = 3

# MCP UI configuration
[mcp.ui]
mode = "compact"
max_events = 50
show_provider_names = true

# MCP renderer profiles for different services
[mcp.ui.renderers]
sequential-thinking = "sequential-thinking"
context7 = "context7"

# MCP provider configuration - External services that connect via MCP
[[mcp.providers]]
name = "time"
command = "uvx"
args = ["mcp-server-time"]
enabled = true
max_concurrent_requests = 3
[mcp.providers.env]

# Agent Client Protocol (ACP) - IDE integration
[acp]
enabled = true

[acp.zed]
enabled = true
transport = "stdio"
# workspace_trust controls ACP trust mode: "tools_policy" (prompts) or "full_auto" (no prompts)
workspace_trust = "full_auto"

[acp.zed.tools]
read_file = true
list_files = true

# Cross-IDE editor context bridge
[ide_context]
enabled = true
inject_into_prompt = true
show_in_tui = true
include_selection_text = true
provider_mode = "auto"

[ide_context.providers.vscode_compatible]
enabled = true

[ide_context.providers.zed]
enabled = true

[ide_context.providers.generic]
enabled = true"#.to_string()
    }

    #[cfg(feature = "bootstrap")]
    fn default_vtcode_gitignore() -> String {
        r#"# Security-focused exclusions
.env, .env.local, secrets/, .aws/, .ssh/

# Development artifacts
target/, build/, dist/, node_modules/, vendor/

# Database files
*.db, *.sqlite, *.sqlite3

# Binary files
*.exe, *.dll, *.so, *.dylib, *.bin

# IDE files (comprehensive)
.vscode/, .idea/, *.swp, *.swo
"#
        .to_string()
    }

    #[cfg(feature = "bootstrap")]
    fn default_vtcode_readme_template() -> &'static str {
        "# VT Code Workspace Files\n\n- Put always-on repository guidance in `AGENTS.md`.\n- Put path-scoped prompt rules in `.vtcode/rules/*.md` using YAML frontmatter.\n- Keep authoring notes and other workspace docs outside `.vtcode/rules/` so they are not loaded into prompt memory.\n"
    }

    #[cfg(feature = "bootstrap")]
    fn default_ast_grep_config_template() -> &'static str {
        "ruleDirs:\n  - rules\nutilDirs:\n  - utils\ntestConfigs:\n  - testDir: rule-tests\n    snapshotDir: __snapshots__\n"
    }

    #[cfg(feature = "bootstrap")]
    fn default_ast_grep_example_rule_template() -> &'static str {
        "id: no-console-log\nlanguage: JavaScript\nseverity: error\nmessage: Avoid `console.log` in checked JavaScript files.\nnote: |\n  Avoid `console.log` in checked JavaScript files.\nrule:\n  pattern: console.log($$$ARGS)\nfiles:\n  - '**/*.js'\n"
    }

    #[cfg(feature = "bootstrap")]
    fn default_ast_grep_example_test_template() -> &'static str {
        "id: no-console-log\nvalid:\n  - |\n    const logger = {\n      info(message) {\n        return message;\n      },\n    };\ninvalid:\n  - |\n    function greet(name) {\n      console.log(name);\n    }\n"
    }

    #[cfg(feature = "bootstrap")]
    fn default_ast_grep_example_snapshot_template() -> &'static str {
        "id: no-console-log\nsnapshots:\n  ? |\n    function greet(name) {\n      console.log(name);\n    }\n  : labels:\n    - source: console.log(name)\n      style: primary\n      start: 25\n      end: 42\n"
    }

    #[cfg(feature = "bootstrap")]
    /// Create sample configuration file
    pub fn create_sample_config<P: AsRef<Path>>(output: P) -> Result<()> {
        let output = output.as_ref();
        let config_content = Self::default_vtcode_toml_template();

        fs::write(output, config_content)
            .with_context(|| format!("Failed to write config file: {}", output.display()))?;

        Ok(())
    }
}

fn default_primary_agent() -> String {
    DEFAULT_PRIMARY_AGENT_NAME.to_string()
}

#[cfg(test)]
mod tests {
    use super::VTCodeConfig;
    use crate::constants::tool_limits;
    use tempfile::tempdir;

    #[cfg(feature = "bootstrap")]
    #[test]
    fn sample_config_template_contains_release_loop_budgets() {
        let template = VTCodeConfig::default_vtcode_toml_template();
        let config: VTCodeConfig = toml::from_str(&template).expect("sample config template should parse");

        assert_eq!(config.agent.harness.max_tool_calls_per_turn, tool_limits::DEFAULT_MAX_TOOL_CALLS_PER_TURN);
        assert_eq!(config.tools.max_tool_loops, tool_limits::DEFAULT_MAX_TOOL_LOOPS);
        assert_eq!(config.automation.full_auto.max_turns, tool_limits::DEFAULT_FULL_AUTO_MAX_TURNS);
        assert_eq!(config.agent.max_conversation_turns, tool_limits::DEFAULT_MAX_CONVERSATION_TURNS);
    }

    #[cfg(feature = "bootstrap")]
    #[test]
    fn bootstrap_project_creates_vtcode_readme() {
        let workspace = tempdir().expect("workspace");
        let created =
            VTCodeConfig::bootstrap_project(workspace.path(), false).expect("bootstrap project should succeed");

        assert!(created.iter().any(|entry| entry == ".vtcode/README.md"), "created files: {created:?}");
        assert!(workspace.path().join(".vtcode/README.md").exists());
    }

    #[cfg(feature = "bootstrap")]
    #[test]
    fn bootstrap_project_creates_ast_grep_scaffold() {
        let workspace = tempdir().expect("workspace");
        let created =
            VTCodeConfig::bootstrap_project(workspace.path(), false).expect("bootstrap project should succeed");

        assert!(created.iter().any(|entry| entry == "sgconfig.yml"));
        assert!(created.iter().any(|entry| entry == "rules/examples/no-console-log.yml"));
        assert!(
            created
                .iter()
                .any(|entry| entry == "rule-tests/examples/no-console-log-test.yml")
        );
        assert!(
            created
                .iter()
                .any(|entry| entry == "rule-tests/__snapshots__/no-console-log-snapshot.yml")
        );

        assert!(workspace.path().join("sgconfig.yml").exists());
        assert!(workspace.path().join("rules/examples/no-console-log.yml").exists());
        assert!(workspace.path().join("rule-tests/examples/no-console-log-test.yml").exists());
        assert!(
            workspace
                .path()
                .join("rule-tests/__snapshots__/no-console-log-snapshot.yml")
                .exists()
        );
    }

    #[cfg(feature = "bootstrap")]
    #[test]
    fn bootstrap_project_preserves_existing_ast_grep_files_without_force() {
        let workspace = tempdir().expect("workspace");
        let sgconfig_path = workspace.path().join("sgconfig.yml");
        let rule_path = workspace.path().join("rules/examples/no-console-log.yml");

        std::fs::create_dir_all(workspace.path().join("rules/examples")).expect("create rules dir");
        std::fs::write(&sgconfig_path, "ruleDirs:\n  - custom-rules\n").expect("write sgconfig");
        std::fs::write(&rule_path, "id: custom-rule\n").expect("write rule");

        let created =
            VTCodeConfig::bootstrap_project(workspace.path(), false).expect("bootstrap project should succeed");

        assert!(!created.iter().any(|entry| entry == "sgconfig.yml"), "created files: {created:?}");
        assert!(
            !created.iter().any(|entry| entry == "rules/examples/no-console-log.yml"),
            "created files: {created:?}"
        );
        assert_eq!(std::fs::read_to_string(&sgconfig_path).expect("read sgconfig"), "ruleDirs:\n  - custom-rules\n");
        assert_eq!(std::fs::read_to_string(&rule_path).expect("read rule"), "id: custom-rule\n");
    }
}
