//! 窓の枠の部品 (#241): バナー､notice､footer の segment､usage の行､
//! composer のエラー行｡

use crate::ui::*;

/// sign-in flow を始める輪郭だけの pill｡呼び出し側は 2 つ (#282 で
/// [`reauthorize_banner`] の `reauthorize` が加わった): body の
/// `sign-in-body` (session がまだ無い) と `reauthorize` (scope が足りない)｡
///
/// #31 (app-only の bearer token からの脱却) と #14 (セッションが
/// `tweet.write` より前のもの) は同じ場所へ至る別々の理由なので､二つの
/// ボタンはラベルだけが違う — 歩調を合わせつづけねばならないほぼ同一の
/// builder chain を二つ持つより､helper を一つ持つ価値がある｡
pub(in crate::ui) fn sign_in_pill(
    id: &'static str,
    label: &'static str,
    theme: Theme,
    cx: &mut Context<'_, TimelineView>,
) -> impl IntoElement {
    div()
        .addressable(id)
        .px_2()
        .py_1()
        .rounded(theme::RADIUS_CONTROL)
        .border_1()
        .border_color(rgb(theme.accent))
        .text_color(rgb(theme.accent))
        .cursor_pointer()
        // #156: 下地が無い輪郭 pill なので `rgba` をそのまま塗る —
        // accent の主ボタンと違って合成の計算は要らない｡
        .hover(|style| style.bg(rgba(theme.control_hover_overlay)))
        .active(|style| style.bg(rgba(theme.control_pressed_overlay)))
        .child(label)
        .on_click(cx.listener(|this, _event, _window, cx| this.sign_in(cx)))
}

pub(in crate::ui) fn notice(message: impl Into<SharedString>, color: u32) -> impl IntoElement {
    div()
        .px_4()
        .py_3()
        .text_color(rgb(color))
        .child(message.into())
}

/// 常設の「セッションが切れた」バナー (#54): [`TimelineView::body`] の
/// `state` を鍵にした match へ畳み込むのではなく､`body` とは独立した
/// バナーの列 (`notice_banners`) に住む行だ — 眼目は､`body` がまったく
/// 正常に読み込まれた timeline を描いている間 (bearer token への fallback
/// 時) でも出しつづけ
/// ねばならない点で､それこそ #54 が起票された状態そのものだ｡
/// `name` は #184 の呼び名だ｡3 人の呼び出し側が同じ姿のバナーを描くので､
/// テストは「どれが出ているか」を名前でしか見分けられない｡
pub(in crate::ui) fn session_notice_banner(
    name: &'static str,
    message: SharedString,
    theme: Theme,
    bg_alpha: u8,
) -> impl IntoElement {
    div()
        .addressable(name)
        .px_4()
        .py_2()
        // #267: 本体と同じ不透明度で — 帯だけ不透明に残さない｡
        .bg(rgba(theme::with_alpha(theme.bg_header, bg_alpha)))
        .border_b_1()
        .border_color(rgb(theme.border))
        .text_color(rgb(theme.danger))
        .child(message)
}

/// reload の cooldown/失敗のバナー (#57) — [`session_notice_banner`] と
/// まったく同じ体裁で､そのすぐ隣に描く｡あちらが `body` から独立している
/// のと同じ理由だ: cooldown や失敗した refresh が説明するのは直近の
/// *リクエスト* であって､いま表示されている (されていない) post ではなく､
/// 何かある時に「ここには何も無い」と読めては決してならない｡
pub(in crate::ui) fn reload_notice_banner(
    notice: &ReloadNotice,
    theme: Theme,
    now: i64,
    bg_alpha: u8,
) -> impl IntoElement {
    // #141: 言葉より先に色がこの行の種類を告げる｡成功を報じる variant は
    // `Outcome` だけで､他の二つと並べて `danger` で塗ると､終わった reload
    // が失敗したもののように見えてしまう｡
    let (message, color) = match *notice {
        ReloadNotice::Cooldown { reset_at, cooldown } => {
            (cooldown_label(cooldown, reset_at, now), theme.danger)
        }
        ReloadNotice::Failed(ref message) => (message.to_string(), theme.danger),
        ReloadNotice::Outcome(ref message) => (message.to_string(), theme.text_muted),
    };
    div()
        // #184 の呼び名｡`session_notice_banner` が名前を持つのと同じ理由で､
        // 並んだバナーはテストからは名前でしか見分けられない｡
        .addressable("banner-reload")
        .px_4()
        .py_2()
        // #267: 本体と同じ不透明度で — 帯だけ不透明に残さない｡
        .bg(rgba(theme::with_alpha(theme.bg_header, bg_alpha)))
        .border_b_1()
        .border_color(rgb(theme.border))
        .text_color(rgb(color))
        .child(message)
}

/// Re-authorize の誘導 (#14, #282) — [`session_notice_banner`] と同じ体裁
/// のバナーに､短い説明と既存の [`sign_in_pill`] を並べる｡footer は 429px
/// で余地が無く (#214)､輪郭付きの pill は 24px の帯に入らない｡出す条件は
/// [`offers_reauthorize`](super::offers_reauthorize) — header に居た頃と
/// 変えていない｡
pub(in crate::ui) fn reauthorize_banner(
    theme: Theme,
    bg_alpha: u8,
    cx: &mut Context<'_, TimelineView>,
) -> impl IntoElement {
    div()
        .addressable("banner-reauthorize")
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .py_2()
        // #267: 本体と同じ不透明度で — 帯だけ不透明に残さない｡
        .bg(rgba(theme::with_alpha(theme.bg_header, bg_alpha)))
        .border_b_1()
        .border_color(rgb(theme.border))
        .child(
            div()
                .text_color(rgb(theme.text_muted))
                .child("This session is missing a scope twigpui needs — re-authorize to grant it."),
        )
        .child(sign_in_pill("reauthorize", "Re-authorize", theme, cx))
}

/// footer の簡潔な usage 要約 (#162､#18 の後継): 数えるのは Posts の
/// resource 数で､リクエスト本数ではない — `usage::posts_totals` が既に
/// Posts kind だけへ絞っているので､ここは受け取った数をそのまま出す｡
/// 見積り金額 (USD) は常に添える: `post_resource_price` はもう既定値
/// (`config` の `DEFAULT_POST_RESOURCE_PRICE`) を持つので､「価格が未設定」
/// という状態は無くなった｡
/// #282: `Density::Compact` では主語 ("Posts today"､"total") を落とす —
/// footer が reload のアイコンとカウントダウンも抱えるようになり､429px で
/// この行が最初に譲る区画になったからだ｡数字と色 (`usage_color`) だけで
/// 予算の意味はもう運べている｡
pub(in crate::ui) fn usage_label(
    today: u64,
    total: u64,
    post_resource_price: f64,
    density: countdown::Density,
) -> String {
    let amount = usage::estimated_amount(today, post_resource_price);
    match density {
        countdown::Density::Wide => {
            format!("Posts today: {today} (~${amount:.2}) · total: {total}")
        }
        countdown::Density::Compact => format!("{today} (~${amount:.2}) · {total}"),
    }
}

/// usage の行をどの theme のスロットで描くか: 今日の件数が
/// `daily_post_budget` に近づくか超えると `warning`/`danger` になり､
/// [`usage::budget_status`] が返す深刻度に対応する; 立てる旗が無ければ､
/// timestamp や byline がすでに使っているのと同じ muted のスロットだ｡
pub(in crate::ui) fn usage_color(status: usage::BudgetStatus, theme: Theme) -> u32 {
    match status {
        usage::BudgetStatus::Ok => theme.text_muted,
        usage::BudgetStatus::Near => theme.warning,
        usage::BudgetStatus::Exceeded => theme.danger,
    }
}

/// composer のエラー行｡status が見せるものを持つときだけ出す (#14) —
/// `Idle`/`Submitting` では `None` なので､そのどちらの状態でも composer
/// は余分な行を描かない｡
pub(in crate::ui) fn compose_error_message(status: &ComposeStatus) -> Option<SharedString> {
    match status {
        ComposeStatus::Failed(message) => Some(SharedString::from(message.clone())),
        ComposeStatus::Idle | ComposeStatus::Submitting => None,
    }
}

// #31 の独立した "Sign in with X" ボタンは #33 とともに消えた｡在ったのは
// ただ一つの状況のためだ: app-only の bearer token での動作｡これは動いて
// いる状態なので主ボタンは "Reload" と言い､結果として OAuth flow へ他に
// 到達できなくなっていた｡あの credential が無ければ未署名の状態は
// `NotAuthenticated` だけで､そこでは *主* ボタンがすでに "Sign in with X"
// と言っている — そして同一のボタンが二つ並ぶことこそ､#31 がそもそも
// 避けようとしていたものだ｡
