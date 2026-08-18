//! The shared half of twigpui's two per-post toggles: repost (#15) and
//! like (#68).
//!
//! Both features have exactly the same shape. X API v2's timeline response
//! carries no field for "has the signed-in user reposted/liked this post"
//! — there is no v2 equivalent of v1.1's `retweeted`/`favorited` — and
//! checking per-post would cost one request per visible post, which is out
//! of the question for a project whose entire cache exists to avoid spend
//! (see #9's module doc). So each feature keeps its own local record of
//! post ids under `state_dir`, and each drives a button that flips on
//! click and rolls back on failure.
//!
//! Neither of those two mechanisms is repost- or like-specific, so they
//! live here once rather than twice:
//!
//! - [`load_all`]/[`mark`]/[`unmark`]/[`persist`] — the id-set file,
//!   parameterized by path so `repost.rs` and `like.rs` each pass their own
//!   ([`Paths::reposted_posts_file`]/[`Paths::liked_posts_file`]).
//! - [`ToggleState`]/[`ToggleStatus`] — the optimistic-update/rollback
//!   state machine the button renders from, mirroring `compose.rs`'s
//!   `ComposeState`/`ComposeStatus` convention.
//!
//! What stays in `repost.rs`/`like.rs` is what genuinely differs: which
//! endpoint to call, which file to record into, and which of X's own error
//! phrasings mean "the local record is stale" rather than "this failed".
//!
//! [`Paths::reposted_posts_file`]: crate::paths::Paths::reposted_posts_file
//! [`Paths::liked_posts_file`]: crate::paths::Paths::liked_posts_file

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

/// The whole contents of one toggle's record file: every post id this app
/// has currently toggled on.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct IdSetFile {
    #[serde(default)]
    post_ids: HashSet<String>,
}

/// Load [`IdSetFile`] from disk. A missing file is a clean "nothing
/// recorded yet from this app"; a corrupt or differently-shaped file is
/// *also* a clean miss rather than an error, mirroring
/// `rate_limit::load_file`/`cache::load_json`'s shared rule.
fn load_file(path: &Path) -> Result<IdSetFile> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(IdSetFile::default());
        }
        Err(error) => {
            return Err(error).with_context(|| format!("could not read {}", path.display()));
        }
    };
    Ok(serde_json::from_str(&contents).unwrap_or_default())
}

fn save_file(path: &Path, file: &IdSetFile) -> Result<()> {
    let json = serde_json::to_vec_pretty(file)
        .with_context(|| format!("could not serialize {}", path.display()))?;
    std::fs::write(path, json).with_context(|| format!("could not write {}", path.display()))
}

/// Every post id currently on file, read once — `ui.rs` calls this whenever
/// the visible timeline changes (a reload, "Load older", startup) to seed
/// each row's default [`ToggleState`], rather than reading disk once per
/// row on every render.
pub(crate) fn load_all(path: &Path) -> Result<HashSet<String>> {
    Ok(load_file(path)?.post_ids)
}

/// Record `post_id`, alongside whatever else was already on file.
pub(crate) fn mark(path: &Path, post_id: &str) -> Result<()> {
    let mut file = load_file(path)?;
    file.post_ids.insert(post_id.to_string());
    save_file(path, &file)
}

/// Remove `post_id` from the record, alongside whatever else was already on
/// file. Removing an id that was never present is not an error.
pub(crate) fn unmark(path: &Path, post_id: &str) -> Result<()> {
    let mut file = load_file(path)?;
    file.post_ids.remove(post_id);
    save_file(path, &file)
}

/// [`mark`] or [`unmark`], whichever `on` calls for — the shape the
/// error-reconciliation paths in `repost.rs`/`like.rs` need, where the
/// value to persist is only known once X's own error has been read.
pub(crate) fn persist(path: &Path, post_id: &str, on: bool) -> Result<()> {
    if on {
        mark(path, post_id)
    } else {
        unmark(path, post_id)
    }
}

/// One toggle button's status, independent of whether the toggle is
/// currently on — kept separate from [`ToggleState`]'s own value the same
/// way `compose.rs`'s `ComposeStatus` is kept separate from its draft text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ToggleStatus {
    Idle,
    /// A create/delete request is in flight (#15) — mirrors #14's
    /// double-submit guard, though both toggles are reversible so a stray
    /// second click matters less than it does there.
    Pending,
    /// The last toggle failed; carries a message for `ui.rs` to render. Not
    /// itself a reason to refuse another attempt.
    Failed(String),
}

/// One post's toggle state (#15, #68): whether it is currently on (by this
/// app's own local record, possibly still optimistic — see
/// [`Self::start_toggle`]) plus [`ToggleStatus`]. Nothing here touches
/// gpui, the network, or the clock — `ui.rs` drives every transition from a
/// click or a finished request, mirroring `compose.rs`'s `ComposeState`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToggleState {
    on: bool,
    status: ToggleStatus,
}

impl ToggleState {
    /// A fresh state seeded from the local record (or the default `false`
    /// for a post never seen before).
    pub(crate) fn new(on: bool) -> Self {
        Self {
            on,
            status: ToggleStatus::Idle,
        }
    }

    pub(crate) fn is_on(&self) -> bool {
        self.on
    }

    pub(crate) fn status(&self) -> &ToggleStatus {
        &self.status
    }

    fn is_pending(&self) -> bool {
        matches!(self.status, ToggleStatus::Pending)
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
        self.on = !self.on;
        self.status = ToggleStatus::Pending;
    }

    /// Refuse a toggle without ever having attempted a request — e.g. #15's
    /// missing-`tweet.write`-scope check runs before `start_toggle`, the
    /// same way `ComposeState::refuse` handles #14's identical check.
    pub(crate) fn refuse(&mut self, message: String) {
        self.status = ToggleStatus::Failed(message);
    }

    /// Apply a finished create/delete request's outcome: `Ok(actual)`
    /// commits to the server's own resulting state (via the caller's own
    /// reconciliation, this may not equal the value [`Self::start_toggle`]
    /// optimistically guessed, though in practice it generally does — see
    /// `repost::reconcile_from_error`'s doc); `Err` rolls the optimistic
    /// flip back to exactly what it was before `start_toggle`, #15's
    /// explicit rollback guarantee.
    pub(crate) fn apply_result(&mut self, result: Result<bool, String>) {
        match result {
            Ok(actual) => {
                self.on = actual;
                self.status = ToggleStatus::Idle;
            }
            Err(message) => {
                self.on = !self.on;
                self.status = ToggleStatus::Failed(message);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(label: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "twigpui-test-toggle-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root.join("toggled.json")
    }

    fn cleanup(path: &Path) {
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    // --- load_all / mark / unmark ---

    #[test]
    fn load_all_is_empty_when_nothing_is_on_file() {
        let path = temp_file("load-all-missing");
        assert!(load_all(&path).unwrap().is_empty());
        cleanup(&path);
    }

    #[test]
    fn mark_then_load_all_contains_the_id() {
        let path = temp_file("mark");
        mark(&path, "1700000000000000001").unwrap();
        assert!(load_all(&path).unwrap().contains("1700000000000000001"));
        cleanup(&path);
    }

    #[test]
    fn unmark_removes_a_previously_recorded_id() {
        let path = temp_file("unmark");
        mark(&path, "1700000000000000001").unwrap();
        unmark(&path, "1700000000000000001").unwrap();
        assert!(!load_all(&path).unwrap().contains("1700000000000000001"));
        cleanup(&path);
    }

    #[test]
    fn unmark_on_an_id_never_recorded_is_not_an_error() {
        let path = temp_file("unmark-absent");
        unmark(&path, "nonexistent").unwrap();
        assert!(load_all(&path).unwrap().is_empty());
        cleanup(&path);
    }

    #[test]
    fn mark_preserves_other_already_recorded_ids() {
        let path = temp_file("mark-multi");
        mark(&path, "1").unwrap();
        mark(&path, "2").unwrap();
        let ids = load_all(&path).unwrap();
        assert!(ids.contains("1"));
        assert!(ids.contains("2"));
        cleanup(&path);
    }

    #[test]
    fn persist_marks_when_on_and_unmarks_when_off() {
        let path = temp_file("persist");
        persist(&path, "1", true).unwrap();
        assert!(load_all(&path).unwrap().contains("1"));
        persist(&path, "1", false).unwrap();
        assert!(!load_all(&path).unwrap().contains("1"));
        cleanup(&path);
    }

    #[test]
    fn a_corrupted_file_is_a_clean_miss_not_an_error() {
        let path = temp_file("corrupt");
        std::fs::write(&path, b"not json at all").unwrap();
        assert!(load_all(&path).unwrap().is_empty());
        cleanup(&path);
    }

    #[test]
    fn mark_recovers_cleanly_from_a_corrupted_existing_file() {
        let path = temp_file("save-over-corrupt");
        std::fs::write(&path, b"{ not valid json").unwrap();
        mark(&path, "1").unwrap();
        assert!(load_all(&path).unwrap().contains("1"));
        cleanup(&path);
    }

    #[test]
    fn a_genuine_io_error_reading_the_file_still_propagates() {
        let path = temp_file("io-error");
        // A directory where a file is expected is a real I/O error, not
        // corruption — it must surface rather than being swallowed.
        std::fs::create_dir(&path).unwrap();
        assert!(load_all(&path).is_err());
        cleanup(&path);
    }

    // --- ToggleState ---

    #[test]
    fn a_fresh_state_is_idle_at_the_seeded_value() {
        let state = ToggleState::new(true);
        assert!(state.is_on());
        assert!(state.can_toggle());
        assert_eq!(state.status(), &ToggleStatus::Idle);
    }

    #[test]
    fn start_toggle_optimistically_flips_from_off_to_on() {
        let mut state = ToggleState::new(false);
        state.start_toggle();
        assert!(state.is_on());
        assert!(
            !state.can_toggle(),
            "a pending toggle must not allow another"
        );
    }

    #[test]
    fn start_toggle_optimistically_flips_from_on_to_off() {
        let mut state = ToggleState::new(true);
        state.start_toggle();
        assert!(!state.is_on());
    }

    #[test]
    fn a_successful_toggle_commits_the_servers_reported_state_and_returns_to_idle() {
        let mut state = ToggleState::new(false);
        state.start_toggle();
        state.apply_result(Ok(true));
        assert!(state.is_on());
        assert!(state.can_toggle());
        assert_eq!(state.status(), &ToggleStatus::Idle);
    }

    #[test]
    fn a_successful_toggle_can_commit_a_state_that_disagrees_with_the_optimistic_guess() {
        // #15's reconciliation path: the server's own resulting state wins
        // even when it differs from what `start_toggle` guessed.
        let mut state = ToggleState::new(false);
        state.start_toggle(); // optimistic guess: true
        state.apply_result(Ok(false)); // server says: actually still false
        assert!(!state.is_on());
        assert!(state.can_toggle());
    }

    #[test]
    fn a_failed_create_toggle_rolls_back_to_off() {
        let mut state = ToggleState::new(false);
        state.start_toggle(); // optimistic true, pending
        state.apply_result(Err("network error".to_string()));

        assert!(!state.is_on(), "rollback must restore the pre-toggle value");
        assert!(state.can_toggle());
        assert_eq!(
            state.status(),
            &ToggleStatus::Failed("network error".to_string())
        );
    }

    #[test]
    fn a_failed_undo_toggle_rolls_back_to_on() {
        let mut state = ToggleState::new(true);
        state.start_toggle(); // optimistic false, pending
        state.apply_result(Err("boom".to_string()));

        assert!(state.is_on(), "rollback must restore the pre-toggle value");
    }

    #[test]
    fn refuse_records_a_message_without_ever_having_toggled() {
        // #15's missing-scope refusal — mirrors `ComposeState::refuse`.
        let mut state = ToggleState::new(false);
        state.refuse("needs re-authorization".to_string());

        assert!(!state.is_on(), "refuse must not touch the value");
        assert_eq!(
            state.status(),
            &ToggleStatus::Failed("needs re-authorization".to_string())
        );
    }
}
