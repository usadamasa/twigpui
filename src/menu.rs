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
        /// Submit the composer's draft (#58), bound to `cmd-enter`. Plain
        /// `enter` is deliberately *not* bound: it has to keep inserting a
        /// newline, and a post is not undoable.
        SubmitPost,
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
    glyphs: "⌘R",
    label: "Reload",
    in_header: true,
    menu_label: Some("Reload"),
};

/// Submit the draft. Plain `enter` is deliberately *not* bound: it has to
/// keep inserting a newline, and a post is not undoable.
const SUBMIT_POST: Shortcut = Shortcut {
    keystroke: "cmd-enter",
    glyphs: "⌘↩",
    label: "Post",
    in_header: true,
    menu_label: Some("Submit Post"),
};

/// Move focus into the composer.
const FOCUS_COMPOSER: Shortcut = Shortcut {
    keystroke: "cmd-n",
    glyphs: "⌘N",
    label: "Focus the composer",
    in_header: true,
    menu_label: Some("New Post"),
};

/// Leave the composer. Absent from the menu bar: "put focus back" is a
/// gesture, not a command anyone goes looking for in a menu.
const BLUR_COMPOSER: Shortcut = Shortcut {
    keystroke: "escape",
    glyphs: "esc",
    label: "Leave the composer",
    in_header: true,
    menu_label: None,
};

/// Quit (#99). The only binding registered with no key context, and the
/// only one the header does not advertise — see [`init`].
const QUIT: Shortcut = Shortcut {
    keystroke: "cmd-q",
    glyphs: "⌘Q",
    label: "Quit",
    in_header: false,
    menu_label: Some("Quit twigpui"),
};

/// Every binding, in the order [`init`] registers them (#99).
///
/// [`shortcuts`] filters it to what the header advertises, and the tests
/// walk all of it: a binding the header keeps quiet about still has to
/// hold a keystroke nothing else holds, and still has to agree with the
/// menu bar. A new shortcut belongs here.
///
/// [`init`] and [`menus`] cannot walk it — each entry there names a
/// concrete action type, and this array carries only the data those two
/// share — so adding a `Shortcut` without binding or listing it is
/// possible. What the tests below make impossible is the half-done
/// version: a binding that says it belongs in the menu bar and isn't
/// there, or vice versa.
const ALL_SHORTCUTS: [&Shortcut; 5] = [
    &RELOAD,
    &SUBMIT_POST,
    &FOCUS_COMPOSER,
    &BLUR_COMPOSER,
    &QUIT,
];

/// Register #58's key bindings. Called once at startup, next to
/// `gpui_component::init` (which registers its own).
///
/// **Every binding here uses a modifier.** The issue's central hazard is a
/// bare `j`/`k`/`n` firing while the user is typing a post; nothing bound
/// here can, because nothing here is a bare letter. When post selection
/// arrives and bare keys become worth having, they will need a second key
/// context that the composer's focus removes — this constant is where that
/// starts.
///
/// [`QUIT`] is the one binding registered with no key context (#99). The
/// others answer a question about the timeline and belong to the view that
/// answers it; quitting is not the window's business, and scoping it would
/// mean `cmd-q` doing nothing whenever focus sat anywhere else.
pub(crate) fn init(cx: &mut gpui::App) {
    cx.bind_keys([
        gpui::KeyBinding::new(RELOAD.keystroke, Reload, Some(KEY_CONTEXT)),
        gpui::KeyBinding::new(SUBMIT_POST.keystroke, SubmitPost, Some(KEY_CONTEXT)),
        gpui::KeyBinding::new(FOCUS_COMPOSER.keystroke, FocusComposer, Some(KEY_CONTEXT)),
        gpui::KeyBinding::new(BLUR_COMPOSER.keystroke, BlurComposer, Some(KEY_CONTEXT)),
        gpui::KeyBinding::new(QUIT.keystroke, Quit, None),
    ]);
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
            .chain(menu_item(&QUIT, Quit))
            .collect(),
        },
        gpui::Menu {
            name: "File".into(),
            items: [
                menu_item(&FOCUS_COMPOSER, FocusComposer),
                menu_item(&SUBMIT_POST, SubmitPost),
            ]
            .into_iter()
            .flatten()
            .collect(),
        },
        gpui::Menu {
            name: "View".into(),
            items: menu_item(&RELOAD, Reload).into_iter().collect(),
        },
    ]
}

/// The menu item for one shortcut, or nothing when [`Shortcut::menu_label`]
/// says it is deliberately absent from the menu bar (#99).
fn menu_item(shortcut: &Shortcut, action: impl gpui::Action) -> Option<gpui::MenuItem> {
    shortcut
        .menu_label
        .map(|label| gpui::MenuItem::action(label, action))
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
    fn no_menu_carries_a_keystroke_in_its_label() {
        // macOS draws the key equivalent from the keymap. Writing "⌘R"
        // into the label would put it on screen twice and make the
        // keystroke a second thing to keep in sync (#99).
        for name in menu_action_names() {
            assert!(!name.contains('⌘'), "{name} spells out its own keystroke");
        }
    }
}
