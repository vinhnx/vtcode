# VT Code Configuration

VT Code configuration gives you fine-grained control over the model, execution environment, and integrations available to the CLI. Use this guide alongside the workflows in the extension, the participant system, and the tool approval mechanisms available in the application.

VT Code uses a configuration file named `vtcode.toml` that can be placed at the root of your project workspace to customize behavior. Interactive sessions poll the active workspace, project, user, and explicit config layers and apply safe changes without restarting.

## Quick navigation

- [Feature flags](#feature-flags)
- [Settings palette and reset](#settings-palette-and-reset)
- [Live reload](#live-reload)
- [Model selection](#model-selection)
- [Instruction guidance and memory](#instruction-guidance-and-persistent-memory)
- [External editor](#external-editor)
- [Fullscreen interaction](#fullscreen-interaction)
- [Execution environment](#execution-environment)
- [Long-running command waits](#long-running-command-waits)
- [Context compaction and session history](#context-compaction-and-session-history)
- [MCP integration](#mcp-integration)
- [WebMCP browser bridge](#webmcp-browser-bridge)
- [Security and approvals](#security-and-approvals)
- [User data directories](../guides/user-data-directories.md)
- [Permissions guide](../guides/permissions.md)
- [Participant system](#participant-system)
- [Profiles and overrides](#profiles-and-overrides)
- [Reference table](#config-reference)
- [Generated field reference](./CONFIG_FIELD_REFERENCE.md)

VT Code supports several mechanisms for setting config values:

- The canonical platform config directory's `vtcode.toml` (for example, `$XDG_CONFIG_HOME/vtcode/vtcode.toml` on Linux/BSD). See the [user data directories guide](../guides/user-data-directories.md) for all category roots and overrides.
- The legacy `$VTCODE_HOME/vtcode.toml` file, defaulting to `~/.vtcode/vtcode.toml`, remains a read-compatibility and migration source.
- The workspace-level `vtcode.toml` file that can be placed at the root of your project (similar to `AGENTS.md` in the OpenAI Codex).
- Environment variables that can override certain configuration options.

Both the workspace `vtcode.toml` and the main `vtcode.toml` file support the following options:

## Settings palette and reset

In an interactive session, `/config` opens the settings palette. Settings are
grouped into sections with human-readable labels, descriptions, effective
values, and source/target information. Use `/config <path>` to open a section
directly; nested selections are restored when returning to a parent view.

The palette's **Reset configuration** action shows the exact target file and
requires confirmation. `/config reset` opens the same confirmation view. A
reset clears only that target layer by replacing its contents with an empty
TOML document. Lower-precedence layers and credentials are preserved.

Saving a setting updates only the selected field in the target layer; it does
not flatten the effective configuration into that file. Custom-provider and
provider endpoint/credential fields are saved to the canonical user config
unless an explicit `--config` file was selected, so trusted provider settings
cannot be copied into repository-controlled configuration.

The CLI exposes the same service for non-interactive repair and cleanup:

```bash
# Clear the active workspace layer (or the explicit --config file)
vtcode config reset

# Clear the canonical user or current project-profile layer
vtcode config reset --global
vtcode config reset --project

# Select an explicit workspace-layer file for this invocation
vtcode --config ./nightly.toml config reset
```

`--global` and `--project` are mutually exclusive. The CLI prints the resolved
file and does not remove secure credentials or any other layer.

## Live reload

While a session is running, changes to watched configuration files are
debounced and reloaded through the normal layered configuration service. Safe
runtime settings such as UI appearance and theme, status-line behavior,
permissions, sandbox and timeout policy, MCP approval policy, and custom
provider definitions are applied without restarting. A provider/model identity
selected for the current session remains stable until a later turn or session
when changing it would invalidate the active client.

If a watched edit is malformed, inaccessible, or fails validation, VT Code
keeps the last valid configuration and displays a warning. Fixing the file
causes the next debounced reload to apply it. File creation and deletion are
also observed, including user/project layers and an explicit `--config` file.

The settings palette is refreshed from a successful reload while retaining its
current section and selected entry; if an entry disappeared, selection falls
back to the first available item.

## Feature flags

Optional and experimental capabilities are toggled via the `[features]` table in `vtcode.toml`. These allow you to customize the behavior of various VT Code features.

```toml
[features]
streaming = true           # enable streaming responses
human_in_the_loop = true   # enable human-in-the-loop tool approval
participant_context = true # include participant context in messages
terminal_integration = true # enable terminal integration features
```

Supported features:

| Key                    | Default | Description                                |
| ---------------------- | :-----: | ------------------------------------------ |
| `streaming`            |  true   | Enable streaming responses in the UI       |
| `human_in_the_loop`    |  true   | Enable tool approval prompts               |
| `participant_context`  |  true   | Include participant context in messages    |
| `terminal_integration` |  true   | Enable terminal integration features       |
| `mcp_enabled`          |  false  | Enable Model Context Protocol integrations |

## Model selection

### agent.provider

The AI provider that VT Code should use.

```toml
[agent]
provider = "anthropic"  # available: openai, anthropic, google, meta, deepseek, copilot, openrouter, vercel, mimo, huggingface, zai, moonshot, minimax, mistral, qwen, stepfun, evolink, poolside, xai, nvidia, merge-gateway, ollama, lmstudio, llamacpp
default_model = "claude-sonnet-5"  # overrides the default model for the selected provider
```

### agent.provider_settings

This option lets you customize the settings for different AI providers.

For example, if you wanted to add custom API endpoints or settings for a provider:

```toml
[agent.provider_settings.openai]
name = "OpenAI"
base_url = "https://api.openai.com/v1"
env_key = "OPENAI_API_KEY"
# Extra query params that need to be added to the URL
query_params = {}

[agent.provider_settings.anthropic]
name = "Anthropic"
base_url = "https://api.anthropic.com/v1"
env_key = "ANTHROPIC_API_KEY"

[agent.provider_settings.google]
name = "Google Gemini"
base_url = "https://generativelanguage.googleapis.com/v1beta"
env_key = "GOOGLE_GEMINI_API_KEY"
# Note: Google's API uses a different format
query_params = { key = "$GOOGLE_GEMINI_API_KEY" }

[agent.provider_settings.ollama]
name = "Ollama"
base_url = "http://localhost:11434/v1"
# No API key required for local Ollama instance

[agent.provider_settings.nvidia]
name = "NVIDIA NIM"
base_url = "https://integrate.api.nvidia.com/v1"
env_key = "NVIDIA_API_KEY"

[agent.provider_settings.merge-gateway]
name = "Merge Gateway"
base_url = "https://api-gateway.merge.dev/v1"
env_key = "MERGE_GATEWAY_API_KEY"
# Native Responses is the default; an explicit /v1/openai base URL keeps Chat Completions compatibility.
# Override the endpoint with MERGE_GATEWAY_BASE_URL when using a proxy.
# The default model is default_routing; explicit provider/model routes are also valid.

[agent.provider_settings.meta]
name = "Meta AI"
base_url = "https://api.meta.ai/v1"
env_key = "META_API_KEY"

[agent.provider_settings.vercel]
name = "Vercel AI Gateway"
base_url = "https://ai-gateway.vercel.sh/v1"
env_key = "AI_GATEWAY_API_KEY"
# OpenAI Chat Completions compatible; override the endpoint with VERCEL_AI_GATEWAY_BASE_URL.
```

Note this makes it possible to use VT Code with non-default models, so long as they are properly configured with the correct API endpoints and authentication.

Or a third-party provider (using a distinct environment variable for the API key):

```toml
[agent.provider_settings.mistral]
name = "Mistral"
base_url = "https://api.mistral.ai/v1"
env_key = "MISTRAL_API_KEY"
```

It is also possible to configure a provider to include extra HTTP headers with a request. These can be hardcoded values (`http_headers`) or values read from environment variables (`env_http_headers`):

```toml
[agent.provider_settings.example]
# name, base_url, ...
# This will add the HTTP header `X-Example-Header` with value `example-value`
# to each request to the model provider.
http_headers = { "X-Example-Header" = "example-value" }
# This will add the HTTP header `X-Example-Features` with the value of the
# `EXAMPLE_FEATURES` environment variable to each request to the model provider
# _if_ the environment variable is set and its value is non-empty.
env_http_headers = { "X-Example-Features" = "EXAMPLE_FEATURES" }
```

### Codex app-server sidecar

Use these settings when you want VT Code to launch the official Codex app-server locally.

```toml
[agent]
provider = "codex"
default_model = "gpt-5-codex"

[agent.codex_app_server]
command = "codex"
args = ["app-server"]
startup_timeout_secs = 10
experimental_features = false
```

- `command = "codex"` means the local `codex` CLI must be installed and available on `$PATH`.
- If your Codex binary lives elsewhere, set `command` to that executable path instead.
- `experimental_features = false` keeps experimental Codex app-server discovery and native `review/start` routing disabled unless you explicitly opt in.
- If the sidecar command is missing, VT Code disables the Codex runtime path early and falls back to another authenticated provider when available.
- In the interactive UI you can open this section directly with `/config codex` or `/config agent.codex_app_server`.

You can also enable the experimental Codex behavior for a single run:

```bash
vtcode --codex-experimental
```

### custom_providers

Use `custom_providers` for named OpenAI-compatible endpoints that are not one of VT Code's built-in providers. Each entry has a stable `name`, a human-friendly `display_name`, a `base_url`, an optional `api_key_env`, a default `model`, and an optional `context_window` in tokens. When omitted, the provider uses the default context window. This describes the provider capability; the separate `context.max_context_tokens` setting can still impose a lower session budget. Secure credentials are scoped by `(name, api_key_env)`; they are not shared with another configured endpoint that uses the same API-key environment variable.

Custom provider definitions are trusted configuration. VT Code rejects a
non-empty `custom_providers` value when its winning value comes from a
repository-controlled workspace or project layer, including command-backed
`auth.command` entries. Put custom providers in the system or user config, or
select a config file explicitly with `--config path/to/file.toml`.

New provider-level fields

```toml
[[custom_providers]]
name = "mycorp"
display_name = "MyCorporateName"
base_url = "https://llm.corp.example/v1"
api_key_env = "MYCORP_API_KEY"
model = "gpt-5.6-sol"
# context_window = 256000   # Optional context window size in tokens (provider capability)
# api_format = "auto"      # Optional provider-level API format hint: auto|openai-chat|openai-responses|anthropic-messages
```

Notes:
- `context_window` declares the provider's capability in tokens and drives the context size shown in the UI, compaction thresholds, and preflight token checks.
- `api_format` is a hint to VT Code about how this provider / endpoint expects model traffic. Accepted values are: `auto`, `openai-chat`, `openai-responses`, and `anthropic-messages`. When omitted VT Code preserves legacy behavior and will try to autodetect; an explicit value is honored and VT Code will not silently fallback to a different format.

Capability defaults and per-model profiles

Custom providers may expose a small, conservative set of capability defaults to use when model metadata is absent. These are useful for gateways and aggregators that do not provide per-model descriptors. Set fields such as `supports_tools`, `supports_vision`, `supports_structured_output`, or `supports_parallel_tool_calls` directly on the provider entry.

Providers and profiles can also pin sampling values. Available fields: `temperature` (0.0-2.0), `top_p` (0.0-1.0), `top_k` (>= 0), `presence_penalty` / `frequency_penalty` (-2.0-2.0), `max_tokens` (> 0; overrides the agent loop's built-in per-task limits), and `reasoning_effort`. Pinning `reasoning_effort` on a profile implies effort support for that model.

For fine-grained overrides you can declare sparse per-model profiles. Profiles live in `custom_providers.profiles."<model-id>"` and only modify runtime defaults for that specific model identifier. IMPORTANT: profiles do not add or enable models in the picker — `model` / `models` remain the allowlist/default. A profile only changes how VT Code treats an already-selected model at runtime (capabilities, context window, api_format, sampling values, etc.).

Example per-model profile:

```toml
[custom_providers.profiles."corp-model"]
api_format = "openai-responses"
context_window = 131072
temperature = 0.2            # sampling temperature (0.0-2.0)
# top_p = 0.9                # nucleus sampling (0.0-1.0)
# top_k = 40                 # top-k cutoff (>= 0)
# presence_penalty = 0.1     # (-2.0-2.0)
# frequency_penalty = -0.5   # (-2.0-2.0)
# max_tokens = 8192          # overrides built-in per-task limits (800/2000)
reasoning_effort = "low"     # implies effort support for this model
supports_tools = true
supports_vision = false
supports_structured_output = true
supports_parallel_tool_calls = true
supports_context_caching = false
supports_responses_compaction = true
supports_context_edits = false
```

Precedence and semantics

When determining a model's runtime shape VT Code applies values in the following order (highest wins):

1. per-model profile (`custom_providers.profiles."<model-id>"`)
2. provider-level defaults (fields on the `[[custom_providers]]` entry)
3. model metadata discovered from the provider (or autodetection)
4. conservative built-in fallback defaults

Sampling values resolve on the same chain, with one extra global layer beneath the provider: profile → provider default → `agent.temperature` / `agent.reasoning_effort` globals → built-in per-task limits (`max_tokens` only). Two built-in behaviors sit above the profile chain: simple sub-tasks force `reasoning_effort = "minimal"` regardless of a profile pin, and backends that reject sampling during reasoning (native Anthropic/MiniMax, or custom profiles with `api_format = "anthropic-messages"`) drop `temperature` while reasoning is active.

Additional rules:
- An explicit boolean `false` in any overriding layer is honored and prevents a higher-level implicit `true` from taking effect.
- Omitting `api_format` preserves legacy autodetection behavior; explicitly setting `api_format` to a value instructs VT Code to use this API shape and not silently fall back.
- Profiles do not make a model available in the picker — use `model` or `models` to control availability.
- Wire delivery depends on the backend's API format. The OpenAI Chat shape sends `temperature`, `top_p`, and both penalties; the OpenAI Responses shape sends them inside a nested `sampling_parameters` object that some compatible endpoints ignore, and currently does not emit `max_output_tokens` for non-native endpoints; `top_k` is accepted in configuration but not serialized for these shapes today (it applies only to backends whose own request builders expose it).
- Name-based OpenAI sampling gates apply to custom endpoints too, by bare model-name match: models named `gpt`, `gpt-5.2`, `gpt-5.4`, `gpt-5.5*` accept sampling only while reasoning effort resolves to `none` (values are silently omitted otherwise), and `gpt-5`/`gpt-5-mini`/`gpt-5-nano` never receive sampling parameters. Prefer neutral model IDs on custom gateways if you need pinned values on such names.

Store a custom provider key with the same explicit identity used by the
configuration:

```bash
vtcode secret add mycorp --key-name MYCORP_API_KEY
vtcode secret status mycorp --key-name MYCORP_API_KEY
```

`api_key_env` defaults to a derived `NAME_API_KEY` value when omitted. Process
environment variables and workspace `.env` entries take precedence over secure
storage. Legacy provider-only entries are migrated only for a provider's
default key; use `--key-name` for every non-default profile.

These entries are editable from `/config`, and they show up in the model picker using `display_name` so you can toggle between multiple custom endpoints without losing track of the active one.

### providers (model list overrides)

Use `[providers.<name>]` to extend a built-in provider's model list with additional custom models. This is useful when you want to add models to an existing provider (e.g., OpenCode Zen, OpenCode Go) without creating an entirely new custom provider.

```toml
[providers.opencode-zen]
models = ["gpt-5.6-sol", "claude-sonnet-5", "glm-5.1"]
base_url = "https://custom-endpoint.example.com"   # optional
api_key_env = "MY_CUSTOM_KEY"                        # optional
```

Each `[providers.<name>]` section supports. Its `api_key_env` override also
becomes the credential identity used by model availability, the picker, and the
runtime:

For the same trust-boundary reason, endpoint and credential overrides are
accepted only from system/user configuration, an explicitly selected config
file, or explicit runtime flags. A repository-controlled workspace or project
file cannot set `base_url` or `api_key_env`; model-list-only overrides remain
available there.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `models` | `string[]` | Yes | List of model identifiers to add to the provider's model picker. |
| `base_url` | `string` | No | Override the provider's default API endpoint. |
| `api_key_env` | `string` | No | Override the provider's default API key environment variable. |

The provider key must match a built-in provider (e.g., `opencode-zen`, `opencode-go`, `openai`, `anthropic`). Custom models appear in the `/model` picker alongside the provider's built-in entries.

**Example: Adding fine-tuned models to OpenAI**

```toml
[providers.openai]
models = ["ft:gpt-5.4:my-org:custom-model:v1"]
```

**Example: Overriding OpenCode Zen endpoint**

```toml
[providers.opencode-zen]
models = ["gpt-5.6-sol", "claude-sonnet-5"]
base_url = "https://my-proxy.example.com/v1"
api_key_env = "MY_PROXY_KEY"
```

### Provider whitelisting

Use `providers_whitelist` to restrict which providers VT Code may access. This is a governance control for environments where only approved inference endpoints should be reachable — for example, a corporate gateway or an air-gapped setup.

```toml
# Allow only corporate gateways + Gemini
providers_whitelist = ["opencode-zen", "opencode-go", "gemini"]
```

When `providers_whitelist` is non-empty:

- The `/model` picker shows only the listed providers.
- The first-run wizard offers only the listed providers.
- The startup validator rejects `agent.provider` values not in the list.
- Saving a model selection that falls outside the list is blocked.

When `providers_whitelist` is empty (the default), all built-in providers and `[[custom_providers]]` entries are available — this is the backward-compatible default.

Whitelist entries may be:

- A built-in provider key (`openai`, `anthropic`, `gemini`, `opencode-zen`, `opencode-go`, `ollama`, `lmstudio`, `llamacpp`, `copilot`, `deepseek`, `openrouter`, `moonshot`, `zai`, `minimax`, `mimo`, `mistral`, `huggingface`, `qwen`, `stepfun`, `evolink`, `poolside`).
- A `name` from a `[[custom_providers]]` entry.

Matching is case-insensitive.

### Model-specific settings

You can also configure model-specific behavior:

```toml
[agent.model_settings]
context_window = 128000    # Context window size in tokens
max_output_tokens = 4096   # Maximum tokens for model output
temperature = 0.7          # Model temperature (0.0-2.0)
top_p = 0.9                # Top-P sampling parameter
```

## Instruction Guidance and Persistent Memory

VT Code separates authored guidance from learned persistent memory.

- Authored guidance automatically includes the canonical user config `AGENTS.md` (and legacy `~/.vtcode/AGENTS.md`), project `AGENTS.md`, project `.vtcode/rules/`, and any `agent.instruction_files` entries you configure. The full content of these files is inlined into the prompt up to `agent.instruction_max_bytes` (default 16384); files that exceed the budget are truncated with a notice in the prompt.
- Persistent memory is a per-repository memory store summarized into a compact startup section after authored guidance.

### Instruction discovery controls

Use these fields to control how VT Code discovers and expands guidance files:

```toml
[agent]
instruction_files = ["docs/runbooks/**/*.md"]
instruction_excludes = ["**/other-team/.vtcode/rules/**"]
instruction_import_max_depth = 5
```

- `instruction_files` adds explicit files or globs to the authored-guidance bundle.
- `instruction_excludes` removes matching `AGENTS.md` or `.vtcode/rules/` files from discovery.
- `instruction_import_max_depth` limits recursive `@path` imports inside guidance files.

Workspace rules live under project `.vtcode/rules/`. Rules without frontmatter are always loaded. Rules with YAML `paths` frontmatter are loaded only when the current instruction context matches those paths.

### Persistent memory controls

Persistent memory uses `memory_summary.md` as the source for a compact startup summary and stores the durable registry under the repository memory directory.

By default, that directory is the user state directory's
`projects/<project>/memory/` path. VT Code also migrates older per-repository
memory directories from the legacy `VTCODE_HOME` root into the canonical state
directory when it resolves repository memory.

```toml
[agent.persistent_memory]
enabled = false
auto_write = true
startup_line_limit = 200
startup_byte_limit = 25600

[agent.small_model]
use_for_memory = true
```

- `agent.persistent_memory.enabled` turns per-repository persistent memory on or off. It defaults to `false`.
- `agent.persistent_memory.auto_write` controls whether VT Code stages and consolidates rollout summaries at session finalization.
- `startup_line_limit` and `startup_byte_limit` cap the scan VT Code uses to build the compact startup summary from `memory_summary.md`.
- `agent.small_model.use_for_memory` enables lightweight-model routing for memory planning, classification, cleanup, and summary refresh.

> Note: `[agent.small_model]` is not exposed through the `/model` picker or the `/settings` model-config view — those surfaces only edit the main model (`agent.provider` + `agent.default_model`). The lightweight route is auto-selected from the main model's provider, or you can set `[agent.small_model]` directly in `vtcode.toml`.

Memory mutation is LLM-assisted only:

- natural-language `remember` / `forget` requests require a valid structured planner response
- session-finalization memory writes use the same LLM-assisted normalization path
- VT Code blocks the mutation instead of falling back to a plain or heuristic-only write when the memory LLM route is unavailable

`agent.persistent_memory.directory_override` is supported, but it may only be set from system, user, or project-profile config layers. A workspace-root `vtcode.toml` cannot redirect memory storage.

### Interactive controls

You can manage this feature without editing TOML directly:

- `/memory` shows loaded `AGENTS.md` sources, matched rules, memory files, pending rollout summaries, and quick actions.
- `/memory` also reports whether one-time legacy cleanup is required and can run that cleanup explicitly.
- `/config memory` jumps directly to the `agent.persistent_memory` settings section.
- `/config agent.persistent_memory` reaches the same section with the full path.

### OpenAI hosted shell skills

For native OpenAI Responses models, VT Code can replace the local `shell` tool with OpenAI's hosted shell environment and mount hosted skills into that environment. This path is separate from VT Code's local `SKILL.md` discovery system: VT Code does not upload or manage hosted skills for you in this workflow.

Use a pre-registered hosted skill by ID:

```toml
[provider.openai.hosted_shell]
enabled = true
environment = "container_auto"
file_ids = ["file_123"]

[[provider.openai.hosted_shell.skills]]
type = "skill_reference"
skill_id = "skill_123"
version = 2
```

Or mount an inline zip bundle directly:

```toml
[provider.openai.hosted_shell]
enabled = true
environment = "container_auto"

[[provider.openai.hosted_shell.skills]]
type = "inline"
bundle_b64 = "UEsFBgAAAAAAAA=="
sha256 = "deadbeef"
```

To allow outbound access for trusted domains in the hosted container, configure a request-scoped allowlist and optional domain secrets:

```toml
[provider.openai.hosted_shell]
enabled = true
environment = "container_auto"

[provider.openai.hosted_shell.network_policy]
type = "allowlist"
allowed_domains = ["httpbin.org"]

[[provider.openai.hosted_shell.network_policy.domain_secrets]]
domain = "httpbin.org"
name = "API_KEY"
value = "debug-secret-123"
```

Notes:

- `provider.openai.hosted_shell` is only used for OpenAI Responses-capable models on the native OpenAI endpoint.
- `environment = "container_reference"` reuses an existing OpenAI container and ignores `file_ids` and `skills`.
- `provider.openai.hosted_shell.network_policy` currently applies only to `container_auto`.
- `type = "allowlist"` requires at least one `allowed_domains` entry. Each `domain_secrets[*].domain` must also appear in `allowed_domains`.
- `version` may be omitted for the default `"latest"` behavior, or set to a pinned integer/string version when your hosted skill deployment requires it.

## External editor

Use `tools.editor` to control the external editor flow used by `/edit`, empty-prompt `Ctrl+E`, and single-click file links in the TUI.

```toml
[tools.editor]
enabled = true
preferred_editor = ""
suspend_tui = true
```

For real file opens, VT Code launches GUI editors immediately and returns to the session without waiting, including when an agent turn is active. Transcript and modal file links use an out-of-band bounded request queue; they do not submit `/edit` input. VS Code reuses the current window when supported and preserves line/column targets. Duplicate pending requests for the same target are coalesced.

If the selected editor is terminal-based (for example `vim`/`nvim`) and `suspend_tui = true`, VT Code suspends the TUI and waits for the editor to close through the serialized terminal-editor path. Temporary-file `/edit` flows still wait so VT Code can read edited content back into the composer.

### Interactive controls

You can manage this feature without editing TOML directly:

- `/config` shows an `External Editor` quick-access entry at the root.
- `/config tools.editor` opens the dedicated editor setup wizard directly.
- The guided flow can also take you to `/config file_opener` when you want to tune ANSI hyperlink URI handling separately.

For full editor detection, launcher behavior, and examples, see [External Editor Configuration](../tools/EDITOR_CONFIG.md).

## Fullscreen interaction

When VT Code is using alternate-screen rendering, you can tune fullscreen-specific mouse and transcript behavior with the `ui.fullscreen` table.

```toml
[ui.fullscreen]
mouse_capture = true
copy_on_select = true
scroll_speed = 3
```

- `mouse_capture` keeps mouse events inside VT Code for click-to-expand, click-to-position, link activation, and wheel scrolling. Set it to `false` when you want the terminal's native text selection while keeping fullscreen rendering.
- `copy_on_select` controls whether text selected inside VT Code is copied automatically on mouse release.
- `scroll_speed` multiplies mouse-wheel scrolling from `1` to `20`. It only affects wheel accumulation; page-based keyboard navigation is unchanged.

VT Code also honors these environment variables for default fullscreen behavior:

- `VTCODE_FULLSCREEN_MOUSE_CAPTURE=0|1`
- `VTCODE_FULLSCREEN_COPY_ON_SELECT=0|1`
- `VTCODE_FULLSCREEN_SCROLL_SPEED=<1-20>`

Transcript Review uses the same fullscreen rendering surface:

- The configured `open_transcript_review` binding (default `Ctrl+T`) opens or closes a session-local whole-conversation review with search, paging, complete copy, and export controls, including from inline mode.
- The configured `toggle_transcript_render_mode` binding (default `R`) toggles rich rendering and ANSI-free raw rendering.
- Compact successful command rows are contiguous-only; their styled shortcut and `click to expand` suffix is clickable when mouse capture is enabled and focuses the first capture in a group.
- `[` hands the complete conversation to native terminal scrollback until you return.
- `v` opens the complete conversation in your configured editor.
- The title's `[close]` control and the footer shortcut guide are mouse/keyboard affordances for the review panel.
- `open_transcript_review` and `toggle_transcript_render_mode` can be rebound through the existing keybinding configuration.
- `Alt+O` remains a compatibility alias. If the review action is unbound, `Ctrl+T` remains readline transpose.
- The review hint uses the primary `open_transcript_review` binding and is omitted when that action is unbound.

The compact review UX can be configured independently:

```toml
[ui.transcript_review]
show_hints = true
show_shortcut_guide = true
show_close_button = true

[ui.keybindings]
open_transcript_review = ["ctrl+t"]
toggle_transcript_render_mode = ["r"]
```

All three review controls default to enabled, while `ui.tool_display_mode`
defaults to `"compact"`. These settings affect only presentation; complete
captures and raw exports remain unchanged.

For the full shortcut list and tmux notes, see [Interactive Mode Reference](../user-guide/interactive-mode.md).

## Execution environment

### workspace.settings

Controls various workspace-specific settings for VT Code execution.

> **Note:** The `[workspace]` section is now implemented. When `use_root_config = true`,
> only the workspace root `vtcode.toml` is used as the active config layer; system,
> user, project, and dot-dir layers are discarded.

```toml
[workspace]
# When true, force workspace root vtcode.toml as the sole active config layer
# (system, user, project, and dot-dir layers are discarded)
use_root_config = true

# Controls whether to include workspace context in messages
include_context = true

# Maximum size of context to include (in bytes)
max_context_size = 1048576  # 1MB
```

### execution.timeout

Controls timeout settings for various operations:

```toml
[execution]
# Timeout for tool executions in seconds
tool_timeout = 300  # 5 minutes

# Timeout for API calls in seconds
api_timeout = 120   # 2 minutes

# Maximum time for participant context resolution
participant_timeout = 30  # 30 seconds
```

### Long-running command waits

The `write_stdin` and `unified_exec` tools support an explicit `wait` action
for command sessions. A wait deadline returns an in-progress session instead of
terminating it; call `wait` again with the returned `session_id` to continue
observing the same process. Waits are excluded from the ordinary per-turn
harness wall-clock budget, but cancellation and the long-running ceiling still
apply.

```toml
[timeouts]
# Hard upper bound for one explicit command wait (seconds).
long_running_command_ceiling_seconds = 3600
```

The requested `wait_timeout_seconds` is clamped to this ceiling. Command output
is kept memory-bounded; the tool response contains a bounded preview and a
`spool_path` when the spool file is open and healthy. For an active session,
`spool_complete = false` identifies a readable partial snapshot. If the
process has exited before draining finishes, `spool_pending = true` indicates
that a later wait can observe the completed spool.

## Context compaction and session history

VT Code has two compaction paths:

- provider-native compaction for providers that support Responses/API-managed compaction
- local fallback compaction for other providers

Local fallback compaction preserves a continuity tail of approximately 20,000
estimated tokens. The tail is made of complete user/assistant/tool protocol
groups; an incomplete trailing tool call is dropped. VT Code summarizes only
the older history prefix and rebuilds the preserved history as:

1. one structured summary message
2. retained recent real user messages and the continuity tail
3. the session memory envelope

Summarized session forks reuse that same handoff shape when you choose a summarized fork from `/fork` or pass `--summarize` on a forked CLI flow.

### Relevant settings

```toml
[agent.harness]
auto_compaction_enabled = true
auto_compaction_threshold_tokens = 120000

[context]
# Optional session safety ceiling; 0 follows the resolved model/provider capacity.
max_context_tokens = 0

[context.dynamic]
enabled = true
persist_history = true
retained_user_messages = 4
```

Notes:

- `agent.harness.auto_compaction_enabled` enables automatic compaction when prompt-side token pressure crosses the configured threshold.
- Disabling `agent.harness.auto_compaction_enabled` skips normal threshold-triggered compaction but does not disable the single bounded post-tool recovery compaction used to recover from a provider failure after tool output.
- `agent.harness.auto_compaction_threshold_tokens` applies to both provider-native compaction and VT Code's local fallback compaction. It remains authoritative when set, but never exceeds the provider's hard context capacity.
- When the harness threshold is unset, VT Code derives the effective prompt budget from the resolved model capacity, the provider route, and a positive `context.max_context_tokens` ceiling. It reserves the next response before deciding whether the request fits.
- `context.max_context_tokens = 0` preserves provider-only threshold resolution for compatibility; a known provider capacity is still a hard upper bound.
- `context.dynamic.persist_history = true` lets VT Code persist compaction artifacts and the session memory envelope so later resumes and summarized forks can reuse that context.
- `context.dynamic.retained_user_messages` controls how many recent real user messages VT Code preserves verbatim on the local fallback compaction path and in summarized forks. The default is `4`.
- The session memory envelope is VT Code's durable working-memory artifact. It is refreshed at turn boundaries and after completed child-agent results, then persisted beside history artifacts as `.memory.json`.
- A soft boundary may mark compaction for the next outer turn boundary; the effective prompt threshold still reserves output before the next model request. Compaction does not issue a hidden summary request from inside an active tool loop.
- Steering follow-ups are stored in schema-version 3 envelopes as UUID-tagged intents: at most 16 pending intents and the most recent 64 applied IDs are retained for restart recovery.

## MCP integration

### mcp

You can configure VT Code to use [Model Context Protocol (MCP) servers](https://modelcontextprotocol.io/) to give VT Code access to external applications, resources, or services.

#### Server configuration

MCP providers are configured as follows:

```toml
[mcp]
enabled = true  # Enable MCP integration

# List of MCP providers to use
[[mcp.providers]]
name = "context7"
command = "npx"
args = ["-y", "context7", "serve", "api"]
enabled = true

[[mcp.providers]]
name = "figma"
command = "figma-mcp-server"
args = ["--port", "4000"]
enabled = false  # Disabled by default

[[mcp.providers]]
name = "github"
command = "github-mcp-server"
enabled = true
```

#### Provider configuration options

Each MCP provider supports these options:

| Field     | Type    | Required | Description                                      |
| --------- | ------- | -------- | ------------------------------------------------ |
| `name`    | string  | Yes      | Unique identifier for the MCP provider           |
| `command` | string  | Yes      | Command to execute to start the MCP server       |
| `args`    | array   | No       | Arguments to pass to the command                 |
| `enabled` | boolean | No       | Whether this provider is enabled (default: true) |
| `env`     | table   | No       | Environment variables to pass to the server      |
| `cwd`     | string  | No       | Working directory for the command                |

## WebMCP browser bridge

WebMCP is a first-class, opt-in bridge for a browser editor. It is not an MCP provider and does not reuse `[mcp]` settings. The browser receives no direct filesystem capability: it sends digest-checked proposals over an origin-validated WebSocket, while VT Code or the headless full-auto policy remains the mutation authority. The bridge is shipped in the main `vtcode` binary; the repository's Vite project is the WebMCP browser app.

```toml
[webmcp]
enabled = false
host = "127.0.0.1"
port = 0
allowed_origins = ["http://localhost:5173"]
allowed_roots = []
pairing_ttl_secs = 300
max_frame_bytes = 1048576
max_in_flight_requests = 8

[webmcp.remote_mcp]
enabled = false
public_url = "https://mcp.example.com/sse/"
authorization_server = "https://login.example.com"
proxy_token_env = "VTCODE_WEBMCP_MCP_PROXY_TOKEN"
allowed_origins = []
max_results = 20
max_scan_files = 256
max_scan_bytes = 16777216
session_ttl_secs = 300
```

`allowed_origins` must contain exact browser origins; wildcards are rejected. Loopback is the default bind host and direct non-loopback binding is rejected. Remote access additionally requires explicit CLI opt-in, a `wss://` public URL, and a TLS-terminating reverse proxy forwarding to the loopback listener. Pairing codes expire after five minutes by default and are consumed once. Authenticated sessions use the same value as an inactivity lease and are refreshed by authenticated browser requests. Tokens remain in memory only.

`webmcp.remote_mcp` is a separate, disabled-by-default read-only MCP surface.
Its HTTPS `public_url` is the canonical `/sse/` endpoint, while `/mcp` serves
modern Streamable HTTP. The configured proxy token is read from
`proxy_token_env`; the external proxy validates OAuth and injects that internal
bearer token. `allowed_origins` in the nested table is an independent MCP
Origin allowlist, and missing MCP `Origin` is accepted. See the [WebMCP
development guide](../development/webmcp.md) for the protocol and threat
model.

## Security and approvals

### security

The security section defines how VT Code handles potentially dangerous operations:

```toml
[security]
# Enable human-in-the-loop approval for tool calls
human_in_the_loop = true

# Default policy for tool execution
# Options: "ask", "allow", "deny"
default_tool_policy = "ask"

# Whether trusted workspaces can bypass some security checks
trusted_workspace_mode = true
```

### tools.policies

Define specific policies for different tools:

```toml
[tools.policies]
# Policy for shell execution tools
exec_command = "ask"      # Options: "ask", "allow", "deny"
write_stdin = "ask"       # Options: "ask", "allow", "deny"
apply_patch = "ask"       # Options: "ask", "allow", "deny"
code_search = "allow"     # Options: "ask", "allow", "deny"
web_search = "ask"        # Options: "ask", "allow", "deny"

# Custom policies for specific tools
custom_tool_example = "deny"
```

### automation

Control automation behavior in VT Code:

```toml
[automation]
# Enable full automation mode (bypasses human approval)
full_auto = false

# Settings for automation when enabled
[automation.full_auto]
enabled = false
# List of tools that are allowed in full automation mode
allowed_tools = ["exec_command", "write_stdin", "apply_patch"]

[automation.scheduled_tasks]
enabled = false
```

`automation.scheduled_tasks.enabled` controls VT Code's internal scheduler surfaces:

- one-shot reminder interception such as `remind me at 3pm to ...`
- scheduler tool `cron` (actions: `create`, `list`, `delete`; legacy names `cron_create`, `cron_list`, `cron_delete` still route to it)
- durable `vtcode schedule ...` commands and the local scheduler daemon

This subsystem is opt-in. Set it to `true` when you want VT Code scheduling enabled.

Set `VTCODE_DISABLE_CRON=1` to disable the scheduler entirely, regardless of config.

## Participant system

### participants

Controls the behavior of the participant system that provides context augmentation:

```toml
[participants]
# Enable participant system for @mention support
enabled = true

# Default participants to always include
default_participants = ["@workspace", "@code"]

# Timeout for participant context resolution (in seconds)
timeout = 15

# Whether to cache participant context between messages
cache_context = true

# Maximum size of context that each participant can provide
max_context_size = 524288  # 512KB
```

### participant.settings

Individual settings for different participants:

```toml
[participants.workspace]
# Include file statistics in workspace context
include_file_stats = true

# Include git status in workspace context
include_git_status = true

# Maximum number of files to list
max_files_to_list = 100

[participants.code]
# Include syntax highlighting information
include_syntax_info = true

# Maximum file size to send for code context (in bytes)
max_file_size = 262144  # 256KB

[participants.terminal]
# Include recent terminal commands
include_recent_commands = true

# Number of recent commands to include
recent_commands_limit = 10

[participants.git]
# Include git repository information
include_repo_info = true

# Include git diff information
include_diff = false
```

## Profiles and overrides

### profiles

A _profile_ is a collection of configuration values that can be set together. Multiple profiles can be defined in `vtcode.toml` and you can specify the one you want to use depending on the project type or your current task.

Here is an example of a `vtcode.toml` that defines multiple profiles:

```toml
# Default settings
[agent]
provider = "openai"
default_model = "gpt-5"

[security]
human_in_the_loop = true
default_tool_policy = "ask"

# Profile for development work
[profiles.development]
[profiles.development.agent]
provider = "openai"
default_model = "gpt-5"

[profiles.development.security]
human_in_the_loop = true
default_tool_policy = "ask"

[profiles.development.participants]
default_participants = ["@workspace", "@code", "@git"]

# Profile for research work
[profiles.research]
[profiles.research.agent]
provider = "anthropic"
default_model = "claude-sonnet-5"

[profiles.research.tools.policies]
web_search = "allow"
code_search = "allow"
exec_command = "deny"

# Profile for local development with Ollama
[profiles.local]
[profiles.local.agent.provider_settings.ollama]
enabled = true

[profiles.local.agent]
provider = "ollama"
default_model = "llama3.1"

[profiles.local.security]
human_in_the_loop = false
default_tool_policy = "allow"
```

Users can specify config values at multiple levels. Values are merged from
lowest to highest precedence as follows:

1. Built-in defaults
2. System-level `/etc/vtcode/vtcode.toml` and `XDG_CONFIG_DIRS` candidates (Unix)
3. Legacy user-level `$VTCODE_HOME/vtcode.toml` (default `~/.vtcode/vtcode.toml`)
4. Canonical user-level config-directory `vtcode.toml`
5. Project profile `.vtcode/projects/<project>/config/vtcode.toml`
6. Workspace fallback `.vtcode/vtcode.toml`
7. Workspace root `vtcode.toml`
8. Explicit config file (`VTCODE_CONFIG_PATH` or `--config path/to/file.toml`), retaining the global layers
9. Runtime overrides (`-c/--config key=value`) and explicit runtime flags

Merge semantics are layered: tables merge recursively, while scalar and array values are replaced by higher-precedence layers.

### workspace-specific overrides

You can also define settings that only apply to specific workspace types:

```toml
# Settings for any workspace containing a package.json
[workspace.nodejs]
[workspace.nodejs.agent]
default_model = "gpt-5"

[workspace.nodejs.participants]
default_participants = ["@workspace", "@code", "@terminal"]

# Settings for any workspace containing a Cargo.toml
[workspace.rust]
[workspace.rust.agent]
default_model = "claude-sonnet-5"

[workspace.rust.participants]
default_participants = ["@workspace", "@code", "@terminal", "@git"]
```

## Observability and telemetry

### telemetry

VT Code can emit telemetry data about usage and performance:

```toml
[telemetry]
# Enable telemetry collection (disabled by default for privacy)
enabled = false

# Whether to include usage analytics
analytics = false

# Whether to report errors to the development team
report_errors = true

# Level of detail for telemetry data
# Options: "minimal", "basic", "detailed"
level = "minimal"
```

### logging

Configure logging behavior in VT Code:

```toml
[logging]
# Enable detailed logging (useful for debugging)
enabled = false

# Log level: "error", "warn", "info", "debug", "trace"
level = "info"

# Whether to include sensitive information in logs (never enabled by default)
include_sensitive = false

# Maximum size of log files before rotation (in bytes)
max_log_size = 10485760  # 10MB
```

## Authentication and authorization

### API keys

Each AI provider requires an API key configuration. These are typically managed through environment variables:

```bash
# Environment variables for API keys
OPENAI_API_KEY=your_openai_api_key
ANTHROPIC_API_KEY=your_anthropic_api_key
GOOGLE_GEMINI_API_KEY=your_google_api_key
DEEPSEEK_API_KEY=your_deepseek_api_key
META_API_KEY=your_meta_api_key
# Meta's documentation also names MODEL_API_KEY as the credential variable.
MODEL_API_KEY=your_meta_api_key
OPENROUTER_API_KEY=your_openrouter_api_key
AI_GATEWAY_API_KEY=your_vercel_ai_gateway_api_key
# Optional Vercel AI Gateway endpoint override:
# VERCEL_AI_GATEWAY_BASE_URL=https://ai-gateway.vercel.sh/v1
MIMO_API_KEY=your_mimo_api_key
EVOLINK_API_KEY=your_evolink_api_key
STEPFUN_API_KEY=your_stepfun_api_key
QWEN_API_KEY=your_qwen_api_key
NVIDIA_API_KEY=your_nvidia_api_key
MERGE_GATEWAY_API_KEY=your_merge_gateway_api_key
# Optional Merge Gateway endpoint override (native default):
# MERGE_GATEWAY_BASE_URL=https://api-gateway.merge.dev/v1
OLLAMA_HOST=http://localhost:11434  # For Ollama
```

### auth.settings

Authentication settings for VT Code:

```toml
[auth]
# Whether to store credentials securely in the OS keychain
secure_storage = true

# Whether to validate API keys on startup
validate_keys = true

# Timeout for authentication requests
timeout = 30  # seconds
```

## Editor context bridge

### ide_context

VT Code can ingest active-editor context from supported IDE families through a shared file bridge:

```toml
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
enabled = true
```

- `inject_into_prompt` injects a compact `Active Editor Context` block into request-time model input, outside the static system prompt.
- `show_in_tui` mirrors the same active editor summary in the inline header.
- `include_selection_text` only sends text when there is an explicit selection.
- `provider_mode` can force one family: `auto`, `vscode_compatible`, `zed`, or `generic`.
- `generic` is the stable bridge for JetBrains and other external adapters that write a canonical JSON snapshot and set `VT_IDE_CONTEXT_FILE`.

For the generic file contract and example payload, see [`docs/ide/editor-context-bridge.md`](../ide/editor-context-bridge.md).

## VS Code Integration

### VS Code Commands for Configuration

VT Code VS Code extension provides several commands to help manage configuration:

- `VT Code: Open Configuration` - Opens the workspace `vtcode.toml` file if it exists
- `VT Code: Toggle Human-in-the-Loop` - Quickly toggle the human_in_the_loop setting
- `VT Code: Configure MCP Providers` - Helper command to manage MCP provider settings
- `VT Code: Open Tools Policy Configuration` - Opens the tools policy section of the config

### Command System Integration

The VT Code extension uses a command system that can be configured through the settings:

```toml
# Configure which commands are available
[commands]
# Whether to enable the ask agent command
ask_agent_enabled = true

# Whether to enable the analyze workspace command
analyze_enabled = true

# Timeout for command execution (in seconds)
command_timeout = 300
```

### Workspace Trust

VT Code follows VS Code's workspace trust model. Some features are only available in trusted workspaces:

```toml
# This setting is respected by VS Code when determining workspace trust
[security]
trusted_workspace_mode = true
```

In untrusted workspaces, VT Code limits CLI automation capabilities to protect your system.

## Configuration Validation and Troubleshooting

### Validation

VT Code validates the configuration file on load. You can check for configuration errors by:

1. Looking at the VT Code output channel in VS Code
2. Using the `VT Code: Open Configuration` command which will highlight any parsing errors
3. Running `vtcode check-config` from the command line if you have the CLI installed

Common configuration errors include:

- Invalid TOML syntax
- Missing required API keys for selected providers
- Invalid provider names

### Troubleshooting

If VT Code is not behaving as expected with your configuration:

1. First, verify the configuration file parses correctly:

    ```toml
    # Make sure all tables close properly
    [agent]
    provider = "openai"
    default_model = "gpt-5"
    # No missing closing brackets
    ```

2. Check that required environment variables are set:

    ```bash
    # Verify API keys are available
    echo $OPENAI_API_KEY
    ```

3. Enable logging temporarily to see what's happening:
    ```toml
    [logging]
    enabled = true
    level = "debug"
    ```

## Config reference

For complete field coverage generated from the live `vtcode-config` schema, use
[`docs/config/CONFIG_FIELD_REFERENCE.md`](./CONFIG_FIELD_REFERENCE.md).

For harness behavior, read `agent.harness`, `automation.full_auto`, and `context.dynamic` together: they jointly define continuation,
turn limits, and context reuse for long-running exec sessions.

| Key                                     | Type / Values                                     | Notes                                                                                                                                                                         |
| --------------------------------------- | ------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `agent.provider`                        | string                                            | Provider to use (e.g., `openai`, `anthropic`, `google`, `meta`, `nvidia`, `merge-gateway`, `ollama`).                                                                          |
| `agent.default_model`                   | string                                            | Default model for the selected provider.                                                                                                                                      |
| `agent.context_window`                  | number                                            | Context window tokens.                                                                                                                                                        |
| `agent.max_output_tokens`               | number                                            | Max output tokens.                                                                                                                                                            |
| `agent.temperature`                     | number                                            | Model temperature (0.0-2.0).                                                                                                                                                  |
| `agent.top_p`                           | number                                            | Top-P sampling parameter (0.0-1.0).                                                                                                                                           |
| `context.semantic_compression`          | boolean                                           | Enable structural-aware context compression (default: false).                                                                                                                 |
| `context.tool_aware_retention`          | boolean                                           | Extend retention for recent tool outputs (default: false).                                                                                                                    |
| `context.max_structural_depth`          | number                                            | AST depth preserved when semantic compression is enabled (default: 3).                                                                                                        |
| `context.preserve_recent_tools`         | number                                            | Recent tool outputs to preserve when retention is enabled (default: 5).                                                                                                       |
| `security.human_in_the_loop`            | boolean                                           | Enable tool approval prompts (default: true).                                                                                                                                 |
| `security.default_tool_policy`          | `ask` \| `allow` \| `deny`                        | Default tool execution policy.                                                                                                                                                |
| `tools.policies.*`                      | `ask` \| `allow` \| `deny`                        | Policies for specific tools.                                                                                                                                                  |
| `mcp.enabled`                           | boolean                                           | Enable MCP integration (default: false).                                                                                                                                      |
| `mcp.providers[].name`                  | string                                            | MCP provider name.                                                                                                                                                            |
| `mcp.providers[].command`               | string                                            | MCP provider command to execute.                                                                                                                                              |
| `mcp.providers[].args`                  | array                                             | Arguments for the MCP command.                                                                                                                                                |
| `mcp.providers[].enabled`               | boolean                                           | Whether the provider is enabled.                                                                                                                                              |
| `participants.enabled`                  | boolean                                           | Enable participant system (default: true).                                                                                                                                    |
| `participants.default_participants`     | array                                             | Default participants to include.                                                                                                                                              |
| `participants.timeout`                  | number                                            | Timeout for participant context (seconds).                                                                                                                                    |
| `automation.full_auto.enabled`          | boolean                                           | Enable full automation.                                                                                                                                                  |
| `automation.full_auto.allowed_tools`    | array                                             | Tools allowed during full automation.                                                                                                                                             |
| `automation.full_auto.max_turns`        | integer                                           | Upper bound for autonomous turns before exec pauses.                                                                                                                          |
| `automation.scheduled_tasks.enabled`    | boolean                                           | Enable VT Code's internal scheduler for reminders, cron tools, and `vtcode schedule`. Can still be force-disabled with `VTCODE_DISABLE_CRON=1`.                      |
| `agent.harness.continuation_policy`     | `off` \| `exec_only` \| `all`                     | Controls when the harness may auto-continue after a completion attempt. Default: `all` in interactive and exec sessions; use `exec_only` to keep interactive sessions manual. |
| `agent.harness.event_log_path`          | string \| null                                    | Optional compatibility/export JSONL sink for harness events. Canonical events always live at `<workspace>/.vtcode/sessions/<session_id>/events.jsonl`; unset does not create a global harness file. |
| `sandbox.default_policy`                | `read_only` \| `workspace_write` \| `danger_full_access` \| `external` | Default sandbox policy.                                                                                                                                                      |
| `workspace.use_root_config`             | boolean                                           | When true, force workspace root vtcode.toml as the sole active config layer (system, user, project, and dot-dir layers discarded).                                            |
| `workspace.include_context`             | boolean                                           | Include workspace context.                                                                                                                                                    |
| `workspace.max_context_size`            | number                                            | Max size of workspace context (bytes).                                                                                                                                        |
| `execution.tool_timeout`                | number                                            | Timeout for tool executions (seconds).                                                                                                                                        |
| `execution.api_timeout`                 | number                                            | Timeout for API calls (seconds).                                                                                                                                              |
| `telemetry.enabled`                     | boolean                                           | Enable telemetry (default: false).                                                                                                                                            |
| `telemetry.analytics`                   | boolean                                           | Enable usage analytics.                                                                                                                                                       |
| `logging.enabled`                       | boolean                                           | Enable detailed logging.                                                                                                                                                      |
| `logging.level`                         | `error` \| `warn` \| `info` \| `debug` \| `trace` | Log level.                                                                                                                                                                    |
| `auth.secure_storage`                   | boolean                                           | Store credentials securely (default: true).                                                                                                                                   |
| `auth.validate_keys`                    | boolean                                           | Validate API keys on startup.                                                                                                                                                 |
| `commands.ask_agent_enabled`            | boolean                                           | Enable the ask agent command.                                                                                                                                                 |
| `commands.analyze_enabled`              | boolean                                           | Enable the analyze command.                                                                                                                                                   |
| `commands.command_timeout`              | number                                            | Command execution timeout (seconds).                                                                                                                                          |
| `profiles.*.agent.provider`             | string                                            | Provider override for a profile.                                                                                                                                              |
| `profiles.*.security.human_in_the_loop` | boolean                                           | Security setting override for a profile.                                                                                                                                      |
| `profiles.*.tools.policies.*`           | `ask` \| `allow` \| `deny`                        | Tool policy override for a profile.                                                                                                                                           |
| `providers.<name>.models`               | array                                             | Additional model identifiers to add to a built-in provider's model picker.                                                                                                    |
| `providers.<name>.base_url`             | string                                            | Override the provider's default API endpoint.                                                                                                                                  |
| `providers.<name>.api_key_env`          | string                                            | Override the provider's default API key environment variable.                                                                                                                  |
