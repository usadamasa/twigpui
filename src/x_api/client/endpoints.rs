//! エンドポイントごとの [`XClient`] のメソッド (#241)｡どれも URL を
//! `urls.rs` から組み､親の `get`/`post`/`delete` か `send_with_retry` で
//! 送り､レスポンスをパースする｡

use anyhow::{Context as _, Result, bail};

use super::XClient;
use super::status::describe_problem;
use super::urls::{
    create_like_url, create_post_url, create_repost_url, delete_like_url, delete_post_url,
    delete_repost_url, following_url, home_timeline_url, list_members_url, list_members_write_url,
    list_timeline_url, me_url, owned_lists_url, remove_list_member_url, timeline_url,
    tweets_by_id_url, user_lookup_url,
};
use crate::paths::Paths;
use crate::rate_limit::Endpoint;
use crate::x_api::model::{
    Draft, ListPageResponse, ListSummary, TimelineItem, TimelineResponse, User, UserLookupResponse,
    UserPageResponse,
};

impl XClient {
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
            self.send_user_id_once(&url, member_user_id, now)
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
            self.send_tweet_id_once(&url, source_tweet_id, now)
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
            self.send_tweet_id_once(&url, tweet_id, now)
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
