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
use crate::profile::Profile;
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
    // #169: the port, and so the redirect URI, belong to whichever
    // installation this binary is — a development build must not send the
    // real app's redirect URI, nor bind the port the real one listens on.
    let profile = Profile::current();
    let redirect_uri = callback::redirect_uri(profile);

    let url = pkce::build_authorize_url(client_id, &redirect_uri, &challenge, &state);
    std::process::Command::new("open")
        .arg(&url)
        .status()
        .context("could not open the browser")?;

    let code = callback::await_authorization_code(executor, &state, profile).await?;
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

/// A user-context access token from a stored (or just-refreshed) OAuth
/// session, plus the scope X granted it (#14).
///
/// A struct rather than the enum this was until #33: the app-only bearer
/// token was the only other variant, and every question callers asked of it
/// ("is this OAuth?", "what scope?") existed to work around the fact that
/// it might not be. There is only one kind of credential now.
///
/// `scope: None` means unrecorded/unknown — a pre-#14 token, or one whose
/// token-endpoint response carried no `scope` at all — which
/// `tokens::has_scope` treats as insufficient for anything scope-gated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Credential {
    pub(crate) token: String,
    pub(crate) scope: Option<String>,
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
/// resolve to no credential at all, but only one of them means the user was
/// signed in a moment ago and now silently isn't.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resolution {
    pub(crate) credential: Option<Credential>,
    pub(crate) demotion: Option<SessionDemotion>,
}

/// Find a usable credential without opening a browser: a fresh stored OAuth
/// session, or a stale one refreshed in place. `credential: None` means
/// there is no usable session and the caller (`--fetch-only`, or `ui.rs` at
/// startup) should ask the user to sign in.
///
/// Before #33 this had a third outcome — falling back to the app-only
/// bearer token — which is what `demotion` existed to explain: the timeline
/// still rendered, so silently losing the ability to post looked identical
/// to everything being fine. `demotion` survives the bearer token because
/// the same explanation is still owed, only now the visible consequence is
/// "signed out" rather than "quietly less capable".
pub(crate) fn resolve_credential(config: &Config, paths: &Paths, now: i64) -> Result<Resolution> {
    let Some(stored) = tokens::load(paths)? else {
        return Ok(Resolution {
            credential: None,
            demotion: None,
        });
    };

    if !stored.needs_refresh(now) {
        return Ok(Resolution {
            credential: Some(Credential {
                token: stored.access_token,
                scope: stored.scope,
            }),
            demotion: None,
        });
    }

    let client_id = &config.oauth_client_id;
    let Some(refresh) = &stored.refresh_token else {
        return Ok(Resolution {
            credential: None,
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
                credential: Some(Credential {
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
                credential: None,
                demotion: Some(SessionDemotion::Rejected(format!("{error:#}"))),
            })
        }
    }
}

/// Explain why a stored OAuth session couldn't be used as-is (#54), for
/// `ui.rs`'s on-screen banner and `--fetch-only`'s stderr alike.
///
/// Both remaining cases are fixed the same way — sign in again — so both
/// messages say so. `NoClientId` was a third case until #33 made a client
/// id mandatory at startup: an app that cannot start without one cannot
/// later discover it is missing.
pub(crate) fn describe_demotion(demotion: &SessionDemotion) -> String {
    match demotion {
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

    fn test_config(oauth_client_id: &str) -> Config {
        Config {
            oauth_client_id: oauth_client_id.to_string(),
            target_username: "someone".to_string(),
            max_results: 20,
            min_fetch_interval_seconds: 60,
            theme: crate::theme::ThemeMode::default(),
            log_level: crate::log::Level::default(),
            request_price: None,
            daily_request_budget: None,
            list_id: None,
            auto_sync_list: false,
            sync_interval_seconds: 21_600,
            sync_prune_limit_percent: 10,
            sync_members_refresh_seconds: 604_800,
            auto_refresh: false,
            auto_refresh_interval_seconds: 300,
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
    fn resolve_credential_is_none_when_there_is_no_stored_session() {
        let root = temp_root("nothing");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let config = test_config("client-id");
        let resolution = resolve_credential(&config, &paths, 0).unwrap();
        assert!(resolution.credential.is_none());
        assert_eq!(resolution.demotion, None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- SessionDemotion (#54) ---

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

        let config = test_config("client-id");
        let resolution = resolve_credential(&config, &paths, 1_000_000).unwrap();
        // #33: there is no fallback credential left, so a session that
        // cannot be renewed leaves the app signed out — and says why.
        assert_eq!(resolution.credential, None);
        assert_eq!(resolution.demotion, Some(SessionDemotion::NoRefreshToken));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn resolve_credential_reports_why_even_though_there_is_nothing_to_fall_back_to() {
        // `credential: None` and `demotion: Some(_)` are independent facts,
        // and `ui.rs` needs both: the first decides whether the app can read
        // anything, the second decides whether the "session expired" banner
        // shows.
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

        let config = test_config("client-id");
        let resolution = resolve_credential(&config, &paths, 1_000_000).unwrap();
        assert_eq!(resolution.credential, None);
        assert_eq!(resolution.demotion, Some(SessionDemotion::NoRefreshToken));

        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- describe_demotion (#54) ---

    #[test]
    fn describe_demotion_for_no_refresh_token_points_at_signing_in_again() {
        let message = describe_demotion(&SessionDemotion::NoRefreshToken);
        assert!(message.contains("Sign in with X"), "{message}");
    }

    #[test]
    fn describe_demotion_for_a_rejected_refresh_carries_the_detail_and_points_at_signing_in_again()
    {
        let message = describe_demotion(&SessionDemotion::Rejected(
            "invalid_grant: token expired".to_string(),
        ));
        assert!(
            message.contains("invalid_grant: token expired"),
            "{message}"
        );
        assert!(message.contains("Sign in with X"), "{message}");
    }
}
