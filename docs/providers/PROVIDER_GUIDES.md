# Provider Guides

This index collects provider-specific guides for configuring VT Code with different LLM backends.

## Provider whitelisting

Use `providers_whitelist` in `vtcode.toml` to restrict which providers VT Code may access. This is a governance control for environments where only approved inference endpoints should be reachable — for example, a corporate gateway or an air-gapped setup.

```toml
# Allow only corporate gateways + Gemini
providers_whitelist = ["opencode-zen", "opencode-go", "gemini"]
```

When `providers_whitelist` is non-empty:

- The `/model` picker shows only the listed providers.
- The first-run wizard offers only the listed providers.
- The startup validator rejects `agent.provider` values not in the list.
- Saving a model selection that falls outside the list is blocked.

When `providers_whitelist` is empty (the default), all built-in providers and `[[custom_providers]]` entries are available.

Whitelist entries may be a built-in provider key or a `name` from `[[custom_providers]]`. Matching is case-insensitive.

See the [Configuration guide](../config/config.md#provider-whitelisting) for full details.

## Custom providers

Use `[[custom_providers]]` for an OpenAI-compatible endpoint that does not have
a dedicated built-in provider. A custom provider can represent a private
gateway, an aggregator, or an internal inference service.

### Basic configuration

A typical entry includes a stable `name`, a human-friendly `display_name`, a
`base_url`, and a default `model`. Set `api_key_env` when the endpoint requires
an API key. Use `models` to expose several known model IDs in the model picker.

```toml
[[custom_providers]]
name = "mycorp"
display_name = "MyCorp"
base_url = "https://llm.corp.example/v1"
api_key_env = "MYCORP_API_KEY"
model = "gpt-5-mini"
models = ["gpt-5-mini", "gpt-5.6-sol"]
context_window = 256000   # optional; defaults to 128000 tokens
```

Set the corresponding environment variable before launching VT Code:

```bash
export MYCORP_API_KEY="..."
```

Secure credentials are scoped by the custom provider `name` and `api_key_env`.
They are not shared with another configured endpoint that happens to reuse the
same environment variable.

### Capability settings

`context_window` declares the provider capability in tokens. It controls the
context size shown in the UI, automatic compaction, and preflight token checks.
When omitted, the provider uses the default context window. The separate
`context.max_context_tokens` setting can impose a lower session budget.

Use `api_format` when the endpoint needs an explicit request shape. Accepted
values are `auto`, `openai-chat`, `openai-responses`, and
`anthropic-messages`. Omitting the field preserves legacy autodetection. An
explicit value is honored and does not silently fall back to another format.

Provider-level capability defaults are useful when the endpoint does not
return model metadata. Supported defaults include:

- `supports_tools`
- `supports_reasoning`
- `supports_reasoning_effort`
- `supports_vision`
- `supports_structured_output`
- `supports_parallel_tool_calls`
- `supports_context_caching`
- `supports_responses_compaction`
- `supports_context_edits`

Sampling values can also be pinned at the provider level or per model profile:
`temperature` (0.0-2.0), `top_p` (0.0-1.0), `top_k` (>= 0),
`presence_penalty` / `frequency_penalty` (-2.0-2.0), `max_tokens` (> 0), and
`reasoning_effort`. Pinning `reasoning_effort` in a profile implies effort
support for that model.

### Model profiles

Use sparse profiles for model-specific overrides:

```toml
[custom_providers.profiles."corp-model"]
api_format = "openai-responses"
context_window = 131072
temperature = 0.2            # sampling temperature (0.0-2.0)
# max_tokens = 8192          # overrides built-in per-task limits (800/2000)
reasoning_effort = "low"
supports_tools = true
supports_vision = false
supports_structured_output = true
supports_parallel_tool_calls = true
```

Profiles apply only to an existing model identifier. They do not add models to
the picker. Use `model` or `models` on the provider entry to control model
availability.

When VT Code resolves a model's runtime shape, values are applied in this
order, from highest to lowest priority:

1. The matching per-model profile
2. Provider-level capability defaults
3. Metadata returned by the provider
4. Conservative built-in defaults

Sampling values resolve on the same chain with one extra global layer beneath
the provider: profile → provider default → `agent.temperature` /
`agent.reasoning_effort` globals → built-in per-task limits (`max_tokens`
only). Simple sub-tasks force `reasoning_effort = "minimal"` regardless of a
profile pin, and reasoning-rejecting backends (native Anthropic/MiniMax, or
`api_format = "anthropic-messages"` profiles) drop `temperature` while
reasoning is active.

An explicit boolean `false` is honored at every level. Omitting `api_format`
preserves autodetection, while an explicit value selects that API shape.

Name-based OpenAI sampling gates apply to custom endpoints too: models named
`gpt`, `gpt-5.2`, `gpt-5.4`, `gpt-5.5*` accept sampling only while reasoning
effort resolves to `none` (pinned values are silently omitted otherwise),
`gpt-5`/`gpt-5-mini`/`gpt-5-nano` never receive sampling parameters, and
`gpt-6-astra` never receives sampling parameters (`temperature`/`top_p` are
unsupported by the model). Prefer neutral model IDs on gateways if you need
pinned values on such names.

### Validate the configuration

List built-in and custom providers, then open the provider configuration flow:

```bash
vtcode models list
vtcode models config
```

Run a simple request after configuration:

```bash
vtcode ask "Summarize this repository"
```

Worked examples: [Atlas Cloud](./atlascloud.md) and [OmniRoute](./omniroute.md).
See the [Configuration guide](../config/config.md#custom_providers) for the
complete field reference and precedence rules.

## Collapsed tool-result disclosure

When compact transcript review or bounded tool-output mode hides most of a
tool result, VT Code adds one exact disclosure to canonical conversation
history for every provider and model:

```text
Only you see that command's output — the user's terminal shows at most a few lines of it. If the user needs to read any of it, put it in your reply.
```

Provider adapters translate that typed history marker into their supported
system, history, instructions, or transcript representation. Anthropic wire
routes whose selected provider/model capability supports it use
`clear_at: "next_user_message"` with the required beta; unsupported Anthropic
models and gateways use a top-level system directive. Routes without that
capability never receive the Anthropic-only `clear_at` field. Complete tool
output remains available in Transcript Review, so this disclosure does not
replace the retained evidence or expose provider chain-of-thought.

## Google Gemini

-   **Official docs:** [Gemini API models](https://ai.google.dev/gemini-api/docs/models) · [Gemini 3.7 Flash](https://ai.google.dev/gemini-api/docs/models/gemini-3.7-flash)
-   **Provider key:** `gemini` (env: `GEMINI_API_KEY` or `GOOGLE_API_KEY`)
-   **Default model:** `gemini-3-flash-preview`
-   **Curated models:**
    -   `gemini-3.7-flash` — latest flash model, 1M context, tunable thinking (low/medium/high)
    -   `gemini-3.6-flash` — flash model with improved reasoning and efficiency, 1M context
    -   `gemini-3.5-flash-lite` — cost-optimized lightweight flash model, 1M context
-   **Features:** Streaming, tool calls, structured output, image/video/audio input, context caching, code execution, and configurable reasoning effort
-   Configuration details are covered in the main [Getting Started guide](../user-guide/getting-started.md#api-requirements).
-   Models and constants are defined in [`crates/codegen/vtcode-config/src/constants/models/google.rs`](../../crates/codegen/vtcode-config/src/constants/models/google.rs).

## OpenAI

-   **Official docs:**
    -   [API reference index](https://developers.openai.com/api/reference/llms.txt)
    -   [Models catalog](https://developers.openai.com/api/docs/models)
    -   [Deprecations](https://developers.openai.com/api/docs/deprecations)
- Follow the [Getting Started guide](../user-guide/getting-started.md#api-requirements) for API key setup.
-   See [`crates/codegen/vtcode-config/src/constants/models/openai.rs`](../../crates/codegen/vtcode-config/src/constants/models/openai.rs) for the latest supported models.
-   **Authentication methods (in priority order):**
    1.  **ChatGPT subscription OAuth** — `vtcode login openai` or `/login openai`. No API key needed. VT Code performs an in-process PKCE browser login with full auto-refresh. The Codex CLI is **not** required. By default, VT Code reuses Codex's public OAuth client identity as an **unofficial compatibility mechanism** (OpenAI has not documented or guaranteed third-party reuse, and a public client ID is not authorization to reuse another tool's registration). Organizations with their own OpenAI-issued client pair can override via `VTCODE_OPENAI_OAUTH_CLIENT_ID` / `VTCODE_OPENAI_OAUTH_ORIGINATOR` (both must be set together).
    2.  **Codex auth.json fallback** — if you have Codex CLI installed and authenticated (`codex login`), VT Code automatically detects `~/.codex/auth.json` and uses it at runtime when no VT Code-managed session is stored. Validate with `vtcode login openai --from-codex`.
    3.  **API key** — `vtcode secret add openai` or set `OPENAI_API_KEY`. Use `/secret` to manage stored keys.
-   **Login/logout commands:**
    -   CLI: `vtcode login openai`, `vtcode login openai --from-codex`, `vtcode logout openai`
    -   TUI: `/login openai`, `/logout openai`, `/auth`
-   **Logout semantics:** `vtcode logout openai` (or `/logout openai`) clears VT Code's managed session only. If Codex's auth.json exists, VT Code will continue using it as a fallback until you run `codex logout`.
-   See the [OAuth authentication guide](../guides/oauth-authentication.md) for full details.
-   VT Code's default OpenAI profile is `gpt-5.4` with `reasoning_effort = "none"` and `verbosity = "medium"`; raise reasoning only when the task shape justifies the extra latency.
-   VT Code applies a compact GPT-5.4 prompt contract rather than a verbatim cookbook prompt: compact outputs, low-risk follow-through, dependency-aware tool use, completeness checks, verification, and conditional grounding/citation rules.
-   Deprecated models (gpt-5, gpt-5-mini, gpt-5-nano, o3, o4-mini, gpt-5-codex, gpt-5.1-codex, etc.) are removed from the model picker but retained in routing constants for backward compatibility with existing configs.
-   **GPT-6 Astra (`gpt-6-astra`):** most capable OpenAI model for end-to-end agentic work (1,050,000 context, 128k max output, reasoning `low`/`medium`/`high`/`xhigh`/`max`). The catalog drives its Responses API route, sampling and logprob capabilities, cache TTL, prompt contract, and supported reasoning levels. Unsupported reasoning requests are blocked before transport unless `agent.allow_reasoning_effort_downgrade` explicitly permits selection of the nearest lower catalog level.
-   File inputs are supported for native OpenAI Responses API requests through `input_file` parts.
-   Supported file input fields in VT Code message parts: `file_id`, `file_data`, `file_url`, `filename`.
-   `file_url` is Responses API only; VT Code rejects `file_url` when a request uses Chat Completions.
-   VT Code only upgrades local non-image file refs such as `@report.pdf` and `@"Quarterly Deck.pptx"` into structured file attachments for native OpenAI Responses sessions on `api.openai.com`.
-   Remote external document URLs such as `@https://example.com/letter.pdf` are also only elevated to structured `file_url` inputs for native OpenAI Responses sessions.
-   ChatGPT subscription sessions, OpenAI-compatible endpoints, and other providers keep non-image `@file` refs as plain text plus file-reference metadata so the agent can resolve the path and read the file with tools.
-   Raw image paths still use the existing multimodal image path flow. Non-image files require explicit `@...` references.
-   Official OpenAI Responses replays now preserve assistant phase metadata for replayed assistant history (`commentary` for preambles/progress updates, `final_answer` for completed answers) when the target GPT model supports it. VT Code does not send this field to Chat Completions, tool/user items, or non-native OpenAI-compatible endpoints.
-   OpenAI Responses hosted tools currently map through `ToolDefinition` for `web_search`, `file_search`, hosted `tool_search`, and remote `mcp`, with hosted config passed through directly on each tool entry.
-   OpenAI hosted shell mounts are configured through `provider.openai.hosted_shell` in `vtcode.toml`.
-   Hosted shell skill mounts support both `skill_reference` and `inline` bundle entries; VT Code forwards them to OpenAI but does not upload/create hosted skills in this path.
-   This hosted-shell workflow is separate from VT Code's local `SKILL.md` filesystem skills.
-   For large corpora, prefer File Search/Retrieval instead of sending full files inline.
-   For spreadsheet-heavy analysis, use Hosted Shell workflows instead of large inline sheet prompts.

## Anthropic

-   **Provider key:** `anthropic` (env: `ANTHROPIC_API_KEY`)
-   **Default model:** `claude-sonnet-5`
-   **Curated models:** `claude-sonnet-5`, `claude-fable-5`, `claude-mythos-5`, `claude-opus-5`, `claude-opus-4-8`, `claude-sonnet-4-6`, and `claude-haiku-4-5`
-   Collapsed or bounded tool output is disclosed to every provider/model with the fixed turn-scoped notice. Anthropic wire routes use `clear_at: "next_user_message"` plus the required beta only when the selected provider/model capability allows it; unsupported Claude models and other routes use ordinary top-level system/history mapping.
-   Key management and defaults mirror the Gemini/OpenAI flow in [Getting Started](../user-guide/getting-started.md#api-requirements).
-   Supported model IDs live in [`crates/codegen/vtcode-config/src/constants/models/anthropic.rs`](../../crates/codegen/vtcode-config/src/constants/models/anthropic.rs).

## DeepSeek

-   **Provider key:** `deepseek`
-   **Authentication:** `DEEPSEEK_API_KEY` environment variable
-   **Base URL:** `https://api.deepseek.com/v1`, override with `DEEPSEEK_BASE_URL`
-   **Default model:** `deepseek-v4-pro`
-   **Curated models:**
    -   `deepseek-v4-pro` — high-performance reasoning model with advanced thinking capabilities
    -   `deepseek-v4-flash` — official release with significantly enhanced agent capabilities for coding and tool use
-   **Features:** Streaming, tool calls, structured output, and reasoning support

## xAI (Grok)

-   **Official docs:** [xAI Docs](https://docs.x.ai) · [Models](https://docs.x.ai/developers/models)
-   **Provider key:** `xai`
-   **Authentication:** `XAI_API_KEY` environment variable
-   **Setup:** Set `XAI_API_KEY` from the [xAI Console](https://console.x.ai/), then configure `provider = "xai"` in `vtcode.toml`
-   **Default model:** `grok-4.6`
-   **Curated models:**
    -   `grok-4.6` — flagship reasoning model, 500k context, reasoning_effort (low/medium/high/xhigh)
    -   `grok-4.5` — previous flagship reasoning model, 500k context
    -   `grok-4.3` — balanced general-purpose model, 1M context
    -   `grok-build-0.1` — fast coding model for agentic software engineering, 256k context
-   **Features:** Streaming, tool calls, structured output, image input, configurable reasoning effort, and 500k-token context

## Meta AI

-   **Guide:** [Meta AI Integration](./meta.md)
-   **Official docs:** [LLM documentation](https://dev.meta.ai/docs/llms.txt) · [Models](https://developer.meta.com/ai/models)
-   **Provider key:** `meta`
-   **Authentication:** `META_API_KEY` or Meta's documented `MODEL_API_KEY`
-   **Base URL:** `https://api.meta.ai/v1`, override with `META_BASE_URL`
-   **Default model:** `muse-spark-1.2`
-   **Curated models:** Muse Spark 1.2, Muse Spark 1.1, and the opt-in Muse Spark 1.2 Contributor tier
-   **Features:** Streaming, tool calls, structured output, multimodal input, reasoning effort, and 1M-token context

## NVIDIA NIM

-   **Guide:** [NVIDIA NIM Integration](./nvidia.md)
-   **Official docs:** [NVIDIA API catalog](https://build.nvidia.com/llms.txt) · [NIM API reference](https://docs.nvidia.com/nim/large-language-models/latest/api-reference.html)
-   **Provider key:** `nvidia`
-   **Authentication:** `NVIDIA_API_KEY` (Bearer token)
-   **Base URL:** `https://integrate.api.nvidia.com/v1`, override with `NVIDIA_BASE_URL`
-   **Default model:** `nvidia/nemotron-3-ultra-550b-a55b`
-   **Curated picker models:** Nemotron 3 Ultra, Nemotron 3 Super, Nemotron 3 Nano, GLM-5.2, and DeepSeek V4 Flash
-   **Features:** Streaming, tool calls, structured output, 1M-token context, and NVIDIA reasoning-content extraction

## Merge Gateway

-   **Guide:** [Merge Gateway Integration](./merge-gateway.md) · [Quick Reference](./merge-gateway-quick-reference.md)
-   **Official docs:** [Merge Gateway API overview](https://docs.merge.dev/merge-gateway/api-overview/llms.txt) · [Coding-agent integration](https://docs.merge.dev/merge-gateway/features/use-in-your-ide/overview)
-   **Provider key:** `merge-gateway`
-   **Authentication:** `MERGE_GATEWAY_API_KEY` (Bearer token; create a key in the [Merge dashboard](https://dashboard.merge.dev/))
-   **Base URL:** `https://api-gateway.merge.dev/v1` (native Responses), override with `MERGE_GATEWAY_BASE_URL`; explicit `/v1/openai` selects legacy Chat Completions
-   **Default model:** `default_routing`
-   **Curated picker models:** `openai/gpt-5.5`, `anthropic/claude-opus-5`, `google/gemini-3.6-flash`, `google/gemini-3.7-flash`, `deepseek/deepseek-v4-pro-0813`, `deepseek/deepseek-v4-flash-0731`, `xai/grok-4.6`, `qwen/qwen3.8-max`, `minimax/minimax-h3`, `moonshot/kimi-k3`, `thinkingmachines/inkling`, `meta/muse-spark-1.1`, `zai/glm-5.3-flash`, `openai/gpt-5.6-luna`, `openai/gpt-5.6-sol`, and `openai/gpt-5.6-terra`
-   **Features:** Native Responses, streaming, tool calling, structured outputs, authenticated paginated model catalog, cache-backed picker metadata, and arbitrary explicit Merge route IDs
-   **Limitations:** Reasoning controls remain route-specific and are not inferred generically; routing metadata and billed cost remain outside VT Code's normalized response fields. Explicit `/v1/openai` endpoints retain the legacy compatibility path.

## GitHub Copilot

-   **Guide:** [GitHub Copilot Managed Auth](./copilot.md)
-   **Runtime dependency:** `copilot` must be installed and runnable for login/logout
-   **Optional fallback:** `gh` is only used when VT Code probes an existing GitHub CLI auth session
-   **Commands:** `vtcode login copilot`, `vtcode logout copilot`, `/login copilot`, `/logout copilot`

## OpenRouter Marketplace

-   **Guide:** [OpenRouter Integration](./openrouter.md)
-   **Official docs:**
    -   [API overview](https://openrouter.ai/docs/api-reference/overview/llms)
    -   [Streaming](https://openrouter.ai/docs/api-reference/streaming/llms)
    -   [Model catalog](https://openrouter.ai/docs/llms)
-   Default model: `xiaomi/mimo-v2.5-pro` (VT Code's default). Xiaomi MiMo V2.5 and V2.5 Pro are also available.
-   For Meta Muse, prefer the official [`meta` provider](./meta.md) when direct Meta access is desired. OpenRouter's `meta/...` entries are separately namespaced marketplace routes.
-   **Meta Muse models via OpenRouter:** `meta/muse-glimmer-30b` and `meta/muse-spark-1.2`
-   **Curated picker catalog:** `openrouter/meta/muse-glimmer-30b`, `openrouter/meta/muse-spark-1.2`, `openrouter/deepseek/deepseek-chat`, `openrouter/z-ai/glm-5.1`, `openrouter/z-ai/glm-5.2`, `openrouter/z-ai/glm-5.3-flash`, `openrouter/moonshotai/kimi-k3`, `openrouter/moonshotai/kimi-k2.6`, `openrouter/moonshotai/kimi-k2.7-code`, `openrouter/qwen/qwen3.7-max`, `openrouter/tencent/hy3-preview`, `openrouter/x-ai/grok-build-0.1`, `openrouter/x-ai/grok-4.6`, `openrouter/xiaomi/mimo-v2.5`, `openrouter/xiaomi/mimo-v2.5-pro`, `openrouter/poolside/laguna-m.1:free`, `openrouter/poolside/laguna-s-2.1:free`, `openrouter/google/gemini-3.5-flash-lite`, `openrouter/google/gemini-3.6-flash`, `openrouter/google/gemini-3.7-flash`, and `openrouter/qwen/qwen3.8-27b`
-   **Xiaomi MiMo models:**
    -   `xiaomi/mimo-v2.5-pro` — flagship agentic model, 1M context, reasoning + tool calls
    -   `xiaomi/mimo-v2.5` — omnimodal model, 1M context, reasoning + tool calls

## Atlas Cloud

-   **Guide:** [Atlas Cloud Integration](./atlascloud.md)
-   **Official docs:**
    -   [LLM / Chat](https://www.atlascloud.ai/docs/models/llm)
    -   [FAQ](https://www.atlascloud.ai/docs/en/faq)
-   **Integration mode:** configure Atlas Cloud through VT Code's `[[custom_providers]]` support because the LLM endpoint is OpenAI-compatible.
-   **Base URL:** `https://api.atlascloud.ai/v1`
-   **Recommended model:** start with `deepseek-ai/deepseek-v4-flash` (DeepSeek's latest flash model, 1M context, $0.14/M input tokens).

## Vercel AI Gateway

-   **Guide:** [Vercel AI Gateway Integration](./vercel-ai-gateway.md) · [Quick Reference](./vercel-ai-gateway-quick-reference.md)
-   **Official docs:** [Vercel AI Gateway](https://vercel.com/docs/ai-gateway) · [Getting started](https://vercel.com/docs/ai-gateway/getting-started)
-   **Provider key:** `vercel` (aliases: `vercel-ai-gateway`, `ai-gateway`)
-   **Auth:** `AI_GATEWAY_API_KEY` (Bearer token; create a key in the [Vercel dashboard](https://vercel.com/dashboard) under AI Gateway)
-   **Base URL:** `https://ai-gateway.vercel.sh/v1` (OpenAI Chat Completions compatible), override with `VERCEL_AI_GATEWAY_BASE_URL`
-   **Default model:** `anthropic/claude-sonnet-5`
-   **Curated models:** 17 models from Anthropic, OpenAI, Google, DeepSeek, Moonshot, Alibaba, MiniMax, and Mistral
-   **Features:** Streaming, tool calls, structured output, reasoning traces (all curated models except `alibaba/qwen3-coder-next` and `mistral/devstral-2`), zero token markup, automatic failover, and unlisted gateway `vendor/model` IDs accepted
-   **Shared IDs:** several curated IDs also exist in OpenRouter's catalog; set `provider = "vercel"` explicitly when configuring the model string (picking from the `/model` picker always routes to Vercel)

## OmniRoute

-   **Guide:** [OmniRoute Integration](./omniroute.md)
-   **Integration mode:** configure OmniRoute through `[[custom_providers]]` as an OpenAI-compatible gateway.
-   **Local base URL:** `http://localhost:20128/v1`
-   **Default model:** `auto` delegates model selection and fallback to OmniRoute.
-   **Features:** Chat Completions, optional Responses API routing for compatible models, streaming, and function tools through VT Code's shared OpenAI-compatible transport.

## Xiaomi MiMo

-   **Provider key:** `mimo`
-   **Docs:** [Xiaomi MiMo Platform](https://platform.xiaomimimo.com/docs/en-US/welcome)
-   **Pricing:** [Pay-as-you-go](https://platform.xiaomimimo.com/docs/en-US/price/pay-as-you-go) · [Subscription](https://platform.xiaomimimo.com/docs/en-US/price/tokenplan/subscription) · [Quick Access](https://platform.xiaomimimo.com/docs/en-US/price/tokenplan/quick-access)
-   **Setup:** Set `MIMO_API_KEY` or use the MiMo provider in VT Code's configuration
-   **Models:**
    -   `mimo-v2.5-pro` — flagship agentic model, 1M context, deep thinking
    -   `mimo-v2.5` — omnimodal model (text, image, audio, video), 1M context

## Ollama Local & Cloud Models

-   **Guide:** [Local Inference Servers](./local-servers.md) (unified `/local` command)
-   **Setup:** Install and run Ollama locally ([official install](https://ollama.com/download))
-   **Configuration:** Local usage needs no key; set `OLLAMA_API_KEY` to access Ollama Cloud
-   **Default model:** `gpt-oss:20b` (local); any locally available model works
-   **Curated picker catalog:** `gpt-oss:20b`, `gemma4`, plus cloud models `deepseek-v4-flash:cloud`, `deepseek-v4-pro:cloud`, `nemotron-3-ultra:cloud`, `kimi-k3:cloud`, `minimax-m3:cloud`, and `glm-5.2:cloud` (also `gpt-oss:120b-cloud` via the OpenAI OSS support)
-   **Cloud models:** Use IDs like `gpt-oss:120b-cloud` with `OLLAMA_BASE_URL=https://ollama.com`
-   **Custom Models:** Use the `custom-ollama` option in the model picker to enter any locally or cloud-available model ID
-   **Base URL:** Configurable via `OLLAMA_BASE_URL` environment variable (defaults to `http://localhost:11434`)
-   **Features:** Streaming, structured tool calling (including Ollama's web search tools), and thinking traces when `reasoning_effort` is enabled

## LM Studio Local Server

-   **Guide:** [LM Studio Provider Guide](./lmstudio.md)
-   **Server:** Enable the OpenAI-compatible Developer server in LM Studio (defaults to `http://localhost:1234/v1`)
-   **Environment:** Optional `LMSTUDIO_API_KEY` when auth is enabled; override host/port via `LMSTUDIO_BASE_URL`
-   **Default model:** `lmstudio-community/openai-gpt-oss-20b` (local inference)
-   **Catalog:** Also ships with `lmstudio-community/meta-llama-3.1-8b-instruct` and `lmstudio-community/gemma-3-12b-it`, plus any custom GGUF models you expose
-   **Features:** Streaming, tool calling, structured output, and reasoning effort passthrough via the shared OpenAI surface

## llama.cpp Local Server

-   **Guide:** [llama.cpp Provider Guide](./llamacpp.md)
-   **Server:** VT Code targets `llama-server` and defaults to `http://localhost:8080/v1`
-   **Environment:** `LLAMACPP_BASE_URL` overrides the endpoint; `LLAMACPP_MODEL_PATH` enables VT Code-managed startup
-   **Managed startup:** VT Code can launch `llama-server -m /path/to/model.gguf --port ...` when the endpoint is localhost and a GGUF path is configured
-   **Starter catalog:** `gpt-oss-20b`, `gemma-4-26b-a4b`, `gemma-4-e4b`, and `step-3.5-flash`
-   **Features:** Streaming, dynamic `/v1/models` discovery, local no-auth defaults, and OpenAI-compatible request handling

## Evolink Multi-Model Gateway

-   **Provider key:** `evolink`
-   **Official docs:** [Evolink Docs](https://docs.evolink.ai/llms.txt)
-   **Base URL:** `https://direct.evolink.ai/v1`
-   **Auth:** `EVOLINK_API_KEY` environment variable
-   **Setup:** Set `EVOLINK_API_KEY` from [Evolink dashboard](https://evolink.ai/dashboard/keys), then configure `provider = "evolink"` in `vtcode.toml`
-   **Models:**
    -   `evolink/gpt-5.2` (default)
    -   `evolink/gpt-5.5`
    -   `evolink/deepseek-v4-pro`
    -   `evolink/deepseek-v4-flash`
    -   `evolink/doubao-seed-2.0-pro`
    -   `evolink/gemini-3.1-pro-preview`
    -   `evolink/gemini-3.5-flash`
    -   `evolink/MiniMax-M3`
    -   `evolink/claude-sonnet-4-6`
    -   `evolink/claude-opus-4-8`
    -   `evolink/claude-haiku-4-5-20251001`
-   **Features:** OpenAI-compatible gateway exposing many upstream models behind one endpoint. Evolink serves models under bare upstream names (e.g. `gpt-5.2`) that collide with VT Code's first-class providers, so curated model IDs are namespaced as `evolink/<model>`. The provider strips the prefix before sending requests upstream.

## Anthropic API Compatibility Server

VT Code provides compatibility with the Anthropic Messages API to help connect existing applications to VT Code, including tools like Claude Code.

- **Feature:** Anthropic API compatibility server
- **Command:** `vtcode anthropic-api --port 11434`
- **Endpoint:** `/v1/messages` (mirrors Anthropic Messages API)
- **Environment variables:**
  - `ANTHROPIC_AUTH_TOKEN=ollama` (required but ignored)
  - `ANTHROPIC_BASE_URL=http://localhost:11434`
  - `ANTHROPIC_API_KEY=ollama` (required but ignored)
- **Features:** Streaming, tool calling, vision support, multi-turn conversations

## Z.AI (ZAI)

-   **Guide:** [Z.AI Integration](./zai.md) · [Quick Reference](./zai-quick-reference.md)
-   **Provider key:** `zai`
-   **Official docs:** [Z.AI Platform](https://z.ai/docs) · [GLM-5.3 Flash](https://docs.z.ai/guides/vlm/glm-5.3-flash) · [GLM-5 docs](https://docs.z.ai/guides/llm/glm-5)
-   **Auth:** `ZAI_API_KEY` environment variable
-   **Base URL:** `https://api.z.ai/api`, override with `ZAI_BASE_URL`
-   **Setup:** Set `ZAI_API_KEY` from Z.AI platform, then configure `provider = "zai"` in `vtcode.toml`
-   **Models:**
    -   `glm-5.3` — flagship coding model, 1M context, reasoning + tool calls
    -   `glm-5.3-flash` — efficient multimodal model, 320B total / 18B active, hybrid sparse+linear attention, 1M context, native vision (text+image), reasoning + tool calls
    -   `glm-5.2` — flagship model for long-horizon tasks, 1M context, reasoning + tool calls
-   **Default:** `glm-5.3`
-   **Features:** Streaming, tool calling, reasoning effort support, native vision (Flash), context caching, structured output, tool streaming

## Moonshot (Kimi)

-   **Provider key:** `moonshot`
-   **Official docs:** [Moonshot Platform](https://platform.moonshot.ai/docs)
-   **Auth:** `MOONSHOT_API_KEY` environment variable
-   **Setup:** Set `MOONSHOT_API_KEY` from Moonshot platform, then configure `provider = "moonshot"` in `vtcode.toml`
-   **Models:**
    -   `kimi-k3` — 2.8T parameter flagship with Delta Attention, native vision, 1M context
    -   `kimi-k2.7-code` — most capable coding model with long-horizon coding breakthrough, 256K context
    -   `kimi-k2.5` — enhanced reasoning model
-   **Default:** `kimi-k3`
-   **Features:** Streaming, tool calling, reasoning support, multimodal input (text, image, video)

## StepFun

-   **Provider key:** `stepfun`
-   **Official docs:** [StepFun Platform](https://platform.stepfun.ai/docs)
-   **Auth:** `STEPFUN_API_KEY` environment variable
-   **Setup:** Set `STEPFUN_API_KEY` from StepFun platform, then configure `provider = "stepfun"` in `vtcode.toml`
-   **Models:**
    -   `step-3.7-flash` — efficient reasoning model based on sparse MoE architecture
-   **Default:** `step-3.7-flash`
-   **Features:** Streaming, tool calling, reasoning support

## MiniMax

-   **Provider key:** `minimax`
-   **Official docs:** [MiniMax Platform](https://platform.minimax.io/docs)
-   **Auth:** `MINIMAX_API_KEY` environment variable
-   **Setup:** Set `MINIMAX_API_KEY` from MiniMax platform, then configure `provider = "minimax"` in `vtcode.toml`
-   **Models:**
    -   `MiniMax-M3` — frontier multimodal coding model, 1M context
    -   `MiniMax-M2.5` — efficient model for general tasks
-   **Default:** `MiniMax-M3`
-   **Features:** Streaming, tool calling

## HuggingFace

-   **Provider key:** `huggingface`
-   **Official docs:** [HuggingFace Inference API](https://huggingface.co/docs/api-inference)
-   **Auth:** `HF_TOKEN` environment variable
-   **Base URL:** `https://router.huggingface.co/v1`, override with `HUGGINGFACE_BASE_URL`
-   **Setup:** Set `HF_TOKEN` from HuggingFace settings, then configure `provider = "huggingface"` in `vtcode.toml`
-   **Default model:** `openai/gpt-oss-120b:huggingface`
-   **Notable models:** `openai/gpt-oss-20b:huggingface`, `deepseek-ai/DeepSeek-R1`, `deepseek-ai/DeepSeek-V4-Pro:together`, `deepseek-ai/DeepSeek-V4-Pro:novita`, `zai-org/GLM-5.1:zai-org`, `zai-org/GLM-5.2:novita`, `moonshotai/Kimi-K3:together`, `moonshotai/Kimi-K2.6:novita`, `MiniMaxAI/MiniMax-M3:novita`, `MiniMaxAI/MiniMax-M2.7:novita`, `stepfun-ai/Step-3.5-Flash:featherless-ai`
-   **Features:** Access to various models through HuggingFace's inference API, including models from OpenAI, DeepSeek, Z.AI, Moonshot, MiniMax, and other providers

## Poolside

-   **Provider key:** `poolside`
-   **Auth:** `POOLSIDE_API_KEY` environment variable
-   **Base URL:** `https://api.poolsi.de/openai/v1`, override with `POOLSIDE_BASE_URL`
-   **Default model:** `poolside/laguna-s-2.1`
-   **Curated picker models:** `poolside/laguna-s-2.1`, `poolside/laguna-m.1`, `poolside/laguna-xs.2`
-   **Setup:** Set `POOLSIDE_API_KEY` from Poolside platform, then configure `provider = "poolside"` in `vtcode.toml`

## Mistral

-   **Provider key:** `mistral`
-   **Authentication:** `MISTRAL_API_KEY` environment variable
-   **Base URL:** `https://api.mistral.ai/v1`, override with `MISTRAL_BASE_URL`
-   **Default model:** `mistral-large-2512`
-   **Curated picker models:** `mistral-large-2512`, `mistral-medium-3-5`, `mistral-small-2603`, `mistral-medium-2508`, `codestral-2508`
-   **Features:** Streaming, tool calls, structured output, and reasoning support

## Qwen

-   **Provider key:** `qwen`
-   **Authentication:** `QWEN_API_KEY` (alternate `DASHSCOPE_API_KEY`)
-   **Base URL:** `https://dashscope.aliyuncs.com/compatible-mode/v1`, override with `QWEN_BASE_URL`
-   **Default model:** `deepseek-v4-flash`
-   **Curated picker models:** `deepseek-v4-flash`, `deepseek-v4-pro`, `glm-5.1`
-   **Features:** Streaming, tool calls, and reasoning support

## OpenCode Zen

-   **Provider key:** `opencode-zen`
-   **Authentication:** `OPENCODE_ZEN_API_KEY` environment variable
-   **Base URL:** `https://opencode.ai/zen/v1`, override with `OPENCODE_ZEN_BASE_URL`
-   **Default model:** `opencode/gpt-5.4`
-   **Curated picker models:** `opencode/gpt-5.4`
-   **Setup:** Set `OPENCODE_ZEN_API_KEY` from the [OpenCode Zen console](https://opencode.ai/docs/zen/), then configure `provider = "opencode-zen"` in `vtcode.toml`
-   **Features:** Curated pay-as-you-go gateway over flagship models

## OpenCode Go

-   **Provider key:** `opencode-go`
-   **Authentication:** `OPENCODE_GO_API_KEY` environment variable
-   **Base URL:** `https://opencode.ai/zen/go/v1`, override with `OPENCODE_GO_BASE_URL`
-   **Default model:** `opencode-go/glm-5.1`
-   **Curated picker models:** `opencode-go/glm-5.1`, `opencode-go/glm-5.2`, `opencode-go/kimi-k2.7-code`, `opencode-go/kimi-k2.6`, `opencode-go/mimo-v2.5-pro`, `opencode-go/mimo-v2.5`, `opencode-go/minimax-m3`, `opencode-go/minimax-m2.7`, `opencode-go/qwen3.7-max`, `opencode-go/qwen3.7-plus`, `opencode-go/qwen3.6-plus`, `opencode-go/deepseek-v4-pro`, `opencode-go/deepseek-v4-flash`
-   **Setup:** Set `OPENCODE_GO_API_KEY` from the [OpenCode Go console](https://opencode.ai/docs/go/), then configure `provider = "opencode-go"` in `vtcode.toml`
-   **Features:** Subscription-based access to flagship open models for agentic coding

> ℹ Additional provider-specific guides will be added as new integrations land in VT Code.
