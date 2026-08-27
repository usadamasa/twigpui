//! 新着を差し出す toast (#206)｡
//!
//! #21 の "N new posts" はヘッダと timeline の間のバーだった｡#206 で
//! timeline の下端に重なる capsule へ移し､出入りをフェードにし､follow が
//! 新着を流し込む間は「まだ視界の上にある数」を数え下げる｡
//!
//! 形は [`super::sync_row`] と同じ｡先に純粋な関数とその test — 何件を言うか､
//! scroll がその数をどう減らすか､濃さとラベルがどう進むか — 続いて
//! `impl TimelineView` が､それを画面に置きタイマーで進める｡
//!
//! 何を差し出すかはここでは決めない｡pending のバッファは
//! [`super::auto_refresh`] のもので､押したときに起きることも
//! ([`TimelineView::apply_pending`]) あちらのまま｡ここは見せ方だけ｡

use super::fade::{Fade, fade_opacity, fade_settled, next_fade};

/// 読み手にまだ届いていない新着の数 (#206)｡toast が言う件数｡
///
/// 出所は 2 つで､足して 1 つの数にする｡`pending` は pill の後ろで待つ
/// バッファの件数 ([`super::auto_refresh::Pending::count`])､`unseen` は
/// follow が流し込んだ行のうち glide がまだ視界へ降ろしていない数
/// ([`unseen_after_scroll`])｡前者は画面にある timeline を基準に数えた
/// ものなので､後者と重なることは無い — follow が流し込んだ行は画面に
/// *ある*｡
pub(super) fn unread_count(pending: Option<usize>, unseen: usize) -> usize {
    pending.unwrap_or(0).saturating_add(unseen)
}

/// follow が流し込んだ新着のうち､scroll 位置がまだ viewport の上に残して
/// いる数 (#206)｡
///
/// `top_item` は `ScrollHandle::logical_scroll_top` の 1 つ目 — viewport の
/// 上端の下にある行の index｡それより小さい index の行はまるごと上にあり､
/// 読み手はまだ見ていない｡減る一方で増えない: 一度視界に降りた行を､
/// 読み手が下へ scroll し直したからといって「新着」に戻さない｡
pub(super) fn unseen_after_scroll(unseen: usize, top_item: usize) -> usize {
    unseen.min(top_item)
}

/// toast の休止位置｡timeline の下端からの距離 (#206)､px｡
///
/// 最後の行のアクション列 (like / repost / 返信) に capsule が重ならない
/// ほど上げず､下端に貼りつかないほど下げない｡
pub(super) const TOAST_INSET_PX: f32 = 16.0;

/// 出るときに下から持ち上がる距離 (#206)､px｡
///
/// フェードだけだと「そこにあったものが濃くなった」に読める｡少し持ち
/// 上がると「届いた」に読める｡大きくすると toast が飛んでくる｡
const TOAST_RISE_PX: f32 = 8.0;

/// この濃さのとき toast が休止位置からどれだけ下にいるか (#206)､px｡
///
/// 濃さに比例させる — 濃さと位置が別々の時計を持つと､薄いまま着いたり
/// 濃いまま動いたりする｡消えるときは同じ道を戻る｡
pub(super) fn toast_drop_px(fade: Fade) -> f32 {
    TOAST_RISE_PX * (1.0 - fade_opacity(fade))
}

/// toast が今どう見えているか (#206)｡
///
/// 濃さと､ラベルが言う件数｡件数を別に持つのは消えていく間のため — 件数が
/// 0 になった瞬間に "0 new posts" と言い換えて薄くなると､何が終わったのか
/// 読む間が無い｡sync の行が最後の status を出したまま薄くなるのと同じ｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Toast {
    /// 濃さ｡
    pub fade: Fade,
    /// ラベルが言う件数｡0 にはならない — [`Self::observe`] を見よ｡
    pub count: usize,
}

impl Toast {
    /// 何も無い｡
    pub(super) const HIDDEN: Self = Self {
        fade: Fade::Hidden,
        count: 0,
    };

    /// 今の件数を見る (#206)｡濃さは動かさない｡
    ///
    /// 件数が 0 でなければラベルに写す — follow の countdown はフェードの
    /// tick を待たずに減るので､描くたびに写さないとラベルが遅れる｡0 なら
    /// 最後の件数を出したままにする｡
    pub(super) fn observe(self, count: usize) -> Self {
        if count > 0 {
            Self { count, ..self }
        } else {
            self
        }
    }

    /// 1 tick 進める (#206)｡件数が 0 なら消える向き､あれば出る向き｡
    pub(super) fn ticked(self, count: usize) -> Self {
        Self {
            fade: next_fade(self.fade, count > 0),
            ..self.observe(count)
        }
    }

    /// この件数に対して､これ以上 tick しても変わらないかどうか (#206)｡
    pub(super) fn settled_for(self, count: usize) -> bool {
        fade_settled(self.fade) && (count > 0) == (self.fade == Fade::Shown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fade::FADE_STEPS;

    // --- 件数 ---

    #[test]
    fn the_toast_counts_the_buffer_and_the_rows_still_above_the_viewport() {
        assert_eq!(unread_count(None, 0), 0);
        assert_eq!(unread_count(Some(2), 0), 2);
        assert_eq!(unread_count(None, 3), 3);
        assert_eq!(unread_count(Some(2), 3), 5);
    }

    #[test]
    fn scrolling_only_ever_lowers_the_unseen_count() {
        // 3 行が上にあり､viewport の上端は index 3 の行の中｡
        assert_eq!(unseen_after_scroll(3, 3), 3);
        // glide が 1 行降ろした｡
        assert_eq!(unseen_after_scroll(3, 2), 2);
        // 読み手が下へ戻っても､見た行は新着に戻らない｡
        assert_eq!(unseen_after_scroll(2, 10), 2);
        assert_eq!(unseen_after_scroll(0, 10), 0);
    }

    // --- 濃さとラベル ---

    #[test]
    fn a_count_raises_the_toast_and_zero_lowers_it() {
        let mut toast = Toast::HIDDEN;
        for _ in 0..FADE_STEPS.saturating_add(1) {
            toast = toast.ticked(3);
        }
        assert_eq!(toast.fade, Fade::Shown);
        assert_eq!(toast.count, 3);
        assert!(toast.settled_for(3));
        assert!(
            !toast.settled_for(0),
            "a shown toast with nothing to say must fall"
        );

        for _ in 0..FADE_STEPS.saturating_add(1) {
            toast = toast.ticked(0);
        }
        assert_eq!(toast.fade, Fade::Hidden);
        assert!(toast.settled_for(0));
        assert!(
            !toast.settled_for(1),
            "a hidden toast with something to say must rise"
        );
    }

    /// 消えていく間､ラベルは最後の件数を言い続ける｡
    #[test]
    fn a_falling_toast_keeps_its_last_count() {
        let shown = Toast {
            fade: Fade::Shown,
            count: 2,
        };
        let falling = shown.ticked(0);
        assert!(matches!(falling.fade, Fade::Falling(_)));
        assert_eq!(
            falling.count, 2,
            "the label must not turn into \"0 new posts\""
        );
        assert_eq!(falling.observe(0).count, 2);
    }

    /// follow の countdown はフェードの tick を待たずにラベルへ届く｡
    #[test]
    fn observing_a_new_count_moves_the_label_without_touching_the_fade() {
        let shown = Toast {
            fade: Fade::Shown,
            count: 3,
        };
        let observed = shown.observe(2);
        assert_eq!(observed.count, 2);
        assert_eq!(observed.fade, Fade::Shown);
    }

    /// 持ち上がりは濃さと同じ時計で動き､着いたら休止位置にいる｡
    #[test]
    fn the_toast_rises_into_place_as_it_darkens() {
        assert!((toast_drop_px(Fade::Shown)).abs() < f32::EPSILON);
        assert!((toast_drop_px(Fade::Hidden) - TOAST_RISE_PX).abs() < f32::EPSILON);
        let mut fade = Fade::Hidden;
        let mut last = toast_drop_px(fade);
        for _ in 0..FADE_STEPS {
            fade = next_fade(fade, true);
            let drop = toast_drop_px(fade);
            assert!(
                drop <= last,
                "the toast sank while rising: {last} -> {drop}"
            );
            last = drop;
        }
    }
}
