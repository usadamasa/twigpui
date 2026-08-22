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
//! | `text_tertiary` on `bg` (#95) | 5.1:1 | pass |
//! | `button_label` on `accent` (idle button) | 5.7:1 | pass |
//! | `button_label` on `button_busy_bg` (busy button) | 6.9:1 | pass |
//! | `danger` on `bg` | 5.8:1 | pass |
//! | `warning` on `bg` (#18) | 5.0:1 | pass |
//! | `like` on `bg` (#95) | 5.8:1 | pass |
//! | `repost` on `bg` (#95) | 4.9:1 | pass |
//!
//! ## Why these are not macOS's literal system colors (#95)
//!
//! #95 settles the look as "follow macOS". Its *hues* are taken from the
//! system palette — systemBlue for the accent, systemRed for `like`,
//! systemGreen for `repost`, and the four-step label ramp — but the
//! luminances are not. Apple's own values fail the table above on a white
//! background: systemBlue (`#007AFF`) reaches 3.6:1, systemGreen
//! (`#34C759`) only 1.8:1, and `secondaryLabelColor` (black at 50% alpha)
//! 3.9:1. This project has documented AA for every text pair since #19, and
//! a look change is not a reason to drop that. So each system hue is kept
//! and darkened until it passes, which reads as the macOS palette without
//! shipping text nobody can read.

use gpui::{App, Pixels, Window, WindowAppearance, px};

/// Body text. macOS's own body style is 13pt, not the 14px `text_sm` the
/// window used to set globally (#95).
pub(crate) const TEXT_BODY: Pixels = px(13.0);

/// Handles, timestamps, engagement counts, and the status bar — macOS's
/// supplementary sizes sit at 11 (#95).
pub(crate) const TEXT_META: Pixels = px(11.0);

/// Buttons, fields, and anything else that reads as a control.
pub(crate) const RADIUS_CONTROL: Pixels = px(6.0);

/// Image thumbnails, which sit a step tighter than a control (#95).
pub(crate) const RADIUS_THUMB: Pixels = px(5.0);

/// How big a toolbar icon is drawn (#95) — matched to the meta text size
/// rather than the body size, since an icon in a toolbar is a control's
/// label, not prose.
pub(crate) const ICON_SIZE: Pixels = px(15.0);

/// How tall one attached image renders (#65), cut from 160px by #95.
///
/// Four attachments at the old height filled the window on their own,
/// which put the post under them off screen — the timeline stopped being a
/// timeline whenever someone posted a grid. At this height a full grid of
/// four still fits beside its neighbours, and a thumbnail is a thing you
/// click to see properly (#70) rather than the thing you read.
pub(crate) const MEDIA_CELL_HEIGHT: Pixels = px(96.0);

/// One timeline row's horizontal padding.
pub(crate) const ROW_PAD_X: Pixels = px(12.0);

/// One timeline row's vertical padding.
pub(crate) const ROW_PAD_Y: Pixels = px(8.0);

/// How far a row separator is indented from the left edge, so it starts
/// where the text does rather than under the avatar — the same inset Mail
/// and Messages use. [`AVATAR_SIZE`] + [`ROW_PAD_X`] + the row's gap (#95).
pub(crate) const SEPARATOR_INSET: Pixels = px(52.0);

/// The toolbar strip at the top of the window (#95).
pub(crate) const TOOLBAR_HEIGHT: Pixels = px(44.0);

/// The status bar at the bottom of the window (#95).
pub(crate) const STATUS_BAR_HEIGHT: Pixels = px(24.0);

/// The corner radius an avatar is drawn with (#98).
///
/// Lives here rather than in `ui.rs` so the two places that draw an avatar
/// — the downloaded image and the initial-carrying placeholder — cannot
/// drift apart, the same reason `AVATAR_SIZE` is one constant. They are
/// the same shape or the row visibly changes when a download lands.
///
/// Sized against [`AVATAR_SIZE`]'s 32px, and matched to
/// [`RADIUS_CONTROL`] (#95): on macOS a small square image reads as an app
/// icon at this radius, and using the control radius keeps one rounding in
/// the window rather than two that almost agree. Buttons share it — the
/// pill shapes went away with #95.
pub(crate) const AVATAR_RADIUS: Pixels = RADIUS_CONTROL;

/// The size one row's author avatar is drawn at (#64), reduced from 44px
/// to macOS's small-icon size by #95 — the old value came from X's own web
/// timeline, which is built for a much wider column.
///
/// Lives here rather than in `ui::render` so it stays next to
/// [`AVATAR_RADIUS`] and [`SEPARATOR_INSET`], both of which are derived
/// from it.
pub(crate) const AVATAR_SIZE: Pixels = px(32.0);

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
    /// De-emphasized text (bylines, engagement counts, placeholder
    /// notices) — the second step of macOS's four-level label ramp (#95).
    pub(crate) text_muted: u32,
    /// The third step of that ramp (#95): timestamps and the "· replying
    /// to" tail, which have to be readable but must not compete with the
    /// byline beside them.
    pub(crate) text_tertiary: u32,
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
    /// Whether this is the dark palette — kept alongside the color slots
    /// rather than re-derived by comparing them, so
    /// [`sync_gpui_component_theme`] (#38) has a single, direct source of
    /// truth for which of gpui-component's own light/dark modes to point at.
    pub(crate) is_dark: bool,
    /// The usage line's color while today's request count is approaching
    /// (but has not yet reached) a configured daily budget (#18) — distinct
    /// from `danger`, which is reserved for the budget actually being
    /// exceeded (and for errors), so the two severities read as visibly
    /// different at a glance.
    pub(crate) warning: u32,
    /// A liked post's action (#95) — systemRed's hue, darkened to clear
    /// the module doc's AA table. Its own slot rather than reusing
    /// `accent`: on macOS "on" states are colored by what they mean, and a
    /// like that reads the same blue as a link says nothing.
    pub(crate) like: u32,
    /// A reposted post's action (#95) — systemGreen's hue, darkened the
    /// same way and for the same reason as `like`.
    pub(crate) repost: u32,
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
            // A step below `text_muted` against the dark `bg`, mirroring
            // what `light` does in the other direction (#95).
            text_tertiary: 0x6b_7a_86,
            accent: 0x1d_9b_f0,
            // Reproduces the pre-#19 button exactly: the button used `BORDER`
            // as its busy fill and `TEXT` as its label, unconditionally.
            button_busy_bg: 0x38_44_4d,
            button_label: 0xf7_f9_f9,
            danger: 0xf4_21_2e,
            is_dark: true,
            // Amber-400-ish: ~9.9:1 against `bg` (0x15_20_2b) by the same
            // WCAG formula the module doc's light-palette table uses.
            warning: 0xfb_bf_24,
            // systemRed / systemGreen lightened for a dark ground, the
            // mirror of what `light` does to them (#95).
            like: 0xff_6b_6b,
            repost: 0x4c_d9_8f,
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
            // 5.1:1 — one readable step below `text_muted`. macOS's own
            // tertiaryLabelColor (black at 26% alpha, 3.4:1 over white)
            // would not clear the module doc's table.
            text_tertiary: 0x6e_6e_73,
            accent: 0x0b_65_c2,
            // A pale hairline border would leave a white `button_label`
            // unreadable, so the busy fill is a mid gray instead — see the
            // module doc's contrast table.
            button_busy_bg: 0x54_5b_63,
            button_label: 0xff_ff_ff,
            danger: 0xc4_1e_3a,
            is_dark: false,
            // Amber-700-ish: ~5.0:1 against `bg` (white), passing the same
            // AA text threshold (4.5:1) the module doc's table checks the
            // other slots against.
            warning: 0xb4_53_09,
            // systemRed's hue at `danger`'s luminance — the same color,
            // since "liked" and "failed" never sit next to each other.
            like: 0xc4_1e_3a,
            // systemGreen darkened from 1.8:1 to 4.9:1 (#95).
            repost: 0x1f_7a_4d,
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

/// Point gpui-component's own global theme at this project's resolved
/// palette (#38).
///
/// The composer's text input is a `gpui_component::input::Input`, which
/// reads its colors from `gpui_component::theme::Theme` — a global entirely
/// separate from this module's [`Theme`]. Without this, that global would
/// stay wherever `gpui_component::init` last left it (synced to the raw OS
/// appearance), which can disagree with what this project just resolved
/// from `config.theme` — e.g. `config.theme = "dark"` on a light-appearance
/// OS would otherwise leave the input rendering light-on-light against a
/// dark surrounding window. Called once, from [`crate::ui::TimelineView::new`]
/// right after [`ThemeMode::resolve`], with a real `Window` in hand — there
/// is no separate "System" case to handle here, `theme` is already concrete.
///
/// Only the color slots gpui-component's `Input` actually reads (per its
/// own source: `background`, `foreground`, `muted_foreground` for the
/// placeholder, `border`/`input` for the box outline, `caret`, `selection`,
/// and `accent`/`primary`/`ring` for focus styling) are pointed at this
/// project's palette; every other slot (menus, tables, charts, …) is left
/// at gpui-component's own default for the resolved light/dark mode, since
/// this app never renders any of those widgets.
pub(crate) fn sync_gpui_component_theme(theme: Theme, window: &mut Window, cx: &mut App) {
    use gpui_component::theme::{Theme as ComponentTheme, ThemeMode as ComponentThemeMode};

    let mode = if theme.is_dark {
        ComponentThemeMode::Dark
    } else {
        ComponentThemeMode::Light
    };
    ComponentTheme::change(mode, Some(window), cx);

    let colors = ComponentTheme::global_mut(cx);
    colors.background = gpui::rgb(theme.bg).into();
    colors.foreground = gpui::rgb(theme.text).into();
    colors.muted_foreground = gpui::rgb(theme.text_muted).into();
    colors.muted = gpui::rgb(theme.bg_header).into();
    colors.border = gpui::rgb(theme.border).into();
    colors.input = gpui::rgb(theme.border).into();
    colors.caret = gpui::rgb(theme.text).into();
    colors.selection = gpui::rgb(theme.accent).into();
    colors.accent = gpui::rgb(theme.accent).into();
    colors.accent_foreground = gpui::rgb(theme.button_label).into();
    colors.primary = gpui::rgb(theme.accent).into();
    colors.primary_foreground = gpui::rgb(theme.button_label).into();
    colors.ring = gpui::rgb(theme.accent).into();
    colors.danger = gpui::rgb(theme.danger).into();
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
        assert_ne!(light.text_tertiary, dark.text_tertiary);
        assert_ne!(light.accent, dark.accent);
        assert_ne!(light.like, dark.like);
        assert_ne!(light.repost, dark.repost);
        assert_ne!(light.button_busy_bg, dark.button_busy_bg);
        assert_ne!(light.button_label, dark.button_label);
        assert_ne!(light.danger, dark.danger);
        assert_ne!(light.is_dark, dark.is_dark);
        assert_ne!(light.warning, dark.warning);
    }

    #[test]
    fn warning_is_distinct_from_danger_in_both_palettes() {
        // #18: `usage_color` maps "near budget" to `warning` and "budget
        // exceeded" to `danger` — if the two colors were the same, the
        // header couldn't visually distinguish the two severities.
        assert_ne!(Theme::light().warning, Theme::light().danger);
        assert_ne!(Theme::dark().warning, Theme::dark().danger);
    }

    #[test]
    fn an_on_state_is_never_the_same_color_as_a_link() {
        // #95: `like` and `repost` exist so an "on" action says which
        // action it is. Collapsing either onto `accent` — the color every
        // link and the primary button already wear — would undo that, and
        // the two are near enough in hue that a careless edit could.
        for theme in [Theme::light(), Theme::dark()] {
            assert_ne!(theme.like, theme.accent);
            assert_ne!(theme.repost, theme.accent);
            assert_ne!(theme.like, theme.repost);
        }
    }

    #[test]
    fn the_three_label_steps_are_distinct_within_a_palette() {
        // #95: the ramp is only a ramp if the steps differ. A palette that
        // set two of them the same would render a byline and a timestamp
        // identically, which is the flattening this issue set out to fix.
        for theme in [Theme::light(), Theme::dark()] {
            assert_ne!(theme.text, theme.text_muted);
            assert_ne!(theme.text_muted, theme.text_tertiary);
            assert_ne!(theme.text, theme.text_tertiary);
        }
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
