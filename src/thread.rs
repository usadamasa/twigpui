//! "Show thread" (#12) のための親チェーンの組み立て｡
//!
//! 返信の親をたどると 1 階層につき `GET /2/tweets?ids=` が 1 リクエスト
//! かかるので (`cache::fetch_thread` を参照)､たどる処理そのものは明示的な
//! クリックのときだけ走り､[`MAX_THREAD_DEPTH`] 階層で打ち止めになる｡この
//! モジュールはその機能の純粋な側だ: `cache::fetch_thread` のたどり処理が
//! 取得できた post を受け取り (親が欠けていれば上限より少なく､天井に
//! 当たったならちょうど上限)､[`assemble_chain`] が表示用に並べ替え､
//! 循環したレスポンスが同じ post を重複させないよう防ぎ､たどるのを
//! やめた理由が自然な終端ではなく上限だったのかを判定する｡ここにあるものは
//! ネットワークにもディスクにも触れない｡触るのは `cache::fetch_thread` と
//! `x_api::client::XClient::tweets_by_id` だけだ｡

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// "Show thread" が親を何階層までたどり､そこで打ち切ってその旨を伝えるか｡
/// 長いスレッドに対して無制限にリクエストを費やさないためだ｡1 階層につき
/// `GET /2/tweets?ids=` が 1 リクエストなので､これは 1 クリックあたりの
/// 最悪ケースのリクエスト数でもある｡
pub(crate) const MAX_THREAD_DEPTH: usize = 5;

/// 組み立てた親チェーンの中の post 1 つ｡[`crate::x_api::TimelineItem`] と
/// 同じように､著者の情報とともにすでに平坦化してある｡
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ThreadItem {
    pub id: String,
    pub text: String,
    pub author_name: String,
    pub author_username: String,
}

/// 返信の親チェーンをたどった (あるいはたどろうとした) 結果 (#12):
/// 見つかった祖先を古い順に並べたもの (スレッドの根が index 0､返信の直接の
/// 親が末尾) と､たどるのをやめた理由が [`MAX_THREAD_DEPTH`] に達したこと
/// なのかどうか — 親が自然に尽きた場合 (会話の根に着いた､あるいは途中で
/// 親が欠けていた) とは区別する｡
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ThreadChain {
    pub items: Vec<ThreadItem>,
    pub capped: bool,
}

/// たどった親チェーンを表示用に組み立てる｡
///
/// `hops` は *たどった順* だ — 返信の直接の親が最初､その親が 2 番目､
/// というように上へ向かう — `cache::fetch_thread` が見つける順序が
/// これだからだ (各階層の id は 1 つ前が解決して初めて分かる)｡表示が
/// 求めるのは逆順で､いちばん古い祖先が上､下りていって返信の直前の post に
/// なる｡だから呼び出し側それぞれにやらせず､ここで反転させる｡
///
/// `reached_cap` は､たどる処理が *なぜ* 止まったかについての記録だ:
/// さらに親が存在すると分かっている状態で [`MAX_THREAD_DEPTH`] に当たって
/// 止まったときだけ `true` になり､親が欠けて止まったときは決して `true` に
/// ならない｡重複した id (API レスポンスが自分自身へ戻ってくる場合｡起きない
/// はずだが､防ぐコストはゼロだ) は取り除き､たどった順で最初に現れたものだけ
/// を残す｡取り除いた結果が [`MAX_THREAD_DEPTH`] 件を超えていたら切り詰め､
/// 呼び出し側が何を渡したかに関わらず `capped` を `true` に固定する｡
/// これにより「上限を超える件数は決して表示しない」という不変条件が､
/// 壊れた入力に対しても保たれる｡
pub(crate) fn assemble_chain(hops: Vec<ThreadItem>, reached_cap: bool) -> ThreadChain {
    let mut seen: HashSet<String> = HashSet::new();
    let mut deduped: Vec<ThreadItem> = hops
        .into_iter()
        .filter(|item| seen.insert(item.id.clone()))
        .collect();

    let capped = reached_cap || deduped.len() > MAX_THREAD_DEPTH;
    deduped.truncate(MAX_THREAD_DEPTH);
    deduped.reverse();

    ThreadChain {
        items: deduped,
        capped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str) -> ThreadItem {
        ThreadItem {
            id: id.to_string(),
            text: format!("text of {id}"),
            author_name: format!("Author {id}"),
            author_username: format!("author{id}"),
        }
    }

    #[test]
    fn reverses_walk_order_to_oldest_first_for_display() {
        // たどった順: 直接の親が最初､その親が 2 番目､根が最後｡
        let hops = vec![item("parent"), item("grandparent"), item("root")];
        let chain = assemble_chain(hops, false);
        assert_eq!(
            chain
                .items
                .iter()
                .map(|i| i.id.as_str())
                .collect::<Vec<_>>(),
            vec!["root", "grandparent", "parent"]
        );
        assert!(!chain.capped);
    }

    #[test]
    fn drops_a_duplicate_id_keeping_the_first_occurrence_in_walk_order() {
        // 循環したレスポンス (起きないはずだが､無条件に信用してはいけない)
        // が同じ post を 2 度描画させてはならない｡
        let hops = vec![item("a"), item("b"), item("a")];
        let chain = assemble_chain(hops, false);
        assert_eq!(
            chain
                .items
                .iter()
                .map(|i| i.id.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "a"]
        );
    }

    #[test]
    fn reports_uncapped_when_the_walk_ended_naturally_at_exactly_five_levels() {
        let hops: Vec<ThreadItem> = (1..=MAX_THREAD_DEPTH)
            .map(|n| item(&n.to_string()))
            .collect();
        let chain = assemble_chain(hops, false);
        assert_eq!(chain.items.len(), MAX_THREAD_DEPTH);
        assert!(!chain.capped);
    }

    #[test]
    fn reports_capped_when_the_walker_stopped_at_the_depth_limit() {
        let hops: Vec<ThreadItem> = (1..=MAX_THREAD_DEPTH)
            .map(|n| item(&n.to_string()))
            .collect();
        let chain = assemble_chain(hops, true);
        assert_eq!(chain.items.len(), MAX_THREAD_DEPTH);
        assert!(chain.capped);
    }

    #[test]
    fn truncates_and_forces_capped_if_handed_more_than_the_depth_limit() {
        // 防御的なケース: たどる処理のループが `MAX_THREAD_DEPTH` を超える
        // hop を作ることはないはずだが､作ったとしても不変条件は保たれる｡
        let hops: Vec<ThreadItem> = (1..=MAX_THREAD_DEPTH + 2)
            .map(|n| item(&n.to_string()))
            .collect();
        let chain = assemble_chain(hops, false);
        assert_eq!(chain.items.len(), MAX_THREAD_DEPTH);
        assert!(chain.capped);
    }

    #[test]
    fn an_empty_walk_is_an_uncapped_empty_chain() {
        // 最初の親からして欠けていた (削除済み/非公開/存在しない) —
        // #12 の「まともに描画しなければならない」ケースだ｡何も見つからず､
        // その理由は深さの上限ではない｡
        let chain = assemble_chain(Vec::new(), false);
        assert_eq!(chain.items, Vec::new());
        assert!(!chain.capped);
    }

    #[test]
    fn a_partial_walk_stopped_by_a_missing_parent_is_not_reported_as_capped() {
        // 2 階層は解決し､3 つ目の親が欠けていた — たどる処理はエラーに
        // ならず正常に止まり､`capped` はその理由が深さの上限だと主張しては
        // ならない｡
        let hops = vec![item("parent"), item("grandparent")];
        let chain = assemble_chain(hops, false);
        assert_eq!(chain.items.len(), 2);
        assert!(!chain.capped);
    }
}
