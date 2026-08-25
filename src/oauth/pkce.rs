//! PKCE の verifier/challenge 生成 (RFC 7636) と authorize URL の組み立て｡
//!
//! ここの関数はどれも入力に対して純粋だ — 乱数も含めて｡乱数は注入された
//! [`RandomSource`] を通って届く｡[`crate::config`] や [`crate::paths`] が
//! 変数の参照を注入するのと同じやり方だ｡テストは固定のバイト列を渡す｡
//! テスト以外で使う実装は [`OsRandom`] だけだ｡

use anyhow::{Context as _, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest as _, Sha256};

use crate::url::Url;

/// authorize 時に要求する scope｡`tweet.write` は #14 が､`like.write` は #68
/// が､`list.read` は #161 が足した — #7 は post・like・List の読み取りが
/// それらを必要とするまで意図して外していた｡各追加より前からサインインして
/// いる人はそれを持たない token を握っており､まさにそれを検知して直すために
/// `oauth::tokens::has_scope` とヘッダーの "Re-authorize" ボタン (#14) が
/// ある｡
///
/// `list.write` は要求し **ない**｡List を作って中身を入れるのは #163 だ｡
/// アプリがまだ使えない書き込み権限を求めるのは､何のためでもない権限を
/// 与えられることになる｡
const SCOPES: &str = "tweet.read users.read tweet.write like.write list.read list.write follows.read \
     offline.access";

/// `https://x.com/i/oauth2/authorize`｡issue で確定した設計に従う｡
const AUTHORIZE_URL: &str = "https://x.com/i/oauth2/authorize";

/// code verifier と CSRF の `state` 値を支えるエントロピーのバイト数｡32
/// バイトの乱数は base64url で 43 文字になる — RFC 7636 §4.1 が code verifier
/// に許す最小の長さであり､推測できない state には十分だ｡
const RANDOM_BYTES: usize = 32;

/// 暗号論的乱数バイトの供給源｡PKCE の生成を OS の RNG ではなく固定の列で
/// テストできるよう注入する — [`crate::config::Config::resolve`] が環境変数の
/// 参照に使うのと同じ継ぎ目だ｡
pub(crate) trait RandomSource {
    fn fill(&self, buf: &mut [u8]) -> Result<()>;
}

/// 実際の供給源｡`getrandom` 経由で OS の CSPRNG を使う｡
pub(crate) struct OsRandom;

impl RandomSource for OsRandom {
    fn fill(&self, buf: &mut [u8]) -> Result<()> {
        getrandom::fill(buf).context("could not read random bytes from the OS")
    }
}

/// PKCE の code verifier を生成する: 32 バイトの乱数を､パディング無しの
/// base64url でエンコードしたもの (RFC 7636 §4.1)｡
pub(crate) fn generate_code_verifier(random: &impl RandomSource) -> Result<String> {
    let mut bytes = [0u8; RANDOM_BYTES];
    random.fill(&mut bytes)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// verifier から S256 の code challenge を導く (RFC 7636 §4.2):
/// `BASE64URL-ENCODE(SHA256(ASCII(code_verifier)))`｡
pub(crate) fn code_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

/// CSRF の `state` パラメータを生成する｡ここも乱数バイトで十分だ —
/// code verifier と違い RFC が定める形は無い｡
pub(crate) fn generate_state(random: &impl RandomSource) -> Result<String> {
    let mut bytes = [0u8; RANDOM_BYTES];
    random.fill(&mut bytes)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// callback が返してきた `state` が送ったものと一致しなければ拒否する —
/// RFC 6749 §10.12 がクライアントに要求する CSRF の検査だ｡
pub(crate) fn verify_state(expected: &str, received: &str) -> Result<()> {
    if expected == received {
        Ok(())
    } else {
        bail!("state mismatch — possible CSRF, aborting sign-in")
    }
}

/// 対話的なサインイン手順のための `https://x.com/i/oauth2/authorize` URL を
/// 組み立てる｡
///
/// [`Url::api`] ではなく [`Url::form`] を使う (#165): OAuth 2.0 §3.1 は
/// `application/x-www-form-urlencoded` のパラメータを求めるので､RFC 3986 の
/// `unreserved` 集合の外は何も残らない — scope の間の空白は `%20` として､
/// `redirect_uri` の `:` と `/` はエスケープされて送られる｡X API 自身の
/// クエリ値はカンマを保つが､こちらは保たない｡`url` のモジュール doc を参照｡
///
/// #165 まではここに手書きの `percent_encode` と､値ごとにそれを 1 回ずつ
/// 呼ぶフォーマット文字列があった｡すべての値がその呼び出しを受けたことは
/// 何も検査していなかった｡#161 がその代償を見せた: scope を 1 つ足すのに
/// テスト内の URL リテラルを手で編集することになった｡
pub(crate) fn build_authorize_url(
    client_id: &str,
    redirect_uri: &str,
    code_challenge: &str,
    state: &str,
) -> String {
    Url::form(AUTHORIZE_URL)
        .param("response_type", "code")
        .param("client_id", client_id)
        .param("redirect_uri", redirect_uri)
        .param("scope", SCOPES)
        .param("state", state)
        .param("code_challenge", code_challenge)
        .param("code_challenge_method", "S256")
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 常に固定のバイト列を返す `RandomSource`｡テストで PKCE の生成を
    /// 決定的にするためのものだ｡
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

    // ここにあった 2 つの `percent_encode` のテストは､関数そのものと一緒に
    // `url` へ移した (#165)｡assert は 1 つずつそのままだ:
    // `both_policies_leave_the_unreserved_set_alone` と
    // `both_policies_escape_a_space_and_a_slash` は同じ入力を同じ期待値と
    // 突き合わせる｡ただし今は､このファイルが持っていた 1 つの方針ではなく
    // 両方の方針について検査する｡

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
             &scope=tweet.read%20users.read%20tweet.write%20like.write%20list.read%20list.write\
             %20follows.read%20offline.access\
             &state=state-abc\
             &code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM\
             &code_challenge_method=S256"
        );
    }

    #[test]
    fn scopes_include_what_68s_like_button_needs() {
        // #68: like には `like.write` が要る｡部分文字列ではなく split して
        // 照合するのは､下の検査と同じ理由だ｡
        assert!(
            SCOPES.split_whitespace().any(|scope| scope == "like.write"),
            "SCOPES must request like.write"
        );
    }

    #[test]
    fn scopes_include_what_163s_list_sync_needs() {
        // #163 はこのアプリがフォローしているアカウントを List へ写す｡
        // これには両側に 1 つずつ scope が要る: `GET /2/users/:id/following`
        // を読むには `follows.read` が､メンバーの追加や削除には
        // `list.write` が要る｡`list.read` (#161) は読み取りしか覆わない｡
        for required in ["follows.read", "list.write"] {
            assert!(
                SCOPES.split_whitespace().any(|scope| scope == required),
                "SCOPES must request {required}"
            );
        }
    }

    #[test]
    fn scopes_include_what_161s_list_timeline_needs() {
        // #161: `GET /2/lists/:id/tweets` には `list.read` が要る｡部分文字列
        // ではなく split して照合するのは､周りの検査と同じ理由だ｡
        assert!(
            SCOPES.split_whitespace().any(|scope| scope == "list.read"),
            "SCOPES must request list.read"
        );
    }

    #[test]
    fn scopes_leave_out_access_nothing_uses_yet() {
        // これが固定する規則は #7 のものだ: アプリが使えないものを決して
        // 求めない｡`list.write` と `follows.read` は #163 が呼び出し元を
        // 与えたときにこのリストを離れた｡この 3 つにはまだ呼び出し元が
        // 無いので､要求すれば同意画面に､どのコードも到達しない権限を
        // 並べることになる｡
        for unused in ["bookmark.read", "like.read", "mute.read"] {
            assert!(
                !SCOPES.split_whitespace().any(|scope| scope == unused),
                "SCOPES must not request {unused} until something needs it"
            );
        }
    }

    #[test]
    fn scopes_include_what_14s_composer_needs() {
        // #14: post には `tweet.write` が要る｡#7 の元々最小だった要求の上に
        // 足した — 部分文字列の検査では､たとえば仮の `tweet.write.extra` に
        // 誤って一致してしまうので､先に split する｡
        assert!(
            SCOPES
                .split_whitespace()
                .any(|scope| scope == "tweet.write")
        );
    }
}
