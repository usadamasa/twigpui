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
//! 何を差し出すかはここでは決めない｡pending のバッファも glide も
//! [`super::auto_refresh`] のもので､押したときに起きること
//! ([`TimelineView::reveal_new_posts`]) はどちらの経路でもあちらの
//! [`TimelineView::start_glide`] を呼ぶだけ｡ここは見せ方と､2 つの経路
//! のうちどちらを呼ぶかの分岐を持つ｡

use super::auto_refresh::pending_label;
use super::fade::{FADE_STEP_MILLIS, Fade, fade_occupies, fade_opacity, fade_settled, next_fade};
use super::render::Addressable as _;
use super::{
    AnyElement, Context, Duration, FontWeight, InteractiveElement as _, IntoElement as _,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, TimelineView, div, px, rgb,
    rgba, theme,
};

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

/// capsule の縁 (#206)｡`rgba` なので下 8 bit が不透明度｡
///
/// accent の塗りの上に白を薄く引く｡明暗どちらのテーマでも accent は
/// 濃い青で､縁が無いと timeline の白や暗色の上で平らな四角に読める｡
const TOAST_EDGE: u32 = 0xffff_ff33;

/// toast のうち､ウィンドウの状態に触る半分 (#206)｡
impl TimelineView {
    /// toast が今言うべき件数 (#206)｡0 なら出さない｡
    fn unread(&self) -> usize {
        unread_count(
            self.pending.as_ref().map(|pending| pending.count),
            self.unseen,
        )
    }

    /// timeline の下端に重ねる toast (#206)｡出さないなら `None`｡
    ///
    /// 呼ぶのは `body` の wrapper｡scroll する一覧の外､band でずれない
    /// wrapper に `absolute` で寄せるので､一覧がどこへ scroll しても toast は
    /// 動かない｡外側の帯は幅いっぱいだが listener を持たない — gpui の
    /// hit test は listener の無い要素に hitbox を置かないので､capsule の
    /// 横のクリックは下の行へ届く｡
    ///
    /// 上向きの矢印は､押したらどの方向へ行くかを告げる (#21)｡濃さと
    /// 持ち上がりは同じ [`Fade`] から引く ([`toast_drop_px`])｡
    pub(super) fn toast(&self, cx: &mut Context<'_, Self>) -> Option<AnyElement> {
        if !fade_occupies(self.toast.fade) {
            return None;
        }
        let theme = self.theme;
        let fade = self.toast.fade;
        Some(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom(px(TOAST_INSET_PX - toast_drop_px(fade)))
                .flex()
                .justify_center()
                .child(
                    div()
                        .addressable("new-posts")
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .px_3()
                        .py_1p5()
                        .rounded_full()
                        .bg(rgb(theme.accent))
                        .border_1()
                        .border_color(rgba(TOAST_EDGE))
                        .shadow_md()
                        .text_color(rgb(theme.button_label))
                        .text_size(theme::TEXT_META)
                        .font_weight(FontWeight::SEMIBOLD)
                        .opacity(fade_opacity(fade))
                        .cursor_pointer()
                        .hover(gpui::Styled::shadow_lg)
                        .child("↑")
                        .child(pending_label(self.toast.count))
                        .on_click(
                            cx.listener(|this, _event, _window, cx| this.reveal_new_posts(cx)),
                        ),
                )
                .into_any_element(),
        )
    }

    /// フェードを今の件数が求める向きへ歩かせる (#206)｡
    ///
    /// `render` の頭で毎フレーム呼ぶ｡sync の行は `show_sync` という 1 つの
    /// 戸口を持つが､toast の件数は 2 つの出所と 6 つの書き手 (poll､fixture､
    /// follow､glide の各フレーム､ホイール､最上部への跳び) を持ち､その
    /// すべてに「フェードを見直せ」を置くと 1 つ忘れた瞬間に toast が
    /// 取り残される｡描画はそのどれの後にも必ず来る｡
    ///
    /// 目的地に着いていればタイマーを持たず､走っていれば触らない — タイマー
    /// が毎段件数を読み直すので､向きが変わっても新しいタイマーは要らない｡
    /// 描画のたびに段を踏まないのもそのためだ: glide の 60fps に引きずられて
    /// 30ms の刻みが 16ms になる｡踏むのは 1 段目だけで､それは
    /// `fade_sync_row` と同じ理由 — タイマーを待つと最初の 30ms が何も
    /// 起きないフレームになる｡
    pub(super) fn fade_toast(&mut self, cx: &mut Context<'_, Self>) {
        let unread = self.unread();
        self.toast = self.toast.observe(unread);
        if self.toast.settled_for(unread) {
            self.toast_fade_task = None;
            return;
        }
        if self.toast_fade_task.is_some() {
            return;
        }
        self.toast = self.toast.ticked(unread);
        if self.toast.settled_for(unread) {
            return;
        }
        self.toast_fade_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(FADE_STEP_MILLIS))
                    .await;
                // `Err` はウィンドウが消えたということ｡
                let Ok(settled) = this.update(cx, |this, cx| {
                    let unread = this.unread();
                    this.toast = this.toast.ticked(unread);
                    cx.notify();
                    if this.toast.settled_for(unread) {
                        // 落ち着いた toast がフレームを焚き続けない｡
                        // `drive_scroll` が自分のスロットを空けるのと同じ｡
                        this.toast_fade_task = None;
                        true
                    } else {
                        false
                    }
                }) else {
                    return;
                };
                if settled {
                    return;
                }
            }
        }));
    }

    /// toast のクリック (#206)｡差し出しているものを follow と同じ
    /// glide で画面へ合流させる｡
    ///
    /// バッファがあれば [`Self::apply_pending`] — バーのクリックと `⌘⇧R` が
    /// 通ってきた経路そのもので､リクエストは飛ばない｡無ければ follow が
    /// すでに流し込んだ行がまだ上にあるということなので､同じ glide を
    /// [`Self::start_glide`] で続きから再開する｡どちらも最上部へ跳ぶ
    /// のではなく､読める速さで降りてくる — 跳ぶのは `ScrollToTop` のみ｡
    ///
    /// 先頭で glide と scroll のモーションを手放すのは､読み手がホイールで
    /// 止めていた場合に備えるため｡それを残したまま新しい glide を始めると､
    /// 両方が同じ `list_scroll` の offset を取り合う｡
    pub(super) fn reveal_new_posts(&mut self, cx: &mut Context<'_, Self>) {
        self.glide = None;
        self.scroll_motion = None;
        self.scroller.release();
        if self.pending.is_some() {
            self.apply_pending(cx);
        } else if self.unseen > 0 {
            // バッファは無いので置き換えるリストも新しい anchor も無い｡
            // offset はすでに歩き出す場所にあるので待たない｡
            self.start_glide(cx, None);
        }
    }

    /// 最上部へ跳ぶ (#22)｡`ScrollToTop` から — トーストの押下は
    /// [`Self::reveal_new_posts`] を経由し､跳ばずに glide へ合流する｡
    ///
    /// 完全にローカル — リクエストもゲートも無いし､報告することも無い｡
    /// ピクセルのオフセットではなく `scroll_to_top_of_item(0)` にしてある
    /// のは､最新の行そのものへ着地させるためだ｡進行中の glide も同じ場所へ
    /// 歩いている — ジャンプがそれに取って代わる｡ホイールの目標も同じ
    /// (#175): 飛んだ先から古い目標へ引き戻してはいけない｡上に残っていた
    /// 新着も全部視界に入るので､countdown は 0 (#206)｡
    pub(super) fn jump_to_top(&mut self, cx: &mut Context<'_, Self>) {
        self.glide = None;
        self.scroll_motion = None;
        self.scroller.release();
        self.list_scroll.scroll_to_top_of_item(0);
        self.unseen = 0;
        cx.notify();
    }

    /// scroll 位置が動いた (#206)｡follow の countdown をそこまで進める｡
    ///
    /// glide と読み手のホイールの両方から呼ぶ｡`logical_scroll_top` は直近の
    /// prepaint の行の bounds と今の offset から数えるので､一覧を置き換えた
    /// 直後の 1 フレームだけは古い行を基準に答える｡glide は補正の着地を
    /// 待ってから呼ぶ (`start_glide` の `SETTLE_FRAMES`)｡ホイールはその窓で
    /// 読み手が握ったときだけ 1 回ずれうるが､握った時点で数は読み手の
    /// ものなので､そのための仕掛けは置かない｡
    pub(super) fn note_scroll_position(&mut self) {
        let (top_item, _) = self.list_scroll.logical_scroll_top();
        self.unseen = unseen_after_scroll(self.unseen, top_item);
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
