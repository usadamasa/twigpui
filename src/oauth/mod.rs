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
    /// session, plus the scope X granted it (#14). `scope: None` means
    /// unrecorded/unknown — a pre-#14 token, or one whose token-endpoint
    /// response carried no `scope` at all — which `tokens::has_scope`
    /// treats as insufficient for anything scope-gated.
    OAuth {
        token: String,
        scope: Option<String>,
    },
    /// The app-only bearer token from configuration.
    Bearer(String),
}

impl Credential {
    pub(crate) fn token(&self) -> &str {
        match self {
            Self::OAuth { token, .. } | Self::Bearer(token) => token,
        }
    }

    /// Whether this is a user-context credential. `false` means the app is
    /// running app-only and signing in would strictly widen what it can do.
    pub(crate) fn is_oauth(&self) -> bool {
        matches!(self, Self::OAuth { .. })
    }

    /// The granted scope for an OAuth session — `None` for a bearer
    /// credential (meaningless there) or an OAuth session whose scope
    /// wasn't recorded. `ui.rs` feeds this to `tokens::has_scope` to decide
    /// whether to offer re-authorization (#14).
    pub(crate) fn scope(&self) -> Option<&str> {
        match self {
            Self::OAuth { scope, .. } => scope.as_deref(),
            Self::Bearer(_) => None,
        }
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
        if credential.is_oauth() {
            Self::Home
        } else {
            Self::SingleUser
        }
    }
}

/// Why a stored OAuth session could not be used as-is, and
/// [`resolve_credential`] had to fall through to the bearer token (or to
/// nothing) instead (#54). `ui.rs` renders this on screen and names the fix,
/// because the app otherwise gives no other sign anything changed: the
/// timeline still renders (on the bearer token, if one is configured), so
/// silently losing the ability to post looks identical to "everything is
/// fine".
///
/// A fresh session, or one refreshed successfully, carries no reason at all
/// — see [`Resolution::demotion`] — since nothing degraded in that case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionDemotion {
    /// The stored session needed a refresh, but no `oauth_client_id` is
    /// configured to refresh it with — the exact shape #54 was filed from: a
    /// shell (or a bundled `.app` launch, #40) that never saw the client id.
    NoClientId,
    /// The stored session needed a refresh, but carries no refresh token to
    /// refresh with — a session that predates `offline.access`, or one X
    /// simply never issued a refresh token for.
    NoRefreshToken,
    /// A refresh was attempted (a client id and a refresh token were both
    /// present) but X rejected it. `detail` carries the token endpoint's own
    /// error text — a revoked token and one expired beyond recovery both
    /// surface as the same generic 400 here, so there is nothing more
    /// specific to classify locally.
    Rejected(String),
}

/// What [`resolve_credential`] found: the credential to use, if any, and —
/// since #54 — whether a stored OAuth session had to be demoted along the
/// way, and why. A stored-but-unrefreshable session is a materially
/// different state from "no credential was ever configured": both can
/// resolve to the same [`Credential::Bearer`] (or no credential at all), but
/// only one of them means the user was signed in a moment ago and now
/// silently isn't.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resolution {
    pub(crate) credential: Option<Credential>,
    pub(crate) demotion: Option<SessionDemotion>,
}

/// Find a usable credential without opening a browser: a fresh stored OAuth
/// session, a stale one refreshed in place, or the app-only bearer token —
/// in that order. `credential: None` means neither is currently usable and
/// the caller (`--fetch-only`, or `ui.rs` at startup) should ask the user to
/// sign in.
pub(crate) fn resolve_credential(config: &Config, paths: &Paths, now: i64) -> Result<Resolution> {
    let Some(stored) = tokens::load(paths)? else {
        return Ok(Resolution {
            credential: config.bearer_token.clone().map(Credential::Bearer),
            demotion: None,
        });
    };

    if !stored.needs_refresh(now) {
        return Ok(Resolution {
            credential: Some(Credential::OAuth {
                token: stored.access_token,
                scope: stored.scope,
            }),
            demotion: None,
        });
    }

    let Some(client_id) = &config.oauth_client_id else {
        return Ok(Resolution {
            credential: config.bearer_token.clone().map(Credential::Bearer),
            demotion: Some(SessionDemotion::NoClientId),
        });
    };
    let Some(refresh) = &stored.refresh_token else {
        return Ok(Resolution {
            credential: config.bearer_token.clone().map(Credential::Bearer),
            demotion: Some(SessionDemotion::NoRefreshToken),
        });
    };

    match refresh_access_token(client_id, refresh) {
        Ok(response) => {
            let mut refreshed = TokenSet::from_response(response, now);
            // RFC 6749 §5.1 lets the token endpoint omit `scope` on a
            // refresh when it's unchanged from what's already granted —
            // `carried_scope` is what keeps a working `tweet.write` session
            // from silently reverting to "unknown" (and spuriously reviving
            // the re-authorize banner) on every routine refresh.
            refreshed.scope = carried_scope(refreshed.scope, stored.scope.as_deref());
            tokens::save(paths, &refreshed)?;
            Ok(Resolution {
                credential: Some(Credential::OAuth {
                    token: refreshed.access_token,
                    scope: refreshed.scope,
                }),
                demotion: None,
            })
        }
        Err(error) => {
            // X rejected the refresh outright — revoked, or expired beyond
            // recovery. #54: demote to the bearer token exactly like the two
            // cases above, rather than propagate as a hard error, which
            // would blank whatever the timeline was already showing over a
            // session problem the *read* path doesn't actually have (see
            // this module's doc and the issue's "do not break the bearer
            // fallback" requirement).
            Ok(Resolution {
                credential: config.bearer_token.clone().map(Credential::Bearer),
                demotion: Some(SessionDemotion::Rejected(format!("{error:#}"))),
            })
        }
    }
}

/// Explain why a stored OAuth session couldn't be used as-is (#54), for
/// `ui.rs`'s on-screen banner and `--fetch-only`'s stderr alike. Always names
/// the concrete `config.toml` path for the one case where there is a setting
/// to change — mirrors `main.rs::report_startup_error`'s rule (#40): point at
/// the file itself, not just "configuration error". For the other two cases
/// there is no file to point at — the fix is clicking "Sign in with X" again
/// — so the message says that instead; `ui.rs`'s `offers_sign_in` already
/// keeps that button reachable whenever a client id is configured, which is
/// true in both of those cases (only [`SessionDemotion::NoClientId`] itself
/// has none to offer).
pub(crate) fn describe_demotion(demotion: &SessionDemotion, paths: &Paths) -> String {
    match demotion {
        SessionDemotion::NoClientId => format!(
            "Your X sign-in session expired and could not be renewed: no oauth_client_id is \
             configured. Add oauth_client_id = \"…\" to {} (or set X_OAUTH_CLIENT_ID), then \
             restart twigpui.",
            paths.settings_file().display()
        ),
        SessionDemotion::NoRefreshToken => "Your X sign-in session expired and carries no \
             refresh token, so it can't be renewed automatically. Click \"Sign in with X\" to \
             start a new session."
            .to_string(),
        SessionDemotion::Rejected(detail) => format!(
            "Your X sign-in session expired and X rejected the attempt to renew it ({detail}). \
             Click \"Sign in with X\" to start a new session."
        ),
    }
}

/// What scope to persist across a refresh (#14): the freshly-returned scope
/// if the token endpoint sent one, otherwise whatever was already recorded.
/// Pure and tested directly rather than only through `resolve_credential`'s
/// full path, since exercising the real refresh branch needs a live HTTP
/// call this crate's tests never make (see the module doc).
fn carried_scope(refreshed_scope: Option<String>, previous_scope: Option<&str>) -> Option<String> {
    refreshed_scope.or_else(|| previous_scope.map(str::to_string))
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
            log_level: crate::log::Level::default(),
            request_price: None,
            daily_request_budget: None,
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
            TimelineSource::for_credential(&Credential::OAuth {
                token: "token".into(),
                scope: None
            }),
            TimelineSource::Home
        );
    }

    #[test]
    fn credential_scope_is_none_for_a_bearer_token() {
        assert_eq!(Credential::Bearer("token".into()).scope(), None);
    }

    #[test]
    fn credential_scope_reflects_the_oauth_sessions_recorded_scope() {
        let credential = Credential::OAuth {
            token: "token".into(),
            scope: Some("tweet.read tweet.write".into()),
        };
        assert_eq!(credential.scope(), Some("tweet.read tweet.write"));
    }

    // --- carried_scope ---

    #[test]
    fn carried_scope_prefers_the_freshly_returned_scope() {
        assert_eq!(
            carried_scope(Some("tweet.read tweet.write".into()), Some("tweet.read")),
            Some("tweet.read tweet.write".into())
        );
    }

    #[test]
    fn carried_scope_falls_back_to_the_previous_scope_when_the_refresh_omitted_it() {
        // RFC 6749 §5.1: the token endpoint may omit `scope` on a refresh
        // when unchanged — this must not silently downgrade a working
        // `tweet.write` session back to "unknown".
        assert_eq!(
            carried_scope(None, Some("tweet.read tweet.write")),
            Some("tweet.read tweet.write".into())
        );
    }

    #[test]
    fn carried_scope_is_none_when_neither_side_has_one() {
        assert_eq!(carried_scope(None, None), None);
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
                scope: Some("tweet.read tweet.write".into()),
            },
        )
        .unwrap();

        let config = test_config(Some("bearer-token"), None);
        let resolution = resolve_credential(&config, &paths, 0).unwrap();
        assert_eq!(
            resolution.credential,
            Some(Credential::OAuth {
                token: "oauth-token".into(),
                scope: Some("tweet.read tweet.write".into()),
            })
        );
        assert_eq!(resolution.demotion, None);

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
                scope: None,
            },
        )
        .unwrap();

        // No client id and no refresh token on the stored session, so a
        // refresh isn't possible — this must fall through to the bearer
        // token rather than erroring, and report why (#54): absence of a
        // client id is checked first, matching the issue's own diagnosis
        // that this is the primary reported trigger.
        let config = test_config(Some("bearer-token"), None);
        let resolution = resolve_credential(&config, &paths, 1_000_000).unwrap();
        assert_eq!(
            resolution.credential,
            Some(Credential::Bearer("bearer-token".into()))
        );
        assert_eq!(resolution.demotion, Some(SessionDemotion::NoClientId));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn resolve_credential_uses_the_bearer_token_when_there_is_no_stored_session() {
        let root = temp_root("none");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let config = test_config(Some("bearer-token"), None);
        let resolution = resolve_credential(&config, &paths, 0).unwrap();
        // #31: the caller must be able to tell this apart from an OAuth
        // session, so it can keep offering to sign in. No stored session at
        // all means nothing degraded, so there is no demotion reason either.
        assert_eq!(
            resolution.credential,
            Some(Credential::Bearer("bearer-token".into()))
        );
        assert!(!resolution.credential.unwrap().is_oauth());
        assert_eq!(resolution.demotion, None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn resolve_credential_is_none_when_nothing_is_configured() {
        let root = temp_root("nothing");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let config = test_config(None, None);
        let resolution = resolve_credential(&config, &paths, 0).unwrap();
        assert!(resolution.credential.is_none());
        assert_eq!(resolution.demotion, None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- SessionDemotion (#54) ---

    #[test]
    fn resolve_credential_demotes_with_no_client_id_reason_when_a_stale_session_has_a_refresh_token_but_no_client_id()
     {
        // The exact shape #54 was filed from: a stored session that needs
        // refreshing, a refresh token present on it, but no oauth_client_id
        // configured in this run — a shell that never exported it, or a
        // bundled launch (#40).
        let root = temp_root("no-client-id");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        tokens::save(
            &paths,
            &TokenSet {
                access_token: "oauth-token".into(),
                refresh_token: Some("refresh-token".into()),
                expires_at: 0,
                scope: Some("tweet.read tweet.write".into()),
            },
        )
        .unwrap();

        let config = test_config(Some("bearer-token"), None);
        let resolution = resolve_credential(&config, &paths, 1_000_000).unwrap();
        assert_eq!(
            resolution.credential,
            Some(Credential::Bearer("bearer-token".into()))
        );
        assert_eq!(resolution.demotion, Some(SessionDemotion::NoClientId));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn resolve_credential_demotes_with_no_refresh_token_reason_when_the_stored_session_carries_none()
     {
        let root = temp_root("no-refresh-token");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        tokens::save(
            &paths,
            &TokenSet {
                access_token: "oauth-token".into(),
                refresh_token: None,
                expires_at: 0,
                scope: Some("tweet.read tweet.write".into()),
            },
        )
        .unwrap();

        // A client id *is* configured this time — the only thing missing is
        // a refresh token on the stored session itself.
        let config = test_config(Some("bearer-token"), Some("client-id"));
        let resolution = resolve_credential(&config, &paths, 1_000_000).unwrap();
        assert_eq!(
            resolution.credential,
            Some(Credential::Bearer("bearer-token".into()))
        );
        assert_eq!(resolution.demotion, Some(SessionDemotion::NoRefreshToken));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn resolve_credential_demotes_to_no_credential_when_neither_a_bearer_token_nor_a_refreshable_session_exists()
     {
        // A demotion reason must surface even when there is nothing to fall
        // back to at all — `credential: None` and `demotion: Some(_)` are
        // independent facts, and `ui.rs` needs both: the first decides
        // whether the app can read anything, the second decides whether the
        // "session expired" banner shows.
        let root = temp_root("no-fallback");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        tokens::save(
            &paths,
            &TokenSet {
                access_token: "oauth-token".into(),
                refresh_token: None,
                expires_at: 0,
                scope: None,
            },
        )
        .unwrap();

        let config = test_config(None, Some("client-id"));
        let resolution = resolve_credential(&config, &paths, 1_000_000).unwrap();
        assert_eq!(resolution.credential, None);
        assert_eq!(resolution.demotion, Some(SessionDemotion::NoRefreshToken));

        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- describe_demotion (#54) ---

    #[test]
    fn describe_demotion_names_the_config_toml_path_for_a_missing_client_id() {
        let root = temp_root("describe-no-client-id");
        let paths = test_paths(&root);

        let message = describe_demotion(&SessionDemotion::NoClientId, &paths);
        assert!(
            message.contains(&paths.settings_file().display().to_string()),
            "{message}"
        );
        assert!(message.contains("oauth_client_id"), "{message}");
    }

    #[test]
    fn describe_demotion_for_no_refresh_token_points_at_signing_in_again() {
        let root = temp_root("describe-no-refresh-token");
        let paths = test_paths(&root);

        let message = describe_demotion(&SessionDemotion::NoRefreshToken, &paths);
        assert!(message.contains("Sign in with X"), "{message}");
    }

    #[test]
    fn describe_demotion_for_a_rejected_refresh_carries_the_detail_and_points_at_signing_in_again()
    {
        let root = temp_root("describe-rejected");
        let paths = test_paths(&root);

        let message = describe_demotion(
            &SessionDemotion::Rejected("invalid_grant: token expired".to_string()),
            &paths,
        );
        assert!(
            message.contains("invalid_grant: token expired"),
            "{message}"
        );
        assert!(message.contains("Sign in with X"), "{message}");
    }
}
