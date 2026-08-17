//! Parent-chain assembly for "Show thread" (#12).
//!
//! Walking a reply's parents costs one `GET /2/tweets?ids=` request per
//! level (see `cache::fetch_thread`), so the walk itself only ever runs on
//! an explicit click, capped at [`MAX_THREAD_DEPTH`] levels. This module is
//! the pure half of that feature: given whatever posts the walk in
//! `cache::fetch_thread` managed to fetch (fewer than the cap when a parent
//! is missing, exactly the cap when it hit the ceiling), [`assemble_chain`]
//! orders them for display, guards against a cyclic response duplicating a
//! post, and decides whether the cap — rather than a natural end — is why
//! the walk stopped. None of this touches the network or disk; only
//! `cache::fetch_thread` and `x_api::client::XClient::tweets_by_id` do.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// How many parent levels "Show thread" will walk before stopping and
/// saying so, rather than spending an unbounded number of requests on a
/// long thread. One `GET /2/tweets?ids=` request per level, so this is also
/// the worst-case request count for a single click.
pub(crate) const MAX_THREAD_DEPTH: usize = 5;

/// One post in an assembled parent chain, already flattened with its author
/// the same way [`crate::x_api::TimelineItem`] is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ThreadItem {
    pub id: String,
    pub text: String,
    pub author_name: String,
    pub author_username: String,
}

/// The result of walking (or attempting to walk) a reply's parent chain
/// (#12): the ancestors found, oldest first (the root of the thread at
/// index 0, the reply's immediate parent last), and whether the walk
/// stopped because [`MAX_THREAD_DEPTH`] was reached — as opposed to running
/// out of parents naturally (the root of the conversation, or a missing
/// parent partway up).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ThreadChain {
    pub items: Vec<ThreadItem>,
    pub capped: bool,
}

/// Assemble a walked parent chain for display.
///
/// `hops` is in *walk order* — the reply's immediate parent first, its own
/// parent second, and so on upward — since that's the order `cache::fetch_thread`
/// discovers them in (each level's id is only known once the previous one
/// resolves). Display wants the opposite: the oldest ancestor at the top,
/// working down to the post immediately before the reply, so the result is
/// reversed here rather than by every caller.
///
/// `reached_cap` is the walker's own record of *why* it stopped: `true` only
/// when it stopped because [`MAX_THREAD_DEPTH`] was hit while a further
/// parent was still known to exist, never because a parent went missing.
/// Duplicate ids (an API response looping back on itself, which should never
/// happen but costs nothing to guard) are dropped, keeping only the first
/// occurrence in walk order; if that dropping ever leaves more than
/// [`MAX_THREAD_DEPTH`] entries the result is truncated and `capped` is
/// forced `true` regardless of what the caller passed, so the invariant
/// "never show more than the cap" holds even under a malformed input.
pub(crate) fn assemble_chain(hops: Vec<ThreadItem>, reached_cap: bool) -> ThreadChain {
    let mut seen: HashSet<String> = HashSet::new();
    let mut deduped: Vec<ThreadItem> = hops
        .into_iter()
        .filter(|item| seen.insert(item.id.clone()))
        .collect();

    let capped = reached_cap || deduped.len() > MAX_THREAD_DEPTH;
    deduped.truncate(MAX_THREAD_DEPTH);
    deduped.reverse();

    ThreadChain {
        items: deduped,
        capped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str) -> ThreadItem {
        ThreadItem {
            id: id.to_string(),
            text: format!("text of {id}"),
            author_name: format!("Author {id}"),
            author_username: format!("author{id}"),
        }
    }

    #[test]
    fn reverses_walk_order_to_oldest_first_for_display() {
        // Walk order: immediate parent first, its parent second, root last.
        let hops = vec![item("parent"), item("grandparent"), item("root")];
        let chain = assemble_chain(hops, false);
        assert_eq!(
            chain
                .items
                .iter()
                .map(|i| i.id.as_str())
                .collect::<Vec<_>>(),
            vec!["root", "grandparent", "parent"]
        );
        assert!(!chain.capped);
    }

    #[test]
    fn drops_a_duplicate_id_keeping_the_first_occurrence_in_walk_order() {
        // A cyclic response (should never happen, but must not be trusted
        // blindly) must not render the same post twice.
        let hops = vec![item("a"), item("b"), item("a")];
        let chain = assemble_chain(hops, false);
        assert_eq!(
            chain
                .items
                .iter()
                .map(|i| i.id.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "a"]
        );
    }

    #[test]
    fn reports_uncapped_when_the_walk_ended_naturally_at_exactly_five_levels() {
        let hops: Vec<ThreadItem> = (1..=MAX_THREAD_DEPTH)
            .map(|n| item(&n.to_string()))
            .collect();
        let chain = assemble_chain(hops, false);
        assert_eq!(chain.items.len(), MAX_THREAD_DEPTH);
        assert!(!chain.capped);
    }

    #[test]
    fn reports_capped_when_the_walker_stopped_at_the_depth_limit() {
        let hops: Vec<ThreadItem> = (1..=MAX_THREAD_DEPTH)
            .map(|n| item(&n.to_string()))
            .collect();
        let chain = assemble_chain(hops, true);
        assert_eq!(chain.items.len(), MAX_THREAD_DEPTH);
        assert!(chain.capped);
    }

    #[test]
    fn truncates_and_forces_capped_if_handed_more_than_the_depth_limit() {
        // Defensive: the walker's own loop should never produce more than
        // `MAX_THREAD_DEPTH` hops, but the invariant holds even if it did.
        let hops: Vec<ThreadItem> = (1..=MAX_THREAD_DEPTH + 2)
            .map(|n| item(&n.to_string()))
            .collect();
        let chain = assemble_chain(hops, false);
        assert_eq!(chain.items.len(), MAX_THREAD_DEPTH);
        assert!(chain.capped);
    }

    #[test]
    fn an_empty_walk_is_an_uncapped_empty_chain() {
        // The very first parent was missing (deleted/protected/absent) —
        // #12's "must render sensibly" case. Nothing was found, and it
        // wasn't because of the depth cap.
        let chain = assemble_chain(Vec::new(), false);
        assert_eq!(chain.items, Vec::new());
        assert!(!chain.capped);
    }

    #[test]
    fn a_partial_walk_stopped_by_a_missing_parent_is_not_reported_as_capped() {
        // Two levels resolved, then the third parent was missing — the walk
        // stops cleanly rather than erroring, and `capped` must not claim
        // the depth limit is why.
        let hops = vec![item("parent"), item("grandparent")];
        let chain = assemble_chain(hops, false);
        assert_eq!(chain.items.len(), 2);
        assert!(!chain.capped);
    }
}
