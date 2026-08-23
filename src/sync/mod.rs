//! Mirroring the accounts this app follows into a List (#163).
//!
//! #161 made a List the window's primary source, which makes the list's
//! membership the timeline's contents. Typing a following list back in by
//! hand is not a thing anyone does twice, and a list does not follow along
//! when the account does — so the two sides get diffed here and the
//! difference applied.
//!
//! # One direction
//!
//! Following is the truth; the list follows it. The reverse would mean
//! adding an account on x.com by editing a list, which is not what a
//! mirror is for.
//!
//! # Two rules that outrank convenience
//!
//! **A partial read never becomes a plan.** Both sides are paged, and the
//! diff is a set difference: an account missing from a truncated follow
//! list reads as unfollowed and earns a deletion, and a truncated member
//! list re-adds accounts that are already there. So [`read_all`]'s failure
//! is fatal to the whole sync rather than something to carry on from —
//! there is no such thing as a usable partial answer here.
//!
//! **Removals are opt-in on the CLI.** A list can hold accounts put there
//! by hand, and a plan always *lists* removals whether or not it is asked
//! to send them, so `--sync-list --apply` still leaves them alone without
//! `--prune`.
//!
//! The background sync does prune (2026-08-23, by the owner's decision),
//! because "the list is what this app follows" is the whole contract it
//! offers. Accounts added to the list by hand are deleted by it. That is
//! the intended behavior, not an accident, and it is why the all-or-
//! nothing rule above matters more than it did: a truncated follow read
//! plus pruning is a mass deletion rather than a missed addition.
//!
//! # Cost
//!
//! Both reads bill per returned resource (`x-api-budget`), so a dry-run
//! against a few thousand follows is dollars, not cents — which is why the
//! plan is written to disk. Re-running `--apply` after a failure resumes
//! from the file rather than paying to read both sides again, and each
//! entry is marked as it lands so nothing is sent twice.
//!
//! # On a timer
//!
//! #163 ruled out automatic polling for that reason. That was overridden
//! (2026-08-23), and the reason was kept rather than dropped: the diff runs
//! on a long, configurable interval whose last *attempt* is persisted in
//! [`SyncState`], so relaunching the app does not buy the same answer
//! again. [`schedule`] holds the decision and [`auto::tick`] performs it.

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::x_api::model::User;

mod auto;
mod run;
mod schedule;

pub(crate) use auto::tick;
pub(crate) use run::{Request, run_cli};
pub(crate) use schedule::{notice, settle};

/// Which side of the diff an entry came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Action {
    /// Followed, but not in the list.
    Add,
    /// In the list, but not followed.
    Remove,
}

/// One account the sync would act on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PlanEntry {
    pub user_id: String,
    /// Carried only so the report can name accounts rather than ids. Not
    /// used to match anything: a screen name changes, an id does not.
    pub username: String,
    pub action: Action,
    /// Set once the request for this entry has come back `Ok`. `#[serde(default)]`
    /// so a plan file written before an interrupted apply still loads.
    #[serde(default)]
    pub applied: bool,
}

/// What a sync would do, as written to [`crate::paths::Paths::sync_plan_file`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Plan {
    /// The list this plan was computed against. Checked before applying:
    /// a plan is only meaningful for the list it was diffed from, and
    /// pointing `list_id` somewhere else between the dry-run and the apply
    /// would otherwise rewrite the wrong list's membership.
    pub list_id: String,
    pub created_at: i64,
    pub entries: Vec<PlanEntry>,
}

impl Plan {
    /// Entries of `action` that have not been applied yet.
    pub(crate) fn pending(&self, action: Action) -> impl Iterator<Item = &PlanEntry> {
        self.entries
            .iter()
            .filter(move |entry| entry.action == action && !entry.applied)
    }

    /// How many entries of `action` are still outstanding.
    pub(crate) fn pending_count(&self, action: Action) -> usize {
        self.pending(action).count()
    }

    /// Record that `user_id`'s `action` went through. A no-op for an id the
    /// plan does not carry, which cannot happen from `apply` but keeps this
    /// from being a panic if it ever does.
    pub(crate) fn mark_applied(&mut self, user_id: &str, action: Action) {
        for entry in &mut self.entries {
            if entry.user_id == user_id && entry.action == action {
                entry.applied = true;
            }
        }
    }

    /// Whether every entry has been applied — the point at which the plan
    /// file has nothing left to say and can be discarded.
    pub(crate) fn is_complete(&self) -> bool {
        self.entries.iter().all(|entry| entry.applied)
    }
}

/// The first scope `--sync-list` needs that `granted` does not carry, or
/// `None` when the session can do the whole job.
///
/// Checked before the first request rather than discovered at it: reading
/// the follow list costs one billed resource per account, so a session
/// that would be refused at the first *write* must be turned away before
/// it pays for the reads. Both scopes are new in #163, so every session
/// authorized before it fails this.
pub(crate) fn missing_scope(granted: Option<&str>) -> Option<&'static str> {
    [
        crate::oauth::tokens::FOLLOWS_READ_SCOPE,
        crate::oauth::tokens::LIST_WRITE_SCOPE,
    ]
    .into_iter()
    .find(|required| !crate::oauth::tokens::has_scope(granted, required))
}

/// Diff the two sides into a plan (#163's core, and the only part of this
/// module that can be tested without a network).
///
/// Matching is by user id throughout. A screen name is not an identity: an
/// account that renames itself between the two reads would otherwise be
/// removed and re-added, spending two writes to change nothing.
///
/// Order is `following`'s for additions and `members`' for removals, so a
/// report reads in the same order the API handed the accounts over rather
/// than in whatever order a hash set iterates.
pub(crate) fn plan(list_id: &str, now: i64, following: &[User], members: &[User]) -> Plan {
    let member_ids: std::collections::HashSet<&str> =
        members.iter().map(|user| user.id.as_str()).collect();
    let following_ids: std::collections::HashSet<&str> =
        following.iter().map(|user| user.id.as_str()).collect();

    let adds = following
        .iter()
        .filter(|user| !member_ids.contains(user.id.as_str()))
        .map(|user| entry(user, Action::Add));
    let removals = members
        .iter()
        .filter(|user| !following_ids.contains(user.id.as_str()))
        .map(|user| entry(user, Action::Remove));

    Plan {
        list_id: list_id.to_string(),
        created_at: now,
        entries: adds.chain(removals).collect(),
    }
}

fn entry(user: &User, action: Action) -> PlanEntry {
    PlanEntry {
        user_id: user.id.clone(),
        username: user.username.clone(),
        action,
        applied: false,
    }
}

/// The dry-run's report: what the plan would do, and what it has already
/// done if this is a re-run.
///
/// Prices are deliberately absent. `x-api-budget` records the read side as
/// measured ($0.005/resource for other people's posts, $0.001 for one's
/// own) but has nothing measured for `/2/lists/:id/members` or for either
/// write, so any figure here would be docs restated as fact. Counts are
/// what this crate actually knows.
pub(crate) fn report(plan: &Plan) -> String {
    let adds = plan.pending_count(Action::Add);
    let removals = plan.pending_count(Action::Remove);
    let done = plan.entries.iter().filter(|entry| entry.applied).count();

    let mut lines = vec![format!(
        "list {}: {adds} to add, {removals} to remove",
        plan.list_id
    )];
    if done > 0 {
        lines.push(format!(
            "{done} entr{} already applied by an earlier run — not resent",
            if done == 1 { "y" } else { "ies" }
        ));
    }
    lines.push(format!(
        "applying costs {} write request(s); removals need --prune",
        adds.saturating_add(removals)
    ));
    for entry in plan.entries.iter().filter(|entry| !entry.applied) {
        let verb = match entry.action {
            Action::Add => "+",
            Action::Remove => "-",
        };
        lines.push(format!("  {verb} @{} ({})", entry.username, entry.user_id));
    }
    lines.join("\n")
}

/// Read `plan` back from `path`. Unlike the timeline caches, a corrupt file
/// is an error rather than a clean miss: a cache miss costs one avoidable
/// request, whereas silently treating an unreadable plan as "no plan" would
/// send an apply back to reading both sides in full.
pub(crate) fn load_plan(path: &std::path::Path) -> Result<Option<Plan>> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("could not read {}", path.display()));
        }
    };
    let plan = serde_json::from_str(&contents)
        .with_context(|| format!("could not parse the sync plan in {}", path.display()))?;
    Ok(Some(plan))
}

/// Write `plan` to `path`.
pub(crate) fn save_plan(path: &std::path::Path, plan: &Plan) -> Result<()> {
    let json = serde_json::to_string_pretty(plan).context("could not serialize the sync plan")?;
    std::fs::write(path, json).with_context(|| format!("could not write {}", path.display()))
}

/// The background sync's clock, as written to
/// [`crate::paths::Paths::sync_state_file`].
///
/// One field, and a struct anyway: it is written by a loop that spends
/// money on a timer, and the next thing anyone wants recorded here (the
/// last outcome, a consecutive-failure count) should not have to change
/// the file's shape to get in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) struct SyncState {
    /// When the last diff was *attempted*. See [`run::auto_tick`] for why
    /// a failed attempt still moves this.
    #[serde(default)]
    pub last_diff_at: Option<i64>,
}

/// Read the sync clock back from `path`.
///
/// Unlike [`load_plan`], a corrupt file is `Ok(default)` rather than an
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

/// Write the sync clock to `path`.
pub(crate) fn save_state(path: &std::path::Path, state: &SyncState) -> Result<()> {
    let json = serde_json::to_string_pretty(state).context("could not serialize the sync state")?;
    std::fs::write(path, json).with_context(|| format!("could not write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(id: &str, username: &str) -> User {
        User {
            id: id.to_string(),
            name: username.to_string(),
            username: username.to_string(),
            profile_image_url: None,
        }
    }

    fn ids(plan: &Plan, action: Action) -> Vec<&str> {
        plan.pending(action)
            .map(|entry| entry.user_id.as_str())
            .collect()
    }

    /// Everything `SCOPES` requests as of #163 — what a session authorized
    /// today carries.
    const CURRENT_SCOPES: &str = "tweet.read users.read tweet.write like.write list.read list.write follows.read \
         offline.access";

    #[test]
    fn a_current_session_is_missing_no_scope() {
        assert_eq!(missing_scope(Some(CURRENT_SCOPES)), None);
    }

    #[test]
    fn a_session_predating_163_is_turned_away_before_it_reads_anything() {
        // The expensive failure this prevents: paging thousands of billed
        // accounts and then being refused at the first write.
        let pre_163 = "tweet.read users.read tweet.write like.write list.read offline.access";
        assert_eq!(missing_scope(Some(pre_163)), Some("follows.read"));
    }

    #[test]
    fn a_session_that_can_read_follows_but_not_write_the_list_is_still_refused() {
        // The live token carried `follows.read` for a while as a leftover
        // from #157's investigation. Reading both sides with it would have
        // been billed in full and then refused at the first add.
        let read_only = "tweet.read users.read tweet.write like.write list.read follows.read \
                         offline.access";
        assert_eq!(missing_scope(Some(read_only)), Some("list.write"));
    }

    #[test]
    fn an_unrecorded_scope_is_treated_as_insufficient() {
        // `has_scope`'s own rule: unknown is not permission.
        assert_eq!(missing_scope(None), Some("follows.read"));
    }

    #[test]
    fn a_followed_account_missing_from_the_list_is_an_addition() {
        let plan = plan("7", 0, &[user("1", "alice")], &[]);
        assert_eq!(ids(&plan, Action::Add), ["1"]);
        assert!(ids(&plan, Action::Remove).is_empty());
    }

    #[test]
    fn a_member_no_longer_followed_is_a_removal() {
        let plan = plan("7", 0, &[], &[user("1", "alice")]);
        assert_eq!(ids(&plan, Action::Remove), ["1"]);
        assert!(ids(&plan, Action::Add).is_empty());
    }

    #[test]
    fn an_account_on_both_sides_is_left_alone() {
        // The overwhelmingly common case on a re-run. Spending a write on
        // it would make every sync cost the size of the whole list.
        let plan = plan("7", 0, &[user("1", "alice")], &[user("1", "alice")]);
        assert!(plan.entries.is_empty());
    }

    #[test]
    fn matching_is_by_id_not_by_screen_name() {
        // An account that renamed itself between the two reads is the same
        // account. Matching on the name would remove and re-add it.
        let plan = plan("7", 0, &[user("1", "newname")], &[user("1", "oldname")]);
        assert!(plan.entries.is_empty());
    }

    #[test]
    fn two_accounts_sharing_a_screen_name_are_still_two_accounts() {
        // The mirror of the test above: the same name on different ids must
        // not collapse into one entry.
        let plan = plan("7", 0, &[user("1", "alice"), user("2", "alice")], &[]);
        assert_eq!(ids(&plan, Action::Add), ["1", "2"]);
    }

    #[test]
    fn additions_keep_the_order_they_were_read_in() {
        let plan = plan(
            "7",
            0,
            &[user("3", "c"), user("1", "a"), user("2", "b")],
            &[],
        );
        assert_eq!(ids(&plan, Action::Add), ["3", "1", "2"]);
    }

    #[test]
    fn the_plan_records_which_list_it_was_diffed_against() {
        // Applying a plan to a different list would rewrite the wrong
        // list's membership; the apply path compares this.
        let plan = plan("2091351590695588200", 0, &[user("1", "a")], &[]);
        assert_eq!(plan.list_id, "2091351590695588200");
    }

    #[test]
    fn marking_an_entry_applied_takes_it_out_of_pending() {
        let mut plan = plan("7", 0, &[user("1", "a"), user("2", "b")], &[]);
        plan.mark_applied("1", Action::Add);
        assert_eq!(ids(&plan, Action::Add), ["2"]);
        assert_eq!(plan.pending_count(Action::Add), 1);
    }

    #[test]
    fn marking_an_addition_does_not_mark_a_removal_of_the_same_id() {
        // An id can legitimately appear on both sides only through a bug,
        // but if it ever does, applying one must not silently retire the
        // other.
        let mut plan = plan("7", 0, &[user("1", "a")], &[user("2", "b")]);
        plan.entries.push(PlanEntry {
            user_id: "1".to_string(),
            username: "a".to_string(),
            action: Action::Remove,
            applied: false,
        });
        plan.mark_applied("1", Action::Add);
        assert_eq!(ids(&plan, Action::Remove), ["2", "1"]);
    }

    #[test]
    fn a_plan_is_complete_only_once_every_entry_landed() {
        let mut plan = plan("7", 0, &[user("1", "a"), user("2", "b")], &[]);
        assert!(!plan.is_complete());
        plan.mark_applied("1", Action::Add);
        assert!(!plan.is_complete());
        plan.mark_applied("2", Action::Add);
        assert!(plan.is_complete());
    }

    #[test]
    fn an_empty_plan_is_complete() {
        assert!(plan("7", 0, &[], &[]).is_complete());
    }

    #[test]
    fn the_report_counts_both_sides_and_says_removals_are_opt_in() {
        let plan = plan("7", 0, &[user("1", "alice")], &[user("2", "bob")]);
        let report = report(&plan);
        assert!(report.contains("1 to add, 1 to remove"), "{report}");
        assert!(report.contains("--prune"), "{report}");
        assert!(report.contains("@alice"), "{report}");
        assert!(report.contains("@bob"), "{report}");
    }

    #[test]
    fn the_report_says_what_an_earlier_run_already_applied() {
        // A re-run that silently showed a smaller number would look like
        // the follow list had shrunk.
        let mut plan = plan("7", 0, &[user("1", "alice"), user("2", "bob")], &[]);
        plan.mark_applied("1", Action::Add);
        let report = report(&plan);
        assert!(report.contains("1 entry already applied"), "{report}");
        assert!(report.contains("1 to add"), "{report}");
        assert!(!report.contains("@alice"), "{report}");
    }

    #[test]
    fn the_report_quotes_no_price() {
        // `x-api-budget` has nothing measured for either write or for
        // `/2/lists/:id/members`. Printing a docs figure as if it were
        // known is the failure #162 is open about.
        let report = report(&plan("7", 0, &[user("1", "alice")], &[]));
        assert!(!report.contains('$'), "{report}");
    }

    #[test]
    fn a_plan_survives_a_round_trip_through_the_file() {
        let dir = std::env::temp_dir().join(format!("twigpui-sync-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("plan.json");

        let mut written = plan("7", 100, &[user("1", "alice")], &[user("2", "bob")]);
        written.mark_applied("1", Action::Add);
        save_plan(&path, &written).unwrap();

        assert_eq!(load_plan(&path).unwrap(), Some(written));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_plan_file_is_no_plan_rather_than_an_error() {
        let path =
            std::env::temp_dir().join(format!("twigpui-no-plan-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert_eq!(load_plan(&path).unwrap(), None);
    }

    #[test]
    fn the_sync_clock_survives_a_round_trip_through_the_file() {
        let dir = std::env::temp_dir().join(format!("twigpui-sync-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");

        let written = SyncState {
            last_diff_at: Some(1_700_000_000),
        };
        save_state(&path, &written).unwrap();
        assert_eq!(load_state(&path), written);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_clock_file_reads_as_never_synced() {
        // Which makes the first launch diff immediately — the behavior the
        // schedule wants from a fresh install.
        let path =
            std::env::temp_dir().join(format!("twigpui-no-state-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert_eq!(load_state(&path).last_diff_at, None);
    }

    #[test]
    fn a_corrupt_clock_file_reads_as_never_synced_rather_than_failing_the_loop() {
        // The opposite of `load_plan`'s rule, on purpose: a bad clock costs
        // one diff that was due within the interval anyway, whereas failing
        // the loop over it would stop the feature outright.
        let path =
            std::env::temp_dir().join(format!("twigpui-bad-state-{}.json", std::process::id()));
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(load_state(&path).last_diff_at, None);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_corrupt_plan_file_is_an_error_naming_the_path() {
        // Unlike a timeline cache, treating this as a miss would send the
        // apply back to paying for both full reads.
        let path =
            std::env::temp_dir().join(format!("twigpui-bad-plan-{}.json", std::process::id()));
        std::fs::write(&path, "{ not json").unwrap();

        let error = load_plan(&path).unwrap_err().to_string();
        assert!(error.contains(&path.display().to_string()), "{error}");

        std::fs::remove_file(&path).unwrap();
    }
}
