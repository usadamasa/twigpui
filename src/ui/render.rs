//! The pieces that turn one timeline item into elements, and the pure
//! functions that decide what those elements say.
//!
//! Split out of `ui` (#126) because `src/ui.rs` had reached its size
//! ceiling with no headroom left, and this is the half that never touches
//! `TimelineView`'s state: free functions over the data a row already
//! holds. Everything here is `pub(super)` rather than `pub(crate)` — only
//! `ui` calls it, and widening the visibility would undo the split.
//!
//! The judgements about *whether* a reload may run, and what to say when
//! one fails, live in [`super::reload_policy`] instead.

use super::*;

/// An outlined pill in the header that starts the sign-in flow.
///
/// #31 (upgrade away from the app-only bearer token) and #14 (the session
/// predates `tweet.write`) are different reasons to reach the same place, so
/// the two buttons differ only in their label — worth one helper rather than
/// two near-identical builder chains that have to be kept in step.
pub(super) fn sign_in_pill(
    id: &'static str,
    label: &'static str,
    theme: Theme,
    cx: &mut Context<'_, TimelineView>,
) -> impl IntoElement {
    div()
        .id(id)
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

/// The persistent "your session expired" banner (#54): a distinct row
/// between the header and everything else, rather than folded into
/// [`TimelineView::body`]'s `state`-keyed match — the whole point is that it
/// has to keep showing even while `body` renders a perfectly normal loaded
/// timeline (on the bearer-token fallback), which is exactly the state #54
/// was filed from.
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

/// The reload cooldown/failure banner (#57) — styled identically to, and
/// drawn right next to, [`session_notice_banner`] for the same reason that
/// one is independent of `body`: a cooldown or a failed refresh describes
/// the most recent *request*, not whatever posts are (or aren't) currently
/// shown, and must never read as "there is nothing here" when there is.
pub(super) fn reload_notice_banner(
    notice: &ReloadNotice,
    theme: Theme,
    now: i64,
) -> impl IntoElement {
    // #141: the color says which kind of line this is before the words do.
    // `Outcome` is the only variant that reports success, and painting it
    // `danger` alongside the other two would make a finished reload look
    // like a failed one.
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

/// `@name`, or nothing at all when the author was missing from the expansion —
/// a bare `@` would read as a broken row.
pub(super) fn byline(author_username: &str) -> String {
    if author_username.is_empty() {
        String::new()
    } else {
        format!("@{author_username}")
    }
}

/// "@name reposted", or "Reposted" alone when the reposting user's screen
/// name was missing from the expansion — mirrors [`byline`]'s empty-author
/// fallback rather than rendering a bare `@`.
pub(super) fn repost_banner_label(reposted_by: &str) -> String {
    if reposted_by.is_empty() {
        "Reposted".to_string()
    } else {
        format!("@{reposted_by} reposted")
    }
}

/// The quoted source, embedded as a bordered card under a quote's own text
/// (#13). Reuses `bg_header` for the fill rather than adding a new color
/// slot — it's already the app's "distinct region" background (the header
/// bar), and the card sits directly on `theme.bg`, so it reads as a clearly
/// separate block without needing its own palette entry.
///
/// `media` is the quoted post's thumbnail grid (#123), or `None` where the
/// card is a small preview rather than the thing being read: the
/// composer's "replying to" and "quoting" strips both sit directly under
/// the row whose images are already on screen.
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

/// "Replying to @name", or a generic fallback when the parent's author
/// wasn't resolvable (deleted, protected, or simply not expanded) — mirrors
/// [`repost_banner_label`]'s empty-author fallback rather than rendering a
/// bare "Replying to @" (#12).
pub(super) fn reply_banner_label(replied_to: &RepliedTo) -> String {
    if replied_to.author_username.is_empty() {
        "Replying to a post".to_string()
    } else {
        format!("Replying to @{}", replied_to.author_username)
    }
}

/// The clickable label for [`thread_toggle_row`], or `None` when the current
/// state (nothing yet loaded but a fetch is running) has no toggle at all —
/// [`TimelineView::thread_section`] renders a plain "Loading thread…" notice
/// for that case instead. `state: None` means "never requested" (offer to
/// fetch, spelling out the worst-case cost up front per #12's "cost must be
/// predictable" requirement); `Some(Failed(_))` offers a retry.
pub(super) fn thread_action_label(state: Option<&ThreadFetchState>) -> Option<&'static str> {
    match state {
        None => Some("Show thread (up to 5 requests)"),
        Some(ThreadFetchState::Failed(_)) => Some("Retry"),
        Some(ThreadFetchState::Loading | ThreadFetchState::Loaded(_)) => None,
    }
}

/// The clickable "Show thread" / "Retry" row (#12), styled like
/// [`load_older_row`] — a link-colored, clickable line rather than a full
/// button, since it's a secondary action on an already-rendered post.
pub(super) fn thread_toggle_row(
    reply_post_id: String,
    first_parent_id: String,
    label: &str,
    theme: Theme,
    cx: &mut Context<'_, TimelineView>,
) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("show-thread-{reply_post_id}")))
        .text_color(rgb(theme.accent))
        .child(label.to_string())
        .on_click(cx.listener(move |this, _event, _window, cx| {
            this.show_thread(reply_post_id.clone(), first_parent_id.clone(), cx);
        }))
}

/// The assembled parent chain (#12), oldest ancestor first, each rendered
/// like [`quote_card`] for visual consistency with the other "embedded post"
/// treatment already in this file. An empty, uncapped chain only happens
/// when the very first parent fetch found nothing (deleted, protected, or
/// otherwise absent) — #12's "must render sensibly" requirement — so that
/// case gets its own message rather than silently showing nothing.
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

/// The header's compact usage summary (#18): request counts are always
/// shown, unconditionally — an estimated amount is appended only once
/// `request_price` is configured, per the issue's core rule that a guessed
/// price is worse than showing no price at all.
pub(super) fn usage_label(today: u64, total: u64, request_price: Option<f64>) -> String {
    match usage::estimated_amount(today, request_price) {
        Some(amount) => format!("Today: {today} req (~{amount:.2}) · Total: {total} req"),
        None => format!("Today: {today} req · Total: {total} req"),
    }
}

/// Which theme slot the usage line renders in: `warning`/`danger` as
/// today's count approaches or crosses `daily_request_budget`, matching the
/// severities [`usage::budget_status`] returns; the same muted slot
/// timestamps and bylines already use once there is nothing to flag.
pub(super) fn usage_color(status: usage::BudgetStatus, theme: Theme) -> u32 {
    match status {
        usage::BudgetStatus::Ok => theme.text_muted,
        usage::BudgetStatus::Near => theme.warning,
        usage::BudgetStatus::Exceeded => theme.danger,
    }
}

/// The composer's error line, if its status has one to show (#14) — `None`
/// for `Idle`/`Submitting`, so the composer renders no extra row in either
/// of those states.
pub(super) fn compose_error_message(status: &ComposeStatus) -> Option<SharedString> {
    match status {
        ComposeStatus::Failed(message) => Some(SharedString::from(message.clone())),
        ComposeStatus::Idle | ComposeStatus::Submitting => None,
    }
}

// #31's separate "Sign in with X" button is gone with #33. It existed for
// exactly one situation: running on an app-only bearer token, which was a
// working state whose primary button therefore said "Reload", leaving the
// OAuth flow otherwise unreachable. Without that credential the only
// unsigned state is `NotAuthenticated`, where the *primary* button already
// says "Sign in with X" — and two identical buttons side by side is what
// #31 was avoiding in the first place.

/// Whether the header should offer to re-authorize (#14): the session
/// exists, but its recorded scope doesn't include what writing needs.
///
/// Distinct from the primary "Sign in with X" button by construction — this
/// requires a session, that one appears only when there isn't one — and
/// they read differently ("Sign in" vs "Re-authorize"). #31's actual lesson
/// was "don't hide the affordance", not "there must be only one button".
///
/// Checks every write scope the app can need, not just #14's: #68 added
/// `like.write`, which X grants separately, so a session authorized before
/// #68 holds `tweet.write` alone. Without this, `toggle_like`'s refusal
/// would point at a "Re-authorize" button that was not being rendered.
pub(super) fn offers_reauthorize(signed_in_with_oauth: bool, oauth_scope: Option<&str>) -> bool {
    signed_in_with_oauth
        && !(oauth::tokens::has_scope(oauth_scope, oauth::tokens::TWEET_WRITE_SCOPE)
            && oauth::tokens::has_scope(oauth_scope, oauth::tokens::LIKE_WRITE_SCOPE))
}

/// Whether post `item` should offer a repost/un-repost toggle (#15).
///
/// Requires a signed-in OAuth session whose own id has resolved
/// (`home_user_id`, via `/me` — #11): the repost endpoints act as *this*
/// account, and there is nothing to call without it. Withheld for one's own
/// post, matching the API's own rejection (#15) — see [`is_own_post`],
/// which for a repost row compares against the *original* author, since
/// that is whose post the row displays and whose post would be reposted.
///
/// A repost row used to be withheld too, because `item.id` is the retweet
/// activity's id rather than the original content's. #52 closed that: the
/// original's id is carried on the item now, and `x_api::action_post_id`
/// is what every caller sends.
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

/// Whether `author_username` is the signed-in account's own (#15) — the API
/// rejects reposting your own post, and checking here saves a
/// guaranteed-failing request, mirroring #14's client-side character-limit
/// check. `home_username: None` (not yet resolved) never withholds the
/// button: safer to let an occasional same-account repost through to the
/// API's own rejection than to hide the button for every post before the
/// signed-in identity is known. Case-insensitive since `home_username`
/// (from `/me`) and `author_username` (from the timeline expansion) are
/// resolved independently.
pub(super) fn is_own_post(home_username: Option<&str>, author_username: &str) -> bool {
    home_username.is_some_and(|home| home.eq_ignore_ascii_case(author_username))
}

/// The repost/un-repost toggle for one post (#15): "Repost" when not
/// reposted, "Reposted" once it is — both clickable (a repost is
/// reversible, so the button doubles as its own undo), styled like
/// [`thread_toggle_row`]. Disabled — no click handler at all, matching
/// #14's double-submit guard — while a request is in flight; a failed
/// attempt shows its message above the (still clickable) toggle, offering a
/// retry.
pub(super) fn repost_row(
    row_id: &str,
    post_id: &str,
    state: &ToggleState,
    theme: Theme,
    cx: &mut Context<'_, TimelineView>,
) -> AnyElement {
    let label = repost_action_label(state);
    // #95, as in `like_row`.
    let color = if state.is_on() {
        theme.repost
    } else {
        theme.text_muted
    };

    let toggle = div()
        .id(SharedString::from(format!("repost-{row_id}")))
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

/// The clickable label for [`repost_row`] (#15): the pending direction
/// while a request is in flight, else the plain on/off label.
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

/// The like/unlike toggle for one post (#68): "Like" when not liked,
/// "Liked" once it is — both clickable, styled like [`repost_row`], which
/// this mirrors down to the disabled-while-pending rule and the failure
/// message rendered above a still-clickable toggle.
pub(super) fn like_row(
    row_id: &str,
    post_id: &str,
    state: &ToggleState,
    theme: Theme,
    cx: &mut Context<'_, TimelineView>,
) -> AnyElement {
    let label = like_action_label(state);
    // #95: an "on" action is colored by what it means, not by the link
    // color every clickable thing in the row already wears.
    let color = if state.is_on() {
        theme.like
    } else {
        theme.text_muted
    };

    let toggle = div()
        .id(SharedString::from(format!("like-{row_id}")))
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

/// The clickable label for [`like_row`] (#68): the pending direction while
/// a request is in flight, else the plain on/off label.
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

/// Whether post `item` should offer a like/unlike toggle (#68).
///
/// Requires a signed-in OAuth session whose own id has resolved
/// (`home_user_id`, via `/me` — #11), for the same reason [`offers_repost`]
/// does: the likes endpoints act as *this* account.
///
/// The one departure from [`offers_repost`]: no [`is_own_post`] check. X
/// rejects reposting your own post but accepts liking it, so #68
/// explicitly instructs against carrying #15's guard over. A repost row is
/// offered a button like any other since #52 — the like lands on the
/// original, via `x_api::action_post_id`.
pub(super) fn offers_like(
    signed_in_with_oauth: bool,
    home_user_id: Option<&str>,
    _item: &TimelineItem,
) -> bool {
    signed_in_with_oauth && home_user_id.is_some()
}

/// The author's name, as a link to their profile on x.com (#70) — or as
/// plain bold text when the username never expanded and [`profile_url`]
/// has nowhere to point.
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
            .id(SharedString::from(format!("profile-{}", item.id)))
            .text_color(rgb(theme.accent))
            .on_click(cx.listener(move |this, _event, _window, cx| {
                this.open_in_browser(url.clone(), cx);
            }))
            .into_any_element(),
        None => name.into_any_element(),
    }
}

/// The "Open in X" affordance on one post's byline row (#70) — always
/// offered, since [`post_permalink`] has an id-only fallback for a post
/// whose author never expanded.
pub(super) fn open_post_link(
    item: &TimelineItem,
    theme: Theme,
    cx: &mut Context<'_, TimelineView>,
) -> impl IntoElement {
    // #52: a repost row's permalink is the original post's — that is what
    // the row displays, and x.com would only redirect there anyway.
    let url = post_permalink(&item.author_username, action_post_id(item));
    div()
        .id(SharedString::from(format!("open-{}", item.id)))
        .text_color(rgb(theme.text_muted))
        .child("Open in X")
        .on_click(cx.listener(move |this, _event, _window, cx| {
            this.open_in_browser(url.clone(), cx);
        }))
}

/// The links from one post's text, as clickable chips under the body (#70).
///
/// Under the text rather than inside it: X's own text carries `t.co`
/// shortlinks, so making the link clickable *in place* would mean splitting
/// the body into interleaved text and link elements, and gpui lays each
/// child out as its own block — the paragraph would stop wrapping as one
/// piece. A row of chips beneath keeps the body intact and still gets the
/// user to the destination, which is what the issue asks for. Each chip is
/// labelled with X's own `display_url` (`example.com/a/b…`), so what is
/// shown matches what the text says even though what is opened is the
/// expanded destination.
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
                .id(SharedString::from(format!("link-{url}")))
                .text_color(rgb(theme.accent))
                .child(link.label.clone())
                .on_click(cx.listener(move |this, _event, _window, cx| {
                    this.open_in_browser(url.clone(), cx);
                })),
        );
    }
    row.into_any_element()
}

/// The "Reply" action for one post (#71), rendered whenever
/// [`offers_reply`] allows it.
///
/// Sets the composer's reply target and nothing else — no request goes out
/// until the draft is submitted, mirroring how [`quote_row`] works. The id
/// it carries is `action_post_id`'s (#52): replying from a repost row must
/// answer the *original* post, or the reply lands under a different
/// conversation entirely.
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
        // The composer's preview of what is being replied to shows text
        // only (#123): its images are already on screen in the row above.
        media: Vec::new(),
    };

    div()
        .id(SharedString::from(format!("reply-{}", item.id)))
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

/// Whether post `item` should offer a delete affordance (#72).
///
/// Own posts only — X rejects deleting anyone else's, and [`is_own_post`]
/// already answers that question for #15. Requires a resolved
/// `home_user_id` for the same reason the other write actions do: without
/// `/me` the app does not yet know whose posts these are.
///
/// **Withheld on a repost row**, unlike every other action since #52. A
/// repost row displays someone's original post; `is_own_post` compares
/// against that original's author, so a repost of your own post would
/// otherwise offer to delete the original from a row the user is reading
/// as "my repost". Removing a repost is [`offers_repost`]'s toggle, and
/// conflating the two on an irreversible action is not a risk worth taking.
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

/// Whether post `item` should offer a "Reply" action (#71).
///
/// Requires the composer to be reachable at all — `signed_in_with_oauth`,
/// the same gate [`offers_quote`] uses, since a reply has nowhere to go
/// without one. Nothing else: X accepts a reply to your own post, and a
/// repost row is fine now that #52 resolves it to the original.
pub(super) fn offers_reply(signed_in_with_oauth: bool, _item: &TimelineItem) -> bool {
    signed_in_with_oauth
}

/// The composer's heading above a reply target (#71) — "Replying to
/// @someone", or the handle-less form when the author never expanded,
/// mirroring [`reply_banner_label`]'s own treatment of the same gap.
pub(super) fn reply_target_label(author_username: &str) -> String {
    if author_username.is_empty() {
        "Replying to a post".to_string()
    } else {
        format!("Replying to @{author_username}")
    }
}

/// Whether post `item` should offer a "Quote" action (#16).
///
/// Requires the composer to even be reachable — `signed_in_with_oauth`,
/// mirroring [`Render::render`]'s own gate on `self.composer` — since
/// quoting has nowhere to go without one. A repost row is offered one like
/// any other since #52 — `x_api::action_post_id` resolves it to the
/// original, which is also the text and author the quote card would carry.
/// Unlike [`offers_repost`], quoting one's own post *is* allowed (#16's
/// design decision — the API doesn't reject it the way it rejects
/// reposting yourself), so there is no `is_own_post` check here.
pub(super) fn offers_quote(signed_in_with_oauth: bool, _item: &TimelineItem) -> bool {
    signed_in_with_oauth
}

/// The "Quote" action for one post (#16), rendered whenever [`offers_quote`]
/// allows it for `item`. Unlike #15's repost toggle this is a one-shot,
/// purely local action, not a per-post request: clicking it only loads the
/// composer's quote target (`ComposeState::set_quote`) so the card renders
/// there — nothing is sent to X until the composer's own "Post" button is
/// clicked, exactly like an ordinary draft.
pub(super) fn quote_row(
    item: &TimelineItem,
    theme: Theme,
    cx: &mut Context<'_, TimelineView>,
) -> AnyElement {
    // #52: quoting a repost row quotes the original, which is also the text
    // and author the card below already shows.
    let post_id = action_post_id(item).to_string();
    let quoted = QuotedPost {
        author_name: item.author_name.clone(),
        author_username: item.author_username.clone(),
        text: item.text.clone(),
        // As above: the row this quote button belongs to is right there.
        media: Vec::new(),
    };

    div()
        .id(SharedString::from(format!("quote-{}", item.id)))
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

/// The header's title (#11): which account's posts these are, and — since
/// #11 introduces a second mode — which mode is showing, so the user is
/// never left guessing whether they're looking at their own home timeline or
/// one account's posts.
///
/// `home_username` is `None` only for the brief window before `/me` has
/// resolved even once (never true once anything is cached or has loaded),
/// which is the one case where the title cannot name the account.
///
/// It took a `TimelineSource` until #33, when the window stopped being able
/// to show anything but the home timeline — the single-user view existed
/// because an app-only bearer token could not read the home one.
pub(super) fn header_title(home_username: Option<&str>) -> String {
    match home_username {
        Some(username) => format!("@{username}"),
        // Before `/me` resolves there is no account to name, and the app's
        // own name is what a macOS toolbar shows in its place.
        None => "twigpui".to_string(),
    }
}

/// What the toolbar's segment calls the timeline being shown (#161).
///
/// The list's own name is deliberately not fetched: `GET /2/lists/:id`
/// is another billed endpoint, and a label is not worth a request on every
/// start. #164 is where a list gains a name, because a switcher has to
/// name the things it switches between — one segment does not.
pub(super) fn tab_label(source: &cache::TimelineSource) -> &'static str {
    match source {
        cache::TimelineSource::Home => "Home",
        cache::TimelineSource::List(_) => "List",
    }
}

/// The toolbar's timeline switcher (#95), shaped like a macOS segmented
/// control: one trough with the selected segment lifted out of it in the
/// window's own background color.
///
/// #63 is what will fill it. Today `header` hands it a single entry, which
/// is honest — Home is the only timeline this app can fetch — and the
/// point of building the frame now is that adding the second one is a
/// change to that array rather than to the toolbar's layout.
///
/// Segments carry no click handler yet for the same reason: with one
/// timeline there is nothing to switch to, and a control that responds to
/// a click by doing nothing is worse than one that plainly does not.
pub(super) fn tab_bar(tabs: &[(&str, bool)], theme: Theme) -> AnyElement {
    let mut trough = div()
        .flex()
        .items_center()
        .p(px(2.0))
        .rounded(theme::RADIUS_CONTROL)
        .bg(rgb(theme.control_trough))
        .text_size(theme::TEXT_META);

    for (label, selected) in tabs {
        trough = trough.child(
            div()
                .px_2()
                .py_0p5()
                .rounded(px(4.0))
                .when(*selected, |segment| {
                    // Lifted out of the track rather than merely tinted:
                    // without the shadow the segment reads as a bordered
                    // chip beside plain text, which is a different control
                    // entirely.
                    segment
                        .bg(rgb(theme.bg))
                        .shadow_sm()
                        .text_color(rgb(theme.text))
                        .font_weight(FontWeight::MEDIUM)
                })
                .when(!*selected, |segment| {
                    segment.text_color(rgb(theme.text_muted))
                })
                .child((*label).to_string()),
        );
    }

    trough.into_any_element()
}

/// How many attached images one row will render (#65). X allows up to four
/// per post, which is also as many as fit before a timeline row stops being
/// a timeline row.
pub(super) const MAX_RENDERED_MEDIA: usize = 4;

/// How tall one thumbnail is (#65). Fixed rather than derived from the
/// media's own `width`/`height`: a row's height must not depend on which
/// images have finished downloading, or the timeline reflows under the
/// reader as they land.
///
/// The value lives in `theme` with the rest of #95's dimensions.
pub(super) use crate::theme::MEDIA_CELL_HEIGHT;

/// How many columns to lay `count` thumbnails out in (#65): one across for
/// a single image, two for anything more. Three across would each be too
/// narrow to read at this height, and X's own maximum of four is two rows
/// of two. Never returns 0 — `chunks` would panic.
pub(super) fn media_columns(count: usize) -> usize {
    if count <= 1 { 1 } else { 2 }
}

/// The badge shown under a non-photo thumbnail (#65), or `None` for a plain
/// photo — and for any `type` this app doesn't recognize, which is the
/// forward-compatible direction: a media type X invents later should render
/// as a bare still rather than as a label nobody can interpret.
pub(super) fn media_badge(kind: Option<&str>) -> Option<&'static str> {
    match kind {
        Some("video") => Some("Video"),
        Some("animated_gif") => Some("GIF"),
        _ => None,
    }
}

/// How big an author avatar renders (#64). One constant because the
/// placeholder has to match the image exactly — a row that reflows when the
/// download lands is worse than no avatar at all.
///
/// Matching sizes alone isn't enough (#103): `post_row` sits the avatar next
/// to a `flex_1` body, and flex's default `flex-shrink: 1` squishes a plain
/// `.size(AVATAR_SIZE)` element once the body's content pushes past the
/// available width. Both the `img` and the placeholder `div` need
/// `flex_shrink_0` alongside this size, not just the size itself. Shape is
/// the third thing the two must agree on, which is why it is
/// [`theme::AVATAR_RADIUS`] and not a literal (#98).
///
/// The value itself lives in `theme` alongside the radius and the row
/// separator's inset, both of which are derived from it (#95).
pub(super) use crate::theme::AVATAR_SIZE;

/// What stands in for an avatar that hasn't downloaded, failed, or never
/// existed (#64): a filled circle carrying the author's initial.
///
/// An initial rather than a blank disc, since it already distinguishes most
/// consecutive authors in a timeline — which is the whole point of #64 —
/// before any image arrives. An author whose name never expanded gets the
/// bare circle; there is no character to show and inventing one would be
/// worse than the gap.
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

/// The character an avatar placeholder shows for `author_name` (#64):
/// its first, uppercased. Empty for an author whose name never expanded —
/// the circle then stands alone rather than showing a made-up initial.
///
/// `char`-wise, not byte-wise, so a name starting with a multi-byte
/// character (which plenty do) is neither split mid-character nor skipped.
/// `to_uppercase` is the Unicode one, which can yield more than one
/// character for some scripts; that is left as-is rather than truncated,
/// since cutting a cased expansion in half produces something wrong rather
/// than something short.
pub(super) fn avatar_initial(author_name: &str) -> String {
    author_name
        .chars()
        .next()
        .map(|first| first.to_uppercase().to_string())
        .unwrap_or_default()
}

/// x.com's canonical URL for one post (#70), built from what
/// [`TimelineItem`] already carries — no request, no API involvement at
/// all.
///
/// `author_username` is empty for a post whose author never expanded (see
/// `x_api::model::build_item`), and `x.com//status/…` would 404. X's own
/// id-only form, `x.com/i/web/status/:id`, resolves the author server-side,
/// so the link still works rather than being withheld exactly when the app
/// knows least about the post.
pub(super) fn post_permalink(author_username: &str, post_id: &str) -> String {
    if author_username.is_empty() {
        format!("https://x.com/i/web/status/{post_id}")
    } else {
        format!("https://x.com/{author_username}/status/{post_id}")
    }
}

/// x.com's URL for one account (#70), or `None` when the username never
/// resolved — unlike a post there is no id-only fallback to reach for, so
/// the affordance is withheld instead of pointing somewhere wrong.
pub(super) fn profile_url(author_username: &str) -> Option<String> {
    (!author_username.is_empty()).then(|| format!("https://x.com/{author_username}"))
}

/// The engagement counts a row shows beside its actions (#67, reshaped by
/// #95).
///
/// Until #95 these were one standalone line under the body — "12 replies ·
/// 34 reposts · 56 likes" — sitting above a column of stacked action
/// labels that named the very same three things. #95 folds the two
/// together: the count now rides next to the action it belongs to, and the
/// separate line is gone, which is one row of height back on every post.
///
/// Each field is `None` when that count is zero, or when the post carried
/// no metrics at all, so a fresh post renders bare actions rather than a
/// run of zeros — the same rule the old line followed by dropping zero
/// parts. The counts are a snapshot from when the row was fetched (see
/// [`PostMetrics`]); nothing here re-reads them.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct RowCounts {
    /// Beside "Reply".
    pub(super) replies: Option<String>,
    /// Beside "Repost" / "Reposted".
    pub(super) reposts: Option<String>,
    /// Beside "Like" / "Liked".
    pub(super) likes: Option<String>,
}

/// Build one row's [`RowCounts`] from whatever metrics it carries.
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

/// One count, abbreviated, or `None` for zero — see [`RowCounts`] for why
/// zero is nothing rather than "0".
fn non_zero_count(count: u64) -> Option<String> {
    (count > 0).then(|| compact_count(count))
}

/// One action with its engagement count beside it (#95), or the action on
/// its own when there is no count to show.
///
/// The count is a sibling rather than part of the action's own element so
/// that clicking the number does nothing: the actions are toggles that
/// spend a request, and a count that looks like part of the button would
/// widen the target for an action the reader only meant to read.
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

/// Abbreviate a count the way X's own UI does — `12345` becomes `12.3K` —
/// so a popular post cannot push the timestamp and byline around by being
/// seven digits wide. A trailing `.0` is dropped (`1000` is `1K`, not
/// `1.0K`); below 1000 the number is shown as-is.
pub(super) fn compact_count(count: u64) -> String {
    #[expect(
        clippy::cast_precision_loss,
        reason = "an abbreviated count is approximate by construction"
    )]
    fn scaled(count: u64, unit: u64, suffix: char) -> String {
        let value = count as f64 / unit as f64;
        // One decimal, truncated rather than rounded, so the label never
        // claims more engagement than the post actually has.
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

/// Turn `2026-08-16T09:00:00.000Z` into `2026-08-16 09:00`.
///
/// The API always returns UTC in RFC 3339, so slicing beats pulling in a date
/// library for a label this small.
pub(super) fn format_timestamp(created_at: Option<&str>) -> String {
    let Some(raw) = created_at else {
        return String::new();
    };
    // `get`, not `&time[..5]` (#47, `clippy::string_slice`): that is a byte
    // range, and a `time` half whose fifth byte falls inside a multi-byte
    // character would panic rather than fall through to the raw string.
    // `created_at` comes from the API, so this is remote input.
    match raw.split_once('T') {
        Some((date, time)) => match time.get(..5) {
            Some(hhmm) => format!("{date} {hhmm}"),
            None => raw.to_string(),
        },
        None => raw.to_string(),
    }
}
