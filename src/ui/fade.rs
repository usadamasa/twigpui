//! 出入りする要素のフェード (#205, #206)｡時計ではなく段の数で持つ｡
//!
//! #205 が sync の行のために `sync_row` へ書いたものを､#206 の新着の
//! toast が 2 つ目の使い手になったのでここへ出した｡どちらも同じ問いに
//! 答えている: 出すか出さないかは別の状態が決め､画面がそこへどこまで
//! 追いついたかだけをこれが持つ｡
//!
//! gpui の `AnimationExt::with_animation` を使わない｡時計が要素の mount
//! 起点で動き､完了を知らせる口が無く､経過を要素の外から読めない｡消える
//! ほうのフェードはどのみち「いつ外すか」を自前で持つ必要がある｡
//!
//! 段で持てば遷移が純粋関数になり､経過時間を mock せずに済む｡進めるのは
//! 使い手のタイマー ([`super::TimelineView::fade_sync_row`] など) で､
//! 1 tick が 1 段｡

/// 出入りする要素が今どれだけ濃いか｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Fade {
    /// 要素が無く､場所も取らない｡
    Hidden,
    /// 場所を取っていて､`1..FADE_STEPS` 段だけ濃い｡
    Rising(u8),
    /// 完全に見えている｡
    Shown,
    /// まだ場所を取っていて､`1..FADE_STEPS` 段だけ薄い｡
    Falling(u8),
}

/// フェードを渡りきる段数 (#205)｡
///
/// 1 段が [`FADE_STEP_MILLIS`] なので端から端まで 180ms｡
/// 消えたと気づくには十分に速く､点滅と読まれるには十分に遅い｡
pub(super) const FADE_STEPS: u8 = 6;

/// 1 tick が [`Fade`] を進める長さ (#205)､ミリ秒｡[`FADE_STEPS`] 段で 180ms｡
///
/// `auto_refresh` の glide と同じく background executor の timer で刻む｡
/// ただし 1 段ずつ数えるので経過時間は読まない｡
pub(super) const FADE_STEP_MILLIS: u64 = 30;

/// 1 tick 進んだフェード (#205)｡
///
/// 途中で向きが変わっても 0 からやり直さず､今の濃さのまま向きだけ変える｡
/// sync の状態は 1 tick で往復しうる (`Applied` → `Idle { pending: 0 }`)
/// ので､やり直すと行が点滅する｡
pub(super) fn next_fade(fade: Fade, wants: bool) -> Fade {
    match (fade, wants) {
        (Fade::Hidden, false) | (Fade::Shown, true) => fade,
        (Fade::Hidden, true) => rising(1),
        (Fade::Shown, false) => falling(1),
        (Fade::Rising(step), true) => rising(step.saturating_add(1)),
        (Fade::Falling(step), false) => falling(step.saturating_add(1)),
        // 折り返し｡`FADE_STEPS - step` が同じ濃さを反対向きの段で言い直す
        // ([`fade_opacity`] を参照)｡
        (Fade::Rising(step), false) => falling(FADE_STEPS.saturating_sub(step)),
        (Fade::Falling(step), true) => rising(FADE_STEPS.saturating_sub(step)),
    }
}

/// 濃くなる途中の段｡渡りきったら [`Fade::Shown`]｡
fn rising(step: u8) -> Fade {
    if step >= FADE_STEPS {
        Fade::Shown
    } else {
        Fade::Rising(step)
    }
}

/// 薄くなる途中の段｡渡りきったら [`Fade::Hidden`]｡
fn falling(step: u8) -> Fade {
    if step >= FADE_STEPS {
        Fade::Hidden
    } else {
        Fade::Falling(step)
    }
}

/// この段の不透明度 (#205)｡
pub(super) fn fade_opacity(fade: Fade) -> f32 {
    match fade {
        Fade::Hidden => 0.0,
        Fade::Shown => 1.0,
        Fade::Rising(step) => ratio(step),
        Fade::Falling(step) => 1.0 - ratio(step),
    }
}

/// `step` 段目が [`FADE_STEPS`] のうち占める割合｡
fn ratio(step: u8) -> f32 {
    f32::from(step) / f32::from(FADE_STEPS)
}

/// 要素が場所を取っているかどうか (#205)｡
///
/// 場所の大きさはフェードの最中も変えない｡sync の行なら高さは
/// `theme::SYNC_ROW_HEIGHT` 固定で､高さも補間すると 1 フレームごとに
/// timeline が押し上げられ､読んでいる行が指の下で滑る｡動かすのは出現と
/// 消失の各 1 回だけ｡
pub(super) fn fade_occupies(fade: Fade) -> bool {
    !matches!(fade, Fade::Hidden)
}

/// これ以上 tick しても変わらないかどうか (#205)｡タイマーを止める条件｡
pub(super) fn fade_settled(fade: Fade) -> bool {
    matches!(fade, Fade::Hidden | Fade::Shown)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unwanted_hidden_element_stays_hidden_and_settled() {
        assert_eq!(next_fade(Fade::Hidden, false), Fade::Hidden);
        assert!(fade_settled(Fade::Hidden));
        assert!(!fade_occupies(Fade::Hidden));
    }

    #[test]
    fn a_wanted_element_rises_from_hidden_to_shown_in_bounded_steps() {
        let mut fade = Fade::Hidden;
        let mut seen = vec![fade_opacity(fade)];
        for _ in 0..FADE_STEPS.saturating_add(2) {
            fade = next_fade(fade, true);
            seen.push(fade_opacity(fade));
        }
        assert_eq!(fade, Fade::Shown);
        assert!(fade_settled(fade));
        // 単調に濃くなり､両端を外れない｡
        for pair in seen.windows(2) {
            let (before, after) = (pair[0], pair[1]);
            assert!(after >= before, "the fade went backwards: {seen:?}");
            assert!((0.0..=1.0).contains(&after), "out of range: {seen:?}");
        }
    }

    #[test]
    fn an_element_that_is_no_longer_wanted_falls_all_the_way_to_hidden() {
        let mut fade = Fade::Shown;
        for _ in 0..FADE_STEPS.saturating_add(2) {
            fade = next_fade(fade, false);
        }
        assert_eq!(fade, Fade::Hidden);
    }

    /// 要素は消えきるまで場所を空けない｡timeline を跳ねさせないための
    /// 不変条件｡
    #[test]
    fn a_falling_element_keeps_its_place_until_it_is_gone() {
        let mut fade = Fade::Shown;
        loop {
            fade = next_fade(fade, false);
            if fade == Fade::Hidden {
                break;
            }
            assert!(fade_occupies(fade), "{fade:?} let the timeline jump early");
        }
    }

    /// 折り返しが飛ばしてよい濃さの幅｡`1.0 - 5.0/6.0` と `1.0/6.0` は同じ
    /// 段を指すが f32 では一致しないので､等値ではなく「1 段未満しか動いて
    /// いない」で押さえる｡防ぎたい 0 からのやり直しは 1 段より桁違いに大きい｡
    const FADE_SLACK: f32 = 0.01;

    /// 落ちている途中で状態が戻ったら､0 からやり直さず今の濃さから戻る｡
    /// やり直すと点滅する｡
    #[test]
    fn a_fade_reversed_midway_resumes_from_where_it_is() {
        let falling = next_fade(Fade::Shown, false);
        let opacity = fade_opacity(falling);
        let reversed = next_fade(falling, true);
        assert!(
            fade_opacity(reversed) + FADE_SLACK >= opacity,
            "reversing dimmed the element: {opacity} -> {}",
            fade_opacity(reversed)
        );
        assert!(fade_occupies(reversed));
    }

    #[test]
    fn a_rise_reversed_midway_resumes_from_where_it_is() {
        let rising = next_fade(Fade::Hidden, true);
        let opacity = fade_opacity(rising);
        let reversed = next_fade(rising, false);
        assert!(
            fade_opacity(reversed) <= opacity + FADE_SLACK,
            "reversing brightened the element: {opacity} -> {}",
            fade_opacity(reversed)
        );
    }

    #[test]
    fn a_settled_fade_needs_no_further_ticks() {
        assert!(fade_settled(Fade::Shown));
        assert!(fade_settled(Fade::Hidden));
        assert!(!fade_settled(next_fade(Fade::Hidden, true)));
        assert!(!fade_settled(next_fade(Fade::Shown, false)));
    }
}
