use anyhow::{Context as _, Result, bail};

/// Runtime configuration, read from the environment (and `.env` if present).
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

impl Config {
    pub(crate) fn from_env() -> Result<Self> {
        // A missing .env is fine — the variables may come from the real environment.
        let _ = dotenvy::dotenv();
        Self::from_vars(|key| std::env::var(key).ok())
    }

    /// Parse and validate the settings from an arbitrary variable lookup.
    ///
    /// Split out from [`Config::from_env`] so the rules below can be tested
    /// without `set_var`, which is `unsafe` and races the other test threads.
    fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let bearer_token = var("X_BEARER_TOKEN")
            .filter(|t| !t.trim().is_empty())
            .context("X_BEARER_TOKEN is unset. Copy .env.example to .env and fill it in.")?;

        let target_username = var("X_TARGET_USERNAME")
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_USERNAME.to_string());
        let target_username = target_username.trim().trim_start_matches('@').to_string();

        let max_results = match var("X_MAX_RESULTS") {
            Some(raw) => raw
                .trim()
                .parse::<u32>()
                .with_context(|| format!("X_MAX_RESULTS is not a number: {raw:?}"))?,
            None => DEFAULT_MAX_RESULTS,
        };
        if !MAX_RESULTS_RANGE.contains(&max_results) {
            bail!(
                "X_MAX_RESULTS must be between {} and {}, got {max_results}",
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
    use super::{Config, DEFAULT_MAX_RESULTS, DEFAULT_USERNAME};

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
        let config = Config::from_vars(vars(&[("X_BEARER_TOKEN", "token")])).unwrap();

        assert_eq!(config.bearer_token, "token");
        assert_eq!(config.target_username, DEFAULT_USERNAME);
        assert_eq!(config.max_results, DEFAULT_MAX_RESULTS);
    }

    #[test]
    fn rejects_a_missing_token() {
        let error = Config::from_vars(vars(&[])).unwrap_err().to_string();
        assert!(error.contains("X_BEARER_TOKEN is unset"), "{error}");
    }

    #[test]
    fn treats_a_blank_token_as_missing() {
        let error = Config::from_vars(vars(&[("X_BEARER_TOKEN", "   ")]))
            .unwrap_err()
            .to_string();
        assert!(error.contains("X_BEARER_TOKEN is unset"), "{error}");
    }

    #[test]
    fn trims_the_token() {
        // A token pasted into .env often carries a trailing newline, and it goes
        // into the Authorization header verbatim.
        let config = Config::from_vars(vars(&[("X_BEARER_TOKEN", "  token\n")])).unwrap();
        assert_eq!(config.bearer_token, "token");
    }

    #[test]
    fn strips_a_leading_at_from_the_username() {
        let config = Config::from_vars(vars(&[
            ("X_BEARER_TOKEN", "token"),
            ("X_TARGET_USERNAME", " @XDevelopers "),
        ]))
        .unwrap();
        assert_eq!(config.target_username, "XDevelopers");
    }

    #[test]
    fn falls_back_to_the_default_username_when_blank() {
        let config = Config::from_vars(vars(&[
            ("X_BEARER_TOKEN", "token"),
            ("X_TARGET_USERNAME", "  "),
        ]))
        .unwrap();
        assert_eq!(config.target_username, DEFAULT_USERNAME);
    }

    #[test]
    fn parses_max_results() {
        let config = Config::from_vars(vars(&[
            ("X_BEARER_TOKEN", "token"),
            ("X_MAX_RESULTS", " 42 "),
        ]))
        .unwrap();
        assert_eq!(config.max_results, 42);
    }

    #[test]
    fn rejects_a_non_numeric_max_results() {
        let error = Config::from_vars(vars(&[
            ("X_BEARER_TOKEN", "token"),
            ("X_MAX_RESULTS", "lots"),
        ]))
        .unwrap_err()
        .to_string();
        assert!(error.contains("not a number"), "{error}");
    }

    #[test]
    fn accepts_both_ends_of_the_api_range() {
        for raw in ["5", "100"] {
            let config =
                Config::from_vars(vars(&[("X_BEARER_TOKEN", "token"), ("X_MAX_RESULTS", raw)]))
                    .unwrap();
            assert_eq!(config.max_results.to_string(), raw);
        }
    }

    #[test]
    fn rejects_max_results_outside_the_api_range() {
        for raw in ["4", "101"] {
            let error =
                Config::from_vars(vars(&[("X_BEARER_TOKEN", "token"), ("X_MAX_RESULTS", raw)]))
                    .unwrap_err()
                    .to_string();
            assert!(error.contains("between 5 and 100"), "{raw}: {error}");
        }
    }
}
