//! Merging a freshly fetched batch into what is already cached, and
//! keeping the result in time order.
//!
//! Split out of `cache` (#117), which had raised its ceiling twice in two
//! pull requests -- 600 to 700 for #97's schema version and field merge,
//! 700 to 800 for #102's ordering pass. Both landed here, which is what
//! made this the piece worth separating.
//!
//! Everything in this file is pure: it takes the cached rows and the
//! incoming ones and returns what should be on file, touching neither the
//! disk nor the network. That is why nearly every test `cache` has lives
//! against these functions. Reading and writing the files, and the
//! functions that spend API requests to fill them, stay in the parent.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use crate::x_api::TimelineItem;

use super::MAX_CACHED_POSTS;

/// The id of the newest cached post, to pass as `since_id` on the next
/// fetch — the first element, since every cache file is stored newest-first.
///
/// X post ids are snowflake-style numeric strings that exceed `u32`, so they
/// stay `String` throughout; nothing here parses one into an integer, and
/// ordering always comes from the API's own response order rather than a
/// lexicographic string comparison (which breaks across digit-count
/// boundaries, e.g. `"9" > "10"`).
pub(crate) fn since_id(cached: &[TimelineItem]) -> Option<&str> {
    cached.first().map(|item| item.id.as_str())
}

/// Which side of what's already cached an incoming batch belongs on (#92).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Side {
    /// A newer batch, from a `since_id` reload — it goes in front.
    Ahead,
    /// An older batch, from following `meta.next_token` (#11's "Load
    /// older") — it goes behind. Putting one in front instead would
    /// silently invert the newest-first invariant every other reader of
    /// the cache relies on.
    Behind,
}

/// Fill whichever of `cached`'s optional/collection fields are empty with
/// `incoming`'s value for that same field, leaving every field `cached`
/// already has untouched (#97).
///
/// One rule, applied identically to every mergeable field rather than one
/// branch per field: **cached `Some`/non-empty wins; incoming fills only
/// what cached is missing.** That is exactly what a row saved before a
/// field existed needs (`author_avatar_url`/`media`/`links` before
/// #64/#65/#70), and it is also exactly what #67's `metrics` snapshot
/// needs — a cached `Some` metrics is never displaced by a fresher one, so
/// there is no metrics-specific branch here. Any field added to
/// [`TimelineItem`] in the future defaults to this same "fill only when
/// missing" behavior once it is added below, which is the safe side to
/// default to.
///
/// `id`/`text`/`author_name`/`author_username` are not `Option`, since the
/// parser always fills them from the API response, so `cached`'s value for
/// those (and every other field not listed below) is kept as-is.
fn merge_item(mut cached: TimelineItem, incoming: &TimelineItem) -> TimelineItem {
    if cached.created_at.is_none() {
        cached.created_at.clone_from(&incoming.created_at);
    }
    if cached.reposted_by.is_none() {
        cached.reposted_by.clone_from(&incoming.reposted_by);
    }
    if cached.quoted.is_none() {
        cached.quoted.clone_from(&incoming.quoted);
    }
    if cached.replied_to.is_none() {
        cached.replied_to.clone_from(&incoming.replied_to);
    }
    if cached.metrics.is_none() {
        cached.metrics = incoming.metrics;
    }
    if cached.links.is_empty() {
        cached.links.clone_from(&incoming.links);
    }
    if cached.author_avatar_url.is_none() {
        cached
            .author_avatar_url
            .clone_from(&incoming.author_avatar_url);
    }
    if cached.original_post_id.is_none() {
        cached
            .original_post_id
            .clone_from(&incoming.original_post_id);
    }
    if cached.media.is_empty() {
        cached.media.clone_from(&incoming.media);
    }
    cached
}

/// Sort `items` into `created_at` descending order (newest first), stable,
/// with rows that have no `created_at` pushed to the end (#102).
///
/// `created_at` is not parsed into a date type here, on purpose. Every
/// timeline endpoint requests it via `tweet.fields=created_at`
/// (`x_api::client`, the three `.../tweet.fields=created_at...` query
/// strings), and the API always renders it as a fixed-width, UTC-only RFC
/// 3339 timestamp: `YYYY-MM-DDTHH:MM:SS.mmmZ`. Fixed width and a single
/// timezone are exactly what [`since_id`]'s doc comment warns post *ids*
/// lack — ids grow in digit count over time, so a lexicographic compare of
/// two ids breaks at each digit-count boundary (`"9" > "10"` as strings,
/// even though id `10` was issued later). `created_at` has no such
/// boundary: year, month, day, hour, minute, second, and millisecond are
/// each zero-padded to a fixed width, so a byte-wise string comparison of
/// two `created_at` values agrees with their chronological order. That is
/// the whole justification for comparing the raw strings below instead of
/// pulling in a date-parsing crate — it is correct here in a way it would
/// not be for `id`.
///
/// The sort is stable ([`slice::sort_by`], not `sort_unstable_by`) rather
/// than merely convenient: two posts sharing the same `created_at` down to
/// the millisecond are not impossible, and when that happens this keeps
/// them in whatever relative order the caller already had — which, for a
/// freshly fetched page, is the API's own response order.
///
/// Rows without a `created_at` sort after every row that has one, rather
/// than joining the string comparison — `None` means "unknown", not
/// "oldest". In practice this is rare: `created_at` is populated on every
/// timeline response, and `None` only shows up on rows written to the
/// cache before the field existed (#97) or if a response ever came back
/// malformed. Sinking those rows to the end rather than raising them to the
/// front is the lower-harm default: a post landing a few slots deep in a
/// mixed-vintage cache is far less visible than an unrelated post jumping
/// to the very top of a newest-first feed.
///
/// This does interact with [`since_id`], which reports `cached.first()` as
/// the newest post to resume from. If a brand-new row with `created_at:
/// None` is ever spliced in, sinking it to the end here means `since_id`
/// reports an *older* post as the newest cached one, so the next reload
/// asks the API for posts since that older id — a range that already
/// includes this row. That re-fetch is harmless **only because `splice`
/// merges the incoming batch by id (#97) instead of blindly concatenating
/// it**: the row comes back from the API, is recognized as already cached,
/// and is folded into place via [`merge_item`] rather than appended a
/// second time. The cost is one wasted request, not a duplicate row. This
/// sort function does not provide that safety net itself — it is entirely
/// a property of `splice`'s id-based merge — so if that merge step is ever
/// weakened or removed, a `None`-`created_at` row sinking below `since_id`
/// stops being a free re-fetch and starts being a silent duplicate.
fn sort_by_created_at_desc(items: &mut [TimelineItem]) {
    items.sort_by(|a, b| match (&a.created_at, &b.created_at) {
        (Some(a_created_at), Some(b_created_at)) => b_created_at.cmp(a_created_at),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    });
}

/// Splice `incoming` into `cached` on the given side, re-sort the result by
/// `created_at` descending (#102, via [`sort_by_created_at_desc`]), and cap
/// it to [`MAX_CACHED_POSTS`].
///
/// The concatenation order below (fresh-then-cached for [`Side::Ahead`],
/// cached-then-fresh for [`Side::Behind`]) is no longer what makes the
/// result newest-first by itself — that used to rest entirely on the API
/// returning each side's posts already in time order (#92's original
/// design). This function now asserts that ordering explicitly with a sort
/// pass rather than assuming it, so newest-first holds regardless of
/// fetch order: a `since_id` reload, a `Load older` page, and a future
/// insert-on-post path (#14) all converge to the same order.
///
/// Ids already in `cached` are dropped from `incoming` — the API returns a
/// post already on file both on an incremental reload and at a page
/// boundary. **The cached copy is the one kept**, in either direction, but
/// not verbatim: it is first passed through [`merge_item`], which fills in
/// whatever fields it is missing from the incoming copy (#97) — otherwise a
/// row cached before a field like `author_avatar_url` existed would keep
/// recurring at a page boundary or `since_id` overlap without ever picking
/// the field up, since neither `reload` nor `load_older_primary` re-fetches an
/// id already on file by any other path. This was true of both functions
/// this replaces before the merge existed (#92: they were the same
/// operation with the concatenation reversed, keeping the cached copy
/// as-is) — a post's `metrics` (#67) are a snapshot from when it was
/// fetched, so "keep what is on file" for a field already present is what
/// makes a reload leave existing rows' counts alone instead of shuffling
/// them; [`merge_item`]'s rule preserves that while also fixing #97.
pub(crate) fn splice(
    cached: Vec<TimelineItem>,
    incoming: Vec<TimelineItem>,
    side: Side,
) -> Vec<TimelineItem> {
    let incoming_by_id: HashMap<&str, &TimelineItem> = incoming
        .iter()
        .map(|item| (item.id.as_str(), item))
        .collect();
    let merged: Vec<TimelineItem> = cached
        .into_iter()
        .map(|item| match incoming_by_id.get(item.id.as_str()) {
            Some(fresh) => merge_item(item, fresh),
            None => item,
        })
        .collect();

    let cached_ids: HashSet<&str> = merged.iter().map(|item| item.id.as_str()).collect();
    let fresh: Vec<TimelineItem> = incoming
        .into_iter()
        .filter(|item| !cached_ids.contains(item.id.as_str()))
        .collect();

    // Both arms move rather than clone: whichever list goes first becomes
    // the buffer the other is appended to.
    let mut spliced = match side {
        Side::Ahead => {
            let mut ahead = fresh;
            ahead.extend(merged);
            ahead
        }
        Side::Behind => {
            let mut behind = merged;
            behind.extend(fresh);
            behind
        }
    };
    // Re-sort by created_at before capping (#102): sorting first and
    // truncating second is the only order that keeps the newest rows. Cap
    // then sort would let a stale, already-in-place ordering decide which
    // rows survive the cut, dropping a genuinely fresh row that merely
    // arrived past the tail of `spliced` while an older row upstream of it
    // survives.
    sort_by_created_at_desc(&mut spliced);
    spliced.truncate(MAX_CACHED_POSTS);
    spliced
}

/// Every item except `post_id` (#72), in the same order.
///
/// Pure, so the "what should be left" half of deleting a post is tested
/// without touching disk — [`forget_post`] is the part that reads and
/// writes files.
pub(crate) fn without_post(items: Vec<TimelineItem>, post_id: &str) -> Vec<TimelineItem> {
    items
        .into_iter()
        .filter(|item| item.id != post_id)
        .collect()
}
