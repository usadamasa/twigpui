//! timeline の一覧の仮想化 (#301): [`gpui::list`] の [`ListState`] を､
//! `ScrollHandle` と同じ顔で `ui` の残りへ見せる｡
//!
//! `layout.rs` は Loaded の全件 (最大 500) に `post_row` を積んでいた｡
//! gpui は画面外の要素も layout・prepaint・paint に回すので､ばね (#175)､
//! glide (#22)､countdown の毎秒､画像が 1 枚届くたびの notify のどれもが
//! 500 行ぶんの組み直しになっていた｡[`gpui::list`] は viewport に入る行
//! (と上下の [`OVERDRAW`]) だけを組む｡行の高さは画像の有無で変わるので
//! `uniform_list` は使えない｡
//!
//! # なぜ facade か
//!
//! [`super::scroll`] のばねと rubber band､[`super::follow`] の glide は
//! **絶対 px** の offset と floor (`-max_offset`) で動く｡`ListState` は
//! 位置を `(item_ix, offset_in_item)` で持つが､全行の高さが分かって
//! いれば px との往復は一意に決まる — `scroll_px_offset_for_scrollbar` /
//! `max_offset_for_scrollbar` / `scroll_by` がそれで､scrollbar のために
//! 用意された口だ｡facade がそれらを `offset` / `max_offset` / `set_offset`
//! の名で出すので､呼び出し側は `ScrollHandle` のときの式のまま動く｡
//!
//! # 測っていない行は 0px
//!
//! `ListState` は測っていない行を 0px と数える｡それでは floor が浅すぎて
//! ホイールの目標が手前で clamp され､glide の出発点も狂う｡だから
//! `measure_all` で組み､一覧の中身が変わるたびに `reset` で測り直す —
//! 1 回の置き換えにつき全行の layout が 1 度走る｡今までは毎フレームだった
//! ものが､poll と reload のたびになる｡
//!
//! # 置き換えは 1 か所で検知する
//!
//! `self.state` を `Loaded` で置き換える経路は 7 つある (起動､fixture､
//! follow､reload､Load older､source の切り替え､削除)｡それぞれに
//! `reset` を書くと 1 つ漏れただけで古い高さのまま描く｡[`TimelineList::sync`]
//! が描画のたびに post の id の並びを前回と比べ､違っていれば `reset`
//! する｡500 件の文字列比較は 1 フレームの中では見えない｡
//!
//! `reset` は scroll 位置を捨てる｡anchor (`scroll_to_top_of_item` /
//! `scroll_to_item`) が積まれていればそれを置き､無ければ直前の位置を
//! 戻す — Load older の追記で先頭へ飛ばないため｡anchor を積むだけで
//! すぐ動かさないのは､呼び出し側が `state` を置き換えてから anchor を
//! 置く順で書かれているからで､すぐ動かすと `sync` の `reset` が消す｡
//! `ScrollHandle` も要求を次の prepaint まで積んでいたので､見える順序は
//! 変わらない｡

#[cfg(test)]
use gpui::Bounds;
use gpui::{IntoElement, ListAlignment, ListOffset, ListState, Pixels, Point, px};

use super::{
    Context, TimelineState, TimelineView, at_the_post_cap, cache, notice, offers_load_older,
};

/// viewport の上下に余分に測っておく高さ､px｡描くのは viewport の中だけで､
/// ここは高さの cache を温めるだけ｡`measure_all` で全行を測っているので
/// 効くのは行の高さが変わった直後の 1 フレームに限られ､大きくする理由は
/// 無い｡
const OVERDRAW: Pixels = px(200.);

/// [`ListState`] に積んでおく scroll の要求｡[`TimelineList::sync`] が
/// `reset` の後に置く｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Anchor {
    /// この index の行の上端を viewport の上端に合わせる｡
    TopOf(usize),
    /// この index の行が丸ごと見えるところまで､最小限だけ動かす｡
    Reveal(usize),
}

/// timeline の一覧の scroll 状態 (#301)｡`ScrollHandle` の後継｡
#[derive(Debug)]
pub(super) struct TimelineList {
    state: ListState,
    /// 直近の [`Self::sync`] で一覧に居た post の id｡並びごと比べる｡
    ids: Vec<String>,
    /// post の後ろに足した行の数 ("Load older" か上限の notice)｡
    trailing: usize,
    anchor: Option<Anchor>,
}

impl TimelineList {
    pub(super) fn new() -> Self {
        Self {
            state: ListState::new(0, ListAlignment::Top, OVERDRAW).measure_all(),
            ids: Vec::new(),
            trailing: 0,
            anchor: None,
        }
    }

    /// [`gpui::list`] へ渡す state｡`Rc` の clone なので同じものを指す｡
    pub(super) fn state(&self) -> ListState {
        self.state.clone()
    }

    /// 一覧の中身を `ListState` に追いつかせる｡描画のたびに､list を組む前に
    /// 呼ぶ｡`trailing` は post の後ろに足す行の数｡
    pub(super) fn sync<'a>(
        &mut self,
        ids: impl ExactSizeIterator<Item = &'a str> + Clone,
        trailing: usize,
    ) {
        let same = ids.len() == self.ids.len()
            && trailing == self.trailing
            && self
                .ids
                .iter()
                .zip(ids.clone())
                .all(|(kept, id)| kept == id);
        if !same {
            self.replace(ids.map(str::to_string).collect(), trailing);
        }
        if let Some(anchor) = self.anchor.take() {
            self.place(anchor);
        }
    }

    /// 中身が変わったときの `sync` の後半｡`ids` を覚え直し､`reset` して
    /// 位置を戻す｡
    fn replace(&mut self, ids: Vec<String>, trailing: usize) {
        let kept = self.state.logical_scroll_top();
        let count = ids.len().saturating_add(trailing);
        self.ids = ids;
        self.trailing = trailing;
        self.state.reset(count);
        if self.anchor.is_none() {
            self.state.scroll_to(kept);
        }
    }

    fn place(&self, anchor: Anchor) {
        match anchor {
            Anchor::TopOf(item_ix) => self.state.scroll_to(ListOffset {
                item_ix,
                offset_in_item: px(0.),
            }),
            Anchor::Reveal(item_ix) => self.state.scroll_to_reveal_item(item_ix),
        }
    }

    /// 今の scroll offset､px｡最上部が 0 で､下へ行くほど負 — `ScrollHandle`
    /// と同じ向き｡
    pub(super) fn offset(&self) -> Point<Pixels> {
        self.state.scroll_px_offset_for_scrollbar()
    }

    /// 末尾まで scroll したときの offset の大きさ､px (正)｡
    pub(super) fn max_offset(&self) -> Point<Pixels> {
        self.state.max_offset_for_scrollbar()
    }

    /// offset を置く｡`x` は無視する — 横には scroll しない｡
    ///
    /// `set_offset_from_scrollbar` ではなく `scroll_by` で動かす: 前者は
    /// layout の前 (`last_layout_bounds` が無い) には何もしないが､
    /// `ScrollHandle::set_offset` はいつでも効いた｡
    pub(super) fn set_offset(&self, offset: Point<Pixels>) {
        let current = self.offset().y;
        // `Pixels` の引き算は `arithmetic_side_effects` に弾かれるので f32 で｡
        self.state
            .scroll_by(px(f32::from(current) - f32::from(offset.y)));
    }

    /// viewport の上端に掛かっている行の index｡
    pub(super) fn top_item(&self) -> usize {
        self.state.logical_scroll_top().item_ix
    }

    /// viewport の上端に掛かっている行と､その行の中でのずれ｡
    pub(super) fn logical_scroll_top(&self) -> (usize, Pixels) {
        let top = self.state.logical_scroll_top();
        (top.item_ix, top.offset_in_item)
    }

    /// `ix` の行の上端を viewport の上端へ｡次の描画で効く｡
    pub(super) fn scroll_to_top_of_item(&mut self, ix: usize) {
        self.anchor = Some(Anchor::TopOf(ix));
    }

    /// `ix` の行が丸ごと見えるところまで最小限だけ動かす｡次の描画で効く｡
    pub(super) fn scroll_to_item(&mut self, ix: usize) {
        self.anchor = Some(Anchor::Reveal(ix));
    }

    /// `ix` の行が直近の描画で置かれた bounds｡viewport の上端より前の行と
    /// 測っていない行には `None`｡テストが行の高さを読むのに使う｡
    #[cfg(test)]
    pub(super) fn bounds_for_item(&self, ix: usize) -> Option<Bounds<Pixels>> {
        self.state.bounds_for_item(ix)
    }
}

/// post の後ろに足す行の数｡"Load older" (#11) と上限の notice は同時には
/// 出ない — 前者は上限の手前､後者は上限でだけ出る｡
pub(super) fn trailing_rows(
    next_page_token: Option<&str>,
    state: &TimelineState,
    single_source: bool,
) -> usize {
    usize::from(offers_load_older(next_page_token, state, single_source) || at_the_post_cap(state))
}

impl TimelineView {
    /// `render` の頭で [`TimelineList::sync`] を呼ぶ｡`body` は `&self` なので
    /// そこでは追いつかせられない｡
    pub(super) fn sync_list(&mut self) {
        let TimelineState::Loaded(items) = &self.state else {
            return;
        };
        let trailing = trailing_rows(
            self.next_page_token.as_deref(),
            &self.state,
            self.sources.len() == 1,
        );
        self.list_scroll
            .sync(items.iter().map(|item| item.id.as_str()), trailing);
    }

    /// list の `ix` 番目の行｡post の後ろは [`trailing_rows`] の行｡
    /// [`gpui::list`] が viewport に入る index だけを渡してくる｡
    pub(super) fn list_row(
        &self,
        ix: usize,
        bg_alpha: u8,
        cx: &mut Context<'_, Self>,
    ) -> gpui::AnyElement {
        let TimelineState::Loaded(items) = &self.state else {
            return gpui::Empty.into_any_element();
        };
        if let Some(item) = items.get(ix) {
            return self.post_row(item, bg_alpha, cx);
        }
        if at_the_post_cap(&self.state) {
            return notice(
                format!(
                    "Showing the most recent {} posts — that is as far back as twigpui keeps.",
                    cache::MAX_CACHED_POSTS
                ),
                self.theme.text_muted,
            )
            .into_any_element();
        }
        super::layout::load_older_row(self.theme, cx).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tests::{draw_until_parked, fixture_window, fixture_with};

    fn ids(range: std::ops::RangeInclusive<usize>) -> Vec<String> {
        range.map(|n| n.to_string()).collect()
    }

    /// `ids` を `sync` に渡す形で｡
    fn synced(list: &mut TimelineList, ids: &[String], trailing: usize) {
        list.sync(ids.iter().map(String::as_str), trailing);
    }

    // --- sync: 中身の変化の検知と位置 ---

    #[test]
    fn syncing_the_same_ids_keeps_the_state_and_the_position() {
        let mut list = TimelineList::new();
        synced(&mut list, &ids(1..=5), 0);
        list.state.scroll_to(ListOffset {
            item_ix: 3,
            offset_in_item: px(0.),
        });
        synced(&mut list, &ids(1..=5), 0);
        assert_eq!(list.state.item_count(), 5);
        assert_eq!(list.top_item(), 3, "an unchanged list is not reset");
    }

    #[test]
    fn appending_rows_keeps_the_reader_where_they_were() {
        let mut list = TimelineList::new();
        synced(&mut list, &ids(1..=5), 0);
        list.state.scroll_to(ListOffset {
            item_ix: 3,
            offset_in_item: px(0.),
        });
        // Load older: 後ろに 5 行足す｡
        synced(&mut list, &ids(1..=10), 0);
        assert_eq!(list.state.item_count(), 10);
        assert_eq!(list.top_item(), 3, "an append must not jump to the top");
    }

    #[test]
    fn a_queued_anchor_wins_over_the_kept_position() {
        let mut list = TimelineList::new();
        synced(&mut list, &ids(3..=5), 0);
        // follow: 前に 2 行差し込み､元の先頭 (今の index 2) を上端に駐める｡
        list.scroll_to_top_of_item(2);
        synced(&mut list, &ids(1..=5), 0);
        assert_eq!(list.top_item(), 2);
    }

    #[test]
    fn an_anchor_without_a_change_is_still_applied() {
        let mut list = TimelineList::new();
        synced(&mut list, &ids(1..=5), 0);
        list.scroll_to_top_of_item(4);
        synced(&mut list, &ids(1..=5), 0);
        assert_eq!(
            list.top_item(),
            4,
            "ScrollToTop / jump land on the next draw"
        );
    }

    #[test]
    fn the_trailing_row_counts_as_an_item() {
        let mut list = TimelineList::new();
        synced(&mut list, &ids(1..=5), 1);
        assert_eq!(list.state.item_count(), 6);
        synced(&mut list, &ids(1..=5), 0);
        assert_eq!(
            list.state.item_count(),
            5,
            "losing the trailing row is a change"
        );
    }

    #[test]
    fn set_offset_works_before_any_layout() {
        // glide と wheel は描画の合間に置く｡`set_offset_from_scrollbar` なら
        // layout 前は黙って何もしないが､`scroll_by` は動く — 行の高さが
        // 分かるまでは 0 に clamp されるだけだ｡
        let mut list = TimelineList::new();
        synced(&mut list, &ids(1..=5), 0);
        list.set_offset(gpui::point(px(0.), px(-10.)));
        assert!(f32::from(list.offset().y) <= 0.);
    }

    // --- 仮想化そのもの ---

    /// #301: 500 行を持っていても組むのは viewport に入る行だけ｡
    ///
    /// `debug_bounds` は一度描いた名前を消せないが､一度も描いていない名前
    /// には `None` を答える — 末尾の行が `None` なら､その行の要素は組まれて
    /// いない｡先頭の行は同じ frame で組まれているので､「何も描いていない」
    /// を「組まなかった」と読み違えることはない｡
    #[gpui::test]
    fn only_the_rows_in_the_viewport_are_built(cx: &mut gpui::TestAppContext) {
        let ids: Vec<String> = (1..=500).map(|n| n.to_string()).collect();
        let shown: Vec<&str> = ids.iter().map(String::as_str).collect();
        let (window, _timeline) = fixture_window(cx, fixture_with(&shown, &[]));
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        draw_until_parked(&mut visual, cx);

        assert!(
            visual.debug_bounds("post-row-1").is_some(),
            "the first row is on screen and has to be laid out"
        );
        assert!(
            visual.debug_bounds("post-row-500").is_none(),
            "the last row is far below the viewport and must not be built"
        );
    }
}
