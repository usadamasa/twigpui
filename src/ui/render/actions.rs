//! 操作の行そのもの (#241): like / repost / reply / quote / open と､本文の
//! リンクの chip｡どれもクリックのために `cx` を取る｡どれを描くかは
//! [`super::offers`] が決める｡

use super::post::{post_permalink, profile_url};
use crate::ui::*;

/// 一つの post の repost/un-repost の toggle (#15): repost していなければ
/// "Repost"､していれば "Reposted" — どちらもクリックできる (repost は
/// 取り消せるので､ボタンは自身の undo も兼ねる)｡体裁は
/// [`thread_toggle_row`] と同じ｡リクエストが飛んでいる間は無効になる —
/// click handler がまったく無く､#14 の二重送信の守りに合わせてある; 失敗
/// した試みは (依然クリックできる) toggle の上にメッセージを出し､再試行を
/// 差し出す｡
pub(in crate::ui) fn repost_row(
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
pub(in crate::ui) fn repost_action_label(state: &ToggleState) -> &'static str {
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
pub(in crate::ui) fn like_row(
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
pub(in crate::ui) fn like_action_label(state: &ToggleState) -> &'static str {
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

/// 著者の名前を x.com の profile へのリンクとして描く (#70) — username が
/// expand されず [`profile_url`] の指す先が無いときは､素の太字にする｡
pub(in crate::ui) fn author_link(
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
pub(in crate::ui) fn open_post_link(
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
pub(in crate::ui) fn link_row(
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
pub(in crate::ui) fn reply_row(
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

/// composer が reply 対象の上に出す見出し (#71) — "Replying to @someone"､
/// 著者が expand されなかったときは handle 無しの形になる｡同じ欠落に対する
/// [`reply_banner_label`] 自身の扱いを写したものだ｡
pub(in crate::ui) fn reply_target_label(author_username: &str) -> String {
    if author_username.is_empty() {
        "Replying to a post".to_string()
    } else {
        format!("Replying to @{author_username}")
    }
}

/// 一つの post の "Quote" 操作 (#16)｡[`offers_quote`] が `item` に対して
/// 許すときに描く｡#15 の repost の toggle と違い､これは post ごとの
/// リクエストではなく一度きりの純粋にローカルな操作だ: クリックしても
/// composer の quote 対象 (`ComposeState::set_quote`) を読み込んでそこへ
/// カードを描くだけで — 普通の下書きとまったく同じく､composer 自身の
/// "Post" ボタンが押されるまで X へは何も送らない｡
pub(in crate::ui) fn quote_row(
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
