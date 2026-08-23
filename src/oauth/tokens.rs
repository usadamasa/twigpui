//! OAuth token model, persistence, and expiry.
//!
//! Mirrors `config.rs`'s injected-`now` seam: [`TokenSet::from_response`] and
//! [`TokenSet::needs_refresh`] never read the real clock, so expiry logic is
//! testable without sleeping or mocking `SystemTime`.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::paths::Paths;

/// Treat a token as due for refresh this many seconds before it actually
/// expires, so a request already in flight doesn't get handed a token that
/// dies mid-request.
const REFRESH_SKEW_SECONDS: i64 = 60;

/// The JSON body X's token endpoint returns from both the authorization-code
/// exchange and a refresh (RFC 6749 §5.1).
#[derive(Debug, Deserialize)]
pub(crate) struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    pub expires_in: u64,
    /// The scope X actually granted (#14) — space-separated, RFC 6749
    /// §5.1. `#[serde(default)]` since the token endpoint may omit it when
    /// unchanged from the request (see `oauth::carried_scope` for how a
    /// refresh response's omission is handled) and, more generally, to keep
    /// this struct tolerant of a token endpoint that omits it altogether.
    #[serde(default)]
    pub scope: Option<String>,
}

/// The `error` JSON body the token endpoint returns on failure (RFC 6749
/// §5.2) — distinct from `x_api::model::ApiProblem`, which describes the
/// read API's own problem-details shape.
#[derive(Debug, Default, Deserialize)]
struct TokenErrorResponse {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// Best-effort human-readable description of a token-endpoint error body.
pub(crate) fn describe_token_error(body: &str) -> Option<String> {
    let problem: TokenErrorResponse = serde_json::from_str(body).ok()?;
    let error = problem.error?;
    Some(match problem.error_description {
        Some(description) => format!("{error}: {description}"),
        None => error,
    })
}

/// A persisted OAuth session: the access token used verbatim in the
/// `Authorization: Bearer` header, an optional refresh token, and an
/// absolute expiry.
///
/// `expires_at` is stored absolute (not the relative `expires_in` the token
/// endpoint returns) so a restart can tell freshness without remembering
/// when the token was issued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TokenSet {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    pub expires_at: i64,
    /// The scope granted alongside this token (#14), space-separated per
    /// RFC 6749 §3.3. `#[serde(default)]` here matches every other
    /// `Option<T>` field in this struct/crate (see `refresh_token` above),
    /// even though `serde_derive` already treats a struct field of type
    /// `Option<T>` as implicitly optional — missing key deserializes to
    /// `None` — with or without the attribute; verified directly by
    /// `parses_a_pre_14_token_set_without_a_scope_field` below, which pastes
    /// an old-format `TokenSet` literal with no `scope` key at all and
    /// still parses. What the issue actually warns about — a new field
    /// silently making `tokens::load` fail and logging every already-
    /// signed-in user out — is real for a non-`Option` field (`access_token`,
    /// `expires_at` above would both break the same way), just not the
    /// specific shape this field happens to take. The attribute stays for
    /// the same reason `refresh_token`'s does: it says "missing is a valid,
    /// expected state" explicitly, rather than relying on a serde default
    /// a future refactor (e.g. switching this to a non-`Option` type) could
    /// silently stop providing.
    #[serde(default)]
    pub scope: Option<String>,
}

impl TokenSet {
    /// Build a `TokenSet` from a fresh token response, resolving
    /// `expires_in` against `now`.
    pub(crate) fn from_response(response: TokenResponse, now: i64) -> Self {
        let expires_in = i64::try_from(response.expires_in).unwrap_or(i64::MAX);
        Self {
            access_token: response.access_token,
            refresh_token: response.refresh_token,
            expires_at: now.saturating_add(expires_in),
            scope: response.scope,
        }
    }

    /// Whether this token should be refreshed before use: already expired,
    /// or inside the skew window.
    pub(crate) fn needs_refresh(&self, now: i64) -> bool {
        // `saturating_add` (#47): `now` comes from the clock, so a
        // machine with an absurd date set must make this say "refresh",
        // not overflow.
        now.saturating_add(REFRESH_SKEW_SECONDS) >= self.expires_at
    }
}

/// The scope #14's composer needs (`POST /2/tweets`), requested at
/// authorize time by `oauth::pkce`'s `SCOPES` constant and checked here
/// before letting a submit go out.
pub(crate) const TWEET_WRITE_SCOPE: &str = "tweet.write";

/// The scope #68's like button needs (`POST`/`DELETE /2/users/:id/likes`).
/// X grants this separately from `tweet.write`, so a session authorized
/// before #68 can post and repost but not like — [`has_scope`] plus the
/// header's "Re-authorize" button is the fix, exactly as for #14.
pub(crate) const LIKE_WRITE_SCOPE: &str = "like.write";

/// The scope #161's List timeline needs (`GET /2/lists/:id/tweets`), added
/// to `SCOPES` by #167. Unlike the two above this one gates a *read*, and
/// only when a list is configured — see `ui::render::offers_reauthorize`.
/// A session authorized before #167 has everything the app needs except
/// this, so configuring a list on an old session is the one way to reach a
/// 403 the app can actually explain.
pub(crate) const LIST_READ_SCOPE: &str = "list.read";

/// Whether a granted scope string includes `required`, per RFC 6749 §3.3's
/// space-separated list — matched by exact token, not a substring check, so
/// e.g. a hypothetical `tweet.write.extra` scope wouldn't false-match a
/// check for `tweet.write`. `granted: None` (an unrecorded/unknown scope —
/// see [`TokenSet::scope`]'s doc) is always insufficient: the conservative
/// choice, since a pre-#14 token might or might not actually carry
/// `tweet.write`, and the safe assumption is "prompt before writing" rather
/// than "assume it's fine".
pub(crate) fn has_scope(granted: Option<&str>, required: &str) -> bool {
    granted.is_some_and(|scopes| scopes.split_whitespace().any(|scope| scope == required))
}

/// Write `tokens` to [`Paths::oauth_token_file`], `0600` (owner read/write
/// only) — the same private-file discipline `paths::create_private_dir` uses
/// for the directories above it.
pub(crate) fn save(paths: &Paths, tokens: &TokenSet) -> Result<()> {
    let path = paths.oauth_token_file();
    let json = serde_json::to_vec_pretty(tokens).context("could not serialize the OAuth tokens")?;

    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("could not open {} for writing", path.display()))?;
    file.write_all(&json)
        .with_context(|| format!("could not write {}", path.display()))
}

/// Load the persisted tokens, or `None` if there is no session yet — a
/// missing file is not an error, mirroring `config::FileSettings::load`.
pub(crate) fn load(paths: &Paths) -> Result<Option<TokenSet>> {
    let path = paths.oauth_token_file();
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("could not read {}", path.display()));
        }
    };
    let tokens = serde_json::from_str(&contents)
        .with_context(|| format!("could not parse {}", path.display()))?;
    Ok(Some(tokens))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN_RESPONSE_JSON: &str = r#"{
        "token_type": "bearer",
        "expires_in": 7200,
        "access_token": "access-abc",
        "scope": "tweet.read users.read offline.access",
        "refresh_token": "refresh-xyz"
    }"#;

    #[test]
    fn parses_a_token_response() {
        let response: TokenResponse = serde_json::from_str(TOKEN_RESPONSE_JSON).unwrap();
        assert_eq!(response.access_token, "access-abc");
        assert_eq!(response.refresh_token.as_deref(), Some("refresh-xyz"));
        assert_eq!(response.expires_in, 7200);
        assert_eq!(
            response.scope.as_deref(),
            Some("tweet.read users.read offline.access")
        );
    }

    #[test]
    fn from_response_computes_an_absolute_expiry() {
        let response: TokenResponse = serde_json::from_str(TOKEN_RESPONSE_JSON).unwrap();
        let tokens = TokenSet::from_response(response, 1_000);
        assert_eq!(tokens.expires_at, 1_000 + 7200);
        assert_eq!(tokens.access_token, "access-abc");
        assert_eq!(
            tokens.scope.as_deref(),
            Some("tweet.read users.read offline.access")
        );
    }

    #[test]
    fn needs_refresh_is_false_well_before_expiry() {
        let tokens = TokenSet {
            access_token: "a".into(),
            refresh_token: None,
            expires_at: 10_000,
            scope: None,
        };
        assert!(!tokens.needs_refresh(0));
    }

    #[test]
    fn needs_refresh_is_true_inside_the_skew_window() {
        let tokens = TokenSet {
            access_token: "a".into(),
            refresh_token: None,
            expires_at: 10_000,
            scope: None,
        };
        assert!(tokens.needs_refresh(10_000 - REFRESH_SKEW_SECONDS));
    }

    #[test]
    fn needs_refresh_is_true_after_expiry() {
        let tokens = TokenSet {
            access_token: "a".into(),
            refresh_token: None,
            expires_at: 10_000,
            scope: None,
        };
        assert!(tokens.needs_refresh(20_000));
    }

    // --- has_scope ---

    #[test]
    fn has_scope_distinguishes_like_write_from_tweet_write() {
        // #68: a pre-#68 session holds `tweet.write` but not `like.write`,
        // and must be told to re-authorize rather than spending a request
        // that is guaranteed to 403.
        let pre_68 = Some("tweet.read users.read tweet.write offline.access");
        assert!(has_scope(pre_68, TWEET_WRITE_SCOPE));
        assert!(!has_scope(pre_68, LIKE_WRITE_SCOPE));
    }

    #[test]
    fn has_scope_is_true_when_the_required_scope_is_present() {
        assert!(has_scope(Some("tweet.read tweet.write"), TWEET_WRITE_SCOPE));
    }

    #[test]
    fn has_scope_is_false_when_the_required_scope_is_missing() {
        assert!(!has_scope(
            Some("tweet.read users.read offline.access"),
            TWEET_WRITE_SCOPE
        ));
    }

    #[test]
    fn has_scope_is_false_for_an_unrecorded_unknown_scope() {
        // #14: a pre-#14 token has no recorded scope at all — treated as
        // insufficient, never as "assume it's fine".
        assert!(!has_scope(None, TWEET_WRITE_SCOPE));
    }

    #[test]
    fn has_scope_does_not_substring_match() {
        assert!(!has_scope(Some("tweet.write.extra"), TWEET_WRITE_SCOPE));
    }

    #[test]
    fn parses_a_pre_14_token_set_without_a_scope_field() {
        // #14: a token file written before this field existed — must
        // deserialize cleanly (see `TokenSet::scope`'s doc for why a
        // missing `#[serde(default)]` would silently log the user out)
        // rather than failing `tokens::load` and dropping the whole
        // session. Deliberately a raw literal rather than trusting
        // `#[serde(default)]` at a glance, mirroring the convention already
        // used for `x_api::model`'s own pre-#13/#12 cache-compat tests.
        let old_format = r#"{
            "access_token": "access-abc",
            "refresh_token": "refresh-xyz",
            "expires_at": 1700000000
        }"#;
        let tokens: TokenSet = serde_json::from_str(old_format).unwrap();
        assert_eq!(tokens.access_token, "access-abc");
        assert_eq!(tokens.refresh_token.as_deref(), Some("refresh-xyz"));
        assert_eq!(tokens.scope, None);
    }

    #[test]
    fn describes_a_token_error_body() {
        let message =
            describe_token_error(r#"{"error":"invalid_grant","error_description":"code expired"}"#)
                .unwrap();
        assert_eq!(message, "invalid_grant: code expired");
    }

    #[test]
    fn describes_a_token_error_without_a_description() {
        let message = describe_token_error(r#"{"error":"invalid_client"}"#).unwrap();
        assert_eq!(message, "invalid_client");
    }

    #[test]
    fn returns_none_for_a_body_with_no_error_field() {
        assert!(describe_token_error("{}").is_none());
    }

    fn test_paths(root: &std::path::Path) -> Paths {
        let home = root.display().to_string();
        Paths::from_vars(move |key| (key == "HOME").then(|| home.clone())).unwrap()
    }

    fn temp_root(label: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "twigpui-test-oauth-tokens-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn saves_and_loads_the_same_tokens() {
        let root = temp_root("roundtrip");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let tokens = TokenSet {
            access_token: "access".to_string(),
            refresh_token: Some("refresh".to_string()),
            expires_at: 123_456,
            scope: Some("tweet.read tweet.write".to_string()),
        };
        save(&paths, &tokens).unwrap();
        let loaded = load(&paths).unwrap();
        assert_eq!(loaded, Some(tokens));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn save_writes_the_token_file_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = temp_root("perms");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        save(
            &paths,
            &TokenSet {
                access_token: "a".into(),
                refresh_token: None,
                expires_at: 1,
                scope: None,
            },
        )
        .unwrap();

        let mode = std::fs::metadata(paths.oauth_token_file())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn load_returns_none_when_the_file_is_missing() {
        let root = temp_root("missing");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        assert!(load(&paths).unwrap().is_none());

        std::fs::remove_dir_all(&root).unwrap();
    }
}
