use anyhow::{Context, Result, anyhow};
use std::path::Path;
use vtcode_config::resolve_openai_auth;
use vtcode_core::config::loader::VTCodeConfig;
use vtcode_core::config::models::Provider;
use vtcode_core::config::types::AgentConfig as CoreAgentConfig;
use vtcode_core::copilot::{CopilotAuthStatusKind, probe_auth_status};
use vtcode_core::llm::factory::{ProviderConfig, create_provider_with_config};
use vtcode_core::llm::provider::LLMProvider;
use vtcode_core::llm::reasoning_effort::ReasoningEffortMapper;
use vtcode_core::llm::rig_adapter::RigProviderCapabilities;
use vtcode_core::utils::ansi::{AnsiRenderer, MessageStyle};
use vtcode_ui::tui::app::{InlineHandle, InlineHeaderContext};

use crate::agent::runloop::unified::model_switch_compaction::{
    ModelSwitchCompactionOutcome, ModelSwitchCompactionRequest, compact_on_model_switch,
};

use crate::agent::runloop::model_picker::{ModelPickerState, ModelSelectionResult};

// Re-exported so call sites can keep importing `ModelSwitchCompactionTargets`
// from this module even though its definition lives in `model_switch_compaction`.
pub(crate) use crate::agent::runloop::unified::model_switch_compaction::ModelSwitchCompactionTargets;
use crate::agent::runloop::welcome::SessionBootstrap;

use crate::agent::runloop::ui::build_inline_header_context;

fn service_tier_message_label(service_tier: Option<vtcode_config::OpenAIServiceTier>) -> &'static str {
    match service_tier {
        Some(vtcode_config::OpenAIServiceTier::Flex) => "flex",
        Some(vtcode_config::OpenAIServiceTier::Priority) => "priority",
        None => "project default",
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Intentional compatibility, platform, test, or API-shape suppression."
)]
pub(crate) async fn finalize_model_selection(
    renderer: &mut AnsiRenderer,
    picker: &ModelPickerState,
    selection: ModelSelectionResult,
    config: &mut CoreAgentConfig,
    vt_cfg: &mut Option<VTCodeConfig>,
    provider_client: &mut Box<dyn LLMProvider>,
    session_bootstrap: &SessionBootstrap,
    handle: &InlineHandle,
    header_context: &mut InlineHeaderContext,
    _full_auto: bool,
    compaction: ModelSwitchCompactionTargets<'_>,
) -> Result<()> {
    // Captured before `compaction` is moved into the compaction request below
    // (Phase E4): whether a request was already dispatched this session, used
    // to decide if a later reasoning-effort change invalidates a live cache.
    let had_prior_request = compaction.session_stats.has_sent_request();
    let prev_provider = config.provider.clone();
    let prev_model = config.model.clone();
    let workspace = config.workspace.clone();
    let auth_cfg = vt_cfg.as_ref().cloned().unwrap_or_default();
    let (api_key, openai_chatgpt_auth) = resolve_runtime_api_key(&workspace, Some(&auth_cfg), &selection).await?;
    let using_chatgpt_auth = selection.provider_enum == Some(Provider::OpenAI) && openai_chatgpt_auth.is_some();
    let custom_provider_enabled = auth_cfg.custom_provider(&selection.provider).is_some();
    let client_installed = selection.provider_enum.is_some() || custom_provider_enabled;
    let provider_name = selection.provider.clone();

    let (new_client, rig_payload, reasoning_adjustment) = if client_installed {
        let new_client = create_provider_with_config(
            &provider_name,
            ProviderConfig {
                api_key: Some(api_key.clone()),
                openai_chatgpt_auth: openai_chatgpt_auth.clone(),
                copilot_auth: Some(auth_cfg.auth.copilot.clone()),
                base_url: None,
                model: Some(selection.model.clone()),
                prompt_cache: Some(config.prompt_cache.clone()),
                timeouts: None,
                openai: Some({
                    let mut openai = auth_cfg.provider.openai.clone();
                    if selection.provider_enum == Some(Provider::OpenAI) && selection.service_tier_supported {
                        openai.service_tier = selection.service_tier;
                    }
                    openai
                }),
                anthropic: None,
                model_behavior: config.model_behavior.clone(),
                workspace_root: Some(config.workspace.clone()),
            },
        )
        .context("Failed to initialize provider for the selected model")?;
        let mapping = ReasoningEffortMapper::resolve(
            new_client.as_ref(),
            &selection.model,
            selection.reasoning,
            auth_cfg.agent.allow_reasoning_effort_downgrade,
        )
        .with_context(|| {
            format!(
                "resolve reasoning effort `{}` for provider `{}` model `{}`",
                selection.reasoning, provider_name, selection.model
            )
        })?;
        let rig_payload = selection
            .provider_enum
            .filter(|_| selection.reasoning != vtcode_core::config::types::ReasoningEffortLevel::None)
            .map(|provider| {
                let supported_efforts = new_client.supported_reasoning_efforts(&selection.model);
                RigProviderCapabilities::new(provider, &selection.model)
                    .reasoning_parameters_for_supported_efforts(mapping.effective, supported_efforts)
                    .map(|payload| payload.map(|value| value.to_string()))
            })
            .transpose()?
            .flatten();
        let reasoning_adjustment = mapping.degraded().then(|| {
            format!(
                "Requested reasoning effort `{}` was downgraded to `{}` for {} / {}.",
                mapping.requested, mapping.effective, provider_name, selection.model
            )
        });
        (Some(new_client), rig_payload, reasoning_adjustment)
    } else {
        (None, None, None)
    };

    // Persist to disk only after provider creation succeeds, so a failure
    // cannot leave vtcode.toml with a partially-updated provider config.
    let updated_cfg = picker.persist_selection(&workspace, &selection).await?;
    *vt_cfg = Some(updated_cfg);

    if let Some(new_client) = new_client {
        *provider_client = vtcode_core::llm::provider::ContextWindowProvider::wrap(
            new_client,
            &selection.model,
            selection.context_window,
        );
    } else {
        renderer.line(
            MessageStyle::Info,
            "Saved selection, but custom providers require manual configuration before taking effect.",
        )?;
    }
    config.provider = provider_name;

    config.model = selection.model.clone();
    config.api_key = api_key;
    config.reasoning_effort = selection.reasoning;
    config.api_key_env = selection.env_key.clone();
    config.openai_chatgpt_auth = openai_chatgpt_auth;
    sync_runtime_custom_api_key(config, &selection);

    if let Some(reasoning_adjustment) = reasoning_adjustment {
        renderer.line(MessageStyle::Info, &reasoning_adjustment)?;
    }

    if let Some(payload) = rig_payload {
        renderer.line(MessageStyle::Info, &format!("Rig reasoning configuration prepared: {payload}"))?;
    }

    let reasoning_label = selection.reasoning.as_str().to_string();
    let next_header_context = build_inline_header_context(
        config,
        vt_cfg.as_ref(),
        session_bootstrap,
        runtime_provider_label(&selection, using_chatgpt_auth),
        selection.model.clone(),
        provider_client.effective_context_size(&selection.model),
        reasoning_label.clone(),
    )
    .await?;
    header_context.clone_from(&next_header_context);
    handle.set_header_context(next_header_context);

    renderer.line(
        MessageStyle::Info,
        &format!("Model set to {} ({}) via {}.", selection.model_display, selection.model, selection.provider_label),
    )?;

    let compact_on_model_switch_enabled = vt_cfg
        .as_ref()
        .map(|cfg| cfg.agent.harness.compact_on_model_switch)
        .unwrap_or(true);

    let outcome = compact_on_model_switch(ModelSwitchCompactionRequest {
        prev_provider,
        prev_model,
        new_provider: selection.provider.clone(),
        new_model: selection.model.clone(),
        client_installed,
        enabled: compact_on_model_switch_enabled,
        provider: provider_client.as_ref(),
        workspace: &config.workspace,
        vt_cfg: vt_cfg.as_ref(),
        targets: compaction,
    })
    .await?;

    match outcome {
        ModelSwitchCompactionOutcome::Unchanged => {
            renderer
                .line(MessageStyle::Info, "Model selection unchanged (same model); conversation history preserved.")?;
        }
        ModelSwitchCompactionOutcome::Disabled => {
            renderer.line(
                MessageStyle::Info,
                "Model switch context compaction is disabled; conversation history preserved.",
            )?;
        }
        ModelSwitchCompactionOutcome::SkippedNoClient => {
            renderer.line(
                MessageStyle::Info,
                "Model switched, but the provider client is not configured; conversation history preserved.",
            )?;
        }
        ModelSwitchCompactionOutcome::LineageCleared => {
            renderer.line(
                MessageStyle::Info,
                "Model switched; previous response lineage cleared (no conversation history to compact).",
            )?;
        }
        ModelSwitchCompactionOutcome::Compacted(outcome) => {
            renderer.line(
                MessageStyle::Info,
                &format!(
                    "Compacted conversation for model switch ({} -> {} messages, {} compaction).",
                    outcome.original_len,
                    outcome.compacted_len,
                    outcome.mode.as_str()
                ),
            )?;
        }
        ModelSwitchCompactionOutcome::AlreadyCompact => {
            renderer.line(MessageStyle::Info, "Model switched; conversation was already compact.")?;
        }
        ModelSwitchCompactionOutcome::Failed(err) => {
            renderer.line(
                MessageStyle::Error,
                &format!("Model switched, but context compaction failed: {err}. Continuing with full history."),
            )?;
        }
    }

    if !selection.known_model {
        renderer.line(
            MessageStyle::Info,
            "The selected model is not part of VT Code's curated list; capabilities may vary.",
        )?;
    }

    if selection.reasoning_supported {
        let message = if selection.reasoning_changed {
            format!("Reasoning effort updated to '{}'.", selection.reasoning)
        } else {
            format!("Reasoning effort remains '{}'.", selection.reasoning)
        };
        renderer.line(MessageStyle::Info, &message)?;

        if selection.reasoning_changed
            && had_prior_request
            && vt_cfg
                .as_ref()
                .is_some_and(|cfg| cfg.prompt_cache.is_provider_enabled(&config.provider))
        {
            renderer.line(
                MessageStyle::Info,
                "This changes the request prefix, so the provider prompt cache will be invalidated; the next request re-pays full input cost.",
            )?;
        }
    }

    if selection.service_tier_supported {
        let message = if selection.service_tier_changed {
            format!("Service tier updated to '{}'.", service_tier_message_label(selection.service_tier))
        } else {
            format!("Service tier remains '{}'.", service_tier_message_label(selection.service_tier))
        };
        renderer.line(MessageStyle::Info, &message)?;
    }

    if using_chatgpt_auth {
        renderer.line(MessageStyle::Info, "Using ChatGPT subscription for OpenAI.")?;
    } else if selection.provider_enum == Some(Provider::Copilot) {
        renderer.line(MessageStyle::Info, "Using GitHub Copilot managed authentication.")?;
    } else if selection.provider_enum == Some(Provider::MiMo) {
        if let Some(method) = selection.mimo_auth_method {
            renderer.line(MessageStyle::Info, &format!("Using MiMo {} authentication.", method.label()))?;
        }
    } else if selection.api_key.is_some() && selection.credential_source.is_none() {
        renderer.line(
            MessageStyle::Info,
            &format!(
                "API key saved to secure storage. The key will not be written to {} or vtcode.toml.",
                selection.env_key,
            ),
        )?;
    } else if selection.credential_source == Some(vtcode_config::api_keys::CredentialSource::SecureStorage) {
        renderer.line(MessageStyle::Info, "Using API key from secure storage.")?;
    } else if selection.credential_source == Some(vtcode_config::api_keys::CredentialSource::Workspace) {
        renderer.line(
            MessageStyle::Info,
            &format!("Using workspace environment variable {} for authentication.", selection.env_key),
        )?;
    } else if selection.requires_api_key {
        renderer.line(
            MessageStyle::Info,
            &format!("Using environment variable {} for authentication.", selection.env_key),
        )?;
    }

    Ok(())
}

async fn resolve_runtime_api_key(
    workspace: &Path,
    vt_cfg: Option<&VTCodeConfig>,
    selection: &ModelSelectionResult,
) -> Result<(String, Option<vtcode_config::auth::OpenAIChatGptAuthHandle>)> {
    if selection.credential_source.is_none()
        && let Some(key) = selection.api_key.as_ref()
    {
        tracing::debug!(
            "resolve_runtime_api_key: using pending_api_key from selection result for provider '{}'",
            selection.provider
        );
        return Ok((key.clone(), None));
    }

    if selection.provider_enum == Some(Provider::Copilot) {
        let Some(cfg) = vt_cfg else {
            return Err(anyhow!("GitHub Copilot configuration is unavailable. Run `vtcode login copilot`."));
        };
        let status = probe_auth_status(&cfg.auth.copilot, Some(workspace)).await;
        return match status.kind {
            CopilotAuthStatusKind::Authenticated => Ok((String::new(), None)),
            CopilotAuthStatusKind::Unauthenticated | CopilotAuthStatusKind::AuthFlowFailed => {
                Err(anyhow!(status.message.unwrap_or_else(|| {
                    "GitHub Copilot is not authenticated. Run `vtcode login copilot`."
                        .to_string()
                })))
            }
            CopilotAuthStatusKind::ServerUnavailable => Err(anyhow!(
                status.message.unwrap_or_else(|| {
                    "GitHub Copilot CLI is unavailable. Install `copilot`, set `VTCODE_COPILOT_COMMAND`, or configure `[auth.copilot].command`."
                        .to_string()
                })
            )),
        };
    }

    if selection.provider_enum.is_none()
        && let Some(cp) = vt_cfg.and_then(|cfg| cfg.custom_provider(&selection.provider))
        && cp.uses_command_auth()
    {
        return Ok((String::new(), None));
    }

    // A provider without a built-in enum must come from the loaded custom
    // provider configuration. Do not probe secure storage for an arbitrary
    // unconfigured name; report the actionable key hint instead.
    if selection.provider_enum.is_none() && vt_cfg.and_then(|cfg| cfg.custom_provider(&selection.provider)).is_none() {
        if selection.requires_api_key {
            return Err(anyhow!(
                "API key not found for provider '{}'. Set {} or enter a key to continue.",
                selection.provider,
                selection.env_key
            ));
        }
        return Ok((String::new(), None));
    }

    let storage_mode = vt_cfg.map(|cfg| cfg.agent.credential_storage_mode).unwrap_or_default();
    let resolved = vtcode_config::api_keys::resolve_credential_with_mode(
        &selection.provider,
        &selection.env_key,
        Some(workspace),
        storage_mode,
    )?;

    if selection.provider_enum == Some(Provider::OpenAI)
        && selection.env_key.eq_ignore_ascii_case(Provider::OpenAI.default_api_key_env())
        && let Some(cfg) = vt_cfg
    {
        let api_key = match resolved.as_ref().and_then(|credential| credential.secret.clone()) {
            Some(api_key) => Some(api_key),
            None if cfg.auth.openai.preferred_method == vtcode_config::OpenAIPreferredMethod::ApiKey => {
                vtcode_config::api_keys::load_stored_api_key_with_mode("openai", storage_mode)?
            }
            None => None,
        };
        let auth = resolve_openai_auth(&cfg.auth.openai, storage_mode, api_key)?;
        return Ok((auth.api_key().to_string(), auth.handle()));
    }

    if let Some(credential) = resolved
        && let Some(secret) = credential.secret
    {
        tracing::debug!(
            "resolve_runtime_api_key: resolved credential for provider '{}' from {:?} (len={})",
            selection.provider,
            credential.source,
            secret.len()
        );
        return Ok((secret, None));
    }

    if selection.requires_api_key {
        return Err(anyhow!(
            "API key not found for provider '{}'. Set {} or enter a key to continue.",
            selection.provider,
            selection.env_key
        ));
    }

    Ok((String::new(), None))
}

#[cfg(test)]
fn read_workspace_api_key(workspace: &Path, env_key: &str) -> Result<Option<String>> {
    vtcode_config::read_workspace_env_value(workspace, env_key)
        .with_context(|| format!("Failed to read environment variable {env_key}"))
}

fn sync_runtime_custom_api_key(config: &mut CoreAgentConfig, selection: &ModelSelectionResult) {
    if selection.provider_enum == Some(Provider::OpenAI) && selection.uses_chatgpt_auth {
        return;
    }

    if selection.api_key.is_some()
        && let Ok(Some(metadata_key)) =
            vtcode_config::api_keys::credential_metadata_key(&selection.provider, &selection.env_key)
    {
        config.custom_api_keys.insert(metadata_key, String::new());
        return;
    }

    config.custom_api_keys.remove(&selection.provider);
    if let Ok(Some(metadata_key)) =
        vtcode_config::api_keys::credential_metadata_key(&selection.provider, &selection.env_key)
    {
        config.custom_api_keys.remove(&metadata_key);
    }
}

fn runtime_provider_label(selection: &ModelSelectionResult, using_chatgpt_auth: bool) -> String {
    if selection.provider_enum == Some(Provider::OpenAI) && using_chatgpt_auth {
        "OpenAI (ChatGPT)".to_string()
    } else if selection.provider_enum == Some(Provider::MiMo) {
        if let Some(method) = selection.mimo_auth_method {
            format!("{} ({})", "Xiaomi MiMo", method.label())
        } else {
            selection.provider_label.clone()
        }
    } else {
        selection.provider_label.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::{read_workspace_api_key, resolve_runtime_api_key};
    use crate::agent::runloop::model_picker::ModelSelectionResult;
    use tempfile::tempdir;
    use vtcode_config::VTCodeConfig;
    use vtcode_core::config::models::Provider;
    use vtcode_core::config::types::ReasoningEffortLevel;

    fn selection(
        provider: &str,
        provider_enum: Option<Provider>,
        env_key: &str,
        api_key: Option<&str>,
        requires_api_key: bool,
    ) -> ModelSelectionResult {
        ModelSelectionResult {
            provider: provider.to_string(),
            provider_label: provider.to_string(),
            provider_enum,
            model: "test-model".to_string(),
            model_display: "test-model".to_string(),
            known_model: false,
            context_window: None,
            reasoning_supported: false,
            reasoning: ReasoningEffortLevel::Medium,
            reasoning_changed: false,
            service_tier_supported: false,
            service_tier: None,
            service_tier_changed: false,
            api_key: api_key.map(ToString::to_string),
            credential_source: None,
            env_key: env_key.to_string(),
            requires_api_key,
            uses_chatgpt_auth: false,
            mimo_auth_method: None,
        }
    }

    #[test]
    fn resolve_runtime_api_key_prefers_workspace_env_file() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join(".env"), "OPENAI_API_KEY=workspace-key\n").expect("workspace env");
        let selection = selection("openai", Some(Provider::OpenAI), "OPENAI_API_KEY", None, true);

        let resolved = tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(resolve_runtime_api_key(dir.path(), None, &selection))
            .expect("workspace env should resolve");

        assert_eq!(resolved.0, "workspace-key");
    }

    #[test]
    fn resolve_runtime_api_key_does_not_write_user_supplied_key_to_workspace_env() {
        let dir = tempdir().expect("temp dir");
        let selection = selection("openai", Some(Provider::OpenAI), "OPENAI_API_KEY", Some("user-key"), true);

        let resolved = tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(resolve_runtime_api_key(dir.path(), None, &selection))
            .expect("user key should resolve");
        let written = read_workspace_api_key(dir.path(), "OPENAI_API_KEY").expect("workspace env read");

        assert_eq!(resolved.0, "user-key");
        assert_eq!(written, None);
    }

    #[test]
    fn resolve_runtime_api_key_errors_for_missing_custom_provider_key() {
        let dir = tempdir().expect("temp dir");
        let selection = selection("custom", None, "CUSTOM_API_KEY", None, true);

        let err = tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(resolve_runtime_api_key(dir.path(), None, &selection))
            .expect_err("missing custom provider key should fail");

        assert!(err.to_string().contains("CUSTOM_API_KEY"));
    }

    #[test]
    fn resolve_runtime_api_key_accepts_custom_provider_command_auth_without_key() {
        let dir = tempdir().expect("temp dir");
        let mut config = VTCodeConfig::default();
        config.custom_providers.push(vtcode_config::core::CustomProviderConfig {
            name: "mycorp".to_string(),
            display_name: "MyCorp".to_string(),
            base_url: "https://llm.example/v1".to_string(),
            context_window: None,
            api_key_env: String::new(),
            auth: Some(vtcode_config::core::CustomProviderCommandAuthConfig {
                command: "print-token".to_string(),
                args: Vec::new(),
                cwd: None,
                timeout_ms: 1_000,
                refresh_interval_ms: 60_000,
            }),
            model: "gpt-5-mini".to_string(),
            models: Vec::new(),
            ..vtcode_config::core::CustomProviderConfig::default()
        });
        let selection = selection("mycorp", None, "", None, false);

        let resolved = tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(resolve_runtime_api_key(dir.path(), Some(&config), &selection))
            .expect("command-auth custom provider should not require a static key");

        assert!(resolved.0.is_empty());
        assert!(resolved.1.is_none());
    }
}
