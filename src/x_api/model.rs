use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// `data` または `includes.users` の下で返されるユーザーオブジェクト｡
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct User {
    pub id: String,
    pub name: String,
    pub username: String,
    /// アカウントのアバター (#64)｡`user.fields` が `profile_image_url` を
    /// 要求しているからこそ届く｡アバターを持たないアカウントがあり､#64 以前の
    /// フィクスチャはどれもこれを省いているので `#[serde(default)]`｡
    #[serde(default)]
    pub profile_image_url: Option<String>,
}

/// `data` の下で返される post オブジェクト｡
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Post {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub author_id: Option<String>,
    /// この post が参照する post — repost､quote､reply､あるいは (X の API 上)
    /// それらが 1 つの post に組み合わさったもの｡たとえば reply スレッドの中から
    /// tweet を quote する場合 (#13)｡何も参照しない素の post はこのフィールドを
    /// 単に省くし､#13 以前の timeline フィクスチャはどれもそうなっているので
    /// `#[serde(default)]`｡1 つの post が複数のエントリを持つときの優先順位は
    /// [`TimelineResponse::into_items`] を見よ｡
    #[serde(default)]
    pub referenced_tweets: Vec<ReferencedTweetRef>,
    /// エンゲージメントの件数 (#67)｡`tweet.fields` が `public_metrics` を
    /// 要求しているからこそ届く — [`crate::x_api::client`] の URL ビルダーを
    /// 見よ｡それ以前のレスポンス (フィクスチャを含む) や､X が件数の報告を
    /// 拒む post では `None`｡
    #[serde(default)]
    pub public_metrics: Option<PostMetrics>,
    /// `entities` オブジェクト (#70)｡`tweet.fields` が要求しているからこそ
    /// 届く｡このクレートが読むのは `urls` だけだ: post の本文は `t.co` の
    /// 短縮リンクを持っていて､リダイレクトを辿らずに実際の宛先へ至る手段は
    /// `expanded_url` しかない｡
    #[serde(default)]
    pub entities: Option<Entities>,
    /// この post に添付されたメディア (#65) — キーだけ｡メディアオブジェクト
    /// 自体は `includes.media` で届き､著者が `includes.users` から結合される
    /// のと同じやり方で [`post_media`] が結合する｡
    #[serde(default)]
    pub attachments: Option<Attachments>,
}

/// post の `attachments` オブジェクト (#65)｡読むのは `media_keys` だけで､
/// poll もここに入るがサポートしていない｡
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Attachments {
    #[serde(default)]
    pub media_keys: Vec<String>,
}

/// `includes.media` の 1 エントリ (#65)｡timeline のリクエストが要求する
/// `media.fields` に対して返ってくる形だ｡
///
/// `media_key` 以外のフィールドはすべてワイヤ上で optional であり､意図して
/// そうモデリングしてある: X は video や animated GIF で `url` を省き
/// (`preview_image_url` だけが与えられる)､著者が書いていなければ `alt_text`
/// を省き､寸法を省いた例もある｡欠けたフィールドは描画を劣化させるにとどめ､
/// パースを失敗させてはならない — このモジュールの他の部分が従うのと同じ規則だ｡
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Media {
    /// X はこれを `media_key` と綴る｡`Media` という struct の上で
    /// `media_key` というフィールド名は同じ語を二度言うことになるので､
    /// ワイヤ上の綴りから改名した — clippy の `struct_field_names` が異議を
    /// 唱えるし､それは正しい｡
    #[serde(rename = "media_key")]
    pub key: String,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub preview_image_url: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub alt_text: Option<String>,
}

/// twigpui が読む X の `entities` オブジェクトの部分集合 (#70) — URL だけ｡
/// mention や hashtag､annotation もそこに入っているが､serde は列挙して
/// いないものを捨てるので､後から足すのは加算的な変更で済む｡
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Entities {
    #[serde(default)]
    pub urls: Vec<UrlEntity>,
}

/// `entities.urls` の 1 エントリ (#70)｡`expanded_url` は post の本文にある
/// `t.co` 短縮リンクが指す宛先で､`display_url` は X 自身による人間向けの
/// 短縮表示 (`example.com/a/b…`) だ｡
///
/// どちらもワイヤ上では optional だ: X は一部のエンティティ (とくにメディア
/// 添付自身の `t.co`) で `expanded_url` を省くし､行き先の無いリンクは死んだ
/// チップとして描かずに捨てる — [`post_links`] を見よ｡
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct UrlEntity {
    #[serde(default)]
    pub expanded_url: Option<String>,
    #[serde(default)]
    pub display_url: Option<String>,
}

/// post の reply/repost/like の件数 (#67)｡
///
/// X の `public_metrics` オブジェクトから deserialize し､[`TimelineItem`]
/// 自体と同じように timeline のキャッシュファイルへ serialize する｡全フィールド
/// をワイヤ上の綴りから改名してある: X は `reply_count`､`retweet_count`､
/// `like_count` と綴るが､このクレートは "retweet" ではなく "repost" と言うし
/// (#15)､他に何も持たない型では `_count` という接尾辞はノイズに読める｡X が
/// 送ってくるがこのクレートが無視する件数 (`quote_count`､`bookmark_count`､
/// `impression_count`) は単に列挙していない — serde は未知のフィールドを捨てる｡
///
/// これは **post を取得した時点のスナップショット** であって､何も更新しない:
/// 差分リロードは `since_id` を送るので､すでに手元にある post が再び返って
/// くることはない (`cache::splice` を見よ)｡だから行の件数は､それが最初に
/// 届いたときに真だった値を見せている｡行ごとに "as of" のタイムスタンプを
/// 添えて描くことは検討して落とした — 行はすでに密だし､ズレよりも件数が
/// そもそも見えることのほうが重要で､#67 はそこを扱う issue だ｡
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PostMetrics {
    #[serde(default, rename = "reply_count")]
    pub replies: u64,
    #[serde(default, rename = "retweet_count")]
    pub reposts: u64,
    #[serde(default, rename = "like_count")]
    pub likes: u64,
}

/// post の `referenced_tweets` の 1 エントリ (#13) — "this post is a
/// reply/quote/retweet of that other post" という API 自身の注記だ｡1 つの
/// post が複数持ちうるので､`Post::referenced_tweets` は `Option` ではなく
/// `Vec` になっている｡
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ReferencedTweetRef {
    #[serde(rename = "type")]
    pub kind: ReferenceKind,
    pub id: String,
}

/// `referenced_tweets[].type` として認識する値｡
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReferenceKind {
    Retweeted,
    Quoted,
    RepliedTo,
    /// 前方互換のため: 将来の API 改訂で増えた未知の参照型が､post 全体の
    /// パースを失敗させてはならない｡未知の形のキャッシュファイルがエラーでは
    /// なく素直なミスになるのと同じだ (`cache::load_json` を見よ)｡
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Includes {
    #[serde(default)]
    pub users: Vec<User>,
    /// 参照先の post (#13) — repost や quote の実体は `data` ではなく､
    /// id をキーにしてここに入っている｡
    #[serde(default)]
    pub tweets: Vec<Post>,
    /// 添付メディア (#65)｡`media_key` をキーにする — `users` や `tweets` が
    /// すでに使っているのと同じ､サイドテーブルとキーの形だ｡
    #[serde(default)]
    pub media: Vec<Media>,
}

/// 部分的な結果と一緒に X が返す `errors` 配列｡完全な失敗のときに返す
/// problem-details のボディでもある｡
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ApiProblem {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub errors: Vec<ApiProblem>,
}

impl ApiProblem {
    /// 入れ子の `errors` を平坦化した､人間が読める最良の説明｡
    pub(crate) fn message(&self) -> Option<String> {
        if let Some(detail) = &self.detail {
            return Some(match &self.title {
                Some(title) => format!("{title}: {detail}"),
                None => detail.clone(),
            });
        }
        if let Some(title) = &self.title {
            return Some(title.clone());
        }
        if let Some(reason) = &self.reason {
            return Some(reason.clone());
        }
        self.errors.iter().find_map(ApiProblem::message)
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct UserLookupResponse {
    #[serde(default)]
    pub data: Option<User>,
}

/// `POST /2/tweets` のリクエストボディ (#14､#16 で拡張) — post の本文と､
/// 任意の quote 対象｡reply/poll はまだサポートしていない (#12 を見よ)｡
///
/// `quote_tweet_id` は､専用の quote リクエスト型や `Endpoint` の variant を
/// 増やすのではなく､意図して同じエンドポイント/struct を再利用している: X に
/// quote 専用のエンドポイントは無く､X が 1 つのエンドポイントとして扱うものの
/// rate limit の追跡を分ければ､二つ目の誤ったウィンドウを作るだけだからだ｡
/// 普通の post では `skip_serializing_if` がこれを (`null` ですらなく) 完全に
/// 消す — X は迷い込んだ null をそのまま拒否することがある｡
#[derive(Debug, Serialize)]
pub(crate) struct PostTweetRequest<'a> {
    pub text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quote_tweet_id: Option<&'a str>,
    /// #71: この post を reply たらしめるもの｡X が指定する形がそうなので
    /// フラットではなく入れ子にしてある｡普通の post では
    /// `skip_serializing_if` がこれを完全に消す — `quote_tweet_id` と同じ
    /// 扱いで､理由も同じだ｡
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply: Option<ReplyRequest<'a>>,
}

/// [`PostTweetRequest`] の中の `reply` オブジェクト (#71)｡
#[derive(Debug, Serialize)]
pub(crate) struct ReplyRequest<'a> {
    pub in_reply_to_tweet_id: &'a str,
}

/// `POST /2/tweets` に publish を頼む中身 (#71): 本文と､composer が持って
/// いた任意の対象だ｡
///
/// `XClient::create_post` → `post` → `send_post_once` と 3 つの位置引数の
/// `Option` を通すのではなく struct にした: 引数が 3 つになると呼び出し側が
/// 読めなくなるし､そのうち 2 つは `Option<&str>` で､型エラーも出ないまま
/// 黙って入れ替わりうる — 取り違えれば quote が他人の会話にぶら下がる reply
/// になる｡
#[derive(Debug, Clone, Copy)]
pub(crate) struct Draft<'a> {
    pub text: &'a str,
    pub quote_tweet_id: Option<&'a str>,
    pub reply_to_post_id: Option<&'a str>,
}

impl<'a> Draft<'a> {
    /// この draft のリクエストボディ｡
    pub(crate) fn to_request(self) -> PostTweetRequest<'a> {
        PostTweetRequest {
            text: self.text,
            quote_tweet_id: self.quote_tweet_id,
            reply: self.reply_to_post_id.map(|id| ReplyRequest {
                in_reply_to_tweet_id: id,
            }),
        }
    }
}

/// `POST /2/users/:id/retweets` (#15) と `POST /2/users/:id/likes` (#68) が
/// 共有するリクエストボディ — 操作対象の post の id だ｡`user_id` (誰の repost
/// や like を作るのか) はここではなく URL に乗る｡同一の型を 2 つ持たず 1 つに
/// した: X はどちらにも同じ 1 フィールドのボディを指定しているので､二つ目の
/// コピーはずれる余地にしかならない｡
#[derive(Debug, Serialize)]
pub(crate) struct TweetIdRequest<'a> {
    pub tweet_id: &'a str,
}

/// `POST /2/lists/:id/members` (#163) が取るリクエストボディ — 追加する
/// アカウントの id だ｡list 自身の id は URL に乗る｡[`TweetIdRequest`] と
/// 同じ形だが別の型なのは､あちらが共有されているのと同じ理由による:
/// フィールド名が違えば別のボディだ｡
///
/// **spec 由来､未検証｡** #163 は `/2/lists/:id/members` にリクエストを 1 つも
/// 使わずに作ったので､このフィールド名は 200 ではなく docs.x.com から来ている｡
/// `x-api-endpoints` は､両者が食い違うことは織り込んでおくべきほど多いと
/// はっきり書いている｡
#[derive(Debug, Serialize)]
pub(crate) struct UserIdRequest<'a> {
    pub user_id: &'a str,
}

/// ユーザーの 1 ページ｡`GET /2/users/:id/following` と
/// `GET /2/lists/:id/members` が返す形だ (#163): アカウントそのものと､次の
/// ページへのカーソル｡
///
/// `data` が `#[serde(default)]` なのは､空のページでどちらのエンドポイントも
/// `[]` を送らずフィールドごと省くからだ — 誰もフォローしていないアカウント､
/// メンバーのいない list であり､最初の sync はまさにその状態から始まる｡
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct UserPageResponse {
    #[serde(default)]
    pub data: Vec<User>,
    #[serde(default)]
    pub meta: Meta,
}

impl UserPageResponse {
    /// この次のページへのカーソル｡末尾では `None`｡
    pub(crate) fn next_token(&self) -> Option<&str> {
        self.meta.next_token.as_deref()
    }
}

/// `GET /2/users/:id/owned_lists` が返す 1 件の List (#164): picker が
/// セグメントに名前を付けるのに要るものだけだ｡キャッシュもフィクスチャもこれを
/// 書き出すので `Serialize` も付けてある｡
///
/// `name` を必須にせず default にしてある: spec はこれを既定フィールドに
/// 挙げているが､`x-api-endpoints` は spec と API が食い違ってきた記録であり､
/// 名前の無い list 1 件でパースに失敗する picker は他のすべての list を
/// 道連れにするからだ｡描画側は id にフォールバックする
/// (`ui::list_picker::segment_label`)｡
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct ListSummary {
    pub id: String,
    #[serde(default)]
    pub name: String,
}

/// `GET /2/users/:id/owned_lists` のレスポンス全体 (#164)｡list を 1 つも
/// 持たないアカウントでは `data` は空ではなく不在になる — [`TimelineResponse`]
/// がすでに許容しているのと同じ形だ｡
#[derive(Debug, Default, Deserialize)]
pub(crate) struct ListPageResponse {
    #[serde(default)]
    pub data: Vec<ListSummary>,
    #[serde(default)]
    pub meta: Meta,
}

impl ListPageResponse {
    /// この次のページへのカーソル｡末尾では `None`｡
    pub(crate) fn next_token(&self) -> Option<&str> {
        self.meta.next_token.as_deref()
    }
}

/// `data` と一緒に返るページネーション情報｡このクレートに関わるのは
/// `next_token` だけだ — `x_api::client::home_timeline_url` が次の (より古い)
/// ページを取るために `pagination_token` として送り返すカーソルであり､#11 の
/// "Load older" ボタンを動かしている｡
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Meta {
    #[serde(default)]
    pub next_token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct TimelineResponse {
    #[serde(default)]
    pub data: Vec<Post>,
    #[serde(default)]
    pub includes: Includes,
    #[serde(default)]
    pub meta: Meta,
}

/// 著者と一緒に平坦化し､そのまま描画できる形にした post｡
///
/// #9 以降 `Serialize`/`Deserialize` を付けている: これが timeline のキャッシュ
/// ファイルへ永続化される形そのものなので､キャッシュ専用の型は要らない｡
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TimelineItem {
    pub id: String,
    pub text: String,
    pub created_at: Option<String>,
    pub author_name: String,
    pub author_username: String,
    /// repost のときだけ入る (#13): それを timeline に浮かせた人の screen
    /// name であり､本文の上の小さな行として出す前提だ — 上の 4 つの
    /// フィールドと違い､ここが `Some` になると本文は *元の* post を指す｡
    /// #13 以前に書かれたキャッシュファイルがこれを単に欠いたまま (`None`)
    /// きれいに deserialize できるよう `#[serde(default)]`｡
    #[serde(default)]
    pub reposted_by: Option<String>,
    /// quote のときに入る — [`TimelineResponse::into_items`] に書いた優先順位
    /// により､quote の repost も含む (#13): quote された post であり､本文の
    /// 下にカードとして描く前提だ｡`reposted_by` と同じキャッシュ互換の理由で
    /// `#[serde(default)]`｡
    #[serde(default)]
    pub quoted: Option<QuotedPost>,
    /// reply のときに入る (#12): この post が誰に返信しているかと､その親の
    /// post id — `ui.rs` の "Show thread" の遡りが起点にするアンカーだ｡
    /// 親は (展開されていればその著者も) #13 の
    /// `referenced_tweets.id`/`.author_id` の expansion によりすでに
    /// `includes` にあるので､追加のリクエスト費用ゼロで埋まる｡
    /// `reposted_by`/`quoted` と同じキャッシュ互換の理由で `#[serde(default)]`｡
    #[serde(default)]
    pub replied_to: Option<RepliedTo>,
    /// 本文の下に出す件数 (#67)｡本文が実際に抱えているほうの post のもので､
    /// この行が repost なら外側の post ではなく元の post のものだ｡
    /// レスポンスが持っていなければ `None`｡`reposted_by`/`quoted`/`replied_to`
    /// と同じく､#67 以前に書かれたキャッシュファイルがきれいに deserialize
    /// できるよう `#[serde(default)]`｡
    #[serde(default)]
    pub metrics: Option<PostMetrics>,
    /// この post の本文にあるリンク (#70)｡本文が持つ `t.co` の短縮リンクを
    /// 展開したもので､本文が実際に抱えているほうの post のもの — repost なら
    /// 元の post のリンクになる｡リンクの無い post では空だ｡上の `metrics` と
    /// 同じく､#70 以前に書かれたキャッシュファイルがきれいに deserialize
    /// できるよう `#[serde(default)]`｡
    #[serde(default)]
    pub links: Vec<PostLink>,
    /// 本文が抱えている post の著者のアバター URL (#64) — repost なら元の
    /// post の著者のもので､`author_name`/`author_username` と揃う｡著者が
    /// 展開されなかったか､アバターを持たない場合は `None`｡上の `links` と
    /// 同じキャッシュ互換の理由で `#[serde(default)]`｡
    #[serde(default)]
    pub author_avatar_url: Option<String>,
    /// repost の行のとき (#52): この行がすでに表示している本文と著者の持ち主､
    /// つまり *元の* post の id｡
    ///
    /// 上の `id` は retweet というアクティビティ自身の id のままだ — 行と
    /// キャッシュ､`replied_to` のスレッド遡りのキーがそれだからだ — が､
    /// write のエンドポイント (`POST /2/users/:id/retweets`､`POST /2/tweets`
    /// の `quote_tweet_id`､`POST /2/users/:id/likes`) はどれも元の post に
    /// 作用する｡このフィールドが無いあいだ #15､#16､#68 はどれも､誤った id を
    /// 送る危険を冒すより repost の行でボタンを出さずにいるほかなかった;
    /// [`action_post_id`] を見よ｡値は `referenced_tweets` の `retweeted`
    /// エントリから埋める｡#13 の結合がすでに手にしているので追加のリクエストは
    /// 要らない｡repost でない post ではすべて `None`｡
    ///
    /// `#[serde(default)]` はいつもの理由による: `cache::load_json` はパース
    /// 失敗を黙ったミスとして扱うので､ここで属性が欠けると全ユーザーの
    /// キャッシュを黙って捨て､その費用で取り直すことになる｡
    #[serde(default)]
    pub original_post_id: Option<String>,
    /// 本文が抱えているほうの post に添付されたメディア (#65) — repost なら
    /// 元の post のもので､その本文と揃う｡添付の無い post では空だ｡#65 以前に
    /// 書かれたキャッシュファイルがきれいに deserialize できるよう
    /// `#[serde(default)]`｡
    #[serde(default)]
    pub media: Vec<PostMedia>,
}

/// 添付された画像 1 枚､あるいは video や GIF のサムネイルを､描画のために
/// 平坦化したもの (#65)｡
///
/// `url` は実際に表示できるものだ: photo なら画像そのもの､video や
/// animated GIF なら静止画の `preview_image_url` — どちらもこのアプリは
/// 再生しない (意図してスコープ外にしてある｡行はサムネイルと､それが何かを
/// 言うバッジを出す)｡表示できるものが何も無いエントリは､穴として描かずに
/// [`post_media`] が捨てる｡
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PostMedia {
    pub url: String,
    /// X 自身の `type` 文字列をそのまま持つ (`photo`､`video`､
    /// `animated_gif`､あるいはもっと新しい何か)｡enum にはパースしない:
    /// これが決めるのはどのバッジを出すかだけで､未知の値は timeline 全体の
    /// パースを失敗させるのではなくバッジを出さないのが正しい｡
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub alt_text: Option<String>,
}

/// `item` に対して write のエンドポイントが作用すべき post の id (#52):
/// repost の行なら元の post､そうでなければ行自身の id だ｡
///
/// 同じ `unwrap_or` を 4 箇所の呼び出し側に書かず関数 1 つにしたので､
/// "repost はどの id に作用するのか" の答えはコードベースにちょうど 1 つある｡
/// 入れ子の参照に特別扱いは要らない: `original_post_id` は `retweeted` の
/// 参照から入るし､*quote の* repost もその quote post の repost であって､
/// それに作用することは行が見せているものに作用することだ｡
pub(crate) fn action_post_id(item: &TimelineItem) -> &str {
    item.original_post_id.as_deref().unwrap_or(&item.id)
}

/// post の本文から開けるリンク 1 件 (#70)｡[`UrlEntity`] を平坦化し､両側を
/// 解決してある: `url` は実際の行き先､`label` はそれに対して見せるものだ｡
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PostLink {
    pub url: String,
    pub label: String,
}

/// reply が誰に返信しているか (#12)｡repost や quote の元 post と同じやり方で
/// `includes` から結合する｡親の著者が解決できなかったとき — 削除済み､
/// 非公開､あるいは単に展開されていない — は､reply の文脈をまるごと隠すのでは
/// なく､元 post を欠いた repost に対する [`build_item`] の既存の作法に倣って
/// `author_name`/`author_username` を空にする (フィールドごと `None` には
/// しない)｡
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RepliedTo {
    pub post_id: String,
    pub author_name: String,
    pub author_username: String,
}

/// quote された post｡著者と一緒に平坦化し､quote の出どころとして
/// [`TimelineItem`] に埋め込む (#13)｡
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct QuotedPost {
    pub author_name: String,
    pub author_username: String,
    pub text: String,
    /// quote された post 自身の添付メディア (#123)｡repost のものと同じやり方
    /// で結合する — それまで quote のカードは本文だけで､画像こそが quote の
    /// 眼目だった場合にそれがまったく出なかった｡#9 より後に足した他の
    /// フィールドと同じキャッシュ互換の理由で `#[serde(default)]`｡
    #[serde(default)]
    pub media: Vec<PostMedia>,
}

impl TimelineResponse {
    /// レスポンスが持っていれば `meta.next_token` — 次の (より古い) ページを
    /// 取るためのカーソル (#11)｡[`Self::into_items`] が所有権を取る前に
    /// 呼び出し側が確認できるよう､消費せず `&self` で読む｡
    pub(crate) fn next_token(&self) -> Option<&str> {
        self.meta.next_token.as_deref()
    }

    /// 各 post を `includes.users` の著者と結合し､さらに — #13 —
    /// `includes.tweets` から参照先とも結合する｡
    ///
    /// post の `referenced_tweets` が複数のエントリを持つときの優先順位
    /// (X はこれを許す — たとえば reply スレッドの中から tweet を quote すると
    /// 同じ post に `quoted` と `replied_to` の両方が付く): `retweeted` が
    /// `quoted` に勝ち､`quoted` が `replied_to` に勝つ｡repost は描画される
    /// 本文を元の post で丸ごと置き換えるので､本文を置き換えず足すだけの
    /// quote カードより優先する｡素の reply 参照はまだ自前の描画を持たない
    /// (スレッド表示は #12) ので､どちらにも勝たない｡これがどこで適用されるかは
    /// [`build_item`] と [`quote_of`] を見よ｡
    ///
    /// 著者が expansion に無い post もそのまま描画し､著者のフィールドは空に
    /// する — 落とせば内容を黙って隠すことになる｡元の post が `includes` に
    /// 無い repost (削除済み､非公開､あるいは単に展開されていない) も同じで､
    /// 空の行にするのではなく､外側の post 自身の — 切り詰められているかも
    /// しれない — 本文にフォールバックする｡
    pub(crate) fn into_items(self) -> Vec<TimelineItem> {
        let users: HashMap<&str, &User> = self
            .includes
            .users
            .iter()
            .map(|u| (u.id.as_str(), u))
            .collect();
        let referenced: HashMap<&str, &Post> = self
            .includes
            .tweets
            .iter()
            .map(|post| (post.id.as_str(), post))
            .collect();

        // #65: メディアのサイドテーブル｡上の 2 つと同じやり方でキーを張る｡
        let media: HashMap<&str, &Media> = self
            .includes
            .media
            .iter()
            .map(|item| (item.key.as_str(), item))
            .collect();

        self.data
            .iter()
            .map(|post| build_item(post, &users, &referenced, &media))
            .collect()
    }
}

/// `includes.users` から取る post の著者の name/username｡著者 id が無いか
/// 展開されていなければ空文字の組になる — [`build_item`] と [`quote_of`] が
/// 埋める著者フィールドすべての背後にある共通のルックアップだ｡
fn author_fields(post: &Post, users: &HashMap<&str, &User>) -> (String, String, Option<String>) {
    let author = post
        .author_id
        .as_deref()
        .and_then(|id| users.get(id).copied());
    (
        author.map(|u| u.name.clone()).unwrap_or_default(),
        author.map(|u| u.username.clone()).unwrap_or_default(),
        author.and_then(|u| u.profile_image_url.clone()),
    )
}

/// post 1 件を著者と結合し､他の post を参照していれば —
/// [`TimelineResponse::into_items`] に書いた優先順位に従って — その参照先とも
/// 結合する｡
fn build_item(
    post: &Post,
    users: &HashMap<&str, &User>,
    referenced: &HashMap<&str, &Post>,
    media: &HashMap<&str, &Media>,
) -> TimelineItem {
    let (author_name, author_username, author_avatar_url) = author_fields(post, users);
    let mut item = TimelineItem {
        id: post.id.clone(),
        text: post.text.clone(),
        created_at: post.created_at.clone(),
        author_name,
        author_username,
        reposted_by: None,
        quoted: None,
        replied_to: None,
        metrics: post.public_metrics,
        links: post_links(post),
        author_avatar_url,
        original_post_id: None,
        media: post_media(post, media),
    };

    if let Some(retweet_ref) = post
        .referenced_tweets
        .iter()
        .find(|r| r.kind == ReferenceKind::Retweeted)
    {
        // 外側の post 自身の著者は repost した人 — 下で元の post の著者に
        // 上書きされる前に捕まえておく｡
        item.reposted_by = Some(item.author_username.clone());
        // #52: write のエンドポイントすべてが要る id｡ここでは追加費用なしに
        // 手に入る — 参照されたのは id であって返ってきたものではない以上､
        // 元の post 自体が展開されたかどうかに関わらず入れる｡
        item.original_post_id = Some(retweet_ref.id.clone());

        if let Some(original) = referenced.get(retweet_ref.id.as_str()).copied() {
            let (author_name, author_username, avatar) = author_fields(original, users);
            item.text.clone_from(&original.text);
            item.author_name = author_name;
            item.author_username = author_username;
            item.author_avatar_url = avatar;
            // quote の repost — あるいは reply の repost — が持つ文脈は､
            // 本文として今表示されている元の post のものであって､外側の post
            // にある (すでに消費した) retweet 参照のものではない｡
            item.quoted = quote_of(original, users, referenced, media);
            item.replied_to = reply_target(original, users, referenced);
            // #67: 本文が元の post のものなら､その下の件数も元の post の
            // ものでなければならない — 外側の repost は自前の件数を持つ｡
            item.metrics = original.public_metrics;
            // #70: リンクは本文に属し､その本文は元の post のものだ｡
            item.links = post_links(original);
            // #65: 添付メディアも同じだ｡
            item.media = post_media(original, media);
        } else {
            // 元の post が `includes` に無い — 行を空にするのではなく､上で
            // すでに入れた外側の post 自身の (切り詰められた `RT @user: …` かも
            // しれない) 本文を残す｡ただし著者フィールドは､著者が展開され
            // なかった post がすでにそうしているのと同じように落とす: 誰が
            // repost したかは分かるが､誰が書いたかは分からない｡
            item.author_name = String::new();
            item.author_username = String::new();
            item.author_avatar_url = None;
            // 上の著者フィールドと同じ理由で空にする: 外側の repost 自身の
            // 件数は元の post のものではない (#67)｡
            item.metrics = None;
            item.links.clear();
            item.media.clear();
        }
    } else {
        if post
            .referenced_tweets
            .iter()
            .any(|r| r.kind == ReferenceKind::Quoted)
        {
            item.quoted = quote_of(post, users, referenced, media);
        }
        // #12: reply は (quote が付いていてもいなくても) 誰に返信しているかを
        // 見せる｡追加のリクエスト費用はゼロだ — 親は #13 の expansion により
        // すでに `includes` にある｡
        item.replied_to = reply_target(post, users, referenced);
    }

    item
}

/// `post` が `replied_to` の参照を持つなら､誰に返信しているか (#12) —
/// [`quote_of`] が quote の出どころを結合するのと同じやり方で `includes` から
/// 結合する｡`None` になるのは `post` が `replied_to` 参照をまったく持たない
/// ときだけだ｡親が `includes` に無い reply (削除済み､非公開､あるいは単に
/// 展開されていない) でも著者フィールドを空にして `Some` を返す — `ui.rs` の
/// "Show thread" が起点にするには id だけで足りるし､reply の文脈をまるごと
/// 落とせば実在するものを隠すことになる｡
fn reply_target(
    post: &Post,
    users: &HashMap<&str, &User>,
    referenced: &HashMap<&str, &Post>,
) -> Option<RepliedTo> {
    let reply_ref = post
        .referenced_tweets
        .iter()
        .find(|r| r.kind == ReferenceKind::RepliedTo)?;
    let (author_name, author_username, _avatar) = referenced
        .get(reply_ref.id.as_str())
        .map(|parent| author_fields(parent, users))
        .unwrap_or_default();
    Some(RepliedTo {
        post_id: reply_ref.id.clone(),
        author_name,
        author_username,
    })
}

/// `post` の本文から開けるリンク (#70)｡
///
/// `expanded_url` の無いエンティティは捨てる: それが無ければ開けるのは本文に
/// すでにある `t.co` の短縮リンクだけで､短縮リンクを言い直すだけのチップは
/// チップが無いより悪い｡重複も捨てる — 同じリンクが二度出ると X はエンティティ
/// を繰り返す — 最初の 1 件を残すので順序は本文と一致する｡X が `display_url`
/// を送ってこないとき､ラベルは URL 自体にフォールバックする｡
fn post_links(post: &Post) -> Vec<PostLink> {
    let mut seen: Vec<PostLink> = Vec::new();
    let Some(entities) = post.entities.as_ref() else {
        return seen;
    };
    for entity in &entities.urls {
        let Some(url) = entity.expanded_url.as_ref() else {
            continue;
        };
        if seen.iter().any(|link| &link.url == url) {
            continue;
        }
        seen.push(PostLink {
            url: url.clone(),
            label: entity.display_url.clone().unwrap_or_else(|| url.clone()),
        });
    }
    seen
}

/// `post` の添付メディア (#65)｡post が持つキーで `includes.media` から
/// 結合する｡
///
/// 対応するエントリの無いキーは飛ばす — 呼び出し側が見てはいけないメディアを
/// X が省くことがある — 表示できるものが何も無いエントリも同様だ: photo なら
/// `url`､video や animated GIF なら `preview_image_url` (どちらもこのアプリは
/// 再生しない)｡順序は post 自身の `media_keys` に従い､それは著者が添付した
/// 順序だ｡
fn post_media(post: &Post, media: &HashMap<&str, &Media>) -> Vec<PostMedia> {
    let Some(attachments) = post.attachments.as_ref() else {
        return Vec::new();
    };
    attachments
        .media_keys
        .iter()
        .filter_map(|key| media.get(key.as_str()).copied())
        .filter_map(|item| {
            let url = item
                .url
                .clone()
                .or_else(|| item.preview_image_url.clone())?;
            Some(PostMedia {
                url,
                kind: item.kind.clone(),
                width: item.width,
                height: item.height,
                alt_text: item.alt_text.clone(),
            })
        })
        .collect()
}

/// `post` が `quoted` の参照を持ち､その post が `includes.tweets` にあれば､
/// quote 先の post｡どちらの理由で `None` になっても [`build_item`] が
/// フォールバックしてよい正当な結果だ (カードを出さないだけで､エラーではない)
/// — quote された post は削除済み､非公開､あるいは単に expansion に無いことが
/// ありうる｡
fn quote_of(
    post: &Post,
    users: &HashMap<&str, &User>,
    referenced: &HashMap<&str, &Post>,
    media: &HashMap<&str, &Media>,
) -> Option<QuotedPost> {
    let quote_ref = post
        .referenced_tweets
        .iter()
        .find(|r| r.kind == ReferenceKind::Quoted)?;
    let quoted_post = referenced.get(quote_ref.id.as_str())?;
    let (author_name, author_username, _avatar) = author_fields(quoted_post, users);
    Some(QuotedPost {
        author_name,
        author_username,
        text: quoted_post.text.clone(),
        media: post_media(quoted_post, media),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMELINE_JSON: &str = r#"{
      "data": [
        {
          "id": "1700000000000000001",
          "text": "hello from the timeline",
          "created_at": "2026-08-16T09:00:00.000Z",
          "author_id": "2244994945"
        },
        {
          "id": "1700000000000000002",
          "text": "a post whose author was not expanded",
          "created_at": "2026-08-16T08:00:00.000Z",
          "author_id": "9999999999"
        }
      ],
      "includes": {
        "users": [
          {
            "id": "2244994945",
            "name": "Developers",
            "username": "XDevelopers",
            "profile_image_url": "https://pbs.twimg.com/profile_images/x.jpg"
          }
        ]
      },
      "meta": { "result_count": 2, "next_token": "abc123" }
    }"#;

    #[test]
    fn joins_posts_with_their_authors() {
        let response: TimelineResponse = serde_json::from_str(TIMELINE_JSON).unwrap();
        let items = response.into_items();

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].text, "hello from the timeline");
        assert_eq!(items[0].author_name, "Developers");
        assert_eq!(items[0].author_username, "XDevelopers");
        assert_eq!(
            items[0].created_at.as_deref(),
            Some("2026-08-16T09:00:00.000Z")
        );
    }

    #[test]
    fn keeps_posts_whose_author_is_missing() {
        let response: TimelineResponse = serde_json::from_str(TIMELINE_JSON).unwrap();
        let items = response.into_items();

        assert_eq!(items[1].id, "1700000000000000002");
        assert_eq!(items[1].author_name, "");
        assert_eq!(items[1].author_username, "");
    }

    #[test]
    fn next_token_is_read_from_meta() {
        // #11: "Load older" が `pagination_token` として送り直すカーソルだ｡
        let response: TimelineResponse = serde_json::from_str(TIMELINE_JSON).unwrap();
        assert_eq!(response.next_token(), Some("abc123"));
    }

    #[test]
    fn next_token_is_none_when_meta_omits_it() {
        let response: TimelineResponse =
            serde_json::from_str(r#"{"meta":{"result_count":0}}"#).unwrap();
        assert_eq!(response.next_token(), None);
    }

    #[test]
    fn next_token_is_none_when_meta_is_absent_entirely() {
        let response: TimelineResponse =
            serde_json::from_str(r#"{"data":[{"id":"1","text":"orphan"}]}"#).unwrap();
        assert_eq!(response.next_token(), None);
    }

    #[test]
    fn parses_an_empty_timeline() {
        let response: TimelineResponse =
            serde_json::from_str(r#"{"meta":{"result_count":0}}"#).unwrap();
        assert!(response.into_items().is_empty());
    }

    #[test]
    fn parses_a_user_lookup() {
        let response: UserLookupResponse = serde_json::from_str(
            r#"{"data":{"id":"2244994945","name":"Developers","username":"XDevelopers"}}"#,
        )
        .unwrap();
        assert_eq!(response.data.unwrap().id, "2244994945");
    }

    #[test]
    fn reads_a_problem_details_body() {
        let problem: ApiProblem = serde_json::from_str(
            r#"{"title":"Unauthorized","detail":"Unauthorized","status":401}"#,
        )
        .unwrap();
        assert_eq!(
            problem.message().as_deref(),
            Some("Unauthorized: Unauthorized")
        );
    }

    #[test]
    fn falls_back_from_detail_to_title_to_reason() {
        let title_only: ApiProblem = serde_json::from_str(r#"{"title":"Unauthorized"}"#).unwrap();
        assert_eq!(title_only.message().as_deref(), Some("Unauthorized"));

        let reason_only: ApiProblem =
            serde_json::from_str(r#"{"reason":"client-not-enrolled"}"#).unwrap();
        assert_eq!(
            reason_only.message().as_deref(),
            Some("client-not-enrolled")
        );

        // 3 つのどれも無いボディは報告するものが無く､呼び出し側は代わりに
        // 生のテキストにフォールバックする｡
        let empty: ApiProblem = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.message(), None);
    }

    #[test]
    fn skips_nested_errors_that_say_nothing() {
        let problem: ApiProblem =
            serde_json::from_str(r#"{"errors":[{},{"title":"Not Found Error"}]}"#).unwrap();
        assert_eq!(problem.message().as_deref(), Some("Not Found Error"));
    }

    #[test]
    fn keeps_a_post_whose_author_id_is_absent_entirely() {
        let response: TimelineResponse =
            serde_json::from_str(r#"{"data":[{"id":"1","text":"orphan"}]}"#).unwrap();
        let items = response.into_items();

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].author_username, "");
        assert_eq!(items[0].created_at, None);
    }

    // --- #13: repost と quote ---

    const REPOST_JSON: &str = r#"{
      "data": [
        {
          "id": "1800000000000000001",
          "text": "RT @XDevelopers: hello from the timeline",
          "created_at": "2026-08-16T10:00:00.000Z",
          "author_id": "3000000000000000001",
          "referenced_tweets": [
            { "type": "retweeted", "id": "1700000000000000001" }
          ]
        }
      ],
      "includes": {
        "users": [
          { "id": "3000000000000000001", "name": "Reposter One", "username": "reposter1" },
          { "id": "2244994945", "name": "Developers", "username": "XDevelopers" }
        ],
        "tweets": [
          {
            "id": "1700000000000000001",
            "text": "hello from the timeline",
            "created_at": "2026-08-16T09:00:00.000Z",
            "author_id": "2244994945"
          }
        ]
      }
    }"#;

    #[test]
    fn a_repost_renders_as_the_original_posts_author_and_text() {
        let response: TimelineResponse = serde_json::from_str(REPOST_JSON).unwrap();
        let items = response.into_items();

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "hello from the timeline");
        assert_eq!(items[0].author_name, "Developers");
        assert_eq!(items[0].author_username, "XDevelopers");
        assert_eq!(items[0].reposted_by.as_deref(), Some("reposter1"));
        assert_eq!(items[0].quoted, None);
    }

    const QUOTE_JSON: &str = r#"{
      "data": [
        {
          "id": "1800000000000000002",
          "text": "this is worth reading",
          "created_at": "2026-08-16T11:00:00.000Z",
          "author_id": "3000000000000000001",
          "referenced_tweets": [
            { "type": "quoted", "id": "1700000000000000001" }
          ]
        }
      ],
      "includes": {
        "users": [
          { "id": "3000000000000000001", "name": "Reposter One", "username": "reposter1" },
          { "id": "2244994945", "name": "Developers", "username": "XDevelopers" }
        ],
        "tweets": [
          {
            "id": "1700000000000000001",
            "text": "hello from the timeline",
            "created_at": "2026-08-16T09:00:00.000Z",
            "author_id": "2244994945"
          }
        ]
      }
    }"#;

    /// *quote された* 側の post がメディアを持つ quote (#123)｡repost に対して
    /// `referenced_tweets.id.attachments.media_keys` が生む形と同じだ (#104)
    /// — どちらも `referenced_tweets` を通って内容に届くので､この expansion は
    /// 両方をカバーする｡
    const QUOTE_WITH_MEDIA_JSON: &str = r#"{
      "data": [
        {
          "id": "1800000000000000003",
          "text": "look at this one",
          "created_at": "2026-08-16T11:00:00.000Z",
          "author_id": "3000000000000000001",
          "referenced_tweets": [
            { "type": "quoted", "id": "1700000000000000003" }
          ]
        }
      ],
      "includes": {
        "users": [
          { "id": "3000000000000000001", "name": "Reposter One", "username": "reposter1" },
          { "id": "2244994945", "name": "Developers", "username": "XDevelopers" }
        ],
        "tweets": [
          {
            "id": "1700000000000000003",
            "text": "the quoted post",
            "created_at": "2026-08-16T09:00:00.000Z",
            "author_id": "2244994945",
            "attachments": { "media_keys": ["k-quoted"] }
          }
        ],
        "media": [
          {
            "media_key": "k-quoted",
            "type": "photo",
            "url": "https://pbs.twimg.com/media/quoted.jpg",
            "alt_text": "the quoted post's photo"
          }
        ]
      }
    }"#;

    #[test]
    fn a_quoted_post_carries_its_own_media() {
        // #123: カードは本文だけを見せていたので､quote の眼目 *そのもの* で
        // あった画像がまったく出なかった｡
        let response: TimelineResponse = serde_json::from_str(QUOTE_WITH_MEDIA_JSON).unwrap();
        let items = response.into_items();

        let quoted = items[0].quoted.as_ref().expect("the quote card's post");
        assert_eq!(quoted.text, "the quoted post");
        assert_eq!(quoted.media.len(), 1);
        assert_eq!(
            quoted.media[0].url,
            "https://pbs.twimg.com/media/quoted.jpg"
        );
        assert_eq!(
            quoted.media[0].alt_text.as_deref(),
            Some("the quoted post's photo")
        );
    }

    #[test]
    fn the_quoting_post_does_not_borrow_the_quoted_posts_media() {
        // ここでは外側の post は自前の添付を持たない｡自身のグリッドはカードの
        // ものを写すのではなく空のままでなければならない｡
        let response: TimelineResponse = serde_json::from_str(QUOTE_WITH_MEDIA_JSON).unwrap();
        let items = response.into_items();

        assert!(items[0].media.is_empty());
    }

    #[test]
    fn a_quote_without_media_leaves_the_card_empty() {
        let response: TimelineResponse = serde_json::from_str(QUOTE_JSON).unwrap();
        let items = response.into_items();

        assert!(items[0].quoted.as_ref().expect("a quote").media.is_empty());
    }

    #[test]
    fn a_quote_attaches_the_quoted_post_without_replacing_the_body() {
        let response: TimelineResponse = serde_json::from_str(QUOTE_JSON).unwrap();
        let items = response.into_items();

        assert_eq!(items[0].text, "this is worth reading");
        assert_eq!(items[0].author_username, "reposter1");
        assert_eq!(items[0].reposted_by, None);
        let quoted = items[0].quoted.as_ref().unwrap();
        assert_eq!(quoted.text, "hello from the timeline");
        assert_eq!(quoted.author_name, "Developers");
        assert_eq!(quoted.author_username, "XDevelopers");
    }

    #[test]
    fn a_reply_reference_does_not_change_the_body_but_is_surfaced_as_replied_to() {
        let json = r#"{
          "data": [
            {
              "id": "1800000000000000003",
              "text": "agreed",
              "author_id": "3000000000000000001",
              "referenced_tweets": [
                { "type": "replied_to", "id": "1700000000000000001" }
              ]
            }
          ],
          "includes": {
            "users": [
              { "id": "3000000000000000001", "name": "Reposter One", "username": "reposter1" }
            ]
          }
        }"#;
        let response: TimelineResponse = serde_json::from_str(json).unwrap();
        let items = response.into_items();

        // reply 自身の本文と著者はそのままだ — repost と違い､それらを置き換える
        // ことはない (#13 の先例｡#12 でも変わらない)｡
        assert_eq!(items[0].text, "agreed");
        assert_eq!(items[0].reposted_by, None);
        assert_eq!(items[0].quoted, None);
        // #12: ここでは親自体が `includes.tweets` に無いが､reply は *出す* —
        // ("Show thread" のための) id だけでも残す価値があり､文脈をまるごと
        // 落とすのではなく著者フィールドを空にする｡
        let replied_to = items[0].replied_to.as_ref().unwrap();
        assert_eq!(replied_to.post_id, "1700000000000000001");
        assert_eq!(replied_to.author_name, "");
        assert_eq!(replied_to.author_username, "");
    }

    #[test]
    fn a_reply_shows_who_it_is_replying_to_when_the_parent_is_expanded() {
        // #12: 親の著者は #13 の `referenced_tweets.id.author_id` expansion の
        // おかげですでに `includes` にあるので､追加のリクエストは要らない｡
        let json = r#"{
          "data": [
            {
              "id": "1800000000000000006",
              "text": "agreed",
              "author_id": "3000000000000000001",
              "referenced_tweets": [
                { "type": "replied_to", "id": "1700000000000000001" }
              ]
            }
          ],
          "includes": {
            "users": [
              { "id": "3000000000000000001", "name": "Reposter One", "username": "reposter1" },
              { "id": "2244994945", "name": "Developers", "username": "XDevelopers" }
            ],
            "tweets": [
              {
                "id": "1700000000000000001",
                "text": "hello from the timeline",
                "author_id": "2244994945"
              }
            ]
          }
        }"#;
        let response: TimelineResponse = serde_json::from_str(json).unwrap();
        let items = response.into_items();

        let replied_to = items[0].replied_to.as_ref().unwrap();
        assert_eq!(replied_to.post_id, "1700000000000000001");
        assert_eq!(replied_to.author_name, "Developers");
        assert_eq!(replied_to.author_username, "XDevelopers");
    }

    #[test]
    fn a_non_reply_post_has_no_reply_target() {
        let response: TimelineResponse = serde_json::from_str(TIMELINE_JSON).unwrap();
        let items = response.into_items();
        assert_eq!(items[0].replied_to, None);
    }

    #[test]
    fn a_repost_of_a_reply_carries_the_originals_reply_target() {
        // #13 の "repost of a quote" の先例に倣う: 本文が元の post のものに
        // なった以上､見せる価値のある reply の文脈は *元の post の* ものだ —
        // 外側の retweet 参照はすでに使い切っている｡
        let json = r#"{
          "data": [
            {
              "id": "1800000000000000007",
              "text": "RT @quoter1: agreed",
              "author_id": "3000000000000000001",
              "referenced_tweets": [
                { "type": "retweeted", "id": "1700000000000000003" }
              ]
            }
          ],
          "includes": {
            "users": [
              { "id": "3000000000000000001", "name": "Reposter One", "username": "reposter1" },
              { "id": "4000000000000000001", "name": "Quote Author", "username": "quoter1" },
              { "id": "2244994945", "name": "Developers", "username": "XDevelopers" }
            ],
            "tweets": [
              {
                "id": "1700000000000000003",
                "text": "agreed",
                "author_id": "4000000000000000001",
                "referenced_tweets": [
                  { "type": "replied_to", "id": "1700000000000000001" }
                ]
              },
              {
                "id": "1700000000000000001",
                "text": "hello from the timeline",
                "author_id": "2244994945"
              }
            ]
          }
        }"#;
        let response: TimelineResponse = serde_json::from_str(json).unwrap();
        let items = response.into_items();

        assert_eq!(items[0].text, "agreed");
        assert_eq!(items[0].reposted_by.as_deref(), Some("reposter1"));
        let replied_to = items[0].replied_to.as_ref().unwrap();
        assert_eq!(replied_to.post_id, "1700000000000000001");
        assert_eq!(replied_to.author_username, "XDevelopers");
    }

    #[test]
    fn a_repost_whose_original_is_missing_from_includes_falls_back_to_its_own_text() {
        // 参照先の post は削除済み､非公開､あるいは単に `includes` に無いことが
        // ある — 空の行や panic ではなく､意味のあるものを描かなければならない｡
        let json = r#"{
          "data": [
            {
              "id": "1800000000000000004",
              "text": "RT @someone: a post that was later deleted",
              "author_id": "3000000000000000001",
              "referenced_tweets": [
                { "type": "retweeted", "id": "9999999999999999999" }
              ]
            }
          ],
          "includes": {
            "users": [
              { "id": "3000000000000000001", "name": "Reposter One", "username": "reposter1" }
            ]
          }
        }"#;
        let response: TimelineResponse = serde_json::from_str(json).unwrap();
        let items = response.into_items();

        assert_eq!(items[0].text, "RT @someone: a post that was later deleted");
        assert_eq!(items[0].author_name, "");
        assert_eq!(items[0].author_username, "");
        assert_eq!(items[0].reposted_by.as_deref(), Some("reposter1"));
    }

    const REPOST_OF_QUOTE_JSON: &str = r#"{
      "data": [
        {
          "id": "1800000000000000005",
          "text": "RT @quoter1: this is worth reading",
          "author_id": "3000000000000000001",
          "referenced_tweets": [
            { "type": "retweeted", "id": "1700000000000000002" }
          ]
        }
      ],
      "includes": {
        "users": [
          { "id": "3000000000000000001", "name": "Reposter One", "username": "reposter1" },
          { "id": "4000000000000000001", "name": "Quote Author", "username": "quoter1" },
          { "id": "2244994945", "name": "Developers", "username": "XDevelopers" }
        ],
        "tweets": [
          {
            "id": "1700000000000000002",
            "text": "this is worth reading",
            "author_id": "4000000000000000001",
            "referenced_tweets": [
              { "type": "quoted", "id": "1700000000000000001" }
            ]
          },
          {
            "id": "1700000000000000001",
            "text": "hello from the timeline",
            "author_id": "2244994945"
          }
        ]
      }
    }"#;

    #[test]
    fn a_repost_of_a_quote_carries_the_nested_quote_card() {
        // #13 の優先順位: 描画される本文は retweeted が取るが､repost された
        // post 自身が持つ quote は見せる価値がある — カードは *repost された*
        // post 自身の `quoted` 参照から来ていて､トップレベルの post (それは
        // 参照を持たない) のものではない｡
        let response: TimelineResponse = serde_json::from_str(REPOST_OF_QUOTE_JSON).unwrap();
        let items = response.into_items();

        assert_eq!(items[0].text, "this is worth reading");
        assert_eq!(items[0].author_username, "quoter1");
        assert_eq!(items[0].reposted_by.as_deref(), Some("reposter1"));
        let quoted = items[0].quoted.as_ref().unwrap();
        assert_eq!(quoted.text, "hello from the timeline");
        assert_eq!(quoted.author_username, "XDevelopers");
    }

    #[test]
    fn an_unrecognized_reference_type_does_not_fail_parsing() {
        // 前方互換: 将来の API 改訂が `referenced_tweets[].type` に新しい値を
        // 足しても､レスポンス全体のパースを壊してはならない｡壊れたキャッシュ
        // ファイルが素直なミスになるのと同じだ｡
        let json = r#"{
          "data": [
            {
              "id": "1",
              "text": "future api shape",
              "referenced_tweets": [ { "type": "some_future_type", "id": "2" } ]
            }
          ]
        }"#;
        let response: TimelineResponse = serde_json::from_str(json).unwrap();
        let items = response.into_items();

        assert_eq!(items[0].text, "future api shape");
        assert_eq!(items[0].reposted_by, None);
        assert_eq!(items[0].quoted, None);
    }

    #[test]
    fn a_timeline_item_from_before_13_still_deserializes() {
        // #13 以前のディスク上のキャッシュファイルは新しいフィールドをどれも
        // 持たない — 全ユーザーのキャッシュを捨てるのではなく､それらをパース
        // し続けなければならない (`cache::load_json` の doc コメントを見よ)｡
        // `#[serde(default)]` の属性を一目見て信じるのではなく､意図して生の
        // リテラルを置いてある｡
        let old_format = r#"{
          "id": "1700000000000000001",
          "text": "hello from the timeline",
          "created_at": "2026-08-16T09:00:00.000Z",
          "author_name": "Developers",
          "author_username": "XDevelopers"
        }"#;
        let item: TimelineItem = serde_json::from_str(old_format).unwrap();
        assert_eq!(item.id, "1700000000000000001");
        assert_eq!(item.text, "hello from the timeline");
        assert_eq!(item.author_name, "Developers");
        assert_eq!(item.reposted_by, None);
        assert_eq!(item.quoted, None);
        assert_eq!(item.replied_to, None);
    }

    #[test]
    fn a_timeline_item_from_before_12_still_deserializes() {
        // #12 は #13 の `reposted_by`/`quoted` の上に `replied_to` を足す｡
        // #13 の頃のビルドが書いたキャッシュファイルにはそのキーが無い —
        // キャッシュ全体を捨ててはならない (`cache::load_json` の doc コメント
        // を見よ)｡上の兄弟テストに倣い､`#[serde(default)]` を一目見て信じるの
        // ではなく意図して生のリテラルを置いてある｡
        let pre_12_format = r#"{
          "id": "1800000000000000001",
          "text": "RT @XDevelopers: hello from the timeline",
          "created_at": "2026-08-16T10:00:00.000Z",
          "author_name": "Developers",
          "author_username": "XDevelopers",
          "reposted_by": "reposter1"
        }"#;
        let item: TimelineItem = serde_json::from_str(pre_12_format).unwrap();
        assert_eq!(item.id, "1800000000000000001");
        assert_eq!(item.reposted_by.as_deref(), Some("reposter1"));
        assert_eq!(item.quoted, None);
        assert_eq!(item.replied_to, None);
    }

    #[test]
    fn serializes_the_post_tweet_request_body_without_a_quote() {
        // #14/#16: 普通の post は `quote_tweet_id` を `null` として送るのでは
        // なく完全に省かなければならない — X は迷い込んだ null をそのまま拒否
        // することがあるので､`quote_tweet_id` が `None` に deserialize し直せる
        // かだけでなく､serialize された正確な形を確かめる｡
        let request = Draft {
            text: "hello",
            quote_tweet_id: None,
            reply_to_post_id: None,
        }
        .to_request();
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"text":"hello"}"#
        );
    }

    #[test]
    fn serializes_the_post_tweet_request_body_with_a_reply() {
        // #71: reply は同じエンドポイントに `reply` オブジェクトを入れ子で
        // 付けたものだ — その中の id がこれをどの会話にぶら下げるかを決めるので､
        // 正確な形を固定する価値がある｡
        let request = Draft {
            text: "hello",
            quote_tweet_id: None,
            reply_to_post_id: Some("1700000000000000001"),
        }
        .to_request();
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"text":"hello","reply":{"in_reply_to_tweet_id":"1700000000000000001"}}"#
        );
    }

    #[test]
    fn serializes_the_post_tweet_request_body_with_a_quote() {
        // #16: quote 専用のエンドポイントではなく `POST /2/tweets` が
        // `quote_tweet_id` を得た — これが quote の post が送るボディの全体だ｡
        let request = Draft {
            text: "hello",
            quote_tweet_id: Some("1700000000000000001"),
            reply_to_post_id: None,
        }
        .to_request();
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"text":"hello","quote_tweet_id":"1700000000000000001"}"#
        );
    }

    #[test]
    fn serializes_the_list_member_request_body() {
        // #163: `XClient::add_list_member` が送るボディの全体｡
        let request = UserIdRequest {
            user_id: "2244994945",
        };
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"user_id":"2244994945"}"#
        );
    }

    #[test]
    fn parses_a_page_of_users_with_its_cursor() {
        let body = r#"{
            "data": [
                {"id": "1", "name": "Alice", "username": "alice"},
                {"id": "2", "name": "Bob", "username": "bob"}
            ],
            "meta": {"next_token": "cursor-abc"}
        }"#;
        let page: UserPageResponse = serde_json::from_str(body).unwrap();
        assert_eq!(
            page.data.iter().map(|u| u.id.as_str()).collect::<Vec<_>>(),
            ["1", "2"]
        );
        assert_eq!(page.next_token(), Some("cursor-abc"));
    }

    #[test]
    fn parses_the_last_page_of_users() {
        // `next_token` が無いのが､どちらのエンドポイントでも "終わりだ" の合図だ｡
        let page: UserPageResponse =
            serde_json::from_str(r#"{"data": [{"id": "1", "name": "A", "username": "a"}]}"#)
                .unwrap();
        assert_eq!(page.next_token(), None);
    }

    #[test]
    fn parses_an_empty_page_of_users() {
        // #163: 誰もフォローしていないアカウントや､メンバーのいない list は
        // `[]` を送らず `data` を省く｡それをエラーとしてパースすれば最初の
        // sync からして失敗する｡
        let page: UserPageResponse =
            serde_json::from_str(r#"{"meta": {"result_count": 0}}"#).unwrap();
        assert!(page.data.is_empty());
        assert_eq!(page.next_token(), None);
    }

    #[test]
    fn serializes_the_shared_tweet_id_request_body() {
        // #15: `x_api::client::XClient::create_repost` が送るリクエストボディの
        // 全体｡
        let request = TweetIdRequest {
            tweet_id: "1700000000000000001",
        };
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"tweet_id":"1700000000000000001"}"#
        );
    }

    #[test]
    fn reads_a_nested_errors_body() {
        let problem: ApiProblem = serde_json::from_str(
            r#"{"errors":[{"title":"Not Found Error","detail":"Could not find user."}]}"#,
        )
        .unwrap();
        assert_eq!(
            problem.message().as_deref(),
            Some("Not Found Error: Could not find user.")
        );
    }

    const METRICS_JSON: &str = r#"{
      "data": [
        {
          "id": "1700000000000000001",
          "text": "a post with engagement",
          "created_at": "2026-08-16T09:00:00.000Z",
          "author_id": "2244994945",
          "public_metrics": {
            "retweet_count": 34,
            "reply_count": 12,
            "like_count": 56,
            "quote_count": 7,
            "impression_count": 8900
          }
        },
        {
          "id": "1700000000000000002",
          "text": "a post from a response that predates public_metrics",
          "author_id": "2244994945"
        }
      ],
      "includes": {
        "users": [
          { "id": "2244994945", "name": "Developers", "username": "XDevelopers" }
        ]
      }
    }"#;

    #[test]
    fn reads_public_metrics_into_the_item() {
        // #67: 追加のリクエストは要らない — `tweet.fields` が `public_metrics`
        // を求めれば timeline のレスポンスに相乗りしてくる｡
        let response: TimelineResponse = serde_json::from_str(METRICS_JSON).unwrap();
        let items = response.into_items();

        assert_eq!(
            items[0].metrics,
            Some(PostMetrics {
                replies: 12,
                reposts: 34,
                likes: 56,
            })
        );
    }

    #[test]
    fn metrics_are_none_when_the_response_omits_them() {
        // #67 の `tweet.fields` 変更より前のレスポンス — あるいは X が件数の
        // 報告を拒む post — はパースできなければならず､失敗してはならない｡
        let response: TimelineResponse = serde_json::from_str(METRICS_JSON).unwrap();
        let items = response.into_items();

        assert_eq!(items[1].metrics, None);
    }

    #[test]
    fn a_repost_shows_the_originals_metrics() {
        // 描画される本文は元の post なので (#13)､その下に出す件数も元の post
        // のものでなければならない｡
        let json = r#"{
          "data": [
            {
              "id": "1700000000000000010",
              "text": "RT @XDevelopers: the original",
              "author_id": "1000000000",
              "public_metrics": { "retweet_count": 1, "reply_count": 0, "like_count": 0 },
              "referenced_tweets": [{ "type": "retweeted", "id": "1700000000000000011" }]
            }
          ],
          "includes": {
            "users": [
              { "id": "1000000000", "name": "Reposter", "username": "reposter" },
              { "id": "2244994945", "name": "Developers", "username": "XDevelopers" }
            ],
            "tweets": [
              {
                "id": "1700000000000000011",
                "text": "the original",
                "author_id": "2244994945",
                "public_metrics": { "retweet_count": 99, "reply_count": 5, "like_count": 400 }
              }
            ]
          }
        }"#;
        let items: TimelineResponse = serde_json::from_str(json).unwrap();
        let items = items.into_items();

        assert_eq!(
            items[0].metrics,
            Some(PostMetrics {
                replies: 5,
                reposts: 99,
                likes: 400,
            })
        );
    }

    #[test]
    fn a_repost_whose_original_is_missing_reports_no_metrics() {
        // この場合に `build_item` が空にする著者フィールドと同じ理屈だ: 外側の
        // post 自身の件数は元の post のものではない｡
        let json = r#"{
          "data": [
            {
              "id": "1700000000000000010",
              "text": "RT @XDevelopers: the original",
              "author_id": "1000000000",
              "public_metrics": { "retweet_count": 1, "reply_count": 0, "like_count": 0 },
              "referenced_tweets": [{ "type": "retweeted", "id": "1700000000000000011" }]
            }
          ]
        }"#;
        let response: TimelineResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.into_items()[0].metrics, None);
    }

    const LINKS_JSON: &str = r#"{
      "data": [
        {
          "id": "1700000000000000001",
          "text": "read this https://t.co/abc and this https://t.co/abc again",
          "author_id": "2244994945",
          "entities": {
            "urls": [
              {
                "url": "https://t.co/abc",
                "expanded_url": "https://example.com/an-article",
                "display_url": "example.com/an-article"
              },
              {
                "url": "https://t.co/abc",
                "expanded_url": "https://example.com/an-article",
                "display_url": "example.com/an-article"
              },
              { "url": "https://t.co/xyz" }
            ]
          }
        },
        {
          "id": "1700000000000000002",
          "text": "no links here",
          "author_id": "2244994945"
        }
      ]
    }"#;

    #[test]
    fn expands_the_links_in_a_posts_text() {
        // #70: 本文は t.co の短縮リンクを持つ｡リダイレクトを辿らずに実際の
        // 宛先へ至る手段は `expanded_url` しかない｡
        let response: TimelineResponse = serde_json::from_str(LINKS_JSON).unwrap();
        let items = response.into_items();

        assert_eq!(
            items[0].links,
            vec![PostLink {
                url: "https://example.com/an-article".to_string(),
                label: "example.com/an-article".to_string(),
            }],
            "the repeated entity must collapse, and the one with no \
             expanded_url must be dropped"
        );
    }

    #[test]
    fn a_post_with_no_entities_has_no_links() {
        let response: TimelineResponse = serde_json::from_str(LINKS_JSON).unwrap();
        assert!(response.into_items()[1].links.is_empty());
    }

    #[test]
    fn a_link_without_a_display_url_falls_back_to_the_url_itself() {
        let json = r#"{
          "data": [
            {
              "id": "1",
              "text": "t",
              "entities": { "urls": [{ "expanded_url": "https://example.com/x" }] }
            }
          ]
        }"#;
        let response: TimelineResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            response.into_items()[0].links,
            vec![PostLink {
                url: "https://example.com/x".to_string(),
                label: "https://example.com/x".to_string(),
            }]
        );
    }

    #[test]
    fn a_repost_carries_the_originals_links() {
        // 本文は元の post のテキストなので､行が実際に解決できるのはその t.co
        // のリンクだ｡
        let json = r#"{
          "data": [
            {
              "id": "10",
              "text": "RT @XDevelopers: read this https://t.co/abc",
              "author_id": "1000000000",
              "referenced_tweets": [{ "type": "retweeted", "id": "11" }]
            }
          ],
          "includes": {
            "users": [
              { "id": "1000000000", "name": "Reposter", "username": "reposter" },
              { "id": "2244994945", "name": "Developers", "username": "XDevelopers" }
            ],
            "tweets": [
              {
                "id": "11",
                "text": "read this https://t.co/abc",
                "author_id": "2244994945",
                "entities": {
                  "urls": [
                    {
                      "expanded_url": "https://example.com/original",
                      "display_url": "example.com/original"
                    }
                  ]
                }
              }
            ]
          }
        }"#;
        let response: TimelineResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            response.into_items()[0].links,
            vec![PostLink {
                url: "https://example.com/original".to_string(),
                label: "example.com/original".to_string(),
            }]
        );
    }

    #[test]
    fn a_repost_whose_original_is_missing_reports_no_links() {
        let json = r#"{
          "data": [
            {
              "id": "10",
              "text": "RT @XDevelopers: read this https://t.co/abc",
              "author_id": "1000000000",
              "entities": {
                "urls": [{ "expanded_url": "https://example.com/outer" }]
              },
              "referenced_tweets": [{ "type": "retweeted", "id": "11" }]
            }
          ]
        }"#;
        let response: TimelineResponse = serde_json::from_str(json).unwrap();
        assert!(response.into_items()[0].links.is_empty());
    }

    #[test]
    fn reads_the_authors_avatar_url() {
        // #64: `user.fields=profile_image_url` がこれを `includes.users` に
        // 入れる｡著者の結合がすでに見に行っている場所だ｡
        let response: TimelineResponse = serde_json::from_str(TIMELINE_JSON).unwrap();
        let items = response.into_items();

        assert_eq!(
            items[0].author_avatar_url.as_deref(),
            Some("https://pbs.twimg.com/profile_images/x.jpg")
        );
        // 2 件目の post の著者は展開されなかったので､見せるアバターも
        // 無い｡
        assert_eq!(items[1].author_avatar_url, None);
    }

    #[test]
    fn a_repost_shows_the_original_authors_avatar() {
        // 署名行は元の著者のものなので (#13)､その横の顔も元の著者のもので
        // なければならない｡
        let json = r#"{
          "data": [
            {
              "id": "10",
              "text": "RT @XDevelopers: the original",
              "author_id": "1000000000",
              "referenced_tweets": [{ "type": "retweeted", "id": "11" }]
            }
          ],
          "includes": {
            "users": [
              {
                "id": "1000000000",
                "name": "Reposter",
                "username": "reposter",
                "profile_image_url": "https://pbs.twimg.com/reposter_normal.jpg"
              },
              {
                "id": "2244994945",
                "name": "Developers",
                "username": "XDevelopers",
                "profile_image_url": "https://pbs.twimg.com/original_normal.jpg"
              }
            ],
            "tweets": [
              { "id": "11", "text": "the original", "author_id": "2244994945" }
            ]
          }
        }"#;
        let response: TimelineResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            response.into_items()[0].author_avatar_url.as_deref(),
            Some("https://pbs.twimg.com/original_normal.jpg")
        );
    }

    const MEDIA_JSON: &str = r#"{
      "data": [
        {
          "id": "1",
          "text": "with photos",
          "author_id": "2244994945",
          "attachments": { "media_keys": ["k-photo", "k-video", "k-missing"] }
        },
        { "id": "2", "text": "no attachments", "author_id": "2244994945" }
      ],
      "includes": {
        "users": [
          { "id": "2244994945", "name": "Developers", "username": "XDevelopers" }
        ],
        "media": [
          {
            "media_key": "k-photo",
            "type": "photo",
            "url": "https://pbs.twimg.com/media/photo.jpg",
            "width": 1200,
            "height": 675,
            "alt_text": "a chart"
          },
          {
            "media_key": "k-video",
            "type": "video",
            "preview_image_url": "https://pbs.twimg.com/media/still.jpg",
            "width": 1280,
            "height": 720
          }
        ]
      }
    }"#;

    #[test]
    fn joins_attached_media_by_key_in_the_posts_own_order() {
        // #65: `users` と `tweets` がすでに使うのと同じサイドテーブルの結合｡
        let response: TimelineResponse = serde_json::from_str(MEDIA_JSON).unwrap();
        let items = response.into_items();

        assert_eq!(items[0].media.len(), 2, "the unmatched key must be skipped");
        assert_eq!(
            items[0].media[0].url,
            "https://pbs.twimg.com/media/photo.jpg"
        );
        assert_eq!(items[0].media[0].alt_text.as_deref(), Some("a chart"));
        assert_eq!(items[0].media[0].kind.as_deref(), Some("photo"));
    }

    #[test]
    fn a_video_falls_back_to_its_preview_still() {
        // このアプリは video を再生しない｡静止画とバッジで描画の全部なので､
        // 通ってこなければならないのは `preview_image_url` だ｡
        let response: TimelineResponse = serde_json::from_str(MEDIA_JSON).unwrap();
        let items = response.into_items();

        assert_eq!(
            items[0].media[1].url,
            "https://pbs.twimg.com/media/still.jpg"
        );
        assert_eq!(items[0].media[1].kind.as_deref(), Some("video"));
    }

    #[test]
    fn media_with_nothing_displayable_is_dropped() {
        // `url` も `preview_image_url` も無い: 描くものが何も無いし､グリッド
        // の穴はサムネイルが 1 枚減るより悪い｡
        let json = r#"{
          "data": [{ "id": "1", "text": "t", "attachments": { "media_keys": ["k"] } }],
          "includes": { "media": [{ "media_key": "k", "type": "photo" }] }
        }"#;
        let response: TimelineResponse = serde_json::from_str(json).unwrap();
        assert!(response.into_items()[0].media.is_empty());
    }

    #[test]
    fn a_post_without_attachments_has_no_media() {
        let response: TimelineResponse = serde_json::from_str(MEDIA_JSON).unwrap();
        assert!(response.into_items()[1].media.is_empty());
    }

    #[test]
    fn a_repost_carries_the_originals_media() {
        // 本文は元の post のテキストなので､その下の画像も元の post のもので
        // なければならない｡
        let json = r#"{
          "data": [
            {
              "id": "10",
              "text": "RT @XDevelopers: look",
              "author_id": "1000000000",
              "referenced_tweets": [{ "type": "retweeted", "id": "11" }]
            }
          ],
          "includes": {
            "users": [
              { "id": "1000000000", "name": "R", "username": "reposter" },
              { "id": "2244994945", "name": "D", "username": "XDevelopers" }
            ],
            "tweets": [
              {
                "id": "11",
                "text": "look",
                "author_id": "2244994945",
                "attachments": { "media_keys": ["k"] }
              }
            ],
            "media": [
              {
                "media_key": "k",
                "type": "photo",
                "url": "https://pbs.twimg.com/media/original.jpg"
              }
            ]
          }
        }"#;
        let response: TimelineResponse = serde_json::from_str(json).unwrap();
        let items = response.into_items();
        assert_eq!(items[0].media.len(), 1);
        assert_eq!(
            items[0].media[0].url,
            "https://pbs.twimg.com/media/original.jpg"
        );
    }

    #[test]
    fn a_cache_file_written_before_media_existed_still_loads() {
        let item: TimelineItem = serde_json::from_str(
            r#"{"id":"1","text":"cached","created_at":null,"author_name":"a","author_username":"b"}"#,
        )
        .unwrap();
        assert!(item.media.is_empty());
    }

    #[test]
    fn a_repost_carries_the_original_posts_id() {
        // #52: `id` は retweet アクティビティのものだが､write のエンドポイント
        // はどれも元の post のものを要る — 参照がすでにそれを名指している｡
        let json = r#"{
          "data": [
            {
              "id": "1700000000000000010",
              "text": "RT @XDevelopers: the original",
              "author_id": "1000000000",
              "referenced_tweets": [{ "type": "retweeted", "id": "1700000000000000011" }]
            }
          ],
          "includes": {
            "users": [
              { "id": "1000000000", "name": "Reposter", "username": "reposter" },
              { "id": "2244994945", "name": "Developers", "username": "XDevelopers" }
            ],
            "tweets": [
              { "id": "1700000000000000011", "text": "the original", "author_id": "2244994945" }
            ]
          }
        }"#;
        let response: TimelineResponse = serde_json::from_str(json).unwrap();
        let items = response.into_items();

        assert_eq!(items[0].id, "1700000000000000010");
        assert_eq!(
            items[0].original_post_id.as_deref(),
            Some("1700000000000000011")
        );
        assert_eq!(action_post_id(&items[0]), "1700000000000000011");
    }

    #[test]
    fn a_repost_whose_original_is_missing_still_carries_its_id() {
        // id は expansion ではなく参照から来るので､元の post が削除済みでも
        // 未展開でもボタンを失わない｡
        let json = r#"{
          "data": [
            {
              "id": "10",
              "text": "RT @XDevelopers: gone",
              "author_id": "1000000000",
              "referenced_tweets": [{ "type": "retweeted", "id": "11" }]
            }
          ]
        }"#;
        let response: TimelineResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            response.into_items()[0].original_post_id.as_deref(),
            Some("11")
        );
    }

    #[test]
    fn an_ordinary_post_carries_no_original_id() {
        let response: TimelineResponse = serde_json::from_str(TIMELINE_JSON).unwrap();
        let items = response.into_items();
        assert_eq!(items[0].original_post_id, None);
        assert_eq!(action_post_id(&items[0]), items[0].id);
    }

    #[test]
    fn a_cache_file_written_before_the_original_id_existed_still_loads() {
        // 意図して生のリテラルを置いてある: `cache::load_json` はパース失敗を
        // *黙った* ミスに変えるので､`#[serde(default)]` が欠けると全ユーザーの
        // キャッシュを黙って捨て､その費用で取り直すことになる｡属性を目視する
        // ことは確かめることと同じではない｡
        let item: TimelineItem = serde_json::from_str(
            r#"{"id":"1","text":"cached","created_at":null,"author_name":"a","author_username":"b","reposted_by":"c"}"#,
        )
        .unwrap();
        assert_eq!(item.original_post_id, None);
        assert_eq!(action_post_id(&item), "1");
    }

    #[test]
    fn a_cache_file_written_before_avatars_existed_still_loads() {
        let item: TimelineItem = serde_json::from_str(
            r#"{"id":"1","text":"cached","created_at":null,"author_name":"a","author_username":"b"}"#,
        )
        .unwrap();
        assert_eq!(item.author_avatar_url, None);
    }

    #[test]
    fn a_cache_file_written_before_links_existed_still_loads() {
        let item: TimelineItem = serde_json::from_str(
            r#"{"id":"1","text":"cached","created_at":null,"author_name":"a","author_username":"b"}"#,
        )
        .unwrap();
        assert!(item.links.is_empty());
    }

    #[test]
    fn a_cache_file_written_before_metrics_existed_still_loads() {
        // #9 のキャッシュファイルはまさにこの型なので､#67 より前にディスクへ
        // 書かれたファイルはフィールドを単に欠いたまま deserialize できなければ
        // ならない｡
        let item: TimelineItem = serde_json::from_str(
            r#"{"id":"1","text":"cached","created_at":null,"author_name":"a","author_username":"b"}"#,
        )
        .unwrap();
        assert_eq!(item.metrics, None);
    }
}
