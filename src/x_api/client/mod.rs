//! X API の client (#241 で 1 ファイルから 4 つに分けた)｡
//!
//! - ここ (`mod.rs`): [`XClient`] と transport — `Authorization` の出どころ､
//!   retry と rate limit のループ､生の送信 1 回｡
//! - `endpoints.rs`: エンドポイントごとの `pub(crate)` メソッド｡
//! - `urls.rs`: URL のビルダーと､それが要求するフィールドの定数｡
//! - `status.rs`: レスポンスのステータスをエラーへ変える側 — [`Denied`]､
//!   429 の記録､retry してよいかの判定｡
//!
//! 子モジュールは親のプライベート項目に届くので､`get`/`post`/`delete` と
//! `send_*_once` はここに private のまま置き､`endpoints.rs` から呼ぶ｡

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use ureq::Agent;

use super::model::{Draft, TweetIdRequest, UserIdRequest};
use crate::oauth;
use crate::paths::Paths;
use crate::rate_limit::{self, Endpoint, RateLimitState};
use crate::usage;

mod endpoints;
mod status;
mod urls;

pub(crate) use status::{Denial, Denied};
use status::{check_status, is_retryable_status, log_429};

const API_BASE: &str = "https://api.x.com/2";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// user-context の OAuth セッションで届く X API 向けの blocking client｡
///
/// 呼び出しはどれもアカウントの API クレジットから課金されるので､UI は明示的な
/// ユーザー操作のときだけ取得する (初回ロードとリロードボタン)｡
#[derive(Clone)]
pub(crate) struct XClient {
    agent: Agent,
    bearer: Bearer,
}

/// リクエストごとに `Authorization` ヘッダへ載せる token の出どころ (#239)｡
#[derive(Clone)]
enum Bearer {
    /// 期限が来たら自分を更新するセッション｡本番の経路はすべてこれだ:
    /// [`XClient`] は auto-refresh のポーリングと list sync へ clone されて
    /// 渡り､どちらもウィンドウの一生ぶん生きる｡token の文字列を持たせると
    /// 起動時の姿のまま凍りつき､X の access token が切れる 2 時間後に
    /// すべての取得が 401 になる (#239)｡`Arc` を共有するので､clone が
    /// いくつあっても更新するのは 1 度だけだ｡
    Renewing(Arc<oauth::Session>),
    /// 固定の token｡テストだけが使う — ここを通る `XClient` は
    /// どのみち何も送らないので､`oauth::Session` とそれが要る `Paths` を
    /// 組み立てさせる意味が無い｡
    #[cfg(test)]
    Static(String),
}

impl XClient {
    /// 期限が来たら自分を更新するセッションを持つ client (#239)｡
    /// [`Bearer::Renewing`] を見よ｡
    pub(crate) fn renewing(session: Arc<oauth::Session>) -> Self {
        Self::with_bearer(Bearer::Renewing(session))
    }

    /// 固定の token を持つ client｡[`Bearer::Static`] を見よ｡
    #[cfg(test)]
    pub(crate) fn new(bearer_token: String) -> Self {
        Self::with_bearer(Bearer::Static(bearer_token))
    }

    fn with_bearer(bearer: Bearer) -> Self {
        let config = Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
            // 失敗が素のステータスコードではなく API 自身の説明を伴うよう､
            // ボディは自分で読む｡
            .http_status_as_error(false)
            .build();
        Self {
            agent: config.into(),
            bearer,
        }
    }

    /// このリクエストに載せる `Authorization` ヘッダの中身｡
    /// [`Bearer::Renewing`] ではここが token を更新しうる継ぎ目で､
    /// 送信 1 回ごとに (retry のたびにも) 通る — 更新の判断は
    /// [`oauth::Session::bearer`] にあり､まだ新しければネットワークへは
    /// 出ない｡
    fn authorization(&self, now: i64) -> Result<String> {
        let token = match &self.bearer {
            Bearer::Renewing(session) => session.bearer(now)?,
            #[cfg(test)]
            Bearer::Static(token) => token.clone(),
        };
        Ok(format!("Bearer {token}"))
    }

    /// GET を 1 回行う｡まず #10 の中心的な規則を守る — `endpoint` の追跡中の
    /// ウィンドウが remaining ゼロを報告し､その reset 時刻がまだ来ていない
    /// あいだは決して送らない ([`rate_limit::decision`] を見よ) — ネットワーク
    /// エラーと 5xx は backoff を挟んで retry し､返る前にレスポンスの
    /// rate limit ヘッダが明かすものを永続化する｡どちらの種類の 429 もここでは
    /// retry しない: 普通の rate limit は自分のスケジュールで回復する
    /// (retry したところで早まらない) し､usage cap の 429 はそもそも回復しない｡
    ///
    /// #18: 実際に HTTP を送るたび [`Self::send_once`] の直前に
    /// [`usage::record_request`] で数える — retry も含めてだ｡X は結果に関わらず
    /// 受け取った送信ごとに課金する (5xx でもサーバーには届いている) ので､
    /// retry したリクエストは論理的な呼び出し 1 回ごとではなく試行 1 回ごとに
    /// 数える｡上の [`rate_limit::decision`] が送る前に拒んだリクエストは､
    /// 何も出ていないので数えない｡クレート全体で `usage.rs` へ書くのはここ
    /// だけであり､意図してそうしてある: このメソッドを通らずにリクエストを
    /// 使うことはできない｡
    ///
    /// 直接の unit test は無い — `cache::reload` がそうでないのと同じく､
    /// ネットワークと､`paths` 経由でファイルシステムに触れるからだ｡この
    /// 振る舞いのテストカバレッジを実際に担っている純粋な継ぎ目は
    /// `rate_limit::decision`､`rate_limit::backoff_delay`､
    /// `rate_limit::parse_headers`､`rate_limit::classify_429` (下の
    /// [`check_status`] 経由)､そして `usage::record` だ｡
    fn get(&self, paths: &Paths, endpoint: Endpoint, url: &str, now: i64) -> Result<String> {
        Self::send_with_retry(paths, endpoint, now, || self.send_once(url, now))
    }

    /// `POST /2/tweets` を 1 回行う (#14､`quote_tweet_id` は #16 で追加)｡
    /// [`Self::get`] がすでに従う rate limit と retry の規則をすべて共有する
    /// — [`Self::send_with_retry`] を見よ｡HTTP メソッドに関わらず #10 の
    /// 中心的な規則がちょうど 1 箇所に留まるよう､両者はこれを共有している｡
    fn post(
        &self,
        paths: &Paths,
        endpoint: Endpoint,
        url: &str,
        draft: Draft<'_>,
        now: i64,
    ) -> Result<String> {
        Self::send_with_retry(paths, endpoint, now, || {
            self.send_post_once(url, draft, now)
        })
    }

    /// DELETE を 1 回行う (#15 の repost 取り消し)｡[`Self::get`] と
    /// [`Self::post`] が [`Self::send_with_retry`] 経由ですでに従う
    /// rate limit と retry の規則をすべて共有する — #10 の中心的な規則は HTTP
    /// メソッドに関わらず同じように適用され､repost の取り消しのためだけに
    /// 並行する retry ループを書くのではなく､DELETE もここでそれを得る｡
    fn delete(&self, paths: &Paths, endpoint: Endpoint, url: &str, now: i64) -> Result<String> {
        Self::send_with_retry(paths, endpoint, now, || self.send_delete_once(url, now))
    }

    /// [`Self::get`] と [`Self::post`] が共有する retry と永続化のループ:
    /// 何かを送る前に #10 の rate limit の判断を守り､ネットワークエラーや 5xx
    /// は backoff を挟んで retry する (どちらの種類の 429 も retry しない —
    /// [`is_retryable_status`] の doc を見よ)｡成功したかどうかに関わらず､
    /// 試行のたびに追跡中のウィンドウの姿を永続化する｡
    ///
    /// `get`/`post` と同じ理由で直接の unit test は無い — ネットワークと､
    /// `paths` 経由でファイルシステムに触れるからだ｡この振る舞いのテスト
    /// カバレッジを実際に担っている純粋な継ぎ目は `rate_limit::decision`､
    /// `rate_limit::backoff_delay`､`rate_limit::parse_headers`､そして
    /// `rate_limit::classify_429` ([`check_status`] 経由) だ｡
    fn send_with_retry(
        paths: &Paths,
        endpoint: Endpoint,
        now: i64,
        send_once: impl Fn() -> Result<(u16, String, RateLimitState)>,
    ) -> Result<String> {
        let tracked = rate_limit::load(paths, endpoint)?;
        rate_limit::decision(tracked, now).map_err(anyhow::Error::from)?;

        let mut attempt = 0u32;
        loop {
            match send_once() {
                Ok((status, body, state)) => {
                    // 送信前ではなくここで数える｡X が実際にリクエストを処理
                    // した (つまり課金した) 証拠は､レスポンスが返ってくること
                    // しかないからだ｡先に数えると､届かなかった接続まで
                    // ユーザーに請求することになる — 不安定なネットワークでは
                    // `MAX_RETRIES` 回まで retry するので､1 回のリロードが
                    // 実在しない 5 件のリクエストをでっち上げかねない｡retry
                    // したリクエストは再び課金されるので､呼び出し単位ではなく
                    // 送信単位で数える｡残る不正確さは逆の場合だ: X が処理した
                    // のにレスポンスが途中で失われたリクエストは､課金されるが
                    // ここでは数えられない｡
                    usage::record_request(paths, endpoint, now)?;

                    // 2xx でないレスポンスでも永続化する — 使い切った
                    // ウィンドウ自身の 429 こそ､*次の* 呼び出しがそもそも送信を
                    // 拒むために #10 が追跡したい情報だ｡
                    rate_limit::save(paths, endpoint, state)?;

                    if is_retryable_status(status) && attempt < rate_limit::MAX_RETRIES {
                        attempt = attempt.saturating_add(1);
                        std::thread::sleep(rate_limit::backoff_delay(
                            attempt,
                            rate_limit::random_jitter_fraction(),
                        ));
                        continue;
                    }

                    // `state.reset_at` を直接は使わない: ウィンドウを使い切って
                    // いないのに返ってきた 429 は､X がここで見せていない別の
                    // limit に拒まれたものなので､その reset ヘッダは誤った
                    // バケツのものだ｡`rate_limit::Refusal` を見よ｡
                    let refusal = rate_limit::Refusal::classify(state, now);
                    if status == 429 {
                        log_429(endpoint, state, refusal, &body);
                    }
                    check_status(endpoint, status, &body, refusal, now)?;
                    return Ok(body);
                }
                Err(error) => {
                    if attempt < rate_limit::MAX_RETRIES {
                        attempt = attempt.saturating_add(1);
                        std::thread::sleep(rate_limit::backoff_delay(
                            attempt,
                            rate_limit::random_jitter_fraction(),
                        ));
                        continue;
                    }
                    return Err(error);
                }
            }
        }
    }

    /// 生の HTTP GET 1 回: リクエストを送り､ボディを読み､返ってきた
    /// `x-rate-limit-*` ヘッダを [`rate_limit::parse_headers`] でパースする｡
    /// [`Self::get`] の retry ループが二度以上呼べるよう､そこから切り出した｡
    fn send_once(&self, url: &str, now: i64) -> Result<(u16, String, RateLimitState)> {
        let mut response = self
            .agent
            .get(url)
            .header("Authorization", self.authorization(now)?)
            .call()
            .with_context(|| format!("request to {url} failed"))?;

        // 自由関数ではなくクロージャにして､ureq のレスポンス型を名指さずに
        // `response` を借りる: 下の最後の呼び出しが返った時点で借用が終わり､
        // 続く `body_mut()` の呼び出しのために `response` が解放される｡
        let header = |name: &str| -> Option<String> {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        };
        let state = rate_limit::parse_headers(
            header("x-rate-limit-limit").as_deref(),
            header("x-rate-limit-remaining").as_deref(),
            header("x-rate-limit-reset").as_deref(),
        );

        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .context("could not read the response body")?;
        Ok((status, body, state))
    }

    /// `POST /2/tweets` のための生の HTTP POST 1 回 (#14､`quote_tweet_id` は
    /// #16 で追加)｡[`Self::send_with_retry`] が両者を同じに扱えるよう
    /// [`Self::send_once`] の形をなぞる｡`send_json` (`ureq` の `json` feature｡
    /// すでに依存にある) が [`super::model::PostTweetRequest`] を serialize し､
    /// `Content-Type: application/json` も設定する｡
    fn send_post_once(
        &self,
        url: &str,
        draft: Draft<'_>,
        now: i64,
    ) -> Result<(u16, String, RateLimitState)> {
        let mut response = self
            .agent
            .post(url)
            .header("Authorization", self.authorization(now)?)
            .send_json(draft.to_request())
            .with_context(|| format!("request to {url} failed"))?;

        let header = |name: &str| -> Option<String> {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        };
        let state = rate_limit::parse_headers(
            header("x-rate-limit-limit").as_deref(),
            header("x-rate-limit-remaining").as_deref(),
            header("x-rate-limit-reset").as_deref(),
        );

        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .context("could not read the response body")?;
        Ok((status, body, state))
    }

    /// 生の HTTP DELETE 1 回 (#15)｡[`Self::send_with_retry`] が同じに扱えるよう
    /// [`Self::send_once`] の形をそのままなぞる — リクエストボディは無く､
    /// [`Self::send_post_once`]/[`Self::send_tweet_id_once`] とはそこが違う｡
    fn send_delete_once(&self, url: &str, now: i64) -> Result<(u16, String, RateLimitState)> {
        let mut response = self
            .agent
            .delete(url)
            .header("Authorization", self.authorization(now)?)
            .call()
            .with_context(|| format!("request to {url} failed"))?;

        let header = |name: &str| -> Option<String> {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        };
        let state = rate_limit::parse_headers(
            header("x-rate-limit-limit").as_deref(),
            header("x-rate-limit-remaining").as_deref(),
            header("x-rate-limit-reset").as_deref(),
        );

        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .context("could not read the response body")?;
        Ok((status, body, state))
    }

    /// ボディが `tweet_id` 1 つだけの 2 つのエンドポイント —
    /// `POST /2/users/:id/retweets` (#15) と `POST /2/users/:id/likes` (#68)
    /// — のための生の HTTP POST 1 回｡[`Self::send_post_once`] の形をなぞるが､
    /// [`super::model::PostTweetRequest`] ではなく [`TweetIdRequest`] を
    /// serialize する｡ボディの型でパラメータ化せず `send_post_once` と別に
    /// してある: ここの重複は数行であって､本当に共有が要る retry と rate limit
    /// のロジックではない (そちらは [`Self::send_with_retry`] にあり､3 つとも
    /// 同じように使っている)｡
    fn send_tweet_id_once(
        &self,
        url: &str,
        tweet_id: &str,
        now: i64,
    ) -> Result<(u16, String, RateLimitState)> {
        let mut response = self
            .agent
            .post(url)
            .header("Authorization", self.authorization(now)?)
            .send_json(TweetIdRequest { tweet_id })
            .with_context(|| format!("request to {url} failed"))?;

        let header = |name: &str| -> Option<String> {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        };
        let state = rate_limit::parse_headers(
            header("x-rate-limit-limit").as_deref(),
            header("x-rate-limit-remaining").as_deref(),
            header("x-rate-limit-reset").as_deref(),
        );

        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .context("could not read the response body")?;
        Ok((status, body, state))
    }

    /// ボディが `user_id` 1 つだけの唯一のエンドポイント —
    /// `POST /2/lists/:id/members` (#163) — のための生の HTTP POST 1 回｡
    /// [`Self::send_tweet_id_once`] の兄弟であり､理由はあれが
    /// [`Self::send_post_once`] の兄弟であるのと同じだ: 共有したいのは retry と
    /// rate limit のロジックで､それはすでに [`Self::send_with_retry`] にあり､
    /// 4 つともそこを通る｡
    fn send_user_id_once(
        &self,
        url: &str,
        user_id: &str,
        now: i64,
    ) -> Result<(u16, String, RateLimitState)> {
        let mut response = self
            .agent
            .post(url)
            .header("Authorization", self.authorization(now)?)
            .send_json(UserIdRequest { user_id })
            .with_context(|| format!("request to {url} failed"))?;

        let header = |name: &str| -> Option<String> {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        };
        let state = rate_limit::parse_headers(
            header("x-rate-limit-limit").as_deref(),
            header("x-rate-limit-remaining").as_deref(),
            header("x-rate-limit-reset").as_deref(),
        );

        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .context("could not read the response body")?;
        Ok((status, body, state))
    }
}
