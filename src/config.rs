use anyhow::{Context as _, Result, bail};
use serde::Deserialize;
use std::path::Path;

use crate::paths::Paths;
use crate::theme::ThemeMode;

/// Runtime configuration, resolved with environment variable > `config.toml`
/// > built-in default precedence.
#[derive(Debug, Clone)]
pub(crate) struct Config {
    /// App-only Bearer token, used verbatim in the `Authorization` header.
    ///
    /// `Option` since #7: an OAuth session is an equally valid credential,
    /// so this is no longer the only way to authenticate. [`Config::resolve`]
    /// only fails when *neither* this nor [`Config::oauth_client_id`] is set.
    pub bearer_token: Option<String>,
    /// OAuth 2.0 client id for the PKCE sign-in flow (#7). Non-secret — a
    /// public OAuth client has no client secret — so unlike `bearer_token`
    /// this may live in `config.toml`.
    pub oauth_client_id: Option<String>,
    /// Screen name whose posts are shown, without a leading `@`.
    pub target_username: String,
    /// Posts requested per fetch. The X API accepts 5..=100.
    pub max_results: u32,
    /// Floor on how often a fetch may run, in seconds (#10). Not enforced by
    /// this crate's own reload path yet — #21's auto-refresh is what
    /// consumes it — but it's plumbed through here now so both the manual
    /// reload cooldown and #21 read the same setting.
    pub min_fetch_interval_seconds: u32,
    /// Color theme (#19): `light`, `dark`, or `system` (follows the OS
    /// appearance). Defaults to `light`; an unrecognized value falls back to
    /// the default rather than failing startup — see [`Config::resolve`].
    pub theme: ThemeMode,
    /// Price per API request (#18), in whatever unit the operator has in
    /// mind — this crate never assumes a currency. `None` by default: the
    /// per-request price depends on the account's plan and there is no way
    /// to know it from here, so no estimated amount is ever shown unless
    /// this is explicitly configured. See `usage.rs`'s module doc.
    pub request_price: Option<f64>,
    /// Daily request-count budget (#18): once today's total across every
    /// tracked endpoint approaches or reaches this, the header's usage line
    /// switches to a warning/danger color — see `usage::budget_status`.
    /// Deliberately a request count, not a monetary amount: unlike
    /// `request_price`, this always has a value to compare against (request
    /// counts are always known), so it works whether or not a price is
    /// configured.
    pub daily_request_budget: Option<u32>,
}

const DEFAULT_USERNAME: &str = "XDevelopers";
const DEFAULT_MAX_RESULTS: u32 = 20;
const MAX_RESULTS_RANGE: std::ops::RangeInclusive<u32> = 5..=100;
/// 60s: comfortably above the per-window cost of a single reload (one or
/// two requests) against even X's tighter per-endpoint rate-limit windows,
/// while still being responsive to a human clicking the reload button.
const DEFAULT_MIN_FETCH_INTERVAL_SECONDS: u32 = 60;

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
    #[serde(default)]
    min_fetch_interval_seconds: Option<u32>,
    /// Non-secret (see [`Config::oauth_client_id`]), so — unlike
    /// `bearer_token` below — this key is allowed in `config.toml`.
    #[serde(default)]
    oauth_client_id: Option<String>,
    /// Raw `theme` value (#19), parsed by [`Config::resolve`] rather than
    /// here so an unrecognized value can fall back to the default instead of
    /// failing the whole file load.
    #[serde(default)]
    theme: Option<String>,
    /// Non-secret (see [`Config::request_price`]'s doc), so this key is
    /// allowed in `config.toml` like `oauth_client_id` above.
    #[serde(default)]
    request_price: Option<f64>,
    /// Non-secret, same reasoning as `request_price`.
    #[serde(default)]
    daily_request_budget: Option<u32>,
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
        // This is the real startup path, and the only place the one-time
        // Time Machine exclusion is worth its ~1s subprocess.
        if paths.ensure_dirs()? {
            paths.exclude_cache_from_backups();
        }
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
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());

        let oauth_client_id = var("X_OAUTH_CLIENT_ID")
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
            .or_else(|| {
                file.oauth_client_id
                    .map(|c| c.trim().to_string())
                    .filter(|c| !c.is_empty())
            });

        // Since #7, an OAuth session is an equally valid credential to the
        // bearer token, so only the *combination* of both being absent is a
        // hard failure — either one alone is enough to run.
        if bearer_token.is_none() && oauth_client_id.is_none() {
            bail!(
                "no credential is configured. Set X_BEARER_TOKEN for app-only access, \
                 or X_OAUTH_CLIENT_ID (or oauth_client_id in config.toml) to sign in \
                 with X via OAuth."
            );
        }

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

        let min_fetch_interval_seconds = match var("X_MIN_FETCH_INTERVAL_SECONDS") {
            Some(raw) => raw.trim().parse::<u32>().with_context(|| {
                format!("X_MIN_FETCH_INTERVAL_SECONDS is not a number: {raw:?}")
            })?,
            None => file
                .min_fetch_interval_seconds
                .unwrap_or(DEFAULT_MIN_FETCH_INTERVAL_SECONDS),
        };
        if min_fetch_interval_seconds == 0 {
            bail!(
                "X_MIN_FETCH_INTERVAL_SECONDS (or min_fetch_interval_seconds in config.toml) \
                 must be greater than 0"
            );
        }

        // env > config.toml > default, same layering as everything else
        // above. Unlike the numeric settings, an unrecognized value here
        // must not `bail!` — a typo'd theme is cosmetic, not a reason to
        // block startup (#19) — so it falls back to the default and warns
        // via eprintln!, the project's established pattern for
        // non-fatal notices (see main.rs).
        // env > config.toml > default, same layering as everything else
        // above. Unlike the numeric settings, an unrecognized value here
        // must not `bail!` — a typo'd theme is cosmetic, not a reason to
        // block startup (#19) — so it falls back to the default and warns
        // via eprintln!, the project's established pattern for
        // non-fatal notices (see main.rs).
        let theme = var("X_THEME")
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .or_else(|| {
                file.theme
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
            })
            .and_then(|raw| {
                ThemeMode::parse(&raw).or_else(|| {
                    eprintln!(
                        "warning: unrecognized theme {raw:?} (expected light, dark, or \
                         system); using {} instead",
                        ThemeMode::default()
                    );
                    None
                })
            })
            .unwrap_or_default();

        let request_price = resolve_request_price(&var, file.request_price)?;
        let daily_request_budget = resolve_daily_request_budget(&var, file.daily_request_budget)?;

        Ok(Self {
            bearer_token,
            oauth_client_id,
            target_username,
            max_results,
            min_fetch_interval_seconds,
            theme,
            request_price,
            daily_request_budget,
        })
    }
}

/// Resolve `request_price` (#18): env > file > unset, the same precedence
/// every other setting in [`Config::resolve`] uses — split out from there
/// only to keep that function under clippy's line-count lint, not because
/// the logic itself is reused elsewhere.
///
/// Unlike every numeric setting `Config::resolve` handles inline, a
/// *missing* value here is the normal case, not something to default away
/// — see [`Config::request_price`]'s doc for why there is no built-in
/// default. Still validated when present, from either source: a negative
/// or non-finite price would silently corrupt every estimated amount
/// downstream.
fn resolve_request_price(
    var: &impl Fn(&str) -> Option<String>,
    file_value: Option<f64>,
) -> Result<Option<f64>> {
    let (value, source) = match var("X_REQUEST_PRICE") {
        Some(raw) => {
            let value = raw
                .trim()
                .parse::<f64>()
                .with_context(|| format!("X_REQUEST_PRICE is not a number: {raw:?}"))?;
            (Some(value), "X_REQUEST_PRICE")
        }
        None => (file_value, "request_price in config.toml"),
    };
    if let Some(value) = value
        && (!value.is_finite() || value < 0.0)
    {
        bail!("{source} must be a non-negative number, got {value}");
    }
    Ok(value)
}

/// Resolve `daily_request_budget` (#18): env > file > unset. Split out for
/// the same reason as [`resolve_request_price`]. No validation beyond
/// parsing as `u32`: every value in that range (including zero) is
/// meaningful to `usage::budget_status`.
fn resolve_daily_request_budget(
    var: &impl Fn(&str) -> Option<String>,
    file_value: Option<u32>,
) -> Result<Option<u32>> {
    match var("X_DAILY_REQUEST_BUDGET") {
        Some(raw) => Ok(Some(raw.trim().parse::<u32>().with_context(|| {
            format!("X_DAILY_REQUEST_BUDGET is not a number: {raw:?}")
        })?)),
        None => Ok(file_value),
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, DEFAULT_MAX_RESULTS, DEFAULT_USERNAME, FileSettings};
    use crate::theme::ThemeMode;

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

        assert_eq!(config.bearer_token.as_deref(), Some("token"));
        assert_eq!(config.target_username, DEFAULT_USERNAME);
        assert_eq!(config.max_results, DEFAULT_MAX_RESULTS);
        assert_eq!(
            config.min_fetch_interval_seconds,
            super::DEFAULT_MIN_FETCH_INTERVAL_SECONDS
        );
        assert_eq!(config.theme, ThemeMode::default());
    }

    // Since #7, a bearer token is one of *two* valid credentials (the other
    // being an OAuth client id), so "no token" alone is no longer an error —
    // only "neither credential" is. This test used to be
    // `rejects_a_missing_token`; it now asserts the new failure condition
    // instead of the old one.
    #[test]
    fn rejects_when_no_credential_is_configured() {
        let error = Config::resolve(vars(&[]), FileSettings::default())
            .unwrap_err()
            .to_string();
        assert!(error.contains("X_BEARER_TOKEN"), "{error}");
        assert!(error.contains("X_OAUTH_CLIENT_ID"), "{error}");
    }

    // A blank token must still count as "not configured" rather than being
    // used verbatim — but with an oauth_client_id present, that no longer
    // means resolution fails, only that `bearer_token` ends up `None`.
    #[test]
    fn treats_a_blank_token_as_unset_rather_than_a_literal_value() {
        let config = Config::resolve(
            vars(&[
                ("X_BEARER_TOKEN", "   "),
                ("X_OAUTH_CLIENT_ID", "client-123"),
            ]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.bearer_token, None);
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
        assert_eq!(config.bearer_token.as_deref(), Some("token"));
    }

    #[test]
    fn resolve_succeeds_with_only_an_oauth_client_id() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.bearer_token, None);
        assert_eq!(config.oauth_client_id.as_deref(), Some("client-123"));
    }

    #[test]
    fn resolve_succeeds_with_both_credentials_configured() {
        let config = Config::resolve(
            vars(&[
                ("X_BEARER_TOKEN", "token"),
                ("X_OAUTH_CLIENT_ID", "client-123"),
            ]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.bearer_token.as_deref(), Some("token"));
        assert_eq!(config.oauth_client_id.as_deref(), Some("client-123"));
    }

    #[test]
    fn trims_the_oauth_client_id() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "  client-123\n")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.oauth_client_id.as_deref(), Some("client-123"));
    }

    #[test]
    fn resolve_reads_the_oauth_client_id_from_the_file_when_env_is_unset() {
        let file = FileSettings {
            oauth_client_id: Some("file-client".to_string()),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[]), file).unwrap();
        assert_eq!(config.oauth_client_id.as_deref(), Some("file-client"));
    }

    #[test]
    fn resolve_prefers_the_env_oauth_client_id_over_the_file() {
        let file = FileSettings {
            oauth_client_id: Some("file-client".to_string()),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "env-client")]), file).unwrap();
        assert_eq!(config.oauth_client_id.as_deref(), Some("env-client"));
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

    // --- min_fetch_interval_seconds layering (env > file > default, #10) ---

    #[test]
    fn resolve_reads_min_fetch_interval_from_the_file_when_env_is_unset() {
        let file = FileSettings {
            min_fetch_interval_seconds: Some(120),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_BEARER_TOKEN", "token")]), file).unwrap();
        assert_eq!(config.min_fetch_interval_seconds, 120);
    }

    #[test]
    fn resolve_prefers_the_env_min_fetch_interval_over_the_file() {
        let file = FileSettings {
            min_fetch_interval_seconds: Some(120),
            ..FileSettings::default()
        };
        let config = Config::resolve(
            vars(&[
                ("X_BEARER_TOKEN", "token"),
                ("X_MIN_FETCH_INTERVAL_SECONDS", "30"),
            ]),
            file,
        )
        .unwrap();
        assert_eq!(config.min_fetch_interval_seconds, 30);
    }

    #[test]
    fn resolve_rejects_a_min_fetch_interval_of_zero() {
        let error = Config::resolve(
            vars(&[
                ("X_BEARER_TOKEN", "token"),
                ("X_MIN_FETCH_INTERVAL_SECONDS", "0"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("X_MIN_FETCH_INTERVAL_SECONDS"), "{error}");
    }

    #[test]
    fn resolve_rejects_a_non_numeric_min_fetch_interval() {
        let error = Config::resolve(
            vars(&[
                ("X_BEARER_TOKEN", "token"),
                ("X_MIN_FETCH_INTERVAL_SECONDS", "soon"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("not a number"), "{error}");
    }

    // --- theme layering (env > file > default, #19) ---

    #[test]
    fn resolve_parses_the_theme_from_env() {
        for (raw, expected) in [
            ("light", ThemeMode::Light),
            ("dark", ThemeMode::Dark),
            ("system", ThemeMode::System),
        ] {
            let config = Config::resolve(
                vars(&[("X_BEARER_TOKEN", "token"), ("X_THEME", raw)]),
                FileSettings::default(),
            )
            .unwrap();
            assert_eq!(config.theme, expected, "{raw}");
        }
    }

    #[test]
    fn resolve_theme_is_case_insensitive_and_trims_whitespace() {
        let config = Config::resolve(
            vars(&[("X_BEARER_TOKEN", "token"), ("X_THEME", "  DARK\n")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.theme, ThemeMode::Dark);
    }

    #[test]
    fn resolve_reads_the_theme_from_the_file_when_env_is_unset() {
        let file = FileSettings {
            theme: Some("dark".to_string()),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_BEARER_TOKEN", "token")]), file).unwrap();
        assert_eq!(config.theme, ThemeMode::Dark);
    }

    #[test]
    fn resolve_prefers_the_env_theme_over_the_file() {
        let file = FileSettings {
            theme: Some("dark".to_string()),
            ..FileSettings::default()
        };
        let config = Config::resolve(
            vars(&[("X_BEARER_TOKEN", "token"), ("X_THEME", "light")]),
            file,
        )
        .unwrap();
        assert_eq!(config.theme, ThemeMode::Light);
    }

    #[test]
    fn resolve_falls_back_to_the_default_theme_when_unset() {
        let config = Config::resolve(
            vars(&[("X_BEARER_TOKEN", "token")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.theme, ThemeMode::default());
    }

    // An unrecognized theme must not fail startup (#19) — it falls back to
    // the default. This must hold for both the env and the file source.

    #[test]
    fn resolve_falls_back_to_the_default_theme_on_an_unrecognized_env_value() {
        let config = Config::resolve(
            vars(&[("X_BEARER_TOKEN", "token"), ("X_THEME", "solarized")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.theme, ThemeMode::default());
    }

    #[test]
    fn resolve_falls_back_to_the_default_theme_on_an_unrecognized_file_value() {
        let file = FileSettings {
            theme: Some("solarized".to_string()),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_BEARER_TOKEN", "token")]), file).unwrap();
        assert_eq!(config.theme, ThemeMode::default());
    }

    #[test]
    fn resolve_falls_back_to_the_default_theme_when_blank() {
        let config = Config::resolve(
            vars(&[("X_BEARER_TOKEN", "token"), ("X_THEME", "   ")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.theme, ThemeMode::default());
    }

    // --- request_price / daily_request_budget (#18) ---

    #[test]
    fn request_price_and_daily_budget_are_unset_by_default() {
        let config = Config::resolve(
            vars(&[("X_BEARER_TOKEN", "token")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.request_price, None);
        assert_eq!(config.daily_request_budget, None);
    }

    #[test]
    fn parses_the_request_price_from_env() {
        let config = Config::resolve(
            vars(&[("X_BEARER_TOKEN", "token"), ("X_REQUEST_PRICE", "0.015")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.request_price, Some(0.015));
    }

    #[test]
    fn rejects_a_non_numeric_request_price() {
        let error = Config::resolve(
            vars(&[("X_BEARER_TOKEN", "token"), ("X_REQUEST_PRICE", "free")]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("X_REQUEST_PRICE"), "{error}");
    }

    #[test]
    fn rejects_a_negative_request_price() {
        let error = Config::resolve(
            vars(&[("X_BEARER_TOKEN", "token"), ("X_REQUEST_PRICE", "-0.01")]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("X_REQUEST_PRICE"), "{error}");
    }

    #[test]
    fn resolve_reads_the_request_price_from_the_file_when_env_is_unset() {
        let file = FileSettings {
            request_price: Some(0.02),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_BEARER_TOKEN", "token")]), file).unwrap();
        assert_eq!(config.request_price, Some(0.02));
    }

    #[test]
    fn resolve_prefers_the_env_request_price_over_the_file() {
        let file = FileSettings {
            request_price: Some(0.02),
            ..FileSettings::default()
        };
        let config = Config::resolve(
            vars(&[("X_BEARER_TOKEN", "token"), ("X_REQUEST_PRICE", "0.05")]),
            file,
        )
        .unwrap();
        assert_eq!(config.request_price, Some(0.05));
    }

    #[test]
    fn resolve_rejects_a_negative_request_price_from_the_file() {
        let file = FileSettings {
            request_price: Some(-1.0),
            ..FileSettings::default()
        };
        let error = Config::resolve(vars(&[("X_BEARER_TOKEN", "token")]), file)
            .unwrap_err()
            .to_string();
        assert!(error.contains("request_price"), "{error}");
    }

    #[test]
    fn parses_the_daily_request_budget_from_env() {
        let config = Config::resolve(
            vars(&[
                ("X_BEARER_TOKEN", "token"),
                ("X_DAILY_REQUEST_BUDGET", "500"),
            ]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.daily_request_budget, Some(500));
    }

    #[test]
    fn rejects_a_non_numeric_daily_request_budget() {
        let error = Config::resolve(
            vars(&[
                ("X_BEARER_TOKEN", "token"),
                ("X_DAILY_REQUEST_BUDGET", "lots"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("X_DAILY_REQUEST_BUDGET"), "{error}");
    }

    #[test]
    fn resolve_reads_the_daily_request_budget_from_the_file_when_env_is_unset() {
        let file = FileSettings {
            daily_request_budget: Some(200),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_BEARER_TOKEN", "token")]), file).unwrap();
        assert_eq!(config.daily_request_budget, Some(200));
    }

    #[test]
    fn resolve_prefers_the_env_daily_request_budget_over_the_file() {
        let file = FileSettings {
            daily_request_budget: Some(200),
            ..FileSettings::default()
        };
        let config = Config::resolve(
            vars(&[
                ("X_BEARER_TOKEN", "token"),
                ("X_DAILY_REQUEST_BUDGET", "50"),
            ]),
            file,
        )
        .unwrap();
        assert_eq!(config.daily_request_budget, Some(50));
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
