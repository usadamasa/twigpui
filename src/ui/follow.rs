//! ポーリングの新着 post を､pill の後ろで待たせずにそのまま画面へ流し込む
//! 経路 (#22) と､そのとき新しい行を視界へ降ろす glide (#208)｡
//!
//! [`super::auto_refresh`] から切り出した｡あちらはいつポーリングし､返って
//! きたものをバッファ ([`super::Pending`]) へ置くかを持つ｡こちらはバッファ
//! の中身を実際に画面へ出すときの動き — 最上部にいるかの判断､流し込み､
//! scroll offset を歩かせるループ — を持つ｡流し込みは pill の押下
//! ([`TimelineView::apply_pending`]) からも来るので､ポーリングのループとは
//! 別の機構として読める｡
//!
//! `impl` より上はすべて純粋で､gpui 抜きでユニットテストできる｡

// `use super::*` ではなく書き下す｡理由は `super::auto_refresh` の前置きと同じ｡
use super::{Context, Duration, Pending, TimelineState, TimelineView, px, scroll};

/// 厳密な最上部からどれだけ離れていても「最上部」と読めるか (#22)､
/// 単位は pixel｡ゼロではない: トラックパッドの弾きは offset をわずかに
/// 届かないところに残しうるし､その読み手は自分が最上部にいると思って
/// いる — 半 pixel で pill が出たら follow が壊れて見える｡
const AT_TOP_TOLERANCE_PX: f32 = 2.0;

/// 読み手が timeline の最上部にいるかどうか (#22)｡
/// `TimelineList::logical_scroll_top` の 2 つ組の答え — viewport の上端の
/// 下にある行の index と､その行のどこまで上端が入り込んでいるか — から
/// 決める｡
pub(super) fn at_top(top_item: usize, offset_in_item: gpui::Pixels) -> bool {
    top_item == 0 && f32::from(offset_in_item).abs() <= AT_TOP_TOLERANCE_PX
}

/// 最上部に貼り付く follow のための `TimelineView` の実行時スイッチ
/// (#22): `config.follow_new_posts` を種にし､View メニューで反転し､
/// ファイルへ書き戻すことは決してない — config が常設の設定で､こちらは
/// 今日の分だ｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FollowMode {
    /// 読み手が最上部にいるとき､ポーリングの新着 post がそのまま流れ込む｡
    Follow,
    /// scroll 位置に関わらず､どのポーリングも pill の後ろで待つ｡
    Pill,
}

impl FollowMode {
    /// `config.follow_new_posts` が種にするモード｡
    pub(super) fn from_config(follow_new_posts: bool) -> Self {
        if follow_new_posts {
            Self::Follow
        } else {
            Self::Pill
        }
    }

    /// View メニューのトグルがすること｡
    pub(super) fn flipped(self) -> Self {
        match self {
            Self::Follow => Self::Pill,
            Self::Pill => Self::Follow,
        }
    }

    /// これがスイッチの [`Self::Follow`] 側かどうか｡
    pub(super) fn is_following(self) -> bool {
        matches!(self, Self::Follow)
    }
}

/// ポーリングの新着 post が pill の後ろで待つのではなく､そのまま画面へ
/// 流れ込むべきかどうか (#22, #177)｡
///
/// 3 つ揃うか､さもなくば無しだ｡モードは読み手の常設の指示｡`loaded` は
/// `Failed`/`Loading` の画面が､誰も見たいと頼んでいないポーリングに黙って
/// 置き換えられるのを防ぐ｡そして `at_top` が「いちばん新しいものを見せろ」
/// と「ここを読んでいる」を分ける — `preserved_scroll_target` が反対側から
/// 引くのと同じ線だ｡
pub(super) fn follows(mode: FollowMode, loaded: bool, at_top: bool) -> bool {
    mode.is_following() && loaded && at_top
}

/// glide が新しい行を流し込む速さ (#208)､px/s｡
///
/// #22 の最初の glide は毎フレーム残り距離の 15% を進んでいた｡画面 1 枚
/// ぶんが 1 秒足らずで通り過ぎ､流れてくる行を目で追えなかった｡これは
/// 「読みながら流れる」ための速さで､post 1 件 (150px 前後) が 0.6 秒ほど
/// かけて視界へ降りてくる｡
const GLIDE_SPEED_PX_PER_S: f32 = 240.;

/// glide の最短時間 (#208)､秒｡数十 px の小さな到着でも一瞬で済ませず､
/// 動いたと分かるだけの時間をかける｡
const GLIDE_MIN_S: f32 = 0.6;

/// glide の最長時間 (#208)､秒｡何十件も一度に来たときに速さの計算どおり
/// 十数秒も歩かせない — その先は読み手が握って止めるより先に終わるべき
/// 長さだ｡
const GLIDE_MAX_S: f32 = 5.;

/// glide をやめて最後の 1 pixel 未満を吸着させてよいだけ最上部に近い
/// 距離 (#22)｡
const GLIDE_DONE_PX: f32 = 1.0;

/// `distance` px を歩く glide にかける時間 (#208)､秒｡距離に比例させ､
/// [`GLIDE_MIN_S`] と [`GLIDE_MAX_S`] で挟む｡向きは問わない｡
pub(super) fn glide_duration_s(distance: f32) -> f32 {
    (distance.abs() / GLIDE_SPEED_PX_PER_S).clamp(GLIDE_MIN_S, GLIDE_MAX_S)
}

/// `start` から歩き始めて `elapsed_s` 秒後に glide が置く scroll offset､
/// または glide が終わっていれば `None` (#22, #208)｡offset は gpui が
/// 持っているもので､最上部で 0､読み手が下へ行くほど負の方向に大きくなる｡
///
/// フレームの回数ではなく経過時間の関数なので､timer が遅れても位置が
/// 飛ぶだけで終点は変わらない (#175 の「実行環境によって終了位置が
/// 変わらない」)｡両端を緩める smoothstep で､動き出しも着地も急がない｡
pub(super) fn glide_y(start: f32, elapsed_s: f32) -> Option<f32> {
    if start.abs() <= GLIDE_DONE_PX {
        return None;
    }
    let duration = glide_duration_s(start);
    if elapsed_s >= duration {
        return None;
    }
    let t = elapsed_s / duration;
    let eased = t * t * (3. - 2. * t);
    Some(start * (1. - eased))
}

/// follow のうち純粋になれない半分: バッファを画面へ合流させることと､
/// そのあと offset を歩かせるループ｡子モジュールは親の非公開項目を
/// 見られるので､`TimelineView` のフィールドは `ui` に閉じたままでよい｡
impl TimelineView {
    /// ポーリングの新着 post をバッファから画面へ合流させる (#22) —
    /// 読み手が最上部にいる場合は [`Self::present_poll`] から直接､それ以外は
    /// トーストの押下 ([`Self::apply_pending`]) から呼ばれる｡バッファが
    /// 空になる経路のうち､丸ごと流し込む唯一の経路｡
    ///
    /// 置き換えそのものは何も動かさない: viewport の上端の下にあった行は
    /// 新しいリストでは index `count` にあり､それを最上部へ戻して駐める
    /// ことで到着が見えなくなる｡そのあと読み手が見るのは glide — 新しい行
    /// が目で追える速さで視界へ滑り降りてくる｡それが #177 の "always
    /// flowing" の印象で､ポーリングがすでに支払った post でできている｡
    pub(super) fn follow(&mut self, pending: Pending, cx: &mut Context<'_, Self>) {
        // ここから先の `list_scroll` を動かすのは読み手ではない｡
        self.release_scroll();
        let count = pending.count;
        // 下の `scroll_to_top_of_item` で動く前の offset｡トースト経由では
        // 0 とは限らず､`start_glide` が anchor の着地を測る基準点になる｡
        let before = f32::from(self.list_scroll.offset().y);
        // 前のポーリングが駐めたバッファはこれより古く､しかも今まさに
        // 置き換えられる timeline を基準に測られている｡
        self.clear_pending();
        let nothing_was_kept = count == pending.items.len();
        self.state = TimelineState::Loaded(pending.items);
        if nothing_was_kept {
            // どの行も新しい — 空の List が初めて埋まるか､重なりの無い
            // 先頭ページか｡その場に留めるべき行が無いので､下の補正は
            // リストの末尾より後ろの index を名指しすることになる｡
            // `ListState::scroll_to` はそれを末尾に clamp するので (#301)､
            // 読み手は一番古い行へ飛ばされる｡代わりに glide 無しで最上部に
            // 着地する: glide は読んでいる行より上の行を見せることであり､
            // ここにはそんな行が無い｡
            self.list_scroll.scroll_to_top_of_item(0);
        } else {
            self.list_scroll.scroll_to_top_of_item(count);
            // #206: 新しい行は全部 viewport の上に駐まっている｡glide が
            // 1 行降ろすたびに `note_scroll_position` が減らす｡
            self.unseen = count;
            self.start_glide(cx, Some(before));
        }
        self.refresh_images(cx);
        cx.notify();
    }

    /// scroll offset を 1 フレームずつ最上部まで歩いて戻す (#22)｡
    ///
    /// `settle_from` が `Some(before)` なら､歩く距離はこれが呼ばれた時点では
    /// まだそこに無い: [`Self::follow`] の `scroll_to_top_of_item` は次の
    /// 描画の頭 (`sync_list`､#301) で着地する｡だからループは最初の数フレームを､offset が
    /// `before` から動くのを待つのに使う｡回数には上限があり､決して着地
    /// しない補正 (空のリスト､描画をやめたウィンドウ) は､ハングではなく
    /// pill がやるのと同じ吸着に落ちる｡
    ///
    /// `None` なら待たない: すでに動いていた glide をトーストの押下で
    /// 再開する経路 ([`TimelineView::reveal_new_posts`]) はリストを
    /// 置き換えず新しい anchor も置かないので､offset は呼ばれた時点で
    /// すでに歩き出す場所にある｡
    ///
    /// どのステップも､offset が今どこにあるかを前のステップが置いた
    /// ところと比べる｡差があれば読み手がホイールを回しているということで､
    /// glide はスクロールバーを取り合うのではなく読み手が置いたところで
    /// 止まる — [`Self::apply_poll`] に置き換えではなくバッファを選ばせた
    /// のと同じ譲り方だ｡ホイールの経路 (#175) は glide を drop する
    /// ことでも同じ結果を先に出す; ここの比較はその裏の保険である｡
    ///
    /// 時刻は壁時計ではなくフレームごとに [`scroll::FRAME_S`] を足して
    /// 数える (#208)｡テストの executor は timer の時計だけを進めるので､
    /// `Instant` で測ると 1 フレームが数マイクロ秒になり glide が永遠に
    /// 終わらない｡
    pub(super) fn start_glide(&mut self, cx: &mut Context<'_, Self>, settle_from: Option<f32>) {
        /// 補正が決して着地しないと結論づけるまでに､何フレーム待つか｡
        const SETTLE_FRAMES: u8 = 10;
        /// glide が置いたところから offset がどれだけ離れていたら読み手の
        /// scroll と読むか､単位は pixel｡`before` が 0 に近ければ今までの
        /// `GLIDE_DONE_PX` の判定と同じ意味になる｡
        const GRAB_PX: f32 = 1.0;

        // この先の offset は glide のもの｡呼び出し側の手放しを当てにしない｡
        self.release_scroll();
        self.glide = Some(cx.spawn(async move |this, cx| {
            let frame = Duration::from_secs_f32(scroll::FRAME_S);
            if let Some(before) = settle_from {
                for _ in 0..SETTLE_FRAMES {
                    cx.background_executor().timer(frame).await;
                    // `Err` はウィンドウが消えたということ — ここも以下も
                    // `start_auto_refresh` の約束｡
                    let Ok(settled) = this.update(cx, |this, _| {
                        (f32::from(this.list_scroll.offset().y) - before).abs() > GRAB_PX
                    }) else {
                        return;
                    };
                    if settled {
                        break;
                    }
                }
            }
            let Ok(start) = this.update(cx, |this, _| f32::from(this.list_scroll.offset().y))
            else {
                return;
            };
            let mut elapsed_s = 0.0_f32;
            let mut last_set: Option<f32> = None;
            loop {
                let Ok(done) = this.update(cx, |this, cx| {
                    let offset = this.list_scroll.offset();
                    let y = f32::from(offset.y);
                    if let Some(expected) = last_set
                        && (y - expected).abs() > GRAB_PX
                    {
                        return true;
                    }
                    if let Some(next) = glide_y(start, elapsed_s) {
                        this.list_scroll.set_offset(gpui::point(offset.x, px(next)));
                        this.note_scroll_position();
                        last_set = Some(next);
                        cx.notify();
                        false
                    } else {
                        this.list_scroll.set_offset(gpui::point(offset.x, px(0.)));
                        // 最上部に着いた｡上に残っている行は無い (#206)｡
                        this.unseen = 0;
                        cx.notify();
                        true
                    }
                }) else {
                    return;
                };
                if done {
                    return;
                }
                cx.background_executor().timer(frame).await;
                elapsed_s += scroll::FRAME_S;
            }
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- #22: 最上部に貼り付く follow ---

    #[test]
    fn the_reader_at_the_exact_top_is_at_the_top() {
        assert!(at_top(0, px(0.)));
    }

    // 許容量はトラックパッドのためのもので､offset を最上部からわずかに
    // ずらして残す — その読み手は自分が最上部にいると思っているし､半
    // pixel のせいで pill が出たら follow が壊れて見える｡
    #[test]
    fn a_hair_below_the_top_still_counts() {
        assert!(at_top(0, px(-1.5)));
    }

    #[test]
    fn a_reader_scrolled_into_the_first_row_is_not_at_the_top() {
        assert!(!at_top(0, px(-40.)));
    }

    #[test]
    fn a_reader_rows_down_is_not_at_the_top_whatever_the_pixel_says() {
        assert!(!at_top(3, px(0.)));
    }

    // follow には 3 つすべてが要る: スイッチが入っていること､前に足す
    // timeline があること､そして位置が「いちばん新しいものを見せろ」と
    // 言っている読み手｡どれか 1 つでも欠ければ pill に落ちる｡
    #[test]
    fn follow_needs_the_switch_a_loaded_timeline_and_a_reader_at_the_top() {
        assert!(follows(FollowMode::Follow, true, true));
        assert!(
            !follows(FollowMode::Pill, true, true),
            "switched off means the pill"
        );
        assert!(
            !follows(FollowMode::Follow, false, true),
            "nothing loaded means the pill"
        );
        assert!(
            !follows(FollowMode::Follow, true, false),
            "scrolled down means the pill"
        );
    }

    #[test]
    fn the_toggle_flips_between_the_two_modes_and_back() {
        assert_eq!(FollowMode::Follow.flipped(), FollowMode::Pill);
        assert_eq!(FollowMode::Pill.flipped(), FollowMode::Follow);
    }

    // --- #208: glide の速さ ---

    // glide は時刻の関数で､上へしか動かず､最上部を越えない｡フレームを
    // 何回刻んだかではなく経過時間で位置が決まるので､timer の揺れは速さを
    // 乱すだけで終点を動かさない｡
    #[test]
    fn a_glide_moves_monotonically_toward_the_top_without_overshooting() {
        let start = -1_000.0_f32;
        let mut previous = start;
        let mut t = 0.0_f32;
        while let Some(y) = glide_y(start, t) {
            assert!(
                y >= previous,
                "the glide must not turn back at t={t}: {y} < {previous}"
            );
            assert!(
                y <= 0.,
                "the glide must not overshoot the top at t={t}: {y}"
            );
            previous = y;
            t += 0.016;
        }
        assert!(
            previous > -50.,
            "by the time the glide reports done it must be nearly at the top, was {previous}"
        );
    }

    #[test]
    fn a_glide_is_finished_once_its_duration_has_passed() {
        let duration = glide_duration_s(-1_000.);
        assert!(
            glide_y(-1_000., duration).is_none(),
            "at the duration the glide is over"
        );
        assert!(
            glide_y(-1_000., duration * 0.5).is_some(),
            "halfway through it is still walking"
        );
        assert!(glide_y(0., 0.).is_none(), "nothing to walk from the top");
        assert!(
            glide_y(-0.5, 0.).is_none(),
            "half a pixel is not worth a frame"
        );
    }

    // 読める速さ (#208): 1 行ぶん (150px 程度) でも一瞬では済ませず､画面
    // 1 枚ぶんは数秒かけ､どれだけ遠くても上限で打ち切る｡
    #[test]
    fn a_glide_paces_itself_by_distance_between_a_floor_and_a_ceiling() {
        let one_row = glide_duration_s(-150.);
        let a_screenful = glide_duration_s(-800.);
        let far = glide_duration_s(-30_000.);
        assert!(
            one_row >= 0.5,
            "one row must not flash past, took {one_row}s"
        );
        assert!(
            a_screenful > one_row && a_screenful >= 2.,
            "a screenful must take visibly longer than a row, took {a_screenful}s"
        );
        assert!(far <= 6., "a huge batch must still end, took {far}s");
        assert!(
            (glide_duration_s(-800.) - glide_duration_s(800.)).abs() < f32::EPSILON,
            "pace depends on distance, not direction"
        );
    }

    // #175 の要求でもある: フレーム数や実行環境によって終了位置が変わらない｡
    // 60Hz と 30Hz で同じ時刻を刻めば同じ場所にいる｡
    #[test]
    fn a_glide_is_at_the_same_place_regardless_of_frame_rate() {
        let start = -1_000.0_f32;
        let at_60hz = glide_y(start, 0.016 * 30.);
        let at_30hz = glide_y(start, 0.032 * 15.);
        match (at_60hz, at_30hz) {
            (Some(a), Some(b)) => {
                assert!(
                    (a - b).abs() < 0.001,
                    "same elapsed time, same offset: {a} vs {b}"
                );
            }
            other => unreachable!("half a second in, both should still be gliding: {other:?}"),
        }
    }
}
