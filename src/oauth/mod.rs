//! OAuth 2.0 Authorization Code + PKCE (#7).
//!
//! Ties the three seams together: [`pkce`] generates the verifier/challenge
//! and state, [`callback`] runs the loopback listener that catches the
//! redirect, and [`tokens`] persists what comes back. [`sign_in`] is the only
//! entry point `ui.rs` calls to run the interactive flow; [`resolve_credential`]
//! is what both `ui.rs` (at startup) and `--fetch-only` use to find a usable
//! credential without opening a browser.

mod callback;
mod pkce;
pub(crate) mod tokens;

use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use gpui::BackgroundExecutor;
use ureq::Agent;

use crate::config::Config;
use crate::paths::Paths;
use tokens::{TokenResponse, TokenSet};

/// `https://api.x.com/2/oauth2/token`, per the issue's confirmed design.
const TOKEN_URL: &str = "https://api.x.com/2/oauth2/token";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Current time as a Unix timestamp — the one real clock read in this
/// module. Every function below it takes `now` as a parameter instead, the
/// same seam `config.rs` uses for environment lookups.
pub(crate) fn unix_now() -> i64 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    i64::try_from(secs).unwrap_or(i64::MAX)
}

fn agent() -> Agent {
    // Mirrors `x_api::client::XClient::new`'s config: read the body
    // ourselves on a non-2xx status so the token endpoint's own error text
    // makes it into the message, and cap the wait like every other request
    // this app makes.
    let config = Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .http_status_as_error(false)
        .build();
    config.into()
}

/// Run the interactive sign-in flow end to end: build the PKCE pair and
/// state, open the system browser at the authorize URL, wait for the
/// loopback redirect, and exchange the code for tokens.
///
/// Runs on `executor` so the loopback listener's poll loop can yield
/// between accept attempts — see `callback::await_authorization_code`. Not
/// unit-tested directly: it opens a real browser and binds a real socket.
pub(crate) async fn sign_in(executor: &BackgroundExecutor, client_id: &str) -> Result<TokenSet> {
    let random = pkce::OsRandom;
    let verifier = pkce::generate_code_verifier(&random)?;
    let challenge = pkce::code_challenge(&verifier);
    let state = pkce::generate_state(&random)?;
    let redirect_uri = callback::redirect_uri();

    let url = pkce::build_authorize_url(client_id, &redirect_uri, &challenge, &state);
    std::process::Command::new("open")
        .arg(&url)
        .status()
        .context("could not open the browser")?;

    let code = callback::await_authorization_code(executor, &state).await?;
    let response = exchange_authorization_code(client_id, &code, &verifier, &redirect_uri)?;
    Ok(TokenSet::from_response(response, unix_now()))
}

fn exchange_authorization_code(
    client_id: &str,
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> Result<TokenResponse> {
    request_token(&[
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", client_id),
        ("code_verifier", code_verifier),
    ])
}

fn refresh_access_token(client_id: &str, refresh_token: &str) -> Result<TokenResponse> {
    request_token(&[
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client_id),
    ])
}

/// POST one token-endpoint request. This is a **public client** (#7's
/// confirmed design): `client_id` travels in the body alongside the grant,
/// never as HTTP Basic auth, because there is no client secret to pair it
/// with.
fn request_token(form: &[(&str, &str)]) -> Result<TokenResponse> {
    let mut response = agent()
        .post(TOKEN_URL)
        .send_form(form.iter().copied())
        .context("token request failed")?;
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_to_string()
        .context("could not read the token response body")?;

    if !(200..300).contains(&status) {
        let detail = tokens::describe_token_error(&body).unwrap_or_else(|| body.clone());
        bail!("token request failed with HTTP {status}: {detail}");
    }

    serde_json::from_str(&body).context("could not parse the token response")
}

/// Which credential [`resolve_credential`] found. Both carry a token that
/// goes into the same `Authorization: Bearer` header, so the token alone
/// cannot tell them apart — and callers need to, because an app-only bearer
/// token cannot read the home timeline or write anything (#11, #14–#17).
/// `ui.rs` uses this to keep offering "Sign in with X" while running on a
/// bearer token (#31).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Credential {
    /// A user-context access token from a stored (or just-refreshed) OAuth
    /// session.
    OAuth(String),
    /// The app-only bearer token from configuration.
    Bearer(String),
}

impl Credential {
    pub(crate) fn token(&self) -> &str {
        match self {
            Self::OAuth(token) | Self::Bearer(token) => token,
        }
    }

    /// Whether this is a user-context credential. `false` means the app is
    /// running app-only and signing in would strictly widen what it can do.
    pub(crate) fn is_oauth(&self) -> bool {
        matches!(self, Self::OAuth(_))
    }
}

/// Which timeline `ui.rs` shows (#11), decided once — alongside the
/// credential itself — rather than scattered as `if credential.is_oauth()`
/// checks through `cache.rs` and `ui.rs`. A pure function of the credential
/// kind: an OAuth session can read the home timeline
/// (`GET /2/users/:id/timelines/reverse_chronological`); an app-only bearer
/// token gets a 401 there, so it keeps the milestone-1 single-user view
/// (`GET /2/users/:id/tweets` for `Config::target_username`) instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimelineSource {
    /// The signed-in user's own home timeline.
    Home,
    /// `Config::target_username`'s posts — the pre-#11 behavior, kept as the
    /// fallback for an app-only bearer token.
    SingleUser,
}

impl TimelineSource {
    pub(crate) fn for_credential(credential: &Credential) -> Self {
        // TODO(#11): stubbed to always report `SingleUser`, so the
        // OAuth-maps-to-Home test fails on behavior instead of a missing
        // symbol.
        let _ = credential;
        Self::SingleUser
    }
}

/// Find a usable credential without opening a browser: a fresh stored OAuth
/// session, a stale one refreshed in place, or the app-only bearer token —
/// in that order. `None` means neither is currently usable and the caller
/// (`--fetch-only`, or `ui.rs` at startup) should ask the user to sign in.
pub(crate) fn resolve_credential(
    config: &Config,
    paths: &Paths,
    now: i64,
) -> Result<Option<Credential>> {
    if let Some(stored) = tokens::load(paths)? {
        if !stored.needs_refresh(now) {
            return Ok(Some(Credential::OAuth(stored.access_token)));
        }
        if let (Some(client_id), Some(refresh)) = (&config.oauth_client_id, &stored.refresh_token) {
            let response = refresh_access_token(client_id, refresh)?;
            let refreshed = TokenSet::from_response(response, now);
            tokens::save(paths, &refreshed)?;
            return Ok(Some(Credential::OAuth(refreshed.access_token)));
        }
        // Stale and unrefreshable (no client id configured, or X issued no
        // refresh token) — fall through to the bearer token rather than
        // erroring, since that may still be a usable credential.
    }

    Ok(config.bearer_token.clone().map(Credential::Bearer))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(bearer_token: Option<&str>, oauth_client_id: Option<&str>) -> Config {
        Config {
            bearer_token: bearer_token.map(str::to_string),
            oauth_client_id: oauth_client_id.map(str::to_string),
            target_username: "someone".to_string(),
            max_results: 20,
            min_fetch_interval_seconds: 60,
            theme: crate::theme::ThemeMode::default(),
        }
    }

    fn test_paths(root: &std::path::Path) -> Paths {
        let home = root.display().to_string();
        Paths::from_vars(move |key| (key == "HOME").then(|| home.clone())).unwrap()
    }

    fn temp_root(label: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "twigpui-test-oauth-mod-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn timeline_source_is_home_for_an_oauth_credential() {
        assert_eq!(
            TimelineSource::for_credential(&Credential::OAuth("token".into())),
            TimelineSource::Home
        );
    }

    #[test]
    fn timeline_source_is_single_user_for_a_bearer_credential() {
        assert_eq!(
            TimelineSource::for_credential(&Credential::Bearer("token".into())),
            TimelineSource::SingleUser
        );
    }

    #[test]
    fn resolve_credential_prefers_a_fresh_stored_session_over_the_bearer_token() {
        let root = temp_root("fresh");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        tokens::save(
            &paths,
            &TokenSet {
                access_token: "oauth-token".into(),
                refresh_token: None,
                expires_at: 1_000_000,
            },
        )
        .unwrap();

        let config = test_config(Some("bearer-token"), None);
        let credential = resolve_credential(&config, &paths, 0).unwrap();
        assert_eq!(credential, Some(Credential::OAuth("oauth-token".into())));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn resolve_credential_falls_back_to_the_bearer_token_when_the_stored_session_is_stale_and_unrefreshable()
     {
        let root = temp_root("stale");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        tokens::save(
            &paths,
            &TokenSet {
                access_token: "oauth-token".into(),
                refresh_token: None,
                expires_at: 0,
            },
        )
        .unwrap();

        // No client id and no refresh token on the stored session, so a
        // refresh isn't possible — this must fall through to the bearer
        // token rather than erroring.
        let config = test_config(Some("bearer-token"), None);
        let credential = resolve_credential(&config, &paths, 1_000_000).unwrap();
        assert_eq!(credential, Some(Credential::Bearer("bearer-token".into())));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn resolve_credential_uses_the_bearer_token_when_there_is_no_stored_session() {
        let root = temp_root("none");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let config = test_config(Some("bearer-token"), None);
        let credential = resolve_credential(&config, &paths, 0).unwrap();
        // #31: the caller must be able to tell this apart from an OAuth
        // session, so it can keep offering to sign in.
        assert_eq!(credential, Some(Credential::Bearer("bearer-token".into())));
        assert!(!credential.unwrap().is_oauth());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn resolve_credential_is_none_when_nothing_is_configured() {
        let root = temp_root("nothing");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let config = test_config(None, None);
        let credential = resolve_credential(&config, &paths, 0).unwrap();
        assert!(credential.is_none());

        std::fs::remove_dir_all(&root).unwrap();
    }
}
