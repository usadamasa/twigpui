//! Whether a reload may run, and what the app says when one does not.
//!
//! Split out of `ui` (#126) alongside [`super::render`], on a different
//! line: these are the functions standing between a click and a paid
//! request. `reload_gate` and `reload_cooldown` decide whether to spend
//! anything at all; `reload_failure_outcome` and `cooldown_tick` decide
//! what survives on screen when the answer is no. Together they read as
//! one piece of money-facing logic rather than a handful of functions
//! scattered among render helpers.
//!
//! Pure, and tested as such — the async paths that call them stay in `ui`
//! and are not unit tested.

use super::{Cooldown, ReloadNotice, ReloadTrigger, TimelineState, cache, rate_limit};

/// Countdown text for the reload button while blocked by #10's rate-limit
/// decision. `remaining` is clamped to zero rather than going negative if
/// `reset_at` has (just) passed by the time this renders.
///
/// The two cooldowns read differently on purpose: only one of them is X
/// actually rate limiting this app, and reporting the self-imposed interval
/// as a rate limit would misdescribe what happened.
pub(super) fn cooldown_label(cooldown: Cooldown, reset_at: i64, now: i64) -> String {
    // `saturating_sub` (#47): `reset_at` comes from an API header and
    // `now` from the clock, so neither is this code's to trust.
    let remaining = reset_at.saturating_sub(now).max(0);
    match cooldown {
        Cooldown::LocalInterval => format!("Waiting out the fetch interval — {remaining}s"),
        Cooldown::ApiRateLimit => format!("Rate limited by X — retry in {remaining}s"),
    }
}

/// Classify a failed reload/load-older's error into the [`ReloadNotice`] it
/// should raise (#57) — the single place that decides "rate limit with a
/// known reset time" vs "plain failure", shared by [`map_reload_error`] (the
/// fallback for when there's nothing else to show) and
/// [`reload_failure_outcome`] (the common case, once there's a timeline that
/// must survive the failure).
///
/// #10: a blocked-send carries a known reset time and is shown as a
/// countdown; everything else (including a rate limit whose 429 carried no
/// usable reset header) falls back to the plain error message.
pub(super) fn reload_notice_for_error(error: &anyhow::Error) -> ReloadNotice {
    match error.downcast_ref::<rate_limit::RateLimited>() {
        Some(rate_limit::RateLimited {
            reset_at: Some(reset_at),
        }) => ReloadNotice::Cooldown {
            reset_at: *reset_at,
            cooldown: Cooldown::ApiRateLimit,
        },
        _ => ReloadNotice::Failed(format!("{error:#}").into()),
    }
}

/// Map a failed reload's error to the state that should show it, for when
/// there is nothing else on screen to fall back to. The only caller left as
/// of #57 is [`reload_failure_outcome`], and only once it has confirmed
/// there is no loaded timeline this failure would otherwise evict —
/// `TimelineView::reload` and `TimelineView::load_older` both reach this
/// exclusively through that path now, never directly.
pub(super) fn map_reload_error(error: &anyhow::Error) -> TimelineState {
    match reload_notice_for_error(error) {
        ReloadNotice::Cooldown { reset_at, cooldown } => {
            TimelineState::RateLimited { reset_at, cooldown }
        }
        ReloadNotice::Failed(message) => TimelineState::Failed(message),
    }
}

/// What a failed fetch should do to `state`, and which notice (if any) it
/// should raise (#57) — shared by [`TimelineView::reload`] (via
/// `TimelineView::apply_reload_failure`) and `TimelineView::load_older`, a
/// pure function so "an existing timeline survives a failed fetch" can be
/// unit tested without gpui. A failed refresh is not evidence that whatever
/// is already loaded is wrong, so whenever `state` already holds posts, they
/// are returned untouched and the failure becomes a notice, via
/// [`reload_notice_for_error`] — this is the only branch that returns
/// `Some`. When there is nothing being displayed yet, the failure instead
/// becomes the state itself — the same [`map_reload_error`] mapping every
/// other failed fetch in this file uses — and the notice comes back `None`:
/// `state` (`Failed`/`RateLimited`) is already telling the body what
/// happened, so a banner repeating the identical message would just be a
/// duplicated failure on screen.
pub(super) fn reload_failure_outcome(
    state: TimelineState,
    error: &anyhow::Error,
) -> (TimelineState, Option<ReloadNotice>) {
    match state {
        TimelineState::Loaded(items) => (
            TimelineState::Loaded(items),
            Some(reload_notice_for_error(error)),
        ),
        _ => (map_reload_error(error), None),
    }
}

/// Whether the header should offer a "Load older" button (#11): only once a
/// response has actually carried a `meta.next_token` to resume from, and
/// only while the timeline is in a state where clicking it makes sense.
///
/// Withheld at the post cap, which is the part that matters for money.
/// `cache::splice` truncates back down to `MAX_CACHED_POSTS`, so at the
/// cap a click would spend a real API request and then discard every post it
/// bought — a paid no-op, in a project whose entire cache exists to avoid
/// exactly that. [`at_the_post_cap`] renders an explanation in its place so
/// the button does not just silently vanish.
pub(super) fn offers_load_older(next_page_token: Option<&str>, state: &TimelineState) -> bool {
    match state {
        TimelineState::Loaded(items) => {
            next_page_token.is_some() && items.len() < cache::MAX_CACHED_POSTS
        }
        _ => false,
    }
}

/// Whether the loaded timeline has hit the cap that [`offers_load_older`]
/// stops at, so the body can say why there is nothing further back.
pub(super) fn at_the_post_cap(state: &TimelineState) -> bool {
    matches!(state, TimelineState::Loaded(items) if items.len() >= cache::MAX_CACHED_POSTS)
}

/// Whether [`TimelineView::reload`] should refuse to run right now, per
/// `config.min_fetch_interval_seconds` (#10). `None` means "go ahead" —
/// either there has never been a reload yet, or the interval since the last
/// one has already elapsed. `Some(reset_at)` means "not yet", carrying when
/// it becomes allowed again, in the same shape [`cooldown_label`] expects.
pub(super) fn reload_cooldown(
    last_reload_at: Option<i64>,
    min_interval_seconds: u32,
    now: i64,
) -> Option<i64> {
    let last = last_reload_at?;
    let reset_at = last.saturating_add(i64::from(min_interval_seconds));
    (reset_at > now).then_some(reset_at)
}

/// Whether [`TimelineView::reload`] should refuse to run right now, given
/// `trigger` (#57). `ReloadTrigger::UserAction` bypasses [`reload_cooldown`]
/// entirely and always returns `None` — see [`ReloadTrigger`]'s doc for why
/// a post-submit or sign-in reload must never be blocked by an interval that
/// exists to suppress polling, not to gate a direct response to something
/// the user just did. `ReloadTrigger::Polling` defers to `reload_cooldown`
/// unchanged.
pub(super) fn reload_gate(
    trigger: ReloadTrigger,
    last_reload_at: Option<i64>,
    min_interval_seconds: u32,
    now: i64,
) -> Option<i64> {
    match trigger {
        ReloadTrigger::Polling => reload_cooldown(last_reload_at, min_interval_seconds, now),
        ReloadTrigger::UserAction => None,
    }
}

/// What `state` should become right before spawning a fetch (#57) — shared
/// by [`TimelineView::reload`] and `TimelineView::load_older`, a pure
/// function so "an existing timeline survives a fetch in progress" can be
/// unit tested without gpui. Fetching a fresh copy is not evidence the
/// previous one is stale or wrong, so whenever `previous` already holds
/// posts, this leaves them in place; the header's busy indicator comes from
/// `TimelineView::reloading` instead, set alongside this rather than folded
/// into `state` (see that field's doc). Only when there is nothing loaded
/// yet does this fall back to `TimelineState::Loading`, matching the
/// pre-#57 behavior for the one case where there is nothing to lose.
pub(super) fn reload_start_state(previous: TimelineState) -> TimelineState {
    match previous {
        TimelineState::Loaded(items) => TimelineState::Loaded(items),
        _ => TimelineState::Loading,
    }
}

/// What one wake-up of [`TimelineView::start_cooldown_ticker`]'s loop should
/// do (#57's item 3), given the current `reload_notice` and the time — the
/// pure decision behind that loop, factored out so it's unit-testable
/// without gpui's timer; the loop itself just matches on this and either
/// keeps going or returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CooldownTick {
    /// Nothing to tick: `reload_notice` is `None`, or holds a `Failed`
    /// notice with no countdown to advance — either it was never a
    /// cooldown, or something else (a success, a plain failure) has
    /// already replaced it since the ticker started. The loop should stop
    /// without touching `reload_notice`.
    NotTicking,
    /// Still inside the cooldown window: re-notify so the banner's
    /// countdown advances, then wait another second.
    StillWaiting,
    /// `reset_at` has passed. The loop should clear `reload_notice` — the
    /// banner disappearing, and "Reload" becoming clickable again, is the
    /// user-visible "done waiting" signal — and stop.
    Elapsed,
}

/// Pure core of [`CooldownTick`]'s decision — see its doc for what each
/// variant means the loop should do.
pub(super) fn cooldown_tick(notice: Option<&ReloadNotice>, now: i64) -> CooldownTick {
    match notice {
        Some(ReloadNotice::Cooldown { reset_at, .. }) if *reset_at > now => {
            CooldownTick::StillWaiting
        }
        Some(ReloadNotice::Cooldown { .. }) => CooldownTick::Elapsed,
        Some(ReloadNotice::Failed(_)) | None => CooldownTick::NotTicking,
    }
}

/// Where the reader should be left after a reload prepends new posts
/// (#22): the index in the *new* list of whatever row was at the top of
/// the viewport before, or `None` to leave the scroll position alone.
///
/// `None` means both "stay where you are" cases, which are the same
/// instruction to the caller even though they are different situations:
/// the reader was already at the very top, so the new posts should simply
/// appear above nothing and be seen; or nothing was prepended, so there is
/// nothing to compensate for.
///
/// Anything else shifts the reader. A reload that brings six new posts
/// while someone is twenty rows down moves what they were reading twenty-
/// six rows down the list, and the viewport stays where it was — the text
/// under their eyes changes without them touching anything. Counting the
/// leading ids that were not on file is exactly how far to scroll to undo
/// that.
///
/// Takes ids rather than items so it stays a pure function over what
/// changed, and counts only the *leading* run: an id appearing further
/// down is a post that moved rather than one that arrived, and moving the
/// viewport for it would be wrong.
pub(super) fn preserved_scroll_target(
    previous_ids: &[&str],
    new_ids: &[&str],
    top_item: usize,
) -> Option<usize> {
    if top_item == 0 {
        return None;
    }
    let previous: std::collections::HashSet<&str> = previous_ids.iter().copied().collect();
    let prepended = new_ids
        .iter()
        .take_while(|id| !previous.contains(*id))
        .count();
    if prepended == 0 {
        return None;
    }
    Some(top_item.saturating_add(prepended))
}
