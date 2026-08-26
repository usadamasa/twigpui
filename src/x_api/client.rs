use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use ureq::Agent;

use super::model::{
    ApiProblem, Draft, ListPageResponse, ListSummary, TimelineItem, TimelineResponse,
    TweetIdRequest, User, UserIdRequest, UserLookupResponse, UserPageResponse,
};
use crate::paths::Paths;
use crate::rate_limit::{self, Endpoint, RateLimitState};
use crate::url::Url;
use crate::usage;

const API_BASE: &str = "https://api.x.com/2";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// app-only の Bearer token で届く read エンドポイント向けの blocking client｡
///
/// 呼び出しはどれもアカウントの API クレジットから課金されるので､UI は明示的な
/// ユーザー操作のときだけ取得する (初回ロードとリロードボタン)｡
#[derive(Clone)]
pub(crate) struct XClient {
    agent: Agent,
    bearer_token: String,
}

impl XClient {
    pub(crate) fn new(bearer_token: String) -> Self {
        let config = Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
            // 失敗が素のステータスコードではなく API 自身の説明を伴うよう､
            // ボディは自分で読む｡
            .http_status_as_error(false)
            .build();
        Self {
            agent: config.into(),
            bearer_token,
        }
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
        Self::send_with_retry(paths, endpoint, now, || self.send_once(url))
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
        Self::send_with_retry(paths, endpoint, now, || self.send_post_once(url, draft))
    }

    /// DELETE を 1 回行う (#15 の repost 取り消し)｡[`Self::get`] と
    /// [`Self::post`] が [`Self::send_with_retry`] 経由ですでに従う
    /// rate limit と retry の規則をすべて共有する — #10 の中心的な規則は HTTP
    /// メソッドに関わらず同じように適用され､repost の取り消しのためだけに
    /// 並行する retry ループを書くのではなく､DELETE もここでそれを得る｡
    fn delete(&self, paths: &Paths, endpoint: Endpoint, url: &str, now: i64) -> Result<String> {
        Self::send_with_retry(paths, endpoint, now, || self.send_delete_once(url))
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
    fn send_once(&self, url: &str) -> Result<(u16, String, RateLimitState)> {
        let mut response = self
            .agent
            .get(url)
            .header("Authorization", format!("Bearer {}", self.bearer_token))
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
    fn send_post_once(&self, url: &str, draft: Draft<'_>) -> Result<(u16, String, RateLimitState)> {
        let mut response = self
            .agent
            .post(url)
            .header("Authorization", format!("Bearer {}", self.bearer_token))
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
    fn send_delete_once(&self, url: &str) -> Result<(u16, String, RateLimitState)> {
        let mut response = self
            .agent
            .delete(url)
            .header("Authorization", format!("Bearer {}", self.bearer_token))
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
    ) -> Result<(u16, String, RateLimitState)> {
        let mut response = self
            .agent
            .post(url)
            .header("Authorization", format!("Bearer {}", self.bearer_token))
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
    fn send_user_id_once(&self, url: &str, user_id: &str) -> Result<(u16, String, RateLimitState)> {
        let mut response = self
            .agent
            .post(url)
            .header("Authorization", format!("Bearer {}", self.bearer_token))
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

    /// screen name を､timeline エンドポイントが要る数値の user id へ解決する｡
    pub(crate) fn user_id_by_username(
        &self,
        paths: &Paths,
        username: &str,
        now: i64,
    ) -> Result<String> {
        let url = user_lookup_url(username);
        let body = self.get(paths, Endpoint::UserLookup, &url, now)?;

        let response: UserLookupResponse =
            serde_json::from_str(&body).context("could not parse the user lookup response")?;
        match response.data {
            Some(user) => Ok(user.id),
            // `data` の無い 200 が､未知の screen name に対する X の報告の仕方だ｡
            None => match describe_problem(&body) {
                Some(message) => bail!("could not resolve @{username}: {message}"),
                None => bail!("could not resolve @{username}: the API returned no user"),
            },
        }
    }

    /// `user_id` の最近の post を直接取得する (呼び出し側はすでに screen name
    /// を解決している — `cache::reload` を見よ)｡新しい順だ｡`since_id` を
    /// 渡すと API はそれより新しい post だけを返し､差分リロードでレスポンスも
    /// クレジットの費用も抑えられる｡
    pub(crate) fn timeline(
        &self,
        paths: &Paths,
        user_id: &str,
        max_results: u32,
        since_id: Option<&str>,
        now: i64,
    ) -> Result<Vec<TimelineItem>> {
        let url = timeline_url(user_id, max_results, since_id);
        let body = self.get(paths, Endpoint::Timeline, &url, now)?;

        let response: TimelineResponse =
            serde_json::from_str(&body).context("could not parse the timeline response")?;
        Ok(response.into_items())
    }

    /// `GET /2/users/me` でサインイン中のユーザー自身の id と screen name を
    /// 解決する (#11)｡OAuth の user-context token が要る — app-only の bearer
    /// token はここで 401 になる｡home timeline 自体と同じだ｡
    pub(crate) fn me(&self, paths: &Paths, now: i64) -> Result<User> {
        let url = me_url();
        let body = self.get(paths, Endpoint::Me, &url, now)?;

        let response: UserLookupResponse =
            serde_json::from_str(&body).context("could not parse the /me response")?;
        match response.data {
            Some(user) => Ok(user),
            None => match describe_problem(&body) {
                Some(message) => bail!("could not resolve the signed-in user: {message}"),
                None => bail!("could not resolve the signed-in user: the API returned no user"),
            },
        }
    }

    /// サインイン中のユーザーの home timeline を 1 ページ取得する (#11)｡新しい
    /// 順で､#11 の "Load older" のための `meta.next_token` も一緒に返す｡
    /// `since_id` (差分リロード) と `pagination_token` (前の `next_token` から
    /// の再開) は実際上は排他だ — [`home_timeline_url`] を見よ — が､どちらが
    /// 要るかを決めるのは呼び出し側で､ここは両方をそのまま通すだけだ｡
    pub(crate) fn home_timeline(
        &self,
        paths: &Paths,
        user_id: &str,
        max_results: u32,
        since_id: Option<&str>,
        pagination_token: Option<&str>,
        now: i64,
    ) -> Result<(Vec<TimelineItem>, Option<String>)> {
        let url = home_timeline_url(user_id, max_results, since_id, pagination_token);
        let body = self.get(paths, Endpoint::HomeTimeline, &url, now)?;

        let response: TimelineResponse =
            serde_json::from_str(&body).context("could not parse the home timeline response")?;
        let next_token = response.next_token().map(str::to_string);
        Ok((response.into_items(), next_token))
    }

    /// List の timeline を 1 ページ取得する (#161)｡新しい順で､"Load older" の
    /// ための `meta.next_token` も一緒に返す — [`Self::home_timeline`] が返す
    /// のと同じ組なので､レスポンスが手に入れば呼び出し側は 2 つの source を
    /// 同じに扱える｡
    ///
    /// **ここには `since_id` が無い**｡このファイルの他の timeline 呼び出しとは
    /// 違う｡`GET /2/lists/:id/tweets` が文書化しているのは `max_results` と
    /// `pagination_token` だけで､差分リロードは表現できない: リロードのたびに
    /// 先頭ページを読み直す｡それは安全だ (`cache::timeline::splice` は id で
    /// マージするので行は重複しない) が､ただではない — `x-api-budget` を見よ:
    /// read は返ってきた resource ごとに課金され､同じ post は UTC の 1 日に
    /// つき 1 回の課金とはいえ､日付の境目をまたいだ最初のリロードは､中身が
    /// 新しいかどうかに関わらず先頭ページ全体をもう一度支払う｡
    pub(crate) fn list_timeline(
        &self,
        paths: &Paths,
        list_id: &str,
        max_results: u32,
        pagination_token: Option<&str>,
        now: i64,
    ) -> Result<(Vec<TimelineItem>, Option<String>)> {
        let url = list_timeline_url(list_id, max_results, pagination_token);
        let body = self.get(paths, Endpoint::ListTimeline, &url, now)?;

        let response: TimelineResponse =
            serde_json::from_str(&body).context("could not parse the list timeline response")?;
        let next_token = response.next_token().map(str::to_string);
        Ok((response.into_items(), next_token))
    }

    /// このアプリがフォローしているアカウントを 1 ページ取得し (#163)､次への
    /// カーソルも返す｡`user_id` はサインイン中のアカウント自身の id (`/me`)､
    /// `pagination_token` は前のページのカーソルで､最初のページでは `None` だ｡
    ///
    /// 全部ではなく 1 ページ: ループするのは呼び出し側なので､途中の失敗は
    /// 部分的な read の意味を決める側から見える｡`sync` はそれを致命的として
    /// 扱う — 部分的なフォロー一覧が決して diff に届いてはならない理由は
    /// [`crate::sync`] の doc を見よ｡
    ///
    /// `follows.read` が要る｡#163 が `SCOPES` に足したもので､それ以前に認可
    /// されたセッションはここで 403 になる｡
    pub(crate) fn following(
        &self,
        paths: &Paths,
        user_id: &str,
        pagination_token: Option<&str>,
        now: i64,
    ) -> Result<(Vec<User>, Option<String>)> {
        let url = following_url(user_id, pagination_token);
        let body = self.get(paths, Endpoint::Following, &url, now)?;
        parse_user_page(&body, "following")
    }

    /// list のメンバーを 1 ページ取得する (#163) — [`Self::following`] が
    /// 供給する diff のもう一方の側だ｡同じ形､一度に 1 ページという同じ契約､
    /// 同じ理屈｡
    pub(crate) fn list_members(
        &self,
        paths: &Paths,
        list_id: &str,
        pagination_token: Option<&str>,
        now: i64,
    ) -> Result<(Vec<User>, Option<String>)> {
        let url = list_members_url(list_id, pagination_token);
        let body = self.get(paths, Endpoint::ListMembers, &url, now)?;
        parse_user_page(&body, "list members")
    }

    /// `user_id` が所有する list を 1 ページ取得する (#164) — ツールバーの
    /// picker がセグメントの名前に使うものだ｡`list.read` が要る｡
    ///
    /// 1 ページだけで､呼び出し側はそこで止まる前提だ: picker はセグメントを
    /// 横に並べたもので､spec の 1 ページ 100 件を超える list を持つアカウント
    /// は､1 リクエストを超えるより前にとっくにセグメントの列を超えている｡
    /// それでもカーソルは返す｡その判断を記録するのは呼び出し側であって､
    /// このメソッドが隠すことではないからだ｡
    ///
    /// 返った list ごとに課金される (`x-api-budget`): リクエスト 1 件に加え､
    /// アカウントが持つ数だけの Lists-read resource｡
    pub(crate) fn owned_lists(
        &self,
        paths: &Paths,
        user_id: &str,
        pagination_token: Option<&str>,
        now: i64,
    ) -> Result<(Vec<ListSummary>, Option<String>)> {
        let url = owned_lists_url(user_id, pagination_token);
        let body = self.get(paths, Endpoint::OwnedLists, &url, now)?;
        parse_list_page(&body)
    }

    /// `POST /2/lists/:id/members` (#163) — `member_user_id` を list に足す｡
    /// `list.write` が要る｡
    ///
    /// 成功時は何も返さない: `sync` はレスポンスの中身ではなく､これが `Ok` を
    /// 返したかどうかで進捗を記録する｡**すでに list にいるアカウントに対する
    /// 2 回目の呼び出しが成功するのかエラーになるのかは未計測だ**｡だから plan
    /// ファイルは､retry が無害であることに頼らず､エントリが着地するたびに印を
    /// 付ける｡
    pub(crate) fn add_list_member(
        &self,
        paths: &Paths,
        list_id: &str,
        member_user_id: &str,
        now: i64,
    ) -> Result<()> {
        let url = list_members_write_url(list_id);
        Self::send_with_retry(paths, Endpoint::AddListMember, now, || {
            self.send_user_id_once(&url, member_user_id)
        })?;
        Ok(())
    }

    /// `DELETE /2/lists/:id/members/:user_id` (#163) — アカウントを list から
    /// 外す｡`list.write` が要る｡操作対象の id はボディのフィールドではなく
    /// パスセグメントで､[`delete_repost_url`] と同じ形だ｡
    pub(crate) fn remove_list_member(
        &self,
        paths: &Paths,
        list_id: &str,
        member_user_id: &str,
        now: i64,
    ) -> Result<()> {
        let url = remove_list_member_url(list_id, member_user_id);
        self.delete(paths, Endpoint::RemoveListMember, &url, now)?;
        Ok(())
    }

    /// `GET /2/tweets?ids=` (#12)｡`ids` は呼び出し側がすでにカンマで繋いだもの
    /// だ — X のクエリパラメータ自体がカンマ区切りの一覧を受け付けるので､
    /// ここでループするものは何も無い｡*id は 100 件まで* で､そこが X の
    /// パラメータの上限だ (#112)｡どちらの呼び出し側も上限には届かない:
    /// 親チェーンの遡りは id を 1 つ送るし､`main::parse_post_ids` はここへ来る
    /// 前に長すぎる一覧を拒む｡分割は意図してどこでもしていない — 1 回の呼び
    /// 出しが課金される複数のリクエストになってしまう｡2 つの呼び出し側が
    /// それに頼っている: `cache::fetch_thread` の親チェーンの遡りは一度に
    /// ちょうど 1 つの id を渡し (各段の id は前の段が解決して初めて分かる)､
    /// `main::fetch_post` (#42) は `--fetch-post` のすべての id を 1 回の呼び
    /// 出しに繋ぐので､5 件の post を引いてもリクエストはちょうど 1 件だ｡返す
    /// のは､要求した post のうち API が返してきたものに対して
    /// [`TimelineResponse::into_items`] が生むものだ: 一部が `data` からまるごと
    /// 欠けている (削除済み､非公開､その他の理由で不在) ときは要求した id の数
    /// より少ないエントリ (ゼロまで) になる — 親チェーンの遡りはそれをエラー
    /// ではなくきれいな停止として扱い､`--fetch-post` は即座に失敗せず不足を
    /// stderr へ報告する｡
    pub(crate) fn tweets_by_id(
        &self,
        paths: &Paths,
        ids: &str,
        now: i64,
    ) -> Result<Vec<TimelineItem>> {
        let url = tweets_by_id_url(ids);
        let body = self.get(paths, Endpoint::TweetById, &url, now)?;

        let response: TimelineResponse =
            serde_json::from_str(&body).context("could not parse the tweets-by-id response")?;
        Ok(response.into_items())
    }

    /// `POST /2/tweets` (#14､`quote_tweet_id` は #16 で追加) — composer の
    /// draft を新しい post として送る｡`quote_tweet_id` を付ければ quote に
    /// なる｡専用の `Endpoint::CreatePost` で追跡する (#10): X は投稿を上の
    /// read エンドポイントとは別に制限するので､どれかとバケツを共有すれば
    /// 両方が壊れる — そして #16 は､独立に追跡すべき quote 専用エンドポイント
    /// が X に無い以上､新しい `Endpoint` の variant を足さずこの同じ
    /// エンドポイントと追跡を意図して再利用している｡成功時は何も返さない —
    /// この呼び出しが作られた post のフィールドを返すのではなく､`ui.rs` は
    /// あとで普通のリロードに入る (他のリロードと同じく #10 の間隔に従う)｡
    /// 今のところそのフィールドを必要とするものは無い｡
    pub(crate) fn create_post(&self, paths: &Paths, draft: Draft<'_>, now: i64) -> Result<()> {
        let url = create_post_url();
        self.post(paths, Endpoint::CreatePost, &url, draft, now)?;
        Ok(())
    }

    /// `POST /2/users/:id/retweets` (#15) — `source_tweet_id` を `user_id`
    /// として repost する (サインイン中のアカウント自身の id｡`/me` から — #11)｡
    /// 専用の `Endpoint::CreateRepost` で追跡する (#10): X は repost の作成を
    /// 他のどのエンドポイントとも別に制限するので､どれかとバケツを共有すれば
    /// 双方の追跡状態が壊れる｡成功時は何も返さない — `repost::create` は
    /// この呼び出しが成功したかどうか､既知の conflict なら
    /// `repost::reconcile_from_error` から､何を永続化するかを決める｡
    pub(crate) fn create_repost(
        &self,
        paths: &Paths,
        user_id: &str,
        source_tweet_id: &str,
        now: i64,
    ) -> Result<()> {
        let url = create_repost_url(user_id);
        Self::send_with_retry(paths, Endpoint::CreateRepost, now, || {
            self.send_tweet_id_once(&url, source_tweet_id)
        })?;
        Ok(())
    }

    /// `DELETE /2/users/:id/retweets/:source_tweet_id` (#15) — repost を
    /// 取り消す｡`CreateRepost` とは独立に､専用の `Endpoint::DeleteRepost` で
    /// 追跡する (#10): X は作成と削除を別々に制限するし､#18 の usage 追跡も
    /// 同じ理由で独立した件数を要る｡
    pub(crate) fn delete_repost(
        &self,
        paths: &Paths,
        user_id: &str,
        source_tweet_id: &str,
        now: i64,
    ) -> Result<()> {
        let url = delete_repost_url(user_id, source_tweet_id);
        self.delete(paths, Endpoint::DeleteRepost, &url, now)?;
        Ok(())
    }

    /// `POST /2/users/:id/likes` (#68) — `user_id` として post に like する｡
    /// 他の write エンドポイントと同じ理由で専用の `Endpoint::CreateLike` で
    /// 追跡する (#10､#18): X はそれぞれを自分のスケジュールで制限するし､
    /// like は read とまったく同じに課金されるので数えなければならない｡
    ///
    /// `like.write` スコープが要る｡持たないセッションは 403 になり､`ui.rs` は
    /// リクエストを使う前にそれを未然に止める｡
    pub(crate) fn create_like(
        &self,
        paths: &Paths,
        user_id: &str,
        tweet_id: &str,
        now: i64,
    ) -> Result<()> {
        let url = create_like_url(user_id);
        Self::send_with_retry(paths, Endpoint::CreateLike, now, || {
            self.send_tweet_id_once(&url, tweet_id)
        })?;
        Ok(())
    }

    /// `DELETE /2/tweets/:id` (#72) — 自分の post を削除する｡
    ///
    /// 取り消せないので､`ui.rs` は明示的な確認の後でしかこれを呼ばない｡post が
    /// サインイン中のアカウントのものであることは､ここでは強制していない: X は
    /// 他人の post の削除を拒むし､`offers_delete` は必ず失敗するリクエストを
    /// 使うのではなく､クライアント側ですでに操作の入口を出さずにおく｡
    pub(crate) fn delete_post(&self, paths: &Paths, post_id: &str, now: i64) -> Result<()> {
        let url = delete_post_url(post_id);
        self.delete(paths, Endpoint::DeletePost, &url, now)?;
        Ok(())
    }

    /// `DELETE /2/users/:id/likes/:tweet_id` (#68) — like を外す｡
    /// [`Self::create_like`] を見よ｡`Endpoint::DeleteLike` で独立に追跡する｡
    pub(crate) fn delete_like(
        &self,
        paths: &Paths,
        user_id: &str,
        tweet_id: &str,
        now: i64,
    ) -> Result<()> {
        let url = delete_like_url(user_id, tweet_id);
        self.delete(paths, Endpoint::DeleteLike, &url, now)?;
        Ok(())
    }
}

/// そのステータスが retry に値するか: サーバー側 (5xx) の失敗だけだ｡
/// ネットワークエラーも retry するが､それがステータスコードとしてここへ届く
/// ことはない — `Self::get` の `Err` の腕へ短絡する｡429 では決して true に
/// ならない (`500..600` に入らない): どちらの種類の 429 も retry の候補では
/// ないというのが #10 の眼目だ｡
fn is_retryable_status(status: u16) -> bool {
    (500..600).contains(&status)
}

fn user_lookup_url(username: &str) -> String {
    Url::api(API_BASE)
        .segment("users")
        .segment("by")
        .segment("username")
        .segment(username)
        .build()
}

/// どのエンドポイントも要求する著者のフィールド (#92) — timeline の URL 2 つ
/// と単一 post のルックアップだ｡編集は 3 箇所ではなく 1 箇所で済む｡
///
/// #165 以降は `&user.fields=…` という断片ではなく組にしてある: `&` と `=` を
/// 書くのは [`Url`] の役目だし､自前の区切りを抱えた定数はクエリの 1 箇所に
/// しか差し込めない｡
const USER_FIELDS: &[(&str, &str)] = &[("user.fields", "name,profile_image_url,username")];

/// 2 つの timeline エンドポイントが要求する `*.fields` と `expansions` (#92)｡
/// 以前は同じものを二度書き出していた｡#104 がその代償だ: repost の画像を直す
/// には長い文字列 1 本を 2 箇所へ貼る必要があり､片方だけ直せば編集し損ねた
/// ほうの timeline で repost が壊れたままになっていた｡
///
/// 共有する部分だけだ｡パス､`max_results`､`since_id`､`pagination_token` は
/// それぞれのビルダーに残る — 違うのはそこだからだ｡
const TIMELINE_FIELDS: &[(&str, &str)] = &[
    (
        "tweet.fields",
        "created_at,entities,public_metrics,referenced_tweets",
    ),
    (
        "expansions",
        "attachments.media_keys,author_id,referenced_tweets.id,\
         referenced_tweets.id.author_id,referenced_tweets.id.attachments.media_keys",
    ),
    (
        "media.fields",
        "alt_text,height,preview_image_url,type,url,width",
    ),
    ("user.fields", "name,profile_image_url,username"),
];

/// `GET /2/users/me` (#11) — サインイン中のユーザー自身の id と screen name を
/// 解決する｡意味を持つのは OAuth の user-context な資格情報のときだけで､
/// app-only の bearer token は home timeline 自体と同じくここで 401 になる｡
fn me_url() -> String {
    Url::api(API_BASE).segment("users").segment("me").build()
}

/// home timeline のエンドポイント (#11)｡返る post の形が同じなので
/// [`timeline_url`] と同じ expansions を付ける｡`since_id` (差分リロード) と
/// `pagination_token` (#11 の "Load older") は実際上は排他だ — リロードは
/// つねにキャッシュの最も新しい post から始まり､"Load older" はつねに前の
/// レスポンスの `meta.next_token` から再開する — が､純粋な URL 組み立ての
/// ロジックがどの呼び出し側に仕えているかを知らずに済むよう､ここでは両方を
/// 独立に受け付ける｡
///
/// repost の *元の* post を `includes.media` に入れるのは
/// `referenced_tweets.id.attachments.media_keys` (#104) だ: 素の
/// `attachments.media_keys` は外側の post 自身の添付しか展開せず､repost は
/// 自前の添付を持たない — メディアは元の post にあり､これが拡張している
/// `referenced_tweets.id` の expansion を通ってしか届かない｡これが無いと､
/// repost の結合ロジック自体 (`item.media = post_media(original, media)`) は
/// すでに正しかったのに､repost された photo/video に対して
/// `model::post_media` は結合する相手を持てなかった｡
fn home_timeline_url(
    user_id: &str,
    max_results: u32,
    since_id: Option<&str>,
    pagination_token: Option<&str>,
) -> String {
    Url::api(API_BASE)
        .segment("users")
        .segment(user_id)
        .segment("timelines")
        .segment("reverse_chronological")
        .number("max_results", max_results)
        .params(TIMELINE_FIELDS)
        .maybe("since_id", since_id)
        .maybe("pagination_token", pagination_token)
        .build()
}

/// timeline のエンドポイントは､`expansions` と `*.fields` のパラメータで
/// もっと要求しないかぎり素の post id を返すので､クエリ文字列が効いている｡
/// `referenced_tweets.id.attachments.media_keys` (#104) がここにもある理由は
/// [`home_timeline_url`] の doc コメントを見よ｡
fn timeline_url(user_id: &str, max_results: u32, since_id: Option<&str>) -> String {
    Url::api(API_BASE)
        .segment("users")
        .segment(user_id)
        .segment("tweets")
        .number("max_results", max_results)
        .params(TIMELINE_FIELDS)
        .maybe("since_id", since_id)
        .build()
}

/// List の timeline のエンドポイント (#161)｡返る post の形が同じなので
/// [`home_timeline_url`] と同じ expansions を付ける — #104 の
/// `referenced_tweets.id.attachments.media_keys` も含むので､メンバーの repost
/// はここでも元の post のメディアを運ぶ｡
///
/// `since_id` のパラメータは無く､書き忘れではない: エンドポイントがそれを
/// 取らない｡その代償は [`XClient::list_timeline`] を見よ｡
fn list_timeline_url(list_id: &str, max_results: u32, pagination_token: Option<&str>) -> String {
    Url::api(API_BASE)
        .segment("lists")
        .segment(list_id)
        .segment("tweets")
        .number("max_results", max_results)
        .params(TIMELINE_FIELDS)
        .maybe("pagination_token", pagination_token)
        .build()
}

/// `GET /2/tweets?ids=` (#12)｡timeline のエンドポイントと同じ expansions を
/// 付けるので､取得した post 自身の著者 (それ自体が reply なら親の id も) が
/// 同じレスポンスで返る｡`ids` はそのまま差し込む — 親チェーンの遡りなら
/// 単一の id (次の id はこれが解決して初めて分かるので､一度に 2 つ以上手元に
/// 持つことはない)､`--fetch-post` (#42) ならカンマ区切りの一覧だ｡X の
/// クエリパラメータ自体がどちらも受け付けるので､このクレートが自前で繋いだり
/// ループしたりする必要はない｡
///
/// 上の timeline のビルダーと違い､こちらは `public_metrics` (#67) も
/// `entities` (#70) も要求しない: 遡った親は [`crate::thread::ThreadItem`]
/// として描かれ､件数もリンクも見せないからだ — そして `--fetch-post` の JSON
/// 出力はそのままの [`TimelineItem`] で､それはまさに同じレスポンスの形から
/// 来ているので､代わりにそれらのフィールドを与える二つ目の URL ビルダーは
/// 無い｡
///
/// 同じ理由で､#104 の `referenced_tweets.id.attachments.media_keys` も意図して
/// *与えていない* (timeline のビルダーがずっと持っている素の
/// `attachments.media_keys`/`media.fields` すら与えていない): 親チェーンの
/// 遡りの結果は [`crate::thread::ThreadItem`] に着地するが､そこには `media`
/// フィールドがそもそも無く､`thread_row` (`ui.rs`) は一度も描かない — 遡った
/// 親のために取得したメディアは､パースされてから黙って捨てられることになる｡
/// "Show thread" (#12) にメディアを通すのは表示層の変更であって
/// (`ThreadItem` にフィールドが増え､`thread_row` に描画の経路が増える)､
/// expansions の修正ではないので､#104 のスコープの外に置く｡`--fetch-post`
/// (#42) は JSON 出力に [`TimelineItem`] の `media` フィールドを残してはいる
/// が､メディアに対応させるなら *素の* `attachments.media_keys`/`media.fields`
/// の組も足すことになる｡このビルダーはどちらも持ったことがないからだ — それは
/// 今サポートしていないフィールドのための新機能であって､#104 が timeline の
/// エンドポイントに対して直す repost 固有の expansions の穴ではない｡
fn tweets_by_id_url(ids: &str) -> String {
    Url::api(API_BASE)
        .segment("tweets")
        .param("ids", ids)
        // 上の理由により [`TIMELINE_FIELDS`] ではなく自前の組を使う:
        // このエンドポイントは意図して要求を減らしている｡
        .params(&[
            ("tweet.fields", "created_at,referenced_tweets"),
            (
                "expansions",
                "author_id,referenced_tweets.id,referenced_tweets.id.author_id",
            ),
        ])
        .params(USER_FIELDS)
        .build()
}

/// #163 の 2 つのページ読み取りが要求するページサイズ｡
///
/// **spec 由来｡** docs.x.com は `/2/users/:id/following` と
/// `/2/lists/:id/members` のどちらにも 1..=100 の `max_results` を与えている｡
/// ここでは何も計測していない｡この範囲の端として最大値に座るのが正しいのは､
/// read がリクエストごとではなく返った resource ごとに課金されるからだ
/// (`x-api-budget`): 小さいページは何も得をせず､同じアカウントを読むのに
/// エンドポイントの rate limit をより多く使う｡
const USER_PAGE_SIZE: u32 = 100;

/// 2 つのページ読み取りが要求する `user.fields`｡dry-run の報告がアカウントを
/// 名指すのに要るものだけだ — ここでアバターや metrics を要求しても､何も
/// 出力しないフィールドになる｡
const SYNC_USER_FIELDS: &[(&str, &str)] = &[("user.fields", "name,username")];

/// `GET /2/users/:id/following` (#163) — このアプリがフォローしている
/// アカウントの 1 ページ｡
fn following_url(user_id: &str, pagination_token: Option<&str>) -> String {
    Url::api(API_BASE)
        .segment("users")
        .segment(user_id)
        .segment("following")
        .number("max_results", USER_PAGE_SIZE)
        .params(SYNC_USER_FIELDS)
        .maybe("pagination_token", pagination_token)
        .build()
}

/// `GET /2/lists/:id/members` (#163) — list のメンバーの 1 ページ｡
fn list_members_url(list_id: &str, pagination_token: Option<&str>) -> String {
    Url::api(API_BASE)
        .segment("lists")
        .segment(list_id)
        .segment("members")
        .number("max_results", USER_PAGE_SIZE)
        .params(SYNC_USER_FIELDS)
        .maybe("pagination_token", pagination_token)
        .build()
}

/// `GET /2/users/:id/owned_lists` (#164) — アカウントが所有する list の
/// 1 ページ｡
///
/// [`USER_PAGE_SIZE`] と同じく **spec 由来** だ: docs.x.com はこの read に
/// 既定 100 の 1..=100 の `max_results` を与えていて､picker 経由の最初の実
/// 取得より先は何も計測していない｡`list.fields` は無い: `id` と `name` が
/// 既定のフィールドであり､picker が描くのはそれだけだ｡
fn owned_lists_url(user_id: &str, pagination_token: Option<&str>) -> String {
    Url::api(API_BASE)
        .segment("users")
        .segment(user_id)
        .segment("owned_lists")
        .number("max_results", USER_PAGE_SIZE)
        .maybe("pagination_token", pagination_token)
        .build()
}

/// `POST /2/lists/:id/members` (#163) — 追加するアカウントの id は URL では
/// なく JSON ボディ ([`UserIdRequest`]) に乗る｡
fn list_members_write_url(list_id: &str) -> String {
    Url::api(API_BASE)
        .segment("lists")
        .segment(list_id)
        .segment("members")
        .build()
}

/// `DELETE /2/lists/:id/members/:user_id` (#163) — ここでは操作対象の id が
/// パスセグメントで､[`delete_repost_url`] と同じ形だ｡
fn remove_list_member_url(list_id: &str, member_user_id: &str) -> String {
    Url::api(API_BASE)
        .segment("lists")
        .segment(list_id)
        .segment("members")
        .segment(member_user_id)
        .build()
}

/// #163 の 2 つのページ読み取りのどちらかから､ユーザーの 1 ページをパース
/// する｡`what` がどちらかを名指すので､パース失敗はフォロー一覧とメンバー
/// 一覧のどちらが読めなかったかを言える — 2 つは同じループで読まれるので､
/// そうでなければエラーは見分けが付かない｡
fn parse_user_page(body: &str, what: &str) -> Result<(Vec<User>, Option<String>)> {
    let response: UserPageResponse = serde_json::from_str(body)
        .with_context(|| format!("could not parse the {what} response"))?;
    let next_token = response.next_token().map(str::to_string);
    Ok((response.data, next_token))
}

/// `GET /2/users/:id/owned_lists` から list の 1 ページをパースする (#164)｡
fn parse_list_page(body: &str) -> Result<(Vec<ListSummary>, Option<String>)> {
    let response: ListPageResponse =
        serde_json::from_str(body).context("could not parse the owned lists response")?;
    let next_token = response.next_token().map(str::to_string);
    Ok((response.data, next_token))
}

/// `POST /2/tweets` (#14) — 上のどの `GET` とも違い､クエリ文字列は無い｡
fn create_post_url() -> String {
    Url::api(API_BASE).segment("tweets").build()
}

/// `POST /2/users/:id/retweets` (#15) — `user_id` はサインイン中のアカウント
/// 自身の id (`/me`､#11)｡対象の post の id は URL ではなく JSON ボディ
/// ([`TweetIdRequest`]) に乗る｡
fn create_repost_url(user_id: &str) -> String {
    Url::api(API_BASE)
        .segment("users")
        .segment(user_id)
        .segment("retweets")
        .build()
}

/// `DELETE /2/users/:id/retweets/:source_tweet_id` (#15) — このクレートで
/// *操作対象* の resource 自身の id が､クエリパラメータでも JSON ボディの
/// フィールドでもなく URL のパスセグメントになる唯一のエンドポイントだ｡
fn delete_repost_url(user_id: &str, source_tweet_id: &str) -> String {
    Url::api(API_BASE)
        .segment("users")
        .segment(user_id)
        .segment("retweets")
        .segment(source_tweet_id)
        .build()
}

/// `DELETE /2/tweets/:id` (#72) — ここの他のどの write エンドポイントとも違い､
/// これはユーザーを名指さない: X は資格情報からアカウントを推し量り､自分の
/// ものでない post を拒む｡
fn delete_post_url(post_id: &str) -> String {
    Url::api(API_BASE)
        .segment("tweets")
        .segment(post_id)
        .build()
}

/// `POST /2/users/:id/likes` (#68) — `user_id` はサインイン中のアカウント自身
/// の id (`/me`､#11)｡対象の post の id は repost とまったく同じく､URL ではなく
/// JSON ボディ ([`TweetIdRequest`]) に乗る｡
fn create_like_url(user_id: &str) -> String {
    Url::api(API_BASE)
        .segment("users")
        .segment(user_id)
        .segment("likes")
        .build()
}

/// `DELETE /2/users/:id/likes/:tweet_id` (#68) — ここでは操作対象の post の id
/// が URL のパスセグメントで､[`delete_repost_url`] と同じ形だ｡
fn delete_like_url(user_id: &str, tweet_id: &str) -> String {
    Url::api(API_BASE)
        .segment("users")
        .segment(user_id)
        .segment("likes")
        .segment(tweet_id)
        .build()
}

/// レスポンスボディに API 自身のエラーテキストがあれば取り出す｡
fn describe_problem(body: &str) -> Option<String> {
    serde_json::from_str::<ApiProblem>(body)
        .ok()
        .and_then(|problem| problem.message())
}

/// 429 の記録を残す (#199): どのエンドポイントか､ウィンドウのヘッダが何と
/// 言っていたか､そしてボディの先頭だ｡これ以前は､不透明な拒否 (300 のうち
/// `remaining` 299 なのに拒まれる) が動かなくなった mtime しか残さず､それが
/// どの上限から来たのかは今も特定できていない — 後からログを読むときに頼れる
/// のがこの行だ｡エラーボディはリクエストパラメータをそのまま返すので
/// (`x-api-endpoints`) ボディは上限を切ってあり､他の行と同じく `log::redact`
/// を通る｡
fn log_429(endpoint: Endpoint, state: RateLimitState, refusal: rate_limit::Refusal, body: &str) {
    let snippet: String = body.chars().take(400).collect();
    let kind = match refusal {
        rate_limit::Refusal::Window { .. } => "window exhausted",
        rate_limit::Refusal::Opaque => "opaque: headroom left in the window",
    };
    crate::log::warn(&format!(
        "{} refused with 429 ({kind}); x-rate-limit limit={} remaining={} reset={}; body: {snippet}",
        endpoint.key(),
        state.limit.map_or("-".to_string(), |n| n.to_string()),
        state.remaining.map_or("-".to_string(), |n| n.to_string()),
        state.reset_at.map_or("-".to_string(), |n| n.to_string()),
    ));
}

/// X がリクエストそのものを拒んだ (#239)｡retry では直らない 2 つの場合を
/// 運ぶ｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Denial {
    /// 401 — token が失効しているか無効｡`x-api-endpoints` の実測どおり､
    /// このレスポンスには `x-rate-limit-*` すら付かない｡
    Rejected,
    /// 403 — この app にそのエンドポイントの権限が無いか､アカウントの
    /// monthly spend cap に当たっている｡後者は残高切れ (429 +
    /// [`rate_limit::UsageCapExceeded`]) とは別物で､平文の detail でしか
    /// 名乗らない｡
    Forbidden,
}

/// 401/403 を型で運ぶ (#239)｡呼び出し側がメッセージを grep せずに
/// 「これは待っても直らない」と判断できるようにするためで､429 の 2 種類を
/// 型に分けた #10 と同じ理屈だ｡
///
/// `endpoint` を持つのは #239 のログがまさにそれを欠いていたからだ:
/// 拒否が 180 秒ごとに 100 行積み上がっても､どの呼び出しが拒まれたのかは
/// どこにも書かれていなかった｡
#[derive(Debug)]
pub(crate) struct Denied {
    pub endpoint: Endpoint,
    pub denial: Denial,
    pub detail: String,
}

impl std::fmt::Display for Denied {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = match self.denial {
            Denial::Rejected => "401 Unauthorized — the bearer token was rejected",
            Denial::Forbidden => "403 Forbidden — this app cannot access the endpoint",
        };
        write!(
            formatter,
            "{}: {reason}: {}",
            self.endpoint.key(),
            self.detail
        )
    }
}

impl std::error::Error for Denied {}

/// レスポンスのステータスを検証し､2xx でないものをエラーへ変換する｡
///
/// `refusal` は普通の 429 がどの limit から来たかで､生の
/// `x-rate-limit-reset` ヘッダを読むのではなく
/// [`rate_limit::Refusal::classify`] がすでにウィンドウの状態と突き合わせて
/// いる｡だからこれらのヘッダが説明しない limit からの 429 は､誤ったウィンドウ
/// の時計ではなく保守的な backoff を持ち — しかもそう明言するので､`sync` は
/// 毎回さらに退がれる｡普通の 429 以外のステータスでは無視する｡
///
/// どのステータスも `endpoint` を名乗る (#239)｡#10 の時点でそう名乗って
/// いたのは 429 のログ ([`log_429`]) だけで､401 と 403 は名前の無いまま
/// ログへ流れていた｡[`log_429`] と同じ `{key}: ` の前置に揃えてある｡
///
/// 401/403 は [`Denied`] へ (#239)､2 種類の 429 は
/// [`rate_limit::classify_429`] を通じて [`rate_limit::UsageCapExceeded`] と
/// [`rate_limit::RateLimited`] へ分かれる｡404 とその他のステータスだけが
/// 平文の `anyhow` エラーのままだ — 呼び出し側がこれらを型で見分ける必要は
/// まだ無い｡
fn check_status(
    endpoint: Endpoint,
    status: u16,
    body: &str,
    refusal: rate_limit::Refusal,
    now: i64,
) -> Result<()> {
    if (200..300).contains(&status) {
        return Ok(());
    }

    let detail = describe_problem(body).unwrap_or_else(|| {
        let snippet: String = body.chars().take(400).collect();
        if snippet.is_empty() {
            "(empty response body)".to_string()
        } else {
            snippet
        }
    });

    let key = endpoint.key();
    match status {
        401 => Err(Denied {
            endpoint,
            denial: Denial::Rejected,
            detail,
        }
        .into()),
        403 => Err(Denied {
            endpoint,
            denial: Denial::Forbidden,
            detail,
        }
        .into()),
        404 => bail!("{key}: 404 Not Found — {detail}"),
        429 => match rate_limit::classify_429(body) {
            rate_limit::RateLimitKind::UsageCapExceeded => {
                Err(rate_limit::UsageCapExceeded { detail }.into())
            }
            rate_limit::RateLimitKind::RateLimited => Err(refusal.into_error(now).into()),
        },
        _ => bail!("{key}: HTTP {status} — {detail}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_success_statuses() {
        assert!(check_status(Endpoint::Me, 200, "{}", rate_limit::Refusal::Opaque, 0).is_ok());
        assert!(check_status(Endpoint::Me, 299, "", rate_limit::Refusal::Opaque, 0).is_ok());
    }

    #[test]
    fn builds_the_user_lookup_url() {
        assert_eq!(
            user_lookup_url("XDevelopers"),
            "https://api.x.com/2/users/by/username/XDevelopers"
        );
    }

    // --- #165: 組み立てた URL を読み解く ---
    //
    // 下のテストはかつてどれも 1 本の長いリテラルで､#161 がその代償だった:
    // スコープを 1 つ足すのに､いくつものテストを手で 1 文字ずつ編集する
    // ことになり､タイプミスと緑の実行の間に立つのは目視だけだった｡
    //
    // そこで今はテストが実際に何についてのものかを言う — "これは `since_id`
    // を足すだけで他は何も変えない"､"この 2 つは同じフィールドを要求する" —
    // 関心の無いフィールド一覧はテストの外に置く｡
    //
    // URL 全体を固定するテストは､エスケープの方針ごとに 1 本ずつ意図して
    // 残してある: ここの
    // `builds_the_home_timeline_url_with_every_expansion` と､`oauth::pkce`
    // の `build_authorize_url_includes_every_required_parameter` だ｡
    // 両者を合わせれば､このクレートがワイヤに出すバイトはどれもどこかで
    // 固定されたままで､だから残りは緩めても安全だ｡

    /// クエリ文字列を除いた `url` のパス｡
    fn path_of(url: &str) -> &str {
        url.split_once('?').map_or(url, |(path, _)| path)
    }

    /// `url` のクエリパラメータを､現れる順に返す｡
    fn query_of(url: &str) -> Vec<(&str, &str)> {
        let Some((_, query)) = url.split_once('?') else {
            return Vec::new();
        };
        query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .collect()
    }

    /// `later` が `earlier` に足したものを､順に返す｡
    ///
    /// 任意パラメータのテストはどれも実のところこの assertion だ: カーソルが
    /// 届き､他は何も動かなかった｡長い共通の前置きを言い直さずに済むよう､
    /// 差分として書いてある｡
    fn added<'a>(earlier: &str, later: &'a str) -> Vec<(&'a str, &'a str)> {
        let before = query_of(earlier);
        query_of(later)
            .into_iter()
            .filter(|pair| !before.iter().any(|seen| seen.0 == pair.0))
            .collect()
    }

    #[test]
    fn the_single_user_timeline_asks_for_the_same_fields_as_the_home_one() {
        // #92 がフィールドの集合を `TIMELINE_FIELDS` へ括り出したのは､まさに
        // この 2 つがずれないようにするためだ｡これがそれを言う assertion で
        // あって､誰かが両方を編集するのを覚えていたときだけ歩調の揃う､同じ
        // 長いリテラルの 2 つのコピーではない｡
        let url = timeline_url("2244994945", 20, None);
        assert_eq!(path_of(&url), "https://api.x.com/2/users/2244994945/tweets");
        assert_eq!(
            query_of(&url),
            query_of(&home_timeline_url("2244994945", 20, None, None))
        );
    }

    #[test]
    fn builds_the_tweets_by_id_url_asking_for_less_than_a_timeline_does() {
        // #12: 親チェーンの遡りは 1 段につき id を 1 つ取得する｡timeline の
        // エンドポイントより *少ない* フィールドしか要求しない — metrics も
        // entities もメディアも無い (それぞれが意図的である理由は
        // `tweets_by_id_url` の doc を見よ) — ので､どちらの一覧も言い直さず
        // 差分を assert する｡
        let url = tweets_by_id_url("1700000000000000001");
        assert_eq!(path_of(&url), "https://api.x.com/2/tweets");

        let query = query_of(&url);
        assert_eq!(query.first(), Some(&("ids", "1700000000000000001")));
        assert!(
            !query.iter().any(|(key, _)| *key == "media.fields"),
            "a walked parent renders as a ThreadItem, which has no media: {query:?}"
        );
        for (key, value) in &query {
            if *key == "tweet.fields" {
                assert!(!value.contains("public_metrics"), "{value}");
                assert!(!value.contains("entities"), "{value}");
            }
        }
    }

    #[test]
    fn builds_the_tweets_by_id_url_with_a_comma_joined_id_list() {
        // #42: `--fetch-post` は `tweets_by_id` を呼ぶ前に要求されたすべての
        // id をカンマで繋ぐ｡このビルダーがループするのではなく､`ids` が
        // そのままクエリ文字列に載ることに頼っている — X の `ids=` パラメータ
        // 自体がカンマ区切りの一覧を受け付けるので､id が 3 つでもリクエストは
        // ちょうど 1 件だ｡#165 以降はこれが `Escaping::Api` の唯一の規則も
        // 固定している: カンマは生のままだ｡
        assert_eq!(
            query_of(&tweets_by_id_url("1,2,3")).first(),
            Some(&("ids", "1,2,3"))
        );
    }

    #[test]
    fn builds_the_create_post_url_with_no_query_string() {
        // #14: 上のどの GET とも違い､POST /2/tweets はクエリパラメータを
        // 取らない — post の本文は代わりに JSON ボディに乗る｡
        assert_eq!(create_post_url(), "https://api.x.com/2/tweets");
    }

    #[test]
    fn builds_the_create_repost_url() {
        assert_eq!(
            create_repost_url("2244994945"),
            "https://api.x.com/2/users/2244994945/retweets"
        );
    }

    #[test]
    fn builds_the_delete_repost_url_with_the_source_tweet_id_as_a_path_segment() {
        assert_eq!(
            delete_repost_url("2244994945", "1700000000000000001"),
            "https://api.x.com/2/users/2244994945/retweets/1700000000000000001"
        );
    }

    #[test]
    fn builds_the_delete_post_url() {
        assert_eq!(
            delete_post_url("1700000000000000001"),
            "https://api.x.com/2/tweets/1700000000000000001"
        );
    }

    #[test]
    fn builds_the_create_like_url() {
        assert_eq!(
            create_like_url("2244994945"),
            "https://api.x.com/2/users/2244994945/likes"
        );
    }

    #[test]
    fn builds_the_delete_like_url_with_the_tweet_id_as_a_path_segment() {
        assert_eq!(
            delete_like_url("2244994945", "1700000000000000001"),
            "https://api.x.com/2/users/2244994945/likes/1700000000000000001"
        );
    }

    #[test]
    fn builds_the_me_url() {
        assert_eq!(me_url(), "https://api.x.com/2/users/me");
    }

    #[test]
    fn builds_the_home_timeline_url_with_every_expansion() {
        assert_eq!(
            home_timeline_url("2244994945", 20, None, None),
            "https://api.x.com/2/users/2244994945/timelines/reverse_chronological?max_results=20&tweet.fields=created_at,entities,public_metrics,referenced_tweets&expansions=attachments.media_keys,author_id,referenced_tweets.id,referenced_tweets.id.author_id,referenced_tweets.id.attachments.media_keys&media.fields=alt_text,height,preview_image_url,type,url,width&user.fields=name,profile_image_url,username"
        );
    }

    #[test]
    fn home_timeline_url_appends_since_id_for_an_incremental_reload() {
        assert_eq!(
            added(
                &home_timeline_url("2244994945", 20, None, None),
                &home_timeline_url("2244994945", 20, Some("1700000000000000001"), None),
            ),
            vec![("since_id", "1700000000000000001")]
        );
    }

    #[test]
    fn home_timeline_url_appends_pagination_token_for_load_older() {
        // #11: "Load older" は前のレスポンスの `meta.next_token` を
        // `pagination_token` として送り直す｡
        assert_eq!(
            added(
                &home_timeline_url("2244994945", 20, None, None),
                &home_timeline_url("2244994945", 20, None, Some("cursor-abc")),
            ),
            vec![("pagination_token", "cursor-abc")]
        );
    }

    #[test]
    fn builds_the_list_timeline_url_with_every_expansion() {
        // #161: home timeline と同じフィールドの集合なので､list の行も home
        // の行も同じ `TimelineItem` にパースされる｡リテラルではなく home の
        // ビルダーと比べているのは､"home timeline と同じ" ということ自体が
        // 主張の全部だからだ｡
        let url = list_timeline_url("2091351590695588200", 20, None);
        assert_eq!(
            path_of(&url),
            "https://api.x.com/2/lists/2091351590695588200/tweets"
        );
        assert_eq!(
            query_of(&url),
            query_of(&home_timeline_url("2244994945", 20, None, None))
        );
    }

    #[test]
    fn list_timeline_url_appends_pagination_token_for_load_older() {
        assert_eq!(
            added(
                &list_timeline_url("2091351590695588200", 20, None),
                &list_timeline_url("2091351590695588200", 20, Some("cursor-abc")),
            ),
            vec![("pagination_token", "cursor-abc")]
        );
    }

    #[test]
    fn the_list_timeline_url_carries_no_since_id() {
        // このエンドポイントは `since_id` を取らない
        // (`XClient::list_timeline` を見よ) ので､送れば API の知らない
        // パラメータになる｡ビルダーの形を信じるのではなく不在を assert する:
        // フィールド一覧は十分に長く､`home_timeline_url` からの誤った貼り付け
        // はレビューで目立たない｡
        assert!(!list_timeline_url("2091351590695588200", 20, None).contains("since_id"));
    }

    #[test]
    fn builds_the_following_url() {
        let url = following_url("2244994945", None);
        assert_eq!(
            path_of(&url),
            "https://api.x.com/2/users/2244994945/following"
        );
        // 費用を決めるのはページサイズだ — read は返ったアカウントごとに課金
        // される — ので固定する｡フィールド一覧は固定しない｡#163 の 2 つの
        // read は `SYNC_USER_FIELDS` を共有していて､隣のテストがすでに両者は
        // 一致すべきだと言っているからだ｡
        assert_eq!(query_of(&url).first(), Some(&("max_results", "100")));
    }

    #[test]
    fn following_url_appends_the_pagination_token() {
        // #163 はカーソルが尽きるまでこれをページ送りする｡token を落とせば
        // 1 ページ目を永遠に読み直し､diff は終わらない｡
        assert_eq!(
            added(
                &following_url("2244994945", None),
                &following_url("2244994945", Some("cursor-abc")),
            ),
            vec![("pagination_token", "cursor-abc")]
        );
    }

    #[test]
    fn builds_the_list_members_url() {
        let url = list_members_url("2091351590695588200", None);
        assert_eq!(
            path_of(&url),
            "https://api.x.com/2/lists/2091351590695588200/members"
        );
        assert_eq!(query_of(&url).first(), Some(&("max_results", "100")));
    }

    #[test]
    fn both_of_the_sync_reads_ask_for_the_same_user_fields() {
        // #163 はフォロー一覧と list のメンバーを読んで差分を取る｡両側に違う
        // フィールドを要求すれば､片側のアカウントは､もう片側が埋める報告に
        // 出せなくなる｡
        assert_eq!(
            query_of(&following_url("2244994945", None)),
            query_of(&list_members_url("2091351590695588200", None))
        );
    }

    #[test]
    fn list_members_url_appends_the_pagination_token() {
        assert_eq!(
            added(
                &list_members_url("2091351590695588200", None),
                &list_members_url("2091351590695588200", Some("cursor-abc")),
            ),
            vec![("pagination_token", "cursor-abc")]
        );
    }

    #[test]
    fn the_read_and_write_list_member_urls_differ_by_more_than_the_method() {
        // 追加はコレクションへ POST し､削除はパスでメンバーを名指す｡
        // コレクションの URL へ DELETE を送っても何も外れず､課金だけされる｡
        assert_eq!(
            list_members_write_url("2091351590695588200"),
            "https://api.x.com/2/lists/2091351590695588200/members"
        );
        assert_eq!(
            remove_list_member_url("2091351590695588200", "2244994945"),
            "https://api.x.com/2/lists/2091351590695588200/members/2244994945"
        );
    }

    #[test]
    fn parses_a_user_page_and_its_cursor() {
        let (users, next) = parse_user_page(
            r#"{"data":[{"id":"1","name":"A","username":"a"}],"meta":{"next_token":"c"}}"#,
            "following",
        )
        .unwrap();
        assert_eq!(users.len(), 1);
        assert_eq!(next.as_deref(), Some("c"));
    }

    #[test]
    fn the_owned_lists_url_reads_one_full_page_of_the_users_own_lists() {
        // #164: picker は切り替え先に名前を付けるので､アカウントが持つ list
        // すべてを 1 回の read で要る｡100 は spec の上限だ｡read はリクエスト
        // ごとではなく返った list ごとに課金されるので､小さいページは 2 回目の
        // リクエストを増やすだけだ｡
        let url = owned_lists_url("5685672", None);
        assert_eq!(
            path_of(&url),
            "https://api.x.com/2/users/5685672/owned_lists"
        );
        assert_eq!(query_of(&url), vec![("max_results", "100")]);
        assert_eq!(
            added(&url, &owned_lists_url("5685672", Some("c"))),
            vec![("pagination_token", "c")]
        );
    }

    #[test]
    fn parses_an_owned_lists_page_and_its_cursor() {
        let (lists, next) = parse_list_page(
            r#"{"data":[{"id":"2091351590695588200","name":"following mirror"},{"id":"7","name":"rust"}],"meta":{"result_count":2,"next_token":"c"}}"#,
        )
        .unwrap();
        assert_eq!(lists.len(), 2);
        assert_eq!(lists[0].id, "2091351590695588200");
        assert_eq!(lists[0].name, "following mirror");
        assert_eq!(next.as_deref(), Some("c"));
    }

    #[test]
    fn an_owned_lists_page_with_no_data_is_empty_not_an_error() {
        // list を 1 つも持たないアカウントには `meta.result_count: 0` が返り､
        // `data` キーはまったく無い — `TimelineResponse` が許容するのと同じ形だ｡
        let (lists, next) = parse_list_page(r#"{"meta":{"result_count":0}}"#).unwrap();
        assert!(lists.is_empty());
        assert!(next.is_none());
    }

    #[test]
    fn an_owned_lists_parse_failure_names_the_read() {
        let error = parse_list_page("not json").unwrap_err().to_string();
        assert!(error.contains("owned lists"), "{error}");
    }

    #[test]
    fn a_user_page_parse_failure_names_which_read_it_was() {
        // どちらの read も同じループを通る｡これが無ければエラーは､どれかの
        // ページが読めなかったとしか言わない｡
        let error = parse_user_page("not json", "list members")
            .unwrap_err()
            .to_string();
        assert!(error.contains("list members"), "{error}");
    }

    #[test]
    fn appends_since_id_when_given() {
        // #9: 差分リロードはキャッシュ内で最も新しい post の id を渡すので､
        // API は新しいものだけを返し､レスポンスもクレジットの費用も抑えられる｡
        assert_eq!(
            added(
                &timeline_url("2244994945", 20, None),
                &timeline_url("2244994945", 20, Some("1700000000000000001")),
            ),
            vec![("since_id", "1700000000000000001")]
        );
    }

    #[test]
    fn explains_an_exhausted_credit_cap() {
        let body =
            r#"{"title":"UsageCapExceeded","detail":"Usage cap exceeded: Monthly product cap"}"#;
        let error = check_status(
            Endpoint::ListTimeline,
            429,
            body,
            rate_limit::Refusal::Opaque,
            0,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("429"), "{error}");
        assert!(error.contains("Usage cap exceeded"), "{error}");
    }

    #[test]
    fn a_usage_cap_429_downcasts_to_the_typed_error() {
        // #10: この区別は呼び出し側での文字列比較ではなく型でなければならない
        // — だからこそ `ui.rs` (や他のどの呼び出し側) もメッセージを grep せず
        // match できる｡
        let body =
            r#"{"title":"UsageCapExceeded","detail":"Usage cap exceeded: Monthly product cap"}"#;
        let error = check_status(
            Endpoint::ListTimeline,
            429,
            body,
            rate_limit::Refusal::Opaque,
            1_700_000_000,
        )
        .unwrap_err();
        let typed = error
            .downcast_ref::<rate_limit::UsageCapExceeded>()
            .unwrap();
        assert!(typed.detail.contains("Usage cap exceeded"), "{typed:?}");
    }

    #[test]
    fn an_ordinary_rate_limit_429_downcasts_to_the_typed_error_carrying_the_retry_time() {
        // `check_status` は渡された refusal をそのまま運ぶ — ウィンドウの状態
        // との突き合わせは `rate_limit::Refusal::classify` にあり､テストも
        // そこにある｡
        let body = r#"{"title":"TooManyRequests","detail":"Rate limit exceeded"}"#;
        let refusal = rate_limit::Refusal::Window {
            reset_at: 1_700_000_000,
        };
        let error =
            check_status(Endpoint::ListTimeline, 429, body, refusal, 1_699_999_000).unwrap_err();
        let typed = error.downcast_ref::<rate_limit::RateLimited>().unwrap();
        assert_eq!(typed.reset_at, Some(1_700_000_000));
        assert!(!typed.opaque);
    }

    #[test]
    fn an_opaque_429_downcasts_to_a_typed_error_that_says_so() {
        // #197: sync はこのフラグを見て backoff を強めるので､client は 2 種類
        // を retry 時刻へ潰さず､そのまま通さなければならない｡
        let body = r#"{"title":"Too Many Requests","detail":"Too Many Requests"}"#;
        let error = check_status(
            Endpoint::ListTimeline,
            429,
            body,
            rate_limit::Refusal::Opaque,
            1_000,
        )
        .unwrap_err();
        let typed = error.downcast_ref::<rate_limit::RateLimited>().unwrap();
        assert!(typed.opaque);
        assert_eq!(
            typed.reset_at,
            Some(1_000 + rate_limit::OPAQUE_LIMIT_BACKOFF_SECONDS)
        );
    }

    #[test]
    fn explains_a_rejected_token() {
        let error = check_status(
            Endpoint::ListTimeline,
            401,
            r#"{"title":"Unauthorized"}"#,
            rate_limit::Refusal::Opaque,
            0,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("bearer token was rejected"), "{error}");
    }

    // --- #239: 拒否はどの呼び出しが拒まれたのかを言う ---
    //
    // issue のログは 180 秒ごとの 403 を 30 行と 401 を 100 行以上積み上げた
    // が､どのエンドポイントのものかはどこにも書かれておらず､読み手には
    // 特定できなかった｡下の 3 本はその穴を塞ぐ｡

    #[test]
    fn a_rejected_token_names_the_endpoint_it_was_rejected_for() {
        let error = check_status(
            Endpoint::ListTimeline,
            401,
            r#"{"title":"Unauthorized","detail":"Unauthorized"}"#,
            rate_limit::Refusal::Opaque,
            0,
        )
        .unwrap_err()
        .to_string();
        assert!(error.starts_with("list_timeline: "), "{error}");
    }

    #[test]
    fn a_spend_cap_403_downcasts_to_the_typed_error_carrying_the_endpoint() {
        // #239 のログの前半｡残高切れ (429 + `UsageCapExceeded`) ではなく
        // アカウントの monthly spend cap で､平文の detail でしか名乗らない｡
        let body = r#"{"title":"Forbidden","detail":"Forbidden: Your monthly spend cap has been reached."}"#;
        let error = check_status(
            Endpoint::ListTimeline,
            403,
            body,
            rate_limit::Refusal::Opaque,
            0,
        )
        .unwrap_err();
        let typed = error.downcast_ref::<Denied>().unwrap();
        assert_eq!(typed.endpoint, Endpoint::ListTimeline);
        assert_eq!(typed.denial, Denial::Forbidden);
        assert!(typed.detail.contains("monthly spend cap"), "{typed:?}");
        assert!(
            typed.to_string().contains("list_timeline: 403 Forbidden"),
            "{typed}"
        );
    }

    #[test]
    fn other_statuses_name_the_endpoint_too() {
        let error = check_status(Endpoint::Me, 404, "", rate_limit::Refusal::Opaque, 0)
            .unwrap_err()
            .to_string();
        assert!(error.starts_with("me: 404 Not Found"), "{error}");
    }

    #[test]
    fn falls_back_to_the_raw_body_when_it_is_not_json() {
        let error = check_status(
            Endpoint::Me,
            503,
            "upstream unavailable",
            rate_limit::Refusal::Opaque,
            0,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("upstream unavailable"), "{error}");
    }

    #[test]
    fn reports_an_empty_body_rather_than_nothing() {
        let error = check_status(Endpoint::Me, 500, "", rate_limit::Refusal::Opaque, 0)
            .unwrap_err()
            .to_string();
        assert!(error.contains("empty response body"), "{error}");
    }

    #[test]
    fn treats_5xx_as_retryable_and_4xx_as_not() {
        assert!(is_retryable_status(500));
        assert!(is_retryable_status(599));
        assert!(!is_retryable_status(429));
        assert!(!is_retryable_status(404));
        assert!(!is_retryable_status(200));
    }
}
