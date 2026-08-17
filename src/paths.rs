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

    /// Create all three directories (recursively) if they do not already
    /// exist.
    pub(crate) fn ensure_dirs(&self) -> Result<()> {
        for dir in [&self.config_dir, &self.cache_dir, &self.state_dir] {
            create_private_dir(dir)?;
        }
        Ok(())
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
}
