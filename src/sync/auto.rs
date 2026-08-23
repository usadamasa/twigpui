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
//! A `Diff` is the expensive one: both sides read in full, one billed
//! resource per account. It is paced by `config.sync_interval_seconds` and
//! by [`super::SyncState`], which persists across launches so relaunching
//! the app does not buy the same answer again.
//!
//! An `Apply` is bounded by [`BATCH`] writes. Not because of the rate limit
//! — the tracked window in `rate_limit` already refuses a send on its own,
//! and that refusal is what [`Outcome::RateLimited`] carries back — but so
//! that no single tick holds the background executor for the length of a
//! two-thousand-account catch-up.

use anyhow::Result;

use super::schedule::Outcome;
use super::{Action, load_plan, load_state, save_plan, save_state, schedule};
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

/// Run one tick.
///
/// `blocked_until` is the caller's memory of the last [`Outcome::RateLimited`]
/// — held by the loop rather than re-read from the tracked window, because
/// the loop is the only thing that needs it and threading it through keeps
/// this function's decision reproducible from its arguments.
///
/// Pruning is unconditional here, unlike `--sync-list`, where it stays
/// behind `--prune`. See this module's parent for why the two paths differ.
pub(crate) fn tick(
    paths: &Paths,
    client: &XClient,
    user_id: &str,
    list_id: &str,
    interval_seconds: u32,
    blocked_until: Option<i64>,
    now: i64,
) -> Result<Outcome> {
    let plan_path = paths.sync_plan_file();
    let state_path = paths.sync_state_file();

    // A plan diffed against a different list is not this list's work. It is
    // dropped rather than applied: `run.rs` refuses in the same situation,
    // and a loop has nobody to refuse to.
    let plan = load_plan(&plan_path)?.filter(|plan| plan.list_id == list_id);
    let pending = plan
        .as_ref()
        .map_or(0, |plan| plan.entries.iter().filter(|e| !e.applied).count());

    let situation = schedule::Situation {
        last_diff_at: load_state(&state_path).last_diff_at,
        interval_seconds,
        pending,
        blocked_until,
    };

    match schedule::next_step(&situation, now) {
        schedule::Step::Wait { until } => Ok(Outcome::Idle { until }),
        schedule::Step::Diff => diff(paths, client, user_id, list_id, now),
        // `pending > 0` is what produced this step, so the plan is `Some`.
        // Listed rather than unwrapped so a later change to the precedence
        // cannot turn this into a panic.
        schedule::Step::Apply => match plan {
            Some(plan) => apply(paths, client, plan, now),
            None => Ok(Outcome::Idle { until: now }),
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
fn diff(
    paths: &Paths,
    client: &XClient,
    user_id: &str,
    list_id: &str,
    now: i64,
) -> Result<Outcome> {
    save_state(
        &paths.sync_state_file(),
        &super::SyncState {
            last_diff_at: Some(now),
        },
    )?;

    let plan = super::run::plan_sync(paths, client, user_id, list_id, now)?;
    let adds = plan.pending_count(Action::Add);
    let removals = plan.pending_count(Action::Remove);
    save_plan(&paths.sync_plan_file(), &plan)?;
    Ok(Outcome::Diffed { adds, removals })
}

/// Send up to [`BATCH`] of the plan's outstanding writes.
///
/// A refusal from the tracked rate-limit window is an outcome rather than
/// an error: nothing was spent, the plan on disk records everything that
/// did land, and the only thing the loop has to do about it is wait. Every
/// other failure propagates.
fn apply(paths: &Paths, client: &XClient, mut plan: super::Plan, now: i64) -> Result<Outcome> {
    let result = super::run::apply_some(paths, client, &mut plan, true, now, BATCH);
    let remaining = plan.entries.iter().filter(|entry| !entry.applied).count();

    if remaining == 0 {
        // Nothing left to resume from. Leaving it behind would make the
        // next tick read it as outstanding work and skip the diff.
        let _ = std::fs::remove_file(paths.sync_plan_file());
    }

    match result {
        Ok(sent) => Ok(Outcome::Applied { sent, remaining }),
        Err(error) => match error.downcast_ref::<RateLimited>() {
            Some(RateLimited {
                reset_at: Some(reset_at),
            }) => Ok(Outcome::RateLimited { until: *reset_at }),
            _ => Err(error),
        },
    }
}
