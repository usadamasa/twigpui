//! What one wake-up of the background sync loop should do.
//!
//! The pure half of the auto-sync added on top of #163, split from
//! [`super::auto`] on the same line `ui::reload_policy` is split from `ui`:
//! everything here is a decision about whether to spend, and none of it
//! makes a request. The loop that acts on a [`Step`] lives next door and is
//! not unit tested, because every branch of it is HTTP.

/// What the loop should do right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// Read the follow side in full — and the members side, unless the
    /// local mirror is fresh enough (#173) — and write a fresh plan. The
    /// expensive one: every account read is a billed resource.
    Diff,
    /// Send a batch of the plan's outstanding writes.
    Apply,
    /// Nothing to do until `until`. The caller sleeps — capping how long
    /// it sleeps in one go is its business, not this function's.
    Wait { until: i64 },
}

/// Everything [`next_step`] needs to know about where the sync has got to.
///
/// A struct rather than five positional arguments: `Option<i64>` twice and
/// two counts next to each other is exactly the shape a call site gets
/// wrong silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Situation {
    /// When the last diff was *attempted*, from the state file, or `None`
    /// if there has never been one. Attempted rather than succeeded — see
    /// [`super::auto`] for why a failed read still moves this.
    pub last_diff_at: Option<i64>,
    /// `config.sync_interval_seconds`.
    pub interval_seconds: u32,
    /// Entries in the plan on file the loop may still send — [`sendable`],
    /// not every unapplied entry. Removals #176 holds are left out; counted,
    /// they would pin [`next_step`] on `Apply` draining a plan it never
    /// finishes.
    pub pending: usize,
    /// When the tracked rate-limit window for a write endpoint reopens,
    /// from the [`crate::rate_limit::RateLimited`] that refused the last
    /// send, or `None` if nothing is holding the loop back.
    pub blocked_until: Option<i64>,
}

/// Decide what one wake-up should do.
///
/// The precedence is the whole design:
///
/// 1. **A live rate limit wins.** Both other steps send requests, and
///    sending into a window that has already refused is how a self-imposed
///    throttle turns into X's.
/// 2. **Draining outranks re-diffing.** The entries in a plan were paid
///    for by the diff that produced them. Re-diffing on top of an
///    undrained plan buys the same answer a second time and throws away
///    the record of what has already been sent.
/// 3. **Then the interval.** A diff that has never run is due immediately;
///    otherwise it is due `interval_seconds` after the last attempt.
///
/// `last_diff_at` in the future is treated as due now rather than waited
/// out. It comes from a file stamped by a clock this code does not own, so
/// a backwards jump (or a hand-edited state file) would otherwise stall
/// the loop until the clock caught up — for a value far enough ahead,
/// forever.
pub(crate) fn next_step(situation: &Situation, now: i64) -> Step {
    if let Some(until) = situation.blocked_until
        && until > now
    {
        return Step::Wait { until };
    }
    if situation.pending > 0 {
        return Step::Apply;
    }
    let Some(last) = situation.last_diff_at else {
        return Step::Diff;
    };
    if last > now {
        return Step::Diff;
    }
    let due_at = last.saturating_add(i64::from(situation.interval_seconds));
    if due_at > now {
        Step::Wait { until: due_at }
    } else {
        Step::Diff
    }
}

/// What one tick did, for the caller to log, show, and pace itself by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Nothing was due. Nothing was sent.
    ///
    /// `pending` is what the plan on file still owes, and it is not
    /// always zero: [`next_step`] checks `blocked_until` *before* it
    /// checks `pending`, so a live rate limit part way through a
    /// catch-up produces an idle tick sitting on hundreds of
    /// outstanding writes. #174's manual sync uses exactly that
    /// distinction to decide when it is finished — "idle" alone would
    /// have it walk away from a plan a full diff was paid for.
    Idle { until: i64, pending: usize },
    /// Both sides were read and a fresh plan written.
    ///
    /// `held` is #176's verdict on the removals: `true` means they are
    /// more of the list's `members_total` than `sync_prune_limit_percent`
    /// allows, so the background sync will drain the additions and leave
    /// the removals in the plan file for `--sync-list --apply --prune`.
    Diffed {
        adds: usize,
        removals: usize,
        members_total: usize,
        held: bool,
    },
    /// A batch of the plan's writes went out.
    Applied { sent: usize, remaining: usize },
    /// A write was refused by the tracked rate-limit window before it was
    /// sent. Nothing was spent, and the plan on disk still records exactly
    /// where the catch-up got to.
    RateLimited { until: i64 },
}

/// When the loop should wake up next, and what it should carry forward
/// about the rate limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Settled {
    /// The soonest the next tick may run. The caller is free to wake
    /// earlier and re-decide; it must not run a tick before this.
    pub wake_at: i64,
    /// What to pass back as [`Situation::blocked_until`].
    pub blocked_until: Option<i64>,
}

/// Pace the loop from what the last tick did.
///
/// `outcome` is `None` when the tick failed outright. That earns a full
/// interval rather than a quick retry, because the failures that reach here
/// have already survived `rate_limit`'s own network retries — a revoked
/// scope, a list that has been deleted, a plan file that will not parse —
/// and none of them gets better by being asked again in a second.
///
/// Everything that did some work comes straight back (`wake_at` is `now`).
/// A diff has just written a plan that wants draining, and an apply batch
/// is one of many. The caller's own floor is what keeps that from being a
/// spin; this function's job is only to say there is more to do.
pub(crate) fn settle(outcome: Option<&Outcome>, now: i64, interval_seconds: u32) -> Settled {
    match outcome {
        None => Settled {
            wake_at: now.saturating_add(i64::from(interval_seconds)),
            blocked_until: None,
        },
        Some(Outcome::Idle { until, .. }) => Settled {
            wake_at: *until,
            blocked_until: None,
        },
        Some(Outcome::RateLimited { until }) => Settled {
            wake_at: *until,
            blocked_until: Some(*until),
        },
        Some(Outcome::Diffed { .. } | Outcome::Applied { .. }) => Settled {
            wake_at: now,
            blocked_until: None,
        },
    }
}

/// What the window should say about `outcome`, or `None` to say nothing.
///
/// Silent by design in the two cases that happen most: a diff that found
/// nothing (the steady state, several times a day) and a batch part way
/// through a catch-up (one every few seconds for hours). Announcing either
/// would turn a background feature into a stream of notifications about
/// itself.
///
/// A rate limit is logged rather than shown for a third reason: the loop
/// re-reaches it on every tick until the window reopens, so a banner would
/// come back however many times it was dismissed.
pub(crate) fn notice(outcome: &Outcome) -> Option<String> {
    match outcome {
        // Shown once per diff, not once per apply tick: the loop
        // re-reaches a held plan on every batch of additions it drains,
        // and a banner that came back with each would be the rate-limit
        // problem below all over again.
        Outcome::Diffed {
            adds,
            removals,
            members_total,
            held: true,
        } => Some(format!(
            "List sync: {adds} to add; {removals} of {members_total} members would be removed, \
             which is over the background limit, so they are held. Run \
             `twigpui --sync-list --apply --prune` to confirm them."
        )),
        Outcome::Diffed {
            adds,
            removals,
            held: false,
            ..
        } if *adds > 0 || *removals > 0 => {
            Some(format!("List sync: {adds} to add, {removals} to remove."))
        }
        Outcome::Applied { sent, remaining: 0 } if *sent > 0 => {
            Some(format!("List sync: {sent} change(s) applied."))
        }
        Outcome::Diffed { .. }
        | Outcome::Applied { .. }
        | Outcome::Idle { .. }
        | Outcome::RateLimited { .. } => None,
    }
}

/// What [`Situation::last_diff_at`] should be for this tick, given
/// whether the caller forced it (#174).
///
/// The whole of the manual trigger's mechanism, and a named function
/// rather than an `if` inside [`super::auto::tick`] because that function
/// makes HTTP requests and so has no test of its own — this had none
/// either until it moved out here, which for a switch that decides
/// whether both sides get re-read is not a good place to leave it.
///
/// Forcing discards the recorded time rather than shortening the interval.
/// `None` is what [`next_step`] already reads as "a diff has never run",
/// so the decision below it is untouched: a live rate limit still refuses
/// the tick, and an undrained plan is still drained before anything is
/// bought again.
pub(crate) fn last_diff_for(forced: bool, recorded: Option<i64>) -> Option<i64> {
    if forced { None } else { recorded }
}

/// Whether a run that was asked for one pass has nothing left to do
/// (#174).
///
/// Only for a loop that is meant to stop — the scheduled sync never does,
/// and asks this nothing. What it is protecting is the plan file: a diff
/// against a few thousand follows costs dollars, so a manual run that
/// walks away leaving entries unsent has thrown that money at a list it
/// did not finish rewriting.
///
/// So "idle" is not the test; **idle with nothing outstanding** is. The
/// two come apart exactly where it matters: [`next_step`] checks
/// `blocked_until` before `pending`, so a rate limit hit half way through
/// a catch-up produces `Idle` with hundreds of writes still owed. That run
/// has to keep waiting, not declare itself done.
///
/// A failed tick (`None`) does not end the run either — [`settle`] has
/// already given it a full interval to come back on, and the plan it was
/// draining is still on disk.
pub(crate) fn is_finished(outcome: Option<&Outcome>) -> bool {
    matches!(outcome, Some(Outcome::Idle { pending: 0, .. }))
}

/// The next `limit` entries the loop should send, taken alternately from
/// the additions and the removals.
///
/// Alternately, rather than every addition and then every removal, because
/// a list that is badly out of date is caught up over hours: sending the
/// adds first would show every new account long before the first stale one
/// went away, and a run interrupted half way would leave the list strictly
/// larger than it should be rather than closer to right.
///
/// `prune` false drops removals entirely — that is the CLI's default, and
/// this function is the one place the two paths differ.
///
/// Returns owned ids rather than borrowing `plan`, because the caller
/// marks entries applied as it goes.
pub(crate) fn next_batch(
    plan: &super::Plan,
    prune: bool,
    limit: usize,
) -> Vec<(super::Action, String)> {
    let mut adds = plan.pending(super::Action::Add);
    let mut removals = plan.pending(super::Action::Remove);
    let mut batch = Vec::new();
    while batch.len() < limit {
        let add = adds.next();
        let removal = if prune { removals.next() } else { None };
        if add.is_none() && removal.is_none() {
            break;
        }
        if let Some(entry) = add {
            batch.push((super::Action::Add, entry.user_id.clone()));
        }
        if batch.len() >= limit {
            break;
        }
        if let Some(entry) = removal {
            batch.push((super::Action::Remove, entry.user_id.clone()));
        }
    }
    batch
}

/// Whether the background sync may send `plan`'s removals (#176).
///
/// The cap is a share of the list: removals go out when they would delete
/// at most `limit_percent` of the `members_total` the plan was diffed
/// against. It is for the failure `read_all`'s all-or-nothing rule cannot
/// see — a follow read that comes back short *with a 200* (an outage, a
/// scope quietly dropped, a regression upstream of `plan`) reads as a mass
/// unfollow, and with pruning unconditional that is a mass deletion.
///
/// Over the cap, *every* removal is held rather than the first N sent: the
/// suspicion is about the read, and a bad read's first N are no better
/// than its last. Held removals stay in the plan file — they were paid
/// for — where `--sync-list --apply --prune` sends them under a person's
/// eye. The CLI has no cap: its dry-run report shows the same numbers, and
/// typing `--prune` after reading them is the confirmation.
///
/// Measured on what is still pending, not on the plan as diffed, so a plan
/// the CLI has already pruned most of is not still held for the part that
/// landed. A `members_total` of 0 with removals pending holds them: that is
/// a plan file from before this cap (`#[serde(default)]`), and the next
/// diff replaces it.
///
/// `limit_percent` is `config.sync_prune_limit_percent`, 0..=100. 100
/// allows emptying the list; 0 makes the background sync additive only.
pub(crate) fn prune_allowed(plan: &super::Plan, limit_percent: u8) -> bool {
    let removals = plan.pending_count(super::Action::Remove);
    if removals == 0 {
        return true;
    }
    // Cross-multiplied rather than divided, so 1 of 15 at 10% (1.5
    // allowed) is over the line rather than rounded onto it.
    removals.saturating_mul(100)
        <= plan
            .members_total
            .saturating_mul(usize::from(limit_percent))
}

/// How many of `plan`'s entries the background sync may still send —
/// [`Situation::pending`]'s value (#176).
///
/// Additions always count. Removals count only while `prune` (the
/// [`prune_allowed`] verdict) says they may go; held ones are not
/// outstanding work for the loop, because the loop will never do them.
pub(crate) fn sendable(plan: &super::Plan, prune: bool) -> usize {
    let adds = plan.pending_count(super::Action::Add);
    if prune {
        adds.saturating_add(plan.pending_count(super::Action::Remove))
    } else {
        adds
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::{Action, Plan, PlanEntry};

    fn plan_of(adds: &[&str], removals: &[&str]) -> Plan {
        let entry = |user_id: &str, action| PlanEntry {
            user_id: user_id.to_string(),
            username: format!("user{user_id}"),
            action,
            applied: false,
        };
        Plan {
            list_id: "7".to_string(),
            created_at: 0,
            members_total: 0,
            members_source: super::super::MembersSource::Read,
            entries: adds
                .iter()
                .map(|id| entry(id, Action::Add))
                .chain(removals.iter().map(|id| entry(id, Action::Remove)))
                .collect(),
        }
    }

    /// The batch as a compact `+id` / `-id` string, so the interleaving is
    /// readable in the assertion rather than buried in a tuple vec.
    fn batch(plan: &Plan, prune: bool, limit: usize) -> String {
        next_batch(plan, prune, limit)
            .iter()
            .map(|(action, user_id)| match action {
                Action::Add => format!("+{user_id}"),
                Action::Remove => format!("-{user_id}"),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// A settled loop: a diff has run, nothing is outstanding, nothing is
    /// blocked. Tests override the one field they are about.
    fn idle() -> Situation {
        Situation {
            last_diff_at: Some(1_000),
            interval_seconds: 21_600,
            pending: 0,
            blocked_until: None,
        }
    }

    #[test]
    fn a_sync_that_has_never_run_diffs_immediately() {
        let situation = Situation {
            last_diff_at: None,
            ..idle()
        };
        assert_eq!(next_step(&situation, 0), Step::Diff);
    }

    #[test]
    fn nothing_happens_again_until_the_interval_has_elapsed() {
        // The persisted `last_diff_at` is what stops a relaunch from
        // paying for both full reads over again.
        assert_eq!(next_step(&idle(), 1_001), Step::Wait { until: 22_600 });
    }

    #[test]
    fn the_diff_comes_due_exactly_one_interval_after_the_last_one() {
        assert_eq!(next_step(&idle(), 22_600), Step::Diff);
    }

    #[test]
    fn a_diff_that_is_long_overdue_is_still_just_one_diff() {
        // A machine asleep for a week wakes up owing one sync, not a week
        // of them.
        assert_eq!(next_step(&idle(), 1_000_000), Step::Diff);
    }

    #[test]
    fn outstanding_entries_are_drained_before_the_next_diff() {
        // The entries were paid for by the diff that found them. Re-diffing
        // on top of them buys the same answer twice.
        let situation = Situation {
            pending: 3,
            ..idle()
        };
        assert_eq!(next_step(&situation, 1_000_000), Step::Apply);
    }

    #[test]
    fn draining_happens_inside_the_interval_too() {
        // The apply drip is the point of the whole thing: a list 2,000
        // accounts behind is caught up a batch at a time, not held until
        // the next diff is due.
        let situation = Situation {
            pending: 2_000,
            ..idle()
        };
        assert_eq!(next_step(&situation, 1_001), Step::Apply);
    }

    #[test]
    fn a_live_rate_limit_outranks_the_work_waiting_behind_it() {
        // Both other steps send. Sending into a window that has already
        // refused is how the self-imposed throttle becomes X's.
        let situation = Situation {
            pending: 5,
            blocked_until: Some(2_000),
            ..idle()
        };
        assert_eq!(next_step(&situation, 1_500), Step::Wait { until: 2_000 });
    }

    #[test]
    fn a_rate_limit_outranks_an_overdue_diff_as_well() {
        let situation = Situation {
            blocked_until: Some(2_000),
            ..idle()
        };
        assert_eq!(next_step(&situation, 1_500), Step::Wait { until: 2_000 });
    }

    #[test]
    fn an_elapsed_rate_limit_holds_nothing_back() {
        let situation = Situation {
            pending: 5,
            blocked_until: Some(2_000),
            ..idle()
        };
        assert_eq!(next_step(&situation, 2_000), Step::Apply);
    }

    #[test]
    fn a_last_diff_stamped_in_the_future_is_treated_as_due_now() {
        // The stamp comes from a file written by a clock this code does not
        // own. Waiting it out would stall the loop until the clock caught
        // up — for a value far enough ahead, forever.
        let situation = Situation {
            last_diff_at: Some(i64::MAX),
            ..idle()
        };
        assert_eq!(next_step(&situation, 1_000), Step::Diff);
    }

    // --- settle ---

    #[test]
    fn a_failed_tick_earns_a_full_interval_rather_than_a_retry() {
        // Everything that reaches here has already survived `rate_limit`'s
        // own network retries.
        let settled = settle(None, 1_000, 21_600);
        assert_eq!(settled.wake_at, 22_600);
        assert_eq!(settled.blocked_until, None);
    }

    #[test]
    fn a_rate_limit_is_both_the_deadline_and_the_thing_carried_forward() {
        // Carried forward is what makes the *next* tick refuse to send
        // before the window reopens, instead of finding out again the
        // expensive way.
        let settled = settle(Some(&Outcome::RateLimited { until: 5_000 }), 1_000, 21_600);
        assert_eq!(settled.wake_at, 5_000);
        assert_eq!(settled.blocked_until, Some(5_000));
    }

    #[test]
    fn an_idle_tick_waits_out_the_deadline_it_was_handed() {
        let settled = settle(
            Some(&Outcome::Idle {
                until: 9_000,
                pending: 0,
            }),
            1_000,
            21_600,
        );
        assert_eq!(settled.wake_at, 9_000);
        assert_eq!(settled.blocked_until, None);
    }

    // --- #174: forcing a tick past the interval ---

    #[test]
    fn an_ordinary_tick_is_paced_by_the_recorded_diff_time() {
        assert_eq!(last_diff_for(false, Some(1_000)), Some(1_000));
    }

    #[test]
    fn a_forced_tick_discards_the_recorded_diff_time() {
        assert_eq!(last_diff_for(true, Some(1_000)), None);
    }

    // The end-to-end shape of the button: a diff four hours from due
    // becomes due now, and nothing else about the decision moves.
    #[test]
    fn forcing_turns_a_tick_that_would_have_waited_into_a_diff() {
        let recorded = Some(1_000);
        let waiting = Situation {
            last_diff_at: last_diff_for(false, recorded),
            interval_seconds: 21_600,
            ..idle()
        };
        assert_eq!(
            next_step(&waiting, 1_100),
            Step::Wait { until: 22_600 },
            "unforced, this is hours away"
        );

        let forced = Situation {
            last_diff_at: last_diff_for(true, recorded),
            ..waiting
        };
        assert_eq!(next_step(&forced, 1_100), Step::Diff);
    }

    // Forcing drops the interval and *only* the interval. Both of the
    // checks above it in `next_step` still hold, which is what keeps the
    // button from being a way to spend around them.
    #[test]
    fn forcing_does_not_get_past_a_live_rate_limit() {
        let situation = Situation {
            last_diff_at: last_diff_for(true, Some(1_000)),
            blocked_until: Some(5_000),
            ..idle()
        };
        assert_eq!(next_step(&situation, 1_100), Step::Wait { until: 5_000 });
    }

    #[test]
    fn forcing_drains_an_outstanding_plan_before_buying_a_new_one() {
        let situation = Situation {
            last_diff_at: last_diff_for(true, Some(1_000)),
            pending: 340,
            ..idle()
        };
        assert_eq!(next_step(&situation, 1_100), Step::Apply);
    }

    // --- #174: when a one-pass run may stop ---

    #[test]
    fn a_run_is_finished_once_it_goes_idle_with_nothing_owed() {
        assert!(is_finished(Some(&Outcome::Idle {
            until: 9_000,
            pending: 0
        })));
    }

    // The case the whole function exists for. `next_step` weighs
    // `blocked_until` ahead of `pending`, so a rate limit part way through
    // a catch-up looks idle — and stopping there abandons a plan a full
    // paid diff produced.
    #[test]
    fn a_run_blocked_part_way_through_a_catch_up_is_not_finished() {
        assert!(!is_finished(Some(&Outcome::Idle {
            until: 9_000,
            pending: 340
        })));
    }

    #[test]
    fn a_run_that_just_sent_a_batch_is_not_finished() {
        assert!(!is_finished(Some(&Outcome::Applied {
            sent: 20,
            remaining: 0
        })));
    }

    #[test]
    fn a_run_that_was_refused_by_the_rate_limit_is_not_finished() {
        assert!(!is_finished(Some(&Outcome::RateLimited { until: 9_000 })));
    }

    // `settle` has already handed a failed tick a full interval to come
    // back on, and whatever it was draining is still on disk.
    #[test]
    fn a_run_whose_tick_failed_is_not_finished() {
        assert!(!is_finished(None));
    }

    #[test]
    fn a_diff_comes_straight_back_to_drain_what_it_found() {
        let settled = settle(
            Some(&Outcome::Diffed {
                adds: 3,
                removals: 1,
                members_total: 100,
                held: false,
            }),
            1_000,
            21_600,
        );
        assert_eq!(settled.wake_at, 1_000);
    }

    #[test]
    fn an_applied_batch_comes_straight_back_for_the_next_one() {
        let settled = settle(
            Some(&Outcome::Applied {
                sent: 20,
                remaining: 400,
            }),
            1_000,
            21_600,
        );
        assert_eq!(settled.wake_at, 1_000);
    }

    #[test]
    fn a_tick_that_did_work_clears_the_remembered_rate_limit() {
        // It just sent something, so whatever window was closed is open.
        let settled = settle(
            Some(&Outcome::Applied {
                sent: 1,
                remaining: 0,
            }),
            1_000,
            21_600,
        );
        assert_eq!(settled.blocked_until, None);
    }

    // --- notice ---

    #[test]
    fn a_diff_that_found_nothing_says_nothing() {
        // The steady state, several times a day.
        assert_eq!(
            notice(&Outcome::Diffed {
                adds: 0,
                removals: 0,
                members_total: 100,
                held: false,
            }),
            None
        );
    }

    #[test]
    fn a_diff_that_found_work_says_what_it_found() {
        let text = notice(&Outcome::Diffed {
            adds: 3,
            removals: 1,
            members_total: 100,
            held: false,
        })
        .unwrap();
        assert!(text.contains("3 to add"), "{text}");
        assert!(text.contains("1 to remove"), "{text}");
    }

    #[test]
    fn a_batch_part_way_through_a_catch_up_says_nothing() {
        // One every few seconds for hours, if it did.
        assert_eq!(
            notice(&Outcome::Applied {
                sent: 20,
                remaining: 400
            }),
            None
        );
    }

    #[test]
    fn the_batch_that_finishes_the_catch_up_reports_it() {
        let text = notice(&Outcome::Applied {
            sent: 12,
            remaining: 0,
        })
        .unwrap();
        assert!(text.contains("12"), "{text}");
    }

    #[test]
    fn a_final_batch_that_sent_nothing_still_says_nothing() {
        assert_eq!(
            notice(&Outcome::Applied {
                sent: 0,
                remaining: 0
            }),
            None
        );
    }

    #[test]
    fn neither_idling_nor_a_rate_limit_reaches_the_banner() {
        // The rate limit especially: the loop re-reaches it on every tick
        // until the window reopens, so a banner would come back however
        // many times it was dismissed.
        assert_eq!(
            notice(&Outcome::Idle {
                until: 9_000,
                pending: 0
            }),
            None
        );
        assert_eq!(notice(&Outcome::RateLimited { until: 9_000 }), None);
    }

    // --- next_batch ---

    #[test]
    fn a_batch_alternates_additions_and_removals() {
        // A list caught up over hours should get closer to right the whole
        // way, not grow to its final size first and shed the stale members
        // afterwards.
        let plan = plan_of(&["1", "2"], &["3", "4"]);
        assert_eq!(batch(&plan, true, 10), "+1 -3 +2 -4");
    }

    #[test]
    fn a_batch_stops_at_the_limit_mid_pair() {
        // The odd limit is the case where the interleaving could quietly
        // send one extra request.
        let plan = plan_of(&["1", "2"], &["3", "4"]);
        assert_eq!(batch(&plan, true, 3), "+1 -3 +2");
    }

    #[test]
    fn a_batch_carries_on_with_whichever_side_still_has_entries() {
        let plan = plan_of(&["1", "2", "3"], &["9"]);
        assert_eq!(batch(&plan, true, 10), "+1 -9 +2 +3");
    }

    #[test]
    fn removals_alone_still_fill_a_batch() {
        let plan = plan_of(&[], &["7", "8"]);
        assert_eq!(batch(&plan, true, 10), "-7 -8");
    }

    #[test]
    fn without_prune_a_batch_is_additions_only() {
        // The CLI's default. Removals stay listed in the plan and unsent.
        let plan = plan_of(&["1", "2"], &["3", "4"]);
        assert_eq!(batch(&plan, false, 10), "+1 +2");
    }

    #[test]
    fn without_prune_removals_do_not_eat_into_the_limit() {
        // The bug the interleaving invites: counting a skipped removal as
        // one of the `limit` sends, so a bounded batch sends half as many
        // additions as it was asked for.
        let plan = plan_of(&["1", "2"], &["3", "4"]);
        assert_eq!(batch(&plan, false, 2), "+1 +2");
    }

    #[test]
    fn an_already_applied_entry_is_never_in_a_batch() {
        // What makes a resumed apply cheap: the plan file remembers what
        // went through, and re-sending it would spend a write to change
        // nothing.
        let mut plan = plan_of(&["1", "2"], &["3"]);
        plan.mark_applied("1", Action::Add);
        assert_eq!(batch(&plan, true, 10), "+2 -3");
    }

    #[test]
    fn a_fully_applied_plan_yields_an_empty_batch() {
        let mut plan = plan_of(&["1"], &["3"]);
        plan.mark_applied("1", Action::Add);
        plan.mark_applied("3", Action::Remove);
        assert_eq!(batch(&plan, true, 10), "");
    }

    #[test]
    fn a_zero_limit_sends_nothing() {
        let plan = plan_of(&["1"], &["3"]);
        assert_eq!(batch(&plan, true, 0), "");
    }

    #[test]
    fn an_interval_that_would_overflow_the_clock_still_answers() {
        // `saturating_add` rather than `+`: `interval_seconds` is a u32
        // from config and `last_diff_at` is from a file, so neither bound
        // is this function's to assume.
        let situation = Situation {
            last_diff_at: Some(i64::MAX.saturating_sub(1)),
            interval_seconds: u32::MAX,
            ..idle()
        };
        assert_eq!(
            next_step(&situation, i64::MAX.saturating_sub(1)),
            Step::Wait { until: i64::MAX }
        );
    }

    // --- #176: the prune cap ---

    /// A plan whose list had `members` accounts when it was diffed.
    fn plan_against(members: usize, adds: &[&str], removals: &[&str]) -> Plan {
        Plan {
            members_total: members,
            ..plan_of(adds, removals)
        }
    }

    const TEN: [&str; 10] = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "10"];
    const ELEVEN: [&str; 11] = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11"];

    #[test]
    fn removals_within_the_limit_are_allowed() {
        // 10 of 100 is exactly 10%: at the limit, not over it.
        assert!(prune_allowed(&plan_against(100, &[], &TEN), 10));
    }

    #[test]
    fn one_removal_over_the_limit_holds_them_all() {
        // Held, not trimmed to fit: a plan that wants to remove more than
        // the cap allows is a plan whose follow read is suspect as a
        // whole, and sending the first ten of a bad diff is still sending
        // a bad diff.
        assert!(!prune_allowed(&plan_against(100, &[], &ELEVEN), 10));
    }

    #[test]
    fn a_plan_with_no_removals_has_nothing_to_hold() {
        assert!(prune_allowed(&plan_against(0, &["1"], &[]), 10));
    }

    #[test]
    fn a_plan_that_does_not_know_the_list_size_holds_every_removal() {
        // A plan file written before #176 carries no `members_total` and
        // reads as 0. Anything divided by an unknown total is over the
        // limit; the next diff at the interval replaces the file.
        assert!(!prune_allowed(&plan_against(0, &[], &["1"]), 10));
    }

    #[test]
    fn a_limit_of_one_hundred_percent_turns_the_cap_off() {
        // Emptying the list is within a 100% limit by definition.
        assert!(prune_allowed(&plan_against(3, &[], &["1", "2", "3"]), 100));
    }

    #[test]
    fn a_limit_of_zero_never_prunes_in_the_background() {
        assert!(!prune_allowed(&plan_against(1_000, &[], &["1"]), 0));
    }

    #[test]
    fn already_applied_removals_do_not_count_against_the_limit() {
        // What is measured is what would be sent now. A plan the CLI has
        // already pruned most of is not still over the cap for the part
        // that landed.
        let mut plan = plan_against(100, &[], &ELEVEN);
        plan.mark_applied("11", Action::Remove);
        assert!(prune_allowed(&plan, 10));
    }

    #[test]
    fn sendable_counts_removals_only_when_they_may_be_sent() {
        let plan = plan_against(100, &["a", "b"], &["1", "2", "3"]);
        assert_eq!(sendable(&plan, true), 5);
        assert_eq!(sendable(&plan, false), 2);
    }

    #[test]
    fn sendable_skips_what_already_landed() {
        let mut plan = plan_against(100, &["a"], &["1"]);
        plan.mark_applied("a", Action::Add);
        assert_eq!(sendable(&plan, true), 1);
    }

    #[test]
    fn a_diff_whose_removals_are_held_says_so_and_names_the_way_through() {
        let text = notice(&Outcome::Diffed {
            adds: 2,
            removals: 30,
            members_total: 100,
            held: true,
        })
        .unwrap();
        assert!(text.contains("30 of 100"), "{text}");
        assert!(text.contains("--prune"), "{text}");
    }

    #[test]
    fn a_diff_whose_removals_will_be_sent_reads_as_before() {
        let text = notice(&Outcome::Diffed {
            adds: 2,
            removals: 3,
            members_total: 100,
            held: false,
        })
        .unwrap();
        assert_eq!(text, "List sync: 2 to add, 3 to remove.");
    }
}
