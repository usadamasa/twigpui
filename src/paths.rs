//! XDG Base Directory paths for twigpui's persisted files.
//!
//! [`Paths`] resolves the three base directories once, at startup. The
//! convention going forward is that every file twigpui persists gets its own
//! accessor here (like [`Paths::settings_file`]) — callers never join paths
//! themselves. Accessors are added incrementally as the files that need them
//! land: the OAuth token store (#7), the response cache (#9), and the panel
//! layout (#24) each add their own accessor with their own issue rather than
//! being anticipated here.

use anyhow::{Context as _, Result};
use std::path::PathBuf;

/// The three XDG Base Directory locations twigpui writes under, each with a
/// `twigpui` component appended.
#[derive(Debug)]
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
    fn from_vars(_var: impl Fn(&str) -> Option<String>) -> Result<Self> {
        // TODO: stub — ignores XDG_* and HOME entirely, real resolution
        // lands in the implementation commit.
        Ok(Self {
            config_dir: PathBuf::from("/stub/config"),
            cache_dir: PathBuf::from("/stub/cache"),
            state_dir: PathBuf::from("/stub/state"),
        })
    }

    /// Path to the `config.toml` settings file, under `config_dir`.
    pub(crate) fn settings_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    /// Create all three directories (recursively) if they do not already
    /// exist.
    pub(crate) fn ensure_dirs(&self) -> Result<()> {
        // TODO: stub — does nothing yet. Real implementation creates the
        // directories with 0700 permissions.
        let _dirs = [&self.config_dir, &self.cache_dir, &self.state_dir];
        Ok(())
    }
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
        let paths = Paths::from_vars(vars(&[("HOME", "/home/alice"), ("XDG_CACHE_HOME", "   ")]))
            .unwrap();
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
    fn ensure_dirs_creates_all_three_directories_with_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = std::env::temp_dir().join(format!(
            "twigpui-test-ensure-dirs-{}",
            std::process::id()
        ));
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
