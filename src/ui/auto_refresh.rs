//! When the window polls its own timeline, and what it does with what
//! comes back (#21).
//!
//! Split out of `ui` the way [`super::reload_policy`] was (#126), but on a
//! different line. That file holds decisions and leaves the acting to
//! `ui`; this one holds the whole of #21 — the pure decisions *and* the
//! loop that acts on them, as an `impl TimelineView` block below the
//! functions it calls. A timer, a buffer, and the two ways the buffer is
//! emptied are one mechanism, and half of it filed under `ui`'s other
//! three thousand lines is half nobody finds.
//!
//! What the split still buys is the thing it was for: everything above the
//! `impl` is pure, so the decisions that make auto-refresh either cheap or
//! expensive are unit tested without gpui.
//!
//! # Why this is not `since_id` polling
//!
//! #21 was written for the home timeline, where an incremental fetch is a
//! `since_id` away. #161 moved the window onto a List, and
//! `GET /2/lists/:id/tweets` accepts no `since_id` at all — see
//! `XClient::list_timeline`. There is no cheaper request to send: a poll
//! re-reads the head page or it does not run.
//!
//! That sounds worse than it is. Reads bill per returned resource,
//! deduplicated within a UTC day (see the `x-api-budget` skill), so
//! re-reading the same head page all afternoon bills only the posts that
//! were genuinely new — which is what reading them costs however they
//! arrive. The repeated charge is one head page after each UTC midnight,
//! bounded by `max_results`.
//!
//! So the design here spends its care somewhere else than on the request:
//! on not disturbing the reader with what the request brought back. A poll
//! never replaces what is on screen. It parks the merged timeline in a
//! [`Pending`] buffer and the window offers it as a count the reader can
//! press — #21's own wording, and the reason the scroll position is never
//! moved by a fetch nobody asked for.

// `super::*` rather than a list, matching [`super::render`]: the `impl`
// block below reaches most of what `ui` imports, and keeping the two child
// modules' preambles the same shape is worth more than an exact list that
// has to be edited every time a method moves in or out.
use super::*;

/// How long a tick waits before looking again when a fetch the reader
/// started is still in flight.
///
/// Short, because it is not a cadence — it is a re-check. Whichever fetch
/// is running has already moved `last_reload_at` to its own start time, so
/// the tick after this one computes a full interval from there. Nothing
/// polls twice as a result of waiting a few seconds here.
const BUSY_RECHECK_SECONDS: i64 = 5;

/// What one wake-up of the auto-refresh loop should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tick {
    /// Nothing due yet. Sleep until this unix time and decide again —
    /// the loop re-reads the clock rather than trusting this deadline,
    /// so a machine that slept through it simply polls on waking.
    Wait { until: i64 },
    /// Spend a poll now.
    Poll,
}

/// Everything [`next_tick`] decides from.
#[derive(Debug, Clone, Copy)]
pub(super) struct Situation {
    /// When the last fetch of any kind went out — the button, a shortcut,
    /// a previous poll. `None` when nothing has been fetched this session,
    /// which is the startup-cache-hit case: see [`next_tick`] for why that
    /// falls back to `started_at` rather than polling straight away.
    pub last_reload_at: Option<i64>,
    /// When the loop was started, as the anchor for the first poll.
    ///
    /// A fixed timestamp rather than "now plus an interval" computed each
    /// wake-up: the latter moves with the clock, so the deadline would
    /// recede exactly as fast as the loop approached it and the first poll
    /// would never arrive.
    pub started_at: i64,
    pub interval_seconds: u32,
    /// Whether a fetch is already in flight — see [`BUSY_RECHECK_SECONDS`].
    pub busy: bool,
}

/// What this wake-up should do.
///
/// The anchor is `last_reload_at`, falling back to `started_at`, which is
/// what keeps auto-refresh a *cadence* rather than a change to what the
/// app spends at either end:
///
/// - A manual reload pushes the next poll a full interval out, so pressing
///   the button does not also buy a poll a few seconds later.
/// - A startup that spent nothing because the cache answered (#9) still
///   spends nothing for one interval. Auto-refresh adds a rhythm to a
///   window left open; it is not a second opinion on what startup decided.
pub(super) fn next_tick(situation: &Situation, now: i64) -> Tick {
    if situation.busy {
        return Tick::Wait {
            until: now.saturating_add(BUSY_RECHECK_SECONDS),
        };
    }
    let anchor = situation.last_reload_at.unwrap_or(situation.started_at);
    let due = anchor.saturating_add(i64::from(situation.interval_seconds));
    if due > now {
        Tick::Wait { until: due }
    } else {
        Tick::Poll
    }
}

/// Posts a poll fetched that the reader has not been shown yet (#21).
#[derive(Debug)]
pub(super) struct Pending {
    /// The whole merged timeline the poll came back with, not just the new
    /// rows — `cache::reload_primary` returns cache and fresh batch already
    /// spliced, and that combined list is what should be displayed once the
    /// reader asks for it. Keeping only the new rows would drop everything
    /// a "Load older" had appended.
    pub items: Vec<TimelineItem>,
    /// How many of them are new relative to what is on screen. What the
    /// pill counts, and never zero — see [`pending_after_poll`].
    pub count: usize,
}

/// What a finished poll leaves waiting for the reader.
///
/// `None` means the poll found nothing new, which is the ordinary outcome
/// and must leave the screen completely untouched: no pill, no banner, no
/// scroll. A poll that reports "no new posts" every five minutes is noise
/// the reader did not ask for, unlike a reload they pressed themselves
/// (#141), which says so precisely because they are waiting to hear.
///
/// Counted with [`newly_arrived`] — the same leading-run rule the manual
/// reload's own count and scroll compensation use, so the pill can never
/// promise more posts than pressing it actually reveals.
pub(super) fn pending_after_poll(
    displayed: &[&str],
    incoming: Vec<TimelineItem>,
) -> Option<Pending> {
    let incoming_ids: Vec<&str> = incoming.iter().map(|item| item.id.as_str()).collect();
    let count = newly_arrived(displayed, &incoming_ids);
    if count == 0 {
        return None;
    }
    Some(Pending {
        items: incoming,
        count,
    })
}

/// What the pill says.
///
/// Singular and plural spelled out rather than an "(s)", matching
/// `reload_policy::reload_outcome_label`, which this deliberately reads
/// like: the two report the same fact from the two directions a post can
/// arrive from, and a reader should not have to notice which one they are
/// looking at.
pub(super) fn pending_label(count: usize) -> String {
    match count {
        1 => "1 new post".to_string(),
        n => format!("{n} new posts"),
    }
}

/// The half of auto-refresh that cannot be pure: the loop that spends
/// the request, and what the window does with the answer (#21).
///
/// An `impl` block in a child module, unlike [`super::reload_policy`] and
/// [`super::render`], which are free functions over data. The reason is
/// that #21 is one mechanism — a timer, a buffer, and the two ways the
/// buffer is emptied — and splitting it across a pure file here and four
/// methods in `ui` would leave neither half readable on its own. A child
/// module can see its parent's private items, so `TimelineView`'s fields
/// stay private to `ui` and nothing is widened to make this possible.
impl TimelineView {
    /// Poll the timeline for new posts on a timer while the window is open
    /// (#21).
    ///
    /// Returns before spawning anything when `config.auto_refresh` is off
    /// or there is no client to fetch with. That early return is the whole
    /// of #21's "switch it off and the app sends nothing" condition:
    /// nothing else in this method is reachable, so there is no timer left
    /// running to be trusted not to fire.
    ///
    /// Started from [`Self::start`] and again from [`Self::sign_in`], the
    /// same two places `start_auto_sync` is, and for the same reason — a
    /// client only exists after one of them. Reassigning cancels whatever
    /// loop was already running, so a re-sign-in leaves one loop, not two.
    ///
    /// What a tick decides is [`auto_refresh::next_tick`]'s, and what it
    /// does with the result is [`pending_after_poll`]'s; both are pure and
    /// tested next door. What is left here is the part that cannot be:
    /// spending the request and putting the answer somewhere.
    pub(super) fn start_auto_refresh(&mut self, cx: &mut Context<'_, Self>) {
        /// Longest one `timer` call waits, so the loop re-reads the clock
        /// rather than trusting a deadline computed before the machine
        /// slept — `start_auto_sync`'s constant, for its reason.
        const MAX_SLEEP_SECONDS: i64 = 60;
        /// Shortest gap between wake-ups, so the loop stays cancellable.
        const MIN_SLEEP_SECONDS: i64 = 1;

        if !self.config.auto_refresh {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };

        let paths = self.paths.clone();
        let source = self.source.clone();
        let max_results = self.config.max_results;
        let interval_seconds = self.config.auto_refresh_interval_seconds;
        let started_at = oauth::unix_now();
        log::info(&format!(
            "auto-refresh is on, polling every {interval_seconds}s"
        ));

        self.auto_refresh = Some(cx.spawn(async move |this, cx| {
            loop {
                // `Err` is the window being gone, which is the one reason
                // this loop ever ends — `start_auto_sync`'s contract.
                let Ok(situation) = this.update(cx, |this, _| Situation {
                    last_reload_at: this.last_reload_at,
                    started_at,
                    interval_seconds,
                    busy: this.reloading,
                }) else {
                    return;
                };

                let now = oauth::unix_now();
                let sleep_until = match next_tick(&situation, now) {
                    Tick::Wait { until } => until,
                    Tick::Poll => {
                        // Recorded before the request goes out, exactly as
                        // `reload` does it: the fetch has been decided on,
                        // so it is what the next interval is measured
                        // from, whether or not it comes back.
                        let _ = this.update(cx, |this, _| this.last_reload_at = Some(now));

                        let result = {
                            let (paths, client, source) =
                                (paths.clone(), client.clone(), source.clone());
                            cx.background_executor()
                                .spawn(async move {
                                    cache::reload_primary(
                                        &paths,
                                        &client,
                                        &source,
                                        max_results,
                                        oauth::unix_now(),
                                    )
                                })
                                .await
                        };

                        let _ = this.update(cx, |this, cx| this.apply_poll(result, cx));
                        now.saturating_add(i64::from(interval_seconds))
                    }
                };

                let wait = sleep_until
                    .saturating_sub(oauth::unix_now())
                    .clamp(MIN_SLEEP_SECONDS, MAX_SLEEP_SECONDS);
                cx.background_executor()
                    .timer(Duration::from_secs(u64::try_from(wait).unwrap_or(1)))
                    .await;
            }
        }));
    }

    /// What a finished poll does to the window (#21).
    ///
    /// Deliberately quiet. A poll is not something the reader asked for,
    /// so it may not take the screen: `state` is untouched, the scroll
    /// position is untouched, and `reload_notice` — which belongs to the
    /// reader's own last reload, countdown included — is never written
    /// here. All a successful poll can do is fill `pending`, which the
    /// pill offers and nothing else acts on.
    ///
    /// A failed poll does even less: it is logged and dropped. The reload
    /// path raises a banner because someone is waiting to hear the answer;
    /// nobody is waiting on this one, and a network blip five minutes ago
    /// is not worth a red line over a timeline that is fine. `usage` is
    /// still refreshed either way — the request was sent and billed
    /// whether or not it parsed.
    ///
    /// `next_page_token` is deliberately not updated. It is the "Load
    /// older" cursor, describing how far back the reader has paged; a head
    /// page fetched behind their back would reset it mid-scroll.
    pub(super) fn apply_poll(
        &mut self,
        result: anyhow::Result<cache::ReloadedPrimary>,
        cx: &mut Context<'_, Self>,
    ) {
        self.refresh_usage(cx);
        let reloaded = match result {
            Ok(reloaded) => reloaded,
            Err(error) => {
                // `log::redact` runs on the way out — an API error can
                // quote the request that produced it.
                log::error(&format!("auto-refresh poll failed: {error:#}"));
                return;
            }
        };

        // The header names the signed-in account and several affordances
        // need the id; a poll resolves both for free, so it may as well
        // fill them in if the startup fetch never managed to.
        self.home_user_id = Some(reloaded.me.id);
        self.home_username = Some(reloaded.me.username);

        let displayed: Vec<&str> = match &self.state {
            TimelineState::Loaded(items) => items.iter().map(|item| item.id.as_str()).collect(),
            _ => Vec::new(),
        };
        let Some(pending) = pending_after_poll(&displayed, reloaded.items) else {
            // Nothing new. Not even a notice — see this method's doc.
            return;
        };
        self.pending = Some(pending);
        // Images are not fetched ahead. `refresh_avatars`/`refresh_media`
        // read `self.state` to decide what is missing, and both hold a
        // single task slot that assigning cancels — pre-downloading the
        // buffer's images would mean either teaching them to read from
        // somewhere else or cancelling the visible timeline's own
        // downloads on a timer. [`Self::apply_pending`] fetches them the
        // moment the rows are actually on screen, which is the same path
        // and the same brief placeholder a manual reload already has.
        cx.notify();
    }

    /// Show what the last poll fetched (#21).
    ///
    /// The one thing the pill does, and everything it does: replace the
    /// timeline with the buffered list and put the reader at the top of
    /// it, which is where the posts they just asked to see are.
    ///
    /// Scrolling to the top rather than compensating the way
    /// [`Self::keep_the_reader_in_place`] does for a reload, because the
    /// two answer opposite requests. A reload is "refresh this, I am
    /// reading here"; pressing a pill that counts new posts is "show me
    /// those", and leaving the reader exactly where they were would be a
    /// button that visibly does nothing.
    ///
    /// No `ReloadNotice::Outcome` is raised: the pill already said how
    /// many posts there were, and a banner repeating the count the instant
    /// the pill disappears is the same fact told twice.
    pub(super) fn apply_pending(&mut self, cx: &mut Context<'_, Self>) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        self.state = TimelineState::Loaded(pending.items);
        self.list_scroll.scroll_to_top_of_item(0);
        self.refresh_images(cx);
        cx.notify();
    }

    /// Drop whatever a poll left waiting (#21).
    ///
    /// Called from every path that replaces `state` from a fresher source
    /// than the buffer: a finished reload, a finished "Load older", a
    /// delete, a sign-in. A stale buffer is not merely out of date, it is
    /// wrong in a way that undoes work — applying one fetched before a
    /// delete puts the deleted post back on screen, and one fetched before
    /// a "Load older" drops the page that was just appended.
    ///
    /// The count would be wrong too: it was measured against a timeline
    /// that is no longer what is displayed, so the pill would be promising
    /// posts that are already visible.
    pub(super) fn clear_pending(&mut self) {
        self.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn situation(last_reload_at: Option<i64>, started_at: i64) -> Situation {
        Situation {
            last_reload_at,
            started_at,
            interval_seconds: 300,
            busy: false,
        }
    }

    fn item(id: &str) -> TimelineItem {
        TimelineItem {
            id: id.to_string(),
            text: format!("post {id}"),
            created_at: None,
            author_name: String::new(),
            author_username: "someone".to_string(),
            reposted_by: None,
            quoted: None,
            replied_to: None,
            metrics: None,
            links: Vec::new(),
            author_avatar_url: None,
            original_post_id: None,
            media: Vec::new(),
        }
    }

    #[test]
    fn the_first_poll_is_one_interval_after_the_window_opened() {
        assert_eq!(
            next_tick(&situation(None, 1_000), 1_000),
            Tick::Wait { until: 1_300 }
        );
    }

    // The deadline has to be a fixed instant. Computed from `now` on every
    // wake-up it would recede as fast as the loop approached it, and
    // auto-refresh would be a timer that never fires.
    #[test]
    fn the_first_polls_deadline_does_not_move_as_the_loop_waits() {
        assert_eq!(
            next_tick(&situation(None, 1_000), 1_299),
            Tick::Wait { until: 1_300 }
        );
        assert_eq!(next_tick(&situation(None, 1_000), 1_300), Tick::Poll);
    }

    #[test]
    fn a_poll_is_due_once_the_interval_since_the_last_fetch_has_elapsed() {
        assert_eq!(next_tick(&situation(Some(1_000), 500), 1_300), Tick::Poll);
    }

    #[test]
    fn a_poll_is_not_due_before_the_interval_has_elapsed() {
        assert_eq!(
            next_tick(&situation(Some(1_000), 500), 1_299),
            Tick::Wait { until: 1_300 }
        );
    }

    // #10's interval and #21's cadence must agree about what a reload
    // costs: pressing the button is a fetch, so it pushes the next poll
    // out rather than being followed by one moments later.
    #[test]
    fn a_manual_reload_pushes_the_next_poll_a_full_interval_out() {
        let mut situation = situation(Some(2_000), 500);
        situation.interval_seconds = 300;
        assert_eq!(next_tick(&situation, 2_001), Tick::Wait { until: 2_300 });
    }

    #[test]
    fn a_fetch_in_flight_defers_the_decision_rather_than_polling_beside_it() {
        let mut situation = situation(Some(1_000), 500);
        situation.busy = true;
        assert_eq!(
            next_tick(&situation, 9_000),
            Tick::Wait {
                until: 9_000 + BUSY_RECHECK_SECONDS
            }
        );
    }

    #[test]
    fn a_poll_that_brought_nothing_new_leaves_nothing_waiting() {
        let displayed = ["3", "2", "1"];
        let incoming = vec![item("3"), item("2"), item("1")];

        assert!(pending_after_poll(&displayed, incoming).is_none());
    }

    #[test]
    fn a_poll_that_brought_new_posts_counts_them() {
        let displayed = ["3", "2", "1"];
        let incoming = vec![item("5"), item("4"), item("3"), item("2"), item("1")];

        let pending = pending_after_poll(&displayed, incoming).expect("two posts arrived");
        assert_eq!(pending.count, 2);
    }

    // The buffer is the whole merged list, so applying it cannot drop the
    // pages a "Load older" appended below what the poll fetched.
    #[test]
    fn the_pending_buffer_holds_the_whole_timeline_not_just_the_new_rows() {
        let displayed = ["3", "2", "1"];
        let incoming = vec![item("4"), item("3"), item("2"), item("1")];

        let pending = pending_after_poll(&displayed, incoming).expect("one post arrived");
        assert_eq!(pending.count, 1);
        assert_eq!(pending.items.len(), 4);
    }

    // Only the leading run counts, exactly as a manual reload counts it —
    // an id further down is a post that moved, not one that arrived, and
    // the pill must not promise a post pressing it will not reveal.
    #[test]
    fn only_the_leading_run_of_new_ids_is_counted() {
        let displayed = ["2", "1"];
        let incoming = vec![item("4"), item("2"), item("3"), item("1")];

        let pending = pending_after_poll(&displayed, incoming).expect("one post arrived");
        assert_eq!(pending.count, 1);
    }

    // A window that has nothing on screen yet (a failed startup, an empty
    // list) treats everything a poll brought as new, which is what it is.
    #[test]
    fn everything_is_new_when_nothing_is_displayed_yet() {
        let pending =
            pending_after_poll(&[], vec![item("2"), item("1")]).expect("two posts arrived");
        assert_eq!(pending.count, 2);
    }

    #[test]
    fn one_new_post_is_not_reported_in_the_plural() {
        assert_eq!(pending_label(1), "1 new post");
    }

    #[test]
    fn several_new_posts_are() {
        assert_eq!(pending_label(6), "6 new posts");
    }
}
