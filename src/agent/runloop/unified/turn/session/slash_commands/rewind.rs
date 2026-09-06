use crate::agent::runloop::unified::reasoning::model_supports_reasoning;
use crate::agent::runloop::unified::turn::session::slash_commands::{SlashCommandContext, SlashCommandControl};
use anyhow::Result;
use vtcode_core::core::agent::snapshots::{CheckpointRestore, RevertScope, SnapshotManager, SnapshotMetadata};
use vtcode_core::llm::provider as uni;
use vtcode_core::utils::ansi::{AnsiRenderer, MessageStyle};
use vtcode_ui::tui::app::InlineHandle;

#[cfg(test)]
fn resolve_prompt_boundary_in_history(metadata: &SnapshotMetadata, history: &[uni::Message]) -> Option<usize> {
    let prompt_text = metadata.prompt_text.as_deref().map(str::trim).filter(|value| !value.is_empty());

    if let Some(index) = metadata.prompt_message_index.filter(|index| *index < history.len()) {
        let message = &history[index];
        let matches_prompt = prompt_text.is_none_or(|text| message.content.as_text().trim() == text);
        if message.role == uni::MessageRole::User && matches_prompt {
            return Some(index);
        }
    }

    prompt_text.and_then(|prompt_text| {
        history
            .iter()
            .enumerate()
            .filter(|(_, message)| {
                message.role == uni::MessageRole::User && message.content.as_text().trim() == prompt_text
            })
            .min_by_key(|(index, _)| {
                metadata
                    .prompt_message_index
                    .map_or(usize::MAX / 2, |target| target.abs_diff(*index))
            })
            .map(|(index, _)| index)
    })
}

fn restore_prompt_input(
    handle: &InlineHandle,
    metadata: &SnapshotMetadata,
    conversation: &[vtcode_core::utils::session_archive::SessionMessage],
) -> bool {
    let Some(prompt) = metadata.resolved_prompt_text(conversation) else {
        return false;
    };
    handle.set_input(prompt.to_string());
    handle.force_redraw();
    true
}

fn restore_prompt_input_and_report(
    renderer: &mut AnsiRenderer,
    handle: &InlineHandle,
    metadata: &SnapshotMetadata,
    conversation: &[vtcode_core::utils::session_archive::SessionMessage],
) -> Result<()> {
    if restore_prompt_input(handle, metadata, conversation) {
        renderer.line(MessageStyle::Info, "Restored the selected prompt into the input field.")?;
    }
    Ok(())
}

pub(crate) async fn handle_rewind_latest(
    ctx: SlashCommandContext<'_>,
    scope: RevertScope,
) -> Result<SlashCommandControl> {
    let Some(manager) = ctx.checkpoint_manager else {
        ctx.renderer
            .line(MessageStyle::Info, "In-chat rewind requires access to the checkpoint manager.")?;
        return Ok(SlashCommandControl::Continue);
    };

    let snapshots = match manager.list_snapshots().await {
        Ok(snapshots) => snapshots,
        Err(err) => {
            ctx.renderer
                .line(MessageStyle::Error, &format!("Failed to list checkpoints: {err}"))?;
            return Ok(SlashCommandControl::Continue);
        }
    };

    let Some(latest) = snapshots.first() else {
        ctx.renderer
            .line(MessageStyle::Warning, "No checkpoints available to rewind.")?;
        return Ok(SlashCommandControl::Continue);
    };

    handle_rewind_to_turn(ctx, latest.turn_number, scope).await
}

pub(crate) async fn handle_rewind_to_turn(
    ctx: SlashCommandContext<'_>,
    turn: usize,
    scope: RevertScope,
) -> Result<SlashCommandControl> {
    if let Some(manager) = ctx.checkpoint_manager {
        let supports_reasoning = model_supports_reasoning(&**ctx.provider_client, &ctx.config.model);
        let result = restore_rewind_from_checkpoint(
            ctx.renderer,
            ctx.handle,
            ctx.conversation_history,
            manager,
            turn,
            scope,
            supports_reasoning,
        )
        .await;
        if result.is_ok() {
            if let Some(emitter) = ctx.harness_emitter {
                let _ = emitter.emit(crate::agent::runloop::unified::inline_events::harness::harness_event(
                    vtcode_core::exec::events::HarnessEventKind::SnapshotRestored,
                    Some(format!("Rewound to turn {turn}")),
                    None,
                    None,
                    None,
                ));
            }
        }
        result?;
    } else {
        render_rewind_cli_guidance(ctx.renderer, turn, scope)?;
    }

    Ok(SlashCommandControl::Continue)
}

async fn restore_rewind_from_checkpoint(
    renderer: &mut AnsiRenderer,
    handle: &InlineHandle,
    conversation_history: &mut Vec<uni::Message>,
    manager: &SnapshotManager,
    turn: usize,
    scope: RevertScope,
    supports_reasoning: bool,
) -> Result<()> {
    match manager.restore_snapshot(turn, scope).await {
        Ok(Some(restored)) => render_rewind_restore_success(
            renderer,
            handle,
            conversation_history,
            turn,
            scope,
            restored,
            supports_reasoning,
        ),
        Ok(None) => renderer.line(MessageStyle::Error, &format!("No checkpoint found for turn {turn}")),
        Err(err) => renderer.line(MessageStyle::Error, &format!("Failed to restore checkpoint for turn {turn}: {err}")),
    }
}

fn render_rewind_restore_success(
    renderer: &mut AnsiRenderer,
    handle: &InlineHandle,
    conversation_history: &mut Vec<uni::Message>,
    turn: usize,
    scope: RevertScope,
    restored: CheckpointRestore,
    supports_reasoning: bool,
) -> Result<()> {
    if scope.includes_conversation() {
        *conversation_history = restored.conversation.iter().map(uni::Message::from).collect();

        renderer.clear_screen();
        let resume_lines = crate::agent::runloop::unified::session_setup::build_structured_resume_lines(
            conversation_history,
            supports_reasoning,
        );
        crate::agent::runloop::unified::session_setup::render_resume_lines(renderer, &resume_lines)?;

        renderer.line(
            MessageStyle::Info,
            &format!("Restored conversation history from turn {} ({} messages)", turn, restored.conversation.len()),
        )?;
        restore_prompt_input_and_report(renderer, handle, &restored.metadata, &restored.conversation)?;
    }

    if scope.includes_code() {
        renderer.line(MessageStyle::Info, &format!("Applied code changes from turn {turn}"))?;
    }

    renderer.line(MessageStyle::Info, &format!("Successfully rewound to turn {turn} with scope {scope:?}"))?;
    Ok(())
}

fn render_rewind_cli_guidance(renderer: &mut AnsiRenderer, turn: usize, scope: RevertScope) -> Result<()> {
    renderer.line(MessageStyle::Info, &format!("Rewinding to turn {turn} with scope {scope:?}..."))?;
    renderer.line(
        MessageStyle::Info,
        &format!("Use: `vtcode revert --turn {} --partial {}` from command line", turn, rewind_partial_arg(scope)),
    )?;
    renderer.line(MessageStyle::Info, "Note: In-chat rewind requires access to the checkpoint manager.")?;
    Ok(())
}

fn rewind_partial_arg(scope: RevertScope) -> &'static str {
    match scope {
        RevertScope::Conversation => "conversation",
        RevertScope::Code => "code",
        RevertScope::Both => "both",
    }
}

#[cfg(test)]
mod tests {
    use super::{resolve_prompt_boundary_in_history, rewind_partial_arg};
    use vtcode_core::core::agent::snapshots::{RevertScope, SnapshotMetadata};

    #[test]
    fn rewind_partial_arg_matches_cli_scope_values() {
        assert_eq!(rewind_partial_arg(RevertScope::Conversation), "conversation");
        assert_eq!(rewind_partial_arg(RevertScope::Code), "code");
        assert_eq!(rewind_partial_arg(RevertScope::Both), "both");
    }

    #[test]
    fn resolve_prompt_boundary_prefers_metadata_index_when_it_matches() {
        let history = vec![
            vtcode_core::llm::provider::Message::user("first".to_string()),
            vtcode_core::llm::provider::Message::assistant("reply".to_string()),
            vtcode_core::llm::provider::Message::user("target".to_string()),
        ];
        let metadata = SnapshotMetadata {
            id: "turn_2".to_string(),
            turn_number: 2,
            created_at: 0,
            description: "target".to_string(),
            message_count: 3,
            file_count: 0,
            touched_files: Vec::new(),
            prompt_text: Some("target".to_string()),
            prompt_message_index: Some(2),
            session_id: None,
            runtime_turn_id: None,
            session_turn_number: None,
            turn_diagnostics: None,
        };

        assert_eq!(resolve_prompt_boundary_in_history(&metadata, &history), Some(2));
    }

    #[test]
    fn resolve_prompt_boundary_falls_back_to_nearest_prompt_match() {
        let history = vec![
            vtcode_core::llm::provider::Message::user("target".to_string()),
            vtcode_core::llm::provider::Message::assistant("reply".to_string()),
            vtcode_core::llm::provider::Message::user("target".to_string()),
        ];
        let metadata = SnapshotMetadata {
            id: "turn_2".to_string(),
            turn_number: 2,
            created_at: 0,
            description: "target".to_string(),
            message_count: 3,
            file_count: 0,
            touched_files: Vec::new(),
            prompt_text: Some("target".to_string()),
            prompt_message_index: Some(2),
            session_id: None,
            runtime_turn_id: None,
            session_turn_number: None,
            turn_diagnostics: None,
        };

        assert_eq!(resolve_prompt_boundary_in_history(&metadata, &history), Some(2));
    }
}
