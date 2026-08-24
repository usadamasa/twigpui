//! The background sync's memory: everything it knows about *when* it may
//! spend, in one file, moved only by one pure function (#197, #198).
//!
//! # Why one struct
//!
//! Before this, the sync's pacing lived in four places: the last diff time
//! here, the rate-limit deadline in a loop variable in `ui::list_sync`, the
//! outstanding count in another, and the 15-minute window in
//! `rate_limit.json`. The hand-offs between them lost information. #198 is
//! the plainest case: the loop wakes every minute to re-decide, and the
//! tick that found nothing to do — because it was told to wait — returned
//! `Idle`, and settling `Idle` cleared the deadline it had just waited on.
//! Two minutes after every refusal the loop sent again. Relaunching the
//! app cleared it too: the release build was started eight times in the
//! twenty hours #197 covers, and each launch sent into the same cap
//! straight away.
//!
//! So the deadline is a field of the state on disk, and [`settle`] is the
//! only thing that changes any field. What it does not know about — an
//! `Idle` tick — it leaves alone.
//!
//! # Backing away from a cap that will not say when it lifts
//!
//! #193 measured `POST /2/lists/:id/members` refusing with `remaining`
//! 299 of 300: a limit the `x-rate-limit-*` headers do not describe. The
//! 900-second wait it introduced was a guess at one refusal; #197 then
//! saw the same refusal repeat for over twenty hours, and a fixed wait
//! meant a rejected — and, for a write, possibly billed — request every
//! fifteen minutes for all of them. [`opaque_backoff_seconds`] doubles the
//! wait on every consecutive opaque refusal and caps it at six hours: at
//! most four wasted requests a day against a cap that stays down, and at
//! most six hours before one that has lifted is noticed.
//!
//! The streak is reset by a write that lands, and by nothing else. Reads
//! keep working while the write cap is down (#197: the diff succeeded, the
//! adds did not), so a diff must not count as evidence the cap has lifted.
//!
//! # Sending slowly to begin with
//!
//! The lock in #197 followed roughly 100–140 additions in about eighteen
//! minutes — seven a minute, sent in batches of twenty a second apart.
//! [`APPLY_PAUSE_SECONDS`] with `sync_writes_per_minute` (default 2, a
//! config knob since the cap's 24-hour recovery was measured) holds the
//! sustained rate down. Whether the default is under the cap is unknown;
//! what it is for is not to be the thing that trips it. Raising the knob
//! after a clean run is the sanctioned way to probe the cap's size — the
//! ladder absorbs the answer either way.

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use super::schedule::Outcome;

/// How long the loop waits between one batch of writes and the next when
/// the plan still has more. With `sync_writes_per_minute` this is the
/// sustained write rate — see the module docs for where the figure comes
/// from. The pause is the fixed half on purpose: one knob ("writes per
/// minute") is legible in a way a batch-size-and-interval pair is not.
pub(crate) const APPLY_PAUSE_SECONDS: i64 = 60;

/// The most a single opaque refusal is backed away from: six hours. Long
/// enough that a cap which stays down for a day costs four requests, not
/// ninety-six; short enough that one which lifts is noticed the same
/// quarter-day.
pub(crate) const OPAQUE_BACKOFF_CEILING_SECONDS: i64 = 21_600;

/// The background sync's clock and pacing, as written to
/// [`crate::paths::Paths::sync_state_file`].
///
/// Every field is `#[serde(default)]`: the file on a machine that ran the
/// previous version has only `last_diff_at`, and must load rather than
/// cost a diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) struct SyncState {
    /// When the last diff was *attempted*. See [`super::auto`] for why a
    /// failed attempt still moves this.
    #[serde(default)]
    pub last_diff_at: Option<i64>,
    /// Until when nothing may be sent — a rate limit's deadline, the
    /// backoff after an opaque refusal, or the interval a failed tick
    /// earned. Persisted so a relaunch honours it (#198).
    #[serde(default)]
    pub blocked_until: Option<i64>,
    /// Consecutive opaque refusals with no write landing in between. Drives
    /// [`opaque_backoff_seconds`], and is what the status bar shows when a
    /// catch-up has been refused for hours (#197).
    #[serde(default)]
    pub refusals: u32,
}

impl SyncState {
    /// Whether `blocked_until` is still ahead of `now`.
    pub(crate) fn is_blocked(&self, now: i64) -> bool {
        self.blocked_until.is_some_and(|until| until > now)
    }
}

/// How long to wait after the `refusals`-th consecutive opaque refusal.
///
/// Doubles from `rate_limit::OPAQUE_LIMIT_BACKOFF_SECONDS` (15 minutes)
/// and stops at [`OPAQUE_BACKOFF_CEILING_SECONDS`]: 15m, 30m, 1h, 2h, 4h,
/// then 6h for every refusal after. A `refusals` of 0 is treated as the
/// first — the function answers "how long now", and there is no such
/// thing as a zeroth wait.
pub(crate) fn opaque_backoff_seconds(refusals: u32) -> i64 {
    let floor = crate::rate_limit::OPAQUE_LIMIT_BACKOFF_SECONDS;
    let doublings = refusals.saturating_sub(1);
    // Past a handful of doublings the ceiling has long since won, so the
    // shift is bounded rather than trusted to a u32 from a file.
    let factor = 1i64 << doublings.min(16);
    floor
        .saturating_mul(factor)
        .min(OPAQUE_BACKOFF_CEILING_SECONDS)
}

/// What [`settle`] leaves behind: the state to persist, and when the loop
/// should wake up next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Settled {
    pub state: SyncState,
    /// The soonest the next tick may run. The caller is free to wake
    /// earlier and re-decide — `schedule::next_step` reads `state` and
    /// will say `Wait` — but must not run a tick before this.
    pub wake_at: i64,
}

/// Move the state on from what the last tick did. The only function that
/// writes to a [`SyncState`] after it is loaded.
///
/// `outcome` is `None` when the tick failed outright. That earns a full
/// interval rather than a quick retry, because the failures that reach
/// here have already survived `rate_limit`'s own network retries — a
/// revoked scope, a list that has been deleted, a plan file that will not
/// parse — and none of them gets better by being asked again in a second.
/// The interval is recorded in `blocked_until`, so a relaunch does not
/// retry it either.
///
/// An `Idle` tick changes nothing (#198). It is the loop re-deciding on a
/// deadline this state already holds; the deadline is not its to clear.
///
/// A refusal moves `blocked_until`. An opaque one also lengthens the
/// streak and waits [`opaque_backoff_seconds`] of it; one the window
/// explains waits for the window and leaves the streak alone, because a
/// window that reopens on schedule is not evidence about the hidden cap
/// either way. A write that landed just before the refusal (`sent > 0`)
/// resets the streak first: the cap was demonstrably up a moment ago, so
/// this is refusal one, not refusal five.
///
/// A batch that sent something clears both — the cap took a write — and
/// comes back after [`APPLY_PAUSE_SECONDS`] if the plan has more. A diff
/// comes straight back to drain what it found.
pub(crate) fn settle(
    state: SyncState,
    outcome: Option<&Outcome>,
    now: i64,
    interval_seconds: u32,
) -> Settled {
    let mut next = state;
    let wake_at = match outcome {
        None => {
            let until = now.saturating_add(i64::from(interval_seconds));
            next.blocked_until = Some(until);
            until
        }
        Some(Outcome::Idle { until, .. }) => *until,
        Some(Outcome::RateLimited {
            until,
            opaque,
            sent,
            ..
        }) => {
            if *sent > 0 {
                next.refusals = 0;
            }
            let until = if *opaque {
                next.refusals = next.refusals.saturating_add(1);
                now.saturating_add(opaque_backoff_seconds(next.refusals))
            } else {
                *until
            };
            next.blocked_until = Some(until);
            until
        }
        Some(Outcome::Applied { sent, remaining }) => {
            if *sent > 0 {
                next.refusals = 0;
                next.blocked_until = None;
            }
            if *remaining > 0 {
                now.saturating_add(APPLY_PAUSE_SECONDS)
            } else {
                now
            }
        }
        Some(Outcome::Diffed { .. }) => now,
    };
    Settled {
        state: next,
        wake_at,
    }
}

/// Read the state back from `path`.
///
/// Unlike `load_plan`, a corrupt file is `Ok(default)` rather than an
/// error. The two failures are not symmetric: an unreadable *plan* would
/// send an apply back to paying for both full reads, whereas an unreadable
/// *clock* costs exactly one diff that was going to happen within the
/// interval anyway. Failing the whole loop over it would be the more
/// expensive answer, because the loop is the feature.
pub(crate) fn load_state(path: &std::path::Path) -> SyncState {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return SyncState::default();
    };
    serde_json::from_str(&contents).unwrap_or_default()
}

/// Write the state to `path`.
pub(crate) fn save_state(path: &std::path::Path, state: &SyncState) -> Result<()> {
    let json = serde_json::to_string_pretty(state).context("could not serialize the sync state")?;
    std::fs::write(path, json).with_context(|| format!("could not write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rate_limit::OPAQUE_LIMIT_BACKOFF_SECONDS;
    use crate::sync::schedule::{Situation, Step, next_step};

    const INTERVAL: u32 = 21_600;

    /// A settled sync: a diff has run, nothing is blocked, nothing has been
    /// refused. Tests override the one field they are about.
    fn calm() -> SyncState {
        SyncState {
            last_diff_at: Some(1_000),
            blocked_until: None,
            refusals: 0,
        }
    }

    fn opaque(sent: usize) -> Outcome {
        Outcome::RateLimited {
            until: 1_000 + OPAQUE_LIMIT_BACKOFF_SECONDS,
            opaque: true,
            sent,
            remaining: 2_157,
        }
    }

    // --- the ladder ---

    #[test]
    fn the_first_opaque_refusal_waits_the_floor() {
        assert_eq!(opaque_backoff_seconds(1), OPAQUE_LIMIT_BACKOFF_SECONDS);
    }

    #[test]
    fn each_further_refusal_doubles_the_wait() {
        assert_eq!(opaque_backoff_seconds(2), 1_800);
        assert_eq!(opaque_backoff_seconds(3), 3_600);
        assert_eq!(opaque_backoff_seconds(4), 7_200);
        assert_eq!(opaque_backoff_seconds(5), 14_400);
    }

    #[test]
    fn the_wait_stops_growing_at_six_hours() {
        // 900 × 2⁵ = 28,800 would be the sixth; the ceiling wins.
        assert_eq!(opaque_backoff_seconds(6), OPAQUE_BACKOFF_CEILING_SECONDS);
        assert_eq!(opaque_backoff_seconds(40), OPAQUE_BACKOFF_CEILING_SECONDS);
        assert_eq!(
            opaque_backoff_seconds(u32::MAX),
            OPAQUE_BACKOFF_CEILING_SECONDS
        );
    }

    #[test]
    fn a_streak_of_zero_is_read_as_the_first_wait() {
        assert_eq!(opaque_backoff_seconds(0), OPAQUE_LIMIT_BACKOFF_SECONDS);
    }

    // --- settle: refusals ---

    #[test]
    fn an_opaque_refusal_starts_a_streak_and_blocks_for_the_floor() {
        let settled = settle(calm(), Some(&opaque(0)), 1_000, INTERVAL);
        assert_eq!(settled.state.refusals, 1);
        assert_eq!(
            settled.state.blocked_until,
            Some(1_000 + OPAQUE_LIMIT_BACKOFF_SECONDS)
        );
        assert_eq!(settled.wake_at, 1_000 + OPAQUE_LIMIT_BACKOFF_SECONDS);
    }

    #[test]
    fn a_second_opaque_refusal_waits_twice_as_long() {
        // #197's failure: the same 429 every fifteen minutes for twenty
        // hours. The second one must not wait the same fifteen minutes.
        let state = SyncState {
            refusals: 1,
            ..calm()
        };
        let settled = settle(state, Some(&opaque(0)), 10_000, INTERVAL);
        assert_eq!(settled.state.refusals, 2);
        assert_eq!(settled.state.blocked_until, Some(10_000 + 1_800));
    }

    #[test]
    fn a_write_that_landed_before_the_refusal_restarts_the_streak() {
        // The cap took a write a moment ago, so this refusal is the first
        // of a new streak, not the sixth of the old one.
        let state = SyncState {
            refusals: 5,
            ..calm()
        };
        let settled = settle(state, Some(&opaque(3)), 10_000, INTERVAL);
        assert_eq!(settled.state.refusals, 1);
        assert_eq!(
            settled.state.blocked_until,
            Some(10_000 + OPAQUE_LIMIT_BACKOFF_SECONDS)
        );
    }

    #[test]
    fn a_refusal_the_window_explains_waits_for_the_window_and_is_not_a_streak() {
        // The 15-minute window is exhausted and says when it reopens. That
        // is not the hidden cap, so it neither lengthens the streak nor
        // waits the ladder.
        let state = SyncState {
            refusals: 3,
            ..calm()
        };
        let outcome = Outcome::RateLimited {
            until: 1_500,
            opaque: false,
            sent: 0,
            remaining: 40,
        };
        let settled = settle(state, Some(&outcome), 1_000, INTERVAL);
        assert_eq!(settled.state.refusals, 3);
        assert_eq!(settled.state.blocked_until, Some(1_500));
        assert_eq!(settled.wake_at, 1_500);
    }

    // --- settle: the streak ends only with a write ---

    #[test]
    fn a_batch_that_sent_something_ends_the_streak_and_the_block() {
        let state = SyncState {
            refusals: 4,
            blocked_until: Some(900),
            ..calm()
        };
        let outcome = Outcome::Applied {
            sent: 2,
            remaining: 100,
        };
        let settled = settle(state, Some(&outcome), 1_000, INTERVAL);
        assert_eq!(settled.state.refusals, 0);
        assert_eq!(settled.state.blocked_until, None);
    }

    #[test]
    fn a_diff_does_not_end_the_streak() {
        // Reads work while the write cap is down (#197: the diff went
        // through; the adds did not). A diff succeeding says nothing
        // about whether writes would.
        let state = SyncState {
            refusals: 4,
            ..calm()
        };
        let outcome = Outcome::Diffed {
            adds: 3,
            removals: 0,
            members_total: 100,
            held: false,
        };
        let settled = settle(state, Some(&outcome), 1_000, INTERVAL);
        assert_eq!(settled.state.refusals, 4);
        assert_eq!(settled.wake_at, 1_000);
    }

    #[test]
    fn a_batch_that_sent_nothing_changes_nothing() {
        // Cannot happen from a plan with entries, but if it does it is not
        // evidence the cap has lifted.
        let state = SyncState {
            refusals: 2,
            blocked_until: Some(900),
            ..calm()
        };
        let outcome = Outcome::Applied {
            sent: 0,
            remaining: 0,
        };
        assert_eq!(settle(state, Some(&outcome), 1_000, INTERVAL).state, state);
    }

    // --- settle: pacing ---

    #[test]
    fn a_batch_with_more_to_send_pauses_before_the_next() {
        // Two a minute, not twenty a second — see the module docs.
        let outcome = Outcome::Applied {
            sent: 2,
            remaining: 2_155,
        };
        let settled = settle(calm(), Some(&outcome), 1_000, INTERVAL);
        assert_eq!(settled.wake_at, 1_000 + APPLY_PAUSE_SECONDS);
    }

    #[test]
    fn the_batch_that_finishes_the_plan_comes_straight_back() {
        // Nothing left to pace; the next tick decides whether a diff is
        // due.
        let outcome = Outcome::Applied {
            sent: 2,
            remaining: 0,
        };
        assert_eq!(
            settle(calm(), Some(&outcome), 1_000, INTERVAL).wake_at,
            1_000
        );
    }

    #[test]
    fn a_diff_comes_straight_back_to_drain_what_it_found() {
        let outcome = Outcome::Diffed {
            adds: 3,
            removals: 1,
            members_total: 100,
            held: false,
        };
        assert_eq!(
            settle(calm(), Some(&outcome), 1_000, INTERVAL).wake_at,
            1_000
        );
    }

    // --- settle: idle and failure ---

    #[test]
    fn an_idle_tick_leaves_the_state_exactly_as_it_found_it() {
        // #198. The loop re-decides every minute; the tick that was told
        // to wait must not clear what it was told to wait for.
        let state = SyncState {
            refusals: 2,
            blocked_until: Some(5_000),
            ..calm()
        };
        let outcome = Outcome::Idle {
            until: 5_000,
            pending: 2_157,
        };
        let settled = settle(state, Some(&outcome), 1_060, INTERVAL);
        assert_eq!(settled.state, state);
        assert_eq!(settled.wake_at, 5_000);
    }

    #[test]
    fn a_failed_tick_earns_a_full_interval_and_records_it() {
        // Recorded rather than only waited: relaunching the app must not
        // retry a revoked scope or a deleted list straight away.
        let settled = settle(calm(), None, 1_000, INTERVAL);
        assert_eq!(settled.wake_at, 1_000 + i64::from(INTERVAL));
        assert_eq!(
            settled.state.blocked_until,
            Some(1_000 + i64::from(INTERVAL))
        );
        assert_eq!(settled.state.refusals, 0);
    }

    // #198 end to end: refused, woken a minute later and told to wait,
    // woken a minute after that — still waiting. This is the sequence the
    // old `settle` lost on the second step, and the one the release build
    // was observed playing out every two minutes.
    #[test]
    fn a_refusal_still_holds_two_wake_ups_later() {
        let refused_at = 1_000;
        let first = settle(calm(), Some(&opaque(0)), refused_at, INTERVAL);
        let until = first.state.blocked_until.unwrap();

        let situation = |state: SyncState| Situation {
            last_diff_at: state.last_diff_at,
            interval_seconds: INTERVAL,
            pending: 2_157,
            blocked_until: state.blocked_until,
        };
        let woke_at = refused_at + 60;
        assert_eq!(
            next_step(&situation(first.state), woke_at),
            Step::Wait { until }
        );

        let idle = Outcome::Idle {
            until,
            pending: 2_157,
        };
        let second = settle(first.state, Some(&idle), woke_at, INTERVAL);
        assert_eq!(
            next_step(&situation(second.state), woke_at + 60),
            Step::Wait { until },
            "the refusal was forgotten one wake-up after it was handed over"
        );
        assert_eq!(next_step(&situation(second.state), until), Step::Apply);
    }

    // --- the file ---

    #[test]
    fn the_state_survives_a_round_trip_through_the_file() {
        let dir = std::env::temp_dir().join(format!("twigpui-sync-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");

        let written = SyncState {
            last_diff_at: Some(1_700_000_000),
            blocked_until: Some(1_700_000_900),
            refusals: 3,
        };
        save_state(&path, &written).unwrap();
        assert_eq!(load_state(&path), written);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_file_reads_as_never_synced() {
        // Which makes the first launch diff immediately — the behavior the
        // schedule wants from a fresh install.
        let path =
            std::env::temp_dir().join(format!("twigpui-no-state-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert_eq!(load_state(&path), SyncState::default());
    }

    #[test]
    fn a_corrupt_file_reads_as_never_synced_rather_than_failing_the_loop() {
        // The opposite of `load_plan`'s rule, on purpose: a bad clock costs
        // one diff that was due within the interval anyway, whereas failing
        // the loop over it would stop the feature outright.
        let path =
            std::env::temp_dir().join(format!("twigpui-bad-state-{}.json", std::process::id()));
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(load_state(&path), SyncState::default());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_file_from_before_the_backoff_fields_still_loads() {
        // What every machine that ran the previous version has on disk.
        // Losing the clock here would cost a diff on the first launch
        // after upgrading.
        let state: SyncState = serde_json::from_str(r#"{"last_diff_at":1787470513}"#).unwrap();
        assert_eq!(state.last_diff_at, Some(1_787_470_513));
        assert_eq!(state.blocked_until, None);
        assert_eq!(state.refusals, 0);
    }

    #[test]
    fn is_blocked_reads_the_deadline_against_now() {
        let state = SyncState {
            blocked_until: Some(2_000),
            ..calm()
        };
        assert!(state.is_blocked(1_999));
        assert!(!state.is_blocked(2_000));
        assert!(!calm().is_blocked(0));
    }
}
