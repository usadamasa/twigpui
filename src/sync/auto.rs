//! One wake-up of the background sync: the half that spends.
//!
//! [`super::schedule`] decides what a tick should do; this performs it. The
//! split is `run.rs`'s, for `run.rs`'s reason — nothing here is unit
//! tested, because every branch of it is an HTTP request or a file the
//! request's result is written to. What carries the coverage is
//! [`super::schedule::next_step`] and [`super::schedule::next_batch`],
//! which are pure and live next door.
//!
//! # What a tick is allowed to cost
//!
//! A `Diff` is the expensive one: the follow side read in full, one billed
//! resource per account, and the members side too unless the local mirror
//! is younger than `config.sync_members_refresh_seconds` (#173 — that read
//! bills at ten times the rate). It is paced by `config.sync_interval_seconds`
//! and by [`super::SyncState`], which persists across launches so
//! relaunching the app does not buy the same answer again.
//!
//! An `Apply` is bounded by [`BATCH`] writes. Not because of the rate limit
//! — the tracked window in `rate_limit` already refuses a send on its own,
//! and that refusal is what [`Outcome::RateLimited`] carries back — but so
//! that no single tick holds the background executor for the length of a
//! two-thousand-account catch-up.
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

use anyhow::Result;

use super::schedule::Outcome;
use super::{
    Action, MembersSource, load_plan, load_state, mirror, save_plan, save_state, schedule,
};
use crate::paths::Paths;
use crate::rate_limit::RateLimited;
use crate::x_api::XClient;

/// Writes sent per `Apply` tick.
///
/// Small enough that a tick finishes in seconds — the loop should be able
/// to notice a quit, and the entries it did send are already on disk —
/// and large enough that a list a few thousand accounts behind catches up
/// in hours rather than days at one tick per wake-up.
const BATCH: usize = 20;

/// How the caller wants this tick paced — everything that is about *when*
/// rather than *what*.
///
/// A struct because the three arrived one at a time and the third took
/// [`tick`] past clippy's argument count, but also because two of them are
/// the loop's own memory rather than configuration, and grouping them says
/// so.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Pacing {
    /// `config.sync_interval_seconds`.
    pub interval_seconds: u32,
    /// The caller's memory of the last [`Outcome::RateLimited`] — held by
    /// the loop rather than re-read from the tracked window, because the
    /// loop is the only thing that needs it and threading it through keeps
    /// [`tick`]'s decision reproducible from its arguments.
    pub blocked_until: Option<i64>,
    /// #174's manual start: drop the interval for this one tick.
    ///
    /// Done by handing [`schedule::next_step`] no last-diff time at all,
    /// which is the value it already reads as "a diff has never run".
    /// Nothing else about the decision changes, and that is the reason it
    /// is done this way rather than with a shorter interval or a fourth
    /// [`schedule::Step`]: the precedence still holds, so a live rate
    /// limit still refuses the tick and an undrained plan is still
    /// drained before both sides are re-read. Pressing the button while a
    /// catch-up is outstanding therefore spends nothing on reads — it
    /// resumes the plan already paid for.
    pub forced: bool,
    /// `config.sync_members_refresh_seconds` (#173): how old the local
    /// member mirror may be before a scheduled diff re-reads the list in
    /// full. Paced here rather than passed next to `prune_limit_percent`
    /// because it is about *when* the expensive read happens, and because
    /// `forced` overrides it — a manual start always reads in full.
    pub members_refresh_seconds: u32,
}

/// Run one tick.
///
/// Pruning is not opt-in here, unlike `--sync-list`, where it stays behind
/// `--prune` — see this module's parent for why the two paths differ — but
/// it is capped at `prune_limit_percent` of the list (#176); see the
/// module docs.
pub(crate) fn tick(
    paths: &Paths,
    client: &XClient,
    user_id: &str,
    list_id: &str,
    pacing: Pacing,
    prune_limit_percent: u8,
    now: i64,
) -> Result<Outcome> {
    let plan_path = paths.sync_plan_file();
    let state_path = paths.sync_state_file();

    // A plan diffed against a different list is not this list's work. It is
    // dropped rather than applied: `run.rs` refuses in the same situation,
    // and a loop has nobody to refuse to.
    let plan = load_plan(&plan_path)?.filter(|plan| plan.list_id == list_id);
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
        last_diff_at: schedule::last_diff_for(pacing.forced, load_state(&state_path).last_diff_at),
        interval_seconds: pacing.interval_seconds,
        pending,
        blocked_until: pacing.blocked_until,
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
            pacing,
            prune_limit_percent,
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

/// Read the follow side (and the members side, unless the mirror is fresh
/// enough to stand in for it — #173) and write a fresh plan.
///
/// A forced tick that gets this far reads both sides in full whatever the
/// mirror's age: the person who pressed the button was told that is what
/// it costs, and it is also how the mirror gets a fresh generation on
/// demand. (Whether it gets this far is [`schedule::next_step`]'s call —
/// an outstanding plan is drained first, and no read happens at all.)
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
/// verdict [`tick`] takes at apply time.
fn diff(
    paths: &Paths,
    client: &XClient,
    user_id: &str,
    list_id: &str,
    pacing: Pacing,
    prune_limit_percent: u8,
    now: i64,
) -> Result<Outcome> {
    save_state(
        &paths.sync_state_file(),
        &super::SyncState {
            last_diff_at: Some(now),
        },
    )?;

    let mirror_max_age = if pacing.forced {
        None
    } else {
        Some(pacing.members_refresh_seconds)
    };
    let plan = super::run::plan_sync(paths, client, user_id, list_id, now, mirror_max_age)?;
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
        // The cap exists for a read that came back wrong. When the read
        // was the mirror, the mirror is the suspect: drop it so the next
        // diff pages the list itself and replaces this plan with one
        // computed from the truth.
        if plan.members_source == MembersSource::Mirror {
            mirror::discard_mirror(&paths.sync_members_file())?;
            crate::log::warn(
                "list sync: the held removals came from the local member mirror; discarded \
                 it so the next diff reads the list in full",
            );
        }
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
/// A refusal from the tracked rate-limit window is an outcome rather than
/// an error: nothing was spent, the plan on disk records everything that
/// did land, and the only thing the loop has to do about it is wait. Every
/// other failure propagates.
///
/// `prune` is [`tick`]'s verdict from [`schedule::prune_allowed`]. With it
/// false the batch is additions only, and `remaining` counts what may still
/// be sent rather than every unapplied entry — the same reading of
/// "outstanding" as `pending`, so the completion notice fires when the
/// additions are drained even though held removals stay behind.
fn apply(
    paths: &Paths,
    client: &XClient,
    mut plan: super::Plan,
    prune: bool,
    now: i64,
) -> Result<Outcome> {
    let result = super::run::apply_some(paths, client, &mut plan, prune, now, BATCH);
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

    match result {
        Ok(sent) => Ok(Outcome::Applied { sent, remaining }),
        Err(error) => match error.downcast_ref::<RateLimited>() {
            Some(RateLimited {
                reset_at: Some(reset_at),
            }) => Ok(Outcome::RateLimited { until: *reset_at }),
            Some(RateLimited { reset_at: None }) => Err(error),
            None => {
                // A refused write against a plan diffed from the mirror is
                // the mirror's most likely failure showing: an addition of
                // an account the list already holds, or a removal of one
                // it no longer does. Both files go — the plan cost one
                // follow read to derive, and keeping it would have the
                // next tick retry the same write into the same refusal
                // rather than let a full read replace it (#173).
                if plan.members_source == MembersSource::Mirror {
                    mirror::discard_mirror(&paths.sync_members_file())?;
                    let _ = std::fs::remove_file(paths.sync_plan_file());
                    crate::log::warn(
                        "list sync: a write from a plan diffed against the local member \
                         mirror was refused; discarded the mirror and the plan so the next \
                         diff reads the list in full",
                    );
                }
                Err(error)
            }
        },
    }
}
