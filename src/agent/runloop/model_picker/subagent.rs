//! Subagent model and reasoning selection.
//!
//! This module handles the interactive model picker for delegated/subagent sessions,
//! including shortcut aliases, concrete model selection, and reasoning override configuration.

use std::path::Path;
use std::sync::Arc;
use tokio::sync::Notify;
use tokio::task;

use anyhow::{Result, anyhow};
use vtcode_config::VTCodeConfig;
use vtcode_core::config::models::ModelId;
use vtcode_core::config::types::ReasoningEffortLevel;
use vtcode_core::llm::ModelResolver;
use vtcode_core::ui::{InlineListSearchConfig, InlineListSelection};
use vtcode_core::utils::ansi::{AnsiRenderer, MessageStyle};
use vtcode_ui::tui::app::{
    InlineHandle, InlineListItem, InlineSession, TransientSubmission, WizardModalMode, WizardStep,
};

use super::DynamicModelRegistry;
use super::options::{ModelOption, build_filtered_options, find_option_index};
use super::rendering::{
    dynamic_model_subtitle, join_with_label, model_search_value, static_model_search_terms, static_model_subtitle,
};
use super::selection::{
    SelectionDetail, parse_model_selection, reasoning_level_description, reasoning_level_label, selection_from_option,
};
use crate::agent::runloop::unified::overlay_prompt::{OverlayWaitOutcome, wait_for_overlay_submission};
use crate::agent::runloop::unified::state::CtrlCState;
use crate::agent::runloop::unified::wizard_modal::{WizardModalOutcome, show_wizard_modal_and_wait};

const SUBAGENT_MODEL_ACTION_PREFIX: &str = "subagent-model:";
const SUBAGENT_REASONING_ACTION_PREFIX: &str = "subagent-reasoning:";
const SUBAGENT_MODEL_PROMPT_ID: &str = "subagent-model-id";
const SUBAGENT_SHORTCUTS: [(&str, &str); 5] = [
    ("inherit", "Use the parent session model and configuration."),
    ("small", "Use VT Code's lightweight delegated-model shortcut."),
    ("haiku", "Use the Anthropic Haiku shortcut alias for delegated work."),
    ("sonnet", "Use the Anthropic Sonnet shortcut alias for delegated work."),
    ("opus", "Use the Anthropic Opus shortcut alias for delegated work."),
];

#[derive(Clone, Debug)]
pub(super) enum SubagentModelChoice {
    Target(SubagentModelTarget),
    Refresh,
    Manual,
}

#[derive(Clone, Debug)]
pub(crate) struct SubagentModelSelection {
    pub(crate) model: String,
    pub(crate) reasoning_effort: Option<String>,
}

#[derive(Clone, Debug)]
pub(super) enum SubagentModelTarget {
    Shortcut { model: String },
    Concrete(SelectionDetail),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SubagentReasoningChoice {
    KeepCurrent,
    Unset,
    Explicit(ReasoningEffortLevel),
}

pub(crate) async fn pick_subagent_model(
    renderer: &mut AnsiRenderer,
    handle: &InlineHandle,
    session: &mut InlineSession,
    ctrl_c_state: &Arc<CtrlCState>,
    ctrl_c_notify: &Arc<Notify>,
    vt_cfg: Option<&VTCodeConfig>,
    workspace: Option<&Path>,
    current_model: &str,
    current_reasoning_effort: Option<&str>,
) -> Result<Option<SubagentModelSelection>> {
    if !renderer.supports_inline_ui() {
        renderer.line(MessageStyle::Info, "Interactive subagent model selection requires inline UI.")?;
        return Ok(None);
    }

    let options = build_filtered_options(vt_cfg);

    let mut dynamic_models = DynamicModelRegistry::load(&options, workspace, vt_cfg).await;
    loop {
        let Some(target) = select_subagent_model_target(
            handle,
            session,
            ctrl_c_state,
            ctrl_c_notify,
            &dynamic_models,
            &options,
            current_model,
        )
        .await?
        else {
            return Ok(None);
        };

        let target = match target {
            SubagentModelChoice::Target(target) => target,
            SubagentModelChoice::Refresh => {
                renderer.line(MessageStyle::Info, "Refreshing local model inventory...")?;
                dynamic_models = DynamicModelRegistry::load(&options, workspace, vt_cfg).await;
                continue;
            }
            SubagentModelChoice::Manual => {
                let Some(target) =
                    prompt_subagent_model_id(renderer, handle, session, ctrl_c_state, ctrl_c_notify, &options).await?
                else {
                    return Ok(None);
                };
                target
            }
        };

        let Some(selection) =
            select_subagent_reasoning(handle, session, ctrl_c_state, ctrl_c_notify, target, current_reasoning_effort)
                .await?
        else {
            return Ok(None);
        };

        return Ok(Some(selection));
    }
}

async fn select_subagent_model_target(
    handle: &InlineHandle,
    session: &mut InlineSession,
    ctrl_c_state: &Arc<CtrlCState>,
    ctrl_c_notify: &Arc<Notify>,
    dynamic_models: &DynamicModelRegistry,
    options: &[ModelOption],
    current_model: &str,
) -> Result<Option<SubagentModelChoice>> {
    let mut items = Vec::new();
    for (shortcut, description) in subagent_model_shortcuts() {
        items.push(InlineListItem {
            title: (*shortcut).to_string(),
            subtitle: Some((*description).to_string()),
            badge: Some("Shortcut".to_string()),
            indent: 0,
            selection: Some(InlineListSelection::ConfigAction(format!(
                "{SUBAGENT_MODEL_ACTION_PREFIX}shortcut:{shortcut}"
            ))),
            search_value: Some(format!("{shortcut} shortcut alias delegated model {description}")),
        });
    }

    for (index, option) in options.iter().enumerate() {
        let current_provider = if current_model.eq_ignore_ascii_case(&option.id) {
            option.provider.as_ref()
        } else {
            ""
        };
        items.push(InlineListItem {
            title: option.display.to_string(),
            subtitle: Some(join_with_label(
                option.provider.label(),
                static_model_subtitle(option, current_provider, current_model),
            )),
            badge: Some(option.provider.label().to_string()),
            indent: 0,
            selection: Some(InlineListSelection::Model(index)),
            search_value: Some(model_search_value(
                option.provider,
                &option.display,
                &option.id,
                Some(&option.description),
                &static_model_search_terms(&option.model, option.supports_reasoning),
            )),
        });
    }

    for entry_index in parseable_subagent_dynamic_indexes(dynamic_models) {
        let Some(detail) = dynamic_models.detail(entry_index) else {
            continue;
        };
        let Some(provider) = detail.provider_enum else {
            continue;
        };
        let current_provider = if current_model.eq_ignore_ascii_case(&detail.model_id) {
            provider.as_ref()
        } else {
            ""
        };
        items.push(InlineListItem {
            title: detail.model_display.clone(),
            subtitle: Some(join_with_label(
                provider.label(),
                dynamic_model_subtitle(
                    provider,
                    &detail.model_id,
                    detail.reasoning_supported,
                    current_provider,
                    current_model,
                ),
            )),
            badge: Some(provider.label().to_string()),
            indent: 0,
            selection: Some(InlineListSelection::DynamicModel(entry_index)),
            search_value: Some(model_search_value(
                provider,
                &detail.model_display,
                &detail.model_id,
                None,
                &[provider.label().to_string(), "dynamic".to_string()],
            )),
        });
    }

    items.push(InlineListItem {
        title: "Refresh local models".to_string(),
        subtitle: Some("Re-query dynamic model inventories without changing workspace config.".to_string()),
        badge: Some("Refresh".to_string()),
        indent: 0,
        selection: Some(InlineListSelection::RefreshDynamicModels),
        search_value: Some("refresh dynamic local models".to_string()),
    });
    items.push(InlineListItem {
        title: "Enter exact model id".to_string(),
        subtitle: Some("Provide a concrete VT Code model id such as `gpt-5.4` or `claude-sonnet-4-6`.".to_string()),
        badge: Some("Manual".to_string()),
        indent: 0,
        selection: Some(InlineListSelection::CustomModel),
        search_value: Some("manual exact model id".to_string()),
    });

    let selected = preferred_subagent_model_selection(options, dynamic_models, current_model)
        .or_else(|| items.first().and_then(|item| item.selection.clone()));
    handle.show_list_modal(
        "Subagent model".to_string(),
        vec!["Pick a shortcut alias or concrete model id — stores only `model` and `reasoning_effort`.".to_string()],
        items,
        selected,
        Some(InlineListSearchConfig {
            label: String::new(),
            placeholder: Some("shortcut, provider, model id".to_string()),
        }),
    );

    let Some(selection) = wait_for_inline_list_selection(handle, session, ctrl_c_state, ctrl_c_notify).await? else {
        return Ok(None);
    };

    let choice = match selection {
        InlineListSelection::ConfigAction(action) => {
            if let Some(shortcut) = action.strip_prefix(&format!("{SUBAGENT_MODEL_ACTION_PREFIX}shortcut:")) {
                SubagentModelChoice::Target(SubagentModelTarget::Shortcut { model: shortcut.to_string() })
            } else {
                return Ok(None);
            }
        }
        InlineListSelection::Model(index) => {
            let option = options
                .get(index)
                .ok_or_else(|| anyhow!("Unable to locate the selected model option"))?;
            SubagentModelChoice::Target(SubagentModelTarget::Concrete(selection_from_option(option)))
        }
        InlineListSelection::DynamicModel(index) => {
            let detail = dynamic_models
                .dynamic_detail(index)
                .ok_or_else(|| anyhow!("Unable to locate the selected dynamic model"))?;
            SubagentModelChoice::Target(SubagentModelTarget::Concrete(detail))
        }
        InlineListSelection::RefreshDynamicModels => SubagentModelChoice::Refresh,
        InlineListSelection::CustomModel => SubagentModelChoice::Manual,
        _ => return Ok(None),
    };

    Ok(Some(choice))
}

async fn select_subagent_reasoning(
    handle: &InlineHandle,
    session: &mut InlineSession,
    ctrl_c_state: &Arc<CtrlCState>,
    ctrl_c_notify: &Arc<Notify>,
    target: SubagentModelTarget,
    current_reasoning_effort: Option<&str>,
) -> Result<Option<SubagentModelSelection>> {
    let model = target.model().to_string();
    if !target.supports_reasoning() {
        return Ok(Some(SubagentModelSelection { model, reasoning_effort: None }));
    }

    let current_reasoning_level = normalized_subagent_reasoning(&target, current_reasoning_effort);
    let current_label = current_reasoning_level.map(reasoning_level_label).unwrap_or("unset");
    let mut items = vec![
        InlineListItem {
            title: format!("Keep current ({current_label})"),
            subtitle: Some("Retain the current reasoning override for this subagent.".to_string()),
            badge: Some("Current".to_string()),
            indent: 0,
            selection: Some(InlineListSelection::ConfigAction(format!("{SUBAGENT_REASONING_ACTION_PREFIX}keep"))),
            search_value: Some("keep current reasoning".to_string()),
        },
        InlineListItem {
            title: "Unset reasoning override".to_string(),
            subtitle: Some("Do not store a `reasoning_effort` override for this subagent.".to_string()),
            badge: Some("Unset".to_string()),
            indent: 0,
            selection: Some(InlineListSelection::ConfigAction(format!("{SUBAGENT_REASONING_ACTION_PREFIX}unset"))),
            search_value: Some("unset clear reasoning".to_string()),
        },
    ];

    for level in subagent_reasoning_levels(target.model(), target.supports_reasoning()) {
        items.push(InlineListItem {
            title: reasoning_level_label(level).to_string(),
            subtitle: Some(reasoning_level_description(level).to_string()),
            badge: None,
            indent: 0,
            selection: Some(InlineListSelection::ConfigAction(format!(
                "{SUBAGENT_REASONING_ACTION_PREFIX}{}",
                level.as_str()
            ))),
            search_value: Some(format!("{} {}", level.as_str(), reasoning_level_label(level))),
        });
    }

    handle.show_list_modal(
        "Subagent reasoning".to_string(),
        vec![format!(
            "Choose the reasoning override for `{}`. Esc cancels the picker.",
            target.model()
        )],
        items,
        Some(InlineListSelection::ConfigAction(format!("{SUBAGENT_REASONING_ACTION_PREFIX}keep"))),
        Some(InlineListSearchConfig {
            label: String::new(),
            placeholder: Some("keep, unset, high".to_string()),
        }),
    );

    let Some(selection) = wait_for_inline_list_selection(handle, session, ctrl_c_state, ctrl_c_notify).await? else {
        return Ok(None);
    };

    let reasoning_choice = match selection {
        InlineListSelection::ConfigAction(action) if action == "subagent-reasoning:keep" => {
            SubagentReasoningChoice::KeepCurrent
        }
        InlineListSelection::ConfigAction(action) if action == "subagent-reasoning:unset" => {
            SubagentReasoningChoice::Unset
        }
        InlineListSelection::ConfigAction(action) => {
            let level_key = action
                .strip_prefix(SUBAGENT_REASONING_ACTION_PREFIX)
                .ok_or_else(|| anyhow!("Unknown subagent reasoning selection"))?;
            let level = ReasoningEffortLevel::parse(level_key)
                .ok_or_else(|| anyhow!("Unknown reasoning effort level `{level_key}`"))?;
            SubagentReasoningChoice::Explicit(level)
        }
        _ => return Ok(None),
    };

    let reasoning_effort = match reasoning_choice {
        SubagentReasoningChoice::KeepCurrent => current_reasoning_level.map(|level| level.as_str().to_string()),
        SubagentReasoningChoice::Unset => None,
        SubagentReasoningChoice::Explicit(level) => Some(level.as_str().to_string()),
    };

    Ok(Some(SubagentModelSelection { model, reasoning_effort }))
}

async fn prompt_subagent_model_id(
    renderer: &mut AnsiRenderer,
    handle: &InlineHandle,
    session: &mut InlineSession,
    ctrl_c_state: &Arc<CtrlCState>,
    ctrl_c_notify: &Arc<Notify>,
    options: &[ModelOption],
) -> Result<Option<SubagentModelTarget>> {
    loop {
        let outcome = show_wizard_modal_and_wait(
            handle,
            session,
            "Subagent model id".to_string(),
            vec![WizardStep {
                title: "Model id".to_string(),
                question: "Enter a concrete VT Code model id. Shortcut aliases such as `inherit` and `small` are available in the list view.".to_string(),
                items: vec![InlineListItem {
                    title: "Enter a model id".to_string(),
                    subtitle: Some(
                        "Press Tab to type inline, then Enter to confirm the model id."
                            .to_string(),
                    ),
                    badge: Some("Input".to_string()),
                    indent: 0,
                    selection: Some(InlineListSelection::RequestUserInputAnswer {
                        question_id: SUBAGENT_MODEL_PROMPT_ID.to_string(),
                        selected: vec![],
                        other: Some(String::new()),
                    }),
                    search_value: Some("manual model id".to_string()),
                }],
                completed: false,
                answer: None,
                allow_freeform: true,
                freeform_label: Some("Model id".to_string()),
                freeform_placeholder: Some("gpt-5.6-sol".to_string()),
                freeform_default: None,
            }],
            0,
            None,
            WizardModalMode::MultiStep,
            ctrl_c_state,
            ctrl_c_notify,
        )
        .await?;

        let Some(selection) = (match outcome {
            WizardModalOutcome::Submitted(selections) => selections.into_iter().next(),
            WizardModalOutcome::Cancelled { .. } => None,
        }) else {
            return Ok(None);
        };

        let InlineListSelection::RequestUserInputAnswer { other, selected, .. } = selection else {
            return Ok(None);
        };
        let raw_value = other.or_else(|| selected.first().cloned()).unwrap_or_default();
        let trimmed = raw_value.trim();
        if trimmed.is_empty() {
            continue;
        }

        let model_id = match trimmed.parse::<ModelId>() {
            Ok(model_id) => model_id,
            Err(_) => {
                renderer.line(MessageStyle::Error, &format!("`{trimmed}` is not a recognized VT Code model id."))?;
                continue;
            }
        };
        let detail = parse_model_selection(options, &format!("{} {}", model_id.provider(), model_id.as_str()), None)?;
        return Ok(Some(SubagentModelTarget::Concrete(detail)));
    }
}

pub(super) fn preferred_subagent_model_selection(
    options: &[ModelOption],
    dynamic_models: &DynamicModelRegistry,
    current_model: &str,
) -> Option<InlineListSelection> {
    let current_trimmed = current_model.trim();
    if current_trimmed.is_empty() {
        return None;
    }
    if let Some(shortcut) = canonical_subagent_shortcut(current_trimmed) {
        return Some(InlineListSelection::ConfigAction(format!("{SUBAGENT_MODEL_ACTION_PREFIX}shortcut:{shortcut}")));
    }
    if let Ok(model_id) = current_trimmed.parse::<ModelId>()
        && let Some(index) = find_option_index(model_id.provider(), &model_id.as_str(), options)
    {
        return Some(InlineListSelection::Model(index));
    }
    parseable_subagent_dynamic_indexes(dynamic_models)
        .into_iter()
        .find_map(|index| {
            dynamic_models
                .detail(index)
                .filter(|detail| detail.model_id.eq_ignore_ascii_case(current_trimmed))
                .map(|_| InlineListSelection::DynamicModel(index))
        })
}

pub(super) fn normalized_subagent_reasoning(
    target: &SubagentModelTarget,
    current_reasoning_effort: Option<&str>,
) -> Option<ReasoningEffortLevel> {
    let level = parse_subagent_reasoning_effort(current_reasoning_effort)?;
    subagent_supports_reasoning_level(target, level).then_some(level)
}

fn is_subagent_shortcut(model: &str) -> bool {
    canonical_subagent_shortcut(model).is_some()
}

fn canonical_subagent_shortcut(model: &str) -> Option<&'static str> {
    subagent_model_shortcuts()
        .iter()
        .find(|(shortcut, _)| shortcut.eq_ignore_ascii_case(model.trim()))
        .map(|(shortcut, _)| *shortcut)
}

pub(super) fn subagent_model_shortcuts() -> &'static [(&'static str, &'static str)] {
    &SUBAGENT_SHORTCUTS
}

fn parse_subagent_reasoning_effort(current_reasoning_effort: Option<&str>) -> Option<ReasoningEffortLevel> {
    current_reasoning_effort
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(ReasoningEffortLevel::parse)
}

pub(super) fn parseable_subagent_dynamic_indexes(dynamic_models: &DynamicModelRegistry) -> Vec<usize> {
    dynamic_models
        .entries
        .iter()
        .enumerate()
        .filter_map(|(index, detail)| detail.model_id.parse::<ModelId>().ok().map(|_| index))
        .collect()
}

pub(super) fn subagent_reasoning_levels(model: &str, supports_reasoning: bool) -> Vec<ReasoningEffortLevel> {
    if !supports_reasoning {
        return Vec::new();
    }

    let mut levels = vec![ReasoningEffortLevel::None];
    if let Some(resolved) = ModelResolver::resolve(None, model, &[], None) {
        levels.extend(
            resolved
                .supported_reasoning_efforts()
                .iter()
                .filter_map(|level| ReasoningEffortLevel::parse(level)),
        );
    } else {
        // Shortcut aliases and unlisted custom routes retain the historical
        // generic effort contract until their concrete provider is resolved.
        levels.extend([
            ReasoningEffortLevel::Low,
            ReasoningEffortLevel::Medium,
            ReasoningEffortLevel::High,
        ]);
    }
    if levels.len() == 1 && is_subagent_shortcut(model) {
        levels.extend([
            ReasoningEffortLevel::Low,
            ReasoningEffortLevel::Medium,
            ReasoningEffortLevel::High,
        ]);
    }
    levels.sort_unstable_by_key(|level| match level {
        ReasoningEffortLevel::None => 0,
        ReasoningEffortLevel::Minimal => 1,
        ReasoningEffortLevel::Low => 2,
        ReasoningEffortLevel::Medium => 3,
        ReasoningEffortLevel::High => 4,
        ReasoningEffortLevel::XHigh => 5,
        ReasoningEffortLevel::Max => 6,
        ReasoningEffortLevel::Unknown => usize::MAX,
    });
    levels.dedup();
    levels
}

fn subagent_supports_reasoning_level(target: &SubagentModelTarget, level: ReasoningEffortLevel) -> bool {
    if !target.supports_reasoning() {
        return false;
    }

    if level == ReasoningEffortLevel::Unknown {
        return false;
    }
    subagent_reasoning_levels(target.model(), true).contains(&level)
}

async fn wait_for_inline_list_selection(
    handle: &InlineHandle,
    session: &mut InlineSession,
    ctrl_c_state: &Arc<CtrlCState>,
    ctrl_c_notify: &Arc<Notify>,
) -> Result<Option<InlineListSelection>> {
    let outcome =
        wait_for_overlay_submission(handle, session, ctrl_c_state, ctrl_c_notify, |submission| match submission {
            TransientSubmission::Selection(selection) => Some(selection),
            _ => None,
        })
        .await?;

    handle.close_modal();
    handle.force_redraw();
    task::yield_now().await;

    Ok(match outcome {
        OverlayWaitOutcome::Submitted(selection) => Some(selection),
        OverlayWaitOutcome::Cancelled
        | OverlayWaitOutcome::Interrupted
        | OverlayWaitOutcome::Deferred
        | OverlayWaitOutcome::Exit => None,
    })
}

impl SubagentModelTarget {
    fn model(&self) -> &str {
        match self {
            Self::Shortcut { model } => model,
            Self::Concrete(detail) => &detail.model_id,
        }
    }

    fn supports_reasoning(&self) -> bool {
        match self {
            Self::Shortcut { .. } => true,
            Self::Concrete(detail) => detail.reasoning_supported,
        }
    }
}
