//! The list sync as the window sees it (#174): what it is doing, how to
//! say so, and how to start one by hand.
//!
//! `sync/` mirrors the accounts this app follows into a List. Until #174
//! that whole feature was invisible from the window and unreachable from
//! it: a loop started at sign-in, woke on a six-hour interval, and said
//! something only on the two outcomes [`sync::notice`] does not suppress.
//! Someone watching a list that was thousands of accounts behind had no
//! way to tell a catch-up in progress from nothing happening at all, and
//! no way to ask for one.
//!
//! So this file adds the two halves the issue names. [`SyncStatus`] is
//! what the status bar reports, updated from every tick's
//! [`sync::Outcome`]; [`TimelineView::start_sync`] is the loop, now
//! startable by hand as well as at sign-in.
//!
//! Laid out like [`super::auto_refresh`], which set the precedent: pure
//! functions with their tests first, then an `impl TimelineView` block for
//! the parts that spend. Same reason, too — a status enum, the loop that
//! writes it, and the button that starts the loop are one mechanism.
//!
//! # Why starting one costs a confirmation
//!
//! A diff reads the whole follow list and the whole list membership, and
//! both bill per account returned (`x-api-budget`). At a few thousand
//! follows that is dollars for one click — the most expensive thing anyone
//! can press in this window by an order of magnitude. The skill's rule for
//! that case is to put the worst case on screen before the press, which is
//! what [`sync_confirm_label`] is for and why the button is a two-step
//! like #72's delete rather than a single click.

// Spelled out rather than `use super::*` like [`super::render`] and
// [`super::auto_refresh`]: this module names few enough of `ui`'s imports
// that clippy's `wildcard_imports` can enumerate them, and so does not let
// the glob past.
use super::{
    AnyElement, Context, Duration, InteractiveElement as _, IntoElement as _, ParentElement as _,
    ReloadNotice, StatefulInteractiveElement as _, Styled as _, Theme, TimelineView, div, log,
    oauth, rgb, sync,
};

/// What the window knows about the list sync right now (#174).
///
/// Written by the loop after every tick and read only by the status bar,
/// so it describes the sync rather than driving it: nothing here decides
/// whether a request goes out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SyncStatus {
    /// Not running, and not startable either — carrying which of the
    /// gates it is stopped at.
    ///
    /// The reason is the point. "List sync: off" tells someone whose
    /// session predates the scopes exactly as much as it tells someone
    /// who has configured no list, and they need opposite things.
    Off(SyncOff),
    /// Not running, but a click would start one. Where a window sits when
    /// `auto_sync_list` is off and everything else is in place.
    Ready,
    /// The signed-in id has not landed yet, so there is nothing to diff a
    /// follow list against. Distinct from [`SyncStatus::Working`] because
    /// nothing is being spent and nothing is in flight — the loop is
    /// waiting on the startup fetch, and if that keeps failing this is
    /// where it stays.
    AwaitingAccount,
    /// A tick is in flight — reads, writes, or both.
    Working,
    /// Between ticks. `pending` is what the plan on file still owes; zero
    /// is the steady state and anything else is a catch-up that has been
    /// paced or blocked.
    Idle { until: i64, pending: usize },
    /// A write was refused by the tracked rate-limit window. Nothing was
    /// spent, and `pending` is still owed.
    RateLimited { until: i64, pending: usize },
    /// The last tick failed outright — a revoked scope, a deleted list, a
    /// plan file that will not parse. The loop has already given itself a
    /// full interval to try again; this is here so the window does not
    /// keep reporting the last success as though it were current.
    Failed,
}

/// Which gate a stopped sync is stopped at — see [`SyncStatus::Off`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SyncOff {
    /// No `list_id`, so there is nothing to mirror into. The one gate a
    /// manual start cannot get past either.
    NoList,
    /// The session does not carry the scopes the sync needs
    /// ([`sync::missing_scope`]). Fixed by "Re-authorize", which the
    /// header already offers, and without a restart.
    MissingScope,
    /// No credential at all yet.
    NotSignedIn,
}

/// Whether the status bar's sync segment is something to press (#174).
///
/// Three of the states say no, for three different reasons: [`SyncStatus::Off`]
/// because the gate a click would hit is the one it is already stopped at,
/// [`SyncStatus::Working`] because a run is in flight — starting a second
/// diff on top of it is the double-charge this guards against — and
/// [`SyncStatus::AwaitingAccount`] because there is nothing to diff
/// against until `/me` resolves.
///
/// [`SyncStatus::RateLimited`] *is* clickable: the loop is already waiting
/// the window out and a restart will simply wait it out again, which costs
/// nothing and is a reasonable thing to ask for.
pub(super) fn offers_sync(status: &SyncStatus) -> bool {
    match status {
        SyncStatus::Ready
        | SyncStatus::Idle { .. }
        | SyncStatus::RateLimited { .. }
        | SyncStatus::Failed => true,
        SyncStatus::Off(_) | SyncStatus::Working | SyncStatus::AwaitingAccount => false,
    }
}

/// What the status bar says about the sync (#174).
///
/// Every string is prefixed "List sync:" so the segment identifies itself
/// — the status bar's other two numbers are about requests and posts, and
/// a bare "1,204 to go" beside them would be a third unlabelled count.
///
/// The counts are what make this progress rather than a state name.
/// "Idle" for six hours and "Idle" during a catch-up that has eleven
/// hundred writes left are the same word for very different situations,
/// and it was not being able to tell them apart that #174 was filed about.
pub(super) fn sync_status_label(status: &SyncStatus, now: i64) -> String {
    match status {
        SyncStatus::Off(SyncOff::NoList) => "List sync: no list configured".to_string(),
        SyncStatus::Off(SyncOff::MissingScope) => "List sync: re-authorize to enable".to_string(),
        SyncStatus::Off(SyncOff::NotSignedIn) => "List sync: not signed in".to_string(),
        SyncStatus::Ready => "List sync: ready".to_string(),
        SyncStatus::AwaitingAccount => "List sync: waiting for your account".to_string(),
        SyncStatus::Working => "List sync: working…".to_string(),
        SyncStatus::Idle { pending: 0, .. } => "List sync: up to date".to_string(),
        SyncStatus::Idle { pending, .. } => format!("List sync: {pending} to go"),
        SyncStatus::RateLimited { until, pending } => {
            // `saturating_sub` and a floor of zero for `cooldown_label`'s
            // reason: `until` comes from an API header and `now` from the
            // clock, so neither is this code's to trust.
            let remaining = until.saturating_sub(now).max(0);
            format!("List sync: rate limited, {pending} to go — {remaining}s")
        }
        SyncStatus::Failed => "List sync: last attempt failed".to_string(),
    }
}

/// What the confirmation says before a manual sync is allowed to spend
/// (#174).
///
/// Names the worst case rather than the likely one, per `x-api-budget`'s
/// rule for a click that fans out into requests. It cannot name a number:
/// how many accounts are on either side is exactly what the reads are for,
/// and guessing from a previous plan would put a figure on screen that the
/// app does not actually know. So it names the shape of the charge and
/// leaves the size to the person who knows how many accounts they follow.
pub(super) fn sync_confirm_label() -> &'static str {
    "Reads your whole follow list and the whole list, billed per account. Sync anyway?"
}

/// What the status bar's sync segment should be colored with (#174).
///
/// `danger` only for the two states that are genuinely wrong — a failed
/// tick, and a gate that needs someone to do something. A rate limit is
/// not one of them: the loop is handling it, and the count beside it is
/// still true.
pub(super) fn sync_status_color(status: &SyncStatus, theme: Theme) -> u32 {
    match status {
        SyncStatus::Failed | SyncStatus::Off(SyncOff::MissingScope) => theme.danger,
        SyncStatus::Ready | SyncStatus::Idle { pending: 1.., .. } => theme.accent,
        SyncStatus::Off(_)
        | SyncStatus::AwaitingAccount
        | SyncStatus::Working
        | SyncStatus::Idle { .. }
        | SyncStatus::RateLimited { .. } => theme.text_tertiary,
    }
}

/// Which [`SyncStatus`] one finished tick leaves behind (#174).
///
/// `None` is a tick that failed, which is [`sync::settle`]'s `None` and
/// means the same thing here: the loop is still alive and has given itself
/// a full interval, but the window must stop reporting the last success as
/// current.
///
/// [`sync::Outcome::Diffed`] comes straight back to drain what it found
/// (`settle` sets `wake_at` to now), so it maps to [`SyncStatus::Working`]
/// rather than an idle state the next tick would overwrite in the same
/// second.
///
/// `owed` is what the plan was known to owe going in, and it exists for
/// exactly one arm: [`sync::Outcome::RateLimited`] carries a deadline and
/// nothing else, because a refusal spends nothing and marks nothing
/// applied — so what the plan owed before the tick is what it owes after.
/// Threading it through the loop is cheaper and more honest than reading
/// the plan file again to recover a number that did not change.
pub(super) fn status_after(
    outcome: Option<&sync::Outcome>,
    wake_at: i64,
    owed: usize,
) -> SyncStatus {
    match outcome {
        None => SyncStatus::Failed,
        Some(sync::Outcome::Idle { pending, .. }) => SyncStatus::Idle {
            until: wake_at,
            pending: *pending,
        },
        Some(sync::Outcome::RateLimited { until }) => SyncStatus::RateLimited {
            until: *until,
            pending: owed,
        },
        Some(sync::Outcome::Applied { remaining, .. }) => SyncStatus::Idle {
            until: wake_at,
            pending: *remaining,
        },
        Some(sync::Outcome::Diffed { .. }) => SyncStatus::Working,
    }
}

/// What [`status_after`] should be handed as `owed` next time round —
/// how much the plan is known to owe after this status.
///
/// A function rather than the loop reaching into the enum, so the one
/// place that has to keep the count alive across a rate limit is the same
/// place that decides what the count means.
pub(super) fn owed_by(status: &SyncStatus) -> usize {
    match status {
        SyncStatus::Idle { pending, .. } | SyncStatus::RateLimited { pending, .. } => *pending,
        // Nothing here knows a count. `Working` is a diff that has just
        // written a plan whose size it was not told; the tick that drains
        // it reports the real figure a moment later.
        SyncStatus::Working
        | SyncStatus::Off(_)
        | SyncStatus::Ready
        | SyncStatus::AwaitingAccount
        | SyncStatus::Failed => 0,
    }
}

/// Why a run was started, and therefore what it is allowed to skip and
/// when it may stop (#174).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SyncTrigger {
    /// The timer, started at sign-in. Honours `config.auto_sync_list` and
    /// runs for as long as the window is open.
    Scheduled,
    /// Someone pressed the status bar's sync segment.
    ///
    /// Two differences, both of them the point of #174. It runs even with
    /// `auto_sync_list` off — turning the timer off and syncing by hand is
    /// the reason to have a button at all — and its first tick ignores the
    /// interval, which is what makes pressing it do something rather than
    /// report that the next diff is four hours away.
    Manual,
}

/// The half of the list sync that cannot be pure: the loop that spends,
/// and the button that starts one (#174).
///
/// An `impl` block in a child module, following [`super::auto_refresh`] —
/// see this module's doc for why the whole feature lives in one file.
impl TimelineView {
    /// Which gate the sync is stopped at right now, or `None` when it
    /// could run (#174).
    ///
    /// The gates were already checked inside the loop's early returns;
    /// this pulls them out so the status bar can *say* which one, which is
    /// half of what the issue asked for. Before it, all three failures
    /// looked identical from the window: nothing.
    ///
    /// `auto_sync_list` is deliberately not one of them. It decides
    /// whether the timer runs, not whether a sync is possible, and a
    /// window with the timer off is exactly where the button matters most.
    fn sync_gate(&self) -> Option<SyncOff> {
        if self.config.list_id.is_none() {
            return Some(SyncOff::NoList);
        }
        if self.client.is_none() {
            return Some(SyncOff::NotSignedIn);
        }
        if sync::missing_scope(self.oauth_scope.as_deref()).is_some() {
            return Some(SyncOff::MissingScope);
        }
        None
    }

    /// Start the list sync: keep `config.list_id`'s membership mirroring
    /// the accounts this app follows.
    ///
    /// Three gates, all reported through [`SyncStatus::Off`] rather than
    /// raised as a banner — none of them is something the reader did, and
    /// an error message about a feature they may not have asked for is not
    /// what a window should open with. Since #174 they are *reported*
    /// though, in the status bar, which is the difference between a
    /// feature that is off and a feature that appears not to exist:
    ///
    /// - no `list_id`, so there is nothing to mirror into,
    /// - no credential yet,
    /// - a session predating the scopes the sync needs, which
    ///   [`sync::missing_scope`] catches before a single billed read.
    ///
    /// `config.auto_sync_list` is a fourth gate, and only for
    /// [`SyncTrigger::Scheduled`]: with the timer off the window sits at
    /// [`SyncStatus::Ready`] and waits to be asked.
    ///
    /// Reuses the credential [`Self::start`] already resolved rather than
    /// resolving its own. `oauth::resolve_credential` rotates the refresh
    /// token and writes it back, so two of them racing could leave the
    /// stored session dead — a much worse outcome than the access token in
    /// hand going stale over a very long run, which is what every other
    /// fetch path in this file already lives with.
    ///
    /// Assigning `self.auto_sync` drops whatever loop was running, so
    /// there is never more than one working the same plan file. That is
    /// also how a manual run supersedes the timer: it is the same slot.
    /// What it does *not* do is stop a tick already in flight — the tick
    /// is one synchronous poll on a background thread and runs to
    /// completion after the drop — which is why the manual path is gated
    /// on [`SyncStatus::Working`] and not on the task slot.
    pub(super) fn start_sync(&mut self, trigger: SyncTrigger, cx: &mut Context<'_, Self>) {
        /// Longest one `timer` call waits, so the loop re-reads the clock
        /// (and notices a machine that slept) rather than trusting a
        /// deadline computed hours ago.
        const MAX_SLEEP_SECONDS: i64 = 60;
        /// Shortest gap between ticks. Only reached between consecutive
        /// apply batches, where the answer is otherwise "immediately" —
        /// enough to keep the loop cancellable mid-catch-up.
        const MIN_SLEEP_SECONDS: i64 = 1;
        /// How long to wait when the signed-in id has not landed yet.
        /// Longer than `MIN_SLEEP_SECONDS` because there is nothing this
        /// loop can do to hurry it along: a startup fetch that keeps
        /// failing leaves `home_user_id` `None` indefinitely, and polling
        /// it every second for the life of the window would be a spin
        /// dressed up as patience.
        const AWAITING_ID_SECONDS: i64 = 30;

        if let Some(off) = self.sync_gate() {
            self.sync_status = SyncStatus::Off(off);
            cx.notify();
            return;
        }
        // Both checked by `sync_gate` above; unwrapped through `else`
        // rather than `expect` so a later change to that function cannot
        // turn this into a panic.
        let (Some(list_id), Some(client)) = (self.config.list_id.clone(), self.client.clone())
        else {
            return;
        };

        let scheduled = self.config.auto_sync_list;
        if matches!(trigger, SyncTrigger::Scheduled) && !scheduled {
            self.sync_status = SyncStatus::Ready;
            cx.notify();
            return;
        }

        let paths = self.paths.clone();
        let interval = self.config.sync_interval_seconds;
        let prune_limit = self.config.sync_prune_limit_percent;
        log::info(&format!(
            "list sync started for {list_id} ({trigger:?}), interval {interval}s"
        ));
        self.sync_status = SyncStatus::Working;
        cx.notify();

        self.auto_sync = Some(cx.spawn(async move |this, cx| {
            let mut blocked_until: Option<i64> = None;
            // What the plan is known to owe, carried across ticks because
            // a rate-limited one reports a deadline and no count — see
            // [`status_after`].
            let mut owed: usize = 0;
            // Consumed by the first tick. `last_diff_at: None` is what
            // `next_step` reads as "a diff has never run", which is
            // exactly the decision a manual start wants — and it leaves
            // the precedence alone, so a live rate limit still wins and an
            // undrained plan is still drained before anything is re-read.
            let mut forced = matches!(trigger, SyncTrigger::Manual);
            loop {
                // `Err` is the window being gone, which is the one reason
                // this loop ever ends other than a finished manual run.
                let Ok(user_id) = this.update(cx, |this, _| this.home_user_id.clone()) else {
                    return;
                };

                let now = oauth::unix_now();
                let sleep_until = match user_id {
                    // The startup fetch has not resolved the signed-in id
                    // yet. There is nothing to diff a follow list against
                    // until it has.
                    None => {
                        let _ = this.update(cx, |this, cx| {
                            this.show_sync(SyncStatus::AwaitingAccount, cx);
                        });
                        now.saturating_add(AWAITING_ID_SECONDS)
                    }
                    Some(user_id) => {
                        // Set before the await and left there for its
                        // whole length. `offers_sync` refuses a start in
                        // this state, and that refusal is the one thing
                        // standing between a second click and a second
                        // full paged read of both sides — dropping this
                        // task would not stop the tick below.
                        let _ = this.update(cx, |this, cx| {
                            this.show_sync(SyncStatus::Working, cx);
                        });
                        let pacing = sync::Pacing {
                            interval_seconds: interval,
                            blocked_until,
                            forced,
                        };
                        let outcome = {
                            let (paths, client, list_id) =
                                (paths.clone(), client.clone(), list_id.clone());
                            cx.background_executor()
                                .spawn(async move {
                                    sync::tick(
                                        &paths,
                                        &client,
                                        &user_id,
                                        &list_id,
                                        pacing,
                                        prune_limit,
                                        now,
                                    )
                                })
                                .await
                        };
                        forced = false;
                        let outcome = match outcome {
                            Ok(outcome) => Some(outcome),
                            Err(error) => {
                                // `log::redact` runs on the way out — an
                                // API error can quote a request URL.
                                log::error(&format!("list sync failed: {error:#}"));
                                None
                            }
                        };
                        let settled = sync::settle(outcome.as_ref(), now, interval);
                        blocked_until = settled.blocked_until;
                        let status = status_after(outcome.as_ref(), settled.wake_at, owed);
                        owed = owed_by(&status);

                        let notice = outcome.as_ref().and_then(sync::notice);
                        let _ = this.update(cx, |this, cx| {
                            this.apply_tick(status, notice, cx);
                        });

                        // A manual run against a window whose timer is off
                        // stops once there is nothing left to do —
                        // `is_finished` insists on idle *with nothing
                        // owed*, so a catch-up paused by a rate limit
                        // keeps waiting rather than walking away from a
                        // plan a paid diff produced.
                        if !scheduled && sync::is_finished(outcome.as_ref()) {
                            let _ =
                                this.update(cx, |this, cx| this.show_sync(SyncStatus::Ready, cx));
                            return;
                        }
                        settled.wake_at
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

    /// Put `status` on screen (#174).
    ///
    /// A one-liner extracted from the loop, which reached for it four
    /// times and was over `clippy::too_many_lines` by roughly the
    /// difference. Worth naming anyway: every write to `sync_status` has
    /// to be followed by a `notify`, and a loop that writes it from four
    /// places is four chances to forget.
    fn show_sync(&mut self, status: SyncStatus, cx: &mut Context<'_, Self>) {
        self.sync_status = status;
        cx.notify();
    }

    /// What one finished tick leaves on screen (#174) — its status, and
    /// whatever [`sync::notice`] decided was worth saying out loud.
    fn apply_tick(
        &mut self,
        status: SyncStatus,
        notice: Option<String>,
        cx: &mut Context<'_, Self>,
    ) {
        // Only into an empty slot: the reload banner is the reader's, and
        // a cooldown countdown they are watching must not be replaced by
        // a background task's news.
        if let Some(text) = notice
            && self.reload_notice.is_none()
        {
            self.reload_notice = Some(ReloadNotice::Outcome(text.into()));
        }
        self.show_sync(status, cx);
    }

    /// The status bar's sync segment (#174): what the sync is doing, and
    /// the way to start one.
    ///
    /// One element for both jobs rather than a label beside a button. The
    /// states where a sync can be started are exactly the states worth
    /// pressing from, so a separate button would spend most of its life
    /// greyed out next to a line that already says why — and the status
    /// bar is a 24px strip with two counts in it already.
    ///
    /// While [`Self::pending_sync`] is set this becomes the confirmation
    /// instead: what it costs, then "Sync" and "Cancel". #72's delete row
    /// is the same shape, and for the same reason — no single click may
    /// spend this much.
    pub(super) fn sync_segment(&self, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = self.theme;

        if self.pending_sync {
            return div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_color(rgb(theme.text_muted))
                        .child(sync_confirm_label()),
                )
                .child(
                    div()
                        .id("sync-confirm")
                        .text_color(rgb(theme.danger))
                        .child("Sync")
                        .on_click(cx.listener(|this, _event, _window, cx| this.confirm_sync(cx))),
                )
                .child(
                    div()
                        .id("sync-cancel")
                        .text_color(rgb(theme.text_muted))
                        .child("Cancel")
                        .on_click(cx.listener(|this, _event, _window, cx| this.cancel_sync(cx))),
                )
                .into_any_element();
        }

        let label = sync_status_label(&self.sync_status, oauth::unix_now());
        let color = sync_status_color(&self.sync_status, theme);
        let clickable = offers_sync(&self.sync_status);

        let segment = div().text_color(rgb(color)).child(label);
        if clickable {
            segment
                .id("sync-now")
                .on_click(cx.listener(|this, _event, _window, cx| this.ask_to_sync(cx)))
                .into_any_element()
        } else {
            segment.into_any_element()
        }
    }

    /// First click on the status bar's sync segment (#174): ask before
    /// spending.
    ///
    /// A two-step like #72's delete, and for a comparable reason — not
    /// that a sync is irreversible, but that it is the most expensive
    /// thing in this window to press by an order of magnitude, and
    /// `x-api-budget` says a click that fans out into requests has to put
    /// its worst case on screen before it is taken.
    ///
    /// Refuses while a tick is in flight. That is not politeness: the tick
    /// is synchronous on a background thread and outlives the task slot
    /// being reassigned, so a second start during a running diff would pay
    /// for both sides twice.
    pub(super) fn ask_to_sync(&mut self, cx: &mut Context<'_, Self>) {
        if !offers_sync(&self.sync_status) {
            return;
        }
        self.pending_sync = true;
        cx.notify();
    }

    /// Take back the ask (#174).
    pub(super) fn cancel_sync(&mut self, cx: &mut Context<'_, Self>) {
        self.pending_sync = false;
        cx.notify();
    }

    /// Second click: start the run (#174).
    ///
    /// Re-checks the status rather than trusting [`Self::ask_to_sync`]'s
    /// check — a scheduled tick can start in the gap between the two
    /// clicks, and that gap is however long someone takes to read the
    /// confirmation.
    pub(super) fn confirm_sync(&mut self, cx: &mut Context<'_, Self>) {
        self.pending_sync = false;
        if !offers_sync(&self.sync_status) {
            cx.notify();
            return;
        }
        self.start_sync(SyncTrigger::Manual, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stopped_sync_says_which_gate_it_is_stopped_at() {
        // The whole reason `SyncOff` carries a variant rather than being a
        // bare bool: these two need opposite things done about them.
        assert_ne!(
            sync_status_label(&SyncStatus::Off(SyncOff::NoList), 0),
            sync_status_label(&SyncStatus::Off(SyncOff::MissingScope), 0)
        );
    }

    #[test]
    fn a_missing_scope_points_at_the_button_that_fixes_it() {
        let label = sync_status_label(&SyncStatus::Off(SyncOff::MissingScope), 0);
        assert!(label.contains("authorize"), "{label}");
    }

    // The distinction #174 was filed about: "idle" for six hours and
    // "idle" with eleven hundred writes still owed are not the same
    // situation, and the word alone cannot tell them apart.
    #[test]
    fn an_idle_sync_with_work_left_says_how_much() {
        assert_eq!(
            sync_status_label(
                &SyncStatus::Idle {
                    until: 0,
                    pending: 1_100
                },
                0
            ),
            "List sync: 1100 to go"
        );
    }

    #[test]
    fn an_idle_sync_with_nothing_left_says_so_instead_of_a_zero() {
        let label = sync_status_label(
            &SyncStatus::Idle {
                until: 0,
                pending: 0,
            },
            0,
        );
        assert!(label.contains("up to date"), "{label}");
        assert!(
            !label.contains('0'),
            "a bare zero reads as a broken count: {label}"
        );
    }

    #[test]
    fn a_rate_limited_sync_counts_down_and_still_says_what_is_owed() {
        assert_eq!(
            sync_status_label(
                &SyncStatus::RateLimited {
                    until: 1_060,
                    pending: 40
                },
                1_000
            ),
            "List sync: rate limited, 40 to go — 60s"
        );
    }

    #[test]
    fn a_rate_limit_countdown_clamps_a_deadline_already_passed() {
        let label = sync_status_label(
            &SyncStatus::RateLimited {
                until: 900,
                pending: 40,
            },
            1_000,
        );
        assert!(label.ends_with("0s"), "never a negative countdown: {label}");
    }

    #[test]
    fn every_label_identifies_which_number_it_is() {
        // The status bar already carries two unlabelled counts. A third
        // that did not name itself would be unreadable beside them.
        for status in [
            SyncStatus::Off(SyncOff::NoList),
            SyncStatus::Off(SyncOff::MissingScope),
            SyncStatus::Off(SyncOff::NotSignedIn),
            SyncStatus::Ready,
            SyncStatus::AwaitingAccount,
            SyncStatus::Working,
            SyncStatus::Idle {
                until: 0,
                pending: 0,
            },
            SyncStatus::Idle {
                until: 0,
                pending: 7,
            },
            SyncStatus::RateLimited {
                until: 0,
                pending: 7,
            },
            SyncStatus::Failed,
        ] {
            let label = sync_status_label(&status, 0);
            assert!(label.starts_with("List sync:"), "{label}");
        }
    }

    // The double-charge guard. A diff reads both sides in full, so a
    // second one started on top of a running tick is the most expensive
    // mistake this window can make.
    #[test]
    fn a_sync_already_working_is_not_offered_again() {
        assert!(!offers_sync(&SyncStatus::Working));
    }

    #[test]
    fn a_sync_stopped_at_a_gate_is_not_offered() {
        assert!(!offers_sync(&SyncStatus::Off(SyncOff::NoList)));
        assert!(!offers_sync(&SyncStatus::Off(SyncOff::MissingScope)));
        assert!(!offers_sync(&SyncStatus::Off(SyncOff::NotSignedIn)));
    }

    #[test]
    fn a_sync_waiting_on_the_signed_in_id_is_not_offered() {
        assert!(!offers_sync(&SyncStatus::AwaitingAccount));
    }

    #[test]
    fn an_idle_or_ready_sync_is_offered() {
        assert!(offers_sync(&SyncStatus::Ready));
        assert!(offers_sync(&SyncStatus::Idle {
            until: 0,
            pending: 0
        }));
    }

    // Restarting into a window the loop is already waiting out costs
    // nothing, and asking for it is a reasonable thing to want to do.
    #[test]
    fn a_rate_limited_sync_is_still_offered() {
        assert!(offers_sync(&SyncStatus::RateLimited {
            until: 0,
            pending: 7
        }));
    }

    #[test]
    fn a_failed_sync_is_offered_so_it_can_be_retried() {
        assert!(offers_sync(&SyncStatus::Failed));
    }

    #[test]
    fn a_failed_tick_stops_the_window_reporting_the_last_success() {
        assert_eq!(status_after(None, 9_000, 0), SyncStatus::Failed);
    }

    // Both come straight back to work (`settle` sets `wake_at` to now), so
    // an idle status here would be overwritten in the same second.
    #[test]
    fn a_diff_leaves_the_status_working_because_the_drain_is_next() {
        assert_eq!(
            status_after(
                Some(&sync::Outcome::Diffed {
                    adds: 3,
                    removals: 1,
                    members_total: 100,
                    held: false,
                }),
                1_000,
                0
            ),
            SyncStatus::Working
        );
    }

    #[test]
    fn a_batch_with_more_to_send_reports_what_is_left() {
        assert_eq!(
            status_after(
                Some(&sync::Outcome::Applied {
                    sent: 20,
                    remaining: 340
                }),
                1_000,
                0
            ),
            SyncStatus::Idle {
                until: 1_000,
                pending: 340
            }
        );
    }

    // The one moment "working" would be wrong: a catch-up stopping.
    #[test]
    fn the_last_batch_of_a_catch_up_reports_it_is_done() {
        assert_eq!(
            status_after(
                Some(&sync::Outcome::Applied {
                    sent: 20,
                    remaining: 0
                }),
                1_000,
                0
            ),
            SyncStatus::Idle {
                until: 1_000,
                pending: 0
            }
        );
    }

    #[test]
    fn an_idle_tick_carries_its_pending_count_into_the_status() {
        assert_eq!(
            status_after(
                Some(&sync::Outcome::Idle {
                    until: 9_000,
                    pending: 12
                }),
                9_000,
                0
            ),
            SyncStatus::Idle {
                until: 9_000,
                pending: 12
            }
        );
    }

    #[test]
    fn the_confirmation_says_what_the_click_will_be_billed_for() {
        let label = sync_confirm_label();
        assert!(label.contains("per account"), "{label}");
    }
}
