//! ウィンドウが開いている間ずっと生き、期限が来たら自分で更新する OAuth
//! セッション (#239)。
//!
//! #239 より前、`ui::tasks::start` は起動時に一度だけ credential を解決し、
//! その access token の文字列を `XClient` に焼き付けていた。auto-refresh の
//! ポーリングも list sync もその client を clone して抱え込むので、X の
//! access token が期限を迎える 2 時間後には、ウィンドウは再起動するまで
//! 401 を出し続けた。
//!
//! ここが直し方だ。token の文字列ではなく、この `Session` を共有する。
//! `Mutex` が refresh を直列化するので、`ui::list_sync` が
//! 「`resolve_credential` を 2 か所から呼ぶと refresh token の回転で保存
//! セッションが死ぬ」と警戒していた競合は構造的に起きない — 更新する側が
//! 1 つしかない。
//!
//! [`Session::bearer`] は refresh を引数で受ける形に割ってあるので、判断
//! (更新するか、持っているものを返すか、諦めるか) はネットワーク無しで
//! テストできる。`config.rs` と `tokens.rs` の `now` 注入と同じ継ぎ目だ。

use std::sync::{Arc, Mutex};

use anyhow::Result;

use super::tokens::{self, TokenResponse, TokenSet};
use crate::paths::Paths;

/// 保存された OAuth セッションの、生きている姿。
pub(crate) struct Session {
    client_id: String,
    paths: Paths,
    /// 直列化の要。`bearer` はこれを握ったまま refresh し、保存し、
    /// 差し替える。同時に走る 2 つのポーリングが同じ refresh token を
    /// 二重に使うことはない。
    state: Mutex<TokenSet>,
}

/// `access_token` と `refresh_token` は `Debug` にも出さない。ログは
/// `log::redact` を通るが、通らない経路 (panic の payload など) がある。
impl std::fmt::Debug for Session {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Session")
            .field("client_id", &self.client_id)
            .field("expires_at", &self.expires_at())
            .field("scope", &self.scope())
            .finish_non_exhaustive()
    }
}

impl Session {
    pub(crate) fn new(client_id: String, paths: Paths, stored: TokenSet) -> Arc<Self> {
        Arc::new(Self {
            client_id,
            paths,
            state: Mutex::new(stored),
        })
    }

    /// X がこのセッションに与えた scope (#14)。composer と like ボタンは
    /// 送信の前にこれを見る。
    pub(crate) fn scope(&self) -> Option<String> {
        self.locked().scope.clone()
    }

    fn expires_at(&self) -> i64 {
        self.locked().expires_at
    }

    /// このリクエストに使う access token。期限の手前なら、返す前に更新する。
    pub(crate) fn bearer(&self, now: i64) -> Result<String> {
        self.bearer_with(now, |client_id, refresh_token| {
            super::refresh_access_token(client_id, refresh_token)
        })
    }

    /// [`Self::bearer`] のうち、ネットワークに触れない側。`refresh` は
    /// 期限が来たときにだけ、`Mutex` を握ったまま 1 度だけ呼ばれる。
    fn bearer_with(
        &self,
        now: i64,
        refresh: impl FnOnce(&str, &str) -> Result<TokenResponse>,
    ) -> Result<String> {
        let mut state = self.locked();
        if !state.needs_refresh(now) {
            return Ok(state.access_token.clone());
        }

        let Some(refresh_token) = state.refresh_token.clone() else {
            return Err(super::SessionExpired {
                detail: "the stored session carries no refresh token".to_string(),
            }
            .into());
        };

        // X が答えたうえで断ったのなら、このセッションは死んでいる — 繰り返し
        // 取得している側 (auto-refresh のポーリング) がそれと分かる型で言う。
        // 届かなかっただけの失敗は素のまま通す。そちらは次の tick には直って
        // いておかしくないので、ポーリングを止める理由にならない。
        let response = refresh(&self.client_id, &refresh_token).map_err(|error| {
            if error
                .downcast_ref::<super::TokenRequestRejected>()
                .is_some()
            {
                anyhow::Error::from(super::SessionExpired {
                    detail: format!("{error:#}"),
                })
            } else {
                error
            }
        })?;
        let mut refreshed = TokenSet::from_response(response, now);
        // `resolve_credential` の refresh 分岐と同じ扱い: token エンドポイント
        // は変わらない scope を省いてよい (RFC 6749 §5.1)。
        refreshed.scope = super::carried_scope(refreshed.scope, state.scope.as_deref());
        // 保存が先。ここで落ちたら、次の起動が使う refresh token は X が
        // たった今回して無効にしたほうになる — 保存より先に手元を差し替えると、
        // その食い違いに気づく手立てが無くなる。
        tokens::save(&self.paths, &refreshed)?;
        let token = refreshed.access_token.clone();
        *state = refreshed;
        Ok(token)
    }

    /// poisoned な `Mutex` でも中身を返す。ここが守っているのは
    /// 「refresh を 1 度に 1 つだけ」であって不変条件ではないので、panic した
    /// スレッドが残した `TokenSet` はそのまま使える — 使えないと、サインイン
    /// しているのに何も取得できないウィンドウになる。
    fn locked(&self) -> std::sync::MutexGuard<'_, TokenSet> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_paths(root: &std::path::Path) -> Paths {
        let home = root.display().to_string();
        Paths::from_vars(move |key| (key == "HOME").then(|| home.clone())).unwrap()
    }

    fn temp_root(label: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "twigpui-test-oauth-session-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    fn stored(expires_at: i64) -> TokenSet {
        TokenSet {
            access_token: "old-access".to_string(),
            refresh_token: Some("old-refresh".to_string()),
            expires_at,
            scope: Some("tweet.read users.read".to_string()),
        }
    }

    fn renewed() -> TokenResponse {
        TokenResponse {
            access_token: "new-access".to_string(),
            refresh_token: Some("new-refresh".to_string()),
            expires_in: 7200,
            scope: None,
        }
    }

    #[test]
    fn hands_back_the_stored_token_without_refreshing_while_it_is_fresh() {
        let root = temp_root("fresh");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let session = Session::new("client".to_string(), paths, stored(10_000));
        let token = session
            .bearer_with(1_000, |_, _| panic!("must not refresh a fresh token"))
            .unwrap();

        assert_eq!(token, "old-access");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn refreshes_and_persists_once_the_token_is_within_the_skew_window() {
        // #239 の中心。起動から 2 時間後、ポーリングはこの経路を通って
        // 生きた token を得なければならない — 起動時に凍らせたものではなく。
        let root = temp_root("refresh");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let session = Session::new("client".to_string(), paths.clone(), stored(10_000));
        let token = session
            .bearer_with(10_000, |client_id, refresh_token| {
                assert_eq!(client_id, "client");
                assert_eq!(refresh_token, "old-refresh");
                Ok(renewed())
            })
            .unwrap();

        assert_eq!(token, "new-access");
        let saved = tokens::load(&paths).unwrap().unwrap();
        assert_eq!(saved.access_token, "new-access");
        assert_eq!(saved.refresh_token.as_deref(), Some("new-refresh"));
        assert_eq!(saved.expires_at, 17_200);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn refreshes_only_once_for_the_next_two_hours() {
        let root = temp_root("once");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let session = Session::new("client".to_string(), paths, stored(10_000));
        session.bearer_with(10_000, |_, _| Ok(renewed())).unwrap();
        let token = session
            .bearer_with(11_000, |_, _| panic!("the refreshed token is still fresh"))
            .unwrap();

        assert_eq!(token, "new-access");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn keeps_the_scope_the_refresh_response_left_out() {
        // RFC 6749 §5.1 は変わらない scope の省略を許す。`carried_scope` が
        // 無いと、日常的な refresh のたびに composer と like ボタンが黙って
        // 使えなくなる。
        let root = temp_root("scope");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let session = Session::new("client".to_string(), paths, stored(10_000));
        session.bearer_with(10_000, |_, _| Ok(renewed())).unwrap();

        assert_eq!(session.scope().as_deref(), Some("tweet.read users.read"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn reports_an_expired_session_when_x_turns_the_refresh_down() {
        // #239 の再来を防ぐ側。X が refresh token を失効させたら、ここは
        // ポーリングが「待っても直らない」と読める型で言わなければならない。
        // 素の `TokenRequestRejected` のまま通すと、`halting_reason` が
        // 拾えず、3 分ごとの取得が止まらないまま朝を迎える。
        let root = temp_root("rejected");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let session = Session::new("client".to_string(), paths, stored(10_000));
        let error = session
            .bearer_with(10_000, |_, _| {
                Err(super::super::TokenRequestRejected {
                    status: 400,
                    detail: "invalid_request: Value passed for the token was invalid.".to_string(),
                }
                .into())
            })
            .unwrap_err();

        let expired = error
            .downcast_ref::<super::super::SessionExpired>()
            .expect("a refusal from X is an expired session, not a blip");
        assert!(expired.detail.contains("invalid_request"), "{expired:?}");

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn leaves_an_unreachable_token_endpoint_as_an_ordinary_failure() {
        // 裏側。届かなかっただけの失敗まで `SessionExpired` にすると、
        // 瞬断ひとつでポーリングが永久に止まり、再サインインを促す嘘の
        // バナーが出る。
        let root = temp_root("unreachable");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let session = Session::new("client".to_string(), paths, stored(10_000));
        let error = session
            .bearer_with(10_000, |_, _| Err(anyhow::anyhow!("token request failed")))
            .unwrap_err();

        assert!(
            error
                .downcast_ref::<super::super::SessionExpired>()
                .is_none()
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn reports_an_expired_session_when_there_is_nothing_to_refresh_with() {
        let root = temp_root("no-refresh-token");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let mut without = stored(10_000);
        without.refresh_token = None;
        let session = Session::new("client".to_string(), paths, without);
        let error = session
            .bearer_with(10_000, |_, _| panic!("there is nothing to refresh with"))
            .unwrap_err();

        assert!(
            error
                .downcast_ref::<super::super::SessionExpired>()
                .is_some()
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
