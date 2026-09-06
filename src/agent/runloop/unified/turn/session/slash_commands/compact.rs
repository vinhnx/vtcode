use std::fs;

use anyhow::{Context, Result};
use tempfile::Builder as TempFileBuilder;
use toml::Value as TomlValue;
use vtcode_core::config::loader::ConfigManager;
use vtcode_core::tools::terminal_app::{EditorLaunchConfig, TerminalAppLauncher};
use vtcode_core::utils::ansi::MessageStyle;

use crate::agent::runloop::slash_commands::CompactConversationCommand;
use crate::agent::runloop::unified::palettes::refresh_runtime_config_from_manager;

use super::config_toml::{ensure_child_table, load_toml_value, preferred_workspace_config_path, save_toml_value};
use super::{SlashCommandContext, SlashCommandControl};
use crate::agent::runloop::unified::external_editor::run_blocking_with_event_loop_suspended;

pub(crate) async fn handle_compact_conversation(
    mut ctx: SlashCommandContext<'_>,
    command: CompactConversationCommand,
) -> Result<SlashCommandControl> {
    match command {
        CompactConversationCommand::Run { options, native_only } => {
            if native_only && !manual_compaction_available(&mut ctx)? {
                return Ok(SlashCommandControl::Continue);
            }
            execute_manual_compaction(&mut ctx, options, native_only).await
        }
        CompactConversationCommand::EditDefaultPrompt => {
            edit_default_prompt(&mut ctx).await?;
            Ok(SlashCommandControl::Continue)
        }
        CompactConversationCommand::ResetDefaultPrompt => {
            reset_default_prompt(&mut ctx).await?;
            Ok(SlashCommandControl::Continue)
        }
    }
}

async fn edit_default_prompt(ctx: &mut SlashCommandContext<'_>) -> Result<()> {
    let editor_config = ctx
        .vt_cfg
        .as_ref()
        .map(|config| config.tools.editor.clone())
        .unwrap_or_default();
    if !editor_config.enabled {
        ctx.renderer
            .line(MessageStyle::Warning, "External editor is disabled (`tools.editor.enabled = false`).")?;
        return Ok(());
    }

    let initial_prompt = current_default_prompt(ctx).unwrap_or_default();
    let temp_file = TempFileBuilder::new()
        .prefix("vtcode-compact-prompt-")
        .suffix(".md")
        .tempfile_in(&ctx.config.workspace)
        .context("Failed to create temporary prompt file")?;
    fs::write(temp_file.path(), initial_prompt).context("Failed to seed temporary prompt file")?;

    let launcher = TerminalAppLauncher::new(ctx.config.workspace.clone());
    let launch_config = EditorLaunchConfig {
        preferred_editor: if editor_config.preferred_editor.trim().is_empty() {
            None
        } else {
            Some(editor_config.preferred_editor.clone())
        },
        wait_for_editor: true,
    };
    let temp_path = temp_file.path().to_path_buf();

    run_blocking_with_event_loop_suspended(ctx.handle, editor_config.suspend_tui, move || {
        launcher.launch_editor_with_config(Some(temp_path.clone()), launch_config)
    })
    .await
    .context("Failed to launch editor")?;

    let edited = fs::read_to_string(temp_file.path()).context("Failed to read edited compaction prompt")?;
    persist_default_prompt(ctx, trimmed_optional(edited)).await?;
    ctx.renderer
        .line(MessageStyle::Info, "Saved workspace default manual compaction prompt.")?;
    Ok(())
}

async fn reset_default_prompt(ctx: &mut SlashCommandContext<'_>) -> Result<()> {
    persist_default_prompt(ctx, None).await?;
    ctx.renderer
        .line(MessageStyle::Info, "Reset workspace default manual compaction prompt.")?;
    Ok(())
}

async fn persist_default_prompt(ctx: &mut SlashCommandContext<'_>, value: Option<String>) -> Result<()> {
    let manager =
        ConfigManager::load_from_workspace(&ctx.config.workspace).context("Failed to load VT Code configuration")?;
    let workspace_config_path = preferred_workspace_config_path(&manager, &ctx.config.workspace);
    let mut root = load_toml_value(&workspace_config_path)?;
    let root_table = root.as_table_mut().context("Workspace config root is not a TOML table")?;
    set_manual_compaction_prompt(root_table, value);
    save_toml_value(&workspace_config_path, &root)?;
    refresh_runtime_config_from_manager(
        ctx.renderer,
        ctx.handle,
        ctx.config,
        ctx.vt_cfg,
        ctx.provider_client.as_ref(),
        ctx.session_bootstrap,
        ctx.full_auto,
    )
    .await
}

async fn execute_manual_compaction(
    ctx: &mut SlashCommandContext<'_>,
    options: vtcode_core::compaction::ManualCompactionOptions,
    native_only: bool,
) -> Result<SlashCommandControl> {
    if ctx.conversation_history.is_empty() {
        ctx.renderer.line(MessageStyle::Info, "No conversation history to compact.")?;
        return Ok(SlashCommandControl::Continue);
    }

    let resolved_options = resolve_manual_compaction_options(ctx, options);
    let harness_snapshot = ctx.tool_registry.harness_context_snapshot();
    let outcome = crate::agent::runloop::unified::turn::compaction::manual_compact_history_in_place(
        crate::agent::runloop::unified::turn::compaction::CompactionContext::new(
            ctx.provider_client.as_ref(),
            &ctx.config.model,
            &harness_snapshot.session_id,
            ctx.thread_id,
            &ctx.config.workspace,
            ctx.vt_cfg.as_ref(),
            ctx.lifecycle_hooks,
            ctx.harness_emitter,
        ),
        crate::agent::runloop::unified::turn::compaction::CompactionState::new(
            ctx.conversation_history,
            ctx.session_stats,
            ctx.context_manager,
        ),
        &resolved_options,
        native_only,
    )
    .await;

    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(err) => {
            ctx.renderer.line(MessageStyle::Error, &format!("Compaction failed: {err}"))?;
            return Ok(SlashCommandControl::Continue);
        }
    };

    let Some(outcome) = outcome else {
        ctx.renderer.line(MessageStyle::Info, "Conversation is already compact.")?;
        return Ok(SlashCommandControl::Continue);
    };

    ctx.session_stats.auto_compact_suppressed = vtcode_core::compaction::SUPPRESS_NONE;

    ctx.renderer.line(
        MessageStyle::Info,
        &format!(
            "Compacted conversation history ({} -> {} messages, {} compaction).",
            outcome.original_len,
            outcome.compacted_len,
            outcome.mode.as_str()
        ),
    )?;
    Ok(SlashCommandControl::Continue)
}

fn resolve_manual_compaction_options(
    ctx: &SlashCommandContext<'_>,
    options: vtcode_core::compaction::ManualCompactionOptions,
) -> vtcode_core::compaction::ManualCompactionOptions {
    let default_prompt = current_default_prompt(ctx);
    vtcode_core::compaction::ManualCompactionOptions {
        instructions: options.instructions.and_then(trimmed_optional).or(default_prompt),
        max_output_tokens: options.max_output_tokens,
        reasoning_effort: options.reasoning_effort,
        allow_reasoning_effort_downgrade: options.allow_reasoning_effort_downgrade,
        verbosity: options.verbosity,
    }
}

fn current_default_prompt(ctx: &SlashCommandContext<'_>) -> Option<String> {
    ctx.vt_cfg
        .as_ref()
        .and_then(|cfg| cfg.provider.openai.manual_compaction.instructions.clone())
        .and_then(trimmed_optional)
}

fn manual_compaction_available(ctx: &mut SlashCommandContext<'_>) -> Result<bool> {
    if ctx.provider_client.supports_manual_openai_compaction(&ctx.config.model) {
        return Ok(true);
    }

    let message = ctx
        .provider_client
        .manual_openai_compaction_unavailable_message(&ctx.config.model);
    ctx.renderer.line(MessageStyle::Error, &message)?;
    Ok(false)
}

fn trimmed_optional(value: String) -> Option<String> {
    let trimmed = value.trim().to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn set_manual_compaction_prompt(root_table: &mut toml::map::Map<String, TomlValue>, value: Option<String>) {
    match value {
        Some(value) => {
            let provider_table = ensure_child_table(root_table, "provider");
            let openai_table = ensure_child_table(provider_table, "openai");
            let manual_table = ensure_child_table(openai_table, "manual_compaction");
            manual_table.insert("instructions".to_string(), TomlValue::String(value));
        }
        None => {
            let remove_openai_table = {
                let provider_table = ensure_child_table(root_table, "provider");
                let openai_table = ensure_child_table(provider_table, "openai");
                let remove_manual_table = {
                    let manual_table = ensure_child_table(openai_table, "manual_compaction");
                    manual_table.remove("instructions");
                    manual_table.is_empty()
                };
                if remove_manual_table {
                    openai_table.remove("manual_compaction");
                }
                openai_table.is_empty()
            };
            if remove_openai_table {
                let remove_provider_table = {
                    let provider_table = ensure_child_table(root_table, "provider");
                    provider_table.remove("openai");
                    provider_table.is_empty()
                };
                if remove_provider_table {
                    root_table.remove("provider");
                }
            }
        }
    }
}
