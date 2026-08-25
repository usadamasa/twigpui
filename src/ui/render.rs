//! 一つの timeline item を要素へ変える部品と､それらの要素が何と言うかを
//! 決める純粋関数｡
//!
//! `ui` から切り出した (#126) のは `src/ui.rs` がサイズの天井に達して
//! 余裕が無くなったからで､こちらは `TimelineView` の状態に一切触れない
//! 側だ: 行がすでに持っているデータの上の自由関数である｡ここのすべてが
//! `pub(crate)` ではなく `pub(super)` なのは — 呼ぶのは `ui` だけであり､
//! 可視性を広げれば分割が台無しになるからだ｡
//!
//! reload を走らせてよい *かどうか* の判断と､失敗したとき何と言うかは
//! 代わりに [`super::reload_policy`] に住む｡

use super::*;

/// 一つの要素に､gpui とテストの双方が使える名前を一つ与える｡
///
/// これは accessibility ではなく *テスト* のための addressability だ｡
/// gpui 0.2.2 は accessibility tree をまったく持たない — AccessKit も
/// role も無く､X というボタンがどこにあるかをウィンドウへ尋ねる手段も
/// 無い — のでここから screen reader へ届くものは何も無く､ARIA 相当と
/// 呼ぶのは大幅に言い過ぎになる｡
///
/// crate が実際に持っているのは `debug_selector` で､テストが引ける名前
/// ([`gpui::VisualTestContext::debug_bounds`]) の下に要素が実際どこへ
/// 配置されたかを記録し､`cargo test` の外では何にもコンパイルされない｡
/// このモジュールの対話的な要素はどれもすでに一意な `.id(..)` を持って
/// いる; この trait が無いとテスト用に名前を付けるにはその文字列をもう
/// 一度書くことになり､一つの要素に二つの名前があれば､どちらかが編集され
/// た最初の瞬間にずれる｡`addressable` は一度だけ書く｡
///
/// そもそも bounds を持つ意義は #184 にある: テストはその中心をクリック
/// でき､それによって `dispatch_action` が飛ばす唯一の段である gpui の
/// hit test を､座標をどこにも書かずに assert 下へ置ける｡
pub(super) trait Addressable: InteractiveElement + Sized {
    /// この要素に gpui の対話性のための名前と､テストが引く名前を与える｡
    fn addressable(self, name: impl Into<SharedString>) -> gpui::Stateful<Self> {
        let name = name.into();
        // selector が先: これは `Self` を返すが､`id` は消費して
        // `Stateful` にする｡どちらも同じ `Interactivity` へ書くので､
        // 順序は他に何も変えない｡
        self.debug_selector({
            let name = name.clone();
            move || name.to_string()
        })
        .id(gpui::ElementId::Name(name))
    }
}

impl<E: InteractiveElement> Addressable for E {}

/// sign-in flow を始める､ヘッダの輪郭だけの pill｡
///
/// #31 (app-only の bearer token からの脱却) と #14 (セッションが
/// `tweet.write` より前のもの) は同じ場所へ至る別々の理由なので､二つの
/// ボタンはラベルだけが違う — 歩調を合わせつづけねばならないほぼ同一の
/// builder chain を二つ持つより､helper を一つ持つ価値がある｡
pub(super) fn sign_in_pill(
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
        .child(label)
        .on_click(cx.listener(|this, _event, _window, cx| this.sign_in(cx)))
}

pub(super) fn notice(message: impl Into<SharedString>, color: u32) -> impl IntoElement {
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
pub(super) fn session_notice_banner(message: SharedString, theme: Theme) -> impl IntoElement {
    div()
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
pub(super) fn reload_notice_banner(
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

/// auto-refresh が差し出す「N new posts」のバー (#21)｡
///
/// timeline の中の行ではなくヘッダと timeline の間のバーにしてある｡それ
/// が差し出しと割り込みの違いだ｡scroll する一覧の中なら一番上に座ること
/// になり — 下へ scroll した人からは見えない｡auto-refresh はまさにその
/// 読み手のために在る｡ここなら動かず､上の二つのバナーの隣にいて､下の
/// timeline は押されるまでまったく動かない｡
///
/// 隣の二つと違い､ヘッダ自身の背景の上に `accent` で塗る:
/// `session_notice_banner` と `reload_notice_banner` は起きたことを報じる
/// が､この列の中でボタンなのはこの帯だけだ｡上向きの矢印は､押したらどの
/// 方向へ行くかを告げる｡読み手を先頭へ戻しもするからだ｡
pub(super) fn new_posts_bar(
    count: usize,
    theme: Theme,
    cx: &mut Context<'_, TimelineView>,
) -> impl IntoElement {
    div()
        .addressable("new-posts")
        .px_4()
        .py_2()
        .bg(rgb(theme.bg_header))
        .border_b_1()
        .border_color(rgb(theme.border))
        .text_color(rgb(theme.accent))
        .child(format!("↑ {}", pending_label(count)))
        .on_click(cx.listener(|this, _event, _window, cx| this.apply_pending(cx)))
}

/// `@name`｡expansion に著者が居なければ何も返さない — 裸の `@` は壊れた
/// 行に見えてしまう｡
pub(super) fn byline(author_username: &str) -> String {
    if author_username.is_empty() {
        String::new()
    } else {
        format!("@{author_username}")
    }
}

/// "@name reposted"､repost したユーザーの screen name が expansion に
/// 無ければ "Reposted" だけ — 裸の `@` を描くのではなく [`byline`] の
/// 著者不在時の fallback を写している｡
pub(super) fn repost_banner_label(reposted_by: &str) -> String {
    if reposted_by.is_empty() {
        "Reposted".to_string()
    } else {
        format!("@{reposted_by} reposted")
    }
}

/// quote 元を､quote 自身のテキストの下に枠付きのカードとして埋め込む
/// (#13)｡塗りは新しい色のスロットを足さず `bg_header` を使い回す — これ
/// はすでにアプリの「区別された領域」の背景 (ヘッダのバー) であり､カード
/// は `theme.bg` の上に直に載るので､自前のパレット項目を持たずとも明確に
/// 別のブロックとして読める｡
///
/// `media` は quote された post のサムネイルの格子 (#123)｡カードが読む
/// 対象ではなく小さな preview である場所では `None` になる: composer の
/// "replying to" と "quoting" の帯はどちらも､画像がすでに画面にある行の
/// 直下に座る｡
pub(super) fn quote_card(
    quoted: &QuotedPost,
    theme: Theme,
    media: Option<AnyElement>,
) -> impl IntoElement {
    let byline = byline(&quoted.author_username);

    div()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .mt_1()
        .rounded(theme::RADIUS_CONTROL)
        .border_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.bg_header))
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    div()
                        .font_weight(FontWeight::BOLD)
                        .child(quoted.author_name.clone()),
                )
                .child(div().text_color(rgb(theme.text_muted)).child(byline)),
        )
        .child(div().child(quoted.text.clone()))
        .children(media)
}

/// "Replying to @name"｡親の著者が解決できなければ (削除済み､保護済み､
/// あるいは単に expand されていない) 一般的な fallback になる — 裸の
/// "Replying to @" を描くのではなく [`repost_banner_label`] の著者不在時
/// の fallback を写している (#12)｡
pub(super) fn reply_banner_label(replied_to: &RepliedTo) -> String {
    if replied_to.author_username.is_empty() {
        "Replying to a post".to_string()
    } else {
        format!("Replying to @{}", replied_to.author_username)
    }
}

/// [`thread_toggle_row`] のクリックできるラベル｡いまの状態 (まだ何も
/// 読めていないが fetch は走っている) に toggle が無ければ `None` — その
/// 場合は代わりに [`TimelineView::thread_section`] が素の "Loading thread…"
/// の notice を描く｡`state: None` は「一度も要求していない」で (取得を
/// 差し出し､#12 の「費用は予測できねばならない」要件に従い最悪の費用を
/// 先に明示する); `Some(Failed(_))` は再試行を差し出す｡
pub(super) fn thread_action_label(state: Option<&ThreadFetchState>) -> Option<&'static str> {
    match state {
        None => Some("Show thread (up to 5 requests)"),
        Some(ThreadFetchState::Failed(_)) => Some("Retry"),
        Some(ThreadFetchState::Loading | ThreadFetchState::Loaded(_)) => None,
    }
}

/// クリックできる "Show thread" / "Retry" の行 (#12)｡[`load_older_row`]
/// と同じ体裁だ — すでに描かれた post に対する二次的な操作なので､完全な
/// ボタンではなくリンク色のクリックできる行にしてある｡
pub(super) fn thread_toggle_row(
    reply_post_id: String,
    first_parent_id: String,
    label: &str,
    theme: Theme,
    cx: &mut Context<'_, TimelineView>,
) -> impl IntoElement {
    div()
        .addressable(format!("show-thread-{reply_post_id}"))
        .text_color(rgb(theme.accent))
        .child(label.to_string())
        .on_click(cx.listener(move |this, _event, _window, cx| {
            this.show_thread(reply_post_id.clone(), first_parent_id.clone(), cx);
        }))
}

/// 組み上がった親の chain (#12)｡最も古い祖先が先頭で､このファイルに
/// すでにある他の「埋め込まれた post」の扱いと見た目を揃えるため､どれも
/// [`quote_card`] と同じように描く｡空で cap もされていない chain は､
/// 最初の親の fetch が何も見つけなかったとき (削除済み､保護済み､その他
/// 不在) にだけ起きる — #12 の「まともに描けねばならない」要件だ — ので､
/// その場合は黙って何も出さず専用のメッセージを出す｡
pub(super) fn render_thread_chain(chain: &ThreadChain, theme: Theme) -> AnyElement {
    if chain.items.is_empty() && !chain.capped {
        return div()
            .text_color(rgb(theme.text_muted))
            .child("The parent post is no longer available.")
            .into_any_element();
    }

    div()
        .flex()
        .flex_col()
        .gap_1()
        .children(
            chain
                .items
                .iter()
                .map(|thread_item| thread_row(thread_item, theme)),
        )
        .when(chain.capped, |column| {
            column.child(div().text_color(rgb(theme.text_muted)).child(format!(
                "Reached the {}-level limit — earlier replies in this thread \
                         aren't shown.",
                thread::MAX_THREAD_DEPTH
            )))
        })
        .into_any_element()
}

pub(super) fn thread_row(thread_item: &thread::ThreadItem, theme: Theme) -> impl IntoElement {
    let byline = byline(&thread_item.author_username);

    div()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .rounded_md()
        .border_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.bg_header))
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    div()
                        .font_weight(FontWeight::BOLD)
                        .child(thread_item.author_name.clone()),
                )
                .child(div().text_color(rgb(theme.text_muted)).child(byline)),
        )
        .child(div().child(thread_item.text.clone()))
}

/// ヘッダの簡潔な usage 要約 (#18): リクエスト数は無条件に必ず出す —
/// 見積り金額を添えるのは `request_price` が設定されてからだけで､推測の
/// 価格は価格を出さないより悪い､という issue の中核の規則に従う｡
pub(super) fn usage_label(today: u64, total: u64, request_price: Option<f64>) -> String {
    match usage::estimated_amount(today, request_price) {
        Some(amount) => format!("Today: {today} req (~{amount:.2}) · Total: {total} req"),
        None => format!("Today: {today} req · Total: {total} req"),
    }
}

/// usage の行をどの theme のスロットで描くか: 今日の件数が
/// `daily_request_budget` に近づくか超えると `warning`/`danger` になり､
/// [`usage::budget_status`] が返す深刻度に対応する; 立てる旗が無ければ､
/// timestamp や byline がすでに使っているのと同じ muted のスロットだ｡
pub(super) fn usage_color(status: usage::BudgetStatus, theme: Theme) -> u32 {
    match status {
        usage::BudgetStatus::Ok => theme.text_muted,
        usage::BudgetStatus::Near => theme.warning,
        usage::BudgetStatus::Exceeded => theme.danger,
    }
}

/// composer のエラー行｡status が見せるものを持つときだけ出す (#14) —
/// `Idle`/`Submitting` では `None` なので､そのどちらの状態でも composer
/// は余分な行を描かない｡
pub(super) fn compose_error_message(status: &ComposeStatus) -> Option<SharedString> {
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
pub(super) fn offers_reauthorize(
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
pub(super) fn offers_repost(
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
pub(super) fn is_own_post(home_username: Option<&str>, author_username: &str) -> bool {
    home_username.is_some_and(|home| home.eq_ignore_ascii_case(author_username))
}

/// 一つの post の repost/un-repost の toggle (#15): repost していなければ
/// "Repost"､していれば "Reposted" — どちらもクリックできる (repost は
/// 取り消せるので､ボタンは自身の undo も兼ねる)｡体裁は
/// [`thread_toggle_row`] と同じ｡リクエストが飛んでいる間は無効になる —
/// click handler がまったく無く､#14 の二重送信の守りに合わせてある; 失敗
/// した試みは (依然クリックできる) toggle の上にメッセージを出し､再試行を
/// 差し出す｡
pub(super) fn repost_row(
    row_id: &str,
    post_id: &str,
    state: &ToggleState,
    theme: Theme,
    cx: &mut Context<'_, TimelineView>,
) -> AnyElement {
    let label = repost_action_label(state);
    // #95｡`like_row` と同じ｡
    let color = if state.is_on() {
        theme.repost
    } else {
        theme.text_muted
    };

    let toggle = div()
        .addressable(format!("repost-{row_id}"))
        .text_color(rgb(color))
        .child(label)
        .when(state.can_toggle(), |element| {
            let id = post_id.to_string();
            element.on_click(cx.listener(move |this, _event, _window, cx| {
                this.toggle_repost(id.clone(), cx);
            }))
        });

    if let ToggleStatus::Failed(message) = state.status() {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_color(rgb(theme.danger)).child(message.clone()))
            .child(toggle)
            .into_any_element()
    } else {
        toggle.into_any_element()
    }
}

/// [`repost_row`] のクリックできるラベル (#15): リクエストが飛んでいる間
/// は pending 中の向き､そうでなければ素の on/off のラベル｡
pub(super) fn repost_action_label(state: &ToggleState) -> &'static str {
    if matches!(state.status(), ToggleStatus::Pending) {
        if state.is_on() {
            "Reposting…"
        } else {
            "Removing repost…"
        }
    } else if state.is_on() {
        "Reposted"
    } else {
        "Repost"
    }
}

/// 一つの post の like/unlike の toggle (#68): like していなければ "Like"､
/// していれば "Liked" — どちらもクリックできる｡体裁は [`repost_row`] と
/// 同じで､pending 中は無効という規則も､依然クリックできる toggle の上に
/// 失敗のメッセージを描くところまで写している｡
pub(super) fn like_row(
    row_id: &str,
    post_id: &str,
    state: &ToggleState,
    theme: Theme,
    cx: &mut Context<'_, TimelineView>,
) -> AnyElement {
    let label = like_action_label(state);
    // #95: "on" の操作は､行のクリックできるものがすでに着ているリンク色
    // ではなく､その意味に応じて色を付ける｡
    let color = if state.is_on() {
        theme.like
    } else {
        theme.text_muted
    };

    let toggle = div()
        .addressable(format!("like-{row_id}"))
        .text_color(rgb(color))
        .child(label)
        .when(state.can_toggle(), |element| {
            let id = post_id.to_string();
            element.on_click(cx.listener(move |this, _event, _window, cx| {
                this.toggle_like(id.clone(), cx);
            }))
        });

    if let ToggleStatus::Failed(message) = state.status() {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_color(rgb(theme.danger)).child(message.clone()))
            .child(toggle)
            .into_any_element()
    } else {
        toggle.into_any_element()
    }
}

/// [`like_row`] のクリックできるラベル (#68): リクエストが飛んでいる間は
/// pending 中の向き､そうでなければ素の on/off のラベル｡
pub(super) fn like_action_label(state: &ToggleState) -> &'static str {
    if matches!(state.status(), ToggleStatus::Pending) {
        if state.is_on() {
            "Liking…"
        } else {
            "Unliking…"
        }
    } else if state.is_on() {
        "Liked"
    } else {
        "Like"
    }
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
pub(super) fn offers_like(
    signed_in_with_oauth: bool,
    home_user_id: Option<&str>,
    _item: &TimelineItem,
) -> bool {
    signed_in_with_oauth && home_user_id.is_some()
}

/// 著者の名前を x.com の profile へのリンクとして描く (#70) — username が
/// expand されず [`profile_url`] の指す先が無いときは､素の太字にする｡
pub(super) fn author_link(
    item: &TimelineItem,
    theme: Theme,
    cx: &mut Context<'_, TimelineView>,
) -> AnyElement {
    let name = div()
        .font_weight(FontWeight::BOLD)
        .child(item.author_name.clone());

    match profile_url(&item.author_username) {
        Some(url) => name
            .addressable(format!("profile-{}", item.id))
            .text_color(rgb(theme.accent))
            .on_click(cx.listener(move |this, _event, _window, cx| {
                this.open_in_browser(url.clone(), cx);
            }))
            .into_any_element(),
        None => name.into_any_element(),
    }
}

/// 一つの post の byline 行にある "Open in X" の導線 (#70) — 著者が
/// expand されなかった post 用に [`post_permalink`] が id だけの fallback
/// を持つので､常に差し出す｡
pub(super) fn open_post_link(
    item: &TimelineItem,
    theme: Theme,
    cx: &mut Context<'_, TimelineView>,
) -> impl IntoElement {
    // #52: repost 行の permalink は元の post のものだ — 行が表示している
    // のもそれだし､どのみち x.com はそこへ redirect するだけだ｡
    let url = post_permalink(&item.author_username, action_post_id(item));
    div()
        .addressable(format!("open-{}", item.id))
        .text_color(rgb(theme.text_muted))
        .child("Open in X")
        .on_click(cx.listener(move |this, _event, _window, cx| {
            this.open_in_browser(url.clone(), cx);
        }))
}

/// 一つの post のテキストに含まれるリンクを､本文の下のクリックできる chip
/// として並べる (#70)｡
///
/// テキストの中ではなく下に置く: X 自身のテキストは `t.co` の短縮リンクを
/// 運ぶので､リンクを *その場で* クリックできるようにするには本文をテキスト
/// とリンクの要素へ交互に分割することになり､gpui は子をそれぞれ独立した
/// ブロックとして配置する — 段落は一続きに折り返さなくなる｡下に chip を
/// 並べれば本文は無傷のままで､それでもユーザーを行き先へ連れていける｡
/// issue が求めているのはそれだ｡各 chip には X 自身の `display_url`
/// (`example.com/a/b…`) をラベルにしてあるので､開かれるのが展開後の
/// 行き先であっても､見えるものはテキストが言うものと一致する｡
pub(super) fn link_row(
    links: &[PostLink],
    theme: Theme,
    cx: &mut Context<'_, TimelineView>,
) -> AnyElement {
    let mut row = div().flex().flex_col().gap_1();
    for link in links {
        let url = link.url.clone();
        row = row.child(
            div()
                .addressable(format!("link-{url}"))
                .text_color(rgb(theme.accent))
                .child(link.label.clone())
                .on_click(cx.listener(move |this, _event, _window, cx| {
                    this.open_in_browser(url.clone(), cx);
                })),
        );
    }
    row.into_any_element()
}

/// 一つの post の "Reply" 操作 (#71)｡[`offers_reply`] が許すときに描く｡
///
/// composer の reply の対象を据えるだけで他は何もしない — [`quote_row`]
/// の働きを写したもので､下書きが送られるまでリクエストは出ない｡運ぶ id は
/// `action_post_id` のもの (#52): repost 行からの reply は *元の* post に
/// 答えねばならない｡さもないと reply はまったく別の会話の下に着く｡
pub(super) fn reply_row(
    item: &TimelineItem,
    theme: Theme,
    cx: &mut Context<'_, TimelineView>,
) -> AnyElement {
    let post_id = action_post_id(item).to_string();
    let replying_to = QuotedPost {
        author_name: item.author_name.clone(),
        author_username: item.author_username.clone(),
        text: item.text.clone(),
        // composer が出す返信先の preview はテキストだけを見せる (#123):
        // その画像は上の行ですでに画面にある｡
        media: Vec::new(),
    };

    div()
        .addressable(format!("reply-{}", item.id))
        .text_color(rgb(theme.text_muted))
        .child("Reply")
        .on_click(cx.listener(move |this, _event, _window, cx| {
            this.compose.set_reply(compose::ReplyTarget {
                post_id: post_id.clone(),
                replying_to: replying_to.clone(),
            });
            cx.notify();
        }))
        .into_any_element()
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
pub(super) fn offers_delete(
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
pub(super) fn offers_reply(signed_in_with_oauth: bool, _item: &TimelineItem) -> bool {
    signed_in_with_oauth
}

/// composer が reply 対象の上に出す見出し (#71) — "Replying to @someone"､
/// 著者が expand されなかったときは handle 無しの形になる｡同じ欠落に対する
/// [`reply_banner_label`] 自身の扱いを写したものだ｡
pub(super) fn reply_target_label(author_username: &str) -> String {
    if author_username.is_empty() {
        "Replying to a post".to_string()
    } else {
        format!("Replying to @{author_username}")
    }
}

/// post `item` が "Quote" 操作を差し出すべきか (#16)｡
///
/// composer にそもそも辿り着けることを要求する — `signed_in_with_oauth`
/// で､[`Render::render`] 自身の `self.composer` に対する条件を写している
/// — それが無ければ quote の行き先が無いからだ｡#52 以降 repost 行にも他と
/// 同じく出す — `x_api::action_post_id` が元の post へ解決し､それが quote
/// カードの運ぶテキストと著者でもある｡[`offers_repost`] と違い､自分の post
/// を quote するのは許されている (#16 の設計上の判断 — API は自分を repost
/// するときのようには拒まない) ので､ここに `is_own_post` の確認は無い｡
pub(super) fn offers_quote(signed_in_with_oauth: bool, _item: &TimelineItem) -> bool {
    signed_in_with_oauth
}

/// 一つの post の "Quote" 操作 (#16)｡[`offers_quote`] が `item` に対して
/// 許すときに描く｡#15 の repost の toggle と違い､これは post ごとの
/// リクエストではなく一度きりの純粋にローカルな操作だ: クリックしても
/// composer の quote 対象 (`ComposeState::set_quote`) を読み込んでそこへ
/// カードを描くだけで — 普通の下書きとまったく同じく､composer 自身の
/// "Post" ボタンが押されるまで X へは何も送らない｡
pub(super) fn quote_row(
    item: &TimelineItem,
    theme: Theme,
    cx: &mut Context<'_, TimelineView>,
) -> AnyElement {
    // #52: repost 行を quote すると元の post を quote する｡それは下の
    // カードがすでに見せているテキストと著者でもある｡
    let post_id = action_post_id(item).to_string();
    let quoted = QuotedPost {
        author_name: item.author_name.clone(),
        author_username: item.author_username.clone(),
        text: item.text.clone(),
        // 上と同じ: この quote ボタンが属する行はすぐそこにある｡
        media: Vec::new(),
    };

    div()
        .addressable(format!("quote-{}", item.id))
        .text_color(rgb(theme.text_muted))
        .child("Quote")
        .on_click(cx.listener(move |this, _event, _window, cx| {
            this.compose.set_quote(compose::QuoteTarget {
                post_id: post_id.clone(),
                quoted: quoted.clone(),
            });
            cx.notify();
        }))
        .into_any_element()
}

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
pub(super) fn header_title(home_username: Option<&str>) -> String {
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
pub(super) fn header_title_element(home_username: Option<&str>, theme: Theme) -> impl IntoElement {
    div()
        .addressable("header-title")
        .ml(theme::ROW_PAD_X)
        .text_size(theme::TEXT_META)
        .text_color(rgb(theme.text_tertiary))
        .child(header_title(home_username))
}

/// ツールバーの timeline 切り替えの trough (#95)｡macOS の segmented
/// control の形をしている: [`tab_segment`] が収まる一本のトラックがあり､
/// 選択中のものだけがウィンドウ自身の背景色でそこから持ち上がる｡
///
/// segment と分けた (かつてはラベルのスライスを取る一つの関数だった) のは､
/// #164 の segment が各々クリックを担い､クリックハンドラには view の `cx`
/// が要るからだ — それは `ui::list_picker` が持つものであって､この
/// ファイルのものではない｡ここに残るのは control の見た目だけである｡
pub(super) fn tab_trough(theme: Theme) -> Div {
    div()
        .flex()
        .items_center()
        .p(px(2.0))
        .rounded(theme::RADIUS_CONTROL)
        .bg(rgb(theme.control_trough))
        .text_size(theme::TEXT_META)
}

/// 切り替えの segment 一つ — [`tab_trough`] を見よ｡
pub(super) fn tab_segment(label: &str, selected: bool, theme: Theme) -> Div {
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

/// 一つの行が描く添付画像の数 (#65)｡X は post あたり四枚まで許し､それは
/// timeline の行が timeline の行でなくなる手前に収まる限度でもある｡
pub(super) const MAX_RENDERED_MEDIA: usize = 4;

/// サムネイル一枚の高さ (#65)｡media 自身の `width`/`height` から導かず
/// 固定にしてある: 行の高さが､どの画像のダウンロードを終えたかに依っては
/// ならない｡さもなくば画像が届くたびに timeline が読み手の下で組み直る｡
///
/// 値は #95 の他の寸法と一緒に `theme` にある｡
pub(super) use crate::theme::MEDIA_CELL_HEIGHT;

/// `count` 枚のサムネイルを何列に並べるか (#65): 一枚なら一列､それ以上は
/// 二列｡三列にすると この高さでは一枚ずつが読むには狭すぎ､X 自身の上限で
/// ある四枚は二列二行にちょうど収まる｡0 は決して返さない — `chunks` が
/// panic するからだ｡
pub(super) fn media_columns(count: usize) -> usize {
    if count <= 1 { 1 } else { 2 }
}

/// 写真でないサムネイルの下に見せるバッジ (#65)｡素の写真なら `None` で､
/// このアプリが知らない `type` でも `None` だ｡そちらが前方互換の向きで
/// ある: X が後から生む media type は､誰にも解せないラベルとしてではなく
/// 素の静止画として描かれるべきだ｡
pub(super) fn media_badge(kind: Option<&str>) -> Option<&'static str> {
    match kind {
        Some("video") => Some("Video"),
        Some("animated_gif") => Some("GIF"),
        _ => None,
    }
}

/// 投稿者のアバターを描く大きさ (#64)｡定数が一つなのは､placeholder が
/// 画像と厳密に一致せねばならないからだ — ダウンロードが届いた瞬間に組み
/// 直る行は､アバターがまったく無いよりも悪い｡
///
/// 大きさを揃えるだけでは足りない (#103): `post_row` はアバターを `flex_1`
/// の本文の隣に据えるので､本文の内容が使える幅を越えると flex の既定の
/// `flex-shrink: 1` が素の `.size(AVATAR_SIZE)` 要素を潰す｡`img` と
/// placeholder の `div` には､大きさそのものだけでなく､この大きさと並べて
/// `flex_shrink_0` も要る｡両者が一致せねばならない三つ目が形であり､だから
/// こそリテラルではなく [`theme::AVATAR_RADIUS`] なのだ (#98)｡
///
/// 値そのものは､どちらもそこから導かれる radius と行の区切り線の inset と
/// 一緒に `theme` にある (#95)｡
pub(super) use crate::theme::AVATAR_SIZE;

/// まだダウンロードされていない､失敗した､あるいはそもそも存在しなかった
/// アバターの代わりに立つもの (#64): 投稿者の頭文字を載せた塗り潰しの円｡
///
/// 空の円盤ではなく頭文字にしたのは､画像が一枚も届く前から timeline の
/// 連続する投稿者をおおむね見分けられるからで — それが #64 の眼目である｡
/// 名前が展開されなかった投稿者には素の円が出る; 見せる文字が無く､
/// でっち上げれば空白よりも悪くなる｡
pub(super) fn avatar_placeholder(author_name: &str, theme: Theme) -> AnyElement {
    let initial = avatar_initial(author_name);

    div()
        .size(AVATAR_SIZE)
        .flex_shrink_0()
        .rounded(theme::AVATAR_RADIUS)
        .bg(rgb(theme.border))
        .flex()
        .items_center()
        .justify_center()
        .text_color(rgb(theme.text_muted))
        .child(initial)
        .into_any_element()
}

/// アバターの placeholder が `author_name` に対して見せる文字 (#64):
/// 先頭の一文字を大文字にしたもの｡名前が展開されなかった投稿者では空に
/// なる — そのとき円は､でっち上げた頭文字を見せるのではなく単独で立つ｡
///
/// バイト単位ではなく `char` 単位なので､マルチバイト文字で始まる名前
/// (そういうものは多い) が文字の途中で切られることも飛ばされることも
/// ない｡`to_uppercase` は Unicode のそれで､字体によっては一文字より多くを
/// 生みうる; それは切り詰めずそのままにしてある｡大文字化した結果を半分で
/// 断てば､短いものではなく誤ったものが出るからだ｡
pub(super) fn avatar_initial(author_name: &str) -> String {
    author_name
        .chars()
        .next()
        .map(|first| first.to_uppercase().to_string())
        .unwrap_or_default()
}

/// post 一つに対する x.com の正準 URL (#70)｡[`TimelineItem`] がすでに
/// 持っているものから組み立てる — リクエストは無く､API は一切関わらない｡
///
/// 投稿者が展開されなかった post では `author_username` が空で
/// (`x_api::model::build_item` を見よ)､`x.com//status/…` は 404 になる｡
/// X 自身の id だけの形 `x.com/i/web/status/:id` はサーバ側で投稿者を
/// 解決するので､アプリがその post について最も知らないまさにそのときに
/// 導線を取り下げるのではなく､リンクは働きつづける｡
pub(super) fn post_permalink(author_username: &str, post_id: &str) -> String {
    if author_username.is_empty() {
        format!("https://x.com/i/web/status/{post_id}")
    } else {
        format!("https://x.com/{author_username}/status/{post_id}")
    }
}

/// アカウント一つに対する x.com の URL (#70)｡username が解決しなかった
/// ときは `None` — post と違い id だけの逃げ道が無いので､誤った先を指す
/// 代わりにその導線を取り下げる｡
pub(super) fn profile_url(author_username: &str) -> Option<String> {
    (!author_username.is_empty()).then(|| format!("https://x.com/{author_username}"))
}

/// 行がアクションの傍らに見せるエンゲージメントの件数 (#67､#95 で作り
/// 直した)｡
///
/// #95 までは本文の下の独立した一行 — "12 replies · 34 reposts ·
/// 56 likes" — で､まさに同じ三つを名指すアクションのラベルが縦に並んだ列
/// の上に載っていた｡#95 は二つを畳み込む: 件数は今や属するアクションの隣に
/// 乗り､独立した行は消えた｡どの post でも一行分の高さが戻ってくる｡
///
/// 各フィールドは､その件数が零のとき､または post が metrics をまったく
/// 持たないとき `None` になる｡だから新しい post は零の連なりではなく素の
/// アクションを描く — 零の部分を落としていた古い一行と同じ規則だ｡件数は
/// 行を取得した時点のスナップショットであり ([`PostMetrics`] を見よ)､
/// ここで読み直すものは何も無い｡
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct RowCounts {
    /// "Reply" の傍ら｡
    pub(super) replies: Option<String>,
    /// "Repost" / "Reposted" の傍ら｡
    pub(super) reposts: Option<String>,
    /// "Like" / "Liked" の傍ら｡
    pub(super) likes: Option<String>,
}

/// 行が持つ metrics から､その行の [`RowCounts`] を組み立てる｡
pub(super) fn row_counts(metrics: Option<&PostMetrics>) -> RowCounts {
    let Some(metrics) = metrics else {
        return RowCounts::default();
    };
    RowCounts {
        replies: non_zero_count(metrics.replies),
        reposts: non_zero_count(metrics.reposts),
        likes: non_zero_count(metrics.likes),
    }
}

/// 件数一つを略記したもの｡零なら `None` — 零が "0" ではなく無である理由は
/// [`RowCounts`] を見よ｡
fn non_zero_count(count: u64) -> Option<String> {
    (count > 0).then(|| compact_count(count))
}

/// アクション一つと､その傍らのエンゲージメント件数 (#95)｡見せる件数が
/// 無ければアクション単独｡
///
/// 件数をアクション自身の要素の一部ではなく兄弟にしてあるのは､数字を
/// クリックしても何も起きないようにするためだ: アクションはリクエストを
/// 費やすトグルであり､ボタンの一部に見える件数は､読み手がただ読むだけの
/// つもりだったアクションの的を広げてしまう｡
pub(super) fn with_count(action: AnyElement, count: Option<&str>, theme: Theme) -> AnyElement {
    let Some(count) = count else {
        return action;
    };

    div()
        .flex()
        .items_center()
        .gap_1()
        .child(action)
        .child(
            div()
                .text_color(rgb(theme.text_muted))
                .child(count.to_string()),
        )
        .into_any_element()
}

/// X 自身の UI と同じやり方で件数を略記する — `12345` は `12.3K` になる —
/// 人気の post が七桁の幅でタイムスタンプと byline を押しのけられない
/// ように｡末尾の `.0` は落とす (`1000` は `1.0K` ではなく `1K`); 1000 未満
/// はそのままの数字を見せる｡
pub(super) fn compact_count(count: u64) -> String {
    #[expect(
        clippy::cast_precision_loss,
        reason = "an abbreviated count is approximate by construction"
    )]
    fn scaled(count: u64, unit: u64, suffix: char) -> String {
        let value = count as f64 / unit as f64;
        // 小数一桁を､丸めずに切り捨てる｡ラベルが post の実際より多くの
        // エンゲージメントを主張しないようにするためだ｡
        let tenths = (value * 10.0).floor() / 10.0;
        if (tenths.fract()).abs() < f64::EPSILON {
            format!("{}{suffix}", tenths.trunc())
        } else {
            format!("{tenths:.1}{suffix}")
        }
    }

    match count {
        0..1_000 => count.to_string(),
        1_000..1_000_000 => scaled(count, 1_000, 'K'),
        _ => scaled(count, 1_000_000, 'M'),
    }
}

/// `2026-08-16T09:00:00.000Z` を `2026-08-16 09:00` にする｡
///
/// API は常に RFC 3339 の UTC を返すので､これほど小さなラベルのために日付
/// ライブラリを引き込むより切り出すほうがよい｡
pub(super) fn format_timestamp(created_at: Option<&str>) -> String {
    let Some(raw) = created_at else {
        return String::new();
    };
    // `&time[..5]` ではなく `get` (#47, `clippy::string_slice`): あれは
    // バイト範囲であり､五バイト目がマルチバイト文字の内側に落ちる `time`
    // 側は､生の文字列へ落ちるのではなく panic する｡`created_at` は API
    // から来るので､これは遠隔からの入力だ｡
    match raw.split_once('T') {
        Some((date, time)) => match time.get(..5) {
            Some(hhmm) => format!("{date} {hhmm}"),
            None => raw.to_string(),
        },
        None => raw.to_string(),
    }
}
