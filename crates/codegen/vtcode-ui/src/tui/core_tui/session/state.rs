/// State management and lifecycle operations for Session
///
/// This module handles session state including:
/// - Session initialization and configuration
/// - Exit management
/// - Redraw and dirty state tracking
/// - Screen clearing
/// - Modal management
/// - Timeline pane toggling
/// - Scroll management operations
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use ratatui::layout::Rect;
use tokio::sync::mpsc::UnboundedSender;

use super::super::types::{
    InlineEvent, InlineListSelection, ListOverlayRequest, LocalAgentEntry, LocalAgentKind, ModalOverlayRequest,
    OverlayRequest, WizardOverlayRequest,
};
use super::mouse_selection::MouseSelectionState;
use super::reflow::is_info_box_line;
use super::status_requires_shimmer;
use super::{
    ActiveOverlay, InlinePromptSuggestionState, Session, SuggestedPromptState,
    modal::{ModalListState, ModalSearchState, ModalState, WizardModalState},
};
use crate::tui::config::constants::ui;
use crate::tui::options::FullscreenInteractionSettings;

const COPY_NOTIFICATION_DURATION: Duration = Duration::from_secs(5);
const COPY_NOTIFICATION_TEXT: &str = "Copied to clipboard";
const COPY_FAILURE_NOTIFICATION_TEXT: &str = "Copy failed";
const ACTION_REQUIRED_STATUS_TEXT: &str = "Action required";
const APPROVAL_REQUIRED_STATUS_TEXT: &str = "Approval required";
const INPUT_REQUIRED_STATUS_TEXT: &str = "Input required";
const ACTIVE_PTY_STATUS_TEXT: &str = "Running PTY command...";

impl Session {
    pub(crate) fn set_task_panel_lines(&mut self, lines: Vec<String>) {
        self.terminal_title_task_progress = extract_task_progress(&lines);
        self.mark_dirty();
    }

    pub(crate) fn clear_inline_prompt_suggestion(&mut self) {
        if self.inline_prompt_suggestion.suggestion.is_none() {
            return;
        }
        self.inline_prompt_suggestion = InlinePromptSuggestionState::default();
        self.mark_dirty();
    }

    pub(crate) fn copy_input_selection_to_clipboard(&mut self) -> bool {
        if self.input_manager.selected_text().is_none() {
            return false;
        }

        // Swallow the key even on hard failure so Ctrl+C never degrades into an
        // interrupt while the user is trying to copy a selection.
        if self.input_manager.copy_selected_text_to_clipboard() {
            self.show_copy_notification();
        } else {
            self.show_copy_failure_notification();
        }
        true
    }

    pub(crate) fn copy_text_to_clipboard(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        if MouseSelectionState::copy_to_clipboard(text) {
            self.show_copy_notification();
        } else {
            self.show_copy_failure_notification();
        }
    }

    pub(crate) fn clear_suggested_prompt_state(&mut self) {
        if !self.suggested_prompt_state.active {
            return;
        }
        self.suggested_prompt_state = SuggestedPromptState::default();
        self.mark_dirty();
    }

    /// Get the next revision counter for message tracking
    pub(crate) fn next_revision(&mut self) -> u64 {
        self.line_revision_counter = self.line_revision_counter.wrapping_add(1);
        self.line_revision_counter
    }

    /// Check if the session should exit
    pub(crate) fn should_exit(&self) -> bool {
        self.should_exit
    }

    /// Request session exit
    pub(crate) fn request_exit(&mut self) {
        self.should_exit = true;
    }

    /// Take the redraw flag and reset it
    ///
    /// Returns true if a redraw was needed
    pub(crate) fn take_redraw(&mut self) -> bool {
        if self.needs_redraw {
            self.needs_redraw = false;
            true
        } else {
            false
        }
    }

    /// Mark the session as needing a redraw (visual-only, no cache invalidation)
    ///
    /// Use this for changes that only affect the visual output without changing
    /// content data: cursor movement, scroll position, hover state, mouse selection.
    pub(crate) fn mark_visual_dirty(&mut self) {
        self.needs_redraw = true;
    }

    /// Mark the session as needing a redraw with full cache invalidation
    ///
    /// Use this for changes that affect content data: new messages, text changes,
    /// config changes, queue updates. Clears header, sidebar, and subprocess caches.
    pub(crate) fn mark_dirty(&mut self) {
        self.needs_redraw = true;
        self.header_lines_cache = None;
        self.header_height_cache.clear();
        self.queued_inputs_preview_cache = None;
        self.subprocess_entries_preview_cache = None;
    }

    /// Mark a specific line as dirty to optimize reflow scans, without
    /// invalidating header or sidebar caches. Use this for transcript-only
    /// changes (e.g. streaming chunks) where chrome hasn't changed.
    pub(crate) fn mark_transcript_line_dirty(&mut self, index: usize) {
        let index = self.reflow_dirty_index(index);
        self.first_dirty_line = match self.first_dirty_line {
            Some(current) => Some(current.min(index)),
            None => Some(index),
        };
        self.needs_redraw = true;
        self.invalidate_transcript_viewport();
    }

    /// Invalidate only the header cache (e.g. when provider/model changes)
    pub(crate) fn invalidate_header_cache(&mut self) {
        self.header_lines_cache = None;
        self.header_height_cache.clear();
        self.needs_redraw = true;
    }

    /// Invalidate only the sidebar cache (e.g. when queue changes)
    pub(crate) fn invalidate_sidebar_cache(&mut self) {
        self.queued_inputs_preview_cache = None;
        self.subprocess_entries_preview_cache = None;
        self.needs_redraw = true;
    }

    pub(crate) fn set_local_agents(&mut self, entries: Vec<LocalAgentEntry>) {
        if self.local_agents != entries {
            self.local_agents = entries;
            self.invalidate_sidebar_cache();
        }
    }

    pub(crate) fn has_delegated_local_agents(&self) -> bool {
        self.local_agents.iter().any(|entry| entry.kind == LocalAgentKind::Delegated)
    }

    pub(crate) fn set_local_agents_drawer_visible(&mut self, visible: bool) {
        if self.local_agents_drawer_visible != visible {
            self.local_agents_drawer_visible = visible;
            self.mark_dirty();
        }
    }

    pub(crate) fn set_transcript_area(&mut self, area: Option<Rect>) {
        self.transcript_area = area;
    }

    pub(crate) fn transcript_area(&self) -> Option<Rect> {
        self.transcript_area
    }

    pub(crate) fn set_input_area(&mut self, area: Option<Rect>) {
        self.input_area = area;
    }

    pub(crate) fn input_area(&self) -> Option<Rect> {
        self.input_area
    }

    pub(crate) fn set_bottom_panel_area(&mut self, area: Option<Rect>) {
        self.bottom_panel_area = area;
    }

    pub(crate) fn bottom_panel_area(&self) -> Option<Rect> {
        self.bottom_panel_area
    }

    pub(crate) fn set_modal_list_area(&mut self, area: Option<Rect>) {
        self.modal_list_area = area;
    }

    pub(crate) fn modal_list_area(&self) -> Option<Rect> {
        self.modal_list_area
    }

    pub(crate) fn set_modal_text_areas(&mut self, areas: Vec<Rect>) {
        self.modal_text_areas = areas;
    }

    pub(crate) fn modal_text_areas(&self) -> &[Rect] {
        &self.modal_text_areas
    }

    pub(crate) fn set_modal_link_targets(&mut self, targets: Vec<super::TranscriptFileLinkTarget>) {
        self.modal_link_targets = targets;
    }

    #[cfg(test)]
    pub(crate) fn modal_link_targets(&self) -> &[super::TranscriptFileLinkTarget] {
        &self.modal_link_targets
    }

    pub(crate) fn input_owner(&self) -> super::input_manager::InputOwner {
        use super::input_manager::InputOwner;
        if self.has_active_overlay() {
            InputOwner::Overlay
        } else if self.input_enabled && !self.activity_state.is_busy() {
            InputOwner::Composer
        } else {
            InputOwner::Runtime
        }
    }

    pub(crate) fn input_enabled(&self) -> bool {
        self.input_owner() == super::input_manager::InputOwner::Composer
    }

    pub(crate) fn set_input_enabled(&mut self, enabled: bool) {
        if let Some(ActiveOverlay::Modal(state)) = self.active_overlay.as_mut() {
            state.restore_input = enabled;
        }
        self.input_enabled = enabled && !self.has_active_overlay();
    }

    pub(crate) fn image_input_enabled(&self) -> bool {
        self.image_input_enabled
    }

    pub(crate) fn input_compact_mode(&self) -> bool {
        self.input_compact_mode
    }

    pub(crate) fn set_input_compact_mode(&mut self, enabled: bool) {
        self.input_compact_mode = enabled;
    }

    pub(crate) fn set_cursor_visible(&mut self, visible: bool) {
        self.cursor_visible = visible;
    }

    pub(crate) fn current_transcript_revision(&self) -> u64 {
        self.line_revision_counter
    }

    pub(crate) fn invalidate_transcript_viewport(&mut self) {
        self.visible_lines_cache = None;
    }

    pub(crate) fn request_transcript_clear(&mut self) {
        self.transcript_clear_required = true;
    }

    pub(crate) fn set_fullscreen_active(&mut self, active: bool) {
        self.fullscreen.active = active;
    }

    pub(crate) fn set_fullscreen_interaction(&mut self, config: FullscreenInteractionSettings) {
        self.fullscreen.interaction = config;
    }

    /// Advance animation state on tick and request redraw when a frame changes.
    pub(crate) fn handle_tick(&mut self) {
        let motion_reduced = self.appearance.motion_reduced();
        self.step_drag_auto_scroll();
        let mut animation_updated = false;
        if !motion_reduced && self.thinking_spinner.is_active && self.thinking_spinner.update() {
            animation_updated = true;
            // Refresh collapsed thinking summaries so the live spinner frame and
            // line count stay current during streaming instead of freezing until
            // a click. `mark_thinking_run_starts_dirty` bumps the run-start
            // revision and drops the visible-window cache; the redraw is
            // requested below via `animation_updated`.
            self.mark_thinking_run_starts_dirty();
        }
        let shimmer_active = if self.appearance.should_animate_progress_status() {
            self.is_shimmer_active()
        } else {
            false
        };
        if shimmer_active && self.shimmer_state.update() {
            animation_updated = true;
        }
        if let Some(until) = self.scroll_cursor_steady_until
            && Instant::now() >= until
        {
            self.scroll_cursor_steady_until = None;
            self.needs_redraw = true;
        }
        if let Some(until) = self.copy_notification_until
            && Instant::now() >= until
        {
            self.copy_notification_until = None;
            self.needs_redraw = true;
        }
        if self.last_shimmer_active && !shimmer_active {
            self.needs_redraw = true;
        }
        self.last_shimmer_active = shimmer_active;
        if animation_updated {
            self.needs_redraw = true;
        }
    }

    pub(crate) fn show_copy_notification(&mut self) {
        self.show_copy_result_notification(false);
    }

    pub(crate) fn show_copy_failure_notification(&mut self) {
        self.show_copy_result_notification(true);
    }

    fn show_copy_result_notification(&mut self, failed: bool) {
        self.copy_notification_until = Some(Instant::now() + COPY_NOTIFICATION_DURATION);
        self.copy_notification_failed = failed;
        self.needs_redraw = true;
    }

    pub(crate) fn copy_notification_text(&self) -> Option<&'static str> {
        self.copy_notification_until.filter(|until| Instant::now() < *until)?;
        Some(if self.copy_notification_failed {
            COPY_FAILURE_NOTIFICATION_TEXT
        } else {
            COPY_NOTIFICATION_TEXT
        })
    }

    fn overlay_attention_status_text(&self) -> Option<&'static str> {
        if let Some(modal) = self.modal_state() {
            let normalized_title = modal.title.trim().to_ascii_lowercase();

            if normalized_title.contains("input required") {
                Some(INPUT_REQUIRED_STATUS_TEXT)
            } else if normalized_title.contains("approval") || normalized_title.contains("permission") {
                Some(APPROVAL_REQUIRED_STATUS_TEXT)
            } else {
                Some(ACTION_REQUIRED_STATUS_TEXT)
            }
        } else if self.wizard_overlay().is_some() || self.has_active_overlay() {
            Some(ACTION_REQUIRED_STATUS_TEXT)
        } else {
            None
        }
    }

    pub(crate) fn status_left_text(&self) -> Option<&str> {
        let active_pty_status = self.active_pty_status_text();

        // During stage states (Planning/Building), show the live activity status
        // (e.g. "Running grep...") when the agent is actively working so the
        // footer reflects what is happening. Fall back to the stage label
        // ("Planning...") when idle between turns.
        if self.activity_state.is_stage() {
            if let Some(left) = self.input_status_left.as_deref() {
                let trimmed = left.trim();
                if !trimmed.is_empty() && status_requires_shimmer(trimmed) {
                    return Some(trimmed);
                }
            }
            if let Some(status) = active_pty_status {
                return Some(status);
            }
            return self.activity_state.status();
        }

        // Compact PTY rendering intentionally keeps the live output row hidden.
        // Keep the global PTY observer visible in the footer in that case, while
        // preserving a more specific loading/approval status when one exists.
        if self.activity_state.status().is_none()
            && active_pty_status.is_some()
            && !self.input_status_left.as_deref().is_some_and(status_requires_shimmer)
        {
            return active_pty_status;
        }

        self.activity_state
            .status()
            .or(self.input_status_left.as_deref())
            .map(str::trim)
            .filter(|value: &&str| !value.is_empty())
            .or_else(|| self.overlay_attention_status_text())
            .or(active_pty_status)
    }

    pub(crate) fn status_right_text(&self) -> Option<&str> {
        self.input_status_right
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }

    /// Status text used to decide whether the footer should animate.
    ///
    /// During a stage state (Planning/Building) the displayed text may be the
    /// stage label when idle, so the raw input status (e.g. "Running tool:
    /// ...") is inspected directly so the spinner keeps animating while tools
    /// execute even before the displayed text has been updated.
    fn animation_status_text(&self) -> &str {
        if self.activity_state.is_stage() {
            if let Some(left) = self.input_status_left.as_deref()
                && !left.trim().is_empty()
                && status_requires_shimmer(left)
            {
                return left;
            }
            self.active_pty_status_text().unwrap_or("")
        } else {
            self.status_left_text().unwrap_or("")
        }
    }

    pub(crate) fn is_running_activity(&self) -> bool {
        if self.activity_state.is_busy() {
            return true;
        }
        let running_status =
            self.appearance.should_animate_progress_status() && status_requires_shimmer(self.animation_status_text());
        let active_pty = self.active_pty_session_count() > 0;
        running_status || active_pty
    }

    pub(crate) fn active_pty_session_count(&self) -> usize {
        self.active_pty_sessions
            .as_ref()
            .map(|counter| counter.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    fn active_pty_status_text(&self) -> Option<&'static str> {
        (self.active_pty_session_count() > 0).then_some(ACTIVE_PTY_STATUS_TEXT)
    }

    pub(crate) fn has_status_spinner(&self) -> bool {
        if self.activity_state.is_busy() {
            return true;
        }
        if !self.appearance.should_animate_progress_status() {
            return false;
        }
        status_requires_shimmer(self.animation_status_text())
    }

    pub(crate) fn is_shimmer_active(&self) -> bool {
        self.has_status_spinner() || self.thinking_spinner.is_active
    }

    pub(crate) fn use_steady_cursor(&self) -> bool {
        if !self.appearance.should_animate_progress_status() {
            self.scroll_cursor_steady_until.is_some()
        } else {
            self.is_shimmer_active() || self.scroll_cursor_steady_until.is_some()
        }
    }

    pub(crate) fn mark_scrolling(&mut self) {
        let steady_duration = Duration::from_millis(ui::TUI_SCROLL_CURSOR_STEADY_MS);
        if steady_duration.is_zero() {
            self.scroll_cursor_steady_until = None;
        } else {
            self.scroll_cursor_steady_until = Some(Instant::now() + steady_duration);
        }
    }

    /// Mark a specific line as dirty to optimize reflow scans
    pub(crate) fn mark_line_dirty(&mut self, index: usize) {
        let index = self.reflow_dirty_index(index);
        self.first_dirty_line = match self.first_dirty_line {
            Some(current) => Some(current.min(index)),
            None => Some(index),
        };
        self.mark_dirty();
    }

    fn reflow_dirty_index(&mut self, index: usize) -> usize {
        // Info, warning, and error messages are rendered as one contiguous
        // block. When a later line is appended, reflow must start at the
        // block head so the cached head can see the new lines.
        let index = self.grouped_message_start(index);
        if let Some(cache) = self.transcript_cache.as_mut() {
            cache.invalidate_message(index);
        }
        index
    }

    fn grouped_message_start(&self, index: usize) -> usize {
        let Some(line) = self.lines.get(index) else {
            return index;
        };

        if !is_info_box_line(line) {
            return index;
        }

        let kind = line.kind;
        let mut start = index;
        while start > 0 && self.lines[start - 1].kind == kind && is_info_box_line(&self.lines[start - 1]) {
            start -= 1;
        }
        start
    }

    /// Ensure the prompt style has a color set
    pub(crate) fn ensure_prompt_style_color(&mut self) {
        if self.prompt_style.color.is_none() {
            self.prompt_style.color = self.theme.primary.or(self.theme.foreground);
        }
    }

    /// Clear the screen and reset scroll
    pub(crate) fn clear_screen(&mut self) {
        self.lines.clear();
        self.collapsed_pastes.clear();
        self.thinking_runs.clear();
        self.user_scrolled = false;
        self.scroll_manager.set_offset(0);
        self.invalidate_transcript_cache();
        self.invalidate_scroll_metrics();
        self.needs_full_clear = true;
        self.mark_dirty();
    }

    /// Toggle logs panel visibility
    pub(crate) fn toggle_logs(&mut self) {
        self.show_logs = !self.show_logs;
        self.invalidate_scroll_metrics();
        self.mark_dirty();
    }

    /// Show a simple modal dialog
    pub(crate) fn show_modal(
        &mut self,
        title: String,
        lines: Vec<String>,
        secure_prompt: Option<super::super::types::SecurePromptConfig>,
    ) {
        self.show_overlay(OverlayRequest::Modal(ModalOverlayRequest {
            title,
            lines,
            secure_prompt,
            is_help_modal: false,
        }));
    }

    /// Show a help modal using the ratatui-cheese Help widget.
    ///
    /// The help flag travels on the request so a queued help modal still
    /// renders as help when it activates instead of clobbering the overlay
    /// that is currently visible.
    pub(crate) fn show_help_modal(&mut self) {
        self.show_overlay(OverlayRequest::Modal(ModalOverlayRequest {
            title: "Keyboard Shortcuts".to_string(),
            lines: Vec::new(),
            secure_prompt: None,
            is_help_modal: true,
        }));
    }

    pub(crate) fn show_overlay(&mut self, request: OverlayRequest) {
        if self.has_active_overlay() {
            self.overlay_queue.push_back(request);
            self.mark_dirty();
            return;
        }
        self.activate_overlay(request);
    }

    pub(crate) fn has_active_overlay(&self) -> bool {
        self.active_overlay.is_some()
    }

    pub(crate) fn modal_state(&self) -> Option<&ModalState> {
        self.active_overlay.as_ref().and_then(ActiveOverlay::as_modal)
    }

    pub(crate) fn modal_state_mut(&mut self) -> Option<&mut ModalState> {
        self.active_overlay.as_mut().and_then(ActiveOverlay::as_modal_mut)
    }

    pub(crate) fn wizard_overlay(&self) -> Option<&WizardModalState> {
        self.active_overlay.as_ref().and_then(ActiveOverlay::as_wizard)
    }

    pub(crate) fn wizard_overlay_mut(&mut self) -> Option<&mut WizardModalState> {
        self.active_overlay.as_mut().and_then(ActiveOverlay::as_wizard_mut)
    }

    pub(crate) fn take_modal_state(&mut self) -> Option<ModalState> {
        if !self
            .active_overlay
            .as_ref()
            .is_some_and(|overlay| matches!(overlay, ActiveOverlay::Modal(_)))
        {
            return None;
        }

        match self.active_overlay.take() {
            Some(ActiveOverlay::Modal(state)) => Some(*state),
            Some(ActiveOverlay::Wizard(_)) | None => None,
        }
    }

    fn activate_overlay(&mut self, request: OverlayRequest) {
        match request {
            OverlayRequest::Modal(request) => {
                self.clear_last_overlay_list_cache();
                self.activate_modal_overlay(request);
            }
            OverlayRequest::List(request) => self.activate_list_overlay(request),
            OverlayRequest::Wizard(request) => {
                self.clear_last_overlay_list_cache();
                self.activate_wizard_overlay(request);
            }
        }
    }

    pub(crate) fn close_overlay(&mut self) {
        let Some(state) = self.active_overlay.take() else {
            return;
        };

        self.modal_list_area = None;
        self.modal_text_areas.clear();
        self.modal_link_targets.clear();
        self.cache_last_overlay_list_state(&state);
        self.input_enabled = state.restore_input() && !self.activity_state.is_busy();
        self.cursor_visible = state.restore_cursor() && !self.activity_state.is_busy();

        if let Some(next_request) = self.overlay_queue.pop_front() {
            self.activate_overlay(next_request);
            return;
        }

        self.mark_dirty();
    }

    fn activate_modal_overlay(&mut self, request: ModalOverlayRequest) {
        let state = ModalState {
            title: request.title,
            lines: request.lines,
            footer_hint: None,
            hotkeys: Vec::new(),
            list: None,
            search: None,
            secure_prompt: request.secure_prompt,
            is_help_modal: request.is_help_modal,
            restore_input: true,
            restore_cursor: true,
        };
        if state.secure_prompt.is_none() {
            self.input_enabled = false;
        }
        self.cursor_visible = false;
        self.active_overlay = Some(ActiveOverlay::Modal(Box::new(state)));
        self.mark_dirty();
    }

    fn activate_list_overlay(&mut self, request: ListOverlayRequest) {
        let anchor_to_bottom = self.should_anchor_list_to_bottom(request.selected.as_ref());
        let mut list_state = ModalListState::new(request.items, request.selected.clone());
        let search_state = request.search.map(ModalSearchState::from);
        if let Some(search) = &search_state {
            list_state.apply_search_with_preference(&search.query, request.selected);
        }
        if anchor_to_bottom {
            list_state.select_last();
        }
        self.clear_last_overlay_list_cache();
        let state = ModalState {
            title: request.title,
            lines: request.lines,
            footer_hint: request.footer_hint,
            hotkeys: request.hotkeys,
            list: Some(list_state),
            search: search_state,
            secure_prompt: None,
            restore_input: true,
            restore_cursor: true,
            is_help_modal: false,
        };
        self.input_enabled = false;
        self.cursor_visible = false;
        self.active_overlay = Some(ActiveOverlay::Modal(Box::new(state)));
        self.mark_dirty();
    }

    fn cache_last_overlay_list_state(&mut self, overlay: &ActiveOverlay) {
        if let ActiveOverlay::Modal(state) = overlay
            && let Some(list) = state.list.as_ref()
        {
            self.last_overlay_list_selection = list.current_selection();
            self.last_overlay_list_was_last = list.selected_is_last();
            return;
        }

        self.last_overlay_list_selection = None;
        self.last_overlay_list_was_last = false;
    }

    fn should_anchor_list_to_bottom(&self, preferred: Option<&InlineListSelection>) -> bool {
        self.last_overlay_list_was_last && self.last_overlay_list_selection.as_ref() == preferred
    }

    fn clear_last_overlay_list_cache(&mut self) {
        self.last_overlay_list_selection = None;
        self.last_overlay_list_was_last = false;
    }

    fn activate_wizard_overlay(&mut self, request: WizardOverlayRequest) {
        let wizard =
            WizardModalState::new(request.title, request.steps, request.current_step, request.search, request.mode);
        self.active_overlay = Some(ActiveOverlay::Wizard(Box::new(wizard)));
        self.input_enabled = false;
        self.cursor_visible = false;
        self.mark_dirty();
    }

    /// Scroll operations
    ///
    /// Note: The scroll offset model is inverted for chat-style display:
    /// offset=0 shows the bottom (newest content), offset=max shows the top.
    /// Therefore "scroll up" (show older content) increases the offset, and
    /// "scroll down" (show newer content) decreases it.
    pub(crate) fn scroll_line_up(&mut self) {
        self.mark_scrolling();
        self.ensure_scroll_metrics();
        let previous_offset = self.scroll_manager.offset();
        self.scroll_manager.scroll_down(1);
        if self.scroll_manager.offset() != previous_offset {
            self.user_scrolled = self.scroll_manager.offset() != 0;
            self.visible_lines_cache = None;
            // Content moves down on screen; shift selection to match.
            self.mouse_selection.adjust_for_scroll(1);
        }
    }

    pub(crate) fn scroll_line_down(&mut self) {
        self.mark_scrolling();
        self.ensure_scroll_metrics();
        let previous_offset = self.scroll_manager.offset();
        self.scroll_manager.scroll_up(1);
        if self.scroll_manager.offset() != previous_offset {
            self.user_scrolled = self.scroll_manager.offset() != 0;
            self.visible_lines_cache = None;
            // Content moves up on screen; shift selection to match.
            self.mouse_selection.adjust_for_scroll(-1);
        }
    }

    pub(crate) fn scroll_page_up(&mut self) {
        self.mark_scrolling();
        self.ensure_scroll_metrics();
        let previous_offset = self.scroll_manager.offset();
        let page = self.viewport_height().max(1);
        self.scroll_manager.scroll_down(page);
        if self.scroll_manager.offset() != previous_offset {
            let actual_delta = self.scroll_manager.offset() - previous_offset;
            self.user_scrolled = self.scroll_manager.offset() != 0;
            self.visible_lines_cache = None;
            self.mouse_selection.adjust_for_scroll(actual_delta as i32);
        }
    }

    pub(crate) fn scroll_page_down(&mut self) {
        self.mark_scrolling();
        self.ensure_scroll_metrics();
        let page = self.viewport_height().max(1);
        let previous_offset = self.scroll_manager.offset();
        self.scroll_manager.scroll_up(page);
        if self.scroll_manager.offset() != previous_offset {
            let actual_delta = previous_offset - self.scroll_manager.offset();
            self.user_scrolled = self.scroll_manager.offset() != 0;
            self.visible_lines_cache = None;
            self.mouse_selection.adjust_for_scroll(-(actual_delta as i32));
        }
    }

    pub(crate) fn viewport_height(&self) -> usize {
        self.transcript_rows.max(1) as usize
    }

    /// Apply coalesced scroll from accumulated scroll events
    /// This is more efficient than calling scroll_line_up/down multiple times
    pub(crate) fn apply_coalesced_scroll(&mut self, line_delta: i32, page_delta: i32) {
        self.mark_scrolling();
        self.ensure_scroll_metrics();
        let previous_offset = self.scroll_manager.offset();

        // Apply page scroll first (larger movements)
        // Inverted offset model: positive delta = scroll down visually = decrease offset
        if page_delta != 0 {
            let page_size = self.viewport_height().max(1);
            if page_delta > 0 {
                self.scroll_manager.scroll_up(page_size * page_delta.unsigned_abs() as usize);
            } else {
                self.scroll_manager.scroll_down(page_size * page_delta.unsigned_abs() as usize);
            }
        }

        // Then apply line scroll
        if line_delta != 0 {
            if line_delta > 0 {
                self.scroll_manager.scroll_up(line_delta.unsigned_abs() as usize);
            } else {
                self.scroll_manager.scroll_down(line_delta.unsigned_abs() as usize);
            }
        }

        // Invalidate visible lines cache if offset actually changed
        if self.scroll_manager.offset() != previous_offset {
            self.invalidate_transcript_viewport();
            // Compute actual row delta for selection adjustment.
            // Inverted model: increasing offset → content moves down → positive row delta.
            let offset_delta = self.scroll_manager.offset() as i64 - previous_offset as i64;
            self.mouse_selection.adjust_for_scroll(offset_delta as i32);
        }
    }

    /// Invalidate scroll metrics to force recalculation
    pub(crate) fn invalidate_scroll_metrics(&mut self) {
        self.scroll_manager.invalidate_metrics();
        self.invalidate_transcript_viewport();
    }

    /// Invalidate the transcript cache
    pub(crate) fn invalidate_transcript_cache(&mut self) {
        self.transcript_presentation_revision = self.transcript_presentation_revision.wrapping_add(1);
        let had_cache = if let Some(cache) = self.transcript_cache.as_mut() {
            cache.invalidate_content();
            true
        } else {
            false
        };
        self.invalidate_transcript_viewport();
        self.request_transcript_clear();

        if had_cache || self.first_dirty_line.is_none() {
            self.first_dirty_line = Some(0);
        }
    }

    /// Get the current maximum scroll offset
    pub(crate) fn current_max_scroll_offset(&mut self) -> usize {
        self.ensure_scroll_metrics();
        self.scroll_manager.max_offset()
    }

    /// Enforce scroll bounds after viewport changes
    pub(crate) fn enforce_scroll_bounds(&mut self) {
        let max_offset = self.current_max_scroll_offset();
        if self.scroll_manager.offset() > max_offset {
            self.scroll_manager.set_offset(max_offset);
        }
    }

    /// Ensure scroll metrics are up to date
    pub(crate) fn ensure_scroll_metrics(&mut self) {
        if self.scroll_manager.metrics_valid() {
            return;
        }

        let viewport_rows = self.viewport_height();
        if self.transcript_width == 0 || viewport_rows == 0 {
            self.scroll_manager.set_viewport_rows(viewport_rows.max(1) as u16);
            self.scroll_manager.set_total_rows(0);
            return;
        }

        let effective_padding = ui::effective_transcript_bottom_padding(viewport_rows);
        let total_rows = self.total_transcript_rows(self.transcript_width) + effective_padding;
        self.scroll_manager.set_viewport_rows(viewport_rows as u16);
        self.scroll_manager.set_total_rows(total_rows);
        self.scroll_manager.clamp_offset();
    }

    /// Prepare transcript scroll parameters
    pub(crate) fn prepare_transcript_scroll(&mut self, total_rows: usize, viewport_rows: usize) -> (usize, usize) {
        let viewport = viewport_rows.max(1);
        let clamped_total = total_rows.max(1);
        self.scroll_manager.set_viewport_rows(viewport as u16);
        self.scroll_manager.set_total_rows(clamped_total);
        let max_offset = self.scroll_manager.max_offset();

        if self.scroll_manager.offset() > max_offset {
            self.scroll_manager.set_offset(max_offset);
        }

        let top_offset = max_offset.saturating_sub(self.scroll_manager.offset());
        (top_offset, clamped_total)
    }

    /// Adjust scroll position after content changes to keep the current view stable.
    /// When the viewport is at the bottom (offset 0), new content naturally stays
    /// in view without adjustment. Only when the user has scrolled up (offset > 0)
    /// do we adjust the offset to prevent the view from drifting.
    pub(crate) fn adjust_scroll_after_change(&mut self, previous_max_offset: usize) {
        let new_max_offset = self.current_max_scroll_offset();

        if self.scroll_manager.offset() > 0 && new_max_offset > previous_max_offset {
            // Keep content position stable when the user has scrolled away from bottom
            use std::cmp::min;
            let current_offset = self.scroll_manager.offset();
            let delta = new_max_offset - previous_max_offset;
            self.scroll_manager.set_offset(min(current_offset + delta, new_max_offset));
        }
        self.enforce_scroll_bounds();
    }

    /// Emit an inline event through the channel and callback
    #[inline]
    pub(crate) fn emit_inline_event(
        &self,
        event: &InlineEvent,
        events: &UnboundedSender<InlineEvent>,
        callback: Option<&(dyn Fn(&InlineEvent) + Send + Sync + 'static)>,
    ) {
        if let Some(cb) = callback {
            cb(event);
        }
        let _ = events.send(event.clone());
    }

    /// Handle scroll down event
    #[inline]
    fn handle_scroll_down(
        &mut self,
        events: &UnboundedSender<InlineEvent>,
        callback: Option<&(dyn Fn(&InlineEvent) + Send + Sync + 'static)>,
    ) {
        self.scroll_line_down();
        self.mark_dirty();
        self.emit_inline_event(&InlineEvent::ScrollLineDown, events, callback);
    }

    /// Handle scroll up event
    #[inline]
    fn handle_scroll_up(
        &mut self,
        events: &UnboundedSender<InlineEvent>,
        callback: Option<&(dyn Fn(&InlineEvent) + Send + Sync + 'static)>,
    ) {
        self.scroll_line_up();
        self.mark_dirty();
        self.emit_inline_event(&InlineEvent::ScrollLineUp, events, callback);
    }
}

fn extract_task_progress(lines: &[String]) -> Option<String> {
    let line = lines
        .iter()
        .find_map(|line| line.trim().strip_prefix("Progress: ").map(str::trim))?;
    let summary = line.split_whitespace().next()?.trim();
    (!summary.is_empty()).then(|| summary.to_string())
}
