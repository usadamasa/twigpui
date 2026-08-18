//! Repost bookkeeping (#15) — the parts that are actually about reposting.
//!
//! The two mechanisms this feature runs on, a local record of post ids and
//! an optimistic-update/rollback button state, are shared with likes (#68)
//! and live in [`crate::toggle`]. What stays here is what is genuinely
//! repost-specific: which endpoint to call, which file to record into, and
//! which of X's own error phrasings mean "the local record is stale"
//! rather than "this failed".
//!
//! **Reposts made from any other client are never reflected here** — the
//! local record is the only source of truth twigpui has, accepted as the
//! tradeoff for a workable button state at zero request cost; see the
//! README and [`crate::paths::Paths::reposted_posts_file`]'s doc.
//!
//! [`create`]/[`remove`] are the thin, not-unit-tested orchestration that
//! actually touches the network (via `XClient`) and disk — mirroring
//! `cache::reload`'s own "not unit-tested directly" convention, since
//! everything they compose is tested standalone.

use std::collections::HashSet;

use anyhow::Result;

use crate::paths::Paths;
use crate::toggle;
use crate::x_api::XClient;

/// Every post id currently recorded as reposted — see [`toggle::load_all`].
pub(crate) fn load_all(paths: &Paths) -> Result<HashSet<String>> {
    toggle::load_all(&paths.reposted_posts_file())
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
/// [`toggle::ToggleState`] carry this function's actual test coverage.
pub(crate) fn create(
    paths: &Paths,
    client: &XClient,
    user_id: &str,
    post_id: &str,
    now: i64,
) -> Result<bool> {
    let path = paths.reposted_posts_file();
    match client.create_repost(paths, user_id, post_id, now) {
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

/// Un-repost `post_id` as `user_id` (#15) — mirrors [`create`] exactly, the
/// other direction.
pub(crate) fn remove(
    paths: &Paths,
    client: &XClient,
    user_id: &str,
    post_id: &str,
    now: i64,
) -> Result<bool> {
    let path = paths.reposted_posts_file();
    match client.delete_repost(paths, user_id, post_id, now) {
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

    #[test]
    fn load_all_reads_the_reposted_record_under_the_state_dir() {
        // The one thing this module still owns about the file: *which*
        // file. The behaviour of reading it is `toggle`'s, tested there.
        let root = std::env::temp_dir().join(format!("twigpui-test-repost-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.display().to_string();
        let paths = Paths::from_vars(move |key| (key == "HOME").then(|| home.clone())).unwrap();
        paths.ensure_dirs().unwrap();

        toggle::mark(&paths.reposted_posts_file(), "1").unwrap();
        assert!(load_all(&paths).unwrap().contains("1"));

        std::fs::remove_dir_all(&root).unwrap();
    }
}
