//! The list's membership as this app last knew it (#173).
//!
//! After the first full read, this app is the only thing writing to the
//! list, and every write it sends is recorded in the plan file as it
//! lands. So the membership can be carried locally and only re-read from
//! X now and then to catch drift — which matters because
//! `GET /2/lists/:id/members` bills per returned account at the Users
//! rate, ten times what the follow side costs as an Owned Read
//! (`x-api-budget`). Re-reading it on every diff was most of what a diff
//! cost.
//!
//! # A mirror is a generation, not a cache
//!
//! [`Mirror::read_at`] is when the full read this mirror descends from was
//! made. Writes move [`Mirror::members`] but never `read_at`, so the age
//! of a mirror is the age of its last contact with the truth, however
//! many entries have been applied against it since. [`Mirror::is_usable`]
//! is the whole rule for when the diff may trust it; everything else falls
//! back to a full read.
//!
//! # What a stale mirror costs
//!
//! A member the mirror still lists that the list no longer holds earns a
//! phantom removal; one the list holds that the mirror lacks earns a
//! phantom addition. #176's prune cap bounds the first (its numerator and
//! denominator both come from the mirror, so the share it measures is
//! unchanged), and [`super::auto`] discards the mirror the moment either
//! a held plan or a failed write suggests it is wrong. Nothing here is
//! unit-tested against the network; the file half is tested the way the
//! plan file is.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use super::{Action, PlanEntry};
use crate::x_api::model::User;

/// One account in the mirror. Only what [`super::plan`] matches on and
/// the report prints — `User` carries more and derives no `Serialize`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Member {
    pub id: String,
    pub username: String,
}

/// The membership of one list, as written to
/// [`crate::paths::Paths::sync_members_file`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Mirror {
    /// The list this mirror describes. A mirror is never used for a
    /// different list, however fresh.
    pub list_id: String,
    /// When the full `list_members` read this descends from was made.
    pub read_at: i64,
    pub members: Vec<Member>,
}

impl Mirror {
    /// A fresh mirror from a full read made at `read_at`.
    pub(crate) fn from_read(list_id: &str, read_at: i64, members: &[User]) -> Self {
        Self {
            list_id: list_id.to_string(),
            read_at,
            members: members
                .iter()
                .map(|user| Member {
                    id: user.id.clone(),
                    username: user.username.clone(),
                })
                .collect(),
        }
    }

    /// Whether a diff of `list_id` at `now` may take this mirror in place
    /// of a full read.
    ///
    /// `max_age_seconds` is `config.sync_members_refresh_seconds`; `0`
    /// turns the mirror off, so every diff reads the list in full. A
    /// `read_at` in the future is refused rather than trusted: it came from
    /// a clock this code does not own, and the safe reading of a
    /// timestamp that cannot be right is that nothing about the file can
    /// be — one full read is what that costs.
    pub(crate) fn is_usable(&self, list_id: &str, now: i64, max_age_seconds: u32) -> bool {
        if max_age_seconds == 0 || self.list_id != list_id {
            return false;
        }
        match now.checked_sub(self.read_at) {
            Some(age) if age >= 0 => age < i64::from(max_age_seconds),
            _ => false,
        }
    }

    /// Seconds since the read this mirror descends from, for the log.
    pub(crate) fn age_seconds(&self, now: i64) -> i64 {
        now.saturating_sub(self.read_at)
    }

    /// Reflect a write that went through: an addition joins the mirror
    /// (once — an id already present is left alone), a removal leaves it.
    pub(crate) fn record(&mut self, entry: &PlanEntry) {
        match entry.action {
            Action::Add => {
                if !self.members.iter().any(|member| member.id == entry.user_id) {
                    self.members.push(Member {
                        id: entry.user_id.clone(),
                        username: entry.username.clone(),
                    });
                }
            }
            Action::Remove => self.members.retain(|member| member.id != entry.user_id),
        }
    }

    /// How many accounts this mirror and a fresh full read disagree on —
    /// the symmetric difference by id. Zero is a mirror that was right;
    /// anything else is worth a line in the log before the mirror is
    /// replaced.
    pub(crate) fn drift(&self, fresh: &[User]) -> usize {
        let mine: HashSet<&str> = self.members.iter().map(|m| m.id.as_str()).collect();
        let theirs: HashSet<&str> = fresh.iter().map(|user| user.id.as_str()).collect();
        mine.symmetric_difference(&theirs).count()
    }

    /// The members as [`super::plan`] takes them. `name` carries the
    /// screen name, as `run::seed_users` does: only `id` and `username`
    /// reach the plan.
    pub(crate) fn users(&self) -> Vec<User> {
        self.members
            .iter()
            .map(|member| User {
                id: member.id.clone(),
                name: member.username.clone(),
                username: member.username.clone(),
                profile_image_url: None,
            })
            .collect()
    }
}

/// Read the mirror back from `path`.
///
/// Missing and corrupt both read as `None`, the way `load_state` does and
/// unlike `load_plan`: a mirror that cannot be read costs exactly one full
/// read of the list, which is what every diff cost before the mirror
/// existed. Failing the sync over it would be the more expensive answer.
pub(crate) fn load_mirror(path: &Path) -> Option<Mirror> {
    let contents = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

/// Write `mirror` to `path`.
pub(crate) fn save_mirror(path: &Path, mirror: &Mirror) -> Result<()> {
    let json =
        serde_json::to_string_pretty(mirror).context("could not serialize the member mirror")?;
    std::fs::write(path, json).with_context(|| format!("could not write {}", path.display()))
}

/// Remove the mirror so the next diff reads the list in full. A mirror
/// that is already gone is not an error — the point is that it is gone.
pub(crate) fn discard_mirror(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("could not remove {}", path.display())),
    }
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

    fn entry(id: &str, action: Action) -> PlanEntry {
        PlanEntry {
            user_id: id.to_string(),
            username: format!("user{id}"),
            action,
            applied: true,
        }
    }

    fn ids(mirror: &Mirror) -> Vec<&str> {
        mirror.members.iter().map(|m| m.id.as_str()).collect()
    }

    const WEEK: u32 = 604_800;

    #[test]
    fn a_mirror_keeps_only_what_the_plan_matches_on() {
        let mirror = Mirror::from_read("7", 100, &[user("1", "alice")]);
        assert_eq!(
            mirror.members,
            [Member {
                id: "1".to_string(),
                username: "alice".to_string()
            }]
        );
        assert_eq!(mirror.read_at, 100);
    }

    #[test]
    fn a_fresh_mirror_of_the_same_list_is_usable() {
        let mirror = Mirror::from_read("7", 1_000, &[]);
        assert!(mirror.is_usable("7", 1_000 + 3_600, WEEK));
    }

    #[test]
    fn a_mirror_of_another_list_is_never_usable() {
        // The plan-file rule, on the read side: a membership is only
        // meaningful for the list it was read from.
        let mirror = Mirror::from_read("7", 1_000, &[]);
        assert!(!mirror.is_usable("8", 1_000, WEEK));
    }

    #[test]
    fn a_mirror_as_old_as_the_refresh_interval_is_stale() {
        // Strict: at exactly the interval the full read is due, not
        // one tick later.
        let mirror = Mirror::from_read("7", 1_000, &[]);
        assert!(mirror.is_usable("7", 1_000 + i64::from(WEEK) - 1, WEEK));
        assert!(!mirror.is_usable("7", 1_000 + i64::from(WEEK), WEEK));
    }

    #[test]
    fn a_refresh_interval_of_zero_turns_the_mirror_off() {
        let mirror = Mirror::from_read("7", 1_000, &[]);
        assert!(!mirror.is_usable("7", 1_000, 0));
    }

    #[test]
    fn a_mirror_read_in_the_future_is_not_trusted() {
        // `next_step`'s rule for a future `last_diff_at`, with the
        // opposite safe direction: there it means "diff now", here it
        // means "do not take the file's word for anything".
        let mirror = Mirror::from_read("7", 2_000, &[]);
        assert!(!mirror.is_usable("7", 1_000, WEEK));
    }

    #[test]
    fn an_applied_addition_joins_the_mirror_once() {
        let mut mirror = Mirror::from_read("7", 0, &[user("1", "alice")]);
        mirror.record(&entry("2", Action::Add));
        mirror.record(&entry("2", Action::Add));
        assert_eq!(ids(&mirror), ["1", "2"]);
    }

    #[test]
    fn an_applied_removal_leaves_the_mirror() {
        let mut mirror = Mirror::from_read("7", 0, &[user("1", "alice"), user("2", "bob")]);
        mirror.record(&entry("1", Action::Remove));
        assert_eq!(ids(&mirror), ["2"]);
    }

    #[test]
    fn recording_does_not_move_the_generation() {
        // Writes change the members, not when the truth was last read.
        // Otherwise a mirror kept busy would never come due for a refresh.
        let mut mirror = Mirror::from_read("7", 1_000, &[]);
        mirror.record(&entry("2", Action::Add));
        assert_eq!(mirror.read_at, 1_000);
    }

    #[test]
    fn drift_counts_both_directions() {
        // One the mirror has that the list lost, one the list has that
        // the mirror never saw.
        let mirror = Mirror::from_read("7", 0, &[user("1", "a"), user("2", "b")]);
        assert_eq!(mirror.drift(&[user("2", "b"), user("3", "c")]), 2);
        assert_eq!(mirror.drift(&[user("1", "a"), user("2", "b")]), 0);
    }

    #[test]
    fn the_users_a_mirror_hands_the_plan_carry_the_screen_name_as_the_name() {
        let mirror = Mirror::from_read("7", 0, &[user("1", "alice")]);
        let users = mirror.users();
        assert_eq!(users.len(), 1);
        let first = users.first().unwrap();
        assert_eq!((first.id.as_str(), first.name.as_str()), ("1", "alice"));
        assert_eq!(first.username, "alice");
    }

    #[test]
    fn a_mirror_survives_a_round_trip_through_the_file() {
        let dir = std::env::temp_dir().join(format!("twigpui-mirror-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("members.json");

        let mut written = Mirror::from_read("7", 100, &[user("1", "alice")]);
        written.record(&entry("2", Action::Add));
        save_mirror(&path, &written).unwrap();
        assert_eq!(load_mirror(&path), Some(written));

        discard_mirror(&path).unwrap();
        assert_eq!(load_mirror(&path), None);
        // Discarding what is already gone is not a failure.
        discard_mirror(&path).unwrap();

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_corrupt_mirror_reads_as_no_mirror_rather_than_failing_the_sync() {
        // `load_state`'s rule, not `load_plan`'s: the cost of a miss here
        // is one full read, which is what every diff used to cost anyway.
        let path =
            std::env::temp_dir().join(format!("twigpui-bad-mirror-{}.json", std::process::id()));
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(load_mirror(&path), None);
        std::fs::remove_file(&path).unwrap();
    }
}
