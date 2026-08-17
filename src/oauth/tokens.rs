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
    let _ = body;
    None
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
}

impl TokenSet {
    /// Build a `TokenSet` from a fresh token response, resolving
    /// `expires_in` against `now`.
    pub(crate) fn from_response(response: TokenResponse, now: i64) -> Self {
        let _ = (response, now);
        Self {
            access_token: String::new(),
            refresh_token: None,
            expires_at: 0,
        }
    }

    /// Whether this token should be refreshed before use: already expired,
    /// or inside the skew window.
    pub(crate) fn needs_refresh(&self, now: i64) -> bool {
        let _ = now;
        false
    }
}

/// Write `tokens` to [`Paths::oauth_token_file`], `0600` (owner read/write
/// only) — the same private-file discipline `paths::create_private_dir` uses
/// for the directories above it.
pub(crate) fn save(paths: &Paths, tokens: &TokenSet) -> Result<()> {
    let path = paths.oauth_token_file();
    let json =
        serde_json::to_vec_pretty(tokens).context("could not serialize the OAuth tokens")?;

    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("could not open {} for writing", path.display()))?;
    let _ = json;
    file.write_all(b"")
        .with_context(|| format!("could not write {}", path.display()))
}

/// Load the persisted tokens, or `None` if there is no session yet — a
/// missing file is not an error, mirroring `config::FileSettings::load`.
pub(crate) fn load(paths: &Paths) -> Result<Option<TokenSet>> {
    let _ = paths;
    Ok(None)
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
    }

    #[test]
    fn from_response_computes_an_absolute_expiry() {
        let response: TokenResponse = serde_json::from_str(TOKEN_RESPONSE_JSON).unwrap();
        let tokens = TokenSet::from_response(response, 1_000);
        assert_eq!(tokens.expires_at, 1_000 + 7200);
        assert_eq!(tokens.access_token, "access-abc");
    }

    #[test]
    fn needs_refresh_is_false_well_before_expiry() {
        let tokens = TokenSet {
            access_token: "a".into(),
            refresh_token: None,
            expires_at: 10_000,
        };
        assert!(!tokens.needs_refresh(0));
    }

    #[test]
    fn needs_refresh_is_true_inside_the_skew_window() {
        let tokens = TokenSet {
            access_token: "a".into(),
            refresh_token: None,
            expires_at: 10_000,
        };
        assert!(tokens.needs_refresh(10_000 - REFRESH_SKEW_SECONDS));
    }

    #[test]
    fn needs_refresh_is_true_after_expiry() {
        let tokens = TokenSet {
            access_token: "a".into(),
            refresh_token: None,
            expires_at: 10_000,
        };
        assert!(tokens.needs_refresh(20_000));
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
        let root =
            std::env::temp_dir().join(format!("twigpui-test-oauth-tokens-{label}-{}", std::process::id()));
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
