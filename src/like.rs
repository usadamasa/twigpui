//! Like bookkeeping (#68) — the mirror image of [`crate::repost`].
//!
//! The local record of post ids and the optimistic-update/rollback button
//! state both live in [`crate::toggle`], shared with reposts. What is here
//! is what differs: the `likes` endpoints, [`Paths::liked_posts_file`], and
//! the error phrasings that mean "the local record is stale".
//!
//! **Likes made from any other client are never reflected here**, the same
//! tradeoff `repost.rs` documents: X API v2's timeline response carries no
//! "did I like this" field, and checking per-post would cost one request
//! per visible row.
//!
//! [`Paths::liked_posts_file`]: crate::paths::Paths::liked_posts_file

use std::collections::HashSet;

use anyhow::Result;

use crate::paths::Paths;
use crate::toggle;
use crate::x_api::XClient;

/// Every post id currently recorded as liked — see [`toggle::load_all`].
pub(crate) fn load_all(paths: &Paths) -> Result<HashSet<String>> {
    toggle::load_all(&paths.liked_posts_file())
}

/// Interpret a failed like/unlike response as a correction to the local
/// record rather than a genuine failure — `repost::reconcile_from_error`'s
/// counterpart, matching the phrasings X uses for likes instead of
/// retweets: `creating: true` recognizes "you have already liked this
/// Tweet", `creating: false` recognizes "you have not liked this Tweet".
/// Returns the corrected value to persist when recognized, `None` for every
/// other failure — callers propagate `None` as an ordinary error.
///
/// **Confidence: unverified against the live API**, exactly as for reposts
/// (see `repost::reconcile_from_error`'s doc for why matching the
/// human-readable message text is nonetheless the more robust choice than
/// matching a bare 403).
///
/// The two directions are deliberately not collapsed into one substring
/// check: "already liked" and "not liked" are each other's mirror image,
/// and matching the wrong direction would silently flip the local record to
/// the wrong value instead of leaving a genuine failure alone.
pub(crate) fn reconcile_from_error(creating: bool, message: &str) -> Option<bool> {
    let lower = message.to_lowercase();
    if creating && lower.contains("already liked") {
        Some(true)
    } else if !creating && (lower.contains("have not liked") || lower.contains("haven't liked")) {
        Some(false)
    } else {
        None
    }
}

/// Like `post_id` as `user_id` (#68): call the API, then persist success.
/// A recognized "already liked" conflict (see [`reconcile_from_error`])
/// corrects the local record instead of propagating an error — the caller
/// (`ui.rs`) treats `Ok` as "here is the now-current state", not
/// necessarily "the create succeeded".
///
/// Not unit-tested directly — it makes a real HTTP request through
/// `client`, mirroring `repost::create`.
pub(crate) fn create(
    paths: &Paths,
    client: &XClient,
    user_id: &str,
    post_id: &str,
    now: i64,
) -> Result<bool> {
    let path = paths.liked_posts_file();
    match client.create_like(paths, user_id, post_id, now) {
        Ok(()) => {
            toggle::mark(&path, post_id)?;
            Ok(true)
        }
        Err(error) => match reconcile_from_error(true, &format!("{error:#}")) {
            Some(actual) => {
                toggle::persist(&path, post_id, actual)?;
                Ok(actual)
            }
            None => Err(error),
        },
    }
}

/// Unlike `post_id` as `user_id` (#68) — mirrors [`create`] exactly, the
/// other direction.
pub(crate) fn remove(
    paths: &Paths,
    client: &XClient,
    user_id: &str,
    post_id: &str,
    now: i64,
) -> Result<bool> {
    let path = paths.liked_posts_file();
    match client.delete_like(paths, user_id, post_id, now) {
        Ok(()) => {
            toggle::unmark(&path, post_id)?;
            Ok(false)
        }
        Err(error) => match reconcile_from_error(false, &format!("{error:#}")) {
            Some(actual) => {
                toggle::persist(&path, post_id, actual)?;
                Ok(actual)
            }
            None => Err(error),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconciles_an_already_liked_conflict_on_create() {
        let message = "403 Forbidden — this app cannot access the endpoint: Forbidden: \
                        You have already liked this Tweet.";
        assert_eq!(reconcile_from_error(true, message), Some(true));
    }

    #[test]
    fn reconciles_a_not_liked_conflict_on_delete() {
        let message = "403 Forbidden — this app cannot access the endpoint: Forbidden: \
                        You have not liked this Tweet.";
        assert_eq!(reconcile_from_error(false, message), Some(false));
    }

    #[test]
    fn reconciliation_is_case_insensitive() {
        assert_eq!(reconcile_from_error(true, "ALREADY LIKED"), Some(true));
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
        assert_eq!(
            reconcile_from_error(false, "you have already liked this tweet"),
            None
        );
    }

    #[test]
    fn a_delete_conflict_message_does_not_reconcile_a_create_attempt() {
        assert_eq!(
            reconcile_from_error(true, "you have not liked this tweet"),
            None
        );
    }

    #[test]
    fn a_repost_conflict_message_does_not_reconcile_a_like_attempt() {
        // The two modules read the same stringified error; each must only
        // recognize its own endpoint's phrasing.
        assert_eq!(
            reconcile_from_error(true, "you have already retweeted this tweet"),
            None
        );
    }

    #[test]
    fn load_all_reads_the_liked_record_under_the_state_dir() {
        let root = std::env::temp_dir().join(format!("twigpui-test-like-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.display().to_string();
        let paths = Paths::from_vars(move |key| (key == "HOME").then(|| home.clone())).unwrap();
        paths.ensure_dirs().unwrap();

        toggle::mark(&paths.liked_posts_file(), "1").unwrap();
        assert!(load_all(&paths).unwrap().contains("1"));
        // The repost record is a different file, so it must stay empty.
        assert!(crate::repost::load_all(&paths).unwrap().is_empty());

        std::fs::remove_dir_all(&root).unwrap();
    }
}
