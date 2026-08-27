//! どの操作を差し出すかの述語 (#241): `offers_*` と [`is_own_post`]｡
//! どれも純粋関数で､`ui/mod.rs` のテストが押さえる｡

use crate::oauth;
use crate::x_api::TimelineItem;

/// ヘッダが再認可を差し出すべきかどうか (#14): セッションは在るが､記録
/// された scope に書き込みが要るものが含まれていない､という状態だ｡
///
/// 主ボタンの "Sign in with X" とは構造上べつものだ — こちらはセッション
/// を要求し､あちらはセッションが無いときにだけ現れる — し､読み方も違う
/// ("Sign in" と "Re-authorize")｡#31 の本当の教訓は「導線を隠すな」で
/// あって「ボタンは一つでなければならない」ではない｡
///
/// #14 のものだけでなく､アプリが要りうる write scope をすべて確認する:
/// #68 が `like.write` を足し､X はこれを別に許可するので､#68 より前に
/// 認可されたセッションは `tweet.write` しか持たない｡これが無いと
/// `toggle_like` の拒否は､描かれていない "Re-authorize" ボタンを指す｡
///
/// `list.read` (#167) もそこへ加わるが､list が設定されている間だけだ
/// (#161)｡ここで最初の *read* の scope であり､欠けるとボタンが無効に
/// なるのではなくウィンドウがそもそも埋まらなくなる最初のものでもある:
/// #167 より前に認可されたセッションは `GET /2/lists/:id/tweets` から
/// 403 を受け取り､他に手掛かりは無い｡無条件に要求せず `reads_a_list` を
/// 条件にすれば､list を一度も設定せずその 403 に当たりようのない人の
/// toolbar からはボタンを外しておける｡
pub(in crate::ui) fn offers_reauthorize(
    signed_in_with_oauth: bool,
    oauth_scope: Option<&str>,
    reads_a_list: bool,
) -> bool {
    let list_read_satisfied =
        !reads_a_list || oauth::tokens::has_scope(oauth_scope, oauth::tokens::LIST_READ_SCOPE);
    signed_in_with_oauth
        && !(oauth::tokens::has_scope(oauth_scope, oauth::tokens::TWEET_WRITE_SCOPE)
            && oauth::tokens::has_scope(oauth_scope, oauth::tokens::LIKE_WRITE_SCOPE)
            && list_read_satisfied)
}

/// post `item` が repost/un-repost の toggle を差し出すべきか (#15)｡
///
/// sign in 済みの OAuth セッションと､解決済みの自分の id (`/me` 経由の
/// `home_user_id` — #11) を要求する: repost の endpoint は *この* アカ
/// ウントとして振る舞い､それが無ければ呼ぶ先が無い｡自分の post には出さ
/// ない｡API 自身の拒否に合わせたものだ (#15) — [`is_own_post`] を見よ｡
/// repost 行では *元の* 著者と比べる｡行が表示しているのも repost される
/// のもその人の post だからだ｡
///
/// repost 行にも以前は出していなかった｡`item.id` が元の内容ではなく
/// retweet という活動の id だからだ｡#52 がそれを閉じた: 元の id はいま
/// item に載っており､どの呼び出し側も `x_api::action_post_id` を送る｡
pub(in crate::ui) fn offers_repost(
    signed_in_with_oauth: bool,
    home_user_id: Option<&str>,
    home_username: Option<&str>,
    item: &TimelineItem,
) -> bool {
    signed_in_with_oauth
        && home_user_id.is_some()
        && !is_own_post(home_username, &item.author_username)
}

/// `author_username` が sign in 済みアカウント自身のものか (#15) — API は
/// 自分の post の repost を拒むので､ここで確認すれば確実に失敗するリク
/// エストを節約できる｡#14 のクライアント側の文字数確認を写したものだ｡
/// `home_username: None` (まだ未解決) はボタンを引っ込めない: sign in した
/// 身元が判る前にすべての post でボタンを隠すより､同一アカウントの repost
/// がたまに API 自身の拒否まで通る方が安全だ｡`home_username` (`/me` 由来)
/// と `author_username` (timeline の expansion 由来) は独立に解決されるの
/// で大文字小文字は区別しない｡
pub(in crate::ui) fn is_own_post(home_username: Option<&str>, author_username: &str) -> bool {
    home_username.is_some_and(|home| home.eq_ignore_ascii_case(author_username))
}

/// post `item` が like/unlike の toggle を差し出すべきか (#68)｡
///
/// [`offers_repost`] と同じ理由で､sign in 済みの OAuth セッションと解決
/// 済みの自分の id (`/me` 経由の `home_user_id` — #11) を要求する:
/// likes の endpoint は *この* アカウントとして振る舞うからだ｡
///
/// [`offers_repost`] からの唯一の逸脱: [`is_own_post`] の確認が無い｡X は
/// 自分の post の repost は拒むが like は受け入れるので､#68 は #15 の
/// 守りを持ち越さないよう明示的に指示している｡#52 以降 repost 行にも他と
/// 同じくボタンを出す — like は `x_api::action_post_id` を通して元の post
/// に着く｡
pub(in crate::ui) fn offers_like(
    signed_in_with_oauth: bool,
    home_user_id: Option<&str>,
    _item: &TimelineItem,
) -> bool {
    signed_in_with_oauth && home_user_id.is_some()
}

/// post `item` が削除の導線を差し出すべきか (#72)｡
///
/// 自分の post だけだ — X は他人のものの削除を拒むし､[`is_own_post`] が
/// #15 のためにすでにその問いへ答えている｡他の write 操作と同じ理由で
/// 解決済みの `home_user_id` を要求する: `/me` が無ければアプリはこれらが
/// 誰の post なのかをまだ知らない｡
///
/// #52 以降の他のすべての操作と違い､**repost 行では出さない**｡repost 行は
/// 誰かの元の post を表示する; `is_own_post` はその元の著者と比べるので､
/// そうしないと自分の post の repost では､ユーザーが「自分の repost」と
/// 読んでいる行から元の post の削除を差し出してしまう｡repost を消すのは
/// [`offers_repost`] の toggle であり､取り返しのつかない操作で二つを混同
/// するのは冒す価値のある危険ではない｡
pub(in crate::ui) fn offers_delete(
    signed_in_with_oauth: bool,
    home_user_id: Option<&str>,
    home_username: Option<&str>,
    item: &TimelineItem,
) -> bool {
    signed_in_with_oauth
        && home_user_id.is_some()
        && item.reposted_by.is_none()
        && is_own_post(home_username, &item.author_username)
}

/// post `item` が "Reply" 操作を差し出すべきか (#71)｡
///
/// composer にそもそも辿り着けることを要求する — [`offers_quote`] が使う
/// のと同じ条件 `signed_in_with_oauth` だ｡それが無ければ reply の行き先が
/// 無い｡他には何も要らない: X は自分の post への reply を受け入れるし､
/// #52 が元の post へ解決するようになったいま repost 行でも問題ない｡
pub(in crate::ui) fn offers_reply(signed_in_with_oauth: bool, _item: &TimelineItem) -> bool {
    signed_in_with_oauth
}

/// post `item` が "Quote" 操作を差し出すべきか (#16)｡
///
/// composer にそもそも辿り着けることを要求する — `signed_in_with_oauth`
/// で､`Render::render` 自身の `self.composer` に対する条件を写している
/// — それが無ければ quote の行き先が無いからだ｡#52 以降 repost 行にも他と
/// 同じく出す — `x_api::action_post_id` が元の post へ解決し､それが quote
/// カードの運ぶテキストと著者でもある｡[`offers_repost`] と違い､自分の post
/// を quote するのは許されている (#16 の設計上の判断 — API は自分を repost
/// するときのようには拒まない) ので､ここに `is_own_post` の確認は無い｡
pub(in crate::ui) fn offers_quote(signed_in_with_oauth: bool, _item: &TimelineItem) -> bool {
    signed_in_with_oauth
}
