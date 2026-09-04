//! OAuth 2.0 Authorization Code + PKCE (#7)｡
//!
//! 3 つの継ぎ目を束ねる: [`pkce`] が verifier/challenge と state を生成し､
//! [`callback`] がリダイレクトを受けるループバックリスナを動かし､
//! [`tokens`] が返ってきたものを永続化する｡[`sign_in`] は対話フローを
//! 走らせるために `ui.rs` が呼ぶ唯一の入口で､[`resolve_credential`] は
//! `ui.rs` (起動時) と `--fetch-only` の両方が､ブラウザを開かずに使える
//! credential を見つけるのに使う｡

mod callback;
mod pkce;
mod session;
pub(crate) mod tokens;

pub(crate) use session::Session;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use gpui::BackgroundExecutor;
use ureq::Agent;

use crate::config::Config;
use crate::paths::Paths;
use crate::profile::Profile;
use tokens::{TokenResponse, TokenSet};

/// `https://api.x.com/2/oauth2/token`｡issue で確定した設計に従う｡
const TOKEN_URL: &str = "https://api.x.com/2/oauth2/token";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// 現在時刻を Unix タイムスタンプで — このモジュールで実際に時計を読む
/// 唯一の箇所｡以下の関数はどれも代わりに `now` を引数で受け取る｡
/// `config.rs` が環境変数の参照に使うのと同じ継ぎ目だ｡
pub(crate) fn unix_now() -> i64 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    i64::try_from(secs).unwrap_or(i64::MAX)
}

fn agent() -> Agent {
    // `x_api::client::XClient::new` の設定に倣う: 2xx でないステータスでも
    // body を自分で読み､token エンドポイント自身のエラーテキストをメッセージ
    // に載せる｡待ち時間はこのアプリの他のリクエストと同じく上限を設ける｡
    let config = Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .http_status_as_error(false)
        .build();
    config.into()
}

/// 対話的なサインインフローを端から端まで走らせる: PKCE のペアと state を
/// 作り､authorize URL をシステムのブラウザで開き､ループバックのリダイレクト
/// を待ち､code を token に交換する｡
///
/// `executor` の上で走るので､ループバックリスナのポーリングループが accept
/// の試行の合間に譲れる — `callback::await_authorization_code` を参照｡直接の
/// ユニットテストは無い: 実際にブラウザを開き実際のソケットを bind する｡
pub(crate) async fn sign_in(executor: &BackgroundExecutor, client_id: &str) -> Result<TokenSet> {
    let random = pkce::OsRandom;
    let verifier = pkce::generate_code_verifier(&random)?;
    let challenge = pkce::code_challenge(&verifier);
    let state = pkce::generate_state(&random)?;
    // #169: port は､ひいては redirect URI は､このバイナリがどのインストール
    // かに属する — development ビルドが本番アプリの redirect URI を送っては
    // ならないし､本番が listen する port を bind してもならない｡
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

/// token エンドポイントが答えたうえで断った (RFC 6749 §5.2)｡リクエストが
/// 届かなかった場合と型で区別する (#239): 届かなかったのなら retry で直り
/// うるが､断られたのなら何度送っても同じ答えが返る｡
#[derive(Debug)]
pub(crate) struct TokenRequestRejected {
    pub(crate) status: u16,
    pub(crate) detail: String,
}

impl std::fmt::Display for TokenRequestRejected {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "token request failed with HTTP {}: {}",
            self.status, self.detail
        )
    }
}

impl std::error::Error for TokenRequestRejected {}

/// 保存された OAuth セッションがもう使えない (#239): refresh するための
/// refresh token が無いか､X が refresh を断った｡待っても直らないので､
/// 繰り返し取得している側 — auto-refresh のポーリング — はこれを見たら
/// やめて､人にサインインし直すよう言う｡
#[derive(Debug)]
pub(crate) struct SessionExpired {
    pub(crate) detail: String,
}

impl std::fmt::Display for SessionExpired {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "the stored X session expired: {}", self.detail)
    }
}

impl std::error::Error for SessionExpired {}

/// token エンドポイントへのリクエストを 1 つ POST する｡これは
/// **public client** だ (#7 で確定した設計): `client_id` は grant と並んで
/// body に載り､HTTP Basic auth としては決して送らない｡対になる client
/// secret が無いからだ｡
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
        return Err(TokenRequestRejected { status, detail }.into());
    }

    serde_json::from_str(&body).context("could not parse the token response")
}

/// 保存された (あるいは今 refresh した) OAuth セッション由来の user-context
/// access token と､X がそれに与えた scope (#14)｡
///
/// #33 まで enum だったものを struct にした: もう一方の variant は app-only
/// bearer token だけで､呼び出し側がこれに投げていた問い ("これは OAuth か?"
/// "scope は何か?") はどれも､そうでないかもしれないという事実を回避する
/// ために存在していた｡今 credential は 1 種類しかない｡
///
/// `scope: None` は未記録・不明を意味する — #14 以前の token か､token
/// エンドポイントのレスポンスに `scope` がまったく無かったもの — で､
/// `tokens::has_scope` はこれを scope で守られた操作には不十分として扱う｡
///
/// #239 で access token の文字列が [`Session`] に替わった｡文字列は解決した
/// 瞬間の姿でしかなく､ウィンドウはそれを `XClient` へ焼き付けたまま何時間
/// も走る — X の access token は 2 時間で切れるので､起動から 2 時間後に
/// すべての取得が 401 になっていた｡`Session` は同じ問いに今の答えを返す｡
#[derive(Debug, Clone)]
pub(crate) struct Credential {
    pub(crate) session: Arc<Session>,
    pub(crate) scope: Option<String>,
}

/// 保存された OAuth セッションをそのまま使えず､[`resolve_credential`] が
/// 代わりに bearer token (あるいは何も無い状態) へ落ちるしかなかった理由
/// (#54)｡`ui.rs` はこれを画面に出して直し方を示す｡そうしないとアプリは何かが
/// 変わった印を他に何も出さないからだ: timeline は (bearer token が設定
/// されていればそれで) 依然として描画されるので､post する能力を黙って失うのは
/// "everything is fine" と見分けがつかない｡
///
/// 新しいセッションや refresh に成功したセッションは理由をまったく持たない
/// — [`Resolution::demotion`] を参照 — その場合は何も劣化していないからだ｡
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionDemotion {
    /// 保存されたセッションは refresh が必要だったが､refresh するための
    /// refresh token を持っていない — `offline.access` 以前のセッションか､
    /// X が単に refresh token を発行しなかったセッション｡
    NoRefreshToken,
    /// refresh を試みた (client id と refresh token が両方あった) が X が
    /// 拒否した｡`detail` は token エンドポイント自身のエラーテキストを運ぶ
    /// — 失効した token も回復不能なほど期限切れの token も､ここでは同じ
    /// 汎用の 400 として現れるので､手元でこれ以上細かく分類する材料は無い｡
    Rejected(String),
}

/// [`resolve_credential`] が見つけたもの: 使う credential があればそれと､
/// #54 以降は､途中で保存された OAuth セッションを降格させる必要があったか
/// どうかと､その理由｡保存されているが refresh できないセッションは
/// "credential が一度も設定されていない" とは実質的に別の状態だ: どちらも
/// credential 無しに解決しうるが､ユーザーがついさっきまでサインインしていて
/// 今は黙ってそうでなくなっている､という意味なのは一方だけだ｡
#[derive(Debug, Clone)]
pub(crate) struct Resolution {
    pub(crate) credential: Option<Credential>,
    pub(crate) demotion: Option<SessionDemotion>,
}

/// ブラウザを開かずに使える credential を見つける: 保存された新しい OAuth
/// セッションか､古くなったものをその場で refresh したもの｡
/// `credential: None` は使えるセッションが無いという意味で､呼び出し側
/// (`--fetch-only` か起動時の `ui.rs`) はユーザーにサインインを促すべきだ｡
///
/// #33 以前はもう 1 つ結果があった — app-only bearer token へのフォール
/// バックだ — `demotion` はそれを説明するために存在していた: timeline は
/// 依然として描画されるので､post する能力を黙って失うのは何も問題ない状態と
/// 見分けがつかなかった｡bearer token が消えても `demotion` が残るのは､同じ
/// 説明が依然として要るからだ｡ただし今は目に見える帰結が "静かに能力が
/// 落ちる" ではなく "サインアウト" になった｡
pub(crate) fn resolve_credential(config: &Config, paths: &Paths, now: i64) -> Result<Resolution> {
    let Some(stored) = tokens::load(paths)? else {
        return Ok(Resolution {
            credential: None,
            demotion: None,
        });
    };

    if stored.needs_refresh(now) && stored.refresh_token.is_none() {
        return Ok(Resolution {
            credential: None,
            demotion: Some(SessionDemotion::NoRefreshToken),
        });
    }

    // #239: refresh を自分で回すのではなく [`Session`] に任せる｡起動時に
    // 一度 `bearer` を呼ぶのは､使えないセッションを *最初の取得より前* に
    // バナーで説明するためだ (#54)｡token がまだ新しければネットワークには
    // 出ず､保存されたものをそのまま返す｡
    let session = Session::new(config.oauth_client_id.clone(), paths.clone(), stored);
    match session.bearer(now) {
        Ok(_) => Ok(Resolution {
            credential: Some(Credential {
                scope: session.scope(),
                session,
            }),
            demotion: None,
        }),
        Err(error) => {
            // X が refresh をきっぱり拒否した — 失効か､回復不能なほどの期限
            // 切れだ｡#54: 上の場合とまったく同じように credential 無しへ
            // 降格させ､ハードエラーとして伝播させない｡伝播させると､*read* の
            // 経路には実際には無いセッションの問題のせいで､timeline がすでに
            // 出していたものを空にしてしまう (このモジュールの doc と issue の
            // "do not break the bearer fallback" という要求を参照)｡
            Ok(Resolution {
                credential: None,
                demotion: Some(SessionDemotion::Rejected(format!("{error:#}"))),
            })
        }
    }
}

/// 保存された OAuth セッションをそのまま使えなかった理由を説明する (#54)｡
/// `ui.rs` の画面上のバナーにも `--fetch-only` の stderr にも使う｡
///
/// 残る 2 つの場合はどちらも同じ直し方 — もう一度サインインする — なので､
/// どちらのメッセージもそう言う｡`NoClientId` は 3 つ目の場合だったが､#33 が
/// 起動時に client id を必須にした: それ無しに起動できないアプリが､後から
/// それが無いと気づくことはありえない｡
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

/// refresh をまたいでどの scope を残すか (#14): token エンドポイントが送って
/// きたなら新しく返ってきた scope を､でなければすでに記録されていたものを｡
/// `resolve_credential` の全経路を通してだけでなく純粋関数として直接テスト
/// している｡実際の refresh 分岐を動かすには､このクレートのテストが決して
/// 行わない実際の HTTP 呼び出しが要るからだ (モジュールの doc を参照)｡
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
            post_resource_price: 0.005,
            daily_post_budget: 1000,
            list_id: None,
            auto_sync_list: false,
            sync_interval_seconds: 21_600,
            sync_prune_limit_percent: 10,
            sync_writes_per_batch: 2,
            auto_refresh: false,
            auto_refresh_interval_seconds: 300,
            follow_new_posts: false,
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
        // RFC 6749 §5.1: 変わっていなければ token エンドポイントは refresh
        // 時の `scope` を省いてよい — これが動いている `tweet.write`
        // セッションを黙って "unknown" へ格下げしてはならない｡
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
        // #33: フォールバックの credential はもう無いので､更新できない
        // セッションはアプリをサインアウト状態にする — そして理由を言う｡
        assert!(resolution.credential.is_none());
        assert_eq!(resolution.demotion, Some(SessionDemotion::NoRefreshToken));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn resolve_credential_reports_why_even_though_there_is_nothing_to_fall_back_to() {
        // `credential: None` と `demotion: Some(_)` は独立した事実で､
        // `ui.rs` には両方が要る: 前者はアプリが何かを読めるかを決め､後者は
        // "session expired" のバナーを出すかを決める｡
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
        assert!(resolution.credential.is_none());
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
