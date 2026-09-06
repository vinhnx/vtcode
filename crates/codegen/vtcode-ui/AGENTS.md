# vtcode-ui

[Root AGENTS.md](../AGENTS.md) | Unified UI: design system, theme registry, TUI framework. Consolidated from `vtcode-design` and `vtcode-theme`.

## Modules

| Area | Path |
|---|---|
| Design system | `design/` — color conversion, style bridging, layout, diff, panel primitives |
| Theme registry | `theme/` — ThemeStyles, runtime state, syntax theme resolution |
| TUI framework | `tui/` — session, widgets, runner, markdown rendering, config |

## Rules

- `design` and `theme` are re-exported at crate root (`pub use design::*; pub use theme::*`) for backward compatibility with the old standalone crates.
- `publish = false` — internal crate, not published to crates.io.
- `tui/core_tui/` owns the full terminal session lifecycle; `tui/core_tui/app/session/task_panel.rs` owns compact TODO-panel wrapping/height/header helpers; `tui/ui/` has reusable widgets (Markdown, interactive list). Headered Markdown tables use intrinsic width when available and labeled wrapped blocks below it, so callers must pass content width after transcript framing. Bridge prompts use the bounded deferred-event queue while transient overlays own input; keep them prompt-only so slash-command parsing remains terminal-only.
- `tui/config/constants/` holds TUI-specific defaults — keep them here, not in `vtcode-config`; snapshot tests live in `tui/core_tui/widgets/snapshots/`.
## Gotchas
- Style bridging is centralized in `design/color.rs` + `design/style.rs` (crate-internal); downstream code uses the `tui/core_tui/style.rs` wrappers — do not fork new converters.
- The `crossterm` dependency enables `event-stream` and `osc52` features; do not duplicate these in downstream crates. It is a maintained fork at `patches/crossterm` (root `[patch.crates-io]`) that adds `Event::ColorSchemeReport` for the Contour `CSI ? 997`/`2031` dark-light extension — re-apply that patch when upgrading crossterm, and treat the fork as workspace code for `-D warnings`.
- Standalone and core session defaults are inline; callers that need alternate-screen rendering must opt in explicitly.
- Floating approval/list overlays own mouse input only inside `modal_list_area`; wheel events outside that hitbox must pass through to the transcript so long plan markdown remains scrollable.
- Floating overlays reuse one bottom-half rectangle for popup rendering and transcript clipping; keep `transcript_area` as the source of truth for scroll metrics and hit-testing, and do not pass an explicit transcript rectangle to `apply_view_rows` as if it were the full viewport before rendering (that breaks scroll-anchor restoration). Modal list hit-testing must go through `modal::visible_index_at_row` with the same `footer_hint`/inline-editor inputs render used, or clicks drift by the summary/editor rows.
- `ActivityState` is the authoritative busy/idle signal even without animation. Derive `InputOwner` from overlay/activity state; activity changes update modal restore flags. Transcript cache validity is explicit, not revision zero. Tick sends coalesce, but input and PTY bytes stay ordered.
- The shared active-PTY counter is also a global loading observer; compact PTY rendering may hide live rows, so keep its footer status fallback in `session/state.rs`.
- PTY/tool reflow must preserve explicit status color on the `•` prefix; apply action/tool styling only to the verb so success, failure, and warning remain visually distinct; fullscreen `Ctrl+T` opens ordered session-local Transcript Review (rich/raw via `r`) but remains text transpose outside fullscreen, and complete PTY captures stay behind bounded live lines. Compact review hints are the only normal-transcript open target; derive their label from the primary binding and rebuild their hit regions after transcript reflow.
- Tool and PTY blocks reserve at least one blank line above and below; shell syntax highlighting is accepted only when it produces distinct token colors, otherwise semantic token styles are the fallback.
- Task-panel tree rows use the shared hanging-prefix wrapper in `session/text_utils.rs`; keep panel row heights derived from wrapped content so transcript and docked panel stay aligned. `toggle_tool_display_mode` is a rebindable session action (default `Alt+T`); dispatch it before the legacy `Alt+T` text-edit shortcut and invalidate transcript caches after toggling. Info/Warning/Error transcript blocks must invalidate from their first line when a member changes or is appended, because later lines affect the cached block head; each Info tool-summary line is a boundary, not part of the box.
- Panic-hook terminal mutation tracking is set only after a successful terminal mutation, so partial TUI setup errors do not emit restore sequences. Alternate-screen teardown clears the alternate viewport before leaving it, and render/finalize writes must use the shared terminal-operation lock so no final frame reaches the main scrollback after restoration is claimed.
