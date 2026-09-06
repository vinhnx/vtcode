use anyhow::{Result, anyhow};

use vtcode_config::OpenAIServiceTier;
use vtcode_config::auth::AuthCredentialsStoreMode;
use vtcode_core::config::models::Provider;
use vtcode_core::config::types::ReasoningEffortLevel;
use vtcode_ui::tui::ui::interactive_list::{SelectionEntry, run_interactive_selection};

use super::dynamic_models::DynamicModelRegistry;
use super::options::{ModelOption, option_indexes_for_provider};
use super::rendering::{
    CUSTOM_PROVIDER_SUBTITLE, CUSTOM_PROVIDER_TITLE, KEEP_CURRENT_DESCRIPTION, dynamic_model_subtitle, join_with_label,
    static_model_subtitle,
};
use super::selection::{
    ReasoningChoice, SelectionDetail, ServiceTierChoice, reasoning_level_description, reasoning_level_label,
    selection_from_option_with_mode, service_tier_label,
};

pub(super) const REFRESH_ENTRY_LABEL: &str = "Refresh dynamic model lists";

pub(super) fn custom_provider_description(selection: &SelectionDetail) -> String {
    if selection.model_id.trim().is_empty() {
        format!("{} • Configure a model in vtcode.toml", selection.provider_label)
    } else {
        format!("{} • {}", selection.provider_label, selection.model_id)
    }
}

#[derive(Clone)]
pub(super) struct ModelSelectionChoice {
    pub(super) entry: SelectionEntry,
    pub(super) outcome: ModelSelectionChoiceOutcome,
}

#[derive(Clone)]
pub(super) enum ModelSelectionChoiceOutcome {
    Predefined(SelectionDetail),
    Manual,
    Refresh,
}

pub(super) enum ModelSelectionListOutcome {
    Predefined(SelectionDetail),
    Manual,
    Refresh,
    Cancelled,
}

pub(super) fn select_model_with_ratatui_list(
    options: &[ModelOption],
    _current_reasoning: ReasoningEffortLevel,
    dynamic_models: &DynamicModelRegistry,
    custom_providers: &[SelectionDetail],
    provider_order: &[Provider],
    storage_mode: AuthCredentialsStoreMode,
) -> Result<ModelSelectionListOutcome> {
    if options.is_empty() {
        return Err(anyhow!("No models available for selection"));
    }

    let estimated = options.len() + dynamic_models.entries.len() + custom_providers.len() + 5;
    let mut choices = Vec::with_capacity(estimated);
    for &provider in provider_order {
        for option_index in option_indexes_for_provider(provider) {
            let Some(option) = options.get(*option_index) else {
                continue;
            };
            let description = join_with_label(provider.label(), static_model_subtitle(option, "", ""));
            choices.push(ModelSelectionChoice {
                entry: SelectionEntry::new(
                    option.display.to_string(),
                    Some(format!("{description}\n{}", option.description)),
                ),
                outcome: ModelSelectionChoiceOutcome::Predefined(selection_from_option_with_mode(option, storage_mode)),
            });
        }
        if provider.is_dynamic() {
            let dynamic_indexes = dynamic_models.indexes_for(provider);
            if dynamic_indexes.is_empty() {
                if let Some(error) = dynamic_models.error_for(provider) {
                    choices.push(ModelSelectionChoice {
                        entry: SelectionEntry::new(
                            format!("{} server not running - Setup instructions", provider.label()),
                            Some(format!("{error}\n{}", provider.local_install_instructions().unwrap_or(""))),
                        ),
                        outcome: ModelSelectionChoiceOutcome::Manual,
                    });
                }
            } else {
                for entry_index in dynamic_indexes {
                    if let Some(detail) = dynamic_models.detail(*entry_index) {
                        let description = join_with_label(
                            provider.label(),
                            dynamic_model_subtitle(provider, &detail.model_id, detail.reasoning_supported, "", ""),
                        );
                        choices.push(ModelSelectionChoice {
                            entry: SelectionEntry::new(
                                detail.model_display.clone(),
                                Some(format!("{description}\nLocally available {} model", provider.label(),)),
                            ),
                            outcome: ModelSelectionChoiceOutcome::Predefined(detail.clone()),
                        });
                    }
                }
            }

            if let Some(warning) = dynamic_models.warning_for(provider) {
                choices.push(ModelSelectionChoice {
                    entry: SelectionEntry::new(
                        format!("{} cache notice", provider.label()),
                        Some(format!("{warning} Select 'Refresh local models' to re-query.")),
                    ),
                    outcome: ModelSelectionChoiceOutcome::Refresh,
                });
            }
        } else if provider == Provider::HuggingFace {
            choices.push(ModelSelectionChoice {
                entry: SelectionEntry::new(
                    "Hugging Face • Custom model",
                    Some("Enter any HF model id (e.g., huggingface <org>/<model>).".to_string()),
                ),
                outcome: ModelSelectionChoiceOutcome::Manual,
            });
        }
    }

    for selection in custom_providers {
        let description = custom_provider_description(selection);
        choices.push(ModelSelectionChoice {
            entry: SelectionEntry::new(
                super::rendering::custom_provider_picker_title(selection),
                Some(format!("{description}\nConfigured custom OpenAI-compatible endpoint")),
            ),
            outcome: ModelSelectionChoiceOutcome::Predefined(selection.clone()),
        });
    }

    choices.push(ModelSelectionChoice {
        entry: SelectionEntry::new(
            REFRESH_ENTRY_LABEL,
            Some("Re-query dynamic provider model lists without closing the picker.".to_string()),
        ),
        outcome: ModelSelectionChoiceOutcome::Refresh,
    });

    choices.push(ModelSelectionChoice {
        entry: SelectionEntry::new(CUSTOM_PROVIDER_TITLE, Some(CUSTOM_PROVIDER_SUBTITLE.to_string())),
        outcome: ModelSelectionChoiceOutcome::Manual,
    });

    let entries: Vec<SelectionEntry> = choices.iter().map(|choice| &choice.entry).cloned().collect();

    let instructions =
        "Provider, model id, reasoning, tools, and input modalities are shown in each entry.".to_string();

    let selection = run_interactive_selection("Models", &instructions, &entries, 0)?;
    let selected_index = match selection {
        Some(index) => index,
        None => return Ok(ModelSelectionListOutcome::Cancelled),
    };

    match &choices[selected_index].outcome {
        ModelSelectionChoiceOutcome::Predefined(detail) => Ok(ModelSelectionListOutcome::Predefined(detail.clone())),
        ModelSelectionChoiceOutcome::Manual => Ok(ModelSelectionListOutcome::Manual),
        ModelSelectionChoiceOutcome::Refresh => Ok(ModelSelectionListOutcome::Refresh),
    }
}

pub(super) fn select_reasoning_with_ratatui(
    selection: &SelectionDetail,
    current: ReasoningEffortLevel,
) -> Result<Option<ReasoningChoice>> {
    let mut entries = vec![SelectionEntry::new(
        format!("Keep current ({})", reasoning_level_label(current)),
        Some(KEEP_CURRENT_DESCRIPTION.to_string()),
    )];

    let mut level_entries: Vec<(usize, ReasoningEffortLevel)> = Vec::new();
    let levels = selection.reasoning_effort_levels();

    for level in levels {
        entries.push(SelectionEntry::new(
            reasoning_level_label(level),
            Some(reasoning_level_description(level).to_string()),
        ));
        level_entries.push((entries.len() - 1, level));
    }

    let mut disable_index = None;
    if let Some(alternative) = selection.reasoning_off_model.clone() {
        entries.push(SelectionEntry::new(
            format!("Use {} (reasoning off)", alternative.display_name()),
            Some(format!(
                "Switch to {} ({}) without enabling structured reasoning.",
                alternative.display_name(),
                alternative.as_str()
            )),
        ));
        disable_index = Some(entries.len() - 1);
    }

    let mut instructions = format!(
        "Select reasoning effort for {}. Esc keeps the current level ({}).",
        selection.model_display,
        reasoning_level_label(current),
    );
    if let Some(alternative) = selection.reasoning_off_model.clone() {
        instructions.push(' ');
        instructions.push_str(&format!(
            "Choose \"Use {} (reasoning off)\" to switch to {}.",
            alternative.display_name(),
            alternative.as_str()
        ));
    }

    let selection_index = run_interactive_selection("Reasoning effort", &instructions, &entries, 0)?;

    let Some(index) = selection_index else {
        return Ok(None);
    };

    if disable_index == Some(index) {
        return Ok(Some(ReasoningChoice::Disable));
    }

    let selected_level = level_entries
        .iter()
        .find_map(|(idx, level)| (*idx == index).then_some(*level))
        .unwrap_or(current);

    Ok(Some(ReasoningChoice::Level(selected_level)))
}

pub(super) fn select_service_tier_with_ratatui(
    selection: &SelectionDetail,
    current: Option<OpenAIServiceTier>,
) -> Result<Option<ServiceTierChoice>> {
    let current_choice = match current {
        Some(OpenAIServiceTier::Flex) => ServiceTierChoice::Flex,
        Some(OpenAIServiceTier::Priority) => ServiceTierChoice::Priority,
        None => ServiceTierChoice::ProjectDefault,
    };

    let choices = [
        (
            format!("Keep current ({})", service_tier_label(current)),
            "Retain the existing service tier configuration.".to_string(),
            current_choice,
        ),
        (
            "Project default".to_string(),
            "Do not send service_tier; inherit the OpenAI Project setting.".to_string(),
            ServiceTierChoice::ProjectDefault,
        ),
        (
            "Flex".to_string(),
            "Send service_tier=flex for lower-cost, lower-priority processing.".to_string(),
            ServiceTierChoice::Flex,
        ),
        (
            "Priority".to_string(),
            "Send service_tier=priority for lower and more consistent latency.".to_string(),
            ServiceTierChoice::Priority,
        ),
    ];

    let entries: Vec<SelectionEntry> = choices
        .iter()
        .map(|(title, subtitle, _)| SelectionEntry::new(title.clone(), Some(subtitle.clone())))
        .collect();

    let instructions = format!(
        "Select a service tier for {}. Esc keeps the current setting ({}).",
        selection.model_display,
        service_tier_label(current)
    );

    let selection_index = run_interactive_selection("Service tier", &instructions, &entries, 0)?;

    let Some(index) = selection_index else {
        return Ok(None);
    };

    Ok(Some(choices[index].2))
}
