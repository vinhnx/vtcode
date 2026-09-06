use std::collections::BTreeSet;
use std::fmt::Write as _;

use crate::config::constants::tools;
use crate::config::types::{CapabilityLevel, ResolvedShellPromptProfile, ShellPromptProfile};
use crate::core::agent::harness_kernel::SessionToolCatalogSnapshot;
use crate::llm::provider::ToolDefinition;
use crate::prompts::sections::SectionBoundaryMode;
use crate::tools::registry::tool_groups;

const TOOL_EXEC_COMMAND: &str = tools::EXEC_COMMAND;
const TOOL_WRITE_STDIN: &str = tools::WRITE_STDIN;
const TOOL_CODE_SEARCH: &str = tools::CODE_SEARCH;
const TOOL_READ_FILE: &str = tools::READ_FILE;
const TOOL_LIST_FILES: &str = tools::LIST_FILES;
const TOOL_APPLY_PATCH: &str = tools::APPLY_PATCH;
const TOOL_REQUEST_USER_INPUT: &str = tools::REQUEST_USER_INPUT;
const TOOL_TASK_TRACKER: &str = tools::TASK_TRACKER;
const TOOL_START_PLANNING: &str = tools::START_PLANNING;

/// Documentation density is independent of the tools a session may execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolGuidanceProfile {
    Minimal,
    Default,
}

impl ToolGuidanceProfile {
    #[must_use]
    pub fn resolve(
        context_tokens: usize,
        default_prompt_tokens: usize,
        max_prompt_tokens: usize,
        input_usd_per_token: Option<f64>,
        max_budget_usd: Option<f64>,
    ) -> Self {
        let exceeds_cost = input_usd_per_token.zip(max_budget_usd).is_some_and(|(price, budget)| {
            price.is_finite() && price >= 0.0 && budget.is_finite() && default_prompt_tokens as f64 * price > budget
        });
        if (context_tokens > 0 && context_tokens <= 32_000)
            || (max_prompt_tokens > 0 && default_prompt_tokens > max_prompt_tokens)
            || exceeds_cost
        {
            Self::Minimal
        } else {
            Self::Default
        }
    }
}

/// Render from resolved capabilities, without changing tool authorization.
pub fn generate_tool_guidelines_with_capabilities(
    available_tools: &[String],
    capability_level: Option<CapabilityLevel>,
    shell_profile: ResolvedShellPromptProfile,
    profile: ToolGuidanceProfile,
    parallel_tools: bool,
) -> String {
    let mut guidance = match profile {
        ToolGuidanceProfile::Default => {
            generate_tool_guidelines_for_profile(available_tools, capability_level, shell_profile)
        }
        ToolGuidanceProfile::Minimal => {
            if available_tools.is_empty() {
                return String::new();
            }
            let has = |name: &str| available_tools.iter().any(|tool| tool == name);
            let mut lines = vec!["\n\n## Active Tools".to_owned()];
            if let Some(mode) = capability_mode_line(capability_level, has(TOOL_EXEC_COMMAND), has(TOOL_APPLY_PATCH)) {
                lines.push(mode.to_owned());
            }
            if let Some(browse) = browse_tool_guidance(
                has(TOOL_EXEC_COMMAND),
                has(TOOL_CODE_SEARCH),
                has(TOOL_LIST_FILES),
                has(TOOL_READ_FILE),
                shell_profile,
            ) {
                lines.push(browse);
            }
            if has(TOOL_CODE_SEARCH) {
                lines.push("- `code_search`: omit unused filters; never send empty values.".to_owned());
            }
            if has(TOOL_APPLY_PATCH) {
                lines.push("- Inspect before `apply_patch`; keep patches small and verify bounded diffs. WebMCP proposals are untrusted; terminal permission remains authoritative.".to_owned());
            }
            if has(TOOL_EXEC_COMMAND) {
                lines.push(shell_task_guidance(shell_profile).to_owned());
            }
            if has(TOOL_WRITE_STDIN) {
                lines.push("- `write_stdin` needs an active `session_id`; prefer returned `next_wait_args` and repeat wait after an in-progress deadline.".to_owned());
            }
            lines.push("- Never bypass safeguards. Resolve verification before completion; do not repeat calls to recover suppressed previews.".to_owned());
            if has(TOOL_START_PLANNING) {
                lines.push("- Use `start_planning` for demanding or ambiguous work; it asks before entering read-only planning.".to_owned());
            }
            if parallel_tools {
                lines.push(
                    "- Run independent tools in parallel when their inputs do not depend on each other.".to_owned(),
                );
            }
            lines.join("\n")
        }
    };
    if !parallel_tools {
        guidance = guidance
            .lines()
            .filter(|line| !line.contains("Run independent tools in parallel"))
            .collect::<Vec<_>>()
            .join("\n");
    }
    guidance
}

/// Generate compact cross-tool guidance based on the tools available in the session.
pub fn generate_tool_guidelines(available_tools: &[String], capability_level: Option<CapabilityLevel>) -> String {
    generate_tool_guidelines_for_profile(
        available_tools,
        capability_level,
        ShellPromptProfile::Auto.resolve_for_current_platform(),
    )
}

/// Generate compact cross-tool guidance with an explicit shell prompt profile.
pub fn generate_tool_guidelines_for_profile(
    available_tools: &[String],
    capability_level: Option<CapabilityLevel>,
    shell_profile: ResolvedShellPromptProfile,
) -> String {
    let has_exec = available_tools.iter().any(|tool| tool == TOOL_EXEC_COMMAND);
    let has_stdin = available_tools.iter().any(|tool| tool == TOOL_WRITE_STDIN);
    let has_search = available_tools.iter().any(|tool| tool == TOOL_CODE_SEARCH);
    let has_read_file = available_tools.iter().any(|tool| tool == TOOL_READ_FILE);
    let has_list_files = available_tools.iter().any(|tool| tool == TOOL_LIST_FILES);
    let has_apply_patch = available_tools.iter().any(|tool| tool == TOOL_APPLY_PATCH);
    let has_start_planning = available_tools.iter().any(|tool| tool == TOOL_START_PLANNING);

    let mut lines = Vec::new();
    if let Some(mode_line) = capability_mode_line(capability_level, has_exec, has_apply_patch) {
        lines.push(mode_line.to_string());
    }
    if let Some(browse_guidance) =
        browse_tool_guidance(has_exec, has_search, has_list_files, has_read_file, shell_profile)
    {
        lines.push(browse_guidance);
    }
    if has_search || has_read_file || has_list_files {
        lines.push(read_only_batching_guidance(has_read_file).to_string());
    }
    if has_apply_patch {
        lines.push("- Use `apply_patch` for file edits after inspection; keep patches small.".to_string());
        lines.push(
            "- Verify bounded diffs; WebMCP edits are untrusted proposals; terminal permission stays authoritative."
                .to_string(),
        );
    }
    if has_exec {
        lines.push(shell_task_guidance(shell_profile).to_string());
    }
    // "Diagnose from evidence; never bypass safeguards" and the
    // completion-as-checkpoint line are already stated unconditionally in the
    // Runtime Guidance / operating-profile sections; repeating them here
    // wastes prompt budget.
    if has_stdin {
        lines.push("- `write_stdin`: reuse the existing `session_id` of an active exec session; prefer the pre-filled `next_wait_args` over `next_continue_args` polling; `spool_complete: false` marks readable partial output; an exited pending spool arrives on a later wait.".to_string());
    }
    if has_search {
        lines.push("- `code_search`: omit unused filters; no empty values (`path: \"\"`).".to_string());
        lines.push(code_search_guidance(has_exec, shell_profile));
    }
    if has_apply_patch || has_exec {
        lines.push("- On `preview_budget_exhausted`, trust the preserved outcome metadata; do not repeat the call. Run one verifier (`&&` chain, no pipes), then synthesize.".to_string());
    }
    if has_search || has_exec {
        lines.push("- Run independent tools in parallel when inputs do not depend on each other.".to_string());
    }
    if has_start_planning {
        lines.push(
            "- For demanding, ambiguous, or multi-phase tasks, call `start_planning` to ask the user before entering the read-only Planning workflow; do not use it for straightforward changes.".to_string(),
        );
    }

    if lines.is_empty() {
        return String::new();
    }

    format!("\n\n## Active Tools\n{}", lines.join("\n"))
}

pub fn append_runtime_tool_prompt_sections(
    prompt: &mut String,
    tool_snapshot: &SessionToolCatalogSnapshot,
    include_catalog_metadata: bool,
) {
    append_runtime_tool_prompt_sections_for_profile(
        prompt,
        tool_snapshot,
        include_catalog_metadata,
        ShellPromptProfile::Auto.resolve_for_current_platform(),
    );
}

pub fn append_runtime_tool_prompt_sections_for_profile(
    prompt: &mut String,
    tool_snapshot: &SessionToolCatalogSnapshot,
    include_catalog_metadata: bool,
    shell_profile: ResolvedShellPromptProfile,
) {
    remove_prompt_section(prompt, "## Active Tools");
    remove_prompt_section(prompt, "[Runtime Tool Catalog]");
    while prompt.ends_with('\n') {
        prompt.pop();
    }

    let available_tools = snapshot_tool_names(tool_snapshot);
    let guidelines =
        generate_runtime_tool_guidelines_for_profile(&available_tools, tool_snapshot.planning_active, shell_profile);
    if !guidelines.is_empty() {
        append_prompt_block(prompt, guidelines.trim_start_matches('\n'));
    }

    if include_catalog_metadata && tool_snapshot.snapshot.is_some() {
        let active_tools = if tool_snapshot.active_tool_names.is_empty() {
            "none".to_string()
        } else {
            tool_snapshot.active_tool_names.join(", ")
        };
        let catalog_metadata = format!(
            "[Runtime Tool Catalog]\n- version: {}\n- epoch: {}\n- catalog_tools: {}\n- available_tools: {}\n- currently_available_tools: {}\n- request_user_input_enabled: {}",
            tool_snapshot.version,
            tool_snapshot.epoch,
            tool_snapshot.catalog_tools(),
            tool_snapshot.available_tools(),
            active_tools,
            tool_snapshot.request_user_input_enabled,
        );
        append_prompt_block(prompt, &catalog_metadata);
    }
}

/// Select documentation density using the active route and session budgets.
pub fn append_runtime_tool_prompt_sections_for_model(
    prompt: &mut String,
    tool_snapshot: &SessionToolCatalogSnapshot,
    include_catalog_metadata: bool,
    shell_profile: ResolvedShellPromptProfile,
    provider: &dyn crate::llm::provider::LLMProvider,
    model: &str,
    config: Option<&crate::config::VTCodeConfig>,
) {
    append_runtime_tool_prompt_sections_for_profile(prompt, tool_snapshot, include_catalog_metadata, shell_profile);
    let pricing = crate::config::models::model_catalog_entry(provider.name(), model).map(|entry| entry.pricing);
    let profile = ToolGuidanceProfile::resolve(
        crate::compaction::effective_context_budget(config, provider, model),
        prompt.len().div_ceil(4),
        config.map_or(0, |cfg| cfg.agent.max_system_prompt_tokens as usize),
        pricing.and_then(|price| price.input),
        config.and_then(|cfg| cfg.agent.harness.max_budget_usd),
    );
    let parallel_tools = provider.supports_parallel_tool_config(model);
    // The detailed planning contract contains no parallel-call hint and remains
    // intact in Default. Minimal retains the same read-only and output contract.
    if tool_snapshot.planning_active && profile == ToolGuidanceProfile::Default {
        return;
    }
    remove_prompt_section(prompt, "## Active Tools");
    let names = snapshot_tool_names(tool_snapshot);
    let capability_level = Some(infer_capability_level(&names));
    let mut guidance =
        generate_tool_guidelines_with_capabilities(&names, capability_level, shell_profile, profile, parallel_tools);
    if tool_snapshot.planning_active {
        guidance.push_str("\n- Planning is read-only. Stop research when the plan is specified or the budget is near; emit one `<proposed_plan>` block with concrete targets and verification for each step.");
        if names.iter().any(|name| name == TOOL_TASK_TRACKER) {
            guidance.push_str("\n- Keep blockers and verification open in `task_tracker`; updates use positive indices or index_path, with index 0 reserved for checklist completion.");
        }
        if names.iter().any(|name| name == TOOL_REQUEST_USER_INPUT) {
            guidance.push_str(
                "\n- Use `request_user_input` only for material blockers remaining after repository exploration.",
            );
        }
    }
    append_prompt_block(prompt, guidance.trim_start_matches('\n'));
}

/// Append a compact summary of tools omitted from a client-local wire payload.
pub fn append_deferred_tools_prompt_section(prompt: &mut String, tools: &[ToolDefinition]) {
    remove_prompt_section(prompt, "[Deferred Tools]");

    let mut lines: Vec<String> = tool_groups(tools)
        .into_iter()
        .filter(|group| group.deferred_count > 0)
        .map(|group| {
            format!("- {} ({} tools): {}", group.name, group.deferred_count, group.description.unwrap_or_default())
        })
        .collect();

    let unnamespaced_deferred = tools
        .iter()
        .filter(|tool| tool.namespace.is_none() && tool.defer_loading == Some(true))
        .count();
    if unnamespaced_deferred > 0 {
        lines.push(format!("- {unnamespaced_deferred} additional deferred tools"));
    }

    if lines.is_empty() {
        return;
    }

    let section = format!(
        "[Deferred Tools]\n{}\nUse `search_tools` to find a deferred capability. Selected definitions become available in the next request segment.",
        lines.join("\n")
    );
    append_prompt_block(prompt, &section);
}

fn append_prompt_block(prompt: &mut String, block: &str) {
    if block.is_empty() {
        return;
    }

    if prompt.is_empty() {
        prompt.push_str(block);
    } else {
        let _ = write!(prompt, "\n\n{block}");
    }
}

fn remove_prompt_section(prompt: &mut String, section_header: &str) {
    while let Some((section_start, section_end)) = find_prompt_section_bounds(prompt, section_header) {
        prompt.replace_range(section_start..section_end, "");
    }
}

fn find_prompt_section_bounds(prompt: &str, section_header: &str) -> Option<(usize, usize)> {
    crate::prompts::sections::find_prompt_section_bounds(prompt, section_header, SectionBoundaryMode::BracketOrMarkdown)
}

fn generate_runtime_tool_guidelines_for_profile(
    available_tools: &[String],
    planning_active: bool,
    shell_profile: ResolvedShellPromptProfile,
) -> String {
    if !planning_active {
        return generate_tool_guidelines_for_profile(available_tools, None, shell_profile);
    }

    let has_exec = available_tools.iter().any(|tool| tool == TOOL_EXEC_COMMAND);
    let has_search = available_tools.iter().any(|tool| tool == TOOL_CODE_SEARCH);
    let has_read_file = available_tools.iter().any(|tool| tool == TOOL_READ_FILE);
    let has_list_files = available_tools.iter().any(|tool| tool == TOOL_LIST_FILES);
    let has_request_user_input = available_tools.iter().any(|tool| tool == TOOL_REQUEST_USER_INPUT);
    let has_task_tracker = available_tools.iter().any(|tool| matches!(tool.as_str(), TOOL_TASK_TRACKER));

    let mut lines = vec!["- Planning workflow active: stay within the read-safe tool list.".to_string()];
    lines.push("- Monitor the available planning tool-loop budget; stop research when the plan is specified or the limit is near, then synthesize one compact decision-ready plan from existing evidence.".to_string());
    lines.push("- Every implementation step in the final plan must name a concrete repository target and include a concrete verification command or observable check.".to_string());
    lines.push("- When the plan is ready, emit only one `<proposed_plan>` block; do not repeat planning policy text or add surrounding prose.".to_string());
    if let Some(browse_guidance) =
        browse_tool_guidance(has_exec, has_search, has_list_files, has_read_file, shell_profile)
    {
        lines.push(browse_guidance);
    }
    if has_exec {
        lines.push("- In Planning workflow, use `exec_command` only for read-only verification.".to_string());
    }
    if has_search {
        lines.push("- `code_search`: omit unused filters; no empty values (`path: \"\"`).".to_string());
    }
    if has_task_tracker {
        lines.push("- Keep `task_tracker` updated as you refine the plan.".to_string());
        lines.push("- Keep blockers and verification open in `task_tracker` until resolved.".to_string());
        lines.push("- Use `task_tracker` action=update with positive flat indices or positive hierarchical index_path values; index: 0 is only for standard checklist-level completion with status=completed, and bulk updates use items.".to_string());
    }
    if has_request_user_input {
        lines.push(
            "- Use `request_user_input` only for material blockers that remain after repository exploration."
                .to_string(),
        );
    }
    if has_search || has_exec {
        lines.push("- If calls repeat without progress, tighten the plan instead of retrying identically.".to_string());
    }

    format!("\n\n## Active Tools\n{}", lines.join("\n"))
}

fn snapshot_tool_names(tool_snapshot: &SessionToolCatalogSnapshot) -> Vec<String> {
    tool_snapshot
        .active_tool_names
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn browse_tool_guidance(
    has_exec: bool,
    has_search: bool,
    has_list_files: bool,
    has_read_file: bool,
    shell_profile: ResolvedShellPromptProfile,
) -> Option<String> {
    if has_exec {
        return Some(shell_browse_guidance(shell_profile).to_string());
    }

    if !(has_search || has_list_files || has_read_file) {
        return None;
    }

    Some("- Use available read-only repository tools for browsing; do not modify files.".to_string())
}

pub fn render_shell_profile_guidance(shell_profile: ResolvedShellPromptProfile) -> String {
    match shell_profile {
        ResolvedShellPromptProfile::UnixLike => {
            "## Shell Profile\n- Active shell profile: `unix_like`. Use Unix-like command syntax in `exec_command.cmd`, for example `ls`, `rg`, `find`, `cat`, `sed`, and `awk`.\n- On macOS, write BSD-compatible flags for BSD tools. VT Code does not rewrite GNU flags for macOS BSD tools.\n- The shell profile controls prompt examples and expected command syntax only; command policy, sandboxing, and approvals remain separate runtime checks.\n- VT Code does not translate GNU-to-BSD, BSD-to-GNU, Unix-to-PowerShell, or PowerShell-to-Unix command flags.".to_string()
        }
        ResolvedShellPromptProfile::PowerShell => {
            "## Shell Profile\n- Active shell profile: `powershell`. Use native PowerShell syntax in `exec_command.cmd`, for example `Get-ChildItem`, `Select-String`, `Get-Content`, and `Where-Object`.\n- On native Windows, use WSL when you need Unix-like workflows or Unix command examples.\n- The shell profile controls prompt examples and expected command syntax only; command policy, sandboxing, and approvals remain separate runtime checks.\n- VT Code does not translate GNU-to-BSD, BSD-to-GNU, Unix-to-PowerShell, or PowerShell-to-Unix command flags.".to_string()
        }
    }
}

fn shell_browse_guidance(shell_profile: ResolvedShellPromptProfile) -> &'static str {
    match shell_profile {
        ResolvedShellPromptProfile::UnixLike => {
            "- Use `exec_command.cmd` with `ls`, `rg`, `find`, `cat`, `sed`, and `awk` for repository browsing."
        }
        ResolvedShellPromptProfile::PowerShell => {
            "- Use `exec_command.cmd` with native PowerShell commands such as `Get-ChildItem`, `Select-String`, `Get-Content`, and `Where-Object` for repository browsing."
        }
    }
}

fn shell_task_guidance(shell_profile: ResolvedShellPromptProfile) -> &'static str {
    match shell_profile {
        ResolvedShellPromptProfile::UnixLike => {
            "- Use `exec_command.cmd` for build tools, test tools, `git diff -- <path>`, and shell-only tasks. In one-shot `exec_command` calls, do not use `!!`, `!$`, `!ssh`, or `fc`; write full command arguments explicitly from conversation or tool results. Interactive shells: review-safe history expansion (Bash `histverify`, zsh `HIST_VERIFY`)."
        }
        ResolvedShellPromptProfile::PowerShell => {
            "- Use `exec_command.cmd` for build tools, test tools, `git diff -- <path>`, and shell-only tasks using native PowerShell syntax."
        }
    }
}

fn read_only_batching_guidance(has_read_file: bool) -> &'static str {
    if has_read_file {
        "- Batch independent read-only calls; use bounded `read_file` ranges, order dependencies, serialize mutations; narrow the range on `line_truncated`."
    } else {
        "- Batch independent read-only calls; order dependent reads, and serialize mutations."
    }
}

fn code_search_guidance(has_exec: bool, _shell_profile: ResolvedShellPromptProfile) -> String {
    const BASE: &str = "- Advanced `code_search` takes `query`; filters `path`, `file_types`, `result_types`, `max_results`; results: definitions, exact syntactic usages. Queries use literal smart-case and `|`-separated literals; truncated: narrow. Example: `{\"query\":\"TurnLoop\",\"path\":\"src\",\"result_types\":[\"definition\"]}`. Do not JSON-encode arrays or integers as strings.";
    if has_exec {
        format!("{BASE} Use `exec_command` or a skill for syntax patterns.")
    } else {
        BASE.to_string()
    }
}

fn capability_mode_line(
    capability_level: Option<CapabilityLevel>,
    has_exec: bool,
    has_file: bool,
) -> Option<&'static str> {
    match capability_level {
        Some(CapabilityLevel::Basic) => {
            Some("- Capabilities: limited. Ask the user to enable more capabilities if file work is required.")
        }
        Some(CapabilityLevel::FileReading | CapabilityLevel::FileListing) => {
            Some("- Capabilities: read-only. Analyze and search, but do not modify files or run shell commands.")
        }
        _ if !has_exec && !has_file => {
            Some("- Capabilities: read-only. Analyze and search, but do not modify files or run shell commands.")
        }
        _ => None,
    }
}

/// Infer capability level from available tools.
pub fn infer_capability_level(available_tools: &[String]) -> CapabilityLevel {
    let has_search = available_tools.iter().any(|t| t == TOOL_CODE_SEARCH);
    let has_edit = available_tools.iter().any(|t| t == TOOL_APPLY_PATCH);
    let has_read = has_edit || available_tools.iter().any(|t| t == TOOL_READ_FILE);
    let has_list = has_search || available_tools.iter().any(|t| t == TOOL_LIST_FILES);
    let has_exec = available_tools.iter().any(|t| t == TOOL_EXEC_COMMAND);

    if has_search {
        CapabilityLevel::CodeSearch
    } else if has_edit {
        CapabilityLevel::Editing
    } else if has_exec {
        CapabilityLevel::Bash
    } else if has_list {
        CapabilityLevel::FileListing
    } else if has_read {
        CapabilityLevel::FileReading
    } else {
        CapabilityLevel::Basic
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documentation_profile_respects_context_tokens_and_known_cost() {
        assert_eq!(ToolGuidanceProfile::resolve(32_000, 100, 1000, None, None), ToolGuidanceProfile::Minimal);
        assert_eq!(ToolGuidanceProfile::resolve(1_000_000, 100, 1000, None, Some(0.0)), ToolGuidanceProfile::Default);
        assert_eq!(ToolGuidanceProfile::resolve(1_000_000, 1001, 1000, None, None), ToolGuidanceProfile::Minimal);
        assert_eq!(
            ToolGuidanceProfile::resolve(1_000_000, 1000, 2000, Some(0.00001), Some(0.001)),
            ToolGuidanceProfile::Minimal
        );
    }

    #[test]
    fn minimal_and_default_tool_guidance_snapshots() {
        let tools = vec![TOOL_READ_FILE.to_owned()];
        let minimal = generate_tool_guidelines_with_capabilities(
            &tools,
            None,
            ResolvedShellPromptProfile::UnixLike,
            ToolGuidanceProfile::Minimal,
            false,
        );
        assert_eq!(
            minimal,
            "\n\n## Active Tools\n- Capabilities: read-only. Analyze and search, but do not modify files or run shell commands.\n- Use available read-only repository tools for browsing; do not modify files.\n- Never bypass safeguards. Resolve verification before completion; do not repeat calls to recover suppressed previews."
        );
        let default = generate_tool_guidelines_with_capabilities(
            &tools,
            None,
            ResolvedShellPromptProfile::UnixLike,
            ToolGuidanceProfile::Default,
            false,
        );
        assert_eq!(
            default,
            "\n\n## Active Tools\n- Capabilities: read-only. Analyze and search, but do not modify files or run shell commands.\n- Use available read-only repository tools for browsing; do not modify files.\n- Batch independent read-only calls; use bounded `read_file` ranges, order dependencies, serialize mutations; narrow the range on `line_truncated`."
        );
    }

    #[test]
    fn tool_guidance_uses_actual_parallel_capabilities_for_three_families() {
        use crate::config::constants::models;
        use crate::llm::provider::LLMProvider;
        use crate::llm::providers::{AnthropicProvider, GeminiProvider, OpenAIProvider};
        let providers: [(Box<dyn LLMProvider>, &str); 3] = [
            (Box::new(OpenAIProvider::new("offline-fixture".into())), models::openai::DEFAULT_MODEL),
            (Box::new(AnthropicProvider::new("offline-fixture".into())), models::anthropic::DEFAULT_MODEL),
            (Box::new(GeminiProvider::new("offline-fixture".into())), models::google::DEFAULT_MODEL),
        ];
        for (provider, model) in providers {
            for profile in [ToolGuidanceProfile::Minimal, ToolGuidanceProfile::Default] {
                let parallel = provider.supports_parallel_tool_config(model);
                let text = generate_tool_guidelines_with_capabilities(
                    &[TOOL_EXEC_COMMAND.to_owned()],
                    None,
                    ResolvedShellPromptProfile::UnixLike,
                    profile,
                    parallel,
                );
                assert_eq!(text.contains("tools in parallel"), parallel);
                let serial = generate_tool_guidelines_with_capabilities(
                    &[TOOL_EXEC_COMMAND.to_owned()],
                    None,
                    ResolvedShellPromptProfile::UnixLike,
                    profile,
                    false,
                );
                assert!(!serial.contains("tools in parallel"));
            }
        }
    }

    #[test]
    fn test_read_only_capability_detection() {
        let tools = vec![TOOL_CODE_SEARCH.to_string()];
        let guidelines = generate_tool_guidelines(&tools, None);
        assert!(guidelines.contains("Capabilities: read-only"));
        assert!(guidelines.contains("do not modify files"));
    }

    #[test]
    fn test_tool_preference_guidance() {
        let tools = vec![TOOL_EXEC_COMMAND.to_string(), TOOL_CODE_SEARCH.to_string()];
        let guidelines = generate_tool_guidelines_for_profile(&tools, None, ResolvedShellPromptProfile::UnixLike);
        assert!(guidelines.contains("Advanced `code_search` takes `query`"));
        assert!(guidelines.contains("literal smart-case"));
        assert!(guidelines.contains("exact syntactic usages"));
        assert!(guidelines.contains("\"result_types\":[\"definition\"]"));
        assert!(guidelines.contains("Do not JSON-encode arrays or integers as strings"));
        assert!(guidelines.contains("omit unused filters"));
        assert!(guidelines.contains("path: \"\""));
        assert!(guidelines.contains("git diff -- <path>"));
        assert!(guidelines.contains("build tools"));
        assert!(guidelines.contains("test tools"));
        // Completion-as-checkpoint guidance lives in the operating profiles;
        // the guidelines section no longer repeats it.
        assert!(!guidelines.contains("Completion is a checkpoint"));
    }

    #[test]
    fn test_edit_workflow_guidance() {
        let tools = vec![TOOL_APPLY_PATCH.to_string()];
        let guidelines = generate_tool_guidelines(&tools, None);
        assert!(guidelines.contains("Use `apply_patch`"));
        assert!(guidelines.contains("patches small"));
        // Completion-as-checkpoint guidance lives in the operating profiles;
        // the guidelines section no longer repeats it.
        assert!(!guidelines.contains("verification resolved"));
    }

    #[test]
    fn test_vt_code_guidance_omits_task_tracker() {
        let tools = vec![
            TOOL_EXEC_COMMAND.to_string(),
            TOOL_WRITE_STDIN.to_string(),
            TOOL_APPLY_PATCH.to_string(),
        ];
        let guidelines = generate_tool_guidelines_for_profile(&tools, None, ResolvedShellPromptProfile::UnixLike);

        assert!(guidelines.contains("exec_command.cmd"));
        for command in ["ls", "rg", "find", "cat", "sed", "awk"] {
            assert!(
                guidelines.contains(&format!("`{command}`")),
                "{command} should be shown as an exec_command.cmd example"
            );
        }
        assert!(guidelines.contains("`write_stdin`"));
        assert!(guidelines.contains("`apply_patch`"));
        assert!(!guidelines.contains("task_tracker"));
        assert!(!guidelines.contains("list_files"));
        assert!(!guidelines.contains("read_file"));
    }

    #[test]
    fn task_tracker_guidance_explains_action_aware_indices() {
        let guidelines = generate_runtime_tool_guidelines_for_profile(
            &[TOOL_TASK_TRACKER.to_string()],
            true,
            ResolvedShellPromptProfile::UnixLike,
        );

        assert!(guidelines.contains("positive flat indices"));
        assert!(guidelines.contains("index: 0"));
        assert!(guidelines.contains("items"));
    }

    #[test]
    fn unix_like_guidance_makes_command_reuse_explicit() {
        let tools = vec![TOOL_EXEC_COMMAND.to_string(), TOOL_WRITE_STDIN.to_string()];
        let guidelines = generate_tool_guidelines_for_profile(&tools, None, ResolvedShellPromptProfile::UnixLike);

        assert!(guidelines.contains("one-shot `exec_command` calls"));
        assert!(guidelines.contains("`!!`, `!$`, `!ssh`, or `fc`"));
        assert!(guidelines.contains("write full command arguments explicitly"));
        assert!(guidelines.contains("conversation or tool results"));
        assert!(guidelines.contains("existing `session_id`"));
        assert!(guidelines.contains("Bash `histverify`"));
        assert!(guidelines.contains("zsh `HIST_VERIFY`"));
    }

    #[test]
    fn powershell_guidance_uses_native_command_examples() {
        let tools = vec![
            TOOL_EXEC_COMMAND.to_string(),
            TOOL_CODE_SEARCH.to_string(),
            TOOL_APPLY_PATCH.to_string(),
        ];
        let guidelines = generate_tool_guidelines_for_profile(&tools, None, ResolvedShellPromptProfile::PowerShell);

        assert!(guidelines.contains("native PowerShell commands"));
        assert!(guidelines.contains("`Get-ChildItem`"));
        assert!(guidelines.contains("`Select-String`"));
        assert!(guidelines.contains("native PowerShell syntax"));
        assert!(guidelines.contains("Advanced `code_search` takes `query`"));
        assert!(guidelines.contains("literal smart-case"));
        assert!(guidelines.contains("omit unused filters"));
        assert!(!guidelines.contains("`ls`, `rg`, `find`, `cat`, `sed`, and `awk`"));
        assert!(!guidelines.contains("shell history expansion"));
        assert!(!guidelines.contains("histverify"));
        assert!(!guidelines.contains("HIST_VERIFY"));
    }

    #[test]
    fn shell_profile_prompt_keeps_policy_and_syntax_separate() {
        let unix = render_shell_profile_guidance(ResolvedShellPromptProfile::UnixLike);
        assert!(unix.contains("Active shell profile: `unix_like`"));
        assert!(unix.contains("does not rewrite GNU flags for macOS BSD tools"));
        assert!(unix.contains("controls prompt examples and expected command syntax only"));
        assert!(unix.contains("does not translate GNU-to-BSD"));

        let powershell = render_shell_profile_guidance(ResolvedShellPromptProfile::PowerShell);
        assert!(powershell.contains("Active shell profile: `powershell`"));
        assert!(powershell.contains("WSL"));
        assert!(powershell.contains("Unix-like workflows"));
        assert!(powershell.contains("PowerShell-to-Unix"));
    }

    #[test]
    fn test_harness_browse_tool_guidance() {
        let tools = vec![TOOL_LIST_FILES.to_string(), TOOL_READ_FILE.to_string()];
        let guidelines = generate_tool_guidelines(&tools, None);
        assert!(guidelines.contains("available read-only repository tools"));
        assert!(guidelines.contains("bounded `read_file` ranges"));
        assert!(!guidelines.contains("list_files"));
        assert!(!guidelines.contains("offset"));
        assert!(!guidelines.contains("per_page"));
    }

    #[test]
    fn test_canonical_browse_tool_guidance_prefers_public_tools() {
        let tools = vec![
            TOOL_CODE_SEARCH.to_string(),
            TOOL_LIST_FILES.to_string(),
            "read_file".to_string(),
        ];
        let guidelines = generate_tool_guidelines(&tools, None);
        assert!(guidelines.contains("available read-only repository tools"));
        assert!(guidelines.contains("code_search"));
        assert!(guidelines.contains("bounded `read_file` ranges"));
    }

    #[test]
    fn test_capability_basic_guidance() {
        let tools = vec![];
        let guidelines = generate_tool_guidelines(&tools, Some(CapabilityLevel::Basic));
        assert!(guidelines.contains("Capabilities: limited"));
        assert!(guidelines.contains("enable more capabilities"));
    }

    #[test]
    fn test_capability_file_reading_guidance() {
        let tools = vec![TOOL_APPLY_PATCH.to_string()];
        let guidelines = generate_tool_guidelines(&tools, Some(CapabilityLevel::FileReading));
        assert!(guidelines.contains("Capabilities: read-only"));
        assert!(guidelines.contains("do not modify"));
    }

    #[test]
    fn test_full_capabilities_no_special_guidance() {
        let tools = vec![
            TOOL_APPLY_PATCH.to_string(),
            TOOL_EXEC_COMMAND.to_string(),
            TOOL_CODE_SEARCH.to_string(),
        ];
        let guidelines = generate_tool_guidelines_for_profile(
            &tools,
            Some(CapabilityLevel::Editing),
            ResolvedShellPromptProfile::UnixLike,
        );

        assert!(!guidelines.contains("Capabilities: limited"));
        assert!(!guidelines.contains("Capabilities: read-only"));
    }

    #[test]
    fn test_empty_tools_shows_read_only_capabilities() {
        let tools = vec![];
        let guidelines = generate_tool_guidelines(&tools, None);
        assert!(guidelines.contains("Capabilities: read-only"));
    }

    #[test]
    fn test_planning_workflow_guidance_keeps_verification_open() {
        let tools = vec![
            TOOL_EXEC_COMMAND.to_string(),
            TOOL_TASK_TRACKER.to_string(),
            TOOL_CODE_SEARCH.to_string(),
        ];
        let guidelines =
            generate_runtime_tool_guidelines_for_profile(&tools, true, ResolvedShellPromptProfile::UnixLike);
        assert!(guidelines.contains("Keep `task_tracker` updated"));
        assert!(guidelines.contains("blockers and verification open"));
    }

    #[test]
    fn test_capability_inference_precedence() {
        let tools = vec![TOOL_APPLY_PATCH.to_string(), TOOL_CODE_SEARCH.to_string()];
        assert_eq!(infer_capability_level(&tools), CapabilityLevel::CodeSearch);

        let tools = vec![TOOL_EXEC_COMMAND.to_string(), TOOL_APPLY_PATCH.to_string()];
        assert_eq!(infer_capability_level(&tools), CapabilityLevel::Editing);
    }

    #[test]
    fn test_capability_inference_variants() {
        let tools = vec![TOOL_APPLY_PATCH.to_string()];
        assert_eq!(infer_capability_level(&tools), CapabilityLevel::Editing);

        let tools = vec![TOOL_EXEC_COMMAND.to_string()];
        assert_eq!(infer_capability_level(&tools), CapabilityLevel::Bash);

        let tools = vec![TOOL_CODE_SEARCH.to_string()];
        assert_eq!(infer_capability_level(&tools), CapabilityLevel::CodeSearch);

        let tools = vec![TOOL_LIST_FILES.to_string()];
        assert_eq!(infer_capability_level(&tools), CapabilityLevel::FileListing);

        let tools = vec!["read_file".to_string()];
        assert_eq!(infer_capability_level(&tools), CapabilityLevel::FileReading);

        let tools = vec!["unknown_tool".to_string()];
        assert_eq!(infer_capability_level(&tools), CapabilityLevel::Basic);
    }

    #[test]
    fn test_guidelines_stay_compact() {
        let tools = vec![
            TOOL_EXEC_COMMAND.to_string(),
            TOOL_CODE_SEARCH.to_string(),
            "read_file".to_string(),
            TOOL_LIST_FILES.to_string(),
            "apply_patch".to_string(),
        ];
        let guidelines = generate_tool_guidelines_for_profile(&tools, None, ResolvedShellPromptProfile::UnixLike);
        assert!(guidelines.contains("Batch independent read-only calls"));
        assert!(guidelines.contains("code_search"));
        let approx_tokens = guidelines.len() / 4;
        // The batching and bounded-diff guardrails are intentionally part of
        // the compact shared prompt; keep the budget below 400 tokens.
        assert!(approx_tokens < 400, "got ~{approx_tokens} tokens");
    }

    #[test]
    fn test_parallel_tool_call_guidance() {
        let tools = vec![
            TOOL_EXEC_COMMAND.to_string(),
            TOOL_CODE_SEARCH.to_string(),
            TOOL_APPLY_PATCH.to_string(),
        ];
        let guidelines = generate_tool_guidelines_for_profile(&tools, None, ResolvedShellPromptProfile::UnixLike);
        assert!(guidelines.contains("parallel"), "Should include parallel tool call guidance");
        assert!(guidelines.contains("inputs do not depend"), "Should mention independent inputs");
    }

    #[test]
    fn test_read_only_batching_guidance_is_explicit() {
        let tools = vec![
            TOOL_CODE_SEARCH.to_string(),
            TOOL_READ_FILE.to_string(),
            TOOL_LIST_FILES.to_string(),
        ];
        let guidelines = generate_tool_guidelines_for_profile(&tools, None, ResolvedShellPromptProfile::UnixLike);

        assert!(guidelines.contains("Batch independent read-only calls"));
        assert!(guidelines.contains("`read_file` ranges"));
        assert!(guidelines.contains("serialize mutations"));
    }

    #[test]
    fn execution_agents_can_suggest_planning_for_demanding_tasks() {
        let tools = vec![TOOL_START_PLANNING.to_string(), TOOL_EXEC_COMMAND.to_string()];
        let guidelines = generate_tool_guidelines_for_profile(&tools, None, ResolvedShellPromptProfile::UnixLike);

        assert!(guidelines.contains("call `start_planning`"));
        assert!(guidelines.contains("do not use it for straightforward changes"));
    }

    #[test]
    fn planning_workflow_runtime_guidance_keeps_exec_read_only() {
        let tools = vec![
            TOOL_APPLY_PATCH.to_string(),
            TOOL_EXEC_COMMAND.to_string(),
            TOOL_CODE_SEARCH.to_string(),
        ];
        let guidelines =
            generate_runtime_tool_guidelines_for_profile(&tools, true, ResolvedShellPromptProfile::UnixLike);

        assert!(guidelines.contains("Planning workflow active"));
        assert!(guidelines.contains("`exec_command` only for read-only verification"));
        assert!(guidelines.contains("concrete repository target"));
        assert!(guidelines.contains("emit only one `<proposed_plan>` block"));
        assert!(guidelines.contains("omit unused filters"));
        assert!(!guidelines.contains("Inspect before edit"));
    }

    #[test]
    fn runtime_tool_guidance_uses_explicit_powershell_profile() {
        let tools = vec![
            TOOL_APPLY_PATCH.to_string(),
            TOOL_EXEC_COMMAND.to_string(),
            TOOL_CODE_SEARCH.to_string(),
        ];
        let guidelines =
            generate_runtime_tool_guidelines_for_profile(&tools, false, ResolvedShellPromptProfile::PowerShell);

        assert!(guidelines.contains("native PowerShell commands"));
        assert!(guidelines.contains("`Get-ChildItem`"));
        assert!(guidelines.contains("`Select-String`"));
        assert!(guidelines.contains("native PowerShell syntax"));
        assert!(!guidelines.contains("`ls`, `rg`, `find`, `cat`, `sed`, and `awk`"));
    }

    #[test]
    fn runtime_tool_guidance_uses_explicit_unix_like_profile() {
        let tools = vec![
            TOOL_APPLY_PATCH.to_string(),
            TOOL_EXEC_COMMAND.to_string(),
            TOOL_CODE_SEARCH.to_string(),
        ];
        let guidelines =
            generate_runtime_tool_guidelines_for_profile(&tools, false, ResolvedShellPromptProfile::UnixLike);

        assert!(guidelines.contains("`ls`, `rg`, `find`, `cat`, `sed`, and `awk`"));
        assert!(guidelines.contains("Advanced `code_search` takes `query`"));
        assert!(guidelines.contains("literal smart-case"));
        assert!(guidelines.contains("shell-only tasks"));
        assert!(!guidelines.contains("native PowerShell commands"));
        assert!(!guidelines.contains("`Get-ChildItem`"));
    }

    #[test]
    fn runtime_tool_prompt_sections_use_explicit_profile_for_active_tools() {
        let mut powershell_prompt = "Base prompt".to_string();
        let mut unix_prompt = "Base prompt".to_string();
        let snapshot = SessionToolCatalogSnapshot::new(
            7,
            9,
            false,
            false,
            Some(std::sync::Arc::new(vec![
                ToolDefinition::function(
                    TOOL_EXEC_COMMAND.to_string(),
                    "Shell".to_string(),
                    serde_json::json!({"type": "object"}),
                ),
                ToolDefinition::function(
                    TOOL_CODE_SEARCH.to_string(),
                    "Bounded source search".to_string(),
                    serde_json::json!({"type": "object"}),
                ),
            ])),
            false,
        );

        append_runtime_tool_prompt_sections_for_profile(
            &mut powershell_prompt,
            &snapshot,
            false,
            ResolvedShellPromptProfile::PowerShell,
        );
        append_runtime_tool_prompt_sections_for_profile(
            &mut unix_prompt,
            &snapshot,
            false,
            ResolvedShellPromptProfile::UnixLike,
        );

        assert!(powershell_prompt.contains("## Active Tools"));
        assert!(powershell_prompt.contains("`Get-ChildItem`"));
        assert!(powershell_prompt.contains("`Select-String`"));
        assert!(!powershell_prompt.contains("`ls`, `rg`, `find`, `cat`, `sed`, and `awk`"));

        assert!(unix_prompt.contains("## Active Tools"));
        assert!(unix_prompt.contains("`ls`, `rg`, `find`, `cat`, `sed`, and `awk`"));
        assert!(unix_prompt.contains("Advanced `code_search` takes `query`"));
        assert!(unix_prompt.contains("literal smart-case"));
        assert!(!unix_prompt.contains("`Get-ChildItem`"));
    }

    #[test]
    fn runtime_tool_prompt_sections_include_catalog_metadata() {
        let mut prompt = "Base prompt".to_string();
        let snapshot = SessionToolCatalogSnapshot::new(
            7,
            9,
            true,
            false,
            Some(std::sync::Arc::new(vec![
                ToolDefinition::function(
                    TOOL_EXEC_COMMAND.to_string(),
                    "Search".to_string(),
                    serde_json::json!({"type": "object"}),
                ),
                ToolDefinition::function(
                    TOOL_APPLY_PATCH.to_string(),
                    "File".to_string(),
                    serde_json::json!({"type": "object"}),
                ),
            ])),
            false,
        );

        append_runtime_tool_prompt_sections(&mut prompt, &snapshot, true);

        assert!(prompt.contains("## Active Tools"));
        assert!(prompt.contains("[Runtime Tool Catalog]"));
        assert!(prompt.contains("catalog_tools: 2"));
        assert!(prompt.contains("currently_available_tools: exec_command, apply_patch"));
        assert!(prompt.contains("request_user_input_enabled: false"));
    }

    #[test]
    fn runtime_tool_prompt_sections_replace_existing_runtime_sections() {
        let mut prompt = "Base prompt".to_string();
        let first = SessionToolCatalogSnapshot::new(
            1,
            2,
            false,
            false,
            Some(std::sync::Arc::new(vec![ToolDefinition::function(
                TOOL_EXEC_COMMAND.to_string(),
                "Search".to_string(),
                serde_json::json!({"type": "object"}),
            )])),
            false,
        );
        let second = SessionToolCatalogSnapshot::new(
            7,
            9,
            true,
            true,
            Some(std::sync::Arc::new(vec![ToolDefinition::function(
                TOOL_APPLY_PATCH.to_string(),
                "File".to_string(),
                serde_json::json!({"type": "object"}),
            )])),
            false,
        );

        append_runtime_tool_prompt_sections(&mut prompt, &first, true);
        append_runtime_tool_prompt_sections(&mut prompt, &second, true);

        assert_eq!(prompt.matches("## Active Tools").count(), 1);
        assert_eq!(prompt.matches("[Runtime Tool Catalog]").count(), 1);
        assert!(prompt.contains("version: 7"));
        assert!(!prompt.contains("version: 1"));
        assert!(prompt.contains("request_user_input_enabled: true"));
        assert!(!prompt.contains("request_user_input_enabled: false"));
    }
}
