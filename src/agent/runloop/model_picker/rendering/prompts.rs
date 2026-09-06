use std::path::Path;

use anyhow::Result;
use vtcode_config::OpenAIServiceTier;
use vtcode_core::config::models::Provider;
use vtcode_core::config::types::ReasoningEffortLevel;
use vtcode_core::ui::{InlineListItem, InlineListSelection, OpenAIServiceTierChoice, reasoning_to_selection_string};
use vtcode_core::utils::ansi::{AnsiRenderer, MessageStyle};

use super::super::selection::{
    SelectionDetail, reasoning_level_description, reasoning_level_label, service_tier_label,
};
use super::{CURRENT_BADGE, KEEP_CURRENT_DESCRIPTION, REASONING_OFF_BADGE, STEP_THREE_TITLE, STEP_TWO_TITLE};

pub(crate) fn render_reasoning_inline(
    renderer: &mut AnsiRenderer,
    selection: &SelectionDetail,
    current: ReasoningEffortLevel,
) -> Result<()> {
    let mut items = Vec::new();
    items.push(InlineListItem {
        title: format!("Keep current ({})", reasoning_level_label(current)),
        subtitle: Some(KEEP_CURRENT_DESCRIPTION.to_string()),
        badge: Some(CURRENT_BADGE.to_string()),
        indent: 0,
        selection: Some(InlineListSelection::Reasoning(reasoning_to_selection_string(current))),
        search_value: None,
    });

    let levels = selection.reasoning_effort_levels();
    if levels.contains(&ReasoningEffortLevel::None) {
        items.push(InlineListItem {
            title: reasoning_level_label(ReasoningEffortLevel::None).to_string(),
            subtitle: Some(reasoning_level_description(ReasoningEffortLevel::None).to_string()),
            badge: Some("GPT-5.x".to_string()),
            indent: 0,
            selection: Some(InlineListSelection::Reasoning(reasoning_to_selection_string(ReasoningEffortLevel::None))),
            search_value: None,
        });
    }

    for level in levels.into_iter().filter(|level| *level != ReasoningEffortLevel::None) {
        items.push(InlineListItem {
            title: reasoning_level_label(level).to_string(),
            subtitle: Some(reasoning_level_description(level).to_string()),
            badge: None,
            indent: 0,
            selection: Some(InlineListSelection::Reasoning(reasoning_to_selection_string(level))),
            search_value: None,
        });
    }

    if let Some(alternative) = selection.reasoning_off_model.as_ref() {
        items.push(InlineListItem {
            title: format!("Use {} (reasoning off)", alternative.display_name()),
            subtitle: Some(format!(
                "Switch to {} ({}) without enabling structured reasoning.",
                alternative.display_name(),
                alternative.as_str()
            )),
            badge: Some(REASONING_OFF_BADGE.to_string()),
            indent: 0,
            selection: Some(InlineListSelection::DisableReasoning),
            search_value: None,
        });
    }
    let mut lines = vec![format!(
        "Step 2 – select reasoning effort for {}.",
        selection.model_display
    )];
    if let Some(alternative) = selection.reasoning_off_model.as_ref() {
        lines.push(format!(
            "Select \"Use {} (reasoning off)\" to switch to {}.",
            alternative.display_name(),
            alternative.as_str()
        ));
    }
    renderer.show_list_modal(
        STEP_TWO_TITLE,
        lines,
        items,
        Some(InlineListSelection::Reasoning(reasoning_to_selection_string(current))),
        None,
    );
    Ok(())
}

pub(crate) fn prompt_reasoning_plain(
    renderer: &mut AnsiRenderer,
    selection: &SelectionDetail,
    current: ReasoningEffortLevel,
) -> Result<()> {
    let is_responses_flagship = selection.reasoning_effort_levels().contains(&ReasoningEffortLevel::None);
    let effort_choices = selection
        .reasoning_effort_levels()
        .into_iter()
        .map(reasoning_level_input)
        .collect::<Vec<_>>()
        .join("/");
    let effort_choices = if effort_choices.is_empty() {
        "skip".to_string()
    } else {
        effort_choices
    };

    if selection.reasoning_optional {
        renderer.line(
            MessageStyle::Info,
            &format!(
                "Step 2 – reasoning effort (current: {current}). Choose {effort_choices} or type 'skip' if the model does not expose configurable reasoning."
            ),
        )?;
    } else if let Some(alternative) = selection.reasoning_off_model.as_ref() {
        let gpt5_hint = if is_responses_flagship {
            " For GPT-5.x, 'none' provides lowest latency."
        } else {
            ""
        };
        renderer.line(
            MessageStyle::Info,
            &format!(
                "Step 2 – select reasoning effort for {} ({}{}). Type 'skip' to keep {} or 'off' to use {} ({}).{}",
                selection.model_display,
                effort_choices,
                "",
                alternative.display_name(),
                alternative.as_str(),
                alternative.display_name(),
                gpt5_hint
            ),
        )?;
    } else {
        let gpt5_hint = if is_responses_flagship {
            " For GPT-5.x, 'none' provides lowest latency."
        } else {
            ""
        };
        renderer.line(
            MessageStyle::Info,
            &format!(
                "Step 2 – select reasoning effort for {} ({}{}). Type 'skip' to keep {}. Current: {}.{}",
                selection.model_display, effort_choices, "", current, current, gpt5_hint
            ),
        )?;
    }
    Ok(())
}

fn reasoning_level_input(level: ReasoningEffortLevel) -> &'static str {
    match level {
        ReasoningEffortLevel::None => "none",
        ReasoningEffortLevel::Minimal => "minimal",
        ReasoningEffortLevel::Low => "low",
        ReasoningEffortLevel::Medium => "medium",
        ReasoningEffortLevel::High => "high",
        ReasoningEffortLevel::XHigh => "xhigh",
        ReasoningEffortLevel::Max => "max",
        ReasoningEffortLevel::Unknown => "unknown",
    }
}

pub(crate) fn prompt_api_key_plain(
    renderer: &mut AnsiRenderer,
    selection: &SelectionDetail,
    _workspace: Option<&Path>,
) -> Result<()> {
    if matches!(selection.provider_enum, Some(Provider::OpenAI)) {
        renderer.line(
            MessageStyle::Info,
            "Authentication – type 'login' to sign in with your ChatGPT subscription, paste an API key, or type 'skip' to reuse a stored credential.",
        )?;
        renderer.line(
            MessageStyle::Info,
            "ChatGPT subscription auth will be stored securely and will not be written to your workspace environment.",
        )?;
        return Ok(());
    }

    renderer.line(
        MessageStyle::Info,
        &format!("API key – enter a key for {} (env: {}).", selection.provider_label, selection.env_key),
    )?;
    renderer.line(MessageStyle::Info, "The key will be saved to secure storage (OS keyring or encrypted file).")?;
    renderer.line(MessageStyle::Info, "The key will NOT be stored in vtcode.toml for security.")?;
    renderer.line(MessageStyle::Info, "Or run `/secret add <provider>` to manage keys separately.")?;

    if matches!(selection.provider_enum, Some(Provider::HuggingFace)) {
        renderer.line(
            MessageStyle::Info,
            "Optional: override base URL with HUGGINGFACE_BASE_URL (default https://router.huggingface.co/v1).",
        )?;
    }
    renderer.line(MessageStyle::Info, "Paste the API key now or type 'skip' to reuse a stored credential.")?;
    Ok(())
}

pub(crate) fn render_service_tier_inline(
    renderer: &mut AnsiRenderer,
    selection: &SelectionDetail,
    current: Option<OpenAIServiceTier>,
) -> Result<()> {
    let items = vec![
        InlineListItem {
            title: format!("Keep current ({})", service_tier_label(current)),
            subtitle: Some("Retain the existing service tier configuration.".to_string()),
            badge: Some(CURRENT_BADGE.to_string()),
            indent: 0,
            selection: Some(InlineListSelection::OpenAIServiceTier(match current {
                Some(OpenAIServiceTier::Flex) => OpenAIServiceTierChoice::Flex,
                Some(OpenAIServiceTier::Priority) => OpenAIServiceTierChoice::Priority,
                None => OpenAIServiceTierChoice::ProjectDefault,
            })),
            search_value: None,
        },
        InlineListItem {
            title: "Project default".to_string(),
            subtitle: Some("Do not send service_tier; inherit the OpenAI Project setting.".to_string()),
            badge: None,
            indent: 0,
            selection: Some(InlineListSelection::OpenAIServiceTier(OpenAIServiceTierChoice::ProjectDefault)),
            search_value: None,
        },
        InlineListItem {
            title: "Flex".to_string(),
            subtitle: Some("Send service_tier=flex for lower-cost, lower-priority processing.".to_string()),
            badge: Some("OpenAI".to_string()),
            indent: 0,
            selection: Some(InlineListSelection::OpenAIServiceTier(OpenAIServiceTierChoice::Flex)),
            search_value: None,
        },
        InlineListItem {
            title: "Priority".to_string(),
            subtitle: Some("Send service_tier=priority for lower and more consistent latency.".to_string()),
            badge: Some("OpenAI".to_string()),
            indent: 0,
            selection: Some(InlineListSelection::OpenAIServiceTier(OpenAIServiceTierChoice::Priority)),
            search_value: None,
        },
    ];

    renderer.show_list_modal(
        STEP_THREE_TITLE,
        vec![
            format!("Select a service tier for {}.", selection.model_display),
            "Applies only to native OpenAI models that support OpenAI service tiers.".to_string(),
        ],
        items,
        Some(InlineListSelection::OpenAIServiceTier(match current {
            Some(OpenAIServiceTier::Flex) => OpenAIServiceTierChoice::Flex,
            Some(OpenAIServiceTier::Priority) => OpenAIServiceTierChoice::Priority,
            None => OpenAIServiceTierChoice::ProjectDefault,
        })),
        None,
    );
    Ok(())
}

pub(crate) fn prompt_service_tier_plain(
    renderer: &mut AnsiRenderer,
    selection: &SelectionDetail,
    current: Option<OpenAIServiceTier>,
) -> Result<()> {
    renderer.line(
        MessageStyle::Info,
        &format!(
            "Service tier – choose 'flex', 'priority', or 'default' for {}. Type 'skip' to keep {}.",
            selection.model_display,
            service_tier_label(current)
        ),
    )?;
    renderer
        .line(MessageStyle::Info, "This applies only to native OpenAI models that support OpenAI service tiers.")?;
    Ok(())
}

pub(crate) fn show_secure_api_modal(
    renderer: &mut AnsiRenderer,
    selection: &SelectionDetail,
    _workspace: Option<&Path>,
) {
    let storage_line = "Saved to secure storage (OS keyring or encrypted file).".to_string();
    let mask_preview = "●●●●●●";
    let lines = vec![
        format!("Bring your own key (BYOK) for {}.", selection.provider_label),
        format!("Expected env: {}", selection.env_key),
        format!("Secure display hint: {}", mask_preview),
        storage_line,
        "Key will NOT be stored in vtcode.toml.".to_string(),
        "Paste the key — it will be auto-detected and saved securely.".to_string(),
    ];
    let prompt_label = format!("{} API key ({})", selection.provider_label, selection.env_key);
    renderer.show_secure_prompt_modal("Secure API key setup", lines, prompt_label);
}

pub(crate) fn prompt_custom_model_entry(renderer: &mut AnsiRenderer) -> Result<()> {
    renderer.line(
        MessageStyle::Info,
        "Enter a provider and model identifier (examples: 'openai gpt-5-nano', 'huggingface meta-llama/Meta-Llama-3-70B-Instruct', 'ollama qwen3:1.7b').",
    )?;
    renderer.line(
        MessageStyle::Info,
        "For Ollama, you can use any locally available model like 'llama3:8b', 'mistral:7b', etc.",
    )?;
    renderer.line(MessageStyle::Info, "Type 'cancel' to exit the picker at any time.")?;
    renderer.line(MessageStyle::Info, "Type 'refresh' to reload LM Studio and Ollama model lists.")?;
    Ok(())
}

pub(crate) fn render_mimo_auth_method_inline(renderer: &mut AnsiRenderer) -> Result<()> {
    let items = vec![
        InlineListItem {
            title: "Pay-as-you-go".to_string(),
            subtitle: Some("Standard API access. Uses sk- key with api-key header.".to_string()),
            badge: Some("Default".to_string()),
            indent: 0,
            selection: Some(InlineListSelection::ConfigAction("mimo-auth:pay-as-you-go".to_string())),
            search_value: Some("mimo payg pay-as-you-go sk api key".to_string()),
        },
        InlineListItem {
            title: "Token Plan".to_string(),
            subtitle: Some(
                "Subscription-based access. Uses tp- key with Bearer token. Includes more models.".to_string(),
            ),
            badge: Some("Subscription".to_string()),
            indent: 0,
            selection: Some(InlineListSelection::ConfigAction("mimo-auth:token-plan".to_string())),
            search_value: Some("mimo token plan subscription tp bearer".to_string()),
        },
    ];

    renderer.show_list_modal(
        "MiMo Auth Method",
        vec![
            "Choose an authentication method for Xiaomi MiMo.".to_string(),
            "Token Plan defaults to Europe cluster (token-plan-ams). Set MIMO_TOKEN_PLAN_BASE_URL to override region."
                .to_string(),
        ],
        items,
        None,
        None,
    );
    Ok(())
}

pub(crate) fn prompt_mimo_auth_method_plain(renderer: &mut AnsiRenderer) -> Result<()> {
    renderer.line(
        MessageStyle::Info,
        "MiMo auth method - choose 'pay-as-you-go' or 'token-plan'. Type 'skip' for default (pay-as-you-go).",
    )?;
    renderer.line(MessageStyle::Info, "Pay-as-you-go: standard API access with sk- key.")?;
    renderer.line(MessageStyle::Info, "Token Plan: subscription-based access with tp- key. Includes more models.")?;
    renderer.line(
        MessageStyle::Info,
        "Note: Token Plan defaults to Europe cluster (token-plan-ams). Set MIMO_TOKEN_PLAN_BASE_URL to use China (token-plan-cn) or Singapore (token-plan-sgp).",
    )?;
    Ok(())
}
