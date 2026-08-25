//! 新しく取得したバッチを既にキャッシュされているものへマージし､
//! 結果を時刻順に保つ｡
//!
//! `cache` から切り出した (#117)｡`cache` は 2 本の pull request で天井を
//! 二度上げていた — #97 の schema version とフィールドのマージで 600 から
//! 700 へ､#102 の並び替えで 700 から 800 へ｡どちらもここに落ちたことが､
//! この部分を分離する価値のあるものにした｡
//!
//! このファイルの中身はすべて純粋である: キャッシュ済みの行と流入する行を
//! 受け取り､ファイルに残るべきものを返すだけで､ディスクにもネットワークにも
//! 触れない｡`cache` が持つテストのほぼすべてがこれらの関数に対して書かれて
//! いるのはそのためだ｡ファイルの読み書きと､それを埋めるために API request を
//! 使う関数は親に残る｡

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use crate::x_api::TimelineItem;

use super::MAX_CACHED_POSTS;

/// キャッシュ済みで最も新しい post の id｡次の fetch に `since_id` として
/// 渡す — キャッシュファイルは常に newest-first で保存されるので先頭要素｡
///
/// X の post id は `u32` を超える snowflake 形式の数値文字列なので､通して
/// `String` のまま扱う｡ここで整数へパースするものは無く､順序は常に API 自身の
/// レスポンス順から来る｡辞書順の文字列比較ではない (桁数の境界で壊れる｡
/// たとえば `"9" > "10"`)｡
pub(crate) fn since_id(cached: &[TimelineItem]) -> Option<&str> {
    cached.first().map(|item| item.id.as_str())
}

/// 流入するバッチがキャッシュ済みのものに対してどちら側に属するか (#92)｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Side {
    /// `since_id` reload から来た新しいバッチ — 前に置く｡
    Ahead,
    /// `meta.next_token` を辿って得た古いバッチ (#11 の "Load older") —
    /// 後ろに置く｡代わりに前へ置くと､キャッシュの他のすべての読み手が
    /// 頼っている newest-first の不変条件を黙って反転させてしまう｡
    Behind,
}

/// `cached` の optional/collection なフィールドのうち空のものを､`incoming` の
/// 同じフィールドの値で埋める｡`cached` が既に持っているフィールドはどれも
/// 触らない (#97)｡
///
/// フィールドごとに分岐を書くのではなく､マージ可能なすべてのフィールドへ同一に
/// 適用される規則が一つあるだけだ: **cached の `Some`/非空が勝ち､incoming は
/// cached に欠けているものだけを埋める｡** これはフィールドが存在する前に保存
/// された行が必要とするものそのもので (#64/#65/#70 より前の
/// `author_avatar_url`/`media`/`links`)､#67 の `metrics` スナップショットが
/// 必要とするものでもある — キャッシュ済みの `Some` な metrics が新しいもので
/// 置き換わることは無いので､metrics 専用の分岐はここに無い｡将来 [`TimelineItem`]
/// へ足されるフィールドも､下に追記されればこの同じ「欠けているときだけ埋める」
/// 挙動が既定になる｡既定として安全な側だ｡
///
/// `id`/`text`/`author_name`/`author_username` は `Option` ではない｡parser が
/// API レスポンスから常に埋めるからで､`cached` のそれらの値 (と下に挙げていない
/// 他のすべてのフィールド) はそのまま保たれる｡
fn merge_item(mut cached: TimelineItem, incoming: &TimelineItem) -> TimelineItem {
    if cached.created_at.is_none() {
        cached.created_at.clone_from(&incoming.created_at);
    }
    if cached.reposted_by.is_none() {
        cached.reposted_by.clone_from(&incoming.reposted_by);
    }
    if cached.quoted.is_none() {
        cached.quoted.clone_from(&incoming.quoted);
    }
    if cached.replied_to.is_none() {
        cached.replied_to.clone_from(&incoming.replied_to);
    }
    if cached.metrics.is_none() {
        cached.metrics = incoming.metrics;
    }
    if cached.links.is_empty() {
        cached.links.clone_from(&incoming.links);
    }
    if cached.author_avatar_url.is_none() {
        cached
            .author_avatar_url
            .clone_from(&incoming.author_avatar_url);
    }
    if cached.original_post_id.is_none() {
        cached
            .original_post_id
            .clone_from(&incoming.original_post_id);
    }
    if cached.media.is_empty() {
        cached.media.clone_from(&incoming.media);
    }
    cached
}

/// `items` を `created_at` の降順 (newest first) に安定ソートする｡`created_at`
/// を持たない行は末尾へ送る (#102)｡
///
/// `created_at` をここで日付型へパースしないのは意図的だ｡どの timeline
/// endpoint も `tweet.fields=created_at` で要求していて (`x_api::client` の
/// 3 つの `.../tweet.fields=created_at...` クエリ文字列)､API は常に固定幅で
/// UTC のみの RFC 3339 タイムスタンプ `YYYY-MM-DDTHH:MM:SS.mmmZ` として返す｡
/// 固定幅と単一タイムゾーンは､[`since_id`] の doc comment が post の *id* に
/// 欠けていると警告しているものそのものだ — id は時間とともに桁数が増えるので､
/// 2 つの id の辞書順比較は桁数の境界ごとに壊れる (文字列としては `"9" > "10"`
/// だが､id `10` の方が後に発行されている)｡`created_at` にその境界は無い:
/// 年・月・日・時・分・秒・ミリ秒がそれぞれ固定幅でゼロ埋めされているので､
/// 2 つの `created_at` のバイト単位の文字列比較は時系列の順序と一致する｡
/// 日付パースの crate を持ち込まずに下で生の文字列を比較している根拠はそれが
/// すべてだ — `id` に対しては正しくないやり方が､ここでは正しい｡
///
/// 安定ソート ([`slice::sort_by`] であって `sort_unstable_by` ではない) なのは
/// 単に都合が良いからではない: ミリ秒まで同じ `created_at` を持つ 2 つの post は
/// あり得ないことではなく､そうなったときに呼び出し側が既に持っていた相対順序を
/// そのまま保つ — 取得したてのページなら､それは API 自身のレスポンス順だ｡
///
/// `created_at` を持たない行は文字列比較に加わらず､持っている行すべての後ろへ
/// 並ぶ — `None` は "oldest" ではなく "unknown" を意味する｡実際にはこれは稀だ:
/// `created_at` はどの timeline レスポンスにも入っていて､`None` が現れるのは
/// フィールドが存在する前にキャッシュへ書かれた行 (#97) か､レスポンスが壊れて
/// 返ってきた場合だけだ｡そういう行を先頭へ上げるのではなく末尾へ沈めるのが
/// 害の小さい既定である: 世代の混ざったキャッシュで post が数枠深く沈むのは､
/// 無関係な post が newest-first のフィードの一番上へ飛び出すのに比べれば
/// はるかに目立たない｡
///
/// これは [`since_id`] と相互作用する｡`since_id` は再開点となる最新の post と
/// して `cached.first()` を報告する｡`created_at: None` の真新しい行が splice
/// されると､ここで末尾へ沈むために `since_id` は *より古い* post を最新の
/// キャッシュ済み post として報告し､次の reload はその古い id 以降の post を
/// API へ要求する — この行を既に含む範囲だ｡その再取得が無害なのは **`splice`
/// が流入バッチを素朴に連結するのではなく id でマージするから (#97) に他ならない**:
/// 行は API から返ってきて､既にキャッシュ済みだと認識され､2 度目として追加される
/// のではなく [`merge_item`] を通して収まる｡代償は無駄な request 1 回であって､
/// 重複行ではない｡このソート関数自体がその安全網を提供しているわけではなく —
/// それはまるごと `splice` の id ベースのマージの性質だ — もしそのマージ段階が
/// 弱められたり取り除かれたりすれば､`since_id` より下へ沈む `created_at` が
/// `None` の行は､無料の再取得ではなく黙った重複になる｡
fn sort_by_created_at_desc(items: &mut [TimelineItem]) {
    items.sort_by(|a, b| match (&a.created_at, &b.created_at) {
        (Some(a_created_at), Some(b_created_at)) => b_created_at.cmp(a_created_at),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    });
}

/// `incoming` を指定された側で `cached` へ継ぎ足し､結果を `created_at` の降順へ
/// 再ソートし (#102､[`sort_by_created_at_desc`] 経由)､[`MAX_CACHED_POSTS`] で
/// 上限を切る｡
///
/// 下の連結順 ([`Side::Ahead`] なら fresh の後に cached､[`Side::Behind`] なら
/// cached の後に fresh) は､もはやそれ自体で結果を newest-first にするものでは
/// ない — かつてはそれが完全に､各側の post を API が既に時刻順で返すことに
/// 依存していた (#92 の当初の設計)｡この関数は今その順序を仮定せず､ソートの
/// 一手で明示的に主張するので､取得順によらず newest-first が保たれる:
/// `since_id` reload も `Load older` のページも､将来の投稿時挿入の経路 (#14) も
/// すべて同じ順序へ収束する｡
///
/// 既に `cached` にある id は `incoming` から落とす — API は増分の reload でも
/// ページ境界でも､既にファイルにある post を返してくる｡**残るのはキャッシュ済み
/// のコピーの方**でありどちらの向きでもそうだが､そのままではない: まず
/// [`merge_item`] を通り､incoming のコピーから欠けているフィールドを埋める
/// (#97) — そうしないと､`author_avatar_url` のようなフィールドが存在する前に
/// キャッシュされた行が､ページ境界や `since_id` の重なりで何度も現れながら
/// そのフィールドを永久に拾えないままになる｡`reload` も `load_older_primary` も､
/// 既にファイルにある id を他の経路で取り直すことはないからだ｡マージが存在する
/// 前は､これが置き換えた 2 つの関数のどちらについてもそうだった (#92: 連結を
/// 逆にしただけの同じ操作で､キャッシュ済みのコピーをそのまま残していた) —
/// post の `metrics` (#67) は取得した時点のスナップショットなので､既にある
/// フィールドについて「ファイルにあるものを残す」ことが､reload に既存行の
/// カウントをかき混ぜさせず放っておかせる｡[`merge_item`] の規則はそれを
/// 保ちながら #97 も直している｡
pub(crate) fn splice(
    cached: Vec<TimelineItem>,
    incoming: Vec<TimelineItem>,
    side: Side,
) -> Vec<TimelineItem> {
    let incoming_by_id: HashMap<&str, &TimelineItem> = incoming
        .iter()
        .map(|item| (item.id.as_str(), item))
        .collect();
    let merged: Vec<TimelineItem> = cached
        .into_iter()
        .map(|item| match incoming_by_id.get(item.id.as_str()) {
            Some(fresh) => merge_item(item, fresh),
            None => item,
        })
        .collect();

    let cached_ids: HashSet<&str> = merged.iter().map(|item| item.id.as_str()).collect();
    let fresh: Vec<TimelineItem> = incoming
        .into_iter()
        .filter(|item| !cached_ids.contains(item.id.as_str()))
        .collect();

    // どちらの腕も clone ではなく move する: 先に来る側がバッファになり､
    // もう一方がそこへ追記される｡
    let mut spliced = match side {
        Side::Ahead => {
            let mut ahead = fresh;
            ahead.extend(merged);
            ahead
        }
        Side::Behind => {
            let mut behind = merged;
            behind.extend(fresh);
            behind
        }
    };
    // 上限を切る前に created_at で再ソートする (#102): 先にソートして後で
    // 切り詰めるのが､最新の行を残す唯一の順序だ｡切ってからソートすると､
    // 古びた既存の並びがどの行を残すか決めることになり､`spliced` の末尾より
    // 後ろへ来ただけの本当に新しい行を落として､その上流にある古い行を
    // 生き残らせてしまう｡
    sort_by_created_at_desc(&mut spliced);
    spliced.truncate(MAX_CACHED_POSTS);
    spliced
}

/// `post_id` を除くすべての item を､同じ順序で返す (#72)｡
///
/// 純粋なので､post 削除のうち「何が残るべきか」の側をディスクに触れずに
/// テストできる — ファイルを読み書きするのは [`forget_post`] の方だ｡
pub(crate) fn without_post(items: Vec<TimelineItem>, post_id: &str) -> Vec<TimelineItem> {
    items
        .into_iter()
        .filter(|item| item.id != post_id)
        .collect()
}
