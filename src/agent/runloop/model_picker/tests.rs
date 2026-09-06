use super::*;
use crate::agent::runloop::unified::state::CtrlCState;
use anyhow::Result;
use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;
use tempfile::tempdir;
use tokio::sync::{Notify, mpsc};
use vtcode_config::OpenAIServiceTier;
use vtcode_config::auth::AuthCredentialsStoreMode;
use vtcode_config::core::ProviderOverrideConfig;
use vtcode_config::core::{CustomProviderApiFormat, CustomProviderConfig, CustomProviderProfileConfig};
use vtcode_config::loader::VTCodeConfig;
use vtcode_core::config::models::ModelId;
use vtcode_core::utils::ansi::AnsiRenderer;
use vtcode_ui::tui::app::{InlineHandle, InlineSession};

use self::options::{MODEL_OPTIONS, build_filtered_options, find_option_index, option_indexes_for_provider};

fn has_model(options: &[ModelOption], model: ModelId) -> bool {
    let id = model.as_str();
    let provider = model.provider();
    options.iter().any(|option| option.id == id && option.provider == provider)
}

#[test]
fn model_picker_lists_new_anthropic_models() {
    let options = MODEL_OPTIONS.as_slice();
    assert!(has_model(options, ModelId::ClaudeOpus5));
    assert!(has_model(options, ModelId::ClaudeOpus5));
    assert!(has_model(options, ModelId::ClaudeSonnet5));
    assert!(has_model(options, ModelId::ClaudeSonnet5));

    // OpenRouter variants
    assert!(has_model(options, ModelId::OpenRouterAnthropicClaudeSonnet5));
    assert!(has_model(options, ModelId::OpenRouterAnthropicClaudeSonnet5));
}

#[test]
fn model_picker_lists_new_zai_models() {
    let options = MODEL_OPTIONS.as_slice();
    assert!(has_model(options, ModelId::ZaiGlm52));
}

#[test]
fn model_picker_lists_new_ollama_cloud_models() {
    let options = MODEL_OPTIONS.as_slice();
    assert!(has_model(options, ModelId::OllamaGptOss20b));
    assert!(has_model(options, ModelId::OllamaGptOss120bCloud));
    assert!(has_model(options, ModelId::OllamaDeepseekV4FlashCloud));
    assert!(has_model(options, ModelId::OllamaDeepseekV4ProCloud));
    assert!(has_model(options, ModelId::OllamaGlm52Cloud));
    assert!(has_model(options, ModelId::OllamaMinimaxM3Cloud));
}

#[test]
fn model_picker_lists_new_gemini_models() {
    let options = MODEL_OPTIONS.as_slice();
    assert!(has_model(options, ModelId::Gemini37Flash));
}

#[test]
fn model_picker_lists_new_openai_codex_models() {
    let options = MODEL_OPTIONS.as_slice();
    assert!(has_model(options, ModelId::GPT56Sol));
}

#[test]
fn subagent_model_shortcuts_include_expected_aliases() {
    let shortcuts = subagent_model_shortcuts()
        .iter()
        .map(|(shortcut, _)| *shortcut)
        .collect::<Vec<_>>();

    assert_eq!(shortcuts, vec!["inherit", "small", "haiku", "sonnet", "opus"]);
}

#[test]
fn subagent_dynamic_model_filter_keeps_only_parseable_model_ids() {
    let registry = DynamicModelRegistry {
        entries: vec![
            selection::selection_from_dynamic(Provider::OpenAI, "gpt-5.6-sol", "gpt-5.6-sol", None, None),
            selection::selection_from_dynamic(Provider::Ollama, "custom-local-model", "custom-local-model", None, None),
        ],
        ..Default::default()
    };

    let indexes = parseable_subagent_dynamic_indexes(&registry);
    assert_eq!(indexes, vec![0]);
}

#[test]
fn subagent_reasoning_levels_only_enable_xhigh_when_supported() {
    let supported = subagent_reasoning_levels("gpt-5.6-sol", true);
    assert!(supported.contains(&ReasoningEffortLevel::XHigh));
    // The GPT-5.6 family also supports Max adaptive reasoning.
    assert!(supported.contains(&ReasoningEffortLevel::Max));

    let sonnet = subagent_reasoning_levels("claude-sonnet-5", true);
    // Claude Sonnet 5 supports both XHigh and Max reasoning.
    assert!(sonnet.contains(&ReasoningEffortLevel::XHigh));
    assert!(sonnet.contains(&ReasoningEffortLevel::Max));

    let shortcut = subagent_reasoning_levels("haiku", true);
    assert!(!shortcut.contains(&ReasoningEffortLevel::XHigh));
    assert!(!shortcut.contains(&ReasoningEffortLevel::Max));

    let unsupported = subagent_reasoning_levels("gpt-4.1", true);
    assert!(!unsupported.contains(&ReasoningEffortLevel::XHigh));
    assert!(!unsupported.contains(&ReasoningEffortLevel::Max));
}

#[test]
fn subagent_reasoning_normalization_drops_invalid_or_unsupported_values() {
    let shortcut = SubagentModelTarget::Shortcut { model: "Haiku".to_string() };
    assert_eq!(normalized_subagent_reasoning(&shortcut, Some("high")), Some(ReasoningEffortLevel::High));
    assert_eq!(normalized_subagent_reasoning(&shortcut, Some("xhigh")), None);
    assert_eq!(normalized_subagent_reasoning(&shortcut, Some("max")), None);
    assert_eq!(normalized_subagent_reasoning(&shortcut, Some("bogus")), None);

    let concrete = SubagentModelTarget::Concrete(selection::selection_from_dynamic(
        Provider::OpenAI,
        "gpt-5.6-sol",
        "gpt-5.6-sol",
        None,
        None,
    ));
    assert_eq!(normalized_subagent_reasoning(&concrete, Some("xhigh")), Some(ReasoningEffortLevel::XHigh));

    let sonnet = SubagentModelTarget::Concrete(selection::selection_from_dynamic(
        Provider::Anthropic,
        "claude-sonnet-5",
        "claude-sonnet-5",
        None,
        None,
    ));
    assert_eq!(normalized_subagent_reasoning(&sonnet, Some("max")), Some(ReasoningEffortLevel::Max));
}

#[test]
fn preferred_subagent_model_selection_canonicalizes_shortcuts() {
    let registry = DynamicModelRegistry::default();
    let selection = preferred_subagent_model_selection(&MODEL_OPTIONS, &registry, "HaIkU");

    assert_eq!(selection, Some(InlineListSelection::ConfigAction("subagent-model:shortcut:haiku".to_string())));
}

#[test]
fn model_search_value_includes_provider_model_aliases() {
    let extra_terms = vec!["reasoning".to_string(), "tools".to_string(), "image".to_string()];
    let value =
        model_search_value(Provider::OpenAI, "GPT-5.4", "gpt-5.6-sol", Some("Latest frontier model"), &extra_terms)
            .to_ascii_lowercase();

    assert!(value.contains("openai gpt-5.4"));
    assert!(value.contains("openai/gpt-5.6-sol"));
    assert!(value.contains("reasoning"));
    assert!(value.contains("tools"));
    assert!(value.contains("image"));
}

#[test]
fn parse_model_selection_uses_custom_provider_display_and_env_key() {
    let mut cfg = VTCodeConfig::default();
    cfg.custom_providers.push(CustomProviderConfig {
        name: "mycorp".to_string(),
        display_name: "MyCorporateName".to_string(),
        base_url: "https://llm.corp.example/v1".to_string(),
        context_window: None,
        api_key_env: "MYCORP_API_KEY".to_string(),
        auth: None,
        model: "gpt-5-mini".to_string(),
        models: Vec::new(),
        ..CustomProviderConfig::default()
    });

    let detail =
        parse_model_selection(&MODEL_OPTIONS, "mycorp gpt-5-mini", Some(&cfg)).expect("custom provider should parse");

    assert_eq!(detail.provider_key, "mycorp");
    assert_eq!(detail.provider_label, "MyCorporateName");
    assert_eq!(detail.env_key, "MYCORP_API_KEY");
    assert_eq!(detail.provider_enum, None);
}

#[test]
fn custom_provider_picker_uses_exact_profile_metadata() {
    let mut cfg = VTCodeConfig::default();
    let mut profiles = BTreeMap::new();
    profiles.insert(
        "gpt-5-mini".to_string(),
        CustomProviderProfileConfig {
            api_format: CustomProviderApiFormat::OpenAIResponses,
            context_window: Some(256_000),
            supports_reasoning: Some(true),
            ..CustomProviderProfileConfig::default()
        },
    );
    cfg.custom_providers.push(CustomProviderConfig {
        name: "mycorp".to_string(),
        display_name: "MyCorporateName".to_string(),
        base_url: "https://llm.corp.example/v1".to_string(),
        api_key_env: "MYCORP_API_KEY".to_string(),
        model: "gpt-5-mini".to_string(),
        profiles,
        ..CustomProviderConfig::default()
    });

    let detail =
        parse_model_selection(&MODEL_OPTIONS, "mycorp gpt-5-mini", Some(&cfg)).expect("custom provider should parse");

    assert_eq!(detail.context_window, Some(256_000));
    assert!(detail.reasoning_supported);
}

#[test]
fn custom_provider_picker_shows_each_model_with_provider_identity() {
    let config = CustomProviderConfig {
        name: "mycorp".to_string(),
        display_name: "MyCorporateName".to_string(),
        base_url: "https://llm.corp.example/v1".to_string(),
        api_key_env: "MYCORP_API_KEY".to_string(),
        model: "gpt-5-mini".to_string(),
        models: vec!["gpt-5-mini", "reasoning-model"].into_iter().map(String::from).collect(),
        ..CustomProviderConfig::default()
    };
    let selections = selections_from_custom_provider(&config);

    assert_eq!(selections.len(), 2);
    assert_eq!(rendering::custom_provider_picker_title(&selections[0]), "gpt-5-mini");
    assert_eq!(rendering::custom_provider_picker_title(&selections[1]), "reasoning-model");
    assert_eq!(rendering::custom_provider_picker_subtitle(&selections[0], "", ""), "MyCorporateName");
    assert_eq!(rendering::custom_provider_picker_subtitle(&selections[1], "", ""), "MyCorporateName");
    assert_eq!(interaction::custom_provider_description(&selections[0]), "MyCorporateName • gpt-5-mini");
    assert_eq!(interaction::custom_provider_description(&selections[1]), "MyCorporateName • reasoning-model");
}

#[test]
fn custom_provider_picker_keeps_single_model_field() {
    let config = CustomProviderConfig {
        name: "mycorp".to_string(),
        display_name: "MyCorporateName".to_string(),
        model: "gpt-5-mini".to_string(),
        ..CustomProviderConfig::default()
    };
    let selections = selections_from_custom_provider(&config);

    assert_eq!(selections.len(), 1);
    assert_eq!(rendering::custom_provider_picker_title(&selections[0]), "gpt-5-mini");
}

#[test]
fn parse_model_selection_marks_command_auth_custom_provider_as_keyless() {
    let mut cfg = VTCodeConfig::default();
    cfg.custom_providers.push(CustomProviderConfig {
        name: "mycorp".to_string(),
        display_name: "MyCorporateName".to_string(),
        base_url: "https://llm.corp.example/v1".to_string(),
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
        ..CustomProviderConfig::default()
    });

    let detail =
        parse_model_selection(&MODEL_OPTIONS, "mycorp gpt-5-mini", Some(&cfg)).expect("custom provider should parse");

    assert!(!detail.requires_api_key);
    assert!(detail.env_key.is_empty());
}

#[test]
fn provider_override_key_name_reaches_picker_selection() {
    let mut overrides = BTreeMap::new();
    overrides.insert(
        "openai".to_string(),
        ProviderOverrideConfig {
            models: vec!["gpt-5.6-sol".to_string()],
            base_url: None,
            api_key_env: Some("CORPORATE_OPENAI_KEY".to_string()),
        },
    );

    let options = options::build_model_options_with_overrides(&overrides);
    let option = options
        .iter()
        .find(|option| option.provider == Provider::OpenAI && option.id == "gpt-5.6-sol")
        .expect("overridden OpenAI model should be listed");
    let detail = selection::selection_from_option_with_mode(option, AuthCredentialsStoreMode::File);

    assert_eq!(option.api_key_env, "CORPORATE_OPENAI_KEY");
    assert_eq!(detail.env_key, "CORPORATE_OPENAI_KEY");
}

#[test]
fn picker_reuses_persisted_key_name_for_active_provider() {
    let mut cfg = VTCodeConfig::default();
    cfg.agent.provider = "openai".to_string();
    cfg.agent.api_key_env = "CORPORATE_OPENAI_KEY".to_string();

    let options = build_filtered_options(Some(&cfg));
    let option = options
        .iter()
        .find(|option| option.provider == Provider::OpenAI && option.id == "gpt-5.6-sol")
        .expect("OpenAI model should be listed");

    assert_eq!(option.api_key_env, "CORPORATE_OPENAI_KEY");
}

#[test]
fn static_model_subtitle_formats_current_capabilities() {
    let option = MODEL_OPTIONS
        .iter()
        .find(|option| option.model == ModelId::GPT56Sol)
        .expect("gpt-5.4 option should exist");

    let subtitle = static_model_subtitle(option, "openai", "gpt-5.6-sol");

    assert_eq!(subtitle, Some("Current • 1M • Reasoning • Tools • image".to_string()));
}

#[test]
fn static_model_search_terms_include_modalities_and_tool_state() {
    let terms = static_model_search_terms(&ModelId::OpenRouterOpenAIGpt5Chat, false);

    assert!(terms.iter().any(|term| term == "no tools"));
    assert!(terms.iter().any(|term| term == "no-tools"));
    assert!(terms.iter().any(|term| term == "tool_call disabled"));
    assert!(terms.iter().any(|term| term == "modalities"));
    assert!(terms.iter().any(|term| term == "file"));
    assert!(terms.iter().any(|term| term == "image"));
    assert!(terms.iter().any(|term| term == "text"));
}

#[test]
fn dynamic_model_subtitle_stays_conservative_for_unknown_local_models() {
    let subtitle =
        dynamic_model_subtitle(Provider::Ollama, "custom-local-model", false, "ollama", "custom-local-model");

    assert_eq!(subtitle, Some("Current • Local".to_string()));
}

#[test]
fn current_model_line_shows_effective_anthropic_context_window() {
    let line = rendering::current_model_line("anthropic", "claude-sonnet-5");
    assert_eq!(line, "Current: anthropic / claude-sonnet-5 • 1M");
}

#[test]
fn step_one_header_lines_explain_codex_runtime_configuration() {
    let lines = rendering::step_one_header_lines("codex", "gpt-5-codex");

    assert!(
        lines.iter().any(|line| line.contains("/config codex")),
        "expected Codex runtime note in picker header"
    );
    assert!(lines.iter().any(|line| line.contains("/model")), "expected note to clarify /model scope");
}

fn base_picker_state(current_provider: &str, current_model: &str) -> ModelPickerState {
    ModelPickerState {
        settings: PickerSettings {
            options: Cow::Borrowed(MODEL_OPTIONS.as_slice()),
            inline_enabled: true,
            vt_cfg: None,
            workspace: None,
            ctrl_c_state: None,
            ctrl_c_notify: None,
            provider_order: picker_provider_order().to_vec(),
            current_reasoning: ReasoningEffortLevel::Medium,
            current_service_tier: None,
            current_provider: current_provider.to_string(),
            current_model: current_model.to_string(),
        },
        step: PickerStep::AwaitModel,
        selection: None,
        custom_providers: Vec::new(),
        dynamic_models: DynamicModelRegistry::default(),
        selected_reasoning: None,
        selected_service_tier: None,
        selected_mimo_auth: None,
        pending_api_key: None,
        pending_credential_source: None,
        plain_mode_active: false,
    }
}

fn session_with_channels() -> (InlineHandle, InlineSession) {
    let (command_tx, _command_rx) = mpsc::unbounded_channel();
    let (_event_tx, event_rx) = mpsc::unbounded_channel();
    let handle = InlineHandle::new_for_tests(command_tx);
    let session = InlineSession {
        handle: handle.clone(),
        events: event_rx,
        worker: None,
    };
    (handle, session)
}

#[test]
fn preferred_model_selection_matches_current_static_model() {
    let model_id = ModelId::ClaudeOpus5.as_str();
    let picker = base_picker_state("anthropic", &model_id);

    let selection = picker.preferred_model_selection();
    let Some(InlineListSelection::Model(index)) = selection else {
        panic!("expected static model selection, got {selection:?}");
    };

    let option = picker.settings.options.get(index).expect("selected index should be valid");
    assert_eq!(option.provider, Provider::Anthropic);
    assert_eq!(option.id, model_id);
}

#[test]
fn static_picker_indexes_resolve_provider_models() {
    let openai_indexes = option_indexes_for_provider(Provider::OpenAI);
    assert!(!openai_indexes.is_empty());

    let gpt56_sol_index = find_option_index(Provider::OpenAI, "GPT-5.6-SOL", &MODEL_OPTIONS)
        .expect("gpt-5.6-sol should be indexed case-insensitively");
    let option = MODEL_OPTIONS.get(gpt56_sol_index).expect("indexed option should exist");
    assert_eq!(option.id, "gpt-5.6-sol");
    assert_eq!(option.provider, Provider::OpenAI);
}

#[test]
fn preferred_model_selection_returns_none_for_unknown_model() {
    let picker = base_picker_state("anthropic", "does-not-exist");
    assert_eq!(picker.preferred_model_selection(), None);
}

#[test]
fn preferred_model_selection_matches_current_custom_provider() {
    let mut picker = base_picker_state("mycorp", "gpt-5-mini");
    let config = CustomProviderConfig {
        name: "mycorp".to_string(),
        display_name: "MyCorporateName".to_string(),
        base_url: "https://llm.corp.example/v1".to_string(),
        context_window: None,
        api_key_env: "MYCORP_API_KEY".to_string(),
        auth: None,
        model: "gpt-5-mini".to_string(),
        models: Vec::new(),
        ..CustomProviderConfig::default()
    };
    picker.custom_providers = selections_from_custom_provider(&config);

    let selection = picker.preferred_model_selection();
    let Some(InlineListSelection::CustomProvider(index)) = selection else {
        panic!("expected custom provider selection, got {selection:?}");
    };

    let detail = picker
        .custom_providers
        .get(index)
        .expect("selected custom provider should be valid");
    assert_eq!(detail.provider_key, "mycorp");
    assert_eq!(detail.provider_label, "MyCorporateName");
    assert_eq!(detail.model_id, "gpt-5-mini");
}

#[test]
fn read_workspace_env_returns_value_when_present() -> Result<()> {
    let dir = tempdir()?;
    let env_path = dir.path().join(".env");
    fs::write(&env_path, "OPENAI_API_KEY=sk-test\n")?;
    let value = read_workspace_env(dir.path(), "OPENAI_API_KEY")?;
    assert_eq!(value, Some("sk-test".to_string()));
    Ok(())
}

#[test]
fn read_workspace_env_returns_none_when_missing_file() -> Result<()> {
    let dir = tempdir()?;
    let value = read_workspace_env(dir.path(), "OPENAI_API_KEY")?;
    assert_eq!(value, None);
    Ok(())
}

#[test]
fn read_workspace_env_returns_none_when_key_absent() -> Result<()> {
    let dir = tempdir()?;
    let env_path = dir.path().join(".env");
    fs::write(&env_path, "OTHER_KEY=value\n")?;
    let value = read_workspace_env(dir.path(), "OPENAI_API_KEY")?;
    assert_eq!(value, None);
    Ok(())
}

#[test]
fn selection_marks_openai_service_tier_support_for_supported_models() {
    let detail = selection_from_option(
        MODEL_OPTIONS
            .iter()
            .find(|option| option.id == "gpt-5.6-sol")
            .expect("gpt-5.4 option should exist"),
    );

    assert!(detail.service_tier_supported);
}

#[test]
fn selection_omits_openai_service_tier_support_for_gpt_oss() {
    let detail = selection_from_option(
        MODEL_OPTIONS
            .iter()
            .find(|option| option.id == "gpt-oss-20b")
            .expect("gpt-oss option should exist"),
    );

    assert!(!detail.service_tier_supported);
}

#[test]
fn picker_separates_openrouter_reasoning_from_effort_support() {
    let option = MODEL_OPTIONS
        .iter()
        .find(|option| option.id == "meta/muse-spark-1.2")
        .expect("OpenRouter Muse Spark route should exist");
    let detail = selection_from_option(option);

    assert!(detail.reasoning_supported);
    assert!(!detail.reasoning_effort_supported);
    assert!(detail.reasoning_effort_levels().is_empty());
}

#[test]
fn openai_codex_reasoning_helpers_match_supported_variants() {
    assert!(!supports_gpt5_none_reasoning("gpt"));
    assert!(supports_gpt5_none_reasoning("gpt-5.6-sol"));
    assert!(supports_gpt5_none_reasoning("gpt-5.5-2026-04-23"));
    assert!(supports_gpt5_none_reasoning("gpt-5.2-codex"));
    assert!(supports_gpt5_none_reasoning("gpt-5-codex"));
    assert!(!supports_gpt5_none_reasoning("gpt-5.1-codex"));

    // The rolling GPT alias inherits its supported effort levels from the
    // generated catalog metadata.
    assert!(supports_xhigh_reasoning("gpt"));
    assert!(supports_xhigh_reasoning("gpt-5.6-sol"));
    assert!(!supports_xhigh_reasoning("gpt-5.5-2026-04-23"));
    assert!(supports_xhigh_reasoning("gpt-5.6"));
    assert!(!supports_xhigh_reasoning("gpt-5.2-codex"));
    assert!(supports_xhigh_reasoning("gpt-5-codex"));
    assert!(!supports_xhigh_reasoning("gpt-5.1-codex"));
    assert!(!supports_xhigh_reasoning("gpt-5.1-codex-max"));

    assert!(supports_xhigh_reasoning("claude-sonnet-5"));
    assert!(supports_xhigh_reasoning("claude-fable-5"));
    assert!(supports_xhigh_reasoning("claude-mythos-5"));
    assert!(supports_max_reasoning("claude-sonnet-5"));
    assert!(supports_max_reasoning("claude-fable-5"));
    assert!(supports_max_reasoning("claude-mythos-5"));
    // The GPT-5.6 family supports Max adaptive reasoning.
    assert!(supports_max_reasoning("gpt-5.6-sol"));
}

#[test]
fn build_result_uses_selected_service_tier() {
    let mut picker = base_picker_state("openai", "gpt-5.6-sol");
    picker.selection = Some(SelectionDetail {
        provider_key: "openai".to_string(),
        provider_label: "OpenAI".to_string(),
        provider_enum: Some(Provider::OpenAI),
        model_id: "gpt-5.6-sol".to_string(),
        model_display: "GPT-5.6 Sol".to_string(),
        known_model: true,
        context_window: None,
        reasoning_supported: true,
        reasoning_effort_supported: true,
        reasoning_optional: false,
        reasoning_off_model: None,
        service_tier_supported: true,
        requires_api_key: false,
        uses_chatgpt_auth: false,
        env_key: "OPENAI_API_KEY".to_string(),
        mimo_auth_method: None,
    });
    picker.selected_reasoning = Some(ReasoningEffortLevel::Low);
    picker.selected_service_tier = Some(Some(OpenAIServiceTier::Priority));

    let result = picker.build_result().expect("result should build");

    assert_eq!(result.service_tier, Some(OpenAIServiceTier::Priority));
    assert!(result.service_tier_changed);
}

#[test]
fn build_result_uses_selected_flex_service_tier() {
    let mut picker = base_picker_state("openai", "gpt-5.6-sol");
    picker.selection = Some(SelectionDetail {
        provider_key: "openai".to_string(),
        provider_label: "OpenAI".to_string(),
        provider_enum: Some(Provider::OpenAI),
        model_id: "gpt-5.6-sol".to_string(),
        model_display: "GPT-5.6 Sol".to_string(),
        known_model: true,
        context_window: None,
        reasoning_supported: true,
        reasoning_effort_supported: true,
        reasoning_optional: false,
        reasoning_off_model: None,
        service_tier_supported: true,
        requires_api_key: false,
        uses_chatgpt_auth: false,
        env_key: "OPENAI_API_KEY".to_string(),
        mimo_auth_method: None,
    });
    picker.selected_reasoning = Some(ReasoningEffortLevel::Low);
    picker.selected_service_tier = Some(Some(OpenAIServiceTier::Flex));

    let result = picker.build_result().expect("result should build");

    assert_eq!(result.service_tier, Some(OpenAIServiceTier::Flex));
    assert!(result.service_tier_changed);
}

#[test]
fn build_result_clears_inherited_reasoning_for_structured_only_route() {
    let mut picker = base_picker_state("openrouter", "meta/muse-spark-1.2");
    picker.selection = Some(SelectionDetail {
        provider_key: "openrouter".to_string(),
        provider_label: "OpenRouter".to_string(),
        provider_enum: Some(Provider::OpenRouter),
        model_id: "meta/muse-spark-1.2".to_string(),
        model_display: "Muse Spark 1.2".to_string(),
        known_model: true,
        context_window: Some(1_048_576),
        reasoning_supported: true,
        reasoning_effort_supported: false,
        reasoning_optional: false,
        reasoning_off_model: None,
        service_tier_supported: false,
        requires_api_key: false,
        uses_chatgpt_auth: false,
        env_key: "OPENROUTER_API_KEY".to_string(),
        mimo_auth_method: None,
    });
    picker.selected_reasoning = Some(ReasoningEffortLevel::High);

    let result = picker.build_result().expect("result should build");

    assert_eq!(result.reasoning, ReasoningEffortLevel::None);
    assert!(result.reasoning_changed);
}

#[tokio::test]
async fn openai_login_stays_in_picker_when_ctrl_c_cancels_auth() {
    let mut picker = base_picker_state("openai", "gpt-5.6-sol");
    picker.selection = Some(SelectionDetail {
        provider_key: "openai".to_string(),
        provider_label: "OpenAI".to_string(),
        provider_enum: Some(Provider::OpenAI),
        model_id: "gpt-5.6-sol".to_string(),
        model_display: "GPT-5.6 Sol".to_string(),
        known_model: true,
        context_window: None,
        reasoning_supported: true,
        reasoning_effort_supported: true,
        reasoning_optional: false,
        reasoning_off_model: None,
        service_tier_supported: true,
        requires_api_key: true,
        uses_chatgpt_auth: false,
        env_key: "OPENAI_API_KEY".to_string(),
        mimo_auth_method: None,
    });

    let (handle, mut session) = session_with_channels();
    let mut renderer = AnsiRenderer::with_inline_ui(handle.clone(), Default::default());
    let ctrl_c_state = Arc::new(CtrlCState::new());
    assert!(matches!(ctrl_c_state.register_signal(), crate::agent::runloop::unified::state::CtrlCSignal::Cancel));
    let ctrl_c_notify = Arc::new(Notify::new());
    let url_guard = crate::agent::runloop::unified::external_url_guard::ExternalUrlGuardContext::new(
        &handle,
        &mut session,
        &ctrl_c_state,
        &ctrl_c_notify,
    );

    let progress = picker
        .handle_api_key(&mut renderer, "login", url_guard)
        .await
        .expect("openai login should cancel cleanly");

    assert!(matches!(progress, ModelPickerProgress::InProgress));
    assert!(picker.pending_api_key.is_none());
}

#[test]
fn picker_provider_order_with_whitelist_filters_to_allowed() {
    use crate::agent::runloop::model_picker::options::picker_provider_order_with_whitelist;

    let order = picker_provider_order_with_whitelist(&["openai".to_string(), "anthropic".to_string()]);
    assert!(order.contains(&Provider::OpenAI));
    assert!(order.contains(&Provider::Anthropic));
    assert!(!order.contains(&Provider::Gemini));
}

#[test]
fn picker_provider_order_with_whitelist_empty_returns_all() {
    use crate::agent::runloop::model_picker::options::picker_provider_order_with_whitelist;

    let order = picker_provider_order_with_whitelist(&[]);
    assert_eq!(order.len(), Provider::all_providers().len());
}

#[test]
fn filter_options_by_whitelist_keeps_only_allowed_providers() {
    use crate::agent::runloop::model_picker::options::filter_options_by_whitelist;

    let filtered = filter_options_by_whitelist(Cow::Borrowed(MODEL_OPTIONS.as_slice()), &["openai".to_string()]);
    assert!(filtered.iter().all(|o| o.provider == Provider::OpenAI));
    assert!(!filtered.is_empty());
}

#[test]
fn filter_options_by_whitelist_empty_returns_all() {
    use crate::agent::runloop::model_picker::options::filter_options_by_whitelist;

    let filtered = filter_options_by_whitelist(Cow::Borrowed(MODEL_OPTIONS.as_slice()), &[]);
    assert_eq!(filtered.len(), MODEL_OPTIONS.len());
}
