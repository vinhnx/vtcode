#![cfg(unix)]
#![allow(
    missing_docs,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
use super::super::*;
use super::helpers::*;

#[test]
fn double_click_selects_transcript_word_and_copies_it() {
    use crate::tui::core_tui::session::mouse_selection::{clipboard_command_override, set_clipboard_command_override};
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    let _guard = CLIPBOARD_TEST_LOCK.lock().expect("clipboard test lock should not be poisoned");

    let temp_dir = std::env::temp_dir().join(format!(
        "vtcode-clipboard-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be after UNIX_EPOCH")
            .as_nanos()
    ));
    fs::create_dir_all(&temp_dir).expect("create temp dir for clipboard script");
    struct TempDirGuard(PathBuf);
    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let _temp_guard = TempDirGuard(temp_dir.clone());

    let clipboard_file = temp_dir.join("clipboard.txt");
    // Create the destination before launching the detached helper. Under
    // parallel nextest load the helper can take longer than the bounded
    // native-copy wait to start; an empty file lets the poll distinguish that
    // startup delay from a failed copy without racing filesystem creation.
    fs::write(&clipboard_file, "").expect("create clipboard fixture");
    let script_name = if cfg!(target_os = "macos") { "pbcopy" } else { "xclip" };
    let script_path = temp_dir.join(script_name);
    fs::write(&script_path, format!("#!/bin/sh\ncat > '{}'\n", clipboard_file.display()))
        .expect("write fake clipboard command");
    let mut permissions = fs::metadata(&script_path).expect("read fake clipboard metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("make fake clipboard executable");

    struct ClipboardCommandGuard(Option<PathBuf>);
    impl Drop for ClipboardCommandGuard {
        fn drop(&mut self) {
            set_clipboard_command_override(self.0.clone());
        }
    }

    let _path_guard = ClipboardCommandGuard(clipboard_command_override());
    set_clipboard_command_override(Some(script_path.clone()));

    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
    session.push_line(InlineMessageKind::Agent, vec![make_segment("hello world")]);

    let (transcript_area, rendered) = rendered_transcript_lines(&mut session, VIEW_ROWS * 2);
    let row = rendered
        .iter()
        .position(|line| line.contains("hello world"))
        .expect("expected hello world to be rendered");
    let column =
        rendered[row].find("hello").expect("expected hello word in rendered line") as u16 + transcript_area.x + 1;
    let row = transcript_area.y + row as u16;

    let (tx, _rx) = mpsc::unbounded_channel();
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    };
    let release = MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    };

    session.handle_event(CrosstermEvent::Mouse(click), &tx, None);
    session.handle_event(CrosstermEvent::Mouse(release), &tx, None);
    session.handle_event(CrosstermEvent::Mouse(click), &tx, None);
    session.handle_event(CrosstermEvent::Mouse(release), &tx, None);

    let mut buffer = Buffer::empty(Rect::new(0, 0, VIEW_WIDTH, VIEW_ROWS * 2));
    for (dy, line) in rendered.iter().enumerate() {
        for (dx, ch) in line.chars().enumerate() {
            buffer[(transcript_area.x + dx as u16, transcript_area.y + dy as u16)].set_symbol(&ch.to_string());
        }
    }
    let selected = session.mouse_selection.extract_text(&buffer, buffer.area);
    assert_eq!(selected, "hello");
    assert!(session.mouse_selection.has_selection);
    assert!(session.mouse_selection.needs_copy());

    session.copy_text_to_clipboard(&selected);
    session.mouse_selection.mark_copied();
    assert!(!session.mouse_selection.needs_copy());

    // Clipboard helpers are intentionally bounded and may finish just after
    // the copy call returns on a busy macOS host. Give the detached helper a
    // short grace period before asserting on its fixture output.
    for _ in 0..100 {
        if fs::read_to_string(&clipboard_file).is_ok_and(|contents| contents == "hello") {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let clipboard_contents = fs::read_to_string(&clipboard_file).expect("read copied transcript text");
    assert_eq!(clipboard_contents, "hello");

    let rendered_status = session
        .render_input_status_line(VIEW_WIDTH)
        .expect("input status line")
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(
        rendered_status.contains("Copied to clipboard"),
        "transcript copy should surface a temporary confirmation"
    );
}

#[test]
fn selecting_input_text_auto_copies_and_keeps_selection() {
    use crate::tui::core_tui::session::mouse_selection::{clipboard_command_override, set_clipboard_command_override};
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    let _guard = CLIPBOARD_TEST_LOCK.lock().expect("clipboard test lock should not be poisoned");

    let temp_dir = std::env::temp_dir().join(format!(
        "vtcode-clipboard-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be after UNIX_EPOCH")
            .as_nanos()
    ));
    fs::create_dir_all(&temp_dir).expect("create temp dir for clipboard script");
    struct TempDirGuard(PathBuf);
    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let _temp_guard = TempDirGuard(temp_dir.clone());

    let clipboard_file = temp_dir.join("clipboard.txt");
    fs::write(&clipboard_file, "").expect("create clipboard fixture");
    let script_name = if cfg!(target_os = "macos") { "pbcopy" } else { "xclip" };
    let script_path = temp_dir.join(script_name);
    fs::write(&script_path, format!("#!/bin/sh\ncat > '{}'\n", clipboard_file.display()))
        .expect("write fake clipboard command");
    let mut permissions = fs::metadata(&script_path).expect("read fake clipboard metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("make fake clipboard executable");

    struct ClipboardCommandGuard(Option<PathBuf>);
    impl Drop for ClipboardCommandGuard {
        fn drop(&mut self) {
            set_clipboard_command_override(self.0.clone());
        }
    }

    let _path_guard = ClipboardCommandGuard(clipboard_command_override());
    set_clipboard_command_override(Some(script_path.clone()));

    let mut session = app_session_with_input("hello world", "hello world".len());
    for _ in 0..5 {
        let result = session.process_key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
        assert!(result.is_none());
    }

    assert_eq!(session.core.input_manager.selection_range(), Some(("hello world".len() - 5, "hello world".len())));

    let rendered = rendered_app_session_lines(&mut session, VIEW_ROWS);
    assert!(
        rendered.iter().any(|line| line.contains("Copied to clipboard")),
        "input copy should surface a temporary confirmation"
    );

    for _ in 0..100 {
        if fs::read_to_string(&clipboard_file).is_ok_and(|contents| contents == "world") {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let clipboard_contents = fs::read_to_string(&clipboard_file).expect("read copied input text");
    assert_eq!(clipboard_contents, "world");

    assert_eq!(session.core.input_manager.selection_range(), Some(("hello world".len() - 5, "hello world".len())));

    let result = session.process_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(result.is_none());
    assert_eq!(session.core.input_manager.selection_range(), Some(("hello world".len() - 5, "hello world".len())));

    let clipboard_contents = fs::read_to_string(&clipboard_file).expect("read copied input text");
    assert_eq!(clipboard_contents, "world");
}

#[test]
fn transcript_copy_failure_surfaces_copy_failed_status() {
    use crate::tui::core_tui::session::mouse_selection::{
        clipboard_command_override, set_clipboard_command_override, set_osc52_write_override,
    };
    use std::path::PathBuf;

    let _guard = CLIPBOARD_TEST_LOCK.lock().expect("clipboard test lock should not be poisoned");

    struct OverrideGuard;
    impl Drop for OverrideGuard {
        fn drop(&mut self) {
            set_clipboard_command_override(None);
            set_osc52_write_override(None);
        }
    }
    let _overrides = OverrideGuard;

    // An unspawnable helper fails before the bounded clipboard poll starts,
    // keeping this failure-path assertion deterministic under parallel load.
    set_clipboard_command_override(Some(PathBuf::from("/vtcode-test/missing-clipboard-helper")));
    set_osc52_write_override(Some(false));

    let mut session = Session::new(InlineTheme::default(), None, VIEW_ROWS);
    session.copy_text_to_clipboard("hello");

    let rendered_status = session
        .render_input_status_line(VIEW_WIDTH)
        .expect("input status line")
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(
        rendered_status.contains("Copy failed"),
        "failed copy should surface a failure notice, got: {rendered_status}"
    );
}

#[test]
fn input_copy_failure_is_swallowed_without_interrupt_and_never_retries() {
    use crate::tui::core_tui::session::mouse_selection::{
        clipboard_command_override, set_clipboard_command_override, set_osc52_write_override,
    };
    use std::path::PathBuf;

    let _guard = CLIPBOARD_TEST_LOCK.lock().expect("clipboard test lock should not be poisoned");

    struct OverrideGuard;
    impl Drop for OverrideGuard {
        fn drop(&mut self) {
            set_clipboard_command_override(None);
            set_osc52_write_override(None);
        }
    }
    let _overrides = OverrideGuard;

    // An unspawnable helper fails before the bounded clipboard poll starts,
    // keeping this failure-path assertion deterministic under parallel load.
    set_clipboard_command_override(Some(PathBuf::from("/vtcode-test/missing-clipboard-helper")));
    set_osc52_write_override(Some(false));

    let mut session = app_session_with_input("hello world", "hello world".len());
    for _ in 0..5 {
        let result = session.process_key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
        assert!(result.is_none());
    }

    let _ = rendered_app_session_lines(&mut session, VIEW_ROWS);
    assert!(
        !session.core.input_manager.selection_needs_copy(),
        "a failed auto-copy must not re-arm or every frame would retry it"
    );

    let result = session.process_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(result.is_none(), "Ctrl+C over a selection must stay a copy attempt even when copying fails");

    let status = session
        .core
        .render_input_status_line(VIEW_WIDTH)
        .expect("input status line after failed copy")
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(status.contains("Copy failed"), "failed copy should remain visible, got: {status}");
}
