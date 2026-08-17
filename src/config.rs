use anyhow::{Context as _, Result, bail};
use serde::Deserialize;
use std::path::Path;

use crate::paths::Paths;

/// Runtime configuration, resolved with environment variable > `config.toml`
/// > built-in default precedence.
#[derive(Debug, Clone)]
pub(crate) struct Config {
    /// App-only Bearer token, used verbatim in the `Authorization` header.
    pub bearer_token: String,
    /// Screen name whose posts are shown, without a leading `@`.
    pub target_username: String,
    /// Posts requested per fetch. The X API accepts 5..=100.
    pub max_results: u32,
}

const DEFAULT_USERNAME: &str = "XDevelopers";
const DEFAULT_MAX_RESULTS: u32 = 20;
const MAX_RESULTS_RANGE: std::ops::RangeInclusive<u32> = 5..=100;

/// The file-level settings loaded from `config.toml`.
///
/// Every field is `Option` and `#[serde(default)]` applies, and the struct
/// deliberately does not use `deny_unknown_fields`: future issues (#19's
/// theme, #24's layout) add keys incrementally, and an older binary reading
/// a newer file must not choke on keys it doesn't know about yet.
#[derive(Debug, Default, Deserialize)]
struct FileSettings {
    #[serde(default)]
    target_username: Option<String>,
    #[serde(default)]
    max_results: Option<u32>,
    /// Present only so [`Config::resolve`] can detect and reject a bearer
    /// token accidentally checked into `config.toml`. Kept as an untyped
    /// `toml::Value` so any shape (string, table, array, ...) under this key
    /// still triggers the check instead of failing with a deserialize error.
    #[serde(default)]
    bearer_token: Option<toml::Value>,
}

impl FileSettings {
    /// Load settings from `path`. A missing file is not an error — it just
    /// means there are no file-level settings yet. A malformed file is an
    /// error whose message names `path`.
    fn load(path: &Path) -> Result<Self> {
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(error).with_context(|| format!("could not read {}", path.display()));
            }
        };
        toml::from_str(&contents).with_context(|| format!("could not parse {}", path.display()))
    }
}

impl Config {
    pub(crate) fn from_env() -> Result<Self> {
        // A missing .env is fine — the variables may come from the real environment.
        let _ = dotenvy::dotenv();
        let paths = Paths::from_env()?;
        paths.ensure_dirs()?;
        let file = FileSettings::load(&paths.settings_file())?;
        Self::resolve(|key| std::env::var(key).ok(), file)
    }

    /// Parse and validate the settings from an arbitrary variable lookup and
    /// already-loaded file settings.
    ///
    /// Split out from [`Config::from_env`] so the rules below can be tested
    /// without `set_var`, which is `unsafe` and races the other test threads.
    fn resolve(var: impl Fn(&str) -> Option<String>, file: FileSettings) -> Result<Self> {
        // config.toml is a hand-edited file that people put in dotfiles repos, so a
        // bearer token must never be readable from it. Reject the key outright
        // rather than silently accepting it — credentials get their own store in #7.
        if file.bearer_token.is_some() {
            bail!(
                "bearer_token must not be set in config.toml. Use the X_BEARER_TOKEN \
                 environment variable (or .env) instead."
            );
        }

        let bearer_token = var("X_BEARER_TOKEN")
            .filter(|t| !t.trim().is_empty())
            .context("X_BEARER_TOKEN is unset. Copy .env.example to .env and fill it in.")?;

        let target_username = var("X_TARGET_USERNAME")
            .filter(|u| !u.trim().is_empty())
            .or_else(|| file.target_username.filter(|u| !u.trim().is_empty()))
            .unwrap_or_else(|| DEFAULT_USERNAME.to_string());
        let target_username = target_username.trim().trim_start_matches('@').to_string();

        let (max_results, max_results_source) = match var("X_MAX_RESULTS") {
            Some(raw) => {
                let value = raw
                    .trim()
                    .parse::<u32>()
                    .with_context(|| format!("X_MAX_RESULTS is not a number: {raw:?}"))?;
                (value, "X_MAX_RESULTS")
            }
            None => match file.max_results {
                Some(value) => (value, "max_results in config.toml"),
                None => (DEFAULT_MAX_RESULTS, "the default"),
            },
        };
        if !MAX_RESULTS_RANGE.contains(&max_results) {
            bail!(
                "{max_results_source} must be between {} and {}, got {max_results}",
                MAX_RESULTS_RANGE.start(),
                MAX_RESULTS_RANGE.end()
            );
        }

        Ok(Self {
            bearer_token: bearer_token.trim().to_string(),
            target_username,
            max_results,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, DEFAULT_MAX_RESULTS, DEFAULT_USERNAME, FileSettings};

    /// Build a lookup over a fixed `(key, value)` table.
    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    #[test]
    fn fills_in_the_defaults_when_only_the_token_is_set() {
        let config = Config::resolve(
            vars(&[("X_BEARER_TOKEN", "token")]),
            FileSettings::default(),
        )
        .unwrap();

        assert_eq!(config.bearer_token, "token");
        assert_eq!(config.target_username, DEFAULT_USERNAME);
        assert_eq!(config.max_results, DEFAULT_MAX_RESULTS);
    }

    #[test]
    fn rejects_a_missing_token() {
        let error = Config::resolve(vars(&[]), FileSettings::default())
            .unwrap_err()
            .to_string();
        assert!(error.contains("X_BEARER_TOKEN is unset"), "{error}");
    }

    #[test]
    fn treats_a_blank_token_as_missing() {
        let error = Config::resolve(vars(&[("X_BEARER_TOKEN", "   ")]), FileSettings::default())
            .unwrap_err()
            .to_string();
        assert!(error.contains("X_BEARER_TOKEN is unset"), "{error}");
    }

    #[test]
    fn trims_the_token() {
        // A token pasted into .env often carries a trailing newline, and it goes
        // into the Authorization header verbatim.
        let config = Config::resolve(
            vars(&[("X_BEARER_TOKEN", "  token\n")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.bearer_token, "token");
    }

    #[test]
    fn strips_a_leading_at_from_the_username() {
        let config = Config::resolve(
            vars(&[
                ("X_BEARER_TOKEN", "token"),
                ("X_TARGET_USERNAME", " @XDevelopers "),
            ]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.target_username, "XDevelopers");
    }

    #[test]
    fn falls_back_to_the_default_username_when_blank() {
        let config = Config::resolve(
            vars(&[("X_BEARER_TOKEN", "token"), ("X_TARGET_USERNAME", "  ")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.target_username, DEFAULT_USERNAME);
    }

    #[test]
    fn parses_max_results() {
        let config = Config::resolve(
            vars(&[("X_BEARER_TOKEN", "token"), ("X_MAX_RESULTS", " 42 ")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.max_results, 42);
    }

    #[test]
    fn rejects_a_non_numeric_max_results() {
        let error = Config::resolve(
            vars(&[("X_BEARER_TOKEN", "token"), ("X_MAX_RESULTS", "lots")]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("not a number"), "{error}");
    }

    #[test]
    fn accepts_both_ends_of_the_api_range() {
        for raw in ["5", "100"] {
            let config = Config::resolve(
                vars(&[("X_BEARER_TOKEN", "token"), ("X_MAX_RESULTS", raw)]),
                FileSettings::default(),
            )
            .unwrap();
            assert_eq!(config.max_results.to_string(), raw);
        }
    }

    #[test]
    fn rejects_max_results_outside_the_api_range() {
        for raw in ["4", "101"] {
            let error = Config::resolve(
                vars(&[("X_BEARER_TOKEN", "token"), ("X_MAX_RESULTS", raw)]),
                FileSettings::default(),
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains("between 5 and 100"), "{raw}: {error}");
        }
    }

    // --- config.toml layering (env > file > default) ---

    #[test]
    fn resolve_reads_target_username_from_the_file_when_env_is_unset() {
        let file = FileSettings {
            target_username: Some("FileUser".to_string()),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_BEARER_TOKEN", "token")]), file).unwrap();
        assert_eq!(config.target_username, "FileUser");
    }

    #[test]
    fn resolve_prefers_the_env_target_username_over_the_file() {
        let file = FileSettings {
            target_username: Some("FileUser".to_string()),
            ..FileSettings::default()
        };
        let config = Config::resolve(
            vars(&[
                ("X_BEARER_TOKEN", "token"),
                ("X_TARGET_USERNAME", "EnvUser"),
            ]),
            file,
        )
        .unwrap();
        assert_eq!(config.target_username, "EnvUser");
    }

    #[test]
    fn resolve_reads_max_results_from_the_file_when_env_is_unset() {
        let file = FileSettings {
            max_results: Some(42),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_BEARER_TOKEN", "token")]), file).unwrap();
        assert_eq!(config.max_results, 42);
    }

    #[test]
    fn resolve_prefers_the_env_max_results_over_the_file() {
        let file = FileSettings {
            max_results: Some(42),
            ..FileSettings::default()
        };
        let config = Config::resolve(
            vars(&[("X_BEARER_TOKEN", "token"), ("X_MAX_RESULTS", "7")]),
            file,
        )
        .unwrap();
        assert_eq!(config.max_results, 7);
    }

    #[test]
    fn resolve_rejects_a_file_max_results_outside_the_api_range() {
        let file = FileSettings {
            max_results: Some(4),
            ..FileSettings::default()
        };
        let error = Config::resolve(vars(&[("X_BEARER_TOKEN", "token")]), file)
            .unwrap_err()
            .to_string();
        assert!(error.contains("between 5 and 100"), "{error}");
        assert!(error.contains("config.toml"), "{error}");
    }

    #[test]
    fn resolve_rejects_a_bearer_token_in_the_file() {
        let file = FileSettings {
            bearer_token: Some(toml::Value::String("leaked".to_string())),
            ..FileSettings::default()
        };
        let error = Config::resolve(vars(&[("X_BEARER_TOKEN", "token")]), file)
            .unwrap_err()
            .to_string();
        assert!(error.contains("X_BEARER_TOKEN"), "{error}");
        assert!(!error.contains("leaked"), "{error}");
    }

    #[test]
    fn file_settings_load_returns_defaults_when_the_file_is_missing() {
        let path = std::env::temp_dir().join(format!(
            "twigpui-test-missing-config-{}.toml",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        let settings = FileSettings::load(&path).unwrap();
        assert!(settings.target_username.is_none());
        assert!(settings.max_results.is_none());
    }

    #[test]
    fn file_settings_load_errors_naming_the_path_on_malformed_toml() {
        let path = std::env::temp_dir().join(format!(
            "twigpui-test-malformed-config-{}.toml",
            std::process::id()
        ));
        std::fs::write(&path, "??? not valid toml ???").unwrap();

        let error = FileSettings::load(&path).unwrap_err().to_string();
        assert!(error.contains(&path.display().to_string()), "{error}");

        std::fs::remove_file(&path).unwrap();
    }
}
