//! URL のビルダーと､各エンドポイントが要求するフィールドの定数 (#241)｡
//! 純粋な文字列の組み立てなので､`client` の URL のテストはすべてここにある｡

use super::API_BASE;
use crate::url::Url;

pub(super) fn user_lookup_url(username: &str) -> String {
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
pub(super) fn me_url() -> String {
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
pub(super) fn home_timeline_url(
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
pub(super) fn timeline_url(user_id: &str, max_results: u32, since_id: Option<&str>) -> String {
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
/// 取らない｡その代償は [`super::XClient::list_timeline`] を見よ｡
pub(super) fn list_timeline_url(
    list_id: &str,
    max_results: u32,
    pagination_token: Option<&str>,
) -> String {
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
/// 出力はそのままの [`crate::x_api::TimelineItem`] で､それはまさに同じ
/// レスポンスの形から来ているので､代わりにそれらのフィールドを与える
/// 二つ目の URL ビルダーは無い｡
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
/// (#42) は JSON 出力に `TimelineItem` の `media` フィールドを残してはいる
/// が､メディアに対応させるなら *素の* `attachments.media_keys`/`media.fields`
/// の組も足すことになる｡このビルダーはどちらも持ったことがないからだ — それは
/// 今サポートしていないフィールドのための新機能であって､#104 が timeline の
/// エンドポイントに対して直す repost 固有の expansions の穴ではない｡
pub(super) fn tweets_by_id_url(ids: &str) -> String {
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
pub(super) fn following_url(user_id: &str, pagination_token: Option<&str>) -> String {
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
pub(super) fn list_members_url(list_id: &str, pagination_token: Option<&str>) -> String {
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
pub(super) fn owned_lists_url(user_id: &str, pagination_token: Option<&str>) -> String {
    Url::api(API_BASE)
        .segment("users")
        .segment(user_id)
        .segment("owned_lists")
        .number("max_results", USER_PAGE_SIZE)
        .maybe("pagination_token", pagination_token)
        .build()
}

/// `POST /2/lists/:id/members` (#163) — 追加するアカウントの id は URL では
/// なく JSON ボディ ([`crate::x_api::model::UserIdRequest`]) に乗る｡
pub(super) fn list_members_write_url(list_id: &str) -> String {
    Url::api(API_BASE)
        .segment("lists")
        .segment(list_id)
        .segment("members")
        .build()
}

/// `DELETE /2/lists/:id/members/:user_id` (#163) — ここでは操作対象の id が
/// パスセグメントで､[`delete_repost_url`] と同じ形だ｡
pub(super) fn remove_list_member_url(list_id: &str, member_user_id: &str) -> String {
    Url::api(API_BASE)
        .segment("lists")
        .segment(list_id)
        .segment("members")
        .segment(member_user_id)
        .build()
}

/// `POST /2/tweets` (#14) — 上のどの `GET` とも違い､クエリ文字列は無い｡
pub(super) fn create_post_url() -> String {
    Url::api(API_BASE).segment("tweets").build()
}

/// `POST /2/users/:id/retweets` (#15) — `user_id` はサインイン中のアカウント
/// 自身の id (`/me`､#11)｡対象の post の id は URL ではなく JSON ボディ
/// ([`crate::x_api::model::TweetIdRequest`]) に乗る｡
pub(super) fn create_repost_url(user_id: &str) -> String {
    Url::api(API_BASE)
        .segment("users")
        .segment(user_id)
        .segment("retweets")
        .build()
}

/// `DELETE /2/users/:id/retweets/:source_tweet_id` (#15) — このクレートで
/// *操作対象* の resource 自身の id が､クエリパラメータでも JSON ボディの
/// フィールドでもなく URL のパスセグメントになる唯一のエンドポイントだ｡
pub(super) fn delete_repost_url(user_id: &str, source_tweet_id: &str) -> String {
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
pub(super) fn delete_post_url(post_id: &str) -> String {
    Url::api(API_BASE)
        .segment("tweets")
        .segment(post_id)
        .build()
}

/// `POST /2/users/:id/likes` (#68) — `user_id` はサインイン中のアカウント自身
/// の id (`/me`､#11)｡対象の post の id は repost とまったく同じく､URL ではなく
/// JSON ボディ ([`crate::x_api::model::TweetIdRequest`]) に乗る｡
pub(super) fn create_like_url(user_id: &str) -> String {
    Url::api(API_BASE)
        .segment("users")
        .segment(user_id)
        .segment("likes")
        .build()
}

/// `DELETE /2/users/:id/likes/:tweet_id` (#68) — ここでは操作対象の post の id
/// が URL のパスセグメントで､[`delete_repost_url`] と同じ形だ｡
pub(super) fn delete_like_url(user_id: &str, tweet_id: &str) -> String {
    Url::api(API_BASE)
        .segment("users")
        .segment(user_id)
        .segment("likes")
        .segment(tweet_id)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
