//! 窓の枠の部品 (#241): バナー､notice､toolbar の表題と segment､usage の
//! 行､composer のエラー行｡

use crate::ui::*;

/// sign-in flow を始める､ヘッダの輪郭だけの pill｡
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
/// `state` を鍵にした match へ畳み込むのではなく､ヘッダと他のすべての
/// 間にある独立した行だ — 眼目は､`body` がまったく正常に読み込まれた
/// timeline を描いている間 (bearer token への fallback 時) でも出しつづけ
/// ねばならない点で､それこそ #54 が起票された状態そのものだ｡
/// `name` は #184 の呼び名だ｡3 人の呼び出し側が同じ姿のバナーを描くので､
/// テストは「どれが出ているか」を名前でしか見分けられない｡
pub(in crate::ui) fn session_notice_banner(
    name: &'static str,
    message: SharedString,
    theme: Theme,
) -> impl IntoElement {
    div()
        .addressable(name)
        .px_4()
        .py_2()
        .bg(rgb(theme.bg_header))
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
        .px_4()
        .py_2()
        .bg(rgb(theme.bg_header))
        .border_b_1()
        .border_color(rgb(theme.border))
        .text_color(rgb(color))
        .child(message)
}

/// ヘッダの簡潔な usage 要約 (#162､#18 の後継): 数えるのは Posts の
/// resource 数で､リクエスト本数ではない — `usage::posts_totals` が既に
/// Posts kind だけへ絞っているので､ここは受け取った数をそのまま出す｡
/// 見積り金額 (USD) は常に添える: `post_resource_price` はもう既定値
/// (`config` の `DEFAULT_POST_RESOURCE_PRICE`) を持つので､「価格が未設定」
/// という状態は無くなった｡
pub(in crate::ui) fn usage_label(today: u64, total: u64, post_resource_price: f64) -> String {
    let amount = usage::estimated_amount(today, post_resource_price);
    format!("Posts today: {today} (~${amount:.2}) · total: {total}")
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

/// ヘッダの表題 (#11): これが誰のアカウントの post か､そして — #11 が
/// 二つ目のモードを持ち込んだので — どのモードを出しているか｡自分の home
/// timeline を見ているのか一つのアカウントの post を見ているのかを､
/// ユーザーが推し量る羽目にならないようにするためだ｡
///
/// `home_username` が `None` になるのは `/me` が一度も解決していない短い
/// 間だけで (何かがキャッシュされたか読み込まれたら二度と起きない)､表題
/// がアカウントを名指せない唯一の場合だ｡
///
/// #33 までは `TimelineSource` を取っていた｡#33 でウィンドウは home
/// timeline 以外を出せなくなった — single-user の view が在ったのは
/// app-only の bearer token が home を読めなかったからだ｡
pub(in crate::ui) fn header_title(home_username: Option<&str>) -> String {
    match home_username {
        Some(username) => format!("@{username}"),
        // `/me` が解決するまで名指せるアカウントは無く､macOS のツールバーが
        // その場所に見せるのはアプリ自身の名前だ｡
        None => "twigpui".to_string(),
    }
}

/// ツールバーが描くままの [`header_title`] (#95)｡
///
/// ツールバー行の `gap` に頼らず自前の左マージンを持つ: あの gap では
/// タイトルが直前にあるものへぴたりと付いたままになる — picker の trough
/// や #164 の取得ボタンがそれで､最初の実機ウィンドウでは
/// `Load lists (1 request)@usadamasa` と出た｡#182 がステータスバーで同じ
/// ものを見つけ､同じやり方で直した｡ウィンドウのテストがこの間隔を測れる
/// よう名前を付けてある｡
pub(in crate::ui) fn header_title_element(
    home_username: Option<&str>,
    theme: Theme,
) -> impl IntoElement {
    div()
        .addressable("header-title")
        .ml(theme::ROW_PAD_X)
        .text_size(theme::TEXT_META)
        .text_color(rgb(theme.text_tertiary))
        .child(header_title(home_username))
}

/// pull-down のトリガー (#192, #43) とメニュー項目に共通の chip｡かつては
/// segmented control の 1 区画で `tab_trough` という一本のトラックへ並んで
/// いたが (#164)､#192/#43 でドロップダウンへ置き換わり trough は不要に
/// なった｡`selected` は今もトリガー自身の常時「持ち上がった」見た目
/// (`source_picker.rs::source_picker_trigger`) と､メニュー項目のチェック
/// 済み表現の両方に使う｡
pub(in crate::ui) fn tab_segment(label: &str, selected: bool, theme: Theme) -> Div {
    div()
        .px_2()
        .py_0p5()
        .rounded(px(4.0))
        .when(selected, |segment| {
            // 色を付けるだけでなくトラックから持ち上げる: 影が無いと
            // segment は素のテキストの傍らに置かれた枠付きの chip に
            // 読め､それはまったく別の control になってしまう｡
            segment
                .bg(rgb(theme.bg))
                .shadow_sm()
                .text_color(rgb(theme.text))
                .font_weight(FontWeight::MEDIUM)
        })
        .when(!selected, |segment| {
            segment.text_color(rgb(theme.text_muted))
        })
        .child(label.to_string())
}
