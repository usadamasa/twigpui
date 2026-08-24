//! The half of #163's sync that spends: the paged reads, the apply loop,
//! and the `--sync-list` entry point.
//!
//! Nothing here is unit-tested, and that is the reason it is a separate
//! file rather than the bottom of `mod.rs`: every function below makes
//! real HTTP requests, the same way `cache`'s reload paths do. What
//! carries the test coverage is [`super::plan`] and the plan file, which
//! are pure and live next door.

use anyhow::{Context as _, Result};

use super::schedule::Outcome;
use super::{Action, Plan, load_plan, load_state, plan, report, save_plan, save_state};
use crate::cache;
use crate::config::Config;
use crate::oauth;
use crate::paths::Paths;
use crate::x_api::XClient;
use crate::x_api::model::User;

/// Page through one of #163's two reads until the cursor runs out,
/// returning every account or nothing at all.
///
/// **All-or-nothing on purpose.** [`super::plan`] is a set difference, so a
/// truncated read is not a smaller answer, it is a wrong one: follows that
/// were never read look unfollowed and earn deletions, and members that
/// were never read get re-added. Returning `Err` on any page's failure is
/// what keeps a half-read side from ever reaching the diff.
///
/// `MAX_PAGES` is a backstop against a cursor that never terminates, not a
/// cap anybody should hit: at 100 accounts a page it allows 20,000, well
/// past X's own following limits. Hitting it is an error rather than a
/// silent truncation, for the same reason a failed page is.
///
/// Not unit-tested — it makes real HTTP requests through `fetch_page`. The
/// part that carries the test coverage is [`super::plan`], which is pure.
fn read_all(
    what: &str,
    mut fetch_page: impl FnMut(Option<&str>) -> Result<(Vec<User>, Option<String>)>,
) -> Result<Vec<User>> {
    const MAX_PAGES: usize = 200;

    let mut all = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let (page, next) = fetch_page(cursor.as_deref())
            .with_context(|| format!("could not read the whole {what} — nothing was changed"))?;
        all.extend(page);
        match next {
            Some(token) => cursor = Some(token),
            None => return Ok(all),
        }
    }
    anyhow::bail!("the {what} did not finish paging after {MAX_PAGES} pages — nothing was changed")
}

/// Read both sides in full and diff them (#163's dry-run).
///
/// Spends the whole read cost of a sync: every account on both sides is a
/// billed resource. Nothing is written to X here — the result is a [`super::Plan`]
/// for [`apply`] to consume.
///
/// Not unit-tested, for the reason [`read_all`] isn't.
pub(super) fn plan_sync(
    paths: &Paths,
    client: &XClient,
    user_id: &str,
    list_id: &str,
    now: i64,
) -> Result<Plan> {
    let following = match paths.profile().sync_seed_usernames() {
        None => read_all("follow list", |cursor| {
            client.following(paths, user_id, cursor, now)
        })?,
        Some(usernames) => seed_users(paths, client, usernames, now)?,
    };
    let members = read_all("list members", |cursor| {
        client.list_members(paths, list_id, cursor, now)
    })?;
    Ok(plan(list_id, now, &following, &members))
}

/// Stand in for the follow-graph read with a fixed set of screen names
/// (#169) — what a development build syncs from, so working on #163 does
/// not bill a dry run for every account the signed-in user follows.
///
/// Resolved through the same cached lookup `cache::reload` uses, so this
/// costs one billed request per name on the first run of the month and
/// nothing afterwards. Only `id` and `username` reach [`super::plan`], so
/// `name` carries the screen name rather than a second lookup's worth of
/// display name.
///
/// Not unit-tested, for the reason [`read_all`] isn't.
fn seed_users(paths: &Paths, client: &XClient, usernames: &[&str], now: i64) -> Result<Vec<User>> {
    usernames
        .iter()
        .map(|username| {
            // Same shape as `cache::reload`'s own lookup: cache first, and
            // persist whatever the API had to be asked for.
            let id = if let Some(id) = cache::cached_user_id(paths, username, now)? {
                id
            } else {
                let id = client
                    .user_id_by_username(paths, username, now)
                    .with_context(|| {
                        format!("could not resolve the development sync seed @{username}")
                    })?;
                cache::save_user_id(paths, username, &id, now)?;
                id
            };
            Ok(User {
                id,
                name: (*username).to_string(),
                username: (*username).to_string(),
                profile_image_url: None,
            })
        })
        .collect()
}

/// Apply `plan`'s outstanding entries, marking and persisting each one as
/// it lands (#163).
///
/// Saved after every entry rather than once at the end: the whole point of
/// the plan file is that an apply interrupted half way — a rate limit, a
/// crash, a `^C` — resumes without re-reading either side and without
/// re-sending what already went through. A single save at the end would
/// lose exactly the information the resume needs.
///
/// `prune` gates removals only. Additions are what a mirror is for;
/// deleting an account someone added to the list by hand is the part #163
/// leaves undecided, so it does not happen unless asked for.
///
/// Stops at the first failure and returns it, with the plan on disk
/// reflecting everything that did land. Carrying on past an error would
/// keep spending writes against a credential or a list that has just
/// proven it cannot take them.
///
/// Not unit-tested, for the reason [`read_all`] isn't.
fn apply(
    paths: &Paths,
    client: &XClient,
    plan: &mut Plan,
    prune: bool,
    now: i64,
) -> (usize, Result<()>) {
    apply_some(paths, client, plan, prune, now, usize::MAX)
}

/// [`apply`], but sending at most `limit` entries before returning — the
/// background sync's unit of work. Returns how many actually went through,
/// **alongside** the failure if one stopped the batch: the count is what
/// lets `sync::state` tell a refusal that followed a landed write from a
/// refusal that followed a refusal, and a `Result<usize>` would have to
/// drop one to report the other.
///
/// The CLI has no use for a bound: `--apply` is a foreground command whose
/// whole job is to finish. The loop does, for two reasons that have nothing
/// to do with rate limits (the tracked window already stops it on its own):
/// a tick that sends two thousand requests holds the background executor
/// for as long as that takes and cannot be shut down cleanly in the middle,
/// and it would send every addition before the first removal, so a list
/// that is badly out of date would show the additions hours before the
/// stale members went away.
///
/// Removals are interleaved for that second reason — `limit` is split
/// across both actions rather than spent on additions first.
///
/// Not unit-tested, for the reason [`read_all`] isn't.
pub(super) fn apply_some(
    paths: &Paths,
    client: &XClient,
    plan: &mut Plan,
    prune: bool,
    now: i64,
    limit: usize,
) -> (usize, Result<()>) {
    let mut sent = 0usize;
    for (action, user_id) in super::schedule::next_batch(plan, prune, limit) {
        let result = match action {
            Action::Add => client.add_list_member(paths, &plan.list_id, &user_id, now),
            Action::Remove => client.remove_list_member(paths, &plan.list_id, &user_id, now),
        };
        if let Err(error) = result {
            return (sent, Err(error));
        }
        plan.mark_applied(&user_id, action);
        sent = sent.saturating_add(1);
        if let Err(error) = save_plan(&paths.sync_plan_file(), plan) {
            return (sent, Err(error));
        }
    }
    (sent, Ok(()))
}

/// What `--sync-list` was asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Request {
    /// Send the plan's writes. Without this the run is a dry-run: it reads
    /// both sides, writes the plan file, prints the report, and stops.
    /// Dry-run-by-default *is* #163's "confirm before applying" — there is
    /// no interactive prompt, because a run that costs dollars should not
    /// be one keystroke away from a shell history entry.
    pub apply: bool,
    /// Also send the removals. Off by default — see [`apply`].
    pub prune: bool,
}

/// `--sync-list` (#163). Returns the process exit code.
///
/// Every failure here is a refusal to spend rather than a partial run: no
/// list configured, no session, a session without the scopes, a plan for a
/// different list. The cheapest of those checks come first.
pub(crate) fn run_cli(config: &Config, paths: &Paths, request: Request) -> i32 {
    let Some(list_id) = config.list_id.clone() else {
        eprintln!(
            "--sync-list needs a list to sync into. Set X_LIST_ID, or add \
             list_id to config.toml."
        );
        return 1;
    };

    let resolution = match oauth::resolve_credential(config, paths, oauth::unix_now()) {
        Ok(resolution) => resolution,
        Err(error) => {
            eprintln!("could not resolve a credential: {error:#}");
            return 1;
        }
    };
    if let Some(demotion) = &resolution.demotion {
        eprintln!("{}", oauth::describe_demotion(demotion));
    }
    let Some(credential) = resolution.credential else {
        eprintln!(
            "no signed-in session is available. Run twigpui without --sync-list and click \
             \"Sign in with X\" once; this flag reuses the session that leaves behind."
        );
        return 1;
    };

    // Before anything is spent: #163 added `follows.read` and `list.write`
    // to `SCOPES`, so a session authorized before it would page the whole
    // follow list only to be refused at the first write — or be refused at
    // the first read, having already paid for a `/me`.
    if let Some(missing) = super::missing_scope(credential.scope.as_deref()) {
        eprintln!(
            "this session was authorized before --sync-list existed and does not carry \
             {missing}. Launch twigpui and click \"Re-authorize\" once, then run this again."
        );
        return 1;
    }

    let client = XClient::new(credential.token);
    let user_id = match resolve_own_id(paths, &client) {
        Ok(user_id) => user_id,
        Err(error) => {
            eprintln!("could not resolve the signed-in account: {error:#}");
            return 1;
        }
    };

    match run(
        paths,
        &client,
        &user_id,
        &list_id,
        request,
        config.sync_interval_seconds,
    ) {
        Ok(report) => {
            println!("{report}");
            0
        }
        Err(error) => {
            eprintln!("sync failed: {error:#}");
            1
        }
    }
}

/// The signed-in account's own id, from the `/me` cache when it is fresh
/// (30 days — see `cache::cached_me`) and from the API otherwise. Its own
/// function so [`run_cli`] reads as a sequence of refusals.
fn resolve_own_id(paths: &Paths, client: &XClient) -> Result<String> {
    let now = oauth::unix_now();
    if let Some(entry) = cache::cached_me(paths, now)? {
        return Ok(entry.id);
    }
    let user = client.me(paths, now)?;
    cache::save_me(paths, &user.id, &user.username, now)?;
    Ok(user.id)
}

/// The part of [`run_cli`] that has a credential and a list, split out so
/// every error above is a plain refusal and everything below is one
/// `Result`.
///
/// `--apply` shares the background sync's memory ([`super::SyncState`]):
/// it reads the backoff, says so, and **sends anyway** — a person at a
/// terminal choosing to send one batch into a cap to see whether it has
/// lifted is the cheapest measurement #197 has, and refusing would take
/// it away. What comes back is settled into the same state, so a write
/// that lands ends the streak for the loop too, and a refusal lengthens
/// it.
fn run(
    paths: &Paths,
    client: &XClient,
    user_id: &str,
    list_id: &str,
    request: Request,
    interval_seconds: u32,
) -> Result<String> {
    let plan_path = paths.sync_plan_file();
    let now = oauth::unix_now();

    if !request.apply {
        let plan = plan_sync(paths, client, user_id, list_id, now)?;
        save_plan(&plan_path, &plan)?;
        return Ok(format!(
            "{}\n\nnothing was changed. Re-run with --apply to send these.",
            report(&plan)
        ));
    }

    let Some(mut plan) = load_plan(&plan_path)? else {
        anyhow::bail!(
            "no sync plan on file. Run --sync-list without --apply first: the dry-run is \
             what reads both sides and writes the plan this consumes."
        );
    };
    // A plan is only meaningful for the list it was diffed against.
    // Applying one after `list_id` changed would rewrite a list nobody
    // asked about, using a diff computed from a different membership.
    anyhow::ensure!(
        plan.list_id == list_id,
        "the plan on file is for list {}, but list {list_id} is configured. Re-run \
         --sync-list without --apply to diff the configured list.",
        plan.list_id
    );

    let state_path = paths.sync_state_file();
    let state = load_state(&state_path);
    if state.is_blocked(now) {
        eprintln!(
            "note: the background sync is backing off until unix time {} after {} consecutive \
             refusal(s); sending anyway, and recording what happens for it",
            state.blocked_until.unwrap_or(now),
            state.refusals
        );
    }

    let (sent, result) = apply(paths, client, &mut plan, request.prune, now);
    let remaining = super::schedule::sendable(&plan, request.prune);
    let outcome = super::schedule::apply_outcome(sent, remaining, result);
    let settled = super::state::settle(state, outcome.as_ref().ok(), now, interval_seconds);
    save_state(&state_path, &settled.state)?;

    let finished = plan.is_complete() || (!request.prune && plan.pending_count(Action::Add) == 0);
    if matches!(outcome, Ok(Outcome::Applied { .. })) && finished {
        // Nothing left to resume from, and leaving it behind would make the
        // next --apply look like there is work outstanding.
        std::fs::remove_file(&plan_path)
            .with_context(|| format!("could not remove {}", plan_path.display()))?;
    }
    match outcome? {
        Outcome::RateLimited { opaque, .. } => anyhow::bail!(
            "rate limited after {sent} write(s) landed{}; the plan on file records them. \
             Backing off until unix time {} (refusal #{}); re-run --apply after that.",
            if opaque {
                " — by a cap the x-rate-limit headers do not describe"
            } else {
                ""
            },
            settled.wake_at,
            settled.state.refusals
        ),
        _ => Ok(report(&plan)),
    }
}
