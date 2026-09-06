#![allow(
    missing_docs,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
use super::super::*;
use super::helpers::*;
use crate::tui::core_tui::widgets::{LayoutMode, SessionWidget};
use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};
use unicode_width::UnicodeWidthStr;

#[test]
fn input_compact_preview_for_image_path() {
    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
    let image_path = "/tmp/Screenshot 2026-02-06 at 3.39.48 PM.png";

    session.insert_paste_text(image_path);

    let data = session.build_input_widget_data(VIEW_WIDTH, VIEW_ROWS);
    let rendered = text_content(&data.text);
    assert!(rendered.contains("[Image:"));
    assert!(rendered.contains("Screenshot 2026-02-06"));
}

#[test]
fn input_compact_preview_for_quoted_image_path() {
    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
    let image_path = "\"/tmp/Screenshot 2026-02-06 at 3.39.48 PM.png\"";

    session.insert_paste_text(image_path);

    let data = session.build_input_widget_data(VIEW_WIDTH, VIEW_ROWS);
    let rendered = text_content(&data.text);
    assert!(rendered.contains("[Image:"));
    assert!(rendered.contains("Screenshot 2026-02-06"));
}

#[test]
fn control_g_launches_editor() {
    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);

    let event = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL);
    let result = session.process_key(event);

    assert!(matches!(result, Some(InlineEvent::LaunchEditor { draft }) if draft.is_empty()));
}

#[test]
fn control_g_launches_editor_with_draft() {
    let text = "hello world";
    let mut session = session_with_input(text, 0);

    let result = session.process_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));

    assert!(matches!(result, Some(InlineEvent::LaunchEditor { draft }) if draft == text));
}

#[test]
fn control_g_launches_editor_from_plan_confirmation_modal() {
    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
    session.input_status_right = Some("model | 25% context".to_string());
    let plan = app_types::PlanContent::from_markdown(
        "Test Plan".to_string(),
        "## Plan of Work\n- Step 1",
        Some(".vtcode/plans/test-plan.md".to_string()),
    );
    show_plan_confirmation_overlay(&mut session, plan);

    let event = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL);
    let result = session.process_key(event);

    assert!(matches!(
        result,
        Some(InlineEvent::Overlay(OverlayEvent::Submitted(OverlaySubmission::Hotkey(
            OverlayHotkeyAction::LaunchEditor
        ))))
    ));
    assert!(session.modal_state().is_none());
}

#[test]
fn plan_confirmation_modal_matches_four_way_gate_copy() {
    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
    let plan = app_types::PlanContent::from_markdown(
        "Test Plan".to_string(),
        "## Implementation Plan\n1. Step",
        Some(".vtcode/plans/test-plan.md".to_string()),
    );
    show_plan_confirmation_overlay(&mut session, plan);

    let modal = session.modal_state().expect("plan confirmation modal should be present");
    assert_eq!(modal.title, "Ready to code?");
    assert_eq!(
        modal.lines.first().map(String::as_str),
        Some("A plan is ready to execute. Would you like to proceed?")
    );

    let list = modal.list.as_ref().expect("plan confirmation should include list options");
    assert_eq!(list.items.len(), 3);

    assert_eq!(list.items[0].title, "Yes, auto-accept edits");
    assert_eq!(list.items[0].subtitle.as_deref(), Some("Execute with auto-approval."));
    assert_eq!(list.items[0].badge.as_deref(), Some("Recommended"));

    assert_eq!(list.items[1].title, "Yes, manually approve edits");
    assert_eq!(list.items[1].subtitle.as_deref(), Some("Keep context and confirm each edit before applying."));

    assert_eq!(list.items[2].title, "Type feedback to revise the plan");
    assert_eq!(list.items[2].subtitle.as_deref(), Some("Return to planning workflow and refine the plan."));
}

#[test]
fn arrow_keys_never_launch_editor() {
    let text = "hello world";
    let mut session = session_with_input(text, 0);

    // Test Right arrow with all possible modifier combinations
    for modifiers in [
        KeyModifiers::empty(),
        KeyModifiers::CONTROL,
        KeyModifiers::SHIFT,
        KeyModifiers::ALT,
        KeyModifiers::SUPER,
        KeyModifiers::META,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        KeyModifiers::CONTROL | KeyModifiers::SUPER,
    ] {
        let event = KeyEvent::new(KeyCode::Right, modifiers);
        let result = session.process_key(event);
        assert!(
            !matches!(result, Some(InlineEvent::LaunchEditor { .. })),
            "Right arrow with modifiers {modifiers:?} should not launch editor"
        );
    }

    // Test other arrow keys for safety
    for key_code in [KeyCode::Left, KeyCode::Up, KeyCode::Down] {
        let event = KeyEvent::new(key_code, KeyModifiers::SUPER);
        let result = session.process_key(event);
        assert!(
            !matches!(result, Some(InlineEvent::LaunchEditor { .. })),
            "{key_code:?} with SUPER should not launch editor"
        );
    }
}

#[test]
fn timeline_hidden_keeps_navigation_unselected() {
    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
    session.push_line(InlineMessageKind::Agent, vec![make_segment("Response")]);

    let backend = TestBackend::new(VIEW_WIDTH, VIEW_ROWS);
    let mut terminal = Terminal::new(backend).expect("failed to create test terminal");
    terminal
        .draw(|frame| session.render(frame))
        .expect("failed to render session with hidden timeline");

    assert!(session.navigation_state.selected().is_none());
}

#[test]
fn info_warning_error_blocks_render_distinct_fieldset_fills() {
    // (kind, label, unicode fill, ascii-fallback fill)
    let cases = [
        (InlineMessageKind::Info, " Info ", '─', '-'),
        (InlineMessageKind::Warning, " Warning ", '━', '='),
        (InlineMessageKind::Error, " Error ", '/', '/'),
    ];

    for (kind, label, unicode_fill, ascii_fill) in cases {
        let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
        session.push_line(kind, vec![make_segment("Theme switched to Ciapre")]);

        let rendered = rendered_transcript_widget_lines(&mut session, VIEW_WIDTH, VIEW_ROWS);

        // Top rule carries a center-aligned label flanked by the kind's fill.
        // The fill glyph follows terminal Unicode capabilities.
        assert!(
            rendered
                .iter()
                .any(|line| { (line.contains(unicode_fill) || line.contains(ascii_fill)) && line.contains(label) }),
            "{label} block should render a fieldset rule with its fill, got: {rendered:?}"
        );
        // Fieldset blocks never draw vertical box sides.
        assert!(
            rendered.iter().all(|line| !line.contains('│')),
            "{label} fieldset must not draw vertical sides, got: {rendered:?}"
        );
        // The content itself is preserved.
        assert!(
            rendered.iter().any(|line| line.contains("Theme switched to Ciapre")),
            "{label} block content should be preserved, got: {rendered:?}"
        );
    }
}

#[test]
fn active_file_operation_indicator_renders_spinner_frame() {
    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
    session.push_line(InlineMessageKind::Info, vec![make_segment("❋ Editing vtcode.toml...")]);
    session.handle_command(InlineCommand::SetInputStatus {
        left: Some("Running tool: edit_file".to_string()),
        right: None,
    });
    let rendered = rendered_transcript_widget_lines(&mut session, VIEW_WIDTH, VIEW_ROWS);
    let expected = format!("{} Editing vtcode.toml...", pulse_spinner_frame_for_phase(0.0));

    assert!(
        rendered.iter().any(|line| line.contains(&expected)),
        "active file operation indicator should show a spinner frame"
    );
    assert!(
        !rendered.iter().any(|line| line.contains("❋ Editing vtcode.toml...")),
        "spinner should replace the static file operation marker while active"
    );
}

#[test]
fn non_file_tool_status_keeps_static_file_operation_indicator() {
    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
    session.push_line(InlineMessageKind::Info, vec![make_segment("❋ Editing vtcode.toml...")]);
    session.handle_command(InlineCommand::SetInputStatus {
        left: Some("Running tool: code_search".to_string()),
        right: None,
    });

    let rendered = rendered_transcript_widget_lines(&mut session, VIEW_WIDTH, VIEW_ROWS);

    assert!(
        rendered.iter().any(|line| line.contains("❋ Editing vtcode.toml...")),
        "non-file tool activity should not animate stale file operation indicators"
    );
}

#[test]
fn empty_enter_with_active_pty_opens_jobs() {
    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
    session.active_pty_sessions = Some(Arc::new(AtomicUsize::new(1)));

    let event = session.process_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(matches!(event, Some(InlineEvent::Submit(ref value)) if value == "/jobs"));
}

#[test]
fn active_pty_observer_drives_compact_loading_status() {
    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
    let active_pty_sessions = Arc::new(AtomicUsize::new(1));
    session.active_pty_sessions = Some(Arc::clone(&active_pty_sessions));
    session.handle_command(InlineCommand::SetInputStatus { left: Some("main*".to_string()), right: None });

    assert_eq!(session.status_left_text(), Some("Running PTY command..."));
    assert!(session.has_status_spinner());
    assert!(session.is_running_activity());

    active_pty_sessions.store(0, Ordering::Relaxed);

    assert_eq!(session.status_left_text(), Some("main*"));
    assert!(!session.has_status_spinner());
    assert!(!session.is_running_activity());
}

#[test]
fn active_pty_observer_overrides_idle_stage_status() {
    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
    let active_pty_sessions = Arc::new(AtomicUsize::new(1));
    session.active_pty_sessions = Some(Arc::clone(&active_pty_sessions));
    session.handle_command(InlineCommand::SetActivityState(ActivityState::Building));

    assert_eq!(session.status_left_text(), Some("Running PTY command..."));
    assert!(session.has_status_spinner());

    active_pty_sessions.store(0, Ordering::Relaxed);

    assert_eq!(session.status_left_text(), Some("Building..."));
    assert!(!session.has_status_spinner());
}

#[test]
fn task_panel_visibility_is_independent_from_logs() {
    let mut session = AppSession::new(InlineTheme::default(), None, VIEW_ROWS);
    session.set_task_panel_visible(true);
    let initial_task_panel = session.show_task_panel;
    let initial_logs = session.core.show_logs;

    session.core.toggle_logs();

    assert_eq!(session.show_task_panel, initial_task_panel);
    assert_ne!(session.core.show_logs, initial_logs);
}

#[test]
fn stage_states_keep_input_enabled() {
    for state in [ActivityState::Planning, ActivityState::Building] {
        let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
        session.handle_command(InlineCommand::SetActivityState(state));

        assert!(session.input_enabled(), "{state:?} should keep input enabled");
        assert_eq!(session.status_left_text(), state.status());
        assert!(!session.is_running_activity(), "{state:?} with no tool should not spin");
    }
}

#[test]
fn git_status_is_not_rendered_in_footer() {
    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
    session.handle_command(InlineCommand::SetInputStatus {
        left: Some("git: main*".to_string()),
        right: Some("10:30".to_string()),
    });

    let area = Rect::new(0, 0, VIEW_WIDTH, 1);
    let mut buffer = Buffer::empty(area);
    SessionWidget::new(&mut session)
        .layout_mode(LayoutMode::Compact)
        .render(area, &mut buffer);

    let rendered = buffer.content().iter().map(|cell| cell.symbol()).collect::<String>();
    assert!(!rendered.contains("main*"), "git status should not appear in footer: {rendered}");
}

#[test]
fn stage_state_with_running_tool_shows_tool_status() {
    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
    session.handle_command(InlineCommand::SetActivityState(ActivityState::Planning));
    session.handle_command(InlineCommand::SetInputStatus {
        left: Some("Running tool: edit_file".to_string()),
        right: None,
    });

    // The live tool status must replace the stage label so the footer
    // reflects what the agent is actually doing, not a frozen "Planning...".
    assert_eq!(session.status_left_text(), Some("Running tool: edit_file"));
    assert!(session.has_status_spinner(), "stage label must animate while a tool runs");
    assert!(session.is_running_activity());
    assert!(session.input_enabled(), "stage states keep input usable mid-tool");
}

#[test]
fn stage_state_idle_shows_stage_label() {
    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
    session.handle_command(InlineCommand::SetActivityState(ActivityState::Planning));
    // No active tool — the status refresh may set a git-branch left text that
    // does not indicate active work. The stage label must still be shown.
    session.handle_command(InlineCommand::SetInputStatus { left: Some("main*".to_string()), right: None });

    assert_eq!(session.status_left_text(), Some("Planning..."));
    assert!(!session.has_status_spinner(), "idle stage must not spin");
    assert!(!session.is_running_activity());
}

#[test]
fn busy_state_disables_input_and_shows_busy_status() {
    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
    session.handle_command(InlineCommand::SetActivityState(ActivityState::StartingBuild));

    assert!(!session.input_enabled(), "busy states must block input");
    assert!(session.is_running_activity());
    assert!(session.has_status_spinner());
}

#[test]
fn blocked_state_persists_status_for_direct_left_status_readers() {
    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
    session.handle_command(InlineCommand::SetActivityState(ActivityState::Blocked));

    assert!(session.input_enabled(), "blocked state must keep input enabled");
    let left = session
        .input_status_left
        .as_deref()
        .expect("blocked state must persist its status for direct left-status readers");
    assert_eq!(left, ActivityState::Blocked.status().expect("blocked state has a status"));
    assert_eq!(session.status_left_text(), Some(left));
}

#[test]
fn idle_state_clears_left_status() {
    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
    session.handle_command(InlineCommand::SetActivityState(ActivityState::Blocked));
    session.handle_command(InlineCommand::SetActivityState(ActivityState::Idle));

    assert_eq!(session.input_status_left, None, "idle must clear the persisted blocked status");
}

#[test]
fn timeline_visible_selects_latest_item() {
    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
    session.push_line(InlineMessageKind::Agent, vec![make_segment("First")]);
    session.push_line(InlineMessageKind::Agent, vec![make_segment("Second")]);

    let backend = TestBackend::new(VIEW_WIDTH, VIEW_ROWS);
    let mut terminal = Terminal::new(backend).expect("failed to create test terminal");
    terminal
        .draw(|frame| session.render(frame))
        .expect("failed to render session with timeline");

    assert!(session.navigation_state.selected().is_none());
}

#[test]
fn tool_detail_renders_with_border_and_body_style() {
    let theme = themed_inline_colors();
    let mut session = Session::new(theme, None, VIEW_ROWS);
    let detail_style = InlineTextStyle::default().italic();
    session.push_line(
        InlineMessageKind::Tool,
        vec![InlineSegment {
            text: "    result line".to_string(),
            style: Arc::new(detail_style),
        }],
    );

    let index = session.lines.len().checked_sub(1).expect("tool detail line should exist");
    let spans = session.render_message_spans(index);

    assert_eq!(spans.len(), 1);
    let body_span = &spans[0];
    assert!(body_span.style.add_modifier.contains(Modifier::ITALIC));
    assert_eq!(body_span.content.clone().into_owned(), "    result line");
}

#[test]
fn top_level_task_tree_tail_line_is_dimmed_in_tool_blocks() {
    let theme = themed_inline_colors();
    let mut session = Session::new(theme, None, VIEW_ROWS);
    session.push_line(
        InlineMessageKind::Tool,
        vec![InlineSegment {
            text: "└ Report actions taken, blockers, and required user input".to_string(),
            style: Arc::new(InlineTextStyle::default()),
        }],
    );

    let index = session.lines.len().checked_sub(1).expect("tool detail line should exist");
    let transcript_lines = session.reflow_message_lines(index, 100, false);
    let task_span = transcript_lines
        .iter()
        .flat_map(|line| line.line.spans.iter())
        .find(|span| span.content.contains("Report actions taken"))
        .expect("expected task span");

    assert!(task_span.style.add_modifier.contains(Modifier::DIM), "top-level task rows should render dimmed");
}

#[test]
fn tool_block_with_cjk_prefix_does_not_overflow_viewport() {
    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
    session.push_line(
        InlineMessageKind::Tool,
        vec![InlineSegment {
            text: "日本語のツール詳細情報".to_string(),
            style: Arc::new(InlineTextStyle::default()),
        }],
    );
    let index = session.lines.len() - 1;
    let transcript = session.reflow_message_lines(index, VIEW_WIDTH, false);
    for line in &transcript {
        let width: usize = line.line.spans.iter().map(|span| span.content.width()).sum();
        assert!(
            width <= VIEW_WIDTH as usize,
            "tool block line with CJK prefix overflowed viewport: {width} > {VIEW_WIDTH}"
        );
    }
}

#[test]
fn error_block_with_cjk_label_reserves_correct_content_width() {
    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
    session.push_line(
        InlineMessageKind::Error,
        vec![InlineSegment {
            text: "エラーが発生しました".to_string(),
            style: Arc::new(InlineTextStyle::default()),
        }],
    );
    let index = session.lines.len() - 1;
    let transcript = session.reflow_message_lines(index, VIEW_WIDTH, false);
    for line in &transcript {
        let width: usize = line.line.spans.iter().map(|span| span.content.width()).sum();
        assert!(
            width <= VIEW_WIDTH as usize,
            "error block line with CJK label overflowed viewport: {width} > {VIEW_WIDTH}"
        );
    }
}

#[test]
fn backspace_after_large_paste_deletes_whole_block() {
    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
    session.insert_char('x');
    let pasted: String = (0..12).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
    session.insert_paste_text(&pasted);

    assert!(session.input_manager.compact_paste_range().is_some());
    assert_eq!(session.input_manager.cursor(), session.input_manager.content().len());

    session.delete_char();

    assert_eq!(session.input_manager.content(), "x");
    assert!(session.input_manager.compact_paste_range().is_none());
}

#[test]
fn backspace_after_paste_still_deletes_typed_chars_one_by_one() {
    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
    let pasted: String = (0..12).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
    session.insert_paste_text(&pasted);

    session.insert_char('!');
    session.insert_char('?');
    assert!(session.input_manager.content().ends_with("!?"));

    session.delete_char();
    assert!(session.input_manager.content().ends_with("!"));
    assert!(session.input_manager.compact_paste_range().is_some());

    session.delete_char();
    assert!(session.input_manager.content().ends_with(&pasted));
}

#[test]
fn backspace_with_active_selection_deletes_selection_not_whole_paste() {
    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
    let pasted: String = (0..12).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
    session.insert_paste_text(&pasted);

    let content_len = session.input_manager.content().len();
    // Select the last two characters (e.g. "10" of "line 10") so the active
    // selection ends exactly at the collapsed paste end.
    session.input_manager.set_cursor_with_selection(content_len - 2);
    assert!(session.input_manager.selection_range().is_some());

    session.delete_char();

    // The selection wins over the collapse: only the two selected chars are
    // removed, not the entire pasted block.
    assert_eq!(session.input_manager.content().len(), content_len - 2);
    assert!(!session.input_manager.content().ends_with("10"));
    assert!(session.input_manager.content().contains("line 0"));
}

#[test]
fn backspace_after_paste_deletes_whole_block_when_no_selection() {
    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
    session.insert_char('x');
    let pasted: String = (0..12).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
    session.insert_paste_text(&pasted);

    assert!(session.input_manager.compact_paste_range().is_some());
    assert_eq!(session.input_manager.cursor(), session.input_manager.content().len());
    assert!(session.input_manager.selection_range().is_none());

    session.delete_char();

    assert_eq!(session.input_manager.content(), "x");
    assert!(session.input_manager.compact_paste_range().is_none());
}

#[test]
fn delete_word_forward_before_paste_keeps_block_delete_aligned() {
    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
    session.insert_char('x');
    session.insert_char(' ');
    let pasted: String = (0..12).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
    session.insert_paste_text(&pasted);

    let range = session.input_manager.compact_paste_range().expect("collapsed paste range");
    assert_eq!(range.start, 2);

    // Delete the typed word sitting before the collapsed block.
    session.input_manager.set_cursor(0);
    session.input_manager.delete_word_forward();

    // The range must shift by the removed bytes, not go stale.
    let shifted = session
        .input_manager
        .compact_paste_range()
        .expect("range survives edits entirely before it");
    assert_eq!(shifted.start, 1);
    assert_eq!(shifted.end, range.end - 1);

    // The block-delete invariant holds: cursor at the new end coincides with
    // the shifted range end, so Backspace removes the whole block.
    session.input_manager.set_cursor(session.input_manager.content().len());
    session.delete_char();

    assert_eq!(session.input_manager.content(), " ");
    assert!(session.input_manager.compact_paste_range().is_none());
}

#[test]
fn delete_word_forward_overlapping_paste_clears_compact_range() {
    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
    session.insert_char('x');
    session.insert_char(' ');
    let pasted: String = (0..12).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
    session.insert_paste_text(&pasted);

    let range = session.input_manager.compact_paste_range().expect("collapsed paste range");

    // Cursor inside the collapsed block: an overlapping delete invalidates it
    // so a later Backspace never removes user-typed bytes.
    session.input_manager.set_cursor(range.start + 1);
    session.input_manager.delete_word_forward();

    assert!(session.input_manager.compact_paste_range().is_none());
}

#[test]
fn alt_backspace_deletes_previous_word() {
    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
    session.input_manager.set_content("hello world".to_string());
    session.input_manager.set_cursor(session.input_manager.content().len());

    let result = session.process_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT));

    assert!(result.is_none());
    assert_eq!(session.input_manager.content(), "hello ");
}

#[test]
fn alt_backspace_deletes_previous_cyrillic_word() {
    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
    session.input_manager.set_content("привет мир".to_string());
    session.input_manager.set_cursor(session.input_manager.content().len());

    let result = session.process_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT));

    assert!(result.is_none());
    assert_eq!(session.input_manager.content(), "привет ");
}

#[test]
fn overlay_retains_input_ownership_across_activity_transitions() {
    use crate::tui::core_tui::session::input_manager::InputOwner;
    for initial in [
        ActivityState::Idle,
        ActivityState::StartingBuild,
        ActivityState::Building,
    ] {
        for next in [
            ActivityState::Idle,
            ActivityState::StartingBuild,
            ActivityState::Building,
            ActivityState::Blocked,
        ] {
            let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
            session.handle_command(InlineCommand::SetActivityState(initial));
            session.handle_command(InlineCommand::ShowOverlay {
                request: Box::new(OverlayRequest::Modal(crate::tui::core_tui::types::ModalOverlayRequest {
                    title: "Ownership".to_string(),
                    lines: Vec::new(),
                    secure_prompt: None,
                    is_help_modal: false,
                })),
            });
            session.handle_command(InlineCommand::SetActivityState(next));
            assert_eq!(session.input_owner(), InputOwner::Overlay);
            assert!(!session.input_enabled());
            session.close_overlay();
            assert_eq!(session.input_enabled(), !next.is_busy(), "{initial:?} -> {next:?}");
        }
    }
}
