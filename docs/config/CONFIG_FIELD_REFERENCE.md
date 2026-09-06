# Config Field Reference

Generated from `vtcode-config` schema (`VTCodeConfig`) for complete field coverage.

Regenerate:

```bash
python3 scripts/generate_config_field_reference.py
```

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `acp.enabled` | `boolean` | no | `false` | Globally enable the ACP bridge |
| `acp.zed.auth.auth_url` | `null \| string` | no | `null` | URL where users can get their API key (optional, for UI display) |
| `acp.zed.auth.default_method` | `string` | no | `"agent"` | Default authentication method for ACP agents Options: "agent" (default - agent handles auth), "env_var", "terminal" |
| `acp.zed.auth.env_var_name` | `null \| string` | no | `null` | Environment variable name for auth (used when default_method is "env_var") Examples: "OPENAI_API_KEY", "ANTHROPIC_API_KEY" |
| `acp.zed.enabled` | `boolean` | no | `false` | Enable Zed integration |
| `acp.zed.tools.list_files` | `boolean` | no | `true` | Toggle the list_files function bridge |
| `acp.zed.tools.read_file` | `boolean` | no | `true` | Toggle the read_file function bridge |
| `acp.zed.transport` | `string` | no | `"stdio"` | Transport used to communicate with the Zed client |
| `acp.zed.workspace_trust` | `string` | no | `"full_auto"` | Desired workspace trust level when running under ACP |
| `agent.api_key_env` | `string` | no | `"OPENROUTER_API_KEY"` | Environment variable that stores the API key for the active provider |
| `agent.checkpointing.enabled` | `boolean` | no | `true` | Enable automatic checkpoints after each successful turn |
| `agent.checkpointing.max_age_days` | `integer \| null` | no | `30` | Maximum age in days before checkpoints are removed automatically (None disables) |
| `agent.checkpointing.max_snapshots` | `integer` | no | `50` | Maximum number of checkpoints to retain on disk |
| `agent.checkpointing.storage_dir` | `null \| string` | no | `null` | Optional custom directory for storing checkpoints (relative to workspace or absolute) |
| `agent.circuit_breaker.enabled` | `boolean` | no | `true` | Enable circuit breaker functionality |
| `agent.circuit_breaker.failure_threshold` | `integer` | no | `7` | Number of consecutive failures before opening circuit |
| `agent.circuit_breaker.max_open_circuits` | `integer` | no | `3` | Number of open circuits before triggering pause |
| `agent.circuit_breaker.pause_on_open` | `boolean` | no | `true` | Pause and ask user when circuit opens (vs auto-backoff) |
| `agent.circuit_breaker.recovery_cooldown` | `integer` | no | `60` | Cooldown period between recovery prompts (seconds) |
| `agent.codex_app_server.args` | `array` | no | `["app-server"]` | Arguments passed before VT Code appends `--listen stdio://`. |
| `agent.codex_app_server.args[]` | `string` | no | `-` | - |
| `agent.codex_app_server.command` | `string` | no | `"codex"` | Executable used to launch the official Codex app-server sidecar. |
| `agent.codex_app_server.experimental_features` | `boolean` | no | `false` | Enable experimental Codex app-server sidecar features. |
| `agent.codex_app_server.startup_timeout_secs` | `integer` | no | `10` | Maximum startup handshake time when launching the sidecar. |
| `agent.credential_storage_mode` | `string` | no | `"auto"` | Preferred storage backend for credentials (OAuth tokens, API keys, etc.) - `keyring`: Use OS-specific secure storage (macOS Keychain, Windows Credential Manager, Linux Secret Service), with encrypted-file fallback when unavailable. - `file`: Use AES-256-GCM encrypted file with machine-derived key - `auto`: Try keyring first, fall back to file if unavailable |
| `agent.custom_api_keys` | `object` | no | `-` | Provider/key identities captured from interactive configuration flows Note: Actual API keys are stored securely in the configured credential backend (OS keyring when available, otherwise encrypted file storage). Keys use `<provider>/<environment-variable>` identity keys and this field only tracks which identities have keys stored (for UI/migration purposes). The keys themselves are NOT serialized to the config file for security. |
| `agent.custom_api_keys.*` | `string` | no | `-` | - |
| `agent.default_model` | `string` | no | `"xiaomi/mimo-v2.5-pro"` | Default model to use |
| `agent.enable_self_review` | `boolean` | no | `false` | Enable an extra self-review pass to refine final responses |
| `agent.enable_split_tool_results` | `boolean` | no | `true` | Enable split tool results for massive token savings (Phase 4) When enabled, tools return dual-channel output: - llm_content: Concise summary sent to LLM (token-optimized, 53-95% reduction) - ui_content: Rich output displayed to user (full details preserved) Applies to: exec_command, code_search, apply_patch, and retained internal helpers Default: true (opt-out for compatibility), recommended for production use |
| `agent.harness.async_approval.auto_approve_below_usd` | `number` | no | `0.1` | Auto-approve tool calls whose estimated cost is below this threshold (USD). Set to 0.0 to require approval for everything. |
| `agent.harness.async_approval.auto_approve_timeout_secs` | `integer \| null` | no | `null` | When set, auto-approve after this many seconds if no explicit answer. The blocker file is updated with the auto-approval decision. |
| `agent.harness.async_approval.enabled` | `boolean` | no | `false` | Master switch. Default: false (opt-in). |
| `agent.harness.async_approval.notify_command` | `null \| string` | no | `null` | Optional command invoked with the blocker file path as argument when an approval request is deferred (e.g. `/usr/local/bin/notify-approval`). |
| `agent.harness.async_approval.timeout_secs` | `integer` | no | `3600` | Maximum time in seconds before a deferred approval is auto-denied. |
| `agent.harness.auto_compaction_enabled` | `boolean` | no | `true` | Enable automatic context compaction when token pressure crosses threshold. Enabled by default. When disabled, normal threshold-triggered automatic compaction is skipped; the bounded post-tool recovery path may still compact the older prefix as a safety fallback after a provider failure. |
| `agent.harness.auto_compaction_instructions` | `null \| string` | no | `null` | Optional custom instructions for the compaction summarization prompt. When set, replaces the default Anthropic compaction prompt entirely. Useful for tool-use scenarios to prevent the model from calling tools during summarization. Only applies to Anthropic provider. |
| `agent.harness.auto_compaction_pause_after` | `boolean` | no | `false` | Whether to pause after compaction (Anthropic only). When true and compaction triggers, the API returns early with `stop_reason: "compaction"` and only the compaction block. The caller can then insert additional messages before the model generates its text response. |
| `agent.harness.auto_compaction_threshold_tokens` | `integer \| null` | no | `null` | Optional absolute compaction threshold (tokens) for native and local compaction. When set, this lowers the trigger but remains capped by the resolved model capacity, the provider route, and `context.max_context_tokens`; output headroom is reserved before the request is sent. When unset, VT Code uses the effective prompt budget directly. |
| `agent.harness.budget_warning_threshold` | `number` | no | `0.75` | Fraction of `max_budget_usd` at which VT Code emits a one-time near-budget warning. Ignored when `max_budget_usd` is unset. |
| `agent.harness.compact_on_model_switch` | `boolean` | no | `true` | Automatically compact conversation context when the main session model or provider is switched mid-conversation, so the newly selected model starts from a summary instead of the outgoing model's raw trace. Default: true. Disable to keep the full history across a model switch. |
| `agent.harness.confidence_escalation.always_escalate_tools` | `array` | no | `["delete_file", "remove", "rm"]` | Tool names that always trigger escalation regardless of confidence. These are matched against the tool call's function name. |
| `agent.harness.confidence_escalation.always_escalate_tools[]` | `string` | no | `-` | - |
| `agent.harness.confidence_escalation.confidence_threshold` | `number` | no | `0.7` | Minimum p_success threshold (0.0–1.0). Tools whose estimated success probability falls below this threshold trigger escalation. |
| `agent.harness.confidence_escalation.cost_threshold_usd` | `number` | no | `0.05` | Maximum estimated cost in USD before escalation. Tool calls whose estimated cost exceeds this threshold trigger escalation. |
| `agent.harness.confidence_escalation.enabled` | `boolean` | no | `false` | Master switch. Default: false (opt-in). |
| `agent.harness.confidence_escalation.max_replan_attempts` | `integer` | no | `3` | Maximum number of re-plan attempts via conversation injection before prompting the user. The agent sees a structured message explaining which tools were blocked and is asked to try a different approach. |
| `agent.harness.confidence_escalation.max_total_escalations` | `integer` | no | `5` | Maximum total escalations (across all chain steps) before aborting with partial results. Must be >= max_replan_attempts. |
| `agent.harness.confidence_escalation.plan_mode_only` | `boolean` | no | `false` | When true, only escalate during PlanBuildEvaluate orchestration mode. During single-mode or other orchestration modes, escalation is skipped. |
| `agent.harness.confidence_escalation.prompt_user_on_exhaust` | `boolean` | no | `true` | Whether to prompt the user when the escalation chain is exhausted. When false, the chain falls through directly to abort-with-partial-results. |
| `agent.harness.confidence_escalation.use_llm_confidence` | `boolean` | no | `false` | Whether to solicit LLM-based confidence estimation alongside heuristics. When false (default), only heuristic signals (error history, tool class) are used to estimate p_success. |
| `agent.harness.context_reset_mode` | `string` | no | `"off"` | When to trigger a context reset — starting a clean session from external artifacts only, discarding conversation history. Distinct from compaction (which preserves conversational continuity). Default: `off` (carry forward history as before). |
| `agent.harness.context_reset_stall_threshold` | `integer` | no | `2` | Number of consecutive stall turns before `on_stall` context reset triggers. Ignored unless `context_reset_mode = "on_stall"`. Default: 2. |
| `agent.harness.continuation_policy` | `string` | no | `"all"` | Controls whether harness-managed continuation loops are enabled. |
| `agent.harness.event_log_path` | `null \| string` | no | `null` | Optional compatibility/export JSONL path for harness events. Canonical events are always stored under the workspace session store; unset configuration creates no global harness file. |
| `agent.harness.max_budget_usd` | `null \| number` | no | `null` | Optional maximum estimated API cost in USD before VT Code stops the session. |
| `agent.harness.max_parallel_tool_calls` | `integer` | no | `4` | Maximum number of tool calls that may execute concurrently within a single parallel batch. Set to `0` to disable the cap (unlimited concurrency). Default: 4. |
| `agent.harness.max_revision_rounds` | `integer` | no | `2` | Maximum generator revision rounds after evaluator rejection. |
| `agent.harness.max_tool_calls_per_turn` | `integer` | no | `120` | Maximum number of tool calls allowed per turn. Defaults to `120`. Set to `0` to disable the cap. |
| `agent.harness.max_tool_retries` | `integer` | no | `2` | Maximum retries for retryable tool errors |
| `agent.harness.max_tool_wall_clock_secs` | `integer` | no | `600` | Maximum wall clock time (seconds) for tool execution in a turn |
| `agent.harness.orchestration_mode` | `string` | no | `"plan_build_evaluate"` | Select the exec/full-auto harness orchestration path. |
| `agent.harness.skeptic_panel.enabled` | `boolean` | no | `false` | Master switch. Default: false (opt-in). |
| `agent.harness.skeptic_panel.models` | `array` | no | `[]` | Model identifiers to run as skeptic evaluators (in addition to the primary evaluator). Empty when `enabled = false`. |
| `agent.harness.skeptic_panel.models[]` | `string` | no | `-` | - |
| `agent.harness.tool_result_clearing.clear_at_least_tokens` | `integer` | no | `30000` | - |
| `agent.harness.tool_result_clearing.clear_tool_inputs` | `boolean` | no | `false` | - |
| `agent.harness.tool_result_clearing.enabled` | `boolean` | no | `true` | - |
| `agent.harness.tool_result_clearing.keep_tool_uses` | `integer` | no | `3` | - |
| `agent.harness.tool_result_clearing.trigger_tokens` | `integer` | no | `100000` | - |
| `agent.idle_turn_limit` | `integer` | no | `3` | Maximum consecutive idle turns (no tool calls, no meaningful response) before the agent runner treats the session as stalled and aborts the loop. |
| `agent.include_structured_reasoning_tags` | `boolean \| null` | no | `null` | Controls inclusion of the structured reasoning tag instructions block. Behavior: - `Some(true)`: always include structured reasoning instructions. - `Some(false)`: never include structured reasoning instructions. - `None` (default): include only for `default` and `specialized` prompt modes. This keeps lightweight/minimal prompts smaller by default while allowing explicit opt-in when users want tag-based reasoning guidance. |
| `agent.include_temporal_context` | `boolean` | no | `true` | Include current date/time in system prompt for temporal awareness Helps LLM understand context for time-sensitive tasks (default: true) |
| `agent.include_working_directory` | `boolean` | no | `true` | Include current working directory in system prompt (default: true) |
| `agent.instruction_excludes` | `array` | no | `[]` | Instruction files or globs to exclude from AGENTS.md and rules discovery |
| `agent.instruction_excludes[]` | `string` | no | `-` | - |
| `agent.instruction_files` | `array` | no | `[]` | Additional instruction files or globs to merge into the hierarchy |
| `agent.instruction_files[]` | `string` | no | `-` | - |
| `agent.instruction_import_max_depth` | `integer` | no | `5` | Maximum recursive `@path` import depth for instruction and rule files |
| `agent.instruction_max_bytes` | `integer` | no | `16384` | Maximum bytes of instruction content to load from AGENTS.md/CLAUDE.md hierarchy |
| `agent.max_conversation_turns` | `integer` | no | `150` | Maximum number of conversation turns before auto-termination |
| `agent.max_review_passes` | `integer` | no | `1` | Maximum number of self-review passes |
| `agent.max_system_prompt_tokens` | `integer` | no | `8000` | Soft token budget for the fully composed system prompt (character-based estimate, ~4 chars/token). Includes workspace instructions, guidelines, and runtime addenda on top of the base prompt. |
| `agent.max_task_retries` | `integer` | no | `2` | Maximum number of retries for agent task execution (default: 2) When an agent task fails due to retryable errors (timeout, network, 503, etc.), it will be retried up to this many times with exponential backoff |
| `agent.onboarding.chat_placeholder` | `null \| string` | no | `null` | Placeholder suggestion for the chat input bar |
| `agent.onboarding.enabled` | `boolean` | no | `true` | Toggle onboarding message rendering |
| `agent.onboarding.guideline_highlight_limit` | `integer` | no | `3` | Maximum number of guideline bullets to surface |
| `agent.onboarding.include_guideline_highlights` | `boolean` | no | `true` | Whether to include AGENTS.md/CLAUDE.md highlights in onboarding message |
| `agent.onboarding.include_language_summary` | `boolean` | no | `false` | Whether to include language summary in onboarding message |
| `agent.onboarding.include_project_overview` | `boolean` | no | `true` | Whether to include project overview in onboarding message |
| `agent.onboarding.include_recommended_actions_in_welcome` | `boolean` | no | `false` | Whether to surface suggested actions inside the welcome text banner |
| `agent.onboarding.include_usage_tips_in_welcome` | `boolean` | no | `false` | Whether to surface usage tips inside the welcome text banner |
| `agent.onboarding.intro_text` | `string` | no | `"Let's get oriented. I preloaded workspace context so we can move fast."` | Introductory text shown at session start |
| `agent.onboarding.recommended_actions` | `array` | no | `["Review the highlighted guidelines and share the task you want to tackle.", "Ask for a workspace tour if you need mo...` | Recommended follow-up actions to display |
| `agent.onboarding.recommended_actions[]` | `string` | no | `-` | - |
| `agent.onboarding.usage_tips` | `array` | no | `["Describe your current coding goal or ask for a quick status overview.", "Reference AGENTS.md/CLAUDE.md guidelines w...` | Tips for collaborating with the agent effectively |
| `agent.onboarding.usage_tips[]` | `string` | no | `-` | - |
| `agent.open_responses.emit_events` | `boolean` | no | `true` | Emit Open Responses events to the event sink When true, streaming events follow Open Responses format (response.created, response.output_item.added, response.output_text.delta, etc.) |
| `agent.open_responses.enabled` | `boolean` | no | `false` | Enable Open Responses specification compliance layer When true, VT Code emits semantic streaming events alongside internal events Default: false (opt-in feature) |
| `agent.open_responses.include_extensions` | `boolean` | no | `true` | Include VT Code extension items (vtcode:file_change, vtcode:web_search, etc.) When false, extension items are omitted from the Open Responses output |
| `agent.open_responses.include_reasoning` | `boolean` | no | `true` | Include reasoning items in Open Responses output When true, model reasoning/thinking is exposed as reasoning items |
| `agent.open_responses.map_tool_calls` | `boolean` | no | `true` | Map internal tool calls to Open Responses function_call items When true, command executions and MCP tool calls are represented as function_call items |
| `agent.persistent_memory.auto_write` | `boolean` | no | `true` | Write durable memory after completed turns and session finalization |
| `agent.persistent_memory.directory_override` | `null \| string` | no | `null` | Optional user-local directory override for persistent memory storage |
| `agent.persistent_memory.enabled` | `boolean` | no | `false` | Toggle main-session persistent memory for this repository. Natural-language saves resolve "it", "this", and "that" only against the immediately preceding assistant answer and require confirmation; identity names and aliases are stored as preferences. |
| `agent.persistent_memory.memories.consolidation_model` | `null \| string` | no | `null` | Overrides the model used for global memory consolidation. |
| `agent.persistent_memory.memories.extract_model` | `null \| string` | no | `null` | Overrides the model used for per-thread memory extraction. |
| `agent.persistent_memory.memories.generate_memories` | `boolean` | no | `true` | Controls whether newly completed threads can be stored as memory-generation inputs. |
| `agent.persistent_memory.memories.use_memories` | `boolean` | no | `true` | Controls whether VT Code injects existing memories into future sessions. |
| `agent.persistent_memory.startup_byte_limit` | `integer` | no | `25600` | Startup byte budget scanned from memory_summary.md before VT Code renders a compact startup summary |
| `agent.persistent_memory.startup_line_limit` | `integer` | no | `200` | Startup line budget scanned from memory_summary.md before VT Code renders a compact startup summary |
| `agent.project_doc_fallback_filenames` | `array` | no | `[]` | Additional filenames to check when AGENTS.md is absent at a directory level. |
| `agent.project_doc_fallback_filenames[]` | `string` | no | `-` | - |
| `agent.project_doc_max_bytes` | `integer` | no | `16384` | Maximum bytes of AGENTS.md/CLAUDE.md content to load from project hierarchy |
| `agent.prompt_suggestions.enabled` | `boolean` | no | `true` | Enable inline prompt suggestions in the chat composer. |
| `agent.prompt_suggestions.model` | `string` | no | `""` | Lightweight model to use for suggestions. Leave empty to auto-select an efficient sibling of the main model. |
| `agent.prompt_suggestions.show_cost_notice` | `boolean` | no | `true` | Whether VT Code should remind users that LLM-backed suggestions consume tokens. |
| `agent.prompt_suggestions.temperature` | `number` | no | `0.3` | Temperature for inline prompt suggestion generation. |
| `agent.provider` | `string` | no | `"openrouter"` | AI provider for single agent mode (gemini, openai, anthropic, meta, openrouter, vercel, zai) |
| `agent.reasoning_effort` | `string` | no | `"none"` | Reasoning effort level for models that support it (none, minimal, low, medium, high, xhigh, max) Applies to: Claude, GPT-5 family, Gemini, Qwen3, DeepSeek with reasoning capability |
| `agent.refine_prompts_enabled` | `boolean` | no | `false` | Enable prompt refinement pass before sending to LLM |
| `agent.refine_prompts_max_passes` | `integer` | no | `1` | Max refinement passes for prompt writing |
| `agent.refine_prompts_model` | `string` | no | `""` | Optional model override for the refiner (empty = auto pick efficient sibling) |
| `agent.refine_temperature` | `number` | no | `0.3` | Temperature for prompt refinement (0.0-1.0, default: 0.3) Lower values ensure prompt refinement is more deterministic/consistent Keep lower than main temperature for stable prompt improvement |
| `agent.require_plan_confirmation` | `boolean` | no | `true` | Require user confirmation before executing a plan generated in planning workflow When true, exiting planning workflow shows the implementation blueprint and requires explicit user approval before enabling edit tools. |
| `agent.shell_prompt_profile` | `string` | no | `"auto"` | Shell syntax profile used in model-facing command examples. This controls prompt wording only; command policy remains in the runtime. Values: auto, unix_like, powershell. |
| `agent.small_model.enabled` | `boolean` | no | `true` | Enable small model tier for efficient operations |
| `agent.small_model.model` | `string` | no | `""` | Small model to use (e.g., claude-4-5-haiku, "gpt-4-mini", "gemini-2.0-flash") Leave empty to auto-select a lightweight sibling of the main model |
| `agent.small_model.temperature` | `number` | no | `0.3` | Temperature for small model responses |
| `agent.small_model.use_for_git_history` | `boolean` | no | `true` | Enable small model for git history processing |
| `agent.small_model.use_for_large_reads` | `boolean` | no | `true` | Enable small model for large file reads (>50KB) |
| `agent.small_model.use_for_memory` | `boolean` | no | `true` | Enable small model for persistent memory classification and summary refresh |
| `agent.small_model.use_for_web_summary` | `boolean` | no | `true` | Enable small model for web content summarization |
| `agent.system_prompt_budget_warning` | `boolean` | no | `true` | Warn when the composed system prompt exceeds `max_system_prompt_tokens`. |
| `agent.system_prompt_mode` | `string` | no | `"default"` | System prompt mode controlling prompt verbosity and token overhead. Options target lean base prompts: minimal (~150-250 tokens), lightweight/default (~250-350 tokens), specialized (~350-500 tokens) before dynamic runtime addenda. |
| `agent.temperature` | `number` | no | `0.7` | Temperature for main LLM responses (0.0-1.0) Lower values = more deterministic, higher values = more creative Recommended: 0.7 for balanced creativity and consistency Range: 0.0 (deterministic) to 1.0 (maximum randomness) |
| `agent.temporal_context_use_utc` | `boolean` | no | `false` | Use UTC instead of local time for temporal context in system prompts |
| `agent.theme` | `string` | no | `"ciapre"` | UI theme identifier controlling ANSI styling |
| `agent.todo_planning_mode` | `boolean` | no | `true` | Enable TODO planning helper mode for structured task management |
| `agent.tool_documentation_mode` | `string` | no | `"progressive"` | Tool documentation mode controlling token overhead for tool definitions Options: minimal (~800 tokens), progressive (~1.2k), full (~3k current) Progressive: signatures upfront, detailed docs on-demand (recommended) Minimal: signatures only, pi-coding-agent style (power users) Full: all documentation upfront (current behavior, default) |
| `agent.trim_system_prompt` | `boolean` | no | `false` | Trim low-priority system prompt sections when over budget. Opt-in: silently dropping instructions changes agent behavior, so the default only warns. |
| `agent.ui_surface` | `string` | no | `"inline"` | Preferred rendering surface for the interactive chat UI (inline by default; auto, alternate, inline) |
| `agent.user_instructions` | `null \| string` | no | `null` | Custom instructions provided by the user via configuration to guide agent behavior |
| `agent.verbosity` | `string` | no | `"medium"` | Verbosity level for output text (low, medium, high) Applies to: GPT-5.4-family Responses workflows and other models that support verbosity control |
| `agent.vibe_coding.enable_conversation_memory` | `boolean` | no | `true` | Enable conversation memory for pronoun resolution |
| `agent.vibe_coding.enable_entity_resolution` | `boolean` | no | `true` | Enable fuzzy entity resolution |
| `agent.vibe_coding.enable_proactive_context` | `boolean` | no | `true` | Enable proactive context gathering |
| `agent.vibe_coding.enable_pronoun_resolution` | `boolean` | no | `true` | Enable pronoun resolution (it, that, this) |
| `agent.vibe_coding.enable_relative_value_inference` | `boolean` | no | `true` | Enable relative value inference (by half, double, etc.) |
| `agent.vibe_coding.enabled` | `boolean` | no | `false` | Enable vibe coding support |
| `agent.vibe_coding.entity_index_cache` | `string` | no | `".vtcode/entity_index.json"` | Entity index cache file path (relative to workspace) |
| `agent.vibe_coding.max_context_files` | `integer` | no | `3` | Maximum files to gather for context (default: 3) |
| `agent.vibe_coding.max_context_snippets_per_file` | `integer` | no | `20` | Maximum code snippets per file (default: 20 lines) |
| `agent.vibe_coding.max_entity_matches` | `integer` | no | `5` | Maximum entity matches to return (default: 5) |
| `agent.vibe_coding.max_memory_turns` | `integer` | no | `50` | Maximum conversation turns to remember (default: 50) |
| `agent.vibe_coding.max_recent_files` | `integer` | no | `20` | Maximum recent files to track (default: 20) |
| `agent.vibe_coding.max_search_results` | `integer` | no | `5` | Maximum search results to include (default: 5) |
| `agent.vibe_coding.min_prompt_length` | `integer` | no | `5` | Minimum prompt length for refinement (default: 5 chars) |
| `agent.vibe_coding.min_prompt_words` | `integer` | no | `2` | Minimum prompt words for refinement (default: 2 words) |
| `agent.vibe_coding.track_value_history` | `boolean` | no | `true` | Track value history for inference |
| `agent.vibe_coding.track_workspace_state` | `boolean` | no | `true` | Track workspace state (file activity, value changes) |
| `auth.copilot.auth_timeout_secs` | `integer` | no | `300` | - |
| `auth.copilot.available_tools` | `array` | no | `["view", "glob", "grep"]` | - |
| `auth.copilot.available_tools[]` | `string` | no | `-` | - |
| `auth.copilot.command` | `null \| string` | no | `null` | - |
| `auth.copilot.excluded_tools` | `array` | no | `[]` | - |
| `auth.copilot.excluded_tools[]` | `string` | no | `-` | - |
| `auth.copilot.host` | `null \| string` | no | `null` | - |
| `auth.copilot.startup_timeout_secs` | `integer` | no | `20` | - |
| `auth.copilot.vtcode_tool_allowlist` | `array` | no | `["exec_command", "write_stdin", "apply_patch", "code_search"]` | - |
| `auth.copilot.vtcode_tool_allowlist[]` | `string` | no | `-` | - |
| `auth.openai.auto_refresh` | `boolean` | no | `true` | - |
| `auth.openai.callback_port` | `integer` | no | `1455` | - |
| `auth.openai.flow_timeout_secs` | `integer` | no | `300` | - |
| `auth.openai.preferred_method` | `string` | no | `"auto"` | - |
| `auth.openrouter.auto_refresh` | `boolean` | no | `true` | Whether to automatically refresh tokens |
| `auth.openrouter.callback_port` | `integer` | no | `8484` | Port for the local callback server |
| `auth.openrouter.flow_timeout_secs` | `integer` | no | `300` | Timeout in seconds for completing the OAuth browser flow. |
| `auth.openrouter.use_oauth` | `boolean` | no | `false` | Whether to use OAuth instead of API key |
| `automation.full_auto.allowed_tools` | `array` | no | `["exec_command", "write_stdin", "apply_patch", "code_search"]` | Allow-list of tools that may execute automatically. |
| `automation.full_auto.allowed_tools[]` | `string` | no | `-` | - |
| `automation.full_auto.enabled` | `boolean` | no | `false` | Enable the runtime flag once the workspace is configured for autonomous runs. |
| `automation.full_auto.max_turns` | `integer` | no | `100` | Maximum number of autonomous agent turns before the exec runner pauses. |
| `automation.full_auto.profile_path` | `null \| string` | no | `null` | Optional path to a profile describing acceptable behaviors. |
| `automation.full_auto.require_profile_ack` | `boolean` | no | `true` | Require presence of a profile/acknowledgement file before activation. |
| `automation.full_auto.verify_mutations` | `boolean` | no | `false` | Run a read-only verifier sub-agent after each mutating tool call. When enabled, the harness spawns a verifier that re-reads the diff and either approves or rejects the change before it is committed. This doubles cost on mutating calls but catches propose-side errors. |
| `automation.loop_engine.enabled` | `boolean` | no | `false` | Enable loop engineering mode. When false, loop-specific features (reconciler, per-iteration skills) are inactive. |
| `automation.loop_engine.max_iterations` | `integer \| null` | no | `null` | Optional circuit breaker: maximum loop iterations before the harness refuses further runs. `None` means unlimited. |
| `automation.loop_engine.preload_skills` | `array` | no | `[]` | Skill names to preload into the agent context at the start of each loop iteration. Empty means no per-iteration skill injection. |
| `automation.loop_engine.preload_skills[]` | `string` | no | `-` | - |
| `automation.loop_engine.reconcile_on_complete` | `boolean` | no | `true` | After a worktree-isolated subagent completes, run the reconciler (diff → verify → merge) automatically. |
| `automation.scheduled_tasks.enabled` | `boolean` | no | `false` | Enable scheduler tools and durable `vtcode schedule` jobs. |
| `chat.askQuestions.enabled` | `boolean` | no | `true` | Enable the Ask Questions tool in interactive chat |
| `commands.allow_glob` | `array` | no | `[]` | Glob patterns allowed for shell commands (applies to Bash) |
| `commands.allow_glob[]` | `string` | no | `-` | - |
| `commands.allow_list` | `array` | no | `[]` | Commands that can be executed without prompting |
| `commands.allow_list[]` | `string` | no | `-` | - |
| `commands.allow_regex` | `array` | no | `[]` | Regex allow patterns for shell commands |
| `commands.allow_regex[]` | `string` | no | `-` | - |
| `commands.approval_prefixes` | `array` | no | `[]` | Command prefixes that skip future shell approval prompts when matched |
| `commands.approval_prefixes[]` | `string` | no | `-` | - |
| `commands.deny_glob` | `array` | no | `[]` | Glob patterns denied for shell commands |
| `commands.deny_glob[]` | `string` | no | `-` | - |
| `commands.deny_list` | `array` | no | `[]` | Commands that are always denied |
| `commands.deny_list[]` | `string` | no | `-` | - |
| `commands.deny_regex` | `array` | no | `[]` | Regex deny patterns for shell commands |
| `commands.deny_regex[]` | `string` | no | `-` | - |
| `commands.extra_path_entries` | `array` | no | `["$HOME/.cargo/bin", "$HOME/.local/bin", "/opt/homebrew/bin", "/usr/local/bin", "$HOME/.asdf/bin", "$HOME/.asdf/shims...` | Additional directories that should be searched/prepended to PATH for command execution |
| `commands.extra_path_entries[]` | `string` | no | `-` | - |
| `context.dynamic.enabled` | `boolean` | no | `true` | Enable dynamic context discovery features |
| `context.dynamic.max_spooled_files` | `integer` | no | `100` | Maximum number of spooled files to keep |
| `context.dynamic.persist_history` | `boolean` | no | `true` | Enable persisting conversation history during summarization |
| `context.dynamic.retained_user_messages` | `integer` | no | `4` | Maximum number of recent user messages to retain verbatim during local compaction |
| `context.dynamic.spool_max_age_secs` | `integer` | no | `3600` | Maximum age in seconds for spooled tool output files before cleanup |
| `context.dynamic.sync_mcp_tools` | `boolean` | no | `true` | Enable syncing MCP tool descriptions to .vtcode/mcp/tools/ |
| `context.dynamic.sync_skills` | `boolean` | no | `true` | Enable generating skill index in .agents/skills/INDEX.md |
| `context.dynamic.sync_terminals` | `boolean` | no | `true` | Enable syncing terminal sessions to .vtcode/terminals/ files |
| `context.dynamic.tool_output_threshold` | `integer` | no | `8192` | Threshold in bytes above which tool outputs are spooled to files |
| `context.ledger.enabled` | `boolean` | no | `true` | - |
| `context.ledger.include_in_prompt` | `boolean` | no | `true` | Inject ledger into the system prompt each turn |
| `context.ledger.max_entries` | `integer` | no | `12` | - |
| `context.ledger.preserve_in_compression` | `boolean` | no | `true` | Preserve ledger entries during context compression |
| `context.max_context_tokens` | `integer` | no | `0` | Optional session prompt safety ceiling. `0` uses the resolved model/provider capacity. The effective auto-compaction boundary is the smaller of the resolved capacity and this ceiling, less reserved output headroom. |
| `context.preserve_recent_turns` | `integer` | no | `10` | Preserve recent turns during context management This field is maintained for compatibility but no longer used for trimming |
| `context.trim_to_percent` | `integer` | no | `60` | Percentage to trim context to when it gets too large This field is maintained for compatibility but no longer used for trimming |
| `custom_providers` | `array` | no | `[]` | User-defined OpenAI-compatible provider endpoints. These entries are editable in `/config` and appear in the model picker using each entry's `display_name`. Non-empty values from repository-controlled workspace/project layers are rejected; define provider endpoints in trusted system/user or explicitly selected config. |
| `custom_providers[].api_format` | `string` | no | `-` | Typed API format for the provider's default profile. |
| `custom_providers[].api_key_env` | `string` | no | `""` | Environment variable name that holds the API key for this endpoint (e.g., "MYCORP_API_KEY"). |
| `custom_providers[].auth` | `CustomProviderCommandAuthConfig \| null` | no | `-` | Optional command-backed bearer token configuration. |
| `custom_providers[].auth.args` | `array` | no | `[]` | Optional command arguments. |
| `custom_providers[].auth.args[]` | `string` | no | `-` | - |
| `custom_providers[].auth.command` | `string` | yes | `-` | Command to execute. Bare names are resolved via `PATH`. Command-backed auth is accepted only from trusted system/user or explicitly selected configuration; repository-controlled workspace/project values are rejected. |
| `custom_providers[].auth.cwd` | `null \| string` | no | `null` | Optional working directory for the token command. |
| `custom_providers[].auth.refresh_interval_ms` | `integer` | no | `300000` | Maximum age for the cached token before rerunning the command. |
| `custom_providers[].auth.timeout_ms` | `integer` | no | `5000` | Maximum time to wait for the command to complete successfully. |
| `custom_providers[].base_url` | `string` | yes | `-` | Base URL of the OpenAI-compatible API endpoint (e.g., `<https://llm.corp.example/v1>`). Non-empty custom providers from repository-controlled workspace/project layers are rejected. |
| `custom_providers[].context_window` | `integer \| null` | no | `-` | Optional context window size in tokens for models served by this endpoint. When omitted, the OpenAI-compatible provider uses its default context window size. |
| `custom_providers[].display_name` | `string` | yes | `-` | Human-friendly label shown in the TUI header, footer, and model picker (e.g., "MyCorporateName"). |
| `custom_providers[].frequency_penalty` | `null \| number` | no | `-` | Optional frequency penalty default (-2.0-2.0). |
| `custom_providers[].max_tokens` | `integer \| null` | no | `-` | Optional max output tokens default (> 0). |
| `custom_providers[].model` | `string` | no | `""` | Default model to use with this endpoint (e.g., "gpt-5-mini"). When [`models`](Self::models) is empty, this single model is what the `/model` picker offers for this provider. When [`models`](Self::models) is non-empty, this field is used as the default selection but the picker lists every entry in [`models`](Self::models). |
| `custom_providers[].models` | `array` | no | `[]` | Optional list of additional model identifiers offered by the provider. Useful for OpenAI-compatible aggregators such as Atlas Cloud that expose many models behind a single endpoint. When set, the `/model` picker shows one entry per model. When empty, the picker falls back to the single [`model`](Self::model) field. |
| `custom_providers[].models[]` | `string` | no | `-` | - |
| `custom_providers[].name` | `string` | yes | `-` | Stable provider key used for routing and persistence (e.g., "mycorp"). Must be lowercase alphanumeric with optional hyphens/underscores. |
| `custom_providers[].presence_penalty` | `null \| number` | no | `-` | Optional presence penalty default (-2.0-2.0). |
| `custom_providers[].profiles` | `object` | no | `-` | Exact model-keyed sparse capability profiles. |
| `custom_providers[].profiles.*.api_format` | `string` | no | `-` | Typed API format for this provider/profile. |
| `custom_providers[].profiles.*.context_window` | `integer \| null` | no | `-` | Optional context window size in tokens. |
| `custom_providers[].profiles.*.frequency_penalty` | `null \| number` | no | `-` | Optional frequency penalty override (-2.0-2.0). |
| `custom_providers[].profiles.*.max_tokens` | `integer \| null` | no | `-` | Optional max output tokens override (> 0). |
| `custom_providers[].profiles.*.presence_penalty` | `null \| number` | no | `-` | Optional presence penalty override (-2.0-2.0). |
| `custom_providers[].profiles.*.reasoning_effort` | `ReasoningEffortLevel \| null` | no | `-` | Optional reasoning effort override sent with requests for this model. |
| `custom_providers[].profiles.*.supports_context_caching` | `boolean \| null` | no | `-` | - |
| `custom_providers[].profiles.*.supports_context_edits` | `boolean \| null` | no | `-` | - |
| `custom_providers[].profiles.*.supports_parallel_tool_calls` | `boolean \| null` | no | `-` | - |
| `custom_providers[].profiles.*.supports_reasoning` | `boolean \| null` | no | `-` | - |
| `custom_providers[].profiles.*.supports_reasoning_effort` | `boolean \| null` | no | `-` | - |
| `custom_providers[].profiles.*.supports_responses_compaction` | `boolean \| null` | no | `-` | - |
| `custom_providers[].profiles.*.supports_structured_output` | `boolean \| null` | no | `-` | - |
| `custom_providers[].profiles.*.supports_tools` | `boolean \| null` | no | `-` | - |
| `custom_providers[].profiles.*.supports_vision` | `boolean \| null` | no | `-` | - |
| `custom_providers[].profiles.*.temperature` | `null \| number` | no | `-` | Optional sampling temperature override (0.0-2.0) sent with requests. |
| `custom_providers[].profiles.*.top_k` | `integer \| null` | no | `-` | Optional top-k override (>= 0). |
| `custom_providers[].profiles.*.top_p` | `null \| number` | no | `-` | Optional nucleus-sampling override (0.0-1.0). |
| `custom_providers[].reasoning_effort` | `ReasoningEffortLevel \| null` | no | `-` | Optional reasoning effort default for models without a profile override. |
| `custom_providers[].supports_context_caching` | `boolean \| null` | no | `-` | Optional support for context caching. |
| `custom_providers[].supports_context_edits` | `boolean \| null` | no | `-` | Optional support for context edits. |
| `custom_providers[].supports_parallel_tool_calls` | `boolean \| null` | no | `-` | Optional support for parallel tool calls. |
| `custom_providers[].supports_reasoning` | `boolean \| null` | no | `-` | Optional support for reasoning. |
| `custom_providers[].supports_reasoning_effort` | `boolean \| null` | no | `-` | Optional support for reasoning effort. |
| `custom_providers[].supports_responses_compaction` | `boolean \| null` | no | `-` | Optional support for responses compaction. |
| `custom_providers[].supports_structured_output` | `boolean \| null` | no | `-` | Optional support for structured output. |
| `custom_providers[].supports_tools` | `boolean \| null` | no | `-` | Optional support for tool calling. |
| `custom_providers[].supports_vision` | `boolean \| null` | no | `-` | Optional support for vision inputs. |
| `custom_providers[].temperature` | `null \| number` | no | `-` | Optional sampling temperature default (0.0-2.0) for models served by this endpoint unless a profile overrides it. |
| `custom_providers[].top_k` | `integer \| null` | no | `-` | Optional top-k default (>= 0). |
| `custom_providers[].top_p` | `null \| number` | no | `-` | Optional nucleus-sampling default (0.0-1.0). |
| `debug.debug_log_dir` | `null \| string` | no | `null` | Directory for debug logs |
| `debug.enable_tracing` | `boolean` | no | `false` | Enable structured logging for development and troubleshooting |
| `debug.max_debug_log_age_days` | `integer` | no | `7` | Maximum age of debug logs to keep (in days) |
| `debug.max_debug_log_size_mb` | `integer` | no | `50` | Maximum size of debug logs before rotating (in MB) |
| `debug.trace_level` | `string` | no | `"info"` | Trace level (error, warn, info, debug, trace) |
| `debug.trace_targets` | `array` | no | `[]` | List of tracing targets to enable Examples: "vtcode_core::agent", "vtcode_core::tools", "vtcode::*" |
| `debug.trace_targets[]` | `string` | no | `-` | - |
| `default_primary_agent` | `string` | no | `"build"` | Primary agent selected at startup when no session override is active. |
| `dotfile_protection.additional_protected_patterns` | `array` | no | `[]` | Additional dotfile patterns to protect (beyond defaults). |
| `dotfile_protection.additional_protected_patterns[]` | `string` | no | `-` | - |
| `dotfile_protection.audit_log_path` | `string` | no | `"/home/f2/.local/state/vtcode/audit/dotfiles.log"` | Path to the audit log file. |
| `dotfile_protection.audit_logging_enabled` | `boolean` | no | `true` | Enable immutable audit logging of all dotfile access attempts. |
| `dotfile_protection.backup_directory` | `string` | no | `"/home/f2/.local/state/vtcode/backups/dotfiles"` | Directory for storing dotfile backups. |
| `dotfile_protection.block_during_automation` | `boolean` | no | `true` | Block modifications during automated operations. |
| `dotfile_protection.blocked_operations` | `array` | no | `["dependency_installation", "code_formatting", "git_operations", "project_initialization", "build_operations", "test_...` | Operations that trigger extra protection. |
| `dotfile_protection.blocked_operations[]` | `string` | no | `-` | - |
| `dotfile_protection.create_backups` | `boolean` | no | `true` | Create backup before any permitted modification. |
| `dotfile_protection.enabled` | `boolean` | no | `true` | Enable dotfile protection globally. |
| `dotfile_protection.max_backups_per_file` | `integer` | no | `10` | Maximum number of backups to retain per file. |
| `dotfile_protection.preserve_permissions` | `boolean` | no | `true` | Preserve original file permissions and ownership. |
| `dotfile_protection.prevent_cascading_modifications` | `boolean` | no | `true` | Prevent cascading modifications (one dotfile change triggering others). |
| `dotfile_protection.require_explicit_confirmation` | `boolean` | no | `true` | Require explicit user confirmation for any dotfile modification. |
| `dotfile_protection.require_secondary_auth_for_whitelist` | `boolean` | no | `true` | Secondary authentication required for whitelisted files. |
| `dotfile_protection.whitelist` | `array` | no | `[]` | Whitelisted dotfiles that can be modified (after secondary confirmation). |
| `dotfile_protection.whitelist[]` | `string` | no | `-` | - |
| `features.memories` | `boolean` | no | `false` | Master toggle for the memories subsystem. When true, VT Code extracts durable context from completed threads and injects it into future sessions. |
| `file_opener` | `string` | no | `"none"` | Codex-compatible clickable citation URI scheme. |
| `history.max_bytes` | `integer \| null` | no | `null` | - |
| `history.persistence` | `string` | no | `"file"` | Local history persistence mode. |
| `hooks.lifecycle.notification` | `array` | no | `[]` | Commands to run when VT Code emits a runtime notification event |
| `hooks.lifecycle.notification[].hooks` | `array` | no | `[]` | List of hook commands to execute sequentially in this group |
| `hooks.lifecycle.notification[].hooks[].command` | `string` | no | `""` | The shell command string to execute |
| `hooks.lifecycle.notification[].hooks[].timeout_seconds` | `integer \| null` | no | `null` | Optional execution timeout in seconds |
| `hooks.lifecycle.notification[].hooks[].type` | `string` | no | `"command"` | Type of hook command (currently only 'command' is supported) |
| `hooks.lifecycle.notification[].matcher` | `null \| string` | no | `null` | Optional regex matcher to filter when this group runs. Matched against context strings (e.g. tool name, project path). |
| `hooks.lifecycle.permission_request` | `array` | no | `[]` | Commands to run when VT Code is about to request interactive permission approval |
| `hooks.lifecycle.permission_request[].hooks` | `array` | no | `[]` | List of hook commands to execute sequentially in this group |
| `hooks.lifecycle.permission_request[].hooks[].command` | `string` | no | `""` | The shell command string to execute |
| `hooks.lifecycle.permission_request[].hooks[].timeout_seconds` | `integer \| null` | no | `null` | Optional execution timeout in seconds |
| `hooks.lifecycle.permission_request[].hooks[].type` | `string` | no | `"command"` | Type of hook command (currently only 'command' is supported) |
| `hooks.lifecycle.permission_request[].matcher` | `null \| string` | no | `null` | Optional regex matcher to filter when this group runs. Matched against context strings (e.g. tool name, project path). |
| `hooks.lifecycle.post_tool_use` | `array` | no | `[]` | Commands to run immediately after a tool returns its output |
| `hooks.lifecycle.post_tool_use[].hooks` | `array` | no | `[]` | List of hook commands to execute sequentially in this group |
| `hooks.lifecycle.post_tool_use[].hooks[].command` | `string` | no | `""` | The shell command string to execute |
| `hooks.lifecycle.post_tool_use[].hooks[].timeout_seconds` | `integer \| null` | no | `null` | Optional execution timeout in seconds |
| `hooks.lifecycle.post_tool_use[].hooks[].type` | `string` | no | `"command"` | Type of hook command (currently only 'command' is supported) |
| `hooks.lifecycle.post_tool_use[].matcher` | `null \| string` | no | `null` | Optional regex matcher to filter when this group runs. Matched against context strings (e.g. tool name, project path). |
| `hooks.lifecycle.pre_compact` | `array` | no | `[]` | Commands to run immediately before VT Code compacts conversation history |
| `hooks.lifecycle.pre_compact[].hooks` | `array` | no | `[]` | List of hook commands to execute sequentially in this group |
| `hooks.lifecycle.pre_compact[].hooks[].command` | `string` | no | `""` | The shell command string to execute |
| `hooks.lifecycle.pre_compact[].hooks[].timeout_seconds` | `integer \| null` | no | `null` | Optional execution timeout in seconds |
| `hooks.lifecycle.pre_compact[].hooks[].type` | `string` | no | `"command"` | Type of hook command (currently only 'command' is supported) |
| `hooks.lifecycle.pre_compact[].matcher` | `null \| string` | no | `null` | Optional regex matcher to filter when this group runs. Matched against context strings (e.g. tool name, project path). |
| `hooks.lifecycle.pre_tool_use` | `array` | no | `[]` | Commands to run immediately before a tool is executed |
| `hooks.lifecycle.pre_tool_use[].hooks` | `array` | no | `[]` | List of hook commands to execute sequentially in this group |
| `hooks.lifecycle.pre_tool_use[].hooks[].command` | `string` | no | `""` | The shell command string to execute |
| `hooks.lifecycle.pre_tool_use[].hooks[].timeout_seconds` | `integer \| null` | no | `null` | Optional execution timeout in seconds |
| `hooks.lifecycle.pre_tool_use[].hooks[].type` | `string` | no | `"command"` | Type of hook command (currently only 'command' is supported) |
| `hooks.lifecycle.pre_tool_use[].matcher` | `null \| string` | no | `null` | Optional regex matcher to filter when this group runs. Matched against context strings (e.g. tool name, project path). |
| `hooks.lifecycle.quiet_success_output` | `boolean` | no | `false` | Suppress plain stdout from successful hooks unless they emit structured fields |
| `hooks.lifecycle.session_end` | `array` | no | `[]` | Commands to run when an agent session ends |
| `hooks.lifecycle.session_end[].hooks` | `array` | no | `[]` | List of hook commands to execute sequentially in this group |
| `hooks.lifecycle.session_end[].hooks[].command` | `string` | no | `""` | The shell command string to execute |
| `hooks.lifecycle.session_end[].hooks[].timeout_seconds` | `integer \| null` | no | `null` | Optional execution timeout in seconds |
| `hooks.lifecycle.session_end[].hooks[].type` | `string` | no | `"command"` | Type of hook command (currently only 'command' is supported) |
| `hooks.lifecycle.session_end[].matcher` | `null \| string` | no | `null` | Optional regex matcher to filter when this group runs. Matched against context strings (e.g. tool name, project path). |
| `hooks.lifecycle.session_start` | `array` | no | `[]` | Commands to run immediately when an agent session begins |
| `hooks.lifecycle.session_start[].hooks` | `array` | no | `[]` | List of hook commands to execute sequentially in this group |
| `hooks.lifecycle.session_start[].hooks[].command` | `string` | no | `""` | The shell command string to execute |
| `hooks.lifecycle.session_start[].hooks[].timeout_seconds` | `integer \| null` | no | `null` | Optional execution timeout in seconds |
| `hooks.lifecycle.session_start[].hooks[].type` | `string` | no | `"command"` | Type of hook command (currently only 'command' is supported) |
| `hooks.lifecycle.session_start[].matcher` | `null \| string` | no | `null` | Optional regex matcher to filter when this group runs. Matched against context strings (e.g. tool name, project path). |
| `hooks.lifecycle.stop` | `array` | no | `[]` | Commands to run after VT Code produces a final answer but before the turn completes |
| `hooks.lifecycle.stop[].hooks` | `array` | no | `[]` | List of hook commands to execute sequentially in this group |
| `hooks.lifecycle.stop[].hooks[].command` | `string` | no | `""` | The shell command string to execute |
| `hooks.lifecycle.stop[].hooks[].timeout_seconds` | `integer \| null` | no | `null` | Optional execution timeout in seconds |
| `hooks.lifecycle.stop[].hooks[].type` | `string` | no | `"command"` | Type of hook command (currently only 'command' is supported) |
| `hooks.lifecycle.stop[].matcher` | `null \| string` | no | `null` | Optional regex matcher to filter when this group runs. Matched against context strings (e.g. tool name, project path). |
| `hooks.lifecycle.subagent_start` | `array` | no | `[]` | Commands to run when a delegated subagent starts |
| `hooks.lifecycle.subagent_start[].hooks` | `array` | no | `[]` | List of hook commands to execute sequentially in this group |
| `hooks.lifecycle.subagent_start[].hooks[].command` | `string` | no | `""` | The shell command string to execute |
| `hooks.lifecycle.subagent_start[].hooks[].timeout_seconds` | `integer \| null` | no | `null` | Optional execution timeout in seconds |
| `hooks.lifecycle.subagent_start[].hooks[].type` | `string` | no | `"command"` | Type of hook command (currently only 'command' is supported) |
| `hooks.lifecycle.subagent_start[].matcher` | `null \| string` | no | `null` | Optional regex matcher to filter when this group runs. Matched against context strings (e.g. tool name, project path). |
| `hooks.lifecycle.subagent_stop` | `array` | no | `[]` | Commands to run when a delegated subagent stops |
| `hooks.lifecycle.subagent_stop[].hooks` | `array` | no | `[]` | List of hook commands to execute sequentially in this group |
| `hooks.lifecycle.subagent_stop[].hooks[].command` | `string` | no | `""` | The shell command string to execute |
| `hooks.lifecycle.subagent_stop[].hooks[].timeout_seconds` | `integer \| null` | no | `null` | Optional execution timeout in seconds |
| `hooks.lifecycle.subagent_stop[].hooks[].type` | `string` | no | `"command"` | Type of hook command (currently only 'command' is supported) |
| `hooks.lifecycle.subagent_stop[].matcher` | `null \| string` | no | `null` | Optional regex matcher to filter when this group runs. Matched against context strings (e.g. tool name, project path). |
| `hooks.lifecycle.task_completed` | `array` | no | `[]` | Deprecated alias for `stop` |
| `hooks.lifecycle.task_completed[].hooks` | `array` | no | `[]` | List of hook commands to execute sequentially in this group |
| `hooks.lifecycle.task_completed[].hooks[].command` | `string` | no | `""` | The shell command string to execute |
| `hooks.lifecycle.task_completed[].hooks[].timeout_seconds` | `integer \| null` | no | `null` | Optional execution timeout in seconds |
| `hooks.lifecycle.task_completed[].hooks[].type` | `string` | no | `"command"` | Type of hook command (currently only 'command' is supported) |
| `hooks.lifecycle.task_completed[].matcher` | `null \| string` | no | `null` | Optional regex matcher to filter when this group runs. Matched against context strings (e.g. tool name, project path). |
| `hooks.lifecycle.task_completion` | `array` | no | `[]` | Deprecated alias for `stop` |
| `hooks.lifecycle.task_completion[].hooks` | `array` | no | `[]` | List of hook commands to execute sequentially in this group |
| `hooks.lifecycle.task_completion[].hooks[].command` | `string` | no | `""` | The shell command string to execute |
| `hooks.lifecycle.task_completion[].hooks[].timeout_seconds` | `integer \| null` | no | `null` | Optional execution timeout in seconds |
| `hooks.lifecycle.task_completion[].hooks[].type` | `string` | no | `"command"` | Type of hook command (currently only 'command' is supported) |
| `hooks.lifecycle.task_completion[].matcher` | `null \| string` | no | `null` | Optional regex matcher to filter when this group runs. Matched against context strings (e.g. tool name, project path). |
| `hooks.lifecycle.user_prompt_submit` | `array` | no | `[]` | Commands to run when the user submits a prompt (pre-processing) |
| `hooks.lifecycle.user_prompt_submit[].hooks` | `array` | no | `[]` | List of hook commands to execute sequentially in this group |
| `hooks.lifecycle.user_prompt_submit[].hooks[].command` | `string` | no | `""` | The shell command string to execute |
| `hooks.lifecycle.user_prompt_submit[].hooks[].timeout_seconds` | `integer \| null` | no | `null` | Optional execution timeout in seconds |
| `hooks.lifecycle.user_prompt_submit[].hooks[].type` | `string` | no | `"command"` | Type of hook command (currently only 'command' is supported) |
| `hooks.lifecycle.user_prompt_submit[].matcher` | `null \| string` | no | `null` | Optional regex matcher to filter when this group runs. Matched against context strings (e.g. tool name, project path). |
| `ide_context.enabled` | `boolean` | no | `true` | - |
| `ide_context.include_selection_text` | `boolean` | no | `true` | - |
| `ide_context.inject_into_prompt` | `boolean` | no | `true` | Inject active editor context into request-time model input. |
| `ide_context.provider_mode` | `string` | no | `"auto"` | - |
| `ide_context.providers.generic.enabled` | `boolean` | no | `true` | - |
| `ide_context.providers.vscode_compatible.enabled` | `boolean` | no | `true` | - |
| `ide_context.providers.zed.enabled` | `boolean` | no | `true` | - |
| `ide_context.show_in_tui` | `boolean` | no | `true` | - |
| `mcp.allowlist.default.configuration` | `null \| object` | no | `null` | Configuration keys permitted for the provider grouped by category |
| `mcp.allowlist.default.logging` | `array \| null` | no | `null` | Logging channels permitted for the provider |
| `mcp.allowlist.default.logging[]` | `string` | no | `-` | - |
| `mcp.allowlist.default.prompts` | `array \| null` | no | `null` | Prompt name patterns permitted for the provider |
| `mcp.allowlist.default.prompts[]` | `string` | no | `-` | - |
| `mcp.allowlist.default.resources` | `array \| null` | no | `null` | Resource name patterns permitted for the provider |
| `mcp.allowlist.default.resources[]` | `string` | no | `-` | - |
| `mcp.allowlist.default.tools` | `array \| null` | no | `null` | Tool name patterns permitted for the provider |
| `mcp.allowlist.default.tools[]` | `string` | no | `-` | - |
| `mcp.allowlist.enforce` | `boolean` | no | `false` | Whether to enforce allow list checks |
| `mcp.allowlist.providers` | `object` | no | `{}` | Provider-specific allow list rules |
| `mcp.allowlist.providers.*.configuration` | `null \| object` | no | `null` | Configuration keys permitted for the provider grouped by category |
| `mcp.allowlist.providers.*.logging` | `array \| null` | no | `null` | Logging channels permitted for the provider |
| `mcp.allowlist.providers.*.logging[]` | `string` | no | `-` | - |
| `mcp.allowlist.providers.*.prompts` | `array \| null` | no | `null` | Prompt name patterns permitted for the provider |
| `mcp.allowlist.providers.*.prompts[]` | `string` | no | `-` | - |
| `mcp.allowlist.providers.*.resources` | `array \| null` | no | `null` | Resource name patterns permitted for the provider |
| `mcp.allowlist.providers.*.resources[]` | `string` | no | `-` | - |
| `mcp.allowlist.providers.*.tools` | `array \| null` | no | `null` | Tool name patterns permitted for the provider |
| `mcp.allowlist.providers.*.tools[]` | `string` | no | `-` | - |
| `mcp.connection_pooling_enabled` | `boolean` | no | `true` | Enable connection pooling for better performance |
| `mcp.connection_timeout_seconds` | `integer` | no | `30` | Connection timeout in seconds |
| `mcp.enabled` | `boolean` | no | `false` | Enable MCP functionality |
| `mcp.experimental_use_rmcp_client` | `boolean` | no | `true` | Toggle experimental RMCP client features |
| `mcp.lifecycle.allow_model_control` | `boolean` | no | `false` | Allow model-callable tools to connect/disconnect MCP servers at runtime. |
| `mcp.max_concurrent_connections` | `integer` | no | `5` | Maximum number of concurrent MCP connections |
| `mcp.providers` | `array` | no | `[]` | Configured MCP providers |
| `mcp.providers[]` | `object` | no | `-` | Configuration for a single MCP provider |
| `mcp.providers[].api_key_env` | `null \| string` | no | `null` | API key environment variable name |
| `mcp.providers[].args` | `array` | yes | `-` | Command arguments |
| `mcp.providers[].args[]` | `string` | no | `-` | - |
| `mcp.providers[].command` | `string` | yes | `-` | Command to execute |
| `mcp.providers[].enabled` | `boolean` | no | `true` | Whether this provider is enabled |
| `mcp.providers[].endpoint` | `string` | yes | `-` | Server endpoint URL |
| `mcp.providers[].env` | `object` | no | `{}` | Provider-specific environment variables |
| `mcp.providers[].env.*` | `string` | no | `-` | - |
| `mcp.providers[].env_http_headers` | `object` | no | `{}` | Headers whose values are sourced from environment variables (`{ header-name = "ENV_VAR" }`). Empty values are ignored. |
| `mcp.providers[].env_http_headers.*` | `string` | no | `-` | - |
| `mcp.providers[].http_headers` | `object` | no | `{}` | Headers to include in requests |
| `mcp.providers[].http_headers.*` | `string` | no | `-` | - |
| `mcp.providers[].max_concurrent_requests` | `integer` | no | `3` | Maximum number of concurrent requests to this provider |
| `mcp.providers[].name` | `string` | yes | `-` | Provider name (used for identification) |
| `mcp.providers[].oauth` | `McpOAuthConfig \| null` | no | `null` | Optional OAuth configuration for providers that issue bearer tokens dynamically. |
| `mcp.providers[].oauth.audience` | `null \| string` | no | `null` | Optional audience/resource hint sent with the auth and token requests. |
| `mcp.providers[].oauth.authorization_url` | `string` | no | `""` | OAuth authorization endpoint. |
| `mcp.providers[].oauth.callback_port` | `integer` | no | `8768` | Local callback server port. |
| `mcp.providers[].oauth.client_id` | `string` | no | `""` | OAuth client identifier. |
| `mcp.providers[].oauth.credentials_store_mode` | `string` | no | `"auto"` | Credential storage backend for this provider's token. |
| `mcp.providers[].oauth.extra_auth_params` | `object` | no | `{}` | Extra query parameters appended to the authorization URL. |
| `mcp.providers[].oauth.extra_auth_params.*` | `string` | no | `-` | - |
| `mcp.providers[].oauth.extra_token_params` | `object` | no | `{}` | Extra form fields appended to token exchanges and refreshes. |
| `mcp.providers[].oauth.extra_token_params.*` | `string` | no | `-` | - |
| `mcp.providers[].oauth.flow_timeout_secs` | `integer` | no | `300` | Browser-flow timeout in seconds. |
| `mcp.providers[].oauth.scopes` | `array` | no | `[]` | Requested scopes. |
| `mcp.providers[].oauth.scopes[]` | `string` | no | `-` | - |
| `mcp.providers[].oauth.token_url` | `string` | no | `""` | OAuth token endpoint. |
| `mcp.providers[].protocol_version` | `string` | no | `"2024-11-05"` | Protocol version |
| `mcp.providers[].startup_timeout_ms` | `integer \| null` | no | `null` | Startup timeout in milliseconds for this provider |
| `mcp.providers[].working_directory` | `null \| string` | no | `null` | Working directory for the command |
| `mcp.request_timeout_seconds` | `integer` | no | `30` | Request timeout in seconds |
| `mcp.requirements.allowed_http_endpoints` | `array` | no | `[]` | Allowed HTTP endpoints when enforcement is enabled |
| `mcp.requirements.allowed_http_endpoints[]` | `string` | no | `-` | - |
| `mcp.requirements.allowed_stdio_commands` | `array` | no | `[]` | Allowed stdio command names when enforcement is enabled |
| `mcp.requirements.allowed_stdio_commands[]` | `string` | no | `-` | - |
| `mcp.requirements.enforce` | `boolean` | no | `false` | Whether requirement checks are enforced |
| `mcp.retry_attempts` | `integer` | no | `3` | Connection retry attempts |
| `mcp.security.api_key_env` | `null \| string` | no | `null` | API key for MCP server authentication (environment variable name) |
| `mcp.security.auth_enabled` | `boolean` | no | `false` | Enable authentication for MCP server |
| `mcp.security.rate_limit.concurrent_requests` | `integer` | no | `10` | Maximum concurrent requests per client |
| `mcp.security.rate_limit.requests_per_minute` | `integer` | no | `100` | Maximum requests per minute per client |
| `mcp.security.validation.max_argument_size` | `integer` | no | `1048576` | Maximum argument size in bytes |
| `mcp.security.validation.path_traversal_protection` | `boolean` | no | `true` | Enable path traversal protection |
| `mcp.security.validation.schema_validation_enabled` | `boolean` | no | `true` | Enable JSON schema validation for tool arguments |
| `mcp.server.bind_address` | `string` | no | `"127.0.0.1"` | Bind address for the MCP server |
| `mcp.server.enabled` | `boolean` | no | `false` | Enable vtcode's MCP server capability |
| `mcp.server.exposed_tools` | `array` | no | `[]` | Tools exposed by the vtcode MCP server |
| `mcp.server.exposed_tools[]` | `string` | no | `-` | - |
| `mcp.server.name` | `string` | no | `"vtcode-mcp-server"` | Server identifier |
| `mcp.server.port` | `integer` | no | `3000` | Port for the MCP server |
| `mcp.server.transport` | `string` | no | `"sse"` | Server transport type |
| `mcp.server.version` | `string` | no | `"0.147.2"` | Server version |
| `mcp.startup_timeout_seconds` | `integer \| null` | no | `null` | Optional timeout (seconds) when starting providers |
| `mcp.tool_cache_capacity` | `integer` | no | `100` | Cache capacity for tool discovery results |
| `mcp.tool_timeout_seconds` | `integer \| null` | no | `null` | Optional timeout (seconds) for tool execution |
| `mcp.ui.max_events` | `integer` | no | `50` | Maximum number of MCP events to display |
| `mcp.ui.mode` | `string` | no | `"compact"` | UI mode for MCP events: "compact" or "full" |
| `mcp.ui.renderers` | `object` | no | `{}` | Custom renderer profiles for provider-specific output formatting |
| `mcp.ui.renderers.*` | `string` | no | `-` | Named renderer profiles for MCP tool output formatting |
| `mcp.ui.show_provider_names` | `boolean` | no | `true` | Show MCP provider names in UI |
| `model.loop_detection_interactive` | `boolean` | no | `true` | Enable interactive prompt for loop detection instead of silently halting |
| `model.loop_detection_threshold` | `integer` | no | `2` | Maximum number of identical tool calls (same tool + same arguments) before triggering loop detection |
| `model.model_supports_reasoning` | `boolean \| null` | no | `null` | Manually enable reasoning support for models that are not natively recognized. Note: Setting this to false will NOT disable reasoning for known reasoning models (e.g. GPT-5). |
| `model.model_supports_reasoning_effort` | `boolean \| null` | no | `null` | Manually enable reasoning effort support for models that are not natively recognized. Note: Setting this to false will NOT disable reasoning effort for known models. |
| `model.skip_loop_detection` | `boolean` | no | `false` | Enable loop hang detection to identify when model is stuck in repetitive behavior |
| `notify` | `array` | no | `[]` | External notification command invoked for supported events. |
| `notify[]` | `string` | no | `-` | - |
| `optimization.agent_execution.enable_performance_prediction` | `boolean` | no | `false` | Enable performance prediction |
| `optimization.agent_execution.idle_backoff_ms` | `integer` | no | `100` | Back-off duration in milliseconds when no work is pending This reduces CPU usage during idle periods |
| `optimization.agent_execution.idle_timeout_ms` | `integer` | no | `5000` | Idle detection timeout in milliseconds (0 to disable) When the agent is idle for this duration, it will enter a low-power state |
| `optimization.agent_execution.max_execution_time_secs` | `integer` | no | `600` | Maximum execution time in seconds |
| `optimization.agent_execution.max_idle_cycles` | `integer` | no | `10` | Maximum consecutive idle cycles before entering deep sleep |
| `optimization.agent_execution.max_memory_mb` | `integer` | no | `1024` | Maximum memory usage in MB |
| `optimization.agent_execution.resource_monitor_interval_ms` | `integer` | no | `100` | Resource monitoring interval in milliseconds |
| `optimization.agent_execution.state_history_size` | `integer` | no | `1000` | State transition history size |
| `optimization.agent_execution.use_optimized_loop` | `boolean` | no | `true` | Enable optimized agent execution loop |
| `optimization.async_pipeline.batch_timeout_ms` | `integer` | no | `100` | Batch timeout in milliseconds |
| `optimization.async_pipeline.cache_size` | `integer` | no | `100` | Result cache size |
| `optimization.async_pipeline.enable_batching` | `boolean` | no | `false` | Enable request batching |
| `optimization.async_pipeline.enable_caching` | `boolean` | no | `true` | Enable result caching |
| `optimization.async_pipeline.max_batch_size` | `integer` | no | `5` | Maximum batch size for tool requests |
| `optimization.command_cache.allowlist` | `array` | no | `["rg", "ls", "git status", "git diff --stat"]` | Allowlist of command prefixes eligible for caching |
| `optimization.command_cache.allowlist[]` | `string` | no | `-` | - |
| `optimization.command_cache.enabled` | `boolean` | no | `true` | Enable command caching |
| `optimization.command_cache.max_entries` | `integer` | no | `128` | Maximum number of cached entries |
| `optimization.command_cache.ttl_ms` | `integer` | no | `2000` | Cache TTL in milliseconds |
| `optimization.file_read_cache.enabled` | `boolean` | no | `true` | Enable file read caching |
| `optimization.file_read_cache.max_entries` | `integer` | no | `128` | Maximum number of cached entries |
| `optimization.file_read_cache.max_read_lines` | `integer` | no | `400` | Absolute ceiling (in lines) for a single line-based `read_file` call. Any read requesting more lines is clamped to this value and the response exposes a `next_read_args` continuation to read the remainder. |
| `optimization.file_read_cache.max_size_bytes` | `integer` | no | `10485760` | Maximum cached file size (bytes) |
| `optimization.file_read_cache.min_size_bytes` | `integer` | no | `262144` | Minimum file size (bytes) before caching |
| `optimization.file_read_cache.ttl_secs` | `integer` | no | `300` | Cache TTL in seconds |
| `optimization.llm_client.cache_ttl_secs` | `integer` | no | `300` | Response cache TTL in seconds |
| `optimization.llm_client.connection_pool_size` | `integer` | no | `4` | Connection pool size |
| `optimization.llm_client.enable_connection_pooling` | `boolean` | no | `false` | Enable connection pooling |
| `optimization.llm_client.enable_request_batching` | `boolean` | no | `false` | Enable request batching |
| `optimization.llm_client.enable_response_caching` | `boolean` | no | `true` | Enable response caching |
| `optimization.llm_client.rate_limit_burst` | `integer` | no | `20` | Rate limit burst capacity |
| `optimization.llm_client.rate_limit_rps` | `number` | no | `10.0` | Rate limit: requests per second |
| `optimization.llm_client.response_cache_size` | `integer` | no | `50` | Response cache size |
| `optimization.memory_pool.enabled` | `boolean` | no | `true` | Enable memory pool (can be disabled for debugging) |
| `optimization.memory_pool.max_string_pool_size` | `integer` | no | `64` | Maximum number of strings to pool |
| `optimization.memory_pool.max_value_pool_size` | `integer` | no | `32` | Maximum number of Values to pool |
| `optimization.memory_pool.max_vec_pool_size` | `integer` | no | `16` | Maximum number of `Vec<String>` to pool |
| `optimization.profiling.auto_export_results` | `boolean` | no | `false` | Auto-export results to file |
| `optimization.profiling.enable_regression_testing` | `boolean` | no | `false` | Enable regression testing |
| `optimization.profiling.enabled` | `boolean` | no | `false` | Enable performance profiling |
| `optimization.profiling.export_file_path` | `string` | no | `"benchmark_results.json"` | Export file path |
| `optimization.profiling.max_history_size` | `integer` | no | `1000` | Maximum benchmark history size |
| `optimization.profiling.max_regression_percent` | `number` | no | `10.0` | Maximum allowed performance regression percentage |
| `optimization.profiling.monitor_interval_ms` | `integer` | no | `100` | Resource monitoring interval in milliseconds |
| `optimization.rl.epsilon` | `number` | no | `0.15` | Exploration constant for the bandit (higher = more exploration). |
| `optimization.rl.latency_weight` | `number` | no | `0.5` | Reward shaping weight for latency vs success trade-off (`0.0..=1.0`). |
| `optimization.rl.strategy` | `string` | no | `"bandit"` | Selection strategy: `bandit` (UCB / epsilon-greedy) or `actor_critic`. |
| `optimization.tool_registry.default_timeout_secs` | `integer` | no | `180` | Tool execution timeout in seconds |
| `optimization.tool_registry.hot_cache_size` | `integer` | no | `16` | Hot cache size for frequently used tools |
| `optimization.tool_registry.max_concurrent_tools` | `integer` | no | `4` | Maximum concurrent tool executions |
| `optimization.tool_registry.middleware_fail_open` | `boolean` | no | `false` | When `true`, middleware `before_execute` failures are logged but do not block the tool call (fail-open). When `false` (the default), middleware errors deny execution (fail-closed). |
| `optimization.tool_registry.use_optimized_registry` | `boolean` | no | `true` | Enable optimized registry |
| `output_style.active_style` | `string` | no | `"default"` | - |
| `permissions.allow` | `array` | no | `[]` | Rules that allow matching tool calls without prompting. |
| `permissions.allow[]` | `string` | no | `-` | - |
| `permissions.ask` | `array` | no | `[]` | Rules that require an interactive prompt when they match. |
| `permissions.ask[]` | `string` | no | `-` | - |
| `permissions.audit_directory` | `string` | no | `"/home/f2/.local/state/vtcode/audit"` | Directory for audit logs (created if not exists) Defaults to the VT Code state directory's `audit` subdirectory. |
| `permissions.audit_enabled` | `boolean` | no | `true` | Enable audit logging of all permission decisions |
| `permissions.auto.allow_exceptions` | `array` | no | `["Allow read-only tools and read-only browsing/search actions.", "Allow file edits and writes inside the current work...` | Narrow allow exceptions applied after block rules. |
| `permissions.auto.allow_exceptions[]` | `string` | no | `-` | - |
| `permissions.auto.block_rules` | `array` | no | `["Block destructive source-control actions such as force-pushes, direct pushes to protected branches, or remote branc...` | Classifier block rules applied in stage 2 reasoning. |
| `permissions.auto.block_rules[]` | `string` | no | `-` | - |
| `permissions.auto.drop_broad_allow_rules` | `boolean` | no | `true` | Drop broad code-execution allow rules while auto permission review is active. |
| `permissions.auto.environment.trusted_domains` | `array` | no | `[]` | - |
| `permissions.auto.environment.trusted_domains[]` | `string` | no | `-` | - |
| `permissions.auto.environment.trusted_git_hosts` | `array` | no | `[]` | - |
| `permissions.auto.environment.trusted_git_hosts[]` | `string` | no | `-` | - |
| `permissions.auto.environment.trusted_git_orgs` | `array` | no | `[]` | - |
| `permissions.auto.environment.trusted_git_orgs[]` | `string` | no | `-` | - |
| `permissions.auto.environment.trusted_paths` | `array` | no | `[]` | - |
| `permissions.auto.environment.trusted_paths[]` | `string` | no | `-` | - |
| `permissions.auto.environment.trusted_services` | `array` | no | `[]` | - |
| `permissions.auto.environment.trusted_services[]` | `string` | no | `-` | - |
| `permissions.auto.max_consecutive_denials` | `integer` | no | `3` | Maximum consecutive denials before auto permission review falls back. |
| `permissions.auto.max_total_denials` | `integer` | no | `20` | Maximum total denials before auto permission review falls back. |
| `permissions.auto.model` | `string` | no | `""` | Optional model override for the transcript reviewer. |
| `permissions.auto.probe_model` | `string` | no | `""` | Optional model override for the prompt-injection probe. |
| `permissions.cache_enabled` | `boolean` | no | `true` | Enable permission decision caching to avoid redundant evaluations |
| `permissions.cache_ttl_seconds` | `integer` | no | `300` | Cache time-to-live in seconds (how long to cache decisions) Default: 300 seconds (5 minutes) |
| `permissions.deny` | `array` | no | `[]` | Rules that deny matching tool calls. |
| `permissions.deny[]` | `string` | no | `-` | - |
| `permissions.enabled` | `boolean` | no | `true` | Enable the enhanced permission system (resolver + audit logger + cache) |
| `permissions.log_allowed_commands` | `boolean` | no | `true` | Log allowed commands to audit trail |
| `permissions.log_denied_commands` | `boolean` | no | `true` | Log denied commands to audit trail |
| `permissions.log_permission_prompts` | `boolean` | no | `true` | Log permission prompts (when user is asked for confirmation) |
| `permissions.resolve_commands` | `boolean` | no | `true` | Enable command resolution to actual paths (helps identify suspicious commands) |
| `prompt_cache.cache_dir` | `string` | no | `"/home/f2/.cache/vtcode/prompts"` | Base directory for local prompt cache storage (supports `~` expansion) |
| `prompt_cache.cache_friendly_prompt_shaping` | `boolean` | no | `true` | Enable prompt-shaping optimizations that improve provider-side cache locality. When enabled, VT Code keeps volatile runtime context at the end of prompt text. |
| `prompt_cache.enable_auto_cleanup` | `boolean` | no | `true` | Automatically evict stale entries on startup/shutdown |
| `prompt_cache.enabled` | `boolean` | no | `true` | Enable prompt caching features globally |
| `prompt_cache.gap_warning_enabled` | `boolean` | no | `true` | Warn before a request when the pause since the previous request likely exceeded the provider prompt cache lifetime (advisory only). |
| `prompt_cache.gap_warning_threshold_secs` | `integer \| null` | no | `null` | Override for the cache-gap warning threshold in seconds. When unset, a provider-specific default is used (Anthropic 300s, OpenAI 600s). |
| `prompt_cache.max_age_days` | `integer` | no | `30` | Maximum age (in days) before cached entries are purged |
| `prompt_cache.max_entries` | `integer` | no | `1000` | Maximum number of cached prompt entries to retain on disk |
| `prompt_cache.min_quality_threshold` | `number` | no | `0.7` | Minimum quality score required before persisting an entry |
| `prompt_cache.providers.anthropic.cache_system_messages` | `boolean` | no | `true` | Apply cache control to system prompts by default |
| `prompt_cache.providers.anthropic.cache_tool_definitions` | `boolean` | no | `true` | Apply cache control to tool definitions by default Default: true (tools are typically stable and benefit from longer caching) |
| `prompt_cache.providers.anthropic.cache_user_messages` | `boolean` | no | `true` | Apply cache control to user messages exceeding threshold |
| `prompt_cache.providers.anthropic.enabled` | `boolean` | no | `true` | - |
| `prompt_cache.providers.anthropic.extended_ttl_seconds` | `integer \| null` | no | `3600` | Extended TTL for Anthropic prompt caching (in seconds) Set to >= 3600 for 1-hour cache on messages |
| `prompt_cache.providers.anthropic.max_breakpoints` | `integer` | no | `4` | Maximum number of cache breakpoints to use (max 4 per Anthropic spec). Default: 4 |
| `prompt_cache.providers.anthropic.messages_ttl_seconds` | `integer` | no | `300` | TTL for subsequent cache breakpoints (messages). Set to >= 3600 for 1-hour cache on messages. Default: 300 (5 minutes) - recommended for frequently changing messages |
| `prompt_cache.providers.anthropic.min_message_length_for_cache` | `integer` | no | `256` | Minimum message length (in characters) before applying cache control to avoid caching very short messages that don't benefit from caching. Default: 256 characters (~64 tokens) |
| `prompt_cache.providers.anthropic.tools_ttl_seconds` | `integer` | no | `3600` | Default TTL in seconds for the first cache breakpoint (tools/system). Anthropic only supports "5m" (300s) or "1h" (3600s) TTL formats. Set to >= 3600 for 1-hour cache on tools and system prompts. Default: 3600 (1 hour) - recommended for stable tool definitions |
| `prompt_cache.providers.deepseek.enabled` | `boolean` | no | `true` | - |
| `prompt_cache.providers.deepseek.surface_metrics` | `boolean` | no | `true` | Emit cache hit/miss metrics from responses when available |
| `prompt_cache.providers.gemini.enabled` | `boolean` | no | `true` | - |
| `prompt_cache.providers.gemini.explicit_ttl_seconds` | `integer \| null` | no | `3600` | TTL for explicit caches (ignored in implicit mode) |
| `prompt_cache.providers.gemini.min_prefix_tokens` | `integer` | no | `1024` | - |
| `prompt_cache.providers.gemini.mode` | `string` | no | `"implicit"` | Gemini prompt caching mode selection |
| `prompt_cache.providers.moonshot.enabled` | `boolean` | no | `true` | - |
| `prompt_cache.providers.openai.enabled` | `boolean` | no | `true` | - |
| `prompt_cache.providers.openai.idle_expiration_seconds` | `integer` | no | `3600` | - |
| `prompt_cache.providers.openai.min_prefix_tokens` | `integer` | no | `1024` | - |
| `prompt_cache.providers.openai.prompt_cache_key_mode` | `string` | no | `"session"` | Strategy for generating OpenAI `prompt_cache_key`. Session-scoped keys derive one stable key per VT Code conversation. |
| `prompt_cache.providers.openai.prompt_cache_retention` | `PromptCacheRetention \| null` | no | `null` | Optional prompt cache retention policy to pass directly into OpenAI Responses API. Supported values are "in_memory" and "24h". If set, VT Code will include `prompt_cache_retention` in the request body to extend the model-side prompt caching window or request the explicit in-memory policy. |
| `prompt_cache.providers.openai.surface_metrics` | `boolean` | no | `true` | - |
| `prompt_cache.providers.openrouter.enabled` | `boolean` | no | `true` | - |
| `prompt_cache.providers.openrouter.propagate_provider_capabilities` | `boolean` | no | `true` | Propagate provider cache instructions automatically |
| `prompt_cache.providers.openrouter.report_savings` | `boolean` | no | `true` | Surface cache savings reported by OpenRouter |
| `prompt_cache.providers.zai.enabled` | `boolean` | no | `false` | - |
| `provider.anthropic.advisor.caching` | `AdvisorCachingConfig \| null` | no | `null` | Enables prompt caching for the advisor's own transcript across calls within a conversation. Only worthwhile for long agent loops (three or more expected advisor calls). |
| `provider.anthropic.advisor.caching.enabled` | `boolean` | no | `false` | Whether advisor-side prompt caching is enabled. |
| `provider.anthropic.advisor.caching.ttl` | `string` | no | `"5m"` | Cache lifetime for the advisor transcript. |
| `provider.anthropic.advisor.enabled` | `boolean` | no | `false` | Master toggle for the Anthropic server-side advisor tool. |
| `provider.anthropic.advisor.max_tokens` | `integer \| null` | no | `null` | Caps the advisor's total output (thinking plus text) per call. Minimum 1024. `None` lets the advisor model choose its own output cap. |
| `provider.anthropic.advisor.max_uses` | `integer \| null` | no | `null` | Maximum number of advisor invocations per request. `None` means unlimited (the API default). Once the executor reaches this cap, further advisor calls return an `advisor_tool_result_error`. |
| `provider.anthropic.advisor.model` | `string` | no | `""` | Advisor model id (must be an Anthropic/Claude model at least as capable as the executor). Empty or omitted falls back to a sensible default advisor for the executor model. |
| `provider.anthropic.count_tokens_enabled` | `boolean` | no | `false` | Enable token counting via the count_tokens endpoint When enabled, the agent can estimate input token counts before making API calls Useful for proactive management of rate limits and costs |
| `provider.anthropic.effort` | `string` | no | `"xhigh"` | Effort level for adaptive thinking/token usage (low, medium, high, xhigh, max) Controls how many tokens Claude uses when responding, trading off between response thoroughness and token efficiency. The default config value keeps Claude Opus 4.7 on `xhigh`; models that do not support `xhigh` fall back to their supported model default, typically `high`. |
| `provider.anthropic.extended_thinking_enabled` | `boolean` | no | `true` | Enable adaptive or extended thinking for Anthropic models When enabled, Claude uses internal reasoning before responding, providing enhanced reasoning capabilities for complex tasks. Only supported by Claude 4, Claude 4.5, and Claude 3.7 Sonnet models. Claude Opus 4.7 uses adaptive thinking instead of budgeted extended thinking. Note: Extended thinking is now auto-enabled by default (31,999 tokens). Set MAX_THINKING_TOKENS=63999 environment variable for 2x budget on 64K models. See: <https://docs.anthropic.com/en/docs/build-with-claude/extended-thinking> |
| `provider.anthropic.interleaved_thinking_beta` | `string` | no | `"interleaved-thinking-2025-05-14"` | Beta header for interleaved thinking feature |
| `provider.anthropic.interleaved_thinking_budget_tokens` | `integer` | no | `31999` | Budget tokens for extended thinking (minimum: 1024, default: 31999) On 64K output models (Opus 4.5, Sonnet 4.5, Haiku 4.5): default 31,999, max 63,999 On 32K output models (Opus 4): max 31,999 Claude Opus 4.7 ignores this setting and uses adaptive thinking instead. Use MAX_THINKING_TOKENS environment variable to override. |
| `provider.anthropic.interleaved_thinking_type_enabled` | `string` | no | `"enabled"` | Type value for enabling interleaved thinking |
| `provider.anthropic.memory.enabled` | `boolean` | no | `false` | - |
| `provider.anthropic.task_budget_beta` | `string` | no | `"task-budgets-2026-03-13"` | Beta header for Anthropic task budgets. |
| `provider.anthropic.task_budget_tokens` | `integer \| null` | no | `null` | Optional Anthropic task budget token total for Claude Opus 4.7. When set, VT Code sends `output_config.task_budget = { type = "tokens", total = N }` and the required beta header. Anthropic currently requires a minimum of 20,000 tokens. |
| `provider.anthropic.thinking_display` | `ThinkingDisplayMode \| null` | no | `null` | Controls how thinking content is returned in API responses. - "summarized": Thinking blocks contain summarized text (default on Opus 4.6 and earlier). - "omitted": Thinking blocks have an empty `thinking` field (default on Opus 4.7+). When set, this overrides the model-specific default. See: <https://docs.anthropic.com/en/docs/build-with-claude/extended-thinking#controlling-thinking-display> |
| `provider.anthropic.tool_search.algorithm` | `string` | no | `"regex"` | Search algorithm: "regex" (Python regex patterns) or "bm25" (natural language) |
| `provider.anthropic.tool_search.always_available_tools` | `array` | no | `[]` | Tool names that should never be deferred (always available) |
| `provider.anthropic.tool_search.always_available_tools[]` | `string` | no | `-` | - |
| `provider.anthropic.tool_search.defer_by_default` | `boolean` | no | `true` | Automatically defer loading of all tools except core tools |
| `provider.anthropic.tool_search.enabled` | `boolean` | no | `true` | Enable tool search feature (requires advanced-tool-use-2025-11-20 beta) |
| `provider.anthropic.tool_search.max_results` | `integer` | no | `5` | Maximum number of tool search results to return |
| `provider.mimo_auth_method` | `MiMoAuthMethod \| null` | no | `-` | Xiaomi MiMo auth method: "payg" or "token-plan" |
| `provider.openai.hosted_shell.container_id` | `null \| string` | no | `-` | Existing OpenAI container ID to reuse when `environment = "container_reference"`. |
| `provider.openai.hosted_shell.enabled` | `boolean` | no | `false` | Enable OpenAI hosted shell instead of VT Code's local shell tool. |
| `provider.openai.hosted_shell.environment` | `string` | no | `"container_auto"` | Environment provisioning mode for hosted shell. |
| `provider.openai.hosted_shell.file_ids` | `array` | no | `-` | File IDs to mount when using `container_auto`. |
| `provider.openai.hosted_shell.file_ids[]` | `string` | no | `-` | - |
| `provider.openai.hosted_shell.network_policy.allowed_domains` | `array` | no | `-` | - |
| `provider.openai.hosted_shell.network_policy.allowed_domains[]` | `string` | no | `-` | - |
| `provider.openai.hosted_shell.network_policy.domain_secrets` | `array` | no | `-` | - |
| `provider.openai.hosted_shell.network_policy.domain_secrets[].domain` | `string` | yes | `-` | - |
| `provider.openai.hosted_shell.network_policy.domain_secrets[].name` | `string` | yes | `-` | - |
| `provider.openai.hosted_shell.network_policy.domain_secrets[].value` | `string` | yes | `-` | - |
| `provider.openai.hosted_shell.network_policy.type` | `string` | no | `"disabled"` | Hosted shell network access policy. |
| `provider.openai.hosted_shell.skills` | `array` | no | `-` | Hosted skills to mount when using `container_auto`. |
| `provider.openai.hosted_shell.skills[]` | `object` | no | `-` | Hosted skill reference mounted into an OpenAI hosted shell environment. |
| `provider.openai.hosted_shell.skills[].bundle_b64` | `string` | yes | `-` | - |
| `provider.openai.hosted_shell.skills[].sha256` | `null \| string` | no | `-` | - |
| `provider.openai.hosted_shell.skills[].skill_id` | `string` | yes | `-` | - |
| `provider.openai.hosted_shell.skills[].type` | `string` | yes | `-` | - |
| `provider.openai.hosted_shell.skills[].version` | `OpenAIHostedSkillVersionKeyword \| integer \| string` | no | `"latest"` | Hosted skill version selector for OpenAI Responses hosted shell mounts. |
| `provider.openai.manual_compaction.instructions` | `null \| string` | no | `-` | Optional custom instructions appended to manual `/compact` requests. |
| `provider.openai.responses_include` | `array` | no | `-` | Optional Responses API `include` selectors. Example: `["reasoning.encrypted_content"]` for encrypted reasoning continuity. |
| `provider.openai.responses_include[]` | `string` | no | `-` | - |
| `provider.openai.responses_store` | `boolean \| null` | no | `-` | Optional Responses API `store` flag. Set to `false` to avoid server-side storage when using Responses-compatible models. |
| `provider.openai.service_tier` | `OpenAIServiceTier \| null` | no | `-` | Optional native OpenAI `service_tier` request parameter. Leave unset to inherit the Project-level default service tier. Options: "flex", "priority" |
| `provider.openai.tool_search.always_available_tools` | `array` | no | `[]` | Tool names that should never be deferred (always available). |
| `provider.openai.tool_search.always_available_tools[]` | `string` | no | `-` | - |
| `provider.openai.tool_search.defer_by_default` | `boolean` | no | `true` | Automatically defer loading of all tools except the core always-on set. |
| `provider.openai.tool_search.enabled` | `boolean` | no | `true` | Enable hosted tool search for OpenAI Responses-compatible models. |
| `provider.openai.websocket_mode` | `boolean` | no | `false` | Enable Responses API WebSocket transport for non-streaming requests on native OpenAI and OpenAI-compatible Responses endpoints. This is an opt-in path designed for long-running, tool-heavy workflows. |
| `provider_overrides` | `object` | no | `{}` | Built-in provider overrides for model lists and endpoint configuration. Maps provider key (e.g., "opencode-zen", "opencode-go") to override config that extends the provider's hardcoded model list with custom models, and optionally overrides the base URL or API key env var. |
| `provider_overrides.*.api_key_env` | `null \| string` | no | `-` | Optional environment variable name for the API key. When set, overrides the provider's default API key environment variable for models from this override. Values from repository-controlled workspace/project layers are rejected. |
| `provider_overrides.*.base_url` | `null \| string` | no | `-` | Optional base URL override for the provider endpoint. When set, custom models from this override are routed to the specified endpoint instead of the provider's default. Values from repository-controlled workspace/project layers are rejected. |
| `provider_overrides.*.models` | `array` | no | `[]` | Additional model identifiers to offer for this built-in provider. These models are appended to the provider's hardcoded model list and appear in the `/model` picker alongside built-in entries. |
| `provider_overrides.*.models[]` | `string` | no | `-` | - |
| `providers_whitelist` | `array` | no | `[]` | Restrict which providers may be used. When non-empty, only providers listed here are visible in the model picker, selectable at first-run, and instantiable at runtime. Empty (the default) means all built-in and custom providers are available. |
| `providers_whitelist[]` | `string` | no | `-` | - |
| `pty.command_timeout_seconds` | `integer` | no | `300` | Command timeout in seconds (prevents hanging commands) |
| `pty.default_cols` | `integer` | no | `80` | Default terminal columns for PTY sessions |
| `pty.default_rows` | `integer` | no | `24` | Default terminal rows for PTY sessions |
| `pty.enabled` | `boolean` | no | `true` | Enable PTY support for interactive commands |
| `pty.large_output_threshold_kb` | `integer` | no | `5000` | Threshold (KB) at which to auto-spool large outputs to disk instead of memory |
| `pty.max_scrollback_bytes` | `integer` | no | `25000000` | Maximum bytes of output to retain per PTY session (prevents memory explosion) |
| `pty.max_sessions` | `integer` | no | `10` | Maximum number of concurrent PTY sessions |
| `pty.preferred_shell` | `null \| string` | no | `null` | Preferred shell program for PTY sessions (e.g. "zsh", "bash"); falls back to $SHELL |
| `pty.scrollback_lines` | `integer` | no | `400` | Total scrollback buffer size (lines) retained per PTY session |
| `pty.shell_zsh_fork` | `boolean` | no | `false` | Feature-gated shell runtime path that routes shell execution through zsh EXEC_WRAPPER hooks. |
| `pty.stdout_tail_lines` | `integer` | no | `20` | Number of recent PTY output lines to display in the chat transcript |
| `pty.zsh_path` | `null \| string` | no | `null` | Optional absolute path to patched zsh used when shell_zsh_fork is enabled. |
| `sandbox.default_policy` | `string` | no | `"read_only"` | Default sandbox policy |
| `sandbox.enabled` | `boolean` | no | `false` | Enable sandboxing for command execution |
| `sandbox.external.docker.cpu_limit` | `string` | no | `""` | CPU limit for container |
| `sandbox.external.docker.image` | `string` | no | `"ubuntu:22.04"` | Docker image to use |
| `sandbox.external.docker.memory_limit` | `string` | no | `""` | Memory limit for container |
| `sandbox.external.microvm.kernel_path` | `string` | no | `""` | Kernel image path |
| `sandbox.external.microvm.memory_mb` | `integer` | no | `512` | Memory size in MB |
| `sandbox.external.microvm.rootfs_path` | `string` | no | `""` | Root filesystem path |
| `sandbox.external.microvm.vcpus` | `integer` | no | `1` | Number of vCPUs |
| `sandbox.external.microvm.vmm` | `string` | no | `""` | VMM to use (firecracker, cloud-hypervisor) |
| `sandbox.external.sandbox_type` | `string` | no | `"none"` | Type of external sandbox |
| `sandbox.network.allowlist` | `array` | no | `[]` | Domain allowlist for network egress. Following field guide: "Default-deny outbound network, then allowlist." |
| `sandbox.network.allowlist[].domain` | `string` | yes | `-` | Domain pattern (e.g., "api.github.com", "*.npmjs.org") |
| `sandbox.network.allowlist[].port` | `integer` | no | `443` | Port (defaults to 443) |
| `sandbox.network.policy` | `string` | yes | `-` | Network egress policy. Defaults to [`NetworkPolicy::AllowlistOnly`] (default-deny, then allowlist). |
| `sandbox.resource_limits.cpu_time_secs` | `integer` | no | `0` | Custom CPU time limit in seconds (0 = use preset) |
| `sandbox.resource_limits.max_disk_mb` | `integer` | no | `0` | Custom disk write limit in MB (0 = use preset) |
| `sandbox.resource_limits.max_memory_mb` | `integer` | no | `0` | Custom memory limit in MB (0 = use preset) |
| `sandbox.resource_limits.max_pids` | `integer` | no | `0` | Custom max processes (0 = use preset) |
| `sandbox.resource_limits.preset` | `string` | no | `"moderate"` | Preset resource limits profile |
| `sandbox.resource_limits.timeout_secs` | `integer` | no | `0` | Custom wall clock timeout in seconds (0 = use preset) |
| `sandbox.seccomp.additional_blocked` | `array` | no | `[]` | Additional syscalls to block |
| `sandbox.seccomp.additional_blocked[]` | `string` | no | `-` | - |
| `sandbox.seccomp.enabled` | `boolean` | no | `true` | Enable seccomp filtering (Linux only) |
| `sandbox.seccomp.log_only` | `boolean` | no | `false` | Log blocked syscalls instead of killing process (for debugging) |
| `sandbox.seccomp.profile` | `string` | no | `"strict"` | Seccomp profile preset |
| `sandbox.sensitive_paths.additional` | `array` | no | `[]` | Additional paths to block |
| `sandbox.sensitive_paths.additional[]` | `string` | no | `-` | - |
| `sandbox.sensitive_paths.exceptions` | `array` | no | `[]` | Paths to explicitly allow (overrides defaults) |
| `sandbox.sensitive_paths.exceptions[]` | `string` | no | `-` | - |
| `sandbox.sensitive_paths.use_defaults` | `boolean` | no | `true` | Use default sensitive paths (SSH, AWS, etc.) |
| `security.auto_apply_detected_patches` | `boolean` | no | `false` | Automatically apply detected patch blocks in assistant replies when no write tool was executed. Defaults to false for safety. |
| `security.encrypt_payloads` | `boolean` | no | `false` | Encrypt payloads passed across executors. |
| `security.gatekeeper.auto_clear_paths` | `array` | no | `[".vtcode/bin", "/home/f2/.local/bin", "/home/f2/.vtcode/bin"]` | Paths eligible for quarantine auto-clear |
| `security.gatekeeper.auto_clear_paths[]` | `string` | no | `-` | - |
| `security.gatekeeper.auto_clear_quarantine` | `boolean` | no | `false` | Attempt to clear quarantine automatically (opt-in) |
| `security.gatekeeper.warn_on_quarantine` | `boolean` | no | `true` | Warn when a quarantined executable is detected |
| `security.hitl_notification_bell` | `boolean` | no | `true` | Play terminal bell notification when HITL approval is required. |
| `security.human_in_the_loop` | `boolean` | no | `true` | Require human confirmation for critical actions |
| `security.integrity_checks` | `boolean` | no | `true` | Enable runtime integrity tagging for critical paths. |
| `security.require_write_tool_for_claims` | `boolean` | no | `true` | Require a successful write tool before accepting claims like "I've updated the file" as applied. When true, such claims are treated as proposals unless a write tool executed successfully. |
| `security.zero_trust_mode` | `boolean` | no | `false` | Enable zero-trust checks between components. |
| `skills.bundled.enabled` | `boolean` | no | `true` | Enable bundled skills shipped with VT Code. |
| `skills.enable-auto-trigger` | `boolean` | no | `true` | Enable auto-trigger on $skill-name mentions |
| `skills.enable-description-matching` | `boolean` | no | `true` | Enable description-based keyword matching for auto-trigger |
| `skills.max-skills-in-prompt` | `integer` | no | `10` | Maximum number of skills to show in system prompt |
| `skills.min-keyword-matches` | `integer` | no | `2` | Minimum keyword matches required for description-based trigger |
| `skills.prompt-format` | `string` | no | `"xml"` | Prompt format for skills section (Agent Skills spec) - "xml": XML wrapping for safety (Claude models default) - "markdown": Plain markdown sections |
| `skills.render-mode` | `string` | no | `"lean"` | Rendering mode for skills in system prompt - "lean": Codex-style minimal (name + description + path only, 40-60% token savings) - "full": Full metadata with version, author, native flags |
| `subagents.auto_delegate_read_only` | `boolean` | no | `true` | - |
| `subagents.background.auto_restore` | `boolean` | no | `false` | - |
| `subagents.background.default_agent` | `null \| string` | no | `null` | - |
| `subagents.background.enabled` | `boolean` | no | `false` | - |
| `subagents.background.refresh_interval_ms` | `integer` | no | `2000` | - |
| `subagents.background.toggle_shortcut` | `string` | no | `"ctrl+b"` | - |
| `subagents.default_timeout_seconds` | `integer` | no | `300` | - |
| `subagents.enabled` | `boolean` | no | `true` | - |
| `subagents.max_concurrent` | `integer` | no | `3` | - |
| `subagents.max_depth` | `integer` | no | `1` | Maximum delegation depth. `1` disables nested delegation (children cannot spawn); `2` allows one level of nesting (root → child → grandchild), `3` two levels, etc. |
| `syntax_highlighting.cache_themes` | `boolean` | no | `true` | Enable theme caching for better performance |
| `syntax_highlighting.enabled` | `boolean` | no | `true` | Enable syntax highlighting for tool output |
| `syntax_highlighting.enabled_languages` | `array` | no | `[]` | Languages to enable syntax highlighting for |
| `syntax_highlighting.enabled_languages[]` | `string` | no | `-` | - |
| `syntax_highlighting.highlight_timeout_ms` | `integer` | no | `5000` | Performance settings - highlight timeout in milliseconds |
| `syntax_highlighting.max_file_size_mb` | `integer` | no | `10` | Maximum file size for syntax highlighting (in MB) |
| `syntax_highlighting.theme` | `string` | no | `"base16-ocean.dark"` | Theme to use for syntax highlighting |
| `telemetry.atif_enabled` | `boolean` | no | `false` | Enable ATIF (Agent Trajectory Interchange Format) trajectory export. When enabled, sessions write an `atif-trajectory.json` alongside the existing `.jsonl` trajectory log. |
| `telemetry.bottleneck_tracing` | `boolean` | no | `false` | Emit bottleneck traces for slow paths |
| `telemetry.dashboards_enabled` | `boolean` | no | `true` | Enable real-time dashboards |
| `telemetry.perf_events` | `boolean` | no | `true` | Emit performance events for file I/O, spawns, and UI latency |
| `telemetry.retention_days` | `integer` | no | `14` | Retention window for historical benchmarking (days) |
| `telemetry.sample_interval_ms` | `integer` | no | `1000` | KPI sampling interval in milliseconds |
| `telemetry.trajectory_enabled` | `boolean` | no | `true` | - |
| `telemetry.trajectory_max_age_days` | `integer` | no | `14` | Maximum age in days for rotated trajectory log files |
| `telemetry.trajectory_max_files` | `integer` | no | `50` | Maximum number of rotated trajectory log files to keep per workspace |
| `telemetry.trajectory_max_size_mb` | `integer` | no | `50` | Maximum total size in MB for all trajectory log files in a workspace |
| `timeouts.adaptive_decay_ratio` | `number` | no | `0.875` | Adaptive timeout decay ratio (0.1-1.0). Lower relaxes faster back to ceiling. |
| `timeouts.adaptive_min_floor_ms` | `integer` | no | `1000` | Minimum timeout floor in milliseconds when applying adaptive clamps. |
| `timeouts.adaptive_success_streak` | `integer` | no | `5` | Number of consecutive successes before relaxing adaptive ceiling. |
| `timeouts.default_ceiling_seconds` | `integer` | no | `180` | Maximum duration (in seconds) for standard, non-PTY tools. |
| `timeouts.long_running_command_ceiling_seconds` | `integer` | no | `3600` | Maximum duration (in seconds) for an explicit long-running command wait. |
| `timeouts.mcp_ceiling_seconds` | `integer` | no | `120` | Maximum duration (in seconds) for MCP calls. |
| `timeouts.pty_ceiling_seconds` | `integer` | no | `300` | Maximum duration (in seconds) for PTY-backed commands. |
| `timeouts.streaming_ceiling_seconds` | `integer` | no | `600` | Maximum duration (in seconds) for streaming API responses. |
| `timeouts.warning_threshold_percent` | `integer` | no | `80` | Percentage (0-100) of the ceiling after which the UI should warn. |
| `tools.client_tool_search` | `boolean` | no | `true` | Enables client-local deferred tool loading for providers without a hosted tool search (e.g. Gemini). When enabled, tools flagged `defer_loading: true` are omitted from the request payload instead of being sent eagerly, and a compact summary of what is discoverable is appended to the system prompt; the model loads them via the local MCP discovery tools. Enabled by default because eager MCP schemas are the dominant source of token inflation. |
| `tools.default_policy` | `string` | no | `"prompt"` | Default policy for tools not explicitly listed |
| `tools.editor.enabled` | `boolean` | no | `true` | Enable external editor support for `/edit` and keyboard shortcuts |
| `tools.editor.preferred_editor` | `string` | no | `""` | Preferred editor command override (supports arguments, e.g. "code --wait") |
| `tools.editor.suspend_tui` | `boolean` | no | `true` | Suspend the TUI event loop while editor is running |
| `tools.loop_thresholds` | `object` | no | `{}` | Tool-specific loop thresholds (Adaptive Loop Detection) Allows setting higher loop limits for read-only tools (e.g., ls, grep) and lower limits for mutating tools. |
| `tools.loop_thresholds.*` | `integer` | no | `-` | - |
| `tools.max_consecutive_blocked_tool_calls_per_turn` | `integer` | no | `8` | Maximum consecutive blocked tool calls allowed per turn before forcing a turn break. The total fuse is 2x this value in normal mode, 4x in Plan Mode, and this value in recovery mode, unless overridden by `max_total_blocked_tool_calls_per_turn`. |
| `tools.max_total_blocked_tool_calls_per_turn` | `integer \| null` | no | `null` | Optional explicit cap for total blocked tool calls per turn. When unset, the runtime derives it from the consecutive cap (2x normal, 4x plan, 1x recovery). |
| `tools.blocked_tool_thresholds` | `object` | no | `{}` | Per-tool consecutive blocked-call cap overrides keyed by tool name. Allows read-only tools to tolerate more denies than mutating tools. |
| `tools.blocked_tool_thresholds.*` | `integer` | no | `-` | - |
| `tools.max_repeated_tool_calls` | `integer` | no | `2` | Maximum number of times the same tool invocation can be retried with the identical arguments within a single turn. |
| `tools.max_sequential_spool_chunk_reads` | `integer` | no | `6` | Maximum sequential spool-chunk `read_file` calls allowed per turn before nudging the agent to switch to targeted extraction/summarization. |
| `tools.max_tool_loops` | `integer` | no | `40` | Maximum inner tool-call loops per user turn. Set to `0` to disable the limit. Prevents infinite tool-calling cycles in interactive chat. This limits how many back-and-forths the agent will perform executing tools and re-asking the model before returning a final answer. |
| `tools.max_tool_rate_per_second` | `integer \| null` | no | `null` | Optional per-second rate limit for tool calls to smooth bursty retries. When unset, the runtime defaults apply. |
| `tools.plugins.allow` | `array` | no | `[]` | Explicit allow-list of plugin identifiers permitted to load. |
| `tools.plugins.allow[]` | `string` | no | `-` | - |
| `tools.plugins.auto_reload` | `boolean` | no | `false` | Enable hot-reload polling for manifests to support rapid iteration. |
| `tools.plugins.default_trust` | `string` | no | `"sandbox"` | Default trust level when a manifest omits trust metadata. |
| `tools.plugins.deny` | `array` | no | `[]` | Explicit block-list of plugin identifiers that must be rejected. |
| `tools.plugins.deny[]` | `string` | no | `-` | - |
| `tools.plugins.enabled` | `boolean` | no | `true` | Toggle the plugin runtime. When disabled, manifests are ignored. |
| `tools.plugins.manifests` | `array` | no | `[]` | Manifest paths (files or directories) that should be scanned for plugins. |
| `tools.plugins.manifests[]` | `string` | no | `-` | - |
| `tools.policies` | `object` | no | `{}` | Specific tool policies |
| `tools.policies.*` | `string` | no | `-` | Tool execution policy |
| `tools.profile` | `string` | no | `"vt_code"` | Model-facing tool profile. |
| `tools.web_fetch.allowed_domains` | `array` | no | `[]` | Inline whitelist - Domains to allow in restricted mode |
| `tools.web_fetch.allowed_domains[]` | `string` | no | `-` | - |
| `tools.web_fetch.blocked_domains` | `array` | no | `[]` | Inline blocklist - Additional domains to block |
| `tools.web_fetch.blocked_domains[]` | `string` | no | `-` | - |
| `tools.web_fetch.blocked_patterns` | `array` | no | `[]` | Additional blocked patterns |
| `tools.web_fetch.blocked_patterns[]` | `string` | no | `-` | - |
| `tools.web_fetch.mode` | `string` | no | `"restricted"` | Security mode: restricted (blocklist) or whitelist (allowlist) |
| `tools.web_fetch.strict_https_only` | `boolean` | no | `true` | Strict HTTPS-only mode |
| `tools.web_search.cache_ttl_secs` | `integer` | no | `300` | How long successful search results are cached before a fresh request is made, in seconds. Defaults to 300s (5 min). |
| `tools.web_search.cooldown_ms` | `integer` | no | `3000` | Minimum gap between consecutive live requests, in milliseconds. Defaults to 3000ms (3s). |
| `tools.web_search.max_results` | `integer` | no | `8` | Default cap on the number of results returned per call. Hard-capped at 20 by the runtime to keep responses inline-friendly. |
| `tools.web_search.provider` | `string` | no | `"auto"` | Provider selection. Currently the only supported backend is DuckDuckGo; this field is kept for future extension. |
| `tools.web_search.session_max_requests` | `integer` | no | `12` | Hard cap on outbound network requests per tool instance. Defaults to 12 to stay well below DDG's soft session quotas. |
| `tools.web_search.timeout_secs` | `integer` | no | `20` | Per-request timeout in seconds. Capped at 60s by the runtime. |
| `tui.alternate_screen` | `TuiAlternateScreen \| null` | no | `null` | - |
| `tui.animations` | `boolean \| null` | no | `null` | - |
| `tui.notification_condition` | `NotificationCondition \| null` | no | `null` | When to deliver desktop notifications relative to terminal focus. Defaults to `unfocused` (only deliver when terminal is not focused). Set to `always` to deliver notifications even when the terminal is focused. |
| `tui.notification_method` | `TerminalNotificationMethod \| null` | no | `null` | - |
| `tui.notifications` | `TuiNotificationsConfig \| null` | no | `null` | - |
| `tui.show_tooltips` | `boolean \| null` | no | `null` | - |
| `ui.allow_tool_ansi` | `boolean` | no | `false` | Allow ANSI escape sequences in tool output (enables colors but may cause layout issues) |
| `ui.bold_is_bright` | `boolean` | no | `false` | Compatibility mode for legacy terminals that map bold to bright colors. When enabled, avoids using bold styling on text that would become bright colors, preventing visibility issues in terminals with "bold is bright" behavior. |
| `ui.color_scheme_mode` | `string` | no | `"auto"` | Color scheme mode for automatic light/dark theme switching. - "auto": Detect from terminal (via the Contour dark/light query where supported, else OSC 11, or the COLORFGBG env var) and follow live palette changes - "light": Force light mode theme selection - "dark": Force dark mode theme selection |
| `ui.dim_completed_todos` | `boolean` | no | `true` | Dim completed todo items (- \[x\]) in agent output |
| `ui.display_mode` | `string` | no | `"minimal"` | UI display mode preset (full, minimal, focused) |
| `ui.fullscreen.copy_on_select` | `boolean` | no | `true` | Copy selected transcript text immediately when the mouse selection ends. Can also be controlled via VTCODE_FULLSCREEN_COPY_ON_SELECT=0/1. |
| `ui.fullscreen.mouse_capture` | `boolean` | no | `true` | Capture mouse events inside the fullscreen UI. Can also be controlled via VTCODE_FULLSCREEN_MOUSE_CAPTURE=0/1. |
| `ui.fullscreen.scroll_speed` | `integer` | no | `3` | Multiplier applied to mouse wheel transcript scrolling in fullscreen mode. Values are clamped to the range 1..=20. Can also be controlled via VTCODE_FULLSCREEN_SCROLL_SPEED. |
| `ui.hide_header` | `boolean` | no | `true` | Hide the full TUI header, showing only version info in a compact line. |
| `ui.inline_viewport_rows` | `integer` | no | `16` | Number of rows to allocate for inline UI viewport |
| `ui.keybindings` | `object` | no | `{}` | Override session action bindings; an empty array explicitly unbinds an action. |
| `ui.keybindings.*` | `array` | no | `[]` | Key specifications for a named session action, such as `open_transcript_review` or `toggle_transcript_render_mode`. |
| `ui.keyboard_protocol.disambiguate_escape_codes` | `boolean` | no | `true` | Resolve Esc key ambiguity (recommended for performance) |
| `ui.keyboard_protocol.enabled` | `boolean` | no | `true` | Enable keyboard protocol enhancements (master toggle) |
| `ui.keyboard_protocol.mode` | `string` | no | `"default"` | Preset mode: default, full, minimal, or custom |
| `ui.keyboard_protocol.report_all_keys` | `boolean` | no | `false` | Report all keys, including modifier-only keys (Shift, Ctrl) |
| `ui.keyboard_protocol.report_alternate_keys` | `boolean` | no | `true` | Report alternate key layouts (e.g. for non-US keyboards) |
| `ui.keyboard_protocol.report_event_types` | `boolean` | no | `true` | Report press, release, and repeat events |
| `ui.layout_mode` | `string` | no | `"auto"` | Override the responsive layout mode |
| `ui.message_block_spacing` | `boolean` | no | `true` | Add spacing between message blocks |
| `ui.minimum_contrast` | `number` | no | `4.5` | Minimum contrast ratio for text against background (WCAG 2.1 standard) - 4.5: WCAG AA (default, suitable for most users) - 7.0: WCAG AAA (enhanced, for low-vision users) - 3.0: Large text minimum - 1.0: Disable contrast enforcement |
| `ui.notifications.backend` | `string` | no | `"auto"` | Preferred backend for desktop notification delivery. |
| `ui.notifications.command_failure` | `boolean \| null` | no | `null` | Notify when a shell/command execution fails. If omitted, falls back to `tool_failure` for backward compatibility. |
| `ui.notifications.completion` | `boolean` | no | `true` | Legacy master toggle for completion notifications. New installs should prefer `completion_success` and `completion_failure`. |
| `ui.notifications.completion_failure` | `boolean \| null` | no | `null` | Notify when a turn/session is partial, failed, or cancelled. If omitted, falls back to `completion`. |
| `ui.notifications.completion_success` | `boolean \| null` | no | `null` | Notify when a turn/session completes successfully. If omitted, falls back to `completion`. |
| `ui.notifications.delivery_mode` | `string` | no | `"desktop"` | Notification transport strategy. |
| `ui.notifications.enabled` | `boolean` | no | `true` | Master toggle for all runtime notifications. |
| `ui.notifications.error` | `boolean` | no | `true` | Notify on runtime/system errors. |
| `ui.notifications.hitl` | `boolean` | no | `true` | Notify when human input/approval is required. |
| `ui.notifications.max_identical_in_window` | `integer` | no | `1` | Maximum identical notifications allowed within the suppression window. |
| `ui.notifications.policy_approval` | `boolean \| null` | no | `null` | Notify when policy approval is required. If omitted, falls back to `hitl` for backward compatibility. |
| `ui.notifications.repeat_window_seconds` | `integer` | no | `30` | Suppression window for repeated identical notifications. |
| `ui.notifications.request` | `boolean \| null` | no | `null` | Notify on generic request events. If omitted, falls back to `hitl` for backward compatibility. |
| `ui.notifications.suppress_when_focused` | `boolean` | no | `true` | Suppress notifications while terminal focus is active. |
| `ui.notifications.tool_failure` | `boolean` | no | `false` | Notify when a tool call fails. |
| `ui.notifications.tool_success` | `boolean` | no | `false` | Notify on successful tool calls. |
| `ui.reasoning_display_mode` | `string` | no | `"toggle"` | Reasoning display mode for chat UI ("always", "toggle", or "hidden") |
| `ui.reasoning_visible_default` | `boolean` | no | `true` | Default visibility for reasoning when display mode is "toggle" |
| `ui.reduce_motion_keep_progress_animation` | `boolean` | no | `false` | Keep animated progress indicators while reduce_motion_mode is enabled. |
| `ui.reduce_motion_mode` | `boolean` | no | `false` | Reduce motion mode: minimizes shimmer/flashing animations. Can also be enabled via VTCODE_REDUCE_MOTION=1 environment variable. |
| `ui.safe_colors_only` | `boolean` | no | `false` | Restrict color palette to the 11 "safe" ANSI colors portable across common themes. Safe colors: red, green, yellow, blue, magenta, cyan + brred, brgreen, brmagenta, brcyan Problematic colors avoided: brblack (invisible in Solarized Dark), bryellow (light themes), white/brwhite (light themes), brblue (Basic Dark). See: <https://blog.xoria.org/terminal-colors/> |
| `ui.screen_reader_mode` | `boolean` | no | `false` | Screen reader mode: disables animations, uses plain text indicators, and optimizes output for assistive technology compatibility. Can also be enabled via VTCODE_SCREEN_READER=1 environment variable. |
| `ui.show_diagnostics_in_transcript` | `boolean` | no | `false` | Show warning/error/fatal diagnostic lines in the TUI transcript and log panel. Also controls whether ERROR-level tracing logs appear in the TUI session log. Errors are always captured in the session archive JSON regardless of this setting. |
| `ui.show_sidebar` | `boolean` | no | `true` | Show the right sidebar (queue, context, tools) |
| `ui.show_task_panel` | `boolean` | no | `false` | Automatically show the plan-mode TODO task tracking panel when a plan is approved or the task tracker is updated. When disabled, the panel can still be toggled with the `toggle_task_panel` keybinding. |
| `ui.show_turn_timer` | `boolean` | no | `false` | Show per-turn elapsed timer line after completed turns |
| `ui.status_line.command` | `null \| string` | no | `null` | - |
| `ui.status_line.command_timeout_ms` | `integer` | no | `200` | - |
| `ui.status_line.mode` | `string` | no | `"auto"` | - |
| `ui.status_line.refresh_interval_ms` | `integer` | no | `1000` | - |
| `ui.status_line.show_clock` | `boolean` | no | `true` | Show the current system time (`HH:MM:SS`) on the status line (default: true). |
| `ui.terminal_title.items` | `array \| null` | no | `null` | - |
| `ui.terminal_title.items[]` | `string` | no | `-` | - |
| `ui.thinking_display` | `string` | no | `"collapsed"` | Default collapse state of agent thinking/reasoning blocks ("collapsed" or "extended") |
| `ui.transcript_review.show_close_button` | `boolean` | no | `true` | Show the mouse-clickable close control in the Transcript Review title. |
| `ui.transcript_review.show_hints` | `boolean` | no | `true` | Show the keyboard and click affordance on compact command rows. |
| `ui.transcript_review.show_shortcut_guide` | `boolean` | no | `true` | Show the keyboard shortcut guide inside Transcript Review. |
| `ui.tool_display_mode` | `string` | no | `"compact"` | Tool transition summary display mode. Options: "compact" (one compact summary per call, with bounded live command output) or "expanded" (the expanded summary layout per call). |
| `ui.tool_output_max_lines` | `integer` | no | `30` | Maximum number of lines to display in tool output (prevents transcript flooding) |
| `ui.tool_output_mode` | `string` | no | `"compact"` | Tool output display mode ("compact" or "full") |
| `ui.tool_output_spool_bytes` | `integer` | no | `80000` | Maximum bytes of output to display before auto-spooling to disk |
| `ui.tool_output_spool_dir` | `null \| string` | no | `null` | Optional custom directory for spooled tool output logs |
| `ui.vim_mode` | `boolean` | no | `false` | Enable Vim-style prompt editing in the interactive terminal UI. |
| `webmcp.allowed_origins` | `array` | no | `[]` | Exact browser origins allowed to pair. |
| `webmcp.allowed_origins[]` | `string` | no | `-` | - |
| `webmcp.allowed_roots` | `array` | no | `[]` | Explicit headless roots; the current bridge serves one root per process. |
| `webmcp.allowed_roots[]` | `string` | no | `-` | - |
| `webmcp.enabled` | `boolean` | no | `false` | Opt-in marker for WebMCP integrations; listener startup still requires an explicit CLI or TUI command. |
| `webmcp.host` | `string` | no | `"127.0.0.1"` | Literal loopback bind host; remote clients require a TLS-terminating reverse proxy. |
| `webmcp.max_frame_bytes` | `integer` | no | `1048576` | Maximum JSON WebSocket frame size. |
| `webmcp.max_in_flight_requests` | `integer` | no | `8` | Maximum concurrent bridge operations. |
| `webmcp.pairing_ttl_secs` | `integer` | no | `300` | One-time pairing lifetime and authenticated-session inactivity lease. |
| `webmcp.port` | `integer` | no | `0` | Bind port. Zero asks the OS for an available port. |
| `webmcp.remote_mcp.allowed_origins` | `array` | no | `[]` | Separate exact MCP Origin allowlist; missing Origin is accepted. |
| `webmcp.remote_mcp.allowed_origins[]` | `string` | no | `-` | - |
| `webmcp.remote_mcp.authorization_server` | `string \| null` | no | `null` | External HTTPS authorization-server URL advertised in protected-resource metadata. |
| `webmcp.remote_mcp.citation_url_prefix` | `string \| null` | no | `null` | Optional HTTP(S) prefix for escaped citation URLs; no file-serving route is added. |
| `webmcp.remote_mcp.enabled` | `boolean` | no | `false` | Enable the read-only `search` and `fetch` MCP endpoints for `webmcp serve`. |
| `webmcp.remote_mcp.max_results` | `integer` | no | `20` | Maximum results returned by `search`. |
| `webmcp.remote_mcp.max_scan_bytes` | `integer` | no | `16777216` | Maximum UTF-8 content bytes scanned by `search`. |
| `webmcp.remote_mcp.max_scan_files` | `integer` | no | `256` | Maximum visible files scanned by `search`. |
| `webmcp.remote_mcp.proxy_token_env` | `string` | no | `"VTCODE_WEBMCP_MCP_PROXY_TOKEN"` | Environment variable containing the internal bearer token injected by the external proxy. |
| `webmcp.remote_mcp.public_url` | `string \| null` | no | `null` | Canonical external HTTPS `/sse/` URL. |
| `webmcp.remote_mcp.session_ttl_secs` | `integer` | no | `300` | Inactivity lifetime for legacy HTTP+SSE sessions. |
| `workspace.include_context` | `boolean` | no | `true` | Include workspace context in messages. |
| `workspace.max_context_size` | `integer \| null` | no | `null` | Maximum size of workspace context to include (in bytes). |
| `workspace.use_root_config` | `boolean` | no | `false` | When true, force the workspace root `vtcode.toml` as the sole active config layer, discarding system, user, project, and dot-dir layers. |
