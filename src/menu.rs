//! The keyboard bindings (#58) and the macOS menu bar (#99).
//!
//! Split out of `ui` because it is the one part of the front end that is
//! not about rendering a timeline: `main` registers all of it before the
//! window exists, and the menu bar outlives any particular window. Keeping
//! it here also keeps every place a keystroke is named inside one file.

gpui::actions!(
    twigpui,
    [
        /// Reload the timeline (#58). Spends API requests, so it is bound
        /// to `cmd-r` — the reload gesture every app shares, and not a key
        /// anyone hits by accident.
        Reload,
        /// Move focus into the composer (#58).
        FocusComposer,
        /// Move focus out of the composer (#58), leaving the draft alone.
        BlurComposer,
        /// Quit the application (#99). gpui ships no quit action of its
        /// own, and without one the app menu has nothing to hang `cmd-q`
        /// on — which is how twigpui ended up quittable only from the
        /// Dock. Handled in `main`, at the `App` level rather than on the
        /// window's root: a global handler still fires when no window
        /// holds focus, which is exactly when someone reaches for `cmd-q`.
        Quit,
        /// Show the About panel (#99) — the other half of the app menu
        /// every macOS application has.
        ShowAbout,
        /// Minimise the window to the Dock (#109), bound to `cmd-m`.
        Minimize,
        /// Close the window (#109), bound to `cmd-w`. With one window,
        /// this ends the app just as `cmd-q` does — see [`CLOSE_WINDOW`].
        CloseWindow,
        /// Jump the timeline back to the newest post (#22), bound to
        /// `cmd-up`. Purely local — it spends nothing.
        ScrollToTop,
    ]
);

/// The key context the timeline's root element carries (#58) — every
/// binding below except [`QUIT`] (#99) is scoped to it rather than
/// registered globally, so a future single-key binding cannot fire while
/// another view has focus.
pub(crate) const KEY_CONTEXT: &str = "Timeline";

/// One binding, defined once (#99).
///
/// Before the menu bar existed, a keystroke was written in `init` and its
/// glyphs in [`shortcuts`], and nothing tied the two together. A menu item
/// would have been a third copy, so the three now read the same constant:
/// `init` binds [`Shortcut::keystroke`], the header prints
/// [`Shortcut::glyphs`], and [`menus`] labels the item. The menu's own key
/// equivalent is not written here at all — macOS resolves it from the
/// keymap, so `keystroke` remains the only place a key is named.
struct Shortcut {
    /// The keystroke as `gpui::KeyBinding::new` parses it.
    keystroke: &'static str,
    /// The key context the binding is scoped to, or `None` to register it
    /// globally. Only [`QUIT`] is global — see [`init`].
    context: Option<&'static str>,
    /// Builds this shortcut's key binding, closing over the action (#119).
    ///
    /// A `const` cannot hold an `impl Action`, but a closure capturing
    /// nothing coerces to a function pointer — enough to name the action
    /// here instead of at every use. Before this, `init` and [`menus`]
    /// paired shortcut with action by hand, and
    /// `menu_item(&RELOAD, FocusComposer)` type-checked into a menu item
    /// labelled Reload that focused the composer under `cmd-n`.
    bind: fn(&'static str, Option<&'static str>) -> gpui::KeyBinding,
    /// Builds this shortcut's menu item, closing over the same action as
    /// [`Shortcut::bind`] — the pairing is written once, in this constant.
    item: fn(&'static str) -> gpui::MenuItem,
    /// The same keystroke written for a human — the header's badge (#58).
    glyphs: &'static str,
    /// How the header names the action.
    label: &'static str,
    /// Whether the header's hint strip (#58) advertises it. The strip is
    /// what this app does that another one would not, so `cmd-q` stays off
    /// it — the one binding on the list nobody needs told about.
    in_header: bool,
    /// How the menu bar names the action, or `None` to keep it out of the
    /// menu bar. The wordings differ on purpose: a menu item is read on
    /// its own ("New Post"), while the header's strip is read as a row of
    /// hints under a heading ("⌘N Focus the composer").
    menu_label: Option<&'static str>,
}

/// Reload the timeline. Spends API requests, so it takes the reload
/// gesture every app shares rather than a key anyone hits by accident.
const RELOAD: Shortcut = Shortcut {
    keystroke: "cmd-r",
    context: Some(KEY_CONTEXT),
    bind: |keystroke, context| gpui::KeyBinding::new(keystroke, Reload, context),
    item: |label| gpui::MenuItem::action(label, Reload),
    glyphs: "⌘R",
    label: "Reload",
    in_header: true,
    menu_label: Some("Reload"),
};

/// Move focus into the composer.
const FOCUS_COMPOSER: Shortcut = Shortcut {
    keystroke: "cmd-n",
    context: Some(KEY_CONTEXT),
    bind: |keystroke, context| gpui::KeyBinding::new(keystroke, FocusComposer, context),
    item: |label| gpui::MenuItem::action(label, FocusComposer),
    glyphs: "⌘N",
    label: "Focus the composer",
    in_header: true,
    menu_label: Some("New Post"),
};

/// Leave the composer. Absent from the menu bar: "put focus back" is a
/// gesture, not a command anyone goes looking for in a menu.
const BLUR_COMPOSER: Shortcut = Shortcut {
    keystroke: "escape",
    context: Some(KEY_CONTEXT),
    bind: |keystroke, context| gpui::KeyBinding::new(keystroke, BlurComposer, context),
    item: |label| gpui::MenuItem::action(label, BlurComposer),
    glyphs: "esc",
    label: "Leave the composer",
    in_header: true,
    menu_label: None,
};

/// Quit (#99). The only binding registered with no key context, and the
/// only one the header does not advertise — see [`init`].
const QUIT: Shortcut = Shortcut {
    keystroke: "cmd-q",
    context: None,
    bind: |keystroke, context| gpui::KeyBinding::new(keystroke, Quit, context),
    item: |label| gpui::MenuItem::action(label, Quit),
    glyphs: "⌘Q",
    label: "Quit",
    in_header: false,
    menu_label: Some("Quit twigpui"),
};

/// Minimise (#109). Off the header strip for the same reason as [`QUIT`]:
/// it is a macOS gesture, not something this app invented.
const MINIMIZE: Shortcut = Shortcut {
    keystroke: "cmd-m",
    context: Some(KEY_CONTEXT),
    bind: |keystroke, context| gpui::KeyBinding::new(keystroke, Minimize, context),
    item: |label| gpui::MenuItem::action(label, Minimize),
    glyphs: "⌘M",
    label: "Minimize",
    in_header: false,
    menu_label: Some("Minimize"),
};

/// Close the window (#109).
///
/// With a single window this ends the app, the same as [`QUIT`] — which is
/// what `cmd-w` does in any one-window macOS app, so it is not worth
/// making it behave differently here. It shares `cmd-q`'s hazard of
/// discarding an unsent draft (#14) and, like `cmd-q`, does not prompt.
const CLOSE_WINDOW: Shortcut = Shortcut {
    keystroke: "cmd-w",
    context: Some(KEY_CONTEXT),
    bind: |keystroke, context| gpui::KeyBinding::new(keystroke, CloseWindow, context),
    item: |label| gpui::MenuItem::action(label, CloseWindow),
    glyphs: "⌘W",
    label: "Close Window",
    in_header: false,
    menu_label: Some("Close Window"),
};

/// Back to the newest post (#22).
///
/// On the header strip, unlike the other additions since #58: it is the
/// one binding here that answers a question the reader actually has while
/// scrolled a long way down, and it costs nothing to press.
const SCROLL_TO_TOP: Shortcut = Shortcut {
    keystroke: "cmd-up",
    context: Some(KEY_CONTEXT),
    bind: |keystroke, context| gpui::KeyBinding::new(keystroke, ScrollToTop, context),
    item: |label| gpui::MenuItem::action(label, ScrollToTop),
    glyphs: "⌘↑",
    label: "Back to the top",
    in_header: true,
    menu_label: Some("Back to Top"),
};

/// Every binding, in the order [`init`] registers them (#99).
///
/// [`init`] registers exactly this list, [`shortcuts`] filters it to what
/// the header advertises, and [`menus`] draws its items from the same
/// entries. Since #119 every entry also carries its own action, so a
/// shortcut added here is bound to the right thing or not bound at all —
/// there is no longer a second table to keep in step.
///
/// [`menus`] still chooses which menu each item belongs to, so a new
/// shortcut with a `menu_label` and no place in `menus` remains possible.
/// That is what `every_menu_labelled_shortcut_is_in_the_menu_bar` catches.
///
/// **A `Shortcut` left out of this array is not bound to anything**, and
/// nothing about the constant itself says so — which is how `MINIMIZE`,
/// `CLOSE_WINDOW` and `SCROLL_TO_TOP` sat here unbound after #109 and #22
/// added them everywhere except this list. `every_menu_item_has_a_binding`
/// is the test that now catches it.
const ALL_SHORTCUTS: [&Shortcut; 7] = [
    &RELOAD,
    &FOCUS_COMPOSER,
    &BLUR_COMPOSER,
    &QUIT,
    &MINIMIZE,
    &CLOSE_WINDOW,
    &SCROLL_TO_TOP,
];

/// Register #58's key bindings. Called once at startup, next to
/// `gpui_component::init` (which registers its own).
///
/// **No binding here is a bare printable key.** The issue's central hazard
/// is a bare `j`/`k`/`n` firing while the user is typing a post; nothing
/// bound here can, because every binding either carries `cmd` or is a
/// named key that types nothing (`escape`). When post selection arrives
/// and bare letters become worth having, they will need a second key
/// context that the composer's focus removes — [`KEY_CONTEXT`] is where
/// that starts.
///
/// Walks [`ALL_SHORTCUTS`] rather than listing the bindings again (#119):
/// each entry already names its own action through [`Shortcut::bind`], so
/// there is no second place where a shortcut and an action are paired up.
///
/// [`QUIT`] is the one entry with no key context (#99). The others answer
/// a question about the timeline and belong to the view that answers it;
/// quitting is not the window's business, and scoping it would mean
/// `cmd-q` doing nothing whenever focus sat anywhere else.
///
/// **Nothing here submits a post (#142).** `cmd-enter` did from #58 until
/// the composer's button turned out to be the only way anyone reached for.
/// Plain `enter` was never bound and still is not, for the reason that
/// outlives the removal: it has to keep inserting a newline, and a post is
/// not undoable. Should a keyboard route ever come back, that is still the
/// constraint it has to satisfy.
pub(crate) fn init(cx: &mut gpui::App) {
    cx.bind_keys(
        ALL_SHORTCUTS
            .iter()
            .map(|shortcut| (shortcut.bind)(shortcut.keystroke, shortcut.context)),
    );
}

/// The application's menu bar (#99), registered by `main` before the
/// window opens.
///
/// Every item's key equivalent comes from the keymap [`init`] registered,
/// so this names actions and wordings only — never a keystroke.
pub(crate) fn menus() -> Vec<gpui::Menu> {
    vec![
        gpui::Menu {
            name: "twigpui".into(),
            items: vec![
                gpui::MenuItem::action("About twigpui", ShowAbout),
                gpui::MenuItem::separator(),
            ]
            .into_iter()
            .chain(QUIT.menu_item())
            .collect(),
        },
        gpui::Menu {
            name: "File".into(),
            items: FOCUS_COMPOSER.menu_item().into_iter().collect(),
        },
        gpui::Menu {
            name: "View".into(),
            items: [RELOAD.menu_item(), SCROLL_TO_TOP.menu_item()]
                .into_iter()
                .flatten()
                .collect(),
        },
        // The name is load-bearing (#109): gpui's macOS platform hands a
        // menu to AppKit's `setWindowsMenu_` only when it is called
        // exactly "Window" (`gpui/src/platform/mac/platform.rs`'s
        // `create_menu_bar`). Rename it and `cmd-w`/`cmd-m` keep working —
        // they are ordinary bindings — but the menu stops being the one
        // macOS treats as the window list.
        gpui::Menu {
            name: "Window".into(),
            items: [MINIMIZE.menu_item(), CLOSE_WINDOW.menu_item()]
                .into_iter()
                .flatten()
                .collect(),
        },
    ]
}

/// The menu item for one shortcut, or nothing when [`Shortcut::menu_label`]
/// says it is deliberately absent from the menu bar (#99).
impl Shortcut {
    /// This shortcut's menu item, or nothing when [`Shortcut::menu_label`]
    /// says it is deliberately absent from the menu bar (#99).
    ///
    /// Takes no action argument (#119): it comes from the same constant
    /// as the label, so there is no second place to get it wrong.
    fn menu_item(&self) -> Option<gpui::MenuItem> {
        self.menu_label.map(self.item)
    }
}

/// The shortcut list shown in the header (#58) and mirrored in the README —
/// one source for both, so a binding cannot be added without the list that
/// tells the user it exists.
///
/// Deliberately short. Anything spending an API request beyond `cmd-r` is
/// absent: "Load older" pages backwards one paid request per press, and a
/// key that spends money on a mis-hit is not a convenience.
pub(crate) fn shortcuts() -> Vec<(&'static str, &'static str)> {
    ALL_SHORTCUTS
        .iter()
        .filter(|shortcut| shortcut.in_header)
        .map(|shortcut| (shortcut.glyphs, shortcut.label))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{ALL_SHORTCUTS, menus, shortcuts};

    // --- #58: keyboard shortcuts ---

    #[test]
    fn no_shortcut_is_a_bare_letter() {
        // The issue's central hazard: a bare `j`/`k`/`n` firing while the
        // user is typing a post. This has to walk `ALL_SHORTCUTS`, not
        // `shortcuts()`: the latter is filtered to what the header
        // advertises, so a binding registered with `in_header: false` —
        // `QUIT` (#99) today, maybe another one tomorrow — would go
        // unchecked. It also has to compare `keystroke`, not `glyphs`:
        // `keystroke` is what `init` actually hands to `KeyBinding::new`,
        // so that is what determines whether gpui fires the binding while
        // someone is typing.
        //
        // `"escape"` is allowed with no modifier: it is a named special
        // key, not a letter that ordinary typing would produce.
        for shortcut in ALL_SHORTCUTS {
            let keystroke = shortcut.keystroke;
            assert!(
                keystroke.starts_with("cmd-") || keystroke == "escape",
                "{} is bound to {keystroke}, which would fire while typing",
                shortcut.label
            );
        }
    }

    #[test]
    fn every_shortcut_is_labelled() {
        for (key, label) in shortcuts() {
            assert!(!key.is_empty(), "a shortcut with no key");
            assert!(!label.is_empty(), "{key} has no label");
        }
    }

    #[test]
    fn no_key_is_bound_twice() {
        let mut keys: Vec<&str> = shortcuts().iter().map(|(key, _)| *key).collect();
        keys.sort_unstable();
        let before = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), before, "two actions share a key");
    }

    #[test]
    fn load_older_has_no_shortcut() {
        // Each press pages backwards for one paid request. A key that
        // spends money on a mis-hit is not a convenience (#58).
        assert!(
            !shortcuts()
                .iter()
                .any(|(_, label)| label.to_lowercase().contains("older")),
            "\"Load older\" must not be bound"
        );
    }

    // --- #99: the menu bar ---

    /// The names of every action item in the menu bar, submenus included.
    fn menu_action_names() -> Vec<String> {
        menus()
            .into_iter()
            .flat_map(|menu| menu.items)
            .filter_map(|item| match item {
                gpui::MenuItem::Action { name, .. } => Some(name.to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn every_menu_item_has_a_binding() {
        // The direction the other tests missed. They all start from
        // `ALL_SHORTCUTS` and check what is in it, so a `Shortcut` left out
        // of that array is invisible to every one of them -- while still
        // appearing in `menus()`, since that names its constants directly.
        //
        // Which is exactly what happened: #109's Minimize and Close Window
        // and #22's Back to Top reached the menu bar and never reached the
        // keymap, so `cmd-m`, `cmd-w` and `cmd-up` did nothing and macOS
        // drew no key equivalent beside any of them. The menu item worked,
        // because it carries its own action; the keystroke had nothing to
        // match against.
        //
        // Walking from the menu inward is what makes the omission visible.
        let bound: Vec<&str> = ALL_SHORTCUTS
            .iter()
            .filter_map(|shortcut| shortcut.menu_label)
            .collect();

        for name in menu_action_names() {
            // `About twigpui` is a menu item with no shortcut by design --
            // the only one, and it is spelled out here rather than skipped
            // by a rule, so a second one cannot slip past.
            if name == "About twigpui" {
                continue;
            }
            assert!(
                bound.contains(&name.as_str()),
                "{name} is in the menu bar but not in ALL_SHORTCUTS, so its keystroke is never bound"
            );
        }
    }

    #[test]
    fn no_keystroke_is_bound_twice() {
        // `no_key_is_bound_twice` covers the header's four. This covers
        // every binding `init` registers, including the ones the header
        // does not advertise (#99's `cmd-q`), and it compares the
        // keystrokes gpui actually parses rather than their glyphs.
        let mut keystrokes: Vec<&str> = ALL_SHORTCUTS
            .iter()
            .map(|shortcut| shortcut.keystroke)
            .collect();
        keystrokes.sort_unstable();
        let before = keystrokes.len();
        keystrokes.dedup();
        assert_eq!(keystrokes.len(), before, "two actions share a keystroke");
    }

    #[test]
    fn every_menu_labelled_shortcut_is_in_the_menu_bar() {
        // The drift #99 asks to prevent: a binding gaining a menu label
        // and never reaching a menu, or losing its item and keeping the
        // label. Both directions are checked, so `menu_label` means
        // exactly what it says.
        let names = menu_action_names();
        for shortcut in ALL_SHORTCUTS {
            match shortcut.menu_label {
                Some(label) => assert!(
                    names.iter().any(|name| name == label),
                    "{label} has a menu label but no menu item"
                ),
                None => assert!(
                    !names.iter().any(|name| name == shortcut.label),
                    "{} is in the menu bar without a menu label",
                    shortcut.label
                ),
            }
        }
    }

    #[test]
    fn the_menu_bar_can_quit() {
        // #99's whole reason for existing: before it, the only way out of
        // the app was the Dock's context menu.
        assert!(
            menu_action_names()
                .iter()
                .any(|name| name.to_lowercase().contains("quit")),
            "no menu item quits the app"
        );
    }

    #[test]
    fn the_window_menu_is_named_exactly_window() {
        // gpui hands a menu to AppKit's `setWindowsMenu_` only on an exact
        // name match (#109). A rename would leave `cmd-w`/`cmd-m` working
        // while quietly demoting the menu to an ordinary one, which is not
        // the kind of regression anyone notices from a diff.
        assert!(
            menus().iter().any(|menu| menu.name.as_ref() == "Window"),
            "no menu is named \"Window\""
        );
    }

    #[test]
    fn the_window_menu_can_minimize_and_close() {
        let names = menu_action_names();
        for expected in ["Minimize", "Close Window"] {
            assert!(
                names.iter().any(|name| name == expected),
                "{expected} is missing from the menu bar"
            );
        }
    }

    #[test]
    fn no_menu_carries_a_keystroke_in_its_label() {
        // macOS draws the key equivalent from the keymap. Writing "⌘R"
        // into the label would put it on screen twice and make the
        // keystroke a second thing to keep in sync (#99).
        for name in menu_action_names() {
            assert!(!name.contains('⌘'), "{name} spells out its own keystroke");
        }
    }
}
