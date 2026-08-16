use anyhow::{Context as _, Result, bail};

/// Runtime configuration, read from the environment (and `.env` if present).
#[derive(Debug, Clone)]
pub struct Config {
    /// App-only Bearer token, used verbatim in the `Authorization` header.
    pub bearer_token: String,
    /// Screen name whose posts are shown, without a leading `@`.
    pub target_username: String,
    /// Posts requested per fetch. The X API accepts 5..=100.
    pub max_results: u32,
}

const DEFAULT_USERNAME: &str = "XDevelopers";
const DEFAULT_MAX_RESULTS: u32 = 20;

impl Config {
    pub fn from_env() -> Result<Self> {
        // A missing .env is fine — the variables may come from the real environment.
        let _ = dotenvy::dotenv();

        let bearer_token = std::env::var("X_BEARER_TOKEN")
            .ok()
            .filter(|t| !t.trim().is_empty())
            .context("X_BEARER_TOKEN is unset. Copy .env.example to .env and fill it in.")?;

        let target_username = std::env::var("X_TARGET_USERNAME")
            .ok()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_USERNAME.to_string());
        let target_username = target_username.trim().trim_start_matches('@').to_string();

        let max_results = match std::env::var("X_MAX_RESULTS") {
            Ok(raw) => raw
                .trim()
                .parse::<u32>()
                .with_context(|| format!("X_MAX_RESULTS is not a number: {raw:?}"))?,
            Err(_) => DEFAULT_MAX_RESULTS,
        };
        if !(5..=100).contains(&max_results) {
            bail!("X_MAX_RESULTS must be between 5 and 100, got {max_results}");
        }

        Ok(Self {
            bearer_token: bearer_token.trim().to_string(),
            target_username,
            max_results,
        })
    }
}
