//! OAuth token のモデル､永続化､有効期限｡
//!
//! `config.rs` の `now` 注入の継ぎ目に倣う: [`TokenSet::from_response`] と
//! [`TokenSet::needs_refresh`] は実際の時計を決して読まないので､期限のロジック
//! は sleep も `SystemTime` のモックも無しにテストできる｡

use std::fs::OpenOptions;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::paths::Paths;

/// token を実際の期限のこの秒数だけ手前で refresh 対象として扱う｡すでに
/// 飛んでいるリクエストが､途中で死ぬ token を渡されないようにするためだ｡
const REFRESH_SKEW_SECONDS: i64 = 60;

/// X の token エンドポイントが authorization-code の交換からも refresh からも
/// 返す JSON の body (RFC 6749 §5.1)｡
#[derive(Debug, Deserialize)]
pub(crate) struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    pub expires_in: u64,
    /// X が実際に与えた scope (#14) — 空白区切り､RFC 6749 §5.1｡
    /// `#[serde(default)]` なのは､リクエストから変わらない場合に token
    /// エンドポイントがこれを省きうるから (refresh レスポンスの省略をどう
    /// 扱うかは `oauth::carried_scope` を参照)､そしてより一般には､これを
    /// 丸ごと省く token エンドポイントにも耐えるようにするためだ｡
    #[serde(default)]
    pub scope: Option<String>,
}

/// 失敗時に token エンドポイントが返す `error` の JSON body (RFC 6749 §5.2)
/// — read API 自身の problem-details の形を表す `x_api::model::ApiProblem`
/// とは別物だ｡
#[derive(Debug, Default, Deserialize)]
struct TokenErrorResponse {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// token エンドポイントのエラー body を人が読める形にする best-effort な説明｡
pub(crate) fn describe_token_error(body: &str) -> Option<String> {
    let problem: TokenErrorResponse = serde_json::from_str(body).ok()?;
    let error = problem.error?;
    Some(match problem.error_description {
        Some(description) => format!("{error}: {description}"),
        None => error,
    })
}

/// 永続化された OAuth セッション: `Authorization: Bearer` ヘッダーにそのまま
/// 使う access token､省略可能な refresh token､そして絶対時刻の期限｡
///
/// `expires_at` は絶対時刻で保存する (token エンドポイントが返す相対の
/// `expires_in` ではない)｡そうすれば再起動後も､token がいつ発行されたかを
/// 覚えていなくても新しさが分かる｡
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TokenSet {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    pub expires_at: i64,
    /// この token と一緒に与えられた scope (#14)｡RFC 6749 §3.3 に従い空白
    /// 区切り｡ここの `#[serde(default)]` は､この struct とクレートの他の
    /// `Option<T>` フィールドすべてに揃えたものだ (上の `refresh_token` を
    /// 参照)｡`serde_derive` はすでに `Option<T>` 型の struct フィールドを
    /// 暗黙に省略可能として扱う — キーが無ければ `None` にデシリアライズ
    /// される — 属性の有無によらず｡下の
    /// `parses_a_pre_14_token_set_without_a_scope_field` が直接それを検証
    /// している｡`scope` キーがまったく無い旧形式の `TokenSet` リテラルを
    /// 貼り付けても､なおパースできる｡issue が実際に警告しているもの —
    /// 新しいフィールドが黙って `tokens::load` を失敗させ､サインイン済みの
    /// ユーザー全員をログアウトさせること — は `Option` でないフィールドに
    /// とっては現実で (上の `access_token`, `expires_at` はどちらも同じよう
    /// に壊れる)､このフィールドがたまたま取っている形には当たらないだけだ｡
    /// 属性を残す理由は `refresh_token` のそれと同じだ: "欠けているのは妥当
    /// で想定された状態だ" と明示し､将来のリファクタ (たとえばこれを
    /// `Option` でない型へ変えること) が黙って提供しなくなりうる serde の
    /// 既定に頼らない｡
    #[serde(default)]
    pub scope: Option<String>,
}

impl TokenSet {
    /// 新しい token レスポンスから `TokenSet` を組み立てる｡`expires_in` は
    /// `now` を基準に解決する｡
    pub(crate) fn from_response(response: TokenResponse, now: i64) -> Self {
        let expires_in = i64::try_from(response.expires_in).unwrap_or(i64::MAX);
        Self {
            access_token: response.access_token,
            refresh_token: response.refresh_token,
            expires_at: now.saturating_add(expires_in),
            scope: response.scope,
        }
    }

    /// この token を使う前に refresh すべきかどうか: すでに期限切れか､
    /// skew の窓の中にいるか｡
    pub(crate) fn needs_refresh(&self, now: i64) -> bool {
        // `saturating_add` (#47): `now` は時計から来るので､でたらめな日付が
        // 設定された機械では､オーバーフローではなく "refresh しろ" と
        // 言わせなければならない｡
        now.saturating_add(REFRESH_SKEW_SECONDS) >= self.expires_at
    }
}

/// #14 の composer が要る scope (`POST /2/tweets`)｡authorize 時に
/// `oauth::pkce` の `SCOPES` 定数が要求し､submit を出す前にここで検査する｡
pub(crate) const TWEET_WRITE_SCOPE: &str = "tweet.write";

/// #68 の like ボタンが要る scope (`POST`/`DELETE /2/users/:id/likes`)｡
/// X はこれを `tweet.write` とは別に与えるので､#68 以前に認可されたセッション
/// は post も repost もできるが like はできない — 直し方は [`has_scope`] と
/// ヘッダーの "Re-authorize" ボタンで､#14 とまったく同じだ｡
pub(crate) const LIKE_WRITE_SCOPE: &str = "like.write";

/// #161 の List timeline が要る scope (`GET /2/lists/:id/tweets`)｡#167 が
/// `SCOPES` に足した｡上の 2 つと違いこれが守るのは *read* で､しかも list が
/// 設定されているときだけだ — `ui::render::offers_reauthorize` を参照｡
/// #167 以前に認可されたセッションはこれ以外すべて揃っているので､古い
/// セッションで list を設定するのが､アプリが実際に説明できる 403 に届く
/// 唯一の道だ｡
pub(crate) const LIST_READ_SCOPE: &str = "list.read";

/// #163 の sync が list を *変える* のに要る scope
/// (`POST`/`DELETE /2/lists/:id/members`)｡`like.write` が `tweet.read` と別に
/// 与えられるのと同じように､`list.read` とは別に与えられる｡
pub(crate) const LIST_WRITE_SCOPE: &str = "list.write";

/// #163 の sync が､このアプリがフォローしているアカウントを読むのに要る
/// scope (`GET /2/users/:id/following`)｡要求したのは #163 が初めてだ —
/// それ以前に実際の token がこれを持っていたのは､#157 の調査中の 1 回の
/// 再認可の残りで､再認可はそれを消した｡sync が仮定せずに検査するのは
/// まさにそのためだ｡
pub(crate) const FOLLOWS_READ_SCOPE: &str = "follows.read";

/// 与えられた scope 文字列が `required` を含むかどうか｡RFC 6749 §3.3 の空白
/// 区切りリストに従う — 部分文字列ではなくトークン単位の完全一致なので､
/// たとえば仮に `tweet.write.extra` という scope があっても `tweet.write` の
/// 検査に誤って一致しない｡`granted: None` (未記録・不明な scope —
/// [`TokenSet::scope`] の doc を参照) は常に不十分だ: #14 以前の token が実際
/// に `tweet.write` を持つかは分からず､安全な仮定は "書く前に尋ねる" で
/// あって "問題ないと決めてかかる" ではないので､保守的な方を選ぶ｡
pub(crate) fn has_scope(granted: Option<&str>, required: &str) -> bool {
    granted.is_some_and(|scopes| scopes.split_whitespace().any(|scope| scope == required))
}

/// `tokens` を [`Paths::oauth_token_file`] へ `0600` (所有者のみ読み書き) で
/// 書く — `paths::create_private_dir` が上位のディレクトリに使うのと同じ
/// 非公開ファイルの規律だ｡
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

/// 永続化された token を読む｡セッションがまだ無ければ `None` — ファイルが
/// 無いのはエラーではない｡`config::FileSettings::load` に倣っている｡
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
        // #68: #68 以前のセッションは `tweet.write` を持つが `like.write` は
        // 持たない｡403 が確実なリクエストを費やす代わりに､再認可するよう
        // 伝えなければならない｡
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
        // #14: #14 以前の token は scope をまったく記録していない — 不十分と
        // して扱い､"問題ない" と決めてかかることは決してしない｡
        assert!(!has_scope(None, TWEET_WRITE_SCOPE));
    }

    #[test]
    fn has_scope_does_not_substring_match() {
        assert!(!has_scope(Some("tweet.write.extra"), TWEET_WRITE_SCOPE));
    }

    #[test]
    fn parses_a_pre_14_token_set_without_a_scope_field() {
        // #14: このフィールドが存在する前に書かれた token ファイル —
        // `tokens::load` を失敗させてセッションを丸ごと捨てるのではなく､
        // きれいにデシリアライズできなければならない (`#[serde(default)]`
        // が無いとなぜ黙ってユーザーをログアウトさせるかは
        // `TokenSet::scope` の doc を参照)｡`#[serde(default)]` をひと目で
        // 信じるのではなく意図して生のリテラルにしてある｡`x_api::model` の
        // #13/#12 以前のキャッシュ互換テストがすでに使う慣習に倣う｡
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
