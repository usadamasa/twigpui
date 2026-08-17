//! Color theme (#19). [`Theme`] bundles every color slot the UI's render
//! helpers need into one `Copy` value, so it can be threaded through
//! `TimelineView`'s free functions without a lifetime or a global.
//!
//! [`ThemeMode`] is the user-facing setting (`light` / `dark` / `system`),
//! resolved from `config.toml` / `X_THEME` in [`crate::config::Config`].
//! [`ThemeMode::resolve`] turns that into a concrete [`Theme`], consulting
//! the OS appearance only for `System`, via gpui's `Window::appearance()`.
//!
//! ## Light palette contrast
//!
//! Computed with the WCAG 2 relative-luminance formula —
//! `(L1 + 0.05) / (L2 + 0.05)` over linearized sRGB channels — against the
//! values in [`Theme::light`]:
//!
//! | Pair | Ratio | AA text threshold (4.5:1) |
//! | --- | --- | --- |
//! | `text` on `bg` | 18.5:1 | pass |
//! | `text_muted` on `bg` | 6.9:1 | pass |
//! | `button_label` on `accent` (idle button) | 5.7:1 | pass |
//! | `button_label` on `button_busy_bg` (busy button) | 6.9:1 | pass |
//! | `danger` on `bg` | 5.8:1 | pass |

use gpui::WindowAppearance;

/// One color slot per named UI role, replacing the `BG` / `TEXT` / ... `u32`
/// constants that used to live directly in `ui.rs`. Grouped per RGB channel,
/// which is also the digit grouping clippy asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Theme {
    /// Main window background.
    pub(crate) bg: u32,
    /// Header bar background — distinct from `bg` so the header reads as a
    /// separate region.
    pub(crate) bg_header: u32,
    /// Header/row separator lines.
    pub(crate) border: u32,
    /// Primary body text.
    pub(crate) text: u32,
    /// De-emphasized text (bylines, timestamps, placeholder notices).
    pub(crate) text_muted: u32,
    /// The primary action button's fill while idle (clickable).
    pub(crate) accent: u32,
    /// The primary action button's fill while busy/disabled — deliberately
    /// its own slot rather than reusing `border`: a light theme's hairline
    /// border is far too pale to keep `button_label` legible as a button
    /// fill (see the module doc's contrast table).
    pub(crate) button_busy_bg: u32,
    /// Text drawn on top of the primary action button, in either fill state
    /// above. Its own slot rather than reusing `text`: on a light theme,
    /// body text is near-black, which fails contrast against `accent`.
    pub(crate) button_label: u32,
    /// Error and rate-limit text.
    pub(crate) danger: u32,
}

impl Theme {
    /// The palette twigpui shipped with before #19, carried over unchanged
    /// so switching to `dark` reproduces the old look exactly.
    pub(crate) const fn dark() -> Self {
        Self {
            bg: 0x15_20_2b,
            bg_header: 0x1b_28_36,
            border: 0x38_44_4d,
            text: 0xf7_f9_f9,
            text_muted: 0x88_99_a6,
            accent: 0x1d_9b_f0,
            // Reproduces the pre-#19 button exactly: the button used `BORDER`
            // as its busy fill and `TEXT` as its label, unconditionally.
            button_busy_bg: 0x38_44_4d,
            button_label: 0xf7_f9_f9,
            danger: 0xf4_21_2e,
        }
    }

    /// The light palette #19 makes the default. See the module doc for the
    /// contrast ratios behind these values.
    pub(crate) const fn light() -> Self {
        Self {
            bg: 0xff_ff_ff,
            bg_header: 0xf5_f7_f8,
            border: 0xd7_dc_e0,
            text: 0x0f_14_19,
            text_muted: 0x54_5b_63,
            accent: 0x0b_65_c2,
            // A pale hairline border would leave a white `button_label`
            // unreadable, so the busy fill is a mid gray instead — see the
            // module doc's contrast table.
            button_busy_bg: 0x54_5b_63,
            button_label: 0xff_ff_ff,
            danger: 0xc4_1e_3a,
        }
    }
}

/// The `theme` setting as configured — distinct from [`Theme`] itself
/// because `System` has no fixed color values until resolved against the
/// window's actual OS appearance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ThemeMode {
    /// Always the light palette.
    #[default]
    Light,
    /// Always the dark palette.
    Dark,
    /// Follows the OS appearance, via gpui's `Window::appearance()`.
    System,
}

impl ThemeMode {
    /// Parse a `theme` setting value (`X_THEME` or `config.toml`'s `theme`
    /// key). Case-insensitive and trims whitespace, matching how
    /// `Config::resolve` treats its other string settings. `None` for
    /// anything else — the caller decides how to report a bad value, since
    /// an unrecognized theme must not fail startup (#19).
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            "system" => Some(Self::System),
            _ => None,
        }
    }

    /// Resolve to a concrete [`Theme`]. `appearance` is only consulted for
    /// `System` — `Light` and `Dark` are fixed regardless of the OS setting.
    pub(crate) fn resolve(self, appearance: WindowAppearance) -> Theme {
        match self {
            Self::Light => Theme::light(),
            Self::Dark => Theme::dark(),
            Self::System => match appearance {
                WindowAppearance::Light | WindowAppearance::VibrantLight => Theme::light(),
                WindowAppearance::Dark | WindowAppearance::VibrantDark => Theme::dark(),
            },
        }
    }
}

impl std::fmt::Display for ThemeMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::System => "system",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Theme, ThemeMode};
    use gpui::WindowAppearance;

    #[test]
    fn light_and_dark_are_distinct_in_every_slot() {
        let light = Theme::light();
        let dark = Theme::dark();
        assert_ne!(light.bg, dark.bg);
        assert_ne!(light.bg_header, dark.bg_header);
        assert_ne!(light.border, dark.border);
        assert_ne!(light.text, dark.text);
        assert_ne!(light.text_muted, dark.text_muted);
        assert_ne!(light.accent, dark.accent);
        assert_ne!(light.button_busy_bg, dark.button_busy_bg);
        assert_ne!(light.button_label, dark.button_label);
        assert_ne!(light.danger, dark.danger);
    }

    #[test]
    fn light_and_dark_ignore_the_os_appearance() {
        assert_eq!(
            ThemeMode::Light.resolve(WindowAppearance::Dark),
            Theme::light()
        );
        assert_eq!(
            ThemeMode::Dark.resolve(WindowAppearance::Light),
            Theme::dark()
        );
    }

    #[test]
    fn system_follows_a_light_os_appearance() {
        assert_eq!(
            ThemeMode::System.resolve(WindowAppearance::Light),
            Theme::light()
        );
        assert_eq!(
            ThemeMode::System.resolve(WindowAppearance::VibrantLight),
            Theme::light()
        );
    }

    #[test]
    fn system_follows_a_dark_os_appearance() {
        assert_eq!(
            ThemeMode::System.resolve(WindowAppearance::Dark),
            Theme::dark()
        );
        assert_eq!(
            ThemeMode::System.resolve(WindowAppearance::VibrantDark),
            Theme::dark()
        );
    }

    #[test]
    fn defaults_to_light() {
        assert_eq!(ThemeMode::default(), ThemeMode::Light);
    }

    #[test]
    fn parses_known_values_case_insensitively_and_trims_whitespace() {
        assert_eq!(ThemeMode::parse("light"), Some(ThemeMode::Light));
        assert_eq!(ThemeMode::parse("  LIGHT\n"), Some(ThemeMode::Light));
        assert_eq!(ThemeMode::parse("Dark"), Some(ThemeMode::Dark));
        assert_eq!(ThemeMode::parse("SYSTEM"), Some(ThemeMode::System));
        assert_eq!(ThemeMode::parse(" system "), Some(ThemeMode::System));
    }

    #[test]
    fn rejects_an_unrecognized_value() {
        assert_eq!(ThemeMode::parse("solarized"), None);
        assert_eq!(ThemeMode::parse(""), None);
    }

    #[test]
    fn display_matches_the_parse_keywords() {
        // The fallback warning in Config::resolve embeds this, so it needs
        // to round-trip through parse() rather than drifting from it.
        for mode in [ThemeMode::Light, ThemeMode::Dark, ThemeMode::System] {
            assert_eq!(ThemeMode::parse(&mode.to_string()), Some(mode));
        }
    }
}
