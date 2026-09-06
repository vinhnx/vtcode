use anstyle::{Color, RgbColor, Style};

use crate::theme::color_math::contrast_ratio;
use crate::theme::registry::all_theme_definitions;
use crate::*;

#[test]
fn test_mono_theme_exists() {
    let result = ensure_theme("mono");
    assert!(result.is_ok(), "Mono theme should be registered");
    assert_eq!(result.unwrap(), "Mono");
}

#[test]
fn test_mono_theme_contrast() {
    let result = validate_theme_contrast("mono");
    assert!(result.errors.is_empty(), "Mono theme should have no errors");
    assert!(result.is_valid);
}

#[test]
fn test_ansi_classic_theme_exists() {
    let result = ensure_theme("ansi-classic");
    assert!(result.is_ok(), "ANSI Classic theme should be registered");
    assert_eq!(result.unwrap(), "ANSI Classic");
}

#[test]
fn test_all_themes_resolvable() {
    for id in available_themes() {
        assert!(ensure_theme(id).is_ok(), "Theme {id} should be resolvable");
    }
}

#[test]
fn test_available_theme_suites_contains_expected_groups() {
    let suites = available_theme_suites();
    let suite_ids: Vec<&str> = suites.iter().map(|suite| suite.id).collect();
    assert!(suite_ids.contains(&"ciapre"));
    assert!(suite_ids.contains(&"vitesse"));
    assert!(suite_ids.contains(&"catppuccin"));
    assert!(suite_ids.contains(&"mono"));
}

#[test]
fn test_theme_suite_resolution() {
    assert_eq!(theme_suite_id("catppuccin-mocha"), Some("catppuccin"));
    assert_eq!(theme_suite_id("vitesse-light"), Some("vitesse"));
    assert_eq!(theme_suite_id("ciapre-dark"), Some("ciapre"));
    assert_eq!(theme_suite_id("mono"), Some("mono"));
    assert_eq!(theme_suite_id("unknown-theme"), None);
}

#[test]
fn test_all_themes_have_readable_foreground_and_accents() {
    let accessibility = ColorAccessibilityConfig::default();
    let min_contrast = accessibility.minimum_contrast;
    for definition in all_theme_definitions().values() {
        let styles = definition.palette.build_styles_with_accessibility(&accessibility);
        let bg = definition.palette.background;

        for (name, color) in [
            ("foreground", style_rgb(styles.output)),
            ("primary", style_rgb(styles.primary)),
            ("secondary", style_rgb(styles.secondary)),
            ("user", style_rgb(styles.user)),
            ("response", style_rgb(styles.response)),
            ("info", style_rgb(styles.info)),
            ("error", style_rgb(styles.error)),
            ("reasoning", style_rgb(styles.reasoning)),
            ("tool", style_rgb(styles.tool)),
            ("tool_detail", style_rgb(styles.tool_detail)),
            ("tool_output", style_rgb(styles.tool_output).or(style_rgb(styles.output))),
            ("pty_output", style_rgb(styles.pty_output)),
            ("status", style_rgb(styles.status)),
            ("mcp", style_rgb(styles.mcp)),
        ] {
            let color = color.unwrap_or_else(|| panic!("{} missing fg color for {}", name, definition.id));
            let ratio = contrast_ratio(color, bg);
            assert!(
                ratio >= min_contrast,
                "theme={} style={} contrast {:.2} < {:.1}",
                definition.id,
                name,
                ratio,
                min_contrast
            );
        }
    }
}

#[test]
fn test_all_themes_error_accent_meets_contrast() {
    // The error/alert token backs the Blocked header badge and error copy, so
    // it must meet the WCAG AA contrast floor against the background in every
    // built-in theme. Unlike body-text tokens it is an accent, so the
    // readability luminance window does not apply.
    let accessibility = ColorAccessibilityConfig::default();
    let min_contrast = accessibility.minimum_contrast;
    for definition in all_theme_definitions().values() {
        let styles = definition.palette.build_styles_with_accessibility(&accessibility);
        let color = style_rgb(styles.error).unwrap_or_else(|| panic!("error token missing fg for {}", definition.id));
        let ratio = contrast_ratio(color, definition.palette.background);
        assert!(
            ratio >= min_contrast,
            "theme={} error accent contrast {:.2} < {:.1}",
            definition.id,
            ratio,
            min_contrast
        );
    }
}

#[test]
fn test_syntax_theme_mapping_dark_themes() {
    assert_eq!(get_syntax_theme_for_ui_theme("dracula"), "Dracula");
    assert_eq!(get_syntax_theme_for_ui_theme("monokai-classic"), "monokai-classic");
    assert_eq!(get_syntax_theme_for_ui_theme("github-dark"), "GitHub Dark");
    assert_eq!(get_syntax_theme_for_ui_theme("atom-one-dark"), "OneDark");
    assert_eq!(get_syntax_theme_for_ui_theme("ayu"), "ayu-dark");
    assert_eq!(get_syntax_theme_for_ui_theme("ayu-mirage"), "ayu-mirage");
}

#[test]
fn test_syntax_theme_mapping_light_themes() {
    assert_eq!(get_syntax_theme_for_ui_theme("solarized-light"), "Solarized (light)");
    assert_eq!(get_syntax_theme_for_ui_theme("vitesse-light"), "base16-ocean.light");
    assert_eq!(get_syntax_theme_for_ui_theme("apple-system-colors-light"), "base16-ocean.light");
}

#[test]
fn test_syntax_theme_mapping_solarized() {
    assert_eq!(get_syntax_theme_for_ui_theme("solarized-dark"), "Solarized (dark)");
    assert_eq!(get_syntax_theme_for_ui_theme("solarized-dark-hc"), "Solarized (dark)");
}

#[test]
fn test_syntax_theme_mapping_gruvbox() {
    assert_eq!(get_syntax_theme_for_ui_theme("gruvbox-dark"), "gruvbox-dark");
    assert_eq!(get_syntax_theme_for_ui_theme("gruvbox-light"), "gruvbox-light");
    assert_eq!(get_syntax_theme_for_ui_theme("gruvbox-material"), "gruvbox-dark");
    assert_eq!(get_syntax_theme_for_ui_theme("gruvbox-material-light"), "gruvbox-light");
}

#[test]
fn test_theme_for_terminal_scheme_change_prefers_suite_twin() {
    // Catppuccin spans both schemes: a report must land on the suite twin.
    let light_twin = theme_for_terminal_scheme_change("catppuccin-mocha", false);
    assert_eq!(theme_suite_id(light_twin), Some("catppuccin"));
    assert!(is_light_theme(light_twin));

    let dark_twin = theme_for_terminal_scheme_change("catppuccin-latte", true);
    assert_eq!(theme_suite_id(dark_twin), Some("catppuccin"));
    assert!(!is_light_theme(dark_twin));
}

#[test]
fn test_theme_for_terminal_scheme_change_falls_back_without_twin() {
    // mono is a single dark theme with no light twin; a light report falls
    // back to the default suggestion for the reported scheme.
    vtcode_commons::ansi_capabilities::set_color_scheme_override(Some(
        vtcode_commons::ansi_capabilities::ColorScheme::Light,
    ));
    assert_eq!(theme_for_terminal_scheme_change("mono", false), "vitesse-light");
    vtcode_commons::ansi_capabilities::set_color_scheme_override(None);
}

#[test]
fn test_theme_for_terminal_scheme_change_falls_back_for_unknown_theme() {
    vtcode_commons::ansi_capabilities::set_color_scheme_override(Some(
        vtcode_commons::ansi_capabilities::ColorScheme::Dark,
    ));
    assert_eq!(theme_for_terminal_scheme_change("not-a-theme", true), DEFAULT_THEME_ID);
    vtcode_commons::ansi_capabilities::set_color_scheme_override(None);
}

fn style_rgb(style: Style) -> Option<RgbColor> {
    match style.get_fg_color() {
        Some(Color::Rgb(rgb)) => Some(rgb),
        _ => None,
    }
}
