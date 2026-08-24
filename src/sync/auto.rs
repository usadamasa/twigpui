//! One wake-up of the background sync: the half that spends.
//!
//! [`super::schedule`] decides what a tick should do, [`super::state`]
//! remembers what came of it, and this performs it in between. The split
//! is `run.rs`'s, for `run.rs`'s reason — nothing here is unit tested,
//! because every branch of it is an HTTP request or a file the request's
//! result is written to. What carries the coverage is
//! [`super::schedule::next_step`], [`super::schedule::next_batch`],
//! [`super::schedule::apply_outcome`] and [`super::state::settle`], which
//! are pure and live next door.
//!
//! # What a tick is allowed to cost
//!
//! A `Diff` is the expensive one: both sides read in full, one billed
//! resource per account. It is paced by `config.sync_interval_seconds` and
//! by [`super::SyncState`], which persists across launches so relaunching
//! the app does not buy the same answer again.
//!
//! An `Apply` is bounded by [`BATCH`] writes, and the loop waits
//! `state::APPLY_PAUSE_SECONDS` between batches. Together they are the
//! sustained write rate, and it is deliberately low — see
//! [`super::state`] for the lock #197 measured and what it followed.
//!
//! # What a tick is allowed to delete
//!
//! Pruning here is unconditional in the sense that nobody is asked — but
//! it is capped (#176). A plan whose removals are more of the list than
//! `config.sync_prune_limit_percent` allows has its additions drained and
//! its removals left in the plan file, unsent, for `--sync-list --apply
//! --prune` to confirm. [`schedule::prune_allowed`] is the verdict and
//! carries the reasoning; the rule this module adds is that a held plan
//! is *not* finished work: [`schedule::sendable`] is what `pending` means
//! here, so a plan with nothing sendable left lets the next diff come due
//! instead of pinning the loop on it.
//!
//! # What a tick leaves in the log (#199)
//!
//! One line per tick that did or was refused something, none for a tick
//! that only waited — the loop wakes every minute, and a line per wake-up
//! would fill the file with the same sentence. A refusal is logged every
//! time because, after #198, a refusal only happens once per backoff, not
//! once per wake-up.

use anyhow::Result;

use super::schedule::Outcome;
use super::{Action, SyncState, load_plan, load_state, save_plan, save_state, schedule, state};
use crate::paths::Paths;
use crate::x_api::XClient;

/// Writes sent per `Apply` tick. With `state::APPLY_PAUSE_SECONDS` this
/// is the sustained rate: two a minute. #197's lock followed roughly seven
/// a minute; this is not known to be under the cap, only not to be the
/// thing that trips it — the backoff in [`super::state`] is what handles
/// the cap itself.
const BATCH: usize = 2;

/// How the caller wants this tick paced — everything that is about *when*
/// rather than *what*, and that is not already in [`SyncState`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct Pacing {
    /// `config.sync_interval_seconds`.
    pub interval_seconds: u32,
    /// #174's manual start: drop the interval for this one tick, and the
    /// block a failed tick earned.
    ///
    /// Done by handing [`schedule::next_step`] no last-diff time at all
    /// ([`schedule::last_diff_for`]), which is the value it already reads
    /// as "a diff has never run", and no block unless it is a refusal's
    /// ([`schedule::blocked_for`]). Nothing else about the decision
    /// changes, and that is the reason it is done this way rather than
    /// with a shorter interval or a fourth [`schedule::Step`]: the
    /// precedence still holds, so a refusal's backoff still refuses the
    /// tick and an undrained plan is still drained before both sides are
    /// re-read. Pressing the button while a catch-up is outstanding
    /// therefore spends nothing on reads — it resumes the plan already
    /// paid for.
    ///
    /// The caller keeps it set across a tick that only waited, so a press
    /// during a backoff is honoured when the backoff ends rather than
    /// consumed by it.
    pub forced: bool,
}

/// What one tick came to: what it did, the state it left on disk, and
/// when the loop should come back.
///
/// The state is returned as well as saved so the window can describe the
/// sync from it — the streak, the deadline — without reading the file
/// again or keeping a copy of its own, which is the copy #198 lost.
#[derive(Debug)]
pub(crate) struct Tick {
    /// `Err` is a tick that failed outright; `state` already carries the
    /// interval it earned.
    pub outcome: Result<Outcome>,
    pub state: SyncState,
    /// The soonest the next tick may run.
    pub wake_at: i64,
}

/// Run one tick: decide, perform, settle, save, log.
///
/// Pruning is not opt-in here, unlike `--sync-list`, where it stays behind
/// `--prune` — see this module's parent for why the two paths differ — but
/// it is capped at `prune_limit_percent` of the list (#176); see the
/// module docs.
///
/// The state is saved whether or not the tick succeeded: a failure earns
/// an interval, and that interval has to survive a relaunch. A save that
/// itself fails is logged and the tick's state is returned anyway, so the
/// running loop at least paces itself correctly until the next save.
pub(crate) fn tick(
    paths: &Paths,
    client: &XClient,
    user_id: &str,
    list_id: &str,
    pacing: Pacing,
    prune_limit_percent: u8,
    now: i64,
) -> Tick {
    let state_path = paths.sync_state_file();
    let mut state = load_state(&state_path);
    let outcome = perform(
        paths,
        client,
        user_id,
        list_id,
        pacing,
        prune_limit_percent,
        &mut state,
        now,
    );
    let settled = state::settle(state, outcome.as_ref().ok(), now, pacing.interval_seconds);
    if let Err(error) = save_state(&state_path, &settled.state) {
        crate::log::error(&format!("list sync: could not save its state: {error:#}"));
    }
    log_outcome(&outcome, settled.state, settled.wake_at);
    Tick {
        outcome,
        state: settled.state,
        wake_at: settled.wake_at,
    }
}

/// The tick's work: what [`schedule::next_step`] says, done.
///
/// `state` is mutated in exactly one case — a diff stamps `last_diff_at`
/// before it reads — and the caller settles and saves whatever comes out.
#[allow(clippy::too_many_arguments)]
fn perform(
    paths: &Paths,
    client: &XClient,
    user_id: &str,
    list_id: &str,
    pacing: Pacing,
    prune_limit_percent: u8,
    state: &mut SyncState,
    now: i64,
) -> Result<Outcome> {
    // A plan diffed against a different list is not this list's work. It is
    // dropped rather than applied: `run.rs` refuses in the same situation,
    // and a loop has nobody to refuse to.
    let plan = load_plan(&paths.sync_plan_file())?.filter(|plan| plan.list_id == list_id);
    // Decided here, on the plan as it is now, rather than once at the diff:
    // the limit is configuration and can change between the two, and a
    // plan file from before the cap has never been judged at all.
    let prune = plan
        .as_ref()
        .is_none_or(|plan| schedule::prune_allowed(plan, prune_limit_percent));
    let pending = plan
        .as_ref()
        .map_or(0, |plan| schedule::sendable(plan, prune));

    let situation = schedule::Situation {
        last_diff_at: schedule::last_diff_for(pacing.forced, state.last_diff_at),
        interval_seconds: pacing.interval_seconds,
        pending,
        blocked_until: schedule::blocked_for(pacing.forced, state),
    };

    match schedule::next_step(&situation, now) {
        // `pending` rides along (#174) precisely because this arm is
        // reached both with a drained plan and with a rate-limited one
        // that still owes hundreds of writes — see
        // [`schedule::is_finished`], which is the one caller that has to
        // tell those apart.
        schedule::Step::Wait { until } => Ok(Outcome::Idle { until, pending }),
        schedule::Step::Diff => diff(
            paths,
            client,
            user_id,
            list_id,
            prune_limit_percent,
            state,
            now,
        ),
        // `pending > 0` is what produced this step, so the plan is `Some`.
        // Listed rather than unwrapped so a later change to the precedence
        // cannot turn this into a panic.
        schedule::Step::Apply => match plan {
            Some(plan) => apply(paths, client, plan, prune, now),
            None => Ok(Outcome::Idle {
                until: now,
                pending: 0,
            }),
        },
    }
}

/// Read both sides and write a fresh plan.
///
/// The clock is stamped **before** the reads, and stays stamped whether or
/// not they succeed. Both halves matter: a crash part way through has
/// already been billed for the pages that landed, so a relaunch must not
/// read them again immediately; and a diff that fails every time — a
/// revoked scope, an endpoint that has started 400ing — would otherwise be
/// retried by every wake-up of the loop forever.
///
/// The prune verdict is taken here too, for the outcome only — so the
/// window hears about a held plan once, when it is made, rather than on
/// every batch of additions drained out of it. What is *enforced* is the
/// verdict [`perform`] takes at apply time.
fn diff(
    paths: &Paths,
    client: &XClient,
    user_id: &str,
    list_id: &str,
    prune_limit_percent: u8,
    state: &mut SyncState,
    now: i64,
) -> Result<Outcome> {
    state.last_diff_at = Some(now);
    save_state(&paths.sync_state_file(), state)?;

    let plan = super::run::plan_sync(paths, client, user_id, list_id, now)?;
    let adds = plan.pending_count(Action::Add);
    let removals = plan.pending_count(Action::Remove);
    let held = !schedule::prune_allowed(&plan, prune_limit_percent);
    save_plan(&paths.sync_plan_file(), &plan)?;
    if held {
        crate::log::warn(&format!(
            "list sync: holding {removals} removal(s) against a list of {} members — over \
             sync_prune_limit_percent ({prune_limit_percent}%). They stay in the plan file; \
             confirm them with --sync-list --apply --prune",
            plan.members_total
        ));
    }
    Ok(Outcome::Diffed {
        adds,
        removals,
        members_total: plan.members_total,
        held,
    })
}

/// Send up to [`BATCH`] of the plan's outstanding writes.
///
/// `prune` is [`perform`]'s verdict from [`schedule::prune_allowed`]. With
/// it false the batch is additions only, and `remaining` counts what may
/// still be sent rather than every unapplied entry — the same reading of
/// "outstanding" as `pending`, so the completion notice fires when the
/// additions are drained even though held removals stay behind.
fn apply(
    paths: &Paths,
    client: &XClient,
    mut plan: super::Plan,
    prune: bool,
    now: i64,
) -> Result<Outcome> {
    let (sent, result) = super::run::apply_some(paths, client, &mut plan, prune, now, BATCH);
    let remaining = schedule::sendable(&plan, prune);

    if plan.is_complete() {
        // Nothing left to resume from. Leaving it behind would make the
        // next tick read it as outstanding work and skip the diff.
        //
        // `is_complete`, not `remaining == 0`: a plan drained of its
        // additions with removals held is *not* removed. Those removals
        // were paid for, and the file is what `--sync-list --apply --prune`
        // sends without reading both sides again. The next diff replaces
        // it either way.
        let _ = std::fs::remove_file(paths.sync_plan_file());
    }

    schedule::apply_outcome(sent, remaining, result)
}

/// The tick's line in the log (#199), or nothing for a tick that only
/// waited.
///
/// `log::redact` runs on the way out — an API error can quote a request
/// URL.
fn log_outcome(outcome: &Result<Outcome>, state: SyncState, wake_at: i64) {
    match outcome {
        Ok(Outcome::Idle { .. }) => {}
        Ok(Outcome::Diffed {
            adds,
            removals,
            members_total,
            held,
        }) => crate::log::info(&format!(
            "list sync: diffed — {adds} to add, {removals} to remove, list holds \
             {members_total}{}",
            if *held { " (removals held)" } else { "" }
        )),
        Ok(Outcome::Applied { sent, remaining }) => {
            crate::log::info(&format!("list sync: sent {sent}, {remaining} to go"));
        }
        Ok(Outcome::RateLimited {
            opaque,
            sent,
            remaining,
            ..
        }) => crate::log::warn(&format!(
            "list sync: write refused ({}) after {sent} sent this batch; {remaining} to go; \
             refusal #{}, retrying at unix time {wake_at}",
            if *opaque {
                "by a cap the headers do not describe"
            } else {
                "window exhausted"
            },
            state.refusals
        )),
        Err(error) => crate::log::error(&format!(
            "list sync failed: {error:#}; next attempt at unix time {wake_at}"
        )),
    }
}
