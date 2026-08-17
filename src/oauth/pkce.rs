//! PKCE verifier/challenge generation (RFC 7636) and authorize-URL building.
//!
//! Every function here is pure over its inputs — including randomness, which
//! arrives through an injected [`RandomSource`] the same way [`crate::config`]
//! and [`crate::paths`] inject a variable lookup. Tests supply a fixed byte
//! sequence; [`OsRandom`] is the only implementation used outside tests.

use std::fmt::Write as _;

use anyhow::{Context as _, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest as _, Sha256};

/// Scopes requested at authorize time. `tweet.write` was added by #14 — #7
/// deliberately left it out until posting needed it. Already-signed-in
/// users from before #14 hold a token without it, which is exactly what
/// `oauth::tokens::has_scope` plus the header's "Re-authorize" button (#14)
/// exist to detect and fix.
const SCOPES: &str = "tweet.read users.read offline.access";

/// `https://x.com/i/oauth2/authorize` per the issue's confirmed design.
const AUTHORIZE_URL: &str = "https://x.com/i/oauth2/authorize";

/// Bytes of entropy behind the code verifier and the CSRF `state` value. 32
/// random bytes base64url-encode to 43 characters — the minimum length RFC
/// 7636 §4.1 allows for a code verifier, and enough for an unguessable state.
const RANDOM_BYTES: usize = 32;

/// Source of cryptographically random bytes, injected so PKCE generation can
/// be tested with a fixed sequence instead of the OS RNG — the same seam
/// [`crate::config::Config::resolve`] uses for environment lookups.
pub(crate) trait RandomSource {
    fn fill(&self, buf: &mut [u8]) -> Result<()>;
}

/// The real source, backed by the OS CSPRNG via `getrandom`.
pub(crate) struct OsRandom;

impl RandomSource for OsRandom {
    fn fill(&self, buf: &mut [u8]) -> Result<()> {
        getrandom::fill(buf).context("could not read random bytes from the OS")
    }
}

/// Generate a PKCE code verifier: 32 random bytes, base64url-encoded without
/// padding (RFC 7636 §4.1).
pub(crate) fn generate_code_verifier(random: &impl RandomSource) -> Result<String> {
    let mut bytes = [0u8; RANDOM_BYTES];
    random.fill(&mut bytes)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// Derive the S256 code challenge from a verifier (RFC 7636 §4.2):
/// `BASE64URL-ENCODE(SHA256(ASCII(code_verifier)))`.
pub(crate) fn code_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

/// Generate the CSRF `state` parameter. Random bytes are enough here too —
/// there's no RFC-mandated shape the way there is for the code verifier.
pub(crate) fn generate_state(random: &impl RandomSource) -> Result<String> {
    let mut bytes = [0u8; RANDOM_BYTES];
    random.fill(&mut bytes)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// Reject a `state` echoed back by the callback that doesn't match what was
/// sent — the CSRF check RFC 6749 §10.12 requires of the client.
pub(crate) fn verify_state(expected: &str, received: &str) -> Result<()> {
    if expected == received {
        Ok(())
    } else {
        bail!("state mismatch — possible CSRF, aborting sign-in")
    }
}

/// Percent-encode one query-parameter value per RFC 3986's `unreserved` set.
/// `client_id`, `redirect_uri`, and `scope` all need this when they're
/// spliced into the authorize URL by hand — unlike the token request body,
/// which `ureq::send_form` encodes for us.
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(char::from(byte));
            }
            _ => {
                // `write!` to a `String` is infallible.
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// Build the `https://x.com/i/oauth2/authorize` URL for the interactive
/// sign-in step.
pub(crate) fn build_authorize_url(
    client_id: &str,
    redirect_uri: &str,
    code_challenge: &str,
    state: &str,
) -> String {
    format!(
        "{AUTHORIZE_URL}?response_type=code&client_id={client_id}&redirect_uri={redirect_uri}\
         &scope={scope}&state={state}&code_challenge={code_challenge}&code_challenge_method=S256",
        client_id = percent_encode(client_id),
        redirect_uri = percent_encode(redirect_uri),
        scope = percent_encode(SCOPES),
        state = percent_encode(state),
        code_challenge = percent_encode(code_challenge),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `RandomSource` that always returns a fixed byte sequence, so PKCE
    /// generation is deterministic in tests.
    struct FixedRandom<'a>(&'a [u8]);

    impl RandomSource for FixedRandom<'_> {
        fn fill(&self, buf: &mut [u8]) -> Result<()> {
            if buf.len() != self.0.len() {
                bail!("fixed random source length mismatch");
            }
            buf.copy_from_slice(self.0);
            Ok(())
        }
    }

    #[test]
    fn code_challenge_matches_the_rfc_7636_test_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            code_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn generate_code_verifier_is_url_safe_and_unpadded() {
        let bytes = [7u8; RANDOM_BYTES];
        let random = FixedRandom(&bytes);
        let verifier = generate_code_verifier(&random).unwrap();

        assert!(!verifier.contains('='), "must not be padded: {verifier}");
        assert!(
            verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "must be url-safe: {verifier}"
        );
        assert!(
            (43..=128).contains(&verifier.len()),
            "RFC 7636 length: {}",
            verifier.len()
        );
    }

    #[test]
    fn generate_code_verifier_is_deterministic_given_the_same_bytes() {
        let bytes = [1u8; RANDOM_BYTES];
        let a = generate_code_verifier(&FixedRandom(&bytes)).unwrap();
        let b = generate_code_verifier(&FixedRandom(&bytes)).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn generate_state_differs_from_a_differently_seeded_verifier() {
        let state = generate_state(&FixedRandom(&[2u8; RANDOM_BYTES])).unwrap();
        let verifier = generate_code_verifier(&FixedRandom(&[3u8; RANDOM_BYTES])).unwrap();
        assert_ne!(state, verifier);
    }

    #[test]
    fn verify_state_accepts_a_match() {
        assert!(verify_state("abc", "abc").is_ok());
    }

    #[test]
    fn verify_state_rejects_a_mismatch_as_csrf() {
        let error = verify_state("abc", "xyz").unwrap_err().to_string();
        assert!(error.to_lowercase().contains("csrf"), "{error}");
    }

    #[test]
    fn percent_encode_leaves_unreserved_characters_alone() {
        assert_eq!(percent_encode("abcXYZ019-._~"), "abcXYZ019-._~");
    }

    #[test]
    fn percent_encode_escapes_everything_else() {
        assert_eq!(percent_encode("a b/c"), "a%20b%2Fc");
    }

    #[test]
    fn build_authorize_url_includes_every_required_parameter() {
        let url = build_authorize_url(
            "client-123",
            "http://127.0.0.1:8733/callback",
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            "state-abc",
        );
        assert_eq!(
            url,
            "https://x.com/i/oauth2/authorize?response_type=code&client_id=client-123\
             &redirect_uri=http%3A%2F%2F127.0.0.1%3A8733%2Fcallback\
             &scope=tweet.read%20users.read%20tweet.write%20offline.access&state=state-abc\
             &code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM\
             &code_challenge_method=S256"
        );
    }

    #[test]
    fn scopes_include_what_14s_composer_needs() {
        // #14: posting requires `tweet.write`, added on top of #7's
        // originally-minimal request — a substring check would false-match
        // e.g. a hypothetical `tweet.write.extra`, so this splits first.
        assert!(
            SCOPES
                .split_whitespace()
                .any(|scope| scope == "tweet.write")
        );
    }
}
