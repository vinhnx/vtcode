use super::*;
#[cfg_attr(not(vendored_crossterm), allow(unused_imports))]
use vtcode_commons::ansi_capabilities::ColorScheme;

impl Session {
    pub(crate) fn cursor(&self) -> usize {
        self.input_manager.cursor()
    }

    pub(crate) fn set_input(&mut self, text: impl Into<String>) {
        self.input_manager.set_content(text.into());
        self.input_compact_mode = self.input_compact_placeholder().is_some();
        self.mark_dirty();
    }

    pub(crate) fn set_cursor(&mut self, pos: usize) {
        self.input_manager.set_cursor(pos);
        self.mark_dirty();
    }

    pub(crate) fn process_key(&mut self, key: KeyEvent) -> Option<InlineEvent> {
        events::process_key(self, key)
    }

    pub fn handle_command(&mut self, command: InlineCommand) {
        let mut command_needs_redraw = true;

        // Track streaming state: set when agent starts responding
        if matches!(
            &command,
            InlineCommand::AppendLine { kind: InlineMessageKind::Agent, segments }
                if !segments.is_empty()
        ) || matches!(
            &command,
            InlineCommand::AppendPastedMessage { kind: InlineMessageKind::Agent, text, .. }
                if !text.is_empty()
        ) || matches!(
            &command,
            InlineCommand::Inline { kind: InlineMessageKind::Agent, segment }
                if !segment.text.is_empty()
        ) {
            self.is_streaming_final_answer = true;
        }

        // Clear streaming state on turn completion (status cleared)
        if let InlineCommand::SetInputStatus { left, right } = &command
            && self.is_streaming_final_answer
            && left.is_none()
            && right.is_none()
        {
            self.is_streaming_final_answer = false;
        }

        match command {
            InlineCommand::AppendLine { kind, segments } => {
                let previous_max_offset = self.current_max_scroll_offset();
                self.clear_thinking_spinner_if_active(kind);
                self.push_line(kind, segments);
                self.adjust_scroll_after_change(previous_max_offset);
                self.request_transcript_clear();
            }
            InlineCommand::AppendPastedMessage { kind, text, line_count } => {
                self.clear_thinking_spinner_if_active(kind);
                self.append_pasted_message(kind, text, line_count);
                self.request_transcript_clear();
            }
            InlineCommand::Inline { kind, segment } => {
                self.clear_thinking_spinner_if_active(kind);
                self.append_inline(kind, segment);
                self.request_transcript_clear();
            }
            InlineCommand::ReplaceLast { count, kind, lines, link_ranges } => {
                self.clear_thinking_spinner_if_active(kind);
                self.replace_last(count, kind, lines, link_ranges);
                self.request_transcript_clear();
            }
            InlineCommand::SetPrompt { prefix, style } => {
                self.prompt_prefix = prefix;
                self.prompt_style = style;
                self.ensure_prompt_style_color();
            }
            InlineCommand::SetPlaceholder { hint, style } => {
                self.placeholder = hint;
                self.placeholder_style = style;
            }
            InlineCommand::SetMessageLabels { agent, user } => {
                self.labels.agent = agent.filter(|label| !label.is_empty());
                self.labels.user = user.filter(|label| !label.is_empty());
                self.invalidate_transcript_cache();
                self.invalidate_scroll_metrics();
            }
            InlineCommand::SetHeaderContext { context } => {
                let mut next_context = *context;
                next_context.reasoning_stage = self.header_context.reasoning_stage.clone();
                next_context.primary_agent = self.header_context.primary_agent.clone();
                next_context.primary_agent_color = self.header_context.primary_agent_color.clone();
                self.header_context = next_context;
                self.invalidate_header_cache();
            }
            InlineCommand::SetInputStatus { left, right } => {
                self.input_status_left = left;
                self.input_status_right = right;
                if self.thinking_spinner.is_active {
                    self.thinking_spinner.stop();
                }
                self.needs_redraw = true;
            }
            InlineCommand::SetActivityState(state) => {
                self.activity_state = state;
                self.input_status_left = state.status().map(ToOwned::to_owned);
                let enabled = !state.is_busy();
                self.set_input_enabled(enabled);
                if let Some(ActiveOverlay::Modal(overlay)) = self.active_overlay.as_mut() {
                    overlay.restore_cursor = enabled;
                }
                self.cursor_visible = enabled && !self.has_active_overlay();
                self.needs_redraw = true;
            }
            InlineCommand::SetTerminalTitleItems { items } => {
                if self.terminal_title_items != items {
                    self.terminal_title_items = items;
                    self.needs_redraw = true;
                } else {
                    command_needs_redraw = false;
                }
            }
            InlineCommand::SetTerminalTitleThreadLabel { label } => {
                let label = label.filter(|value| !value.trim().is_empty());
                if self.terminal_title_thread_label != label {
                    self.terminal_title_thread_label = label;
                    self.needs_redraw = true;
                } else {
                    command_needs_redraw = false;
                }
            }
            InlineCommand::SetTerminalTitleGitBranch { branch } => {
                let branch = branch.filter(|value| !value.trim().is_empty());
                if self.terminal_title_git_branch != branch {
                    self.terminal_title_git_branch = branch;
                    self.needs_redraw = true;
                } else {
                    command_needs_redraw = false;
                }
            }
            InlineCommand::SetTheme { theme } => {
                let previous_theme = self.theme.clone();
                self.theme = theme.clone();
                self.styles.set_theme(theme);
                self.retint_lines_for_theme_change(&previous_theme);
                self.ensure_prompt_style_color();
                self.invalidate_transcript_cache();
            }
            InlineCommand::SetColorSchemeAuto { enabled } => {
                self.auto_color_scheme = enabled;
                command_needs_redraw = false;
            }
            InlineCommand::SetAppearance { appearance } => {
                self.appearance = appearance;
                self.invalidate_header_cache();
                self.invalidate_transcript_cache();
                self.invalidate_scroll_metrics();
            }
            InlineCommand::SetVimModeEnabled(enabled) => {
                self.vim_state.set_enabled(enabled);
                self.needs_redraw = true;
            }
            InlineCommand::SetQueuedInputs { entries } => {
                self.set_queued_inputs_entries(entries);
                self.mark_visual_dirty();
            }
            InlineCommand::SetSubprocessEntries { entries } => {
                self.subprocess_entries = entries;
                self.invalidate_sidebar_cache();
            }
            InlineCommand::SetSubagentPreview { text } => {
                self.subagent_preview = text.filter(|value| !value.trim().is_empty());
                self.invalidate_sidebar_cache();
            }
            InlineCommand::SetPrimaryAgent { name, color } => {
                self.header_context.primary_agent = name.filter(|value| !value.trim().is_empty());
                self.header_context.primary_agent_color = color.filter(|value| !value.trim().is_empty());
                self.invalidate_header_cache();
            }
            InlineCommand::SetCursorVisible(value) => {
                self.cursor_visible = value;
            }
            InlineCommand::SetInputEnabled(value) => {
                self.set_input_enabled(value);
            }
            InlineCommand::SetImageInputEnabled(value) => {
                self.image_input_enabled = value;
            }
            InlineCommand::SetInput(content) => {
                // Check if the content appears to be an error message
                // If it looks like an error, redirect to transcript instead
                if Self::is_error_content(&content) {
                    // Add error to transcript instead of input field
                    crate::tui::utils::transcript::display_error(&content);
                } else {
                    self.clear_suggested_prompt_state();
                    self.clear_inline_prompt_suggestion();
                    self.input_manager.set_content(content);
                    self.input_compact_mode = self.input_compact_placeholder().is_some();
                    self.scroll_manager.set_offset(0);
                }
            }
            InlineCommand::RestoreInputDraft(input) => {
                self.clear_suggested_prompt_state();
                self.clear_inline_prompt_suggestion();
                self.input_manager.set_content(input.text);
                self.input_manager.set_attachments(input.attachments);
                self.input_compact_mode = self.input_compact_placeholder().is_some();
                self.scroll_manager.set_offset(0);
            }
            InlineCommand::ApplySuggestedPrompt(content) => {
                self.apply_suggested_prompt(content);
                self.scroll_manager.set_offset(0);
            }
            InlineCommand::SetInlinePromptSuggestion { suggestion, llm_generated } => {
                self.set_inline_prompt_suggestion(suggestion, llm_generated);
            }
            InlineCommand::ClearInlinePromptSuggestion => {
                self.clear_inline_prompt_suggestion();
            }
            InlineCommand::ClearInput => {
                command::clear_input(self);
            }
            InlineCommand::ForceRedraw => {
                self.mark_dirty();
            }
            InlineCommand::ShowOverlay { request } => {
                self.clear_inline_prompt_suggestion();
                self.show_overlay(*request);
            }
            InlineCommand::CloseOverlay => {
                self.close_overlay();
            }
            InlineCommand::ClearScreen => {
                self.clear_screen();
            }
            InlineCommand::SuspendEventLoop
            | InlineCommand::ResumeEventLoop
            | InlineCommand::ClearInputQueue
            | InlineCommand::StopEventStream
            | InlineCommand::StartEventStream => {
                // Handled by drive_terminal
            }
            InlineCommand::SetSkipConfirmations(skip) => {
                if skip {
                    // Permission bypass is handled by the runtime. Close only
                    // an approval overlay that was already open; user-started
                    // selection modals must remain interactive after this.
                    self.close_overlay();
                }
            }
            InlineCommand::Shutdown => {
                self.request_exit();
            }
            InlineCommand::SetReasoningStage(stage) => {
                self.header_context.reasoning_stage = stage;
                self.invalidate_header_cache();
            }
        }
        if command_needs_redraw {
            self.needs_redraw = true;
        }
    }

    /// Apply an unsolicited terminal color-scheme report (Contour VT extension,
    /// `CSI ? 997 ; Ps n`). No-op unless automatic color-scheme following is
    /// enabled via `SetColorSchemeAuto`, which the host wires from
    /// `ui.color_scheme_mode = "auto"`.
    ///
    /// The shared scheme override is updated first so theme matching and
    /// suggestions agree with the reported scheme; the switch itself goes
    /// through the regular `SetTheme` path so retinting and caches stay
    /// consistent. The user's persisted theme preference is never touched.
    #[cfg_attr(not(vendored_crossterm), allow(dead_code))]
    pub(crate) fn apply_terminal_color_scheme_report(&mut self, dark: bool) {
        if !self.auto_color_scheme {
            return;
        }
        let scheme = if dark { ColorScheme::Dark } else { ColorScheme::Light };
        vtcode_commons::ansi_capabilities::set_color_scheme_override(Some(scheme));
        let active = crate::theme::active_theme_id();
        if crate::theme::theme_matches_terminal_scheme(&active) {
            return;
        }
        let next = crate::theme::theme_for_terminal_scheme_change(&active, dark);
        if crate::theme::set_active_theme(next).is_err() {
            return;
        }
        let theme = crate::tui::core_tui::style::theme_from_styles(&crate::theme::active_styles());
        self.handle_command(InlineCommand::SetTheme { theme });
    }
}
