//! XDG Base Directory paths for twigpui's persisted files.
//!
//! [`Paths`] resolves the three base directories once, at startup. The
//! convention going forward is that every file twigpui persists gets its own
//! accessor here (like [`Paths::settings_file`]) — callers never join paths
//! themselves. Accessors are added incrementally as the files that need them
//! land: the OAuth token store ([`Paths::oauth_token_file`], #7), the
//! response cache (#9), and the panel layout (#24) each add their own
//! accessor with their own issue rather than being anticipated here.

use anyhow::{Context as _, Result};
use std::path::{Path, PathBuf};

/// The three XDG Base Directory locations twigpui writes under, each with a
/// `twigpui` component appended.
// The shared `_dir` postfix is the point, not redundancy — it names what each
// field is (a directory) alongside which XDG category it resolves.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone)]
pub(crate) struct Paths {
    config_dir: PathBuf,
    cache_dir: PathBuf,
    state_dir: PathBuf,
}

impl Paths {
    pub(crate) fn from_env() -> Result<Self> {
        Self::from_vars(|key| std::env::var(key).ok())
    }

    /// Resolve the three directories from an arbitrary variable lookup.
    ///
    /// Split out from [`Paths::from_env`] so the resolution rules can be
    /// tested without `set_var`, which is `unsafe` and races the other test
    /// threads. Mirrors the split used by [`crate::config::Config`].
    ///
    /// `pub(crate)` (rather than private) because `oauth::tokens`'s own
    /// tests need a `Paths` pointed at a scratch directory too, and this is
    /// the same seam `paths.rs`'s own tests already use.
    pub(crate) fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let config_dir = resolve_dir(&var, "XDG_CONFIG_HOME", ".config")?;
        let cache_dir = resolve_dir(&var, "XDG_CACHE_HOME", ".cache")?;
        let state_dir = resolve_dir(&var, "XDG_STATE_HOME", ".local/state")?;
        Ok(Self {
            config_dir,
            cache_dir,
            state_dir,
        })
    }

    /// Path to the `config.toml` settings file, under `config_dir`.
    pub(crate) fn settings_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    /// Path to the OAuth token store, under `state_dir`. Written `0600` by
    /// [`crate::oauth::tokens::save`] — state, not config, because it holds
    /// a credential rather than a hand-edited setting.
    pub(crate) fn oauth_token_file(&self) -> PathBuf {
        self.state_dir.join("oauth_tokens.json")
    }

    /// Path to the screen-name → user-id cache, under `cache_dir` (#9). User
    /// ids are effectively permanent, so caching this alone (TTL'd by
    /// [`crate::cache`]) turns a reload's two requests into one.
    pub(crate) fn user_ids_file(&self) -> PathBuf {
        self.cache_dir.join("user_ids.json")
    }

    /// Path to one user's cached timeline, under `cache_dir` (#9). Split per
    /// user, rather than one shared file, so #24's additional panels can
    /// each grow their own cache file without contention.
    pub(crate) fn timeline_file(&self, user_id: &str) -> PathBuf {
        self.cache_dir.join(format!("timeline-{user_id}.json"))
    }

    /// Path to one user's cached *home* timeline, under `cache_dir` (#11).
    /// Deliberately a different filename from [`Self::timeline_file`] for the
    /// same `user_id`: the home timeline and a single-user timeline are
    /// different content, and a user who has run both modes (e.g. by signing
    /// out and back in with a bearer token) must not have one silently
    /// overwrite the other.
    pub(crate) fn home_timeline_file(&self, user_id: &str) -> PathBuf {
        self.cache_dir.join(format!("home-timeline-{user_id}.json"))
    }

    /// Path to the cached result of `GET /2/users/me` (#11): the signed-in
    /// user's own id and screen name, under `cache_dir`. Immutable for a
    /// given account, so caching it (like #9 caches screen-name → id) avoids
    /// re-spending a request on every start.
    pub(crate) fn me_file(&self) -> PathBuf {
        self.cache_dir.join("me.json")
    }

    /// Path to a cached parent chain for one reply post, under `cache_dir`
    /// (#12). Keyed by the *reply's own* id — the post "Show thread" was
    /// clicked from — so re-opening the same reply renders the already-
    /// walked chain instead of re-spending up to
    /// [`crate::thread::MAX_THREAD_DEPTH`] requests.
    pub(crate) fn thread_file(&self, reply_post_id: &str) -> PathBuf {
        self.cache_dir.join(format!("thread-{reply_post_id}.json"))
    }

    /// Path to the tracked rate-limit state, under `state_dir` (#10). State,
    /// not cache: a process restart does not reset X's rate-limit window, so
    /// losing this file risks firing a request straight into an
    /// already-exhausted window and wasting a paid request, rather than just
    /// costing a slower cold start the way a lost cache entry would.
    pub(crate) fn rate_limit_file(&self) -> PathBuf {
        self.state_dir.join("rate_limit.json")
    }

    /// Path to the tracked per-endpoint request-count usage, under
    /// `state_dir` (#18). State, not cache: unlike the response cache,
    /// losing this file doesn't just cost a slower cold start — it loses the
    /// cumulative spend history itself, which is the whole point of tracking
    /// it in the first place.
    pub(crate) fn usage_file(&self) -> PathBuf {
        self.state_dir.join("usage.json")
    }

    /// Path to the local record of post ids the signed-in user has reposted
    /// from this app, under `state_dir` (#15). State, not cache: the X API
    /// v2 timeline response carries no field for "did I repost this" (no
    /// v1.1-style `retweeted`), so this file is the *only* source of truth
    /// twigpui has for the repost button's initial state — unlike a lost
    /// cache entry, which just costs a slower cold start, losing this file
    /// means every post reposted before the loss shows as "not reposted"
    /// again, risking a duplicate repost on the next click (recoverable via
    /// #15's own error-reconciliation, but not silently harmless).
    pub(crate) fn reposted_posts_file(&self) -> PathBuf {
        self.state_dir.join("reposted_posts.json")
    }

    /// Create all three directories (recursively) if they do not already
    /// exist.
    ///
    /// Returns whether `cache_dir` was created by this call, so the caller
    /// can run the one-time setup in [`Paths::exclude_cache_from_backups`].
    /// That side effect stays out of here deliberately: `ensure_dirs` is
    /// called by most of this crate's filesystem tests, and shelling out to
    /// `tmutil` on each of them costs a second apiece.
    pub(crate) fn ensure_dirs(&self) -> Result<bool> {
        // Sampled before creation, since afterwards the directory exists
        // either way.
        let cache_dir_is_new = !self.cache_dir.exists();

        for dir in [&self.config_dir, &self.cache_dir, &self.state_dir] {
            create_private_dir(dir)?;
        }

        Ok(cache_dir_is_new)
    }

    /// Best-effort exclude `cache_dir` from Time Machine via
    /// `tmutil addexclusion` (#9). `~/Library/Caches` is exempted from
    /// backups automatically by macOS; the XDG cache location this app
    /// actually uses (`~/.cache`) is not, so without this the response cache
    /// would get backed up on every Time Machine run like ordinary data.
    ///
    /// Failure — `tmutil` missing, no permission, anything — is silently
    /// ignored: this is a nice-to-have and must never block startup. The call
    /// takes about a second, so run it only when `ensure_dirs` reports that
    /// it just created the directory.
    pub(crate) fn exclude_cache_from_backups(&self) {
        let _ = std::process::Command::new("tmutil")
            .arg("addexclusion")
            .arg(&self.cache_dir)
            .output();
    }
}

/// Resolve one XDG base directory: `$<xdg_var>/twigpui` if `xdg_var` holds a
/// non-blank absolute path, else `$HOME/<default_relative>/twigpui`.
///
/// Per the XDG Base Directory spec, a relative path in an `XDG_*` variable
/// must be treated as if it were unset, and this also treats a blank value
/// (empty or whitespace-only) as unset.
fn resolve_dir(
    var: &impl Fn(&str) -> Option<String>,
    xdg_var: &str,
    default_relative: &str,
) -> Result<PathBuf> {
    let base = var(xdg_var)
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty() && Path::new(value).is_absolute())
        .map(PathBuf::from);

    let base = if let Some(base) = base {
        base
    } else {
        let home = var("HOME").context("HOME is unset")?;
        PathBuf::from(home).join(default_relative)
    };
    Ok(base.join("twigpui"))
}

/// Create `dir`, and any missing parents, with `0o700` (owner-only)
/// permissions. Creating a directory that already exists is not an error.
///
/// #7 will write an OAuth token file under `state_dir`. Creating every
/// directory `0700` from the start avoids having to retrofit permissions
/// onto a tree that may already contain files by the time that lands.
fn create_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;

    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("could not create directory: {}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::Paths;
    use std::path::PathBuf;

    /// Build a lookup over a fixed `(key, value)` table, mirroring
    /// `config::tests::vars`.
    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    #[test]
    fn falls_back_to_the_xdg_defaults_under_home() {
        let paths = Paths::from_vars(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.config_dir,
            PathBuf::from("/home/alice/.config/twigpui")
        );
        assert_eq!(paths.cache_dir, PathBuf::from("/home/alice/.cache/twigpui"));
        assert_eq!(
            paths.state_dir,
            PathBuf::from("/home/alice/.local/state/twigpui")
        );
    }

    #[test]
    fn honors_xdg_overrides_when_absolute() {
        let paths = Paths::from_vars(vars(&[
            ("HOME", "/home/alice"),
            ("XDG_CONFIG_HOME", "/etc/xdg-config"),
            ("XDG_CACHE_HOME", "/var/xdg-cache"),
            ("XDG_STATE_HOME", "/var/xdg-state"),
        ]))
        .unwrap();
        assert_eq!(paths.config_dir, PathBuf::from("/etc/xdg-config/twigpui"));
        assert_eq!(paths.cache_dir, PathBuf::from("/var/xdg-cache/twigpui"));
        assert_eq!(paths.state_dir, PathBuf::from("/var/xdg-state/twigpui"));
    }

    #[test]
    fn ignores_a_relative_xdg_override_and_falls_back_to_the_default() {
        let paths = Paths::from_vars(vars(&[
            ("HOME", "/home/alice"),
            ("XDG_CONFIG_HOME", "relative/path"),
        ]))
        .unwrap();
        assert_eq!(
            paths.config_dir,
            PathBuf::from("/home/alice/.config/twigpui")
        );
    }

    #[test]
    fn ignores_a_blank_xdg_override_and_falls_back_to_the_default() {
        let paths =
            Paths::from_vars(vars(&[("HOME", "/home/alice"), ("XDG_CACHE_HOME", "   ")])).unwrap();
        assert_eq!(paths.cache_dir, PathBuf::from("/home/alice/.cache/twigpui"));
    }

    #[test]
    fn does_not_need_home_when_all_three_overrides_are_absolute() {
        let paths = Paths::from_vars(vars(&[
            ("XDG_CONFIG_HOME", "/etc/xdg-config"),
            ("XDG_CACHE_HOME", "/var/xdg-cache"),
            ("XDG_STATE_HOME", "/var/xdg-state"),
        ]))
        .unwrap();
        assert_eq!(paths.config_dir, PathBuf::from("/etc/xdg-config/twigpui"));
    }

    #[test]
    fn errors_naming_home_when_a_default_is_needed_and_home_is_unset() {
        let error = Paths::from_vars(vars(&[])).unwrap_err().to_string();
        assert!(error.contains("HOME"), "{error}");
    }

    #[test]
    fn settings_file_is_config_dot_toml_under_the_config_dir() {
        let paths = Paths::from_vars(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.settings_file(),
            PathBuf::from("/home/alice/.config/twigpui/config.toml")
        );
    }

    #[test]
    fn oauth_token_file_is_under_the_state_dir() {
        let paths = Paths::from_vars(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.oauth_token_file(),
            PathBuf::from("/home/alice/.local/state/twigpui/oauth_tokens.json")
        );
    }

    #[test]
    fn user_ids_file_is_under_the_cache_dir() {
        let paths = Paths::from_vars(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.user_ids_file(),
            PathBuf::from("/home/alice/.cache/twigpui/user_ids.json")
        );
    }

    #[test]
    fn timeline_file_is_under_the_cache_dir_named_by_user_id() {
        let paths = Paths::from_vars(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.timeline_file("2244994945"),
            PathBuf::from("/home/alice/.cache/twigpui/timeline-2244994945.json")
        );
    }

    #[test]
    fn home_timeline_file_is_under_the_cache_dir_named_by_user_id() {
        let paths = Paths::from_vars(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.home_timeline_file("2244994945"),
            PathBuf::from("/home/alice/.cache/twigpui/home-timeline-2244994945.json")
        );
    }

    #[test]
    fn home_timeline_file_does_not_collide_with_the_single_user_timeline_file() {
        // #11: same user id, different content — overwriting one with the
        // other would silently corrupt whichever mode wasn't showing.
        let paths = Paths::from_vars(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_ne!(
            paths.timeline_file("2244994945"),
            paths.home_timeline_file("2244994945")
        );
    }

    #[test]
    fn me_file_is_under_the_cache_dir() {
        let paths = Paths::from_vars(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.me_file(),
            PathBuf::from("/home/alice/.cache/twigpui/me.json")
        );
    }

    #[test]
    fn thread_file_is_under_the_cache_dir_named_by_reply_post_id() {
        let paths = Paths::from_vars(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.thread_file("1800000000000000003"),
            PathBuf::from("/home/alice/.cache/twigpui/thread-1800000000000000003.json")
        );
    }

    #[test]
    fn rate_limit_file_is_under_the_state_dir() {
        let paths = Paths::from_vars(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.rate_limit_file(),
            PathBuf::from("/home/alice/.local/state/twigpui/rate_limit.json")
        );
    }

    #[test]
    fn usage_file_is_under_the_state_dir() {
        let paths = Paths::from_vars(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.usage_file(),
            PathBuf::from("/home/alice/.local/state/twigpui/usage.json")
        );
    }

    #[test]
    fn reposted_posts_file_is_under_the_state_dir() {
        let paths = Paths::from_vars(vars(&[("HOME", "/home/alice")])).unwrap();
        assert_eq!(
            paths.reposted_posts_file(),
            PathBuf::from("/home/alice/.local/state/twigpui/reposted_posts.json")
        );
    }

    #[test]
    fn ensure_dirs_creates_all_three_directories_with_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let root =
            std::env::temp_dir().join(format!("twigpui-test-ensure-dirs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        let paths = Paths::from_vars(vars(&[("HOME", &root.display().to_string())])).unwrap();
        paths.ensure_dirs().unwrap();

        for dir in [
            root.join(".config/twigpui"),
            root.join(".cache/twigpui"),
            root.join(".local/state/twigpui"),
        ] {
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{}", dir.display());
        }

        // Calling it again on an already-populated tree must not error.
        paths.ensure_dirs().unwrap();

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn ensure_dirs_reports_the_cache_dir_as_new_only_on_the_call_that_creates_it() {
        // The flag gates a ~1s `tmutil` subprocess, so reporting "new" on
        // every startup would be a visible cost, not just a cosmetic slip.
        let root = std::env::temp_dir().join(format!(
            "twigpui-test-ensure-dirs-new-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);

        let paths = Paths::from_vars(vars(&[("HOME", &root.display().to_string())])).unwrap();
        assert!(paths.ensure_dirs().unwrap(), "first call creates cache_dir");
        assert!(
            !paths.ensure_dirs().unwrap(),
            "second call finds it already there"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }
}
