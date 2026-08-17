//! Local repost bookkeeping (#15).
//!
//! X API v2's timeline response carries no field for "has the signed-in
//! user reposted this post" — there is no v2 equivalent of v1.1's
//! `retweeted`. Checking per-post via `GET /2/tweets/:id/retweeted_by`
//! would cost one request per visible post, which is out of the question
//! for a project whose entire cache exists to avoid spend (see #9's module
//! doc). So this module keeps its own local record instead: every post id
//! this app has reposted, persisted under `state_dir` (state, not cache —
//! see [`crate::paths::Paths::reposted_posts_file`]'s doc for what losing it
//! actually costs). **Reposts made from any other client are never
//! reflected here** — accepted as the tradeoff for a workable button state
//! at zero request cost; see the README.
//!
//! Two pure seams, mirroring `compose.rs`'s and `rate_limit.rs`'s own
//! convention: [`RepostState`] (the per-post optimistic-update/rollback
//! state machine — the button's UI never blocks on the network, see
//! [`RepostState::start_toggle`]) and [`reconcile_from_error`] (a failed
//! response's message text -> a corrected local-record value, when X's own
//! error says the local record is stale). [`create`]/[`remove`] are the
//! thin, not-unit-tested orchestration that actually touches the network
//! (via `XClient`) and disk — mirroring `cache::reload`'s own "not
//! unit-tested directly" convention, since everything they compose is
//! tested standalone.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::paths::Paths;
use crate::x_api::XClient;

/// The whole contents of [`Paths::reposted_posts_file`]: every post id this
/// app has reposted so far.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RepostedFile {
    #[serde(default)]
    post_ids: HashSet<String>,
}

/// Load [`RepostedFile`] from disk. A missing file is a clean "nothing
/// reposted yet from this app"; a corrupt or differently-shaped file is
/// *also* a clean miss rather than an error, mirroring
/// `rate_limit::load_file`/`cache::load_json`'s shared rule.
fn load_file(paths: &Paths) -> Result<RepostedFile> {
    let path = paths.reposted_posts_file();
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RepostedFile::default());
        }
        Err(error) => {
            return Err(error).with_context(|| format!("could not read {}", path.display()));
        }
    };
    Ok(serde_json::from_str(&contents).unwrap_or_default())
}

fn save_file(path: &Path, file: &RepostedFile) -> Result<()> {
    let json = serde_json::to_vec_pretty(file)
        .with_context(|| format!("could not serialize {}", path.display()))?;
    std::fs::write(path, json).with_context(|| format!("could not write {}", path.display()))
}

/// Every post id currently on file, read once — `ui.rs` calls this whenever
/// the visible timeline changes (a reload, "Load older", startup) to seed
/// each row's default [`RepostState`], rather than reading disk once per
/// row on every render.
pub(crate) fn load_all(paths: &Paths) -> Result<HashSet<String>> {
    Ok(load_file(paths)?.post_ids)
}

/// Record `post_id` as reposted, alongside whatever else was already on
/// file.
fn mark_reposted(paths: &Paths, post_id: &str) -> Result<()> {
    let path = paths.reposted_posts_file();
    let mut file = load_file(paths)?;
    file.post_ids.insert(post_id.to_string());
    save_file(&path, &file)
}

/// Remove `post_id` from the record, alongside whatever else was already on
/// file. Removing an id that was never present is not an error.
fn mark_not_reposted(paths: &Paths, post_id: &str) -> Result<()> {
    let path = paths.reposted_posts_file();
    let mut file = load_file(paths)?;
    file.post_ids.remove(post_id);
    save_file(&path, &file)
}

fn persist(paths: &Paths, post_id: &str, reposted: bool) -> Result<()> {
    if reposted {
        mark_reposted(paths, post_id)
    } else {
        mark_not_reposted(paths, post_id)
    }
}

/// Interpret a failed create/delete repost response as a correction to the
/// local record, rather than a genuine failure (#15's only recovery path
/// once the record has drifted from reality): `creating: true` recognizes
/// "you already retweeted this" (the create attempt found the state already
/// true), `creating: false` recognizes "you have not retweeted this" (the
/// delete attempt found the state already false). Returns the corrected
/// value to persist when recognized, `None` for every other failure —
/// callers propagate `None` as an ordinary error.
///
/// **Confidence: unverified against the live API.** X's exact error shape
/// for these two conflicts is not confirmed by this change — see the
/// implementation report. Matched case-insensitively against the same
/// human-readable `title`/`detail`/`reason` text
/// `x_api::client::check_status` already extracts for every other error
/// (via `ApiProblem`), since that text still reaches this function through
/// the stringified `anyhow::Error` regardless of the fixed "403 Forbidden —
/// …" prefix `check_status` adds — more robust to X's exact wording than
/// matching on the status code alone, since a plain 403 is also returned
/// for unrelated permission failures.
pub(crate) fn reconcile_from_error(creating: bool, message: &str) -> Option<bool> {
    let lower = message.to_lowercase();
    if creating && lower.contains("already retweeted") {
        Some(true)
    } else if !creating
        && (lower.contains("have not retweeted") || lower.contains("haven't retweeted"))
    {
        Some(false)
    } else {
        None
    }
}

/// Repost `post_id` as `user_id` (#15): call the API, then persist success.
/// A recognized "already retweeted" conflict (see [`reconcile_from_error`])
/// corrects the local record instead of propagating an error — the caller
/// (`ui.rs`) treats `Ok` as "here is the now-current state", not
/// necessarily "the create succeeded".
///
/// Not unit-tested directly — it makes a real HTTP request through `client`,
/// the same way `cache::reload` isn't. [`reconcile_from_error`] and
/// [`RepostState`] carry this function's actual test coverage.
pub(crate) fn create(
    paths: &Paths,
    client: &XClient,
    user_id: &str,
    post_id: &str,
    now: i64,
) -> Result<bool> {
    match client.create_repost(paths, user_id, post_id, now) {
        Ok(()) => {
            mark_reposted(paths, post_id)?;
            Ok(true)
        }
        Err(error) => match reconcile_from_error(true, &format!("{error:#}")) {
            Some(actual) => {
                persist(paths, post_id, actual)?;
                Ok(actual)
            }
            None => Err(error),
        },
    }
}

/// Un-repost `post_id` as `user_id` (#15) — mirrors [`create`] exactly, the
/// other direction.
pub(crate) fn remove(
    paths: &Paths,
    client: &XClient,
    user_id: &str,
    post_id: &str,
    now: i64,
) -> Result<bool> {
    match client.delete_repost(paths, user_id, post_id, now) {
        Ok(()) => {
            mark_not_reposted(paths, post_id)?;
            Ok(false)
        }
        Err(error) => match reconcile_from_error(false, &format!("{error:#}")) {
            Some(actual) => {
                persist(paths, post_id, actual)?;
                Ok(actual)
            }
            None => Err(error),
        },
    }
}

/// One repost button's status, independent of whether the post is currently
/// reposted — kept separate from [`RepostState`]'s `reposted` field the same
/// way `compose.rs`'s `ComposeStatus` is kept separate from its draft text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RepostStatus {
    Idle,
    /// A create/delete request is in flight (#15) — mirrors #14's
    /// double-submit guard, though a repost is reversible so a stray second
    /// click matters less than it does there.
    Pending,
    /// The last toggle failed; carries a message for `ui.rs` to render. Not
    /// itself a reason to refuse another attempt.
    Failed(String),
}

/// One post's repost button state (#15): whether it is currently reposted
/// (by this app's own local record, possibly still optimistic — see
/// [`Self::start_toggle`]) plus [`RepostStatus`]. Nothing here touches
/// gpui, the network, or the clock — `ui.rs` drives every transition from a
/// click or a finished request, mirroring `compose.rs`'s `ComposeState`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepostState {
    reposted: bool,
    status: RepostStatus,
}

impl RepostState {
    /// A fresh state seeded from the local record (or the default `false`
    /// for a post never seen before).
    pub(crate) fn new(reposted: bool) -> Self {
        Self {
            reposted,
            status: RepostStatus::Idle,
        }
    }

    pub(crate) fn is_reposted(&self) -> bool {
        self.reposted
    }

    pub(crate) fn status(&self) -> &RepostStatus {
        &self.status
    }

    fn is_pending(&self) -> bool {
        matches!(self.status, RepostStatus::Pending)
    }

    /// Whether a click should be allowed to do anything: nothing already in
    /// flight for this exact post.
    pub(crate) fn can_toggle(&self) -> bool {
        !self.is_pending()
    }

    /// Optimistically flip to the opposite state and mark a request in
    /// flight (#15's "flip on click, revert on failure") — the button never
    /// waits on the network to show something changed. Callers must have
    /// already checked [`Self::can_toggle`]; this doesn't re-check.
    pub(crate) fn start_toggle(&mut self) {
        self.reposted = !self.reposted;
        self.status = RepostStatus::Pending;
    }

    /// Refuse a toggle without ever having attempted a request — e.g. #15's
    /// missing-`tweet.write`-scope check runs before `start_toggle`, the
    /// same way `ComposeState::refuse` handles #14's identical check.
    pub(crate) fn refuse(&mut self, message: String) {
        self.status = RepostStatus::Failed(message);
    }

    /// Apply a finished create/delete request's outcome: `Ok(actual)`
    /// commits to the server's own resulting state (via [`create`]/
    /// [`remove`]'s own reconciliation, this may not equal the value
    /// [`Self::start_toggle`] optimistically guessed, though in practice it
    /// generally does — see [`reconcile_from_error`]'s doc); `Err` rolls the
    /// optimistic flip back to exactly what it was before `start_toggle`,
    /// #15's explicit rollback guarantee.
    pub(crate) fn apply_result(&mut self, result: Result<bool, String>) {
        match result {
            Ok(actual) => {
                self.reposted = actual;
                self.status = RepostStatus::Idle;
            }
            Err(message) => {
                self.reposted = !self.reposted;
                self.status = RepostStatus::Failed(message);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_paths(root: &Path) -> Paths {
        let home = root.display().to_string();
        Paths::from_vars(move |key| (key == "HOME").then(|| home.clone())).unwrap()
    }

    fn temp_root(label: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "twigpui-test-repost-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    // --- load_all / mark_reposted / mark_not_reposted ---

    #[test]
    fn load_all_is_empty_when_nothing_is_on_file() {
        let root = temp_root("load-all-missing");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        assert!(load_all(&paths).unwrap().is_empty());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn mark_reposted_then_load_all_contains_the_id() {
        let root = temp_root("mark-reposted");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        mark_reposted(&paths, "1700000000000000001").unwrap();
        let ids = load_all(&paths).unwrap();
        assert!(ids.contains("1700000000000000001"));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn mark_not_reposted_removes_a_previously_reposted_id() {
        let root = temp_root("mark-not-reposted");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        mark_reposted(&paths, "1700000000000000001").unwrap();
        mark_not_reposted(&paths, "1700000000000000001").unwrap();
        assert!(!load_all(&paths).unwrap().contains("1700000000000000001"));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn mark_not_reposted_on_an_id_never_recorded_is_not_an_error() {
        let root = temp_root("mark-not-reposted-absent");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        mark_not_reposted(&paths, "nonexistent").unwrap();
        assert!(load_all(&paths).unwrap().is_empty());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn mark_reposted_preserves_other_already_recorded_ids() {
        let root = temp_root("mark-reposted-multi");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        mark_reposted(&paths, "1").unwrap();
        mark_reposted(&paths, "2").unwrap();
        let ids = load_all(&paths).unwrap();
        assert!(ids.contains("1"));
        assert!(ids.contains("2"));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_corrupted_reposted_file_is_a_clean_miss_not_an_error() {
        let root = temp_root("corrupt");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.reposted_posts_file(), b"not json at all").unwrap();

        assert!(load_all(&paths).unwrap().is_empty());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn mark_reposted_recovers_cleanly_from_a_corrupted_existing_file() {
        let root = temp_root("save-over-corrupt");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.reposted_posts_file(), b"{ not valid json").unwrap();

        mark_reposted(&paths, "1").unwrap();
        assert!(load_all(&paths).unwrap().contains("1"));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_genuine_io_error_reading_the_reposted_file_still_propagates() {
        let root = temp_root("io-error");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        // A directory where a file is expected is a real I/O error, not
        // corruption — it must surface rather than being swallowed.
        std::fs::create_dir(paths.reposted_posts_file()).unwrap();

        assert!(load_all(&paths).is_err());

        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- reconcile_from_error ---

    #[test]
    fn reconciles_an_already_reposted_conflict_on_create() {
        let message = "403 Forbidden — this app cannot access the endpoint: Forbidden: \
                        You have already retweeted this Tweet.";
        assert_eq!(reconcile_from_error(true, message), Some(true));
    }

    #[test]
    fn reconciles_a_not_reposted_conflict_on_delete() {
        let message = "403 Forbidden — this app cannot access the endpoint: Forbidden: \
                        You have not retweeted this Tweet.";
        assert_eq!(reconcile_from_error(false, message), Some(false));
    }

    #[test]
    fn reconciliation_is_case_insensitive() {
        assert_eq!(reconcile_from_error(true, "ALREADY RETWEETED"), Some(true));
    }

    #[test]
    fn does_not_reconcile_an_unrelated_failure() {
        assert_eq!(
            reconcile_from_error(true, "401 Unauthorized — the bearer token was rejected"),
            None
        );
    }

    #[test]
    fn a_create_conflict_message_does_not_reconcile_a_delete_attempt() {
        // The two phrasings are each other's mirror image — matching the
        // wrong direction would silently flip the local record to the
        // wrong value instead of leaving a genuine failure alone.
        assert_eq!(
            reconcile_from_error(false, "you have already retweeted this tweet"),
            None
        );
    }

    #[test]
    fn a_delete_conflict_message_does_not_reconcile_a_create_attempt() {
        assert_eq!(
            reconcile_from_error(true, "you have not retweeted this tweet"),
            None
        );
    }

    // --- RepostState ---

    #[test]
    fn a_fresh_state_is_idle_at_the_seeded_value() {
        let state = RepostState::new(true);
        assert!(state.is_reposted());
        assert!(state.can_toggle());
        assert_eq!(state.status(), &RepostStatus::Idle);
    }

    #[test]
    fn start_toggle_optimistically_flips_from_not_reposted_to_reposted() {
        let mut state = RepostState::new(false);
        state.start_toggle();
        assert!(state.is_reposted());
        assert!(
            !state.can_toggle(),
            "a pending toggle must not allow another"
        );
    }

    #[test]
    fn start_toggle_optimistically_flips_from_reposted_to_not_reposted() {
        let mut state = RepostState::new(true);
        state.start_toggle();
        assert!(!state.is_reposted());
    }

    #[test]
    fn a_successful_toggle_commits_the_servers_reported_state_and_returns_to_idle() {
        let mut state = RepostState::new(false);
        state.start_toggle();
        state.apply_result(Ok(true));
        assert!(state.is_reposted());
        assert!(state.can_toggle());
        assert_eq!(state.status(), &RepostStatus::Idle);
    }

    #[test]
    fn a_successful_toggle_can_commit_a_state_that_disagrees_with_the_optimistic_guess() {
        // #15's reconciliation path: the server's own resulting state wins
        // even when it differs from what `start_toggle` guessed.
        let mut state = RepostState::new(false);
        state.start_toggle(); // optimistic guess: true
        state.apply_result(Ok(false)); // server says: actually still false
        assert!(!state.is_reposted());
        assert!(state.can_toggle());
    }

    #[test]
    fn a_failed_create_toggle_rolls_back_to_not_reposted() {
        let mut state = RepostState::new(false);
        state.start_toggle(); // optimistic true, pending
        state.apply_result(Err("network error".to_string()));

        assert!(
            !state.is_reposted(),
            "rollback must restore the pre-toggle value"
        );
        assert!(state.can_toggle());
        assert_eq!(
            state.status(),
            &RepostStatus::Failed("network error".to_string())
        );
    }

    #[test]
    fn a_failed_un_repost_toggle_rolls_back_to_reposted() {
        let mut state = RepostState::new(true);
        state.start_toggle(); // optimistic false, pending
        state.apply_result(Err("boom".to_string()));

        assert!(
            state.is_reposted(),
            "rollback must restore the pre-toggle value"
        );
    }

    #[test]
    fn refuse_records_a_message_without_ever_having_toggled() {
        // #15's missing-scope refusal — mirrors `ComposeState::refuse`.
        let mut state = RepostState::new(false);
        state.refuse("needs re-authorization".to_string());

        assert!(!state.is_reposted(), "refuse must not touch the value");
        assert_eq!(
            state.status(),
            &RepostStatus::Failed("needs re-authorization".to_string())
        );
    }
}
