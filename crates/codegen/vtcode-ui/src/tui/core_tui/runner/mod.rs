use std::io;
use std::time::Duration;

use anyhow::{Context, Result};
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio_util::sync::CancellationToken;

use crate::tui::config::types::UiSurfacePreference;
use crate::tui::options::FullscreenInteractionSettings;
use crate::tui::ui::tui::log::{clear_tui_log_sender, register_tui_log_sender, set_log_theme_name};

type EventCallback<E> = std::sync::Arc<dyn Fn(&E) + Send + Sync + 'static>;

pub trait TuiCommand {
    fn is_suspend_event_loop(&self) -> bool;
    fn is_resume_event_loop(&self) -> bool;
    fn is_clear_input_queue(&self) -> bool;
    fn is_force_redraw(&self) -> bool;
    fn is_stop_event_stream(&self) -> bool;
    fn is_start_event_stream(&self) -> bool;
}

pub trait TuiSessionDriver {
    type Command: TuiCommand;
    type Event;

    fn handle_command(&mut self, command: Self::Command);
    #[expect(
        clippy::type_complexity,
        reason = "Intentional compatibility, platform, test, or API-shape suppression."
    )]
    fn handle_event(
        &mut self,
        event: crossterm::event::Event,
        events: &UnboundedSender<Self::Event>,
        callback: Option<&(dyn Fn(&Self::Event) + Send + Sync + 'static)>,
    );
    fn handle_tick(&mut self);
    fn render(&mut self, frame: &mut ratatui::Frame<'_>);
    fn take_redraw(&mut self) -> bool;
    fn use_steady_cursor(&self) -> bool;
    fn is_hovering_link(&self) -> bool;
    fn is_selecting_text(&self) -> bool;
    fn should_exit(&self) -> bool;
    fn request_exit(&mut self);
    fn mark_dirty(&mut self);
    fn update_terminal_title(&mut self);
    fn clear_terminal_title(&mut self);
    fn is_running_activity(&self) -> bool;
    fn has_status_spinner(&self) -> bool;
    fn thinking_spinner_active(&self) -> bool;
    fn has_active_navigation_ui(&self) -> bool;
    fn apply_coalesced_scroll(&mut self, line_delta: i32, page_delta: i32);
    fn set_show_logs(&mut self, show: bool);
    fn set_active_pty_sessions(&mut self, sessions: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>);
    fn set_workspace_root(&mut self, root: Option<std::path::PathBuf>);
    fn set_log_receiver(&mut self, receiver: UnboundedReceiver<crate::tui::core_tui::log::LogEntry>);
    fn set_fullscreen_active(&mut self, active: bool);
    fn set_fullscreen_interaction(&mut self, config: FullscreenInteractionSettings);
    fn set_preview_callback(&mut self, callback: Option<PreviewCallback>);
}

impl TuiCommand for crate::tui::core_tui::types::InlineCommand {
    fn is_suspend_event_loop(&self) -> bool {
        matches!(self, crate::tui::core_tui::types::InlineCommand::SuspendEventLoop)
    }

    fn is_resume_event_loop(&self) -> bool {
        matches!(self, crate::tui::core_tui::types::InlineCommand::ResumeEventLoop)
    }

    fn is_clear_input_queue(&self) -> bool {
        matches!(self, crate::tui::core_tui::types::InlineCommand::ClearInputQueue)
    }

    fn is_force_redraw(&self) -> bool {
        matches!(self, crate::tui::core_tui::types::InlineCommand::ForceRedraw)
    }

    fn is_stop_event_stream(&self) -> bool {
        matches!(self, crate::tui::core_tui::types::InlineCommand::StopEventStream)
    }

    fn is_start_event_stream(&self) -> bool {
        matches!(self, crate::tui::core_tui::types::InlineCommand::StartEventStream)
    }
}

use super::types::FocusChangeCallback;
pub(crate) use super::types::PreviewCallback;

mod drive;
mod events;
mod signal;
mod surface;
pub(crate) mod terminal_io;
mod terminal_modes;

use drive::{DriveRuntimeOptions, drive_terminal};
use events::{EventListener, EventSender, TerminalEvent, spawn_event_loop};
use signal::SignalCleanupGuard;
use surface::TerminalSurface;
use terminal_io::{drain_terminal_events, finalize_terminal, prepare_terminal};
use terminal_modes::{TerminalModeState, enable_terminal_modes, restore_terminal_modes};

/// Controls the lifecycle of the async crossterm event stream.
///
/// The event loop must be fully stopped before launching an external editor
/// that needs stdin (e.g., nvim), otherwise the background EventStream task
/// competes with the editor for terminal input, causing freezes.
pub(super) struct EventStreamController {
    cancellation_token: CancellationToken,
    join_handle: Option<tokio::task::JoinHandle<()>>,
    event_tx: EventSender,
    rx_paused: std::sync::Arc<std::sync::atomic::AtomicBool>,
    last_input_elapsed_ms: std::sync::Arc<std::sync::atomic::AtomicU64>,
    session_start: std::time::Instant,
}

impl EventStreamController {
    fn new(
        cancellation_token: CancellationToken,
        join_handle: tokio::task::JoinHandle<()>,
        event_tx: EventSender,
        rx_paused: std::sync::Arc<std::sync::atomic::AtomicBool>,
        last_input_elapsed_ms: std::sync::Arc<std::sync::atomic::AtomicU64>,
        session_start: std::time::Instant,
    ) -> Self {
        Self {
            cancellation_token,
            join_handle: Some(join_handle),
            event_tx,
            rx_paused,
            last_input_elapsed_ms,
            session_start,
        }
    }

    /// Cancel the current event loop task and await its termination.
    /// Creates a fresh CancellationToken for the next `start()` call.
    async fn stop(&mut self) {
        self.cancellation_token.cancel();
        if let Some(handle) = self.join_handle.take() {
            let _ = tokio::time::timeout(Duration::from_millis(100), handle).await;
        }
        self.cancellation_token = CancellationToken::new();
    }

    /// Spawn a new event loop task with a fresh EventStream.
    /// Safe to call multiple times after `stop()`.
    fn start(&mut self) {
        let token = self.cancellation_token.clone();
        let event_tx = self.event_tx.clone();
        let rx_paused = self.rx_paused.clone();
        let last_input = self.last_input_elapsed_ms.clone();
        let session_start = self.session_start;
        self.join_handle = Some(tokio::spawn(async move {
            spawn_event_loop(event_tx, token, rx_paused, last_input, session_start).await;
        }));
    }

    /// Ensure the event loop is stopped for final cleanup on TUI exit.
    async fn shutdown(&mut self) {
        if let Some(handle) = self.join_handle.take() {
            self.cancellation_token.cancel();
            let _ = tokio::time::timeout(Duration::from_millis(100), handle).await;
        }
    }
}

struct TerminalModeRestoreGuard {
    state: Option<TerminalModeState>,
}

impl TerminalModeRestoreGuard {
    fn new(state: TerminalModeState) -> Self {
        Self { state: Some(state) }
    }

    fn state_mut(&mut self) -> &mut TerminalModeState {
        self.state
            .as_mut()
            .expect("terminal mode restore guard must stay armed until shutdown")
    }

    fn restore(&mut self) -> Result<()> {
        if let Some(state) = self.state.take() {
            restore_terminal_modes(&state)?;
        }
        Ok(())
    }

    fn restore_silently(&mut self) {
        if self.state.is_some() {
            if let Err(error) = self.restore() {
                tracing::warn!(%error, "failed to restore terminal modes");
            }
        }
    }
}

impl Drop for TerminalModeRestoreGuard {
    fn drop(&mut self) {
        self.restore_silently();
    }
}

pub(crate) struct TuiOptions<E> {
    pub(crate) surface_preference: UiSurfacePreference,
    pub(crate) inline_rows: u16,
    pub(crate) show_logs: bool,
    pub(crate) log_theme: Option<String>,
    pub(crate) event_callback: Option<EventCallback<E>>,
    pub(crate) focus_callback: Option<FocusChangeCallback>,
    pub(crate) active_pty_sessions: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
    pub(crate) input_activity_counter: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    pub(crate) keyboard_protocol: crate::tui::config::KeyboardProtocolConfig,
    pub(crate) fullscreen: FullscreenInteractionSettings,
    pub(crate) workspace_root: Option<std::path::PathBuf>,
    pub(crate) preview_callback: Option<PreviewCallback>,
}

pub(crate) async fn run_tui<S, F>(
    mut commands: UnboundedReceiver<S::Command>,
    events: UnboundedSender<S::Event>,
    options: TuiOptions<S::Event>,
    make_session: F,
) -> Result<()>
where
    S: TuiSessionDriver,
    F: FnOnce(u16) -> S,
{
    // Create a guard to mark TUI as initialized during the session
    // This ensures the panic hook knows to restore terminal state
    let _panic_guard = crate::tui::ui::tui::panic_hook::TuiPanicGuard::new();

    let _signal_guard = SignalCleanupGuard::new()?;

    let surface = TerminalSurface::detect(options.surface_preference, options.inline_rows)?;
    set_log_theme_name(options.log_theme.clone());
    let mut session = make_session(surface.rows());
    session.set_preview_callback(options.preview_callback.clone());
    session.set_show_logs(options.show_logs);
    session.set_active_pty_sessions(options.active_pty_sessions);
    session.set_workspace_root(options.workspace_root.clone());
    session.set_fullscreen_active(surface.use_alternate());
    session.set_fullscreen_interaction(options.fullscreen);
    if options.show_logs {
        let (log_tx, log_rx) = tokio::sync::mpsc::unbounded_channel();
        session.set_log_receiver(log_rx);
        register_tui_log_sender(log_tx);
    } else {
        clear_tui_log_sender();
    }

    let keyboard_flags = crate::tui::config::keyboard_protocol_to_flags(&options.keyboard_protocol);
    let mut stderr = io::stderr();
    let mut mode_restore_guard =
        TerminalModeRestoreGuard::new(enable_terminal_modes(&mut stderr, &options.fullscreen)?);
    mode_restore_guard.state_mut().save_cursor_position(&mut stderr);
    if surface.use_alternate() {
        mode_restore_guard.state_mut().enter_alternate_screen(&mut stderr)?;
        // Record the surface so the canonical restore path can skip the
        // full-screen clear: leaving the alternate buffer already restores
        // the main screen (see panic_hook::restore_tui).
        crate::tui::core_tui::panic_hook::state::mark_alternate_screen_active(true);
    }
    mode_restore_guard
        .state_mut()
        .push_keyboard_enhancement_flags(&mut stderr, keyboard_flags);

    session.update_terminal_title();

    let backend = CrosstermBackend::new(stderr);
    let mut terminal = Terminal::new(backend).context("failed to initialize inline terminal")?;
    prepare_terminal(&mut terminal)?;

    // Create event listener and channels using the new async pattern
    let (mut input_listener, event_channels) = EventListener::new();
    let cancellation_token = CancellationToken::new();
    let event_loop_token = cancellation_token.clone();
    let event_channels_for_loop = event_channels.clone();
    let rx_paused = event_channels.rx_paused.clone();
    let last_input_elapsed_ms = event_channels.last_input_elapsed_ms.clone();
    let session_start = event_channels.session_start;

    // Ensure any capability or resize responses emitted during terminal setup are not treated as
    // the user's first keystrokes.
    drain_terminal_events();

    // Clone the sender before moving event_channels_for_loop into tokio::spawn.
    let event_tx_for_controller = event_channels_for_loop.tx.clone();

    // Spawn the async event loop after the terminal is fully configured so the first keypress is
    // captured immediately (avoids cooked-mode buffering before raw mode is enabled).
    let event_loop_handle = tokio::spawn(async move {
        spawn_event_loop(
            event_channels_for_loop.tx.clone(),
            event_loop_token,
            rx_paused,
            last_input_elapsed_ms,
            session_start,
        )
        .await;
    });

    let mut event_stream = EventStreamController::new(
        cancellation_token,
        event_loop_handle,
        event_tx_for_controller,
        event_channels.rx_paused.clone(),
        event_channels.last_input_elapsed_ms.clone(),
        event_channels.session_start,
    );

    let drive_result = drive_terminal(
        &mut terminal,
        &mut session,
        &mut commands,
        &events,
        &mut input_listener,
        event_channels,
        DriveRuntimeOptions {
            event_callback: options.event_callback,
            focus_callback: options.focus_callback,
            use_alternate_screen: surface.use_alternate(),
            input_activity_counter: options.input_activity_counter,
            keyboard_flags,
            fullscreen: options.fullscreen,
            preview_callback: options.preview_callback,
        },
        &mut event_stream,
    )
    .await;

    // Gracefully shutdown the event loop (may already be stopped by StopEventStream)
    event_stream.shutdown().await;

    // Drain any pending events before finalizing terminal and disabling modes
    drain_terminal_events();

    // When another party already restored the terminal (host backstop or panic
    // hook), the main screen buffer is live: writes here (line clear, cursor
    // show, clear) would land there instead of on the alternate screen.
    let is_alternate = surface.use_alternate();
    let finalize_result = if crate::tui::core_tui::panic_hook::is_restore_claimed() {
        Ok(())
    } else {
        finalize_terminal(&mut terminal, is_alternate)
    };

    // Restore terminal modes (handles all modes including raw mode)
    if let Err(error) = mode_restore_guard.restore() {
        tracing::warn!(%error, "failed to restore terminal modes");
    }

    // Clear terminal title on exit.
    session.clear_terminal_title();

    drive_result?;
    finalize_result?;

    clear_tui_log_sender();
    vtcode_commons::trace_flush::flush_trace_log();

    Ok(())
}
