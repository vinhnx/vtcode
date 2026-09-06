# Responses API & Reasoning Models

VT Code routes OpenAI Responses models, including the GPT-5 family plus `o3` and `o4-mini`, through the Responses API. This guide focuses on the parts that matter in VT Code: reasoning continuity across tool calls, cache-friendly request shaping, encrypted reasoning for stateless workflows, and the config needed to turn those features on.

VT Code's default OpenAI profile keeps `gpt-5.5` on a compact execution contract: concise structured outputs, outcome-first follow-through by default, dependency-aware tool use, completeness checks, minimal validation loops, and grounding/citation rules that only activate when the task is research or citation sensitive.

## Key Concepts

| Concept                 | Description                                                                                                                                               |
| ----------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Reasoning items**     | Internal chain-of-thought tokens exposed as IDs in the Responses API output. Reusing them keeps tool-enabled turns coherent and helps downstream caching. |
| **Reasoning summaries** | Short, user-visible explanations of what the model computed. VT Code requests summaries automatically for OpenAI Responses reasoning models and folds returned text into normal reasoning output. |
| **Encrypted reasoning** | A stateless, compliance-friendly variant where the API returns encrypted tokens that your sidecar can return verbatim without persisting data.            |

## VT Code configuration guidance

1. **Choose reasoning effort by task shape**: Set `reasoning_effort` inside `vtcode.toml` based on the task, not by defaulting to the highest setting. For execution-heavy and latency-sensitive work, `none` or `low` is usually enough; `medium` or `high` is better for research-heavy or conflict-resolution work; `xhigh` should stay reserved for long-horizon agentic tasks where evals justify the extra cost and latency.

    ```toml
    reasoning_effort = "none"
    ```

2. **Surface reasoning summaries**: VT Code automatically requests `reasoning.summary = "auto"` for OpenAI reasoning models. Returned summary text is folded into the agent’s normal reasoning output and logs, so no extra toggle is required.

3. **Preserve reasoning items across API calls**: VT Code keeps continuity in two ways. It stores `previous_response_id` for OpenAI, OpenAI-compatible Responses sessions, and OpenResponses sessions, and it also preserves structured reasoning items in assistant `reasoning_details` so tool loops can replay them when the next request is built. That matches OpenAI’s guidance to pass `previous_response_id` or reinsert reasoning items explicitly.

4. **Use hybrid continuity + server-side compaction**: VT Code keeps Responses-style continuity (`previous_response_id`) for OpenAI, OpenAI-compatible Responses providers, and OpenResponses providers, and enables compaction via `context_management` on `/responses` requests when `agent.harness.auto_compaction_enabled = true`. This matches OpenAI's recommended stateful path: when you are already chaining with `previous_response_id`, let the API manage context compaction instead of manually pruning request input.

5. **Use encrypted reasoning for ZDR-style compliance**: If you are restricted from storing model state, enable the Responses API flags directly in `vtcode.toml`:

    ```toml
    [provider.openai]
    responses_store = false
    responses_include = ["reasoning.encrypted_content"]
    ```

    The Responses API will return encrypted reasoning state inside each reasoning item, and VT Code will pass that state back on the next OpenAI Responses request. No raw reasoning needs to be persisted locally to preserve continuity.

6. **Cache-friendly prompts and continuity**: The Responses API differentiates cached and uncached tokens. Longer prompts (>= 1,024 tokens) benefit from returning everything, including reasoning items, so the cache can match on both the request and internal context. Higher cache hit ratios reduce costs and latency for GPT-5-family models, especially during long-running agent loops.

    Tip: VT Code sends a stable OpenAI routing key per conversation by default via `prompt_cache_key_mode = "session"` under `[prompt_cache.providers.openai]`. Keep this at `session` for better cache locality; set `off` only when you explicitly want to disable key-based routing. You can also instruct the Responses API to retain cached prefixes for longer by setting `prompt_cache_retention` on the request. VT Code exposes this setting as `# prompt_cache_retention = "24h"` (commented out by default). The public OpenAI contract currently accepts only `in_memory` and `24h`; leaving the setting unset preserves the default in-memory policy.

    Example: Enable 24h retention using CLI config overrides for a Responses model:

    ```bash
    vtcode --model gpt-5 --config prompt_cache.providers.openai.prompt_cache_retention=24h ask "Explain this function"
    ```

    To list the models known to support the OpenAI Responses API, run:

    ```bash
    vtcode models list --provider openai
    ```

7. **Function calling etiquette**: Ensure any VT Code tool definitions expose their JSON schema via the `function` payload. The Responses API requires each tool message to include a `tool_call_id`, and VT Code already handles this when serializing `ToolDefinition`s.

8. **OpenAI-only non-image file inputs**: VT Code upgrades local non-image file refs such as `@report.pdf` and `@"Quarterly Deck.pptx"` into structured file attachments only for native OpenAI Responses sessions on `api.openai.com`. Remote external document URLs such as `@https://example.com/letter.pdf` are elevated to structured `file_url` inputs on that same path only. ChatGPT subscription sessions, OpenAI-compatible endpoints, and other providers keep non-image `@file` refs as plain text plus file-reference metadata so the agent can resolve the path and read it with tools.

9. **Assistant phase continuity**: VT Code preserves assistant phase metadata on official OpenAI Responses replays, including native `api.openai.com` requests and ChatGPT-backed manual history replays, when the target GPT model supports it. Interim preambles and progress updates are sent as `commentary`; completed answers are sent as `final_answer`. The field is omitted for Chat Completions, tool/user items, and non-native OpenAI-compatible endpoints.

10. **Reasoning visibility**: When troubleshooting, inspect `.vtcode/logs/trajectory.jsonl` for `reasoning` entries and correlate them with the configured `reasoning_effort`.

11. **Auto-compaction settings**: Auto compaction is disabled by default. Turn it on explicitly when you want long-session coherence via Responses `context_management`:

    ```toml
    [agent.harness]
    auto_compaction_enabled = true
    # Optional lower trigger; otherwise model/session capacity minus output reserve.
    auto_compaction_threshold_tokens = 200000
    ```

    VT Code applies provider-native server-side compaction on compatible Responses providers/endpoints. On providers without native compaction, the same threshold is reused for VT Code's local fallback summarization path.

12. **Manual `/compact` uses the provider-native endpoint when possible**: VT Code's `/compact` command calls the Responses `/responses/compact` endpoint for compatible providers and keeps the returned canonical output structure as conversation history, including opaque `compaction` items. For providers without native support, VT Code falls back to local summarization.

13. **OpenAI WebSocket mode stays opt-in and applies to OpenAI-compatible non-streaming Responses turns**: When `[provider.openai].websocket_mode = true`, VT Code uses the `/v1/responses` WebSocket transport for non-streaming Responses requests on native `api.openai.com` and configured OpenAI-compatible Responses endpoints. ChatGPT-backed sessions stay on the HTTP path. The transport keeps a reusable in-memory continuation cache per provider instance, sends only incremental `input` when the next turn is a verified prefix extension, and otherwise starts a new chain with the full input window.

14. **Warmup is optional and VT Code only uses it for brand-new WebSocket chains**: VT Code no longer warms every fresh socket automatically. It sends `generate = false` only when a request is starting a brand-new WebSocket chain and there is no reusable continuation cache to chain from. The next generated turn then continues from that warmup response ID on the same socket.

15. **WebSocket recovery follows the current Responses contract**: If the socket closes or a failed turn forces VT Code onto a fresh socket, it reuses cached continuation state only when that response can survive a socket replacement. Otherwise it starts a new WebSocket chain immediately. If the server returns `previous_response_not_found`, VT Code clears the cached continuation, opens a new chain on WebSocket, and resends the full input window instead of silently attempting another stale continuation.

## Example workflow

1. VT Code sends a Responses API request for an OpenAI reasoning model with tools serialized through the shared helper.
2. The response includes tool call instructions plus a reasoning item. VT Code records the response id and preserves any structured reasoning items in message history.
3. Tool outputs are emitted as `function_call_output` messages with the original `tool_call_id`, then the next request is issued with the preserved continuity state.
4. If encrypted reasoning is enabled, the returned `encrypted_content` is replayed automatically on the next OpenAI Responses request.

## Taking it further

### Capability mapping across providers

The harness validates the configured effort against the active provider route's
supported levels before sending a request. Unsupported effort blocks the turn
with a `TurnBlocked` diagnostic; missing capability metadata does not imply support.
`none` omits the effort control, while unknown values block rather than silently
selecting a provider default. These rules apply to interactive and headless runs.

Set `agent.allow_reasoning_effort_downgrade = true` to explicitly permit the nearest
lower supported level, such as Max → XHigh → High. Each downgrade logs the requested
and effective values. Upward coercion is never performed. The effective level also
participates in the prompt-cache identity. Native provider serializers preserve
the mapped value; catalog entries and provider profile overrides define support.

-   Combine reasoning summary output with VT Code’s status line badge and telemetry for interactive tracing.
-   Use reasoning effort tiers together with the status line’s `runtime.reasoning_effort` so your shell hook can show when the agent is "thinking" harder.
-   Keep `.vtcode/logs/trajectory.jsonl` for post-run analysis and to debug why a tool call required an extra turn.

Following these practices keeps VT Code aligned with current OpenAI Responses guidance, delivering better continuity, lower-cost cached prompts, and stronger long-horizon agent behavior.
