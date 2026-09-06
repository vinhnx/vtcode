# vtcode-llm
[Root AGENTS.md](../AGENTS.md) | **Canonical** LLM provider trait, types, and implementations.

## Key Modules

`provider/` trait + shared types | `providers/` per-provider impls | `providers/custom_provider.rs` custom profile router | `provider.rs` re-exports | `client.rs` + `optimized_client.rs` | `copilot/` (feature-gated) | `open_responses/` | `factory_types.rs` + `provider_config_types.rs` config | `system_prompt.rs` injection | `http_client.rs` | `types.rs` shared types | `utils.rs` + `single_response.rs` + `tool_bridge.rs` + `config_adapter.rs` + `rig_adapter.rs` + `provider_base.rs` + `error_display.rs` + `model_resolver.rs` + `usage_cost.rs` (shared normalized usage and raw/effective cost) infra (merged from core) | `process_env.rs` provider child environment policy

## Architecture Notes
- **Canonical home** for all provider code. Core's `llm/` is a thin re-export layer + factory/CGP. Merge Gateway defaults to native `/v1/responses`; an explicit `/v1/openai` base selects legacy Chat Completions. Native snapshots are normalized into incremental deltas after the first cumulative frame is confirmed, while the initial snapshot remains buffered so `fallback_restart` can discard stale pre-output state. Reasoning is route-specific: forward `reasoning_effort` only for routes advertising that control and `thinking.budget_tokens` (clamped below `max_tokens`) for thinking-budget routes, never a generic provider-wide field; preserve provider `ReasoningStage` events when converting to normalized streams so the core runtime lifecycle can render them. Collapsed-output notices use one typed `MessageClearAt` marker after tool-result user messages; only provider/model routes advertising native support use the required beta and `clear_at`, while all other builders translate the marker to ordinary system/history instructions without sending `clear_at`.
- `ModelResolver::resolve_with_mode` and `availability_with_mode` must receive the loaded config's credential storage mode; compatibility wrappers are only for callers without workspace config.
- `ResolvedModel.api_key_env` carries provider-override credential identity through availability and picker selection; do not infer availability from provider alone.
- `system_prompt.rs` provides stub getters with `OnceLock` setters; vtcode-core overrides at init.
- Uses `compact_str::CompactString` (aliased `CompactStr` from `vtcode_core::types`) for small string fields.

## Dependencies

`vtcode-commons` (HTTP, CGP, types) | `vtcode-config` (provider config, timeouts) | `vtcode-utility-tool-specs` (schemas) | `vtcode-exec-events` | `vtcode-macros` | `vtcode-safety` (child-process environment policy)

## Coding Conventions

Providers in `providers/<name>/mod.rs`. Use `anyhow::Result`, `tracing`, not `println!`. Provider-specific types stay local; shared go in `types.rs` or `provider/`. OpenResponses streaming wire parsing dispatches by `type`; keep hot SSE fields borrowed and update the wire fields and mapping together when adding events.
- Custom provider profiles match exact model IDs; explicit API formats select the wire backend without protocol fallback.
- Provider tool formatters return `Result<Option<Value>, LLMError>`; never hide serialization failures. Flattened provider extension maps must reject collisions with reserved wire fields before serialization.
## OpenAI-Compatible Providers

- `providers/openai_compat.rs` owns the shared shell: `OpenAiCompatSpec` (per-provider consts/overrides) + `OpenAiCompatCore<S>` + `impl_openai_compat_provider!`. New compat providers implement a Spec (~50-200 lines), not a full `LLMProvider`; NVIDIA also accepts arbitrary explicit IDs and maps thinking through `chat_template_kwargs`.
- Model normalization happens in `core.prepare()`, not `convert_request()` — payload tests must call `prepare` first. `stream: true` is only inserted when `request.stream` is set.
- Providers with extra protocols (evolink Anthropic path, opencode) hand-write the provider over `OpenAiCompatCore` instead of using the macro.
- Registration contract: keep the type name and 7-arg `from_config` consumed by `impl_standard_provider_constructor!` in vtcode-core. The Open Responses bridge maps authoritative plan-approval `ThreadEvent` variants to `vtcode.*` custom events for client parity. Provider error bodies are stream-read with a 16 KiB cap, then diagnostics use the bounded, secret-redacting sanitizer before metadata, display, or logs. Provider-owned child processes use the shared filtered environment, with only narrow GitHub-auth exceptions for Copilot. Copilot stdio EOF, timeout, and cancellation paths must clear pending calls; frames remain bounded.

- `ContextWindowProvider` carries discovered model capacity per provider instance; delegate every transport/capability method and scope overrides to the selected model.
