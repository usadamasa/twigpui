//! Which installation of twigpui this binary *is* — the development one or
//! the real one (#169).
//!
//! The two are kept apart everywhere they could otherwise collide: the XDG
//! directory component (so the OAuth session, the response cache and the
//! usage ledger are separate files), the OAuth loopback port (so each has
//! its own redirect URI, and so both can wait for a redirect at once), and
//! the window title (so a screenshot, or the Dock, says which one you are
//! looking at).
//!
//! The choice is made at compile time by `debug_assertions`, not by a flag
//! or an environment variable, because the failure mode being designed
//! against is *forgetting*. A flag left off a `cargo run` would sign the
//! development build into the real account and write its cache over the
//! real one; there is no equivalent slip here, since a debug binary
//! physically cannot address the release profile's files. The cost of that
//! choice is that `cargo run --release` from the repository uses the real
//! profile — see `scripts/build-app-bundle.sh --dev`, which builds a debug
//! `.app` when what you want is a development build that behaves like an
//! installed one.

/// The List a development build reads and syncs (#169). A throwaway list
/// on the same account, so working on #161's timeline or #163's sync never
/// touches the List being read for real.
const DEV_LIST_ID: &str = "2091351590695588200";

/// The accounts a development `--sync-list` mirrors, instead of everyone
/// the signed-in user follows (#169).
///
/// Reading the whole follow graph is billed per account returned, so a
/// dry run against a few thousand follows costs dollars — far too much to
/// spend while working on the sync itself. These four stand in for it:
/// X's own accounts, which are stable enough that a hardcoded screen name
/// will not quietly start resolving to somebody else, and few enough that
/// the read side of a development sync is four cached lookups a month
/// rather than a paginated crawl.
const DEV_SYNC_SEED: &[&str] = &["X", "XDevelopers", "Support", "Safety"];

/// Which installation this binary is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Profile {
    /// The real installation — a release build, normally the `.app` bundle
    /// assembled by `scripts/build-app-bundle.sh`.
    Release,
    /// The development installation — any debug build, including a plain
    /// `cargo run`.
    Dev,
}

impl Profile {
    /// The profile this binary was compiled as.
    pub(crate) fn current() -> Self {
        if cfg!(debug_assertions) {
            Self::Dev
        } else {
            Self::Release
        }
    }

    /// The directory name appended to each XDG base directory — the one
    /// thing that keeps `config.toml`, the token store, the response cache
    /// and the usage ledger from being shared between the two profiles.
    pub(crate) fn dir_component(self) -> &'static str {
        match self {
            Self::Release => "twigpui",
            Self::Dev => "twigpui-dev",
        }
    }

    /// The loopback port the OAuth redirect comes back on. X requires an
    /// exact redirect-URI match, so this can't be ephemeral: each profile's
    /// port is registered verbatim against its own X app in the Developer
    /// Portal. Distinct ports also mean a development sign-in and a real
    /// one can be in flight at the same time without one listener stealing
    /// the other's redirect.
    pub(crate) fn loopback_port(self) -> u16 {
        match self {
            Self::Release => 8733,
            Self::Dev => 8734,
        }
    }

    /// The window's title bar text. Different per profile so a screenshot
    /// tool can single one window out by title, and so two running copies
    /// are told apart at a glance.
    pub(crate) fn window_title(self) -> &'static str {
        match self {
            Self::Release => "twigpui",
            Self::Dev => "twigpui (dev)",
        }
    }

    /// The List this profile falls back to when neither `X_LIST_ID` nor
    /// `list_id` in `config.toml` names one (#161, #169).
    ///
    /// A default only for the development build. The release build has no
    /// business guessing which List someone meant, and falling back to a
    /// hardcoded one would read somebody else's list on an unconfigured
    /// install; a development build defaulting to the throwaway list is
    /// what keeps `--sync-list` from being one forgotten export away from
    /// rewriting the real one.
    pub(crate) fn default_list_id(self) -> Option<&'static str> {
        match self {
            Self::Release => None,
            Self::Dev => Some(DEV_LIST_ID),
        }
    }

    /// The accounts `--sync-list` mirrors into the List, or `None` to
    /// mirror everyone the signed-in user follows (#163, #169).
    ///
    /// `Some` only for the development build — see [`DEV_SYNC_SEED`] for
    /// why the real follow graph is the wrong thing to read while working
    /// on the sync.
    pub(crate) fn sync_seed_usernames(self) -> Option<&'static [&'static str]> {
        match self {
            Self::Release => None,
            Self::Dev => Some(DEV_SYNC_SEED),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DEV_LIST_ID, DEV_SYNC_SEED, Profile};

    #[test]
    fn the_two_profiles_never_share_a_directory() {
        // The whole point of #169: a development run must not be able to
        // read or overwrite the real installation's tokens, cache or state.
        assert_ne!(
            Profile::Dev.dir_component(),
            Profile::Release.dir_component()
        );
    }

    #[test]
    fn the_two_profiles_never_share_a_loopback_port() {
        // Sharing one would make the redirect URIs identical, so the two X
        // app registrations could not be told apart — and a sign-in started
        // in one profile could be answered by the other's listener.
        assert_ne!(
            Profile::Dev.loopback_port(),
            Profile::Release.loopback_port()
        );
    }

    #[test]
    fn the_two_profiles_never_share_a_window_title() {
        assert_ne!(Profile::Dev.window_title(), Profile::Release.window_title());
    }

    #[test]
    fn the_release_profile_keeps_the_names_that_predate_this_split() {
        // Changing either would orphan an existing installation's files and
        // invalidate the redirect URI already registered in the Developer
        // Portal, so these are load-bearing literals, not defaults.
        assert_eq!(Profile::Release.dir_component(), "twigpui");
        assert_eq!(Profile::Release.loopback_port(), 8733);
        assert_eq!(Profile::Release.window_title(), "twigpui");
    }

    #[test]
    fn the_dev_profile_matches_what_the_developer_portal_is_registered_with() {
        assert_eq!(Profile::Dev.dir_component(), "twigpui-dev");
        assert_eq!(Profile::Dev.loopback_port(), 8734);
    }

    #[test]
    fn only_the_dev_profile_defaults_to_a_list() {
        // A release build with nothing configured must read the home
        // timeline, not somebody else's list.
        assert_eq!(Profile::Release.default_list_id(), None);
        assert_eq!(Profile::Dev.default_list_id(), Some(DEV_LIST_ID));
    }

    #[test]
    fn only_the_dev_profile_syncs_from_a_fixed_seed() {
        // `None` is what makes a release sync read the real follow graph;
        // flipping these would either bill a development dry run for
        // thousands of accounts or mirror four X accounts over the real
        // list.
        assert_eq!(Profile::Release.sync_seed_usernames(), None);
        assert_eq!(Profile::Dev.sync_seed_usernames(), Some(DEV_SYNC_SEED));
    }

    #[test]
    fn the_dev_seed_is_small_enough_to_read_without_paging() {
        // One page is 100 accounts. A seed that outgrew it would silently
        // reintroduce the paginated read this exists to avoid.
        assert!(
            !DEV_SYNC_SEED.is_empty() && DEV_SYNC_SEED.len() <= 100,
            "{DEV_SYNC_SEED:?}"
        );
    }

    #[test]
    fn the_dev_seed_holds_bare_screen_names() {
        // Resolved through `user_id_by_username`, which takes the name
        // without the `@` and without a URL around it.
        for username in DEV_SYNC_SEED {
            assert!(
                !username.is_empty()
                    && username
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_'),
                "{username:?}"
            );
        }
    }

    /// Pins the mapping itself, not just that the two profiles differ: were
    /// `current` inverted, an ordinary `cargo run` would sign into the real
    /// account and write over the real installation's cache. Compiled only
    /// for the build it describes, so `cargo test --release` doesn't report
    /// a failure for behaving exactly as intended.
    #[test]
    #[cfg(debug_assertions)]
    fn a_debug_build_is_the_dev_profile() {
        assert_eq!(Profile::current(), Profile::Dev);
    }

    /// The other half of [`a_debug_build_is_the_dev_profile`] — this is the
    /// build `scripts/build-app-bundle.sh` ships.
    #[test]
    #[cfg(not(debug_assertions))]
    fn a_release_build_is_the_release_profile() {
        assert_eq!(Profile::current(), Profile::Release);
    }
}
