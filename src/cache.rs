//! Local JSON cache for X API responses (#9).
//!
//! Cuts API spend: startup renders straight from cache with no request at
//! all (see [`startup`]), and an explicit reload spends one request instead
//! of two once a user's id is cached (see [`reload`]). Mirrors
//! `oauth::tokens`'s injected-`now` seam — TTL and merge logic never read
//! the real clock or touch the filesystem themselves, so they're testable in
//! isolation; only the thin `cached_*` / `save_*` wrappers below touch disk.
//! [`reload`] is the one function that also touches the network (via
//! `XClient`), so unlike the rest of this module it is not unit tested — see
//! its own doc comment.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context as _, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::paths::Paths;
use crate::thread::{self, ThreadChain};
use crate::x_api::{TimelineItem, XClient};

/// How long a cached screen-name → user-id mapping stays usable before a
/// reload re-resolves it via the API. User ids are effectively permanent, so
/// this is generous — the whole point is to turn a reload's two requests
/// into one for as long as possible.
const USER_ID_TTL_SECONDS: i64 = 30 * 24 * 60 * 60;

/// Per-user cap on how many cached posts are kept, oldest dropped first.
/// `~/.cache` is not purged automatically by macOS the way `~/Library/Caches`
/// is, so without this an actively reloaded user's cache would grow forever.
///
/// `ui.rs` reads this too: at the cap, [`append_older`] would throw away
/// everything a "Load older" request bought, so the button has to be
/// withheld rather than left to spend credits for nothing.
pub(crate) const MAX_CACHED_POSTS: usize = 500;

/// One cached screen-name → user-id mapping.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct UserIdEntry {
    id: String,
    cached_at: i64,
}

/// The cached result of `GET /2/users/me` (#11): the signed-in user's own id
/// and screen name. Reuses the same TTL policy as [`UserIdEntry`] (ids are
/// effectively permanent) via [`user_id_is_fresh`], but is not stored in
/// [`UserIdCacheFile`]'s `username → id` map — that map only goes one
/// direction, and "who is the signed-in account" needs the reverse: this
/// value is discovered from `/me` itself, not looked up by a screen name the
/// caller already knew. A parallel single-entry file
/// ([`Paths::me_file`]) is the simplest fit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MeEntry {
    pub id: String,
    pub username: String,
    cached_at: i64,
}

/// The whole contents of [`Paths::user_ids_file`]: every screen name
/// resolved so far, keyed exactly as configured
/// (`Config::target_username`, already trimmed and `@`-stripped).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct UserIdCacheFile {
    #[serde(default)]
    users: HashMap<String, UserIdEntry>,
}

/// The whole contents of one [`Paths::timeline_file`].
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TimelineCacheFile {
    fetched_at: i64,
    items: Vec<TimelineItem>,
}

/// Whether a user id cached at `cached_at` is still within the TTL window at
/// `now`.
fn user_id_is_fresh(cached_at: i64, now: i64) -> bool {
    now.saturating_sub(cached_at) < USER_ID_TTL_SECONDS
}

/// Load and parse `path` as JSON. Distinguishes three outcomes: the file
/// doesn't exist (`Ok(None)`, same as `oauth::tokens::load`'s missing-file
/// case), it exists but fails to parse — corruption, or a shape from a
/// future or old version — which is *also* `Ok(None)` rather than an error,
/// and a genuine I/O error (permissions, etc.), which propagates. The whole
/// point of the cache is saving money, so a broken cache file must never
/// stop the app from starting; it just gets silently rebuilt on the next
/// write.
fn load_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("could not read {}", path.display()));
        }
    };
    Ok(serde_json::from_str(&contents).ok())
}

fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let json = serde_json::to_vec_pretty(value)
        .with_context(|| format!("could not serialize {}", path.display()))?;
    std::fs::write(path, json).with_context(|| format!("could not write {}", path.display()))
}

/// The cached user id for `username`, if one is on file and still fresh.
/// `None` means "resolve it via the API and cache it" — either there was
/// nothing cached, the file was unreadable/corrupt, or the TTL lapsed.
pub(crate) fn cached_user_id(paths: &Paths, username: &str, now: i64) -> Result<Option<String>> {
    let file: UserIdCacheFile = load_json(&paths.user_ids_file())?.unwrap_or_default();
    Ok(file
        .users
        .get(username)
        .filter(|entry| user_id_is_fresh(entry.cached_at, now))
        .map(|entry| entry.id.clone()))
}

/// Persist `username`'s resolved id, alongside whatever other screen names
/// were already cached.
pub(crate) fn save_user_id(paths: &Paths, username: &str, user_id: &str, now: i64) -> Result<()> {
    let path = paths.user_ids_file();
    let mut file: UserIdCacheFile = load_json(&path)?.unwrap_or_default();
    file.users.insert(
        username.to_string(),
        UserIdEntry {
            id: user_id.to_string(),
            cached_at: now,
        },
    );
    save_json(&path, &file)
}

/// The cached `/me` result, if one is on file and still fresh. `None` means
/// "resolve it via the API and cache it" — the same contract as
/// [`cached_user_id`].
pub(crate) fn cached_me(paths: &Paths, now: i64) -> Result<Option<MeEntry>> {
    let entry: Option<MeEntry> = load_json(&paths.me_file())?;
    Ok(entry.filter(|entry| user_id_is_fresh(entry.cached_at, now)))
}

/// Persist the signed-in user's id and screen name from `/me`.
pub(crate) fn save_me(paths: &Paths, id: &str, username: &str, now: i64) -> Result<()> {
    let entry = MeEntry {
        id: id.to_string(),
        username: username.to_string(),
        cached_at: now,
    };
    save_json(&paths.me_file(), &entry)
}

/// The cached timeline for `user_id`, newest-first, or `None` if there is
/// nothing usable cached (missing or corrupt file). Unlike the user-id
/// cache, there is no TTL here — staleness is bounded by an explicit
/// reload, never by age alone, matching the issue's "render from cache,
/// only an explicit reload spends credits" decision.
pub(crate) fn load_timeline(paths: &Paths, user_id: &str) -> Result<Option<Vec<TimelineItem>>> {
    let file: Option<TimelineCacheFile> = load_json(&paths.timeline_file(user_id))?;
    Ok(file.map(|file| file.items))
}

/// Persist `items` (already merged and capped by the caller) as `user_id`'s
/// timeline cache.
pub(crate) fn save_timeline(
    paths: &Paths,
    user_id: &str,
    items: &[TimelineItem],
    now: i64,
) -> Result<()> {
    let file = TimelineCacheFile {
        fetched_at: now,
        items: items.to_vec(),
    };
    save_json(&paths.timeline_file(user_id), &file)
}

/// The cached home timeline for `user_id`, newest-first, or `None` if there
/// is nothing usable cached. Mirrors [`load_timeline`] exactly, but reads
/// [`Paths::home_timeline_file`] — a distinct file, so a single-user
/// timeline cached for the same id is never read back as home-timeline
/// content or vice versa (#11).
pub(crate) fn load_home_timeline(
    paths: &Paths,
    user_id: &str,
) -> Result<Option<Vec<TimelineItem>>> {
    let file: Option<TimelineCacheFile> = load_json(&paths.home_timeline_file(user_id))?;
    Ok(file.map(|file| file.items))
}

/// Persist `items` as `user_id`'s home-timeline cache. Mirrors
/// [`save_timeline`], writing to [`Paths::home_timeline_file`] instead.
pub(crate) fn save_home_timeline(
    paths: &Paths,
    user_id: &str,
    items: &[TimelineItem],
    now: i64,
) -> Result<()> {
    let file = TimelineCacheFile {
        fetched_at: now,
        items: items.to_vec(),
    };
    save_json(&paths.home_timeline_file(user_id), &file)
}

/// Render the home timeline straight from cache: `Some` only when both `/me`
/// and a home timeline are already cached (and `/me` is still within its
/// TTL) — mirrors [`startup`], but for #11's home-timeline mode. Returns the
/// resolved [`MeEntry`] alongside the items so the caller (`ui.rs`) can
/// populate the header and the id needed for "Load older" even on a
/// cache-only render.
pub(crate) fn startup_home(
    paths: &Paths,
    now: i64,
) -> Result<Option<(MeEntry, Vec<TimelineItem>)>> {
    let Some(me) = cached_me(paths, now)? else {
        return Ok(None);
    };
    let Some(items) = load_home_timeline(paths, &me.id)? else {
        return Ok(None);
    };
    Ok(Some((me, items)))
}

/// Render straight from cache with no API request at all: `Some` only when
/// both the user id and a timeline are already cached (and the user id is
/// still within its TTL) — anything less and there's nothing trustworthy to
/// show, so the caller falls back to a full [`reload`].
pub(crate) fn startup(
    paths: &Paths,
    username: &str,
    now: i64,
) -> Result<Option<Vec<TimelineItem>>> {
    let Some(user_id) = cached_user_id(paths, username, now)? else {
        return Ok(None);
    };
    load_timeline(paths, &user_id)
}

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

/// Merge freshly fetched posts (newest-first) ahead of what's cached (also
/// newest-first): drop any id already present in `cached` — the API can
/// return a post already on file — then cap the combined, still
/// newest-first list to [`MAX_CACHED_POSTS`].
pub(crate) fn merge_timeline(
    fresh: Vec<TimelineItem>,
    cached: Vec<TimelineItem>,
) -> Vec<TimelineItem> {
    let cached_ids: HashSet<&str> = cached.iter().map(|item| item.id.as_str()).collect();
    let mut merged: Vec<TimelineItem> = fresh
        .into_iter()
        .filter(|item| !cached_ids.contains(item.id.as_str()))
        .collect();
    merged.extend(cached);
    merged.truncate(MAX_CACHED_POSTS);
    merged
}

/// Append freshly fetched *older* posts (also newest-first, from following
/// `meta.next_token` — #11's "Load older") after what's cached: the opposite
/// side from [`merge_timeline`], which puts a newer batch *ahead* of cached.
/// Putting an older batch there instead would silently invert the
/// newest-first invariant every other reader of the cache relies on. Drops
/// any id already present in `cached` — the API can return a post already on
/// file at the page boundary — then caps the combined, still newest-first
/// list to [`MAX_CACHED_POSTS`], same as [`merge_timeline`].
pub(crate) fn append_older(
    cached: Vec<TimelineItem>,
    older: Vec<TimelineItem>,
) -> Vec<TimelineItem> {
    let cached_ids: HashSet<&str> = cached.iter().map(|item| item.id.as_str()).collect();
    let filtered_older: Vec<TimelineItem> = older
        .into_iter()
        .filter(|item| !cached_ids.contains(item.id.as_str()))
        .collect();
    let mut merged = cached;
    merged.extend(filtered_older);
    merged.truncate(MAX_CACHED_POSTS);
    merged
}

/// What a reload spent: the merged, capped timeline to render, and whether
/// the user-id lookup was skipped because it was already cached (in which
/// case the reload cost one request instead of two).
#[derive(Debug)]
pub(crate) struct Reloaded {
    pub items: Vec<TimelineItem>,
    pub user_id_cache_hit: bool,
}

/// Spend the credits an explicit reload is allowed to spend: resolve the
/// user id (from cache if fresh, else one API request, then cached for next
/// time), fetch posts newer than the newest cached one, merge them ahead of
/// what's cached, persist the result, and return it.
///
/// Not unit-tested directly — it makes real HTTP requests through `client`.
/// Everything it composes ([`cached_user_id`], [`save_user_id`],
/// [`load_timeline`], [`since_id`], [`merge_timeline`], [`save_timeline`])
/// is tested standalone, the same way `oauth::resolve_credential`'s
/// network-calling refresh branch isn't directly tested either.
pub(crate) fn reload(
    paths: &Paths,
    client: &XClient,
    username: &str,
    max_results: u32,
    now: i64,
) -> Result<Reloaded> {
    let (user_id, user_id_cache_hit) = if let Some(id) = cached_user_id(paths, username, now)? {
        (id, true)
    } else {
        let id = client.user_id_by_username(paths, username, now)?;
        save_user_id(paths, username, &id, now)?;
        (id, false)
    };

    let cached = load_timeline(paths, &user_id)?.unwrap_or_default();
    let since = since_id(&cached);
    let fresh = client.timeline(paths, &user_id, max_results, since, now)?;
    let items = merge_timeline(fresh, cached);
    save_timeline(paths, &user_id, &items, now)?;
    Ok(Reloaded {
        items,
        user_id_cache_hit,
    })
}

/// What a home-timeline reload spent (#11): the merged, capped timeline to
/// render, the resolved [`MeEntry`] itself (so `ui.rs` can populate the
/// header and remember the id for a later "Load older"), and the response's
/// `meta.next_token`, if any.
///
/// Unlike [`Reloaded`], this carries no `me_cache_hit` flag: nothing in this
/// crate currently reports per-reload request cost for the home-timeline
/// path the way `main.rs`'s `--fetch-only` does for [`Reloaded`] via
/// `user_id_cache_hit`, so tracking it here would be dead weight. Add it back
/// if a caller needs it.
#[derive(Debug)]
pub(crate) struct ReloadedHome {
    pub items: Vec<TimelineItem>,
    pub me: MeEntry,
    pub next_token: Option<String>,
}

/// Spend the credits a home-timeline reload is allowed to spend: resolve
/// `/me` (from cache if fresh, else one API request, then cached for next
/// time), fetch posts newer than the newest cached one, merge them ahead of
/// what's cached (never appended behind — that's [`load_older_home`]'s job),
/// persist the result, and return it alongside `meta.next_token`.
///
/// Mirrors [`reload`], the single-user equivalent. Not unit-tested directly
/// for the same reason `reload` isn't — it makes real HTTP requests through
/// `client`. Everything it composes is tested standalone.
pub(crate) fn reload_home(
    paths: &Paths,
    client: &XClient,
    max_results: u32,
    now: i64,
) -> Result<ReloadedHome> {
    let me = if let Some(entry) = cached_me(paths, now)? {
        entry
    } else {
        let user = client.me(paths, now)?;
        save_me(paths, &user.id, &user.username, now)?;
        MeEntry {
            id: user.id,
            username: user.username,
            cached_at: now,
        }
    };

    let cached = load_home_timeline(paths, &me.id)?.unwrap_or_default();
    let since = since_id(&cached);
    let (fresh, next_token) = client.home_timeline(paths, &me.id, max_results, since, None, now)?;
    let items = merge_timeline(fresh, cached);
    save_home_timeline(paths, &me.id, &items, now)?;
    Ok(ReloadedHome {
        items,
        me,
        next_token,
    })
}

/// Spend one request to fetch the page *behind* `pagination_token` (#11's
/// "Load older"): append it after what's cached — via [`append_older`],
/// never [`merge_timeline`] — persist the combined result, and return it
/// alongside the next `meta.next_token` (`None` once there's nothing further
/// back). `user_id` is the caller's responsibility to supply — `ui.rs` keeps
/// it around from the last [`reload_home`] or [`startup_home`], since this
/// function has no reason to re-resolve `/me` just to page further back
/// through content it's already showing.
///
/// Not unit-tested directly, for the same reason [`reload_home`] isn't.
pub(crate) fn load_older_home(
    paths: &Paths,
    client: &XClient,
    user_id: &str,
    max_results: u32,
    pagination_token: &str,
    now: i64,
) -> Result<(Vec<TimelineItem>, Option<String>)> {
    let cached = load_home_timeline(paths, user_id)?.unwrap_or_default();
    let (older, next_token) = client.home_timeline(
        paths,
        user_id,
        max_results,
        None,
        Some(pagination_token),
        now,
    )?;
    let items = append_older(cached, older);
    save_home_timeline(paths, user_id, &items, now)?;
    Ok((items, next_token))
}

/// The whole contents of one [`Paths::thread_file`]: a cached parent chain
/// for one reply (#12).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ThreadCacheFile {
    fetched_at: i64,
    chain: ThreadChain,
}

/// The cached parent chain for `reply_post_id`, if one is on file (#12).
/// Unlike [`load_timeline`], there is no refresh path here at all — a
/// thread's parents are immutable once posted (aside from deletion, which
/// [`thread::assemble_chain`] already renders sensibly), so a cache hit is
/// trusted forever, matching [`load_timeline`]'s own "no TTL" rule for
/// materially the same reason.
pub(crate) fn load_thread(paths: &Paths, reply_post_id: &str) -> Result<Option<ThreadChain>> {
    let file: Option<ThreadCacheFile> = load_json(&paths.thread_file(reply_post_id))?;
    Ok(file.map(|file| file.chain))
}

/// Persist `chain` as `reply_post_id`'s cached parent chain (#12).
pub(crate) fn save_thread(
    paths: &Paths,
    reply_post_id: &str,
    chain: &ThreadChain,
    now: i64,
) -> Result<()> {
    let file = ThreadCacheFile {
        fetched_at: now,
        chain: chain.clone(),
    };
    save_json(&paths.thread_file(reply_post_id), &file)
}

/// Spend the credits "Show thread" (#12) is allowed to spend: if a chain for
/// `reply_post_id` is already cached, render it for free; otherwise walk
/// upward one `GET /2/tweets?ids=` request per level — starting from
/// `first_parent_id` (the reply's own `TimelineItem::replied_to.post_id`,
/// already known at zero request cost) — stopping at
/// [`thread::MAX_THREAD_DEPTH`] levels or the first missing/absent parent,
/// then cache and return the assembled result.
///
/// An empty result is deliberately *not* cached — see the comment at the
/// bottom of the body.
///
/// The loop below checks the depth cap *before* each fetch, never after, so
/// the worst case is exactly [`thread::MAX_THREAD_DEPTH`] requests: hitting
/// the cap is detected from data already in hand (the last fetched post's
/// own `replied_to`), never by spending one more request to find out.
///
/// Not unit-tested directly — it makes real HTTP requests through `client`,
/// the same way [`reload`] isn't. The pure seam that carries this
/// function's ordering/dedup/cap logic is [`thread::assemble_chain`];
/// [`load_thread`]/[`save_thread`] are tested standalone like the rest of
/// this module's cache accessors.
pub(crate) fn fetch_thread(
    paths: &Paths,
    client: &XClient,
    reply_post_id: &str,
    first_parent_id: &str,
    now: i64,
) -> Result<ThreadChain> {
    if let Some(cached) = load_thread(paths, reply_post_id)? {
        return Ok(cached);
    }

    let mut hops: Vec<thread::ThreadItem> = Vec::new();
    let mut next_id = Some(first_parent_id.to_string());
    let mut reached_cap = false;

    while let Some(id) = next_id.take() {
        if hops.len() >= thread::MAX_THREAD_DEPTH {
            // A further parent is known (`id`) but the cap was already hit
            // by the previous iteration — stop without spending a request
            // to confirm what's already known.
            reached_cap = true;
            break;
        }

        let items = client.tweets_by_id(paths, &id, now)?;
        let Some(fetched) = items.into_iter().next() else {
            // Deleted, protected, or otherwise absent from the response —
            // the walk stops cleanly here rather than erroring (#12).
            break;
        };

        next_id = fetched.replied_to.as_ref().map(|r| r.post_id.clone());
        hops.push(thread::ThreadItem {
            id: fetched.id,
            text: fetched.text,
            author_name: fetched.author_name,
            author_username: fetched.author_username,
        });
    }

    let chain = thread::assemble_chain(hops, reached_cap);
    // An empty chain means the very first parent came back absent. That is
    // usually permanent (deleted, protected), but it is also what a transient
    // hiccup looks like — and this cache has no TTL, so persisting it would
    // wedge "Show thread" on this reply forever with no way out but deleting
    // the file by hand. Re-deriving it costs exactly one request, on an
    // explicit click; that is the cheaper mistake to make.
    if !chain.items.is_empty() {
        save_thread(paths, reply_post_id, &chain, now)?;
    }
    Ok(chain)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str) -> TimelineItem {
        TimelineItem {
            id: id.to_string(),
            text: String::new(),
            created_at: None,
            author_name: String::new(),
            author_username: String::new(),
            reposted_by: None,
            quoted: None,
            replied_to: None,
        }
    }

    fn ids(items: &[TimelineItem]) -> Vec<&str> {
        items.iter().map(|item| item.id.as_str()).collect()
    }

    fn test_paths(root: &Path) -> Paths {
        let home = root.display().to_string();
        Paths::from_vars(move |key| (key == "HOME").then(|| home.clone())).unwrap()
    }

    fn temp_root(label: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("twigpui-test-cache-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    // --- user_id_is_fresh ---

    #[test]
    fn user_id_is_fresh_just_inside_the_ttl_window() {
        assert!(user_id_is_fresh(0, USER_ID_TTL_SECONDS - 1));
    }

    #[test]
    fn user_id_is_stale_once_the_ttl_has_fully_elapsed() {
        assert!(!user_id_is_fresh(0, USER_ID_TTL_SECONDS));
    }

    // --- since_id ---

    #[test]
    fn since_id_is_none_for_an_empty_cache() {
        assert_eq!(since_id(&[]), None);
    }

    #[test]
    fn since_id_is_the_first_and_therefore_newest_cached_post() {
        let cached = vec![item("300"), item("200"), item("100")];
        assert_eq!(since_id(&cached), Some("300"));
    }

    // --- merge_timeline ---

    #[test]
    fn merge_places_fresh_posts_ahead_of_cached_posts() {
        let fresh = vec![item("3"), item("2")];
        let cached = vec![item("1")];
        let merged = merge_timeline(fresh, cached);
        assert_eq!(ids(&merged), vec!["3", "2", "1"]);
    }

    #[test]
    fn merge_drops_a_fresh_post_whose_id_is_already_cached() {
        // The API can hand back a post that's already on file; the cached
        // copy stays put rather than being duplicated.
        let fresh = vec![item("3"), item("2")];
        let cached = vec![item("2"), item("1")];
        let merged = merge_timeline(fresh, cached);
        assert_eq!(ids(&merged), vec!["3", "2", "1"]);
    }

    #[test]
    fn merge_keeps_the_result_ordered_newest_first() {
        let fresh = vec![item("6"), item("5"), item("4")];
        let cached = vec![item("3"), item("2"), item("1")];
        let merged = merge_timeline(fresh, cached);
        assert_eq!(ids(&merged), vec!["6", "5", "4", "3", "2", "1"]);
    }

    #[test]
    fn merge_truncates_to_the_500_post_cap() {
        let fresh = vec![item("502"), item("501")];
        let cached: Vec<_> = (1..=500).rev().map(|n| item(&n.to_string())).collect();
        let merged = merge_timeline(fresh, cached);
        assert_eq!(merged.len(), 500);
        assert_eq!(merged.first().unwrap().id, "502");
        // The two oldest cached posts ("2" and "1") were pushed out by the cap.
        assert!(!ids(&merged).contains(&"1"));
        assert!(!ids(&merged).contains(&"2"));
        assert_eq!(merged.last().unwrap().id, "3");
    }

    // --- append_older ---

    #[test]
    fn append_older_places_older_posts_behind_cached_posts() {
        let cached = vec![item("3"), item("2")];
        let older = vec![item("1")];
        let merged = append_older(cached, older);
        assert_eq!(ids(&merged), vec!["3", "2", "1"]);
    }

    #[test]
    fn append_older_drops_an_older_post_whose_id_is_already_cached() {
        // The page boundary can overlap: the API can hand back a post
        // that's already on file, and it must not be duplicated.
        let cached = vec![item("3"), item("2")];
        let older = vec![item("2"), item("1")];
        let merged = append_older(cached, older);
        assert_eq!(ids(&merged), vec!["3", "2", "1"]);
    }

    #[test]
    fn append_older_keeps_the_result_ordered_newest_first() {
        let cached = vec![item("6"), item("5"), item("4")];
        let older = vec![item("3"), item("2"), item("1")];
        let merged = append_older(cached, older);
        assert_eq!(ids(&merged), vec!["6", "5", "4", "3", "2", "1"]);
    }

    #[test]
    fn append_older_truncates_to_the_500_post_cap() {
        let cached: Vec<_> = (3..=502).rev().map(|n| item(&n.to_string())).collect();
        let older = vec![item("2"), item("1")];
        let merged = append_older(cached, older);
        assert_eq!(merged.len(), 500);
        assert_eq!(merged.first().unwrap().id, "502");
        // The two oldest fetched posts ("2" and "1") are pushed out by the cap.
        assert!(!ids(&merged).contains(&"1"));
        assert!(!ids(&merged).contains(&"2"));
        assert_eq!(merged.last().unwrap().id, "3");
    }

    // --- cached_me / save_me ---

    #[test]
    fn cached_me_is_none_when_the_file_is_missing() {
        let root = temp_root("me-missing");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        assert_eq!(cached_me(&paths, 0).unwrap(), None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn save_me_then_cached_me_roundtrips_while_fresh() {
        let root = temp_root("me-roundtrip");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        save_me(&paths, "2244994945", "alice", 1_000).unwrap();
        let me = cached_me(&paths, 1_000 + USER_ID_TTL_SECONDS - 1)
            .unwrap()
            .unwrap();
        assert_eq!(me.id, "2244994945");
        assert_eq!(me.username, "alice");

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn cached_me_is_none_once_the_ttl_has_elapsed() {
        // #11 reuses #9's TTL policy: an id is effectively permanent, but
        // this still guards against a cache file from an account that has
        // since been deleted or renamed staying trusted forever.
        let root = temp_root("me-stale");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        save_me(&paths, "2244994945", "alice", 0).unwrap();
        let me = cached_me(&paths, USER_ID_TTL_SECONDS).unwrap();
        assert_eq!(me, None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_corrupted_me_cache_file_is_a_clean_miss_not_an_error() {
        let root = temp_root("me-corrupt");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.me_file(), b"not json at all").unwrap();

        assert_eq!(cached_me(&paths, 0).unwrap(), None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- cached_user_id / save_user_id ---

    #[test]
    fn cached_user_id_is_none_when_the_file_is_missing() {
        let root = temp_root("user-id-missing");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        assert_eq!(cached_user_id(&paths, "XDevelopers", 0).unwrap(), None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn save_user_id_then_cached_user_id_roundtrips_while_fresh() {
        let root = temp_root("user-id-roundtrip");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        save_user_id(&paths, "XDevelopers", "2244994945", 1_000).unwrap();
        let id = cached_user_id(&paths, "XDevelopers", 1_000 + USER_ID_TTL_SECONDS - 1).unwrap();
        assert_eq!(id.as_deref(), Some("2244994945"));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn cached_user_id_is_none_once_the_ttl_has_elapsed() {
        let root = temp_root("user-id-stale");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        save_user_id(&paths, "XDevelopers", "2244994945", 0).unwrap();
        let id = cached_user_id(&paths, "XDevelopers", USER_ID_TTL_SECONDS).unwrap();
        assert_eq!(id, None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn save_user_id_preserves_other_already_cached_screen_names() {
        let root = temp_root("user-id-multi");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        save_user_id(&paths, "alice", "1", 0).unwrap();
        save_user_id(&paths, "bob", "2", 0).unwrap();

        assert_eq!(
            cached_user_id(&paths, "alice", 0).unwrap().as_deref(),
            Some("1")
        );
        assert_eq!(
            cached_user_id(&paths, "bob", 0).unwrap().as_deref(),
            Some("2")
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_corrupted_user_id_cache_file_is_a_clean_miss_not_an_error() {
        let root = temp_root("user-id-corrupt");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.user_ids_file(), b"not json at all").unwrap();

        let id = cached_user_id(&paths, "XDevelopers", 0).unwrap();
        assert_eq!(id, None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_genuine_io_error_reading_the_user_id_cache_still_propagates() {
        let root = temp_root("user-id-io-error");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        // A directory where a file is expected is a real I/O error (not
        // NotFound), distinct from corruption — it must surface rather than
        // being swallowed as a cache miss.
        std::fs::create_dir(paths.user_ids_file()).unwrap();

        assert!(cached_user_id(&paths, "XDevelopers", 0).is_err());

        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- load_timeline / save_timeline ---

    #[test]
    fn load_timeline_is_none_when_the_file_is_missing() {
        let root = temp_root("timeline-missing");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        assert_eq!(load_timeline(&paths, "2244994945").unwrap(), None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn save_timeline_then_load_timeline_roundtrips() {
        let root = temp_root("timeline-roundtrip");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let items = vec![item("2"), item("1")];
        save_timeline(&paths, "2244994945", &items, 1_000).unwrap();
        let loaded = load_timeline(&paths, "2244994945").unwrap();
        assert_eq!(loaded, Some(items));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_corrupted_timeline_cache_file_is_a_clean_miss_not_an_error() {
        let root = temp_root("timeline-corrupt");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.timeline_file("2244994945"), b"{ not valid json").unwrap();

        assert_eq!(load_timeline(&paths, "2244994945").unwrap(), None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_timeline_cache_file_from_a_future_shape_is_a_clean_miss_not_an_error() {
        let root = temp_root("timeline-future-shape");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        // Valid JSON, but not the shape this version expects — simulates a
        // cache file written by a future version of twigpui.
        std::fs::write(
            paths.timeline_file("2244994945"),
            br#"{"schema_version": 99, "wildly_different_shape": true}"#,
        )
        .unwrap();

        assert_eq!(load_timeline(&paths, "2244994945").unwrap(), None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- load_home_timeline / save_home_timeline ---

    #[test]
    fn load_home_timeline_is_none_when_the_file_is_missing() {
        let root = temp_root("home-timeline-missing");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        assert_eq!(load_home_timeline(&paths, "2244994945").unwrap(), None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn save_home_timeline_then_load_home_timeline_roundtrips() {
        let root = temp_root("home-timeline-roundtrip");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let items = vec![item("2"), item("1")];
        save_home_timeline(&paths, "2244994945", &items, 1_000).unwrap();
        let loaded = load_home_timeline(&paths, "2244994945").unwrap();
        assert_eq!(loaded, Some(items));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn single_user_and_home_timeline_caches_for_the_same_user_id_do_not_collide() {
        // #11's whole point: the same user id (e.g. someone reloading in
        // single-user mode, then signing in and reloading their home
        // timeline) must not have one mode's cache overwrite the other's.
        let root = temp_root("no-collision");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        save_timeline(&paths, "123", &[item("single-user-post")], 0).unwrap();
        save_home_timeline(&paths, "123", &[item("home-timeline-post")], 0).unwrap();

        assert_eq!(
            load_timeline(&paths, "123").unwrap().unwrap()[0].id,
            "single-user-post"
        );
        assert_eq!(
            load_home_timeline(&paths, "123").unwrap().unwrap()[0].id,
            "home-timeline-post"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- startup_home ---

    #[test]
    fn startup_home_renders_from_cache_when_both_me_and_the_timeline_are_cached() {
        let root = temp_root("startup-home-hit");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        save_me(&paths, "2244994945", "alice", 0).unwrap();
        let items = vec![item("2"), item("1")];
        save_home_timeline(&paths, "2244994945", &items, 0).unwrap();

        let rendered = startup_home(&paths, 0).unwrap();
        let (me, rendered_items) = rendered.unwrap();
        assert_eq!(me.id, "2244994945");
        assert_eq!(me.username, "alice");
        assert_eq!(rendered_items, items);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn startup_home_is_none_when_me_is_not_cached() {
        let root = temp_root("startup-home-no-me");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        assert!(startup_home(&paths, 0).unwrap().is_none());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn startup_home_is_none_when_me_is_cached_but_the_timeline_is_not() {
        let root = temp_root("startup-home-no-timeline");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        save_me(&paths, "2244994945", "alice", 0).unwrap();

        assert!(startup_home(&paths, 0).unwrap().is_none());

        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- load_thread / save_thread ---

    #[test]
    fn load_thread_is_none_when_the_file_is_missing() {
        let root = temp_root("thread-missing");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        assert_eq!(load_thread(&paths, "1800000000000000003").unwrap(), None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn save_thread_then_load_thread_roundtrips() {
        let root = temp_root("thread-roundtrip");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let chain = ThreadChain {
            items: vec![thread::ThreadItem {
                id: "1700000000000000001".to_string(),
                text: "hello from the timeline".to_string(),
                author_name: "Developers".to_string(),
                author_username: "XDevelopers".to_string(),
            }],
            capped: false,
        };
        save_thread(&paths, "1800000000000000003", &chain, 1_000).unwrap();
        let loaded = load_thread(&paths, "1800000000000000003").unwrap();
        assert_eq!(loaded, Some(chain));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_corrupted_thread_cache_file_is_a_clean_miss_not_an_error() {
        let root = temp_root("thread-corrupt");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.thread_file("1800000000000000003"), b"not json at all").unwrap();

        assert_eq!(load_thread(&paths, "1800000000000000003").unwrap(), None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- startup ---

    #[test]
    fn startup_renders_from_cache_when_both_the_user_id_and_timeline_are_cached() {
        let root = temp_root("startup-hit");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        save_user_id(&paths, "XDevelopers", "2244994945", 0).unwrap();
        let items = vec![item("2"), item("1")];
        save_timeline(&paths, "2244994945", &items, 0).unwrap();

        let rendered = startup(&paths, "XDevelopers", 0).unwrap();
        assert_eq!(rendered, Some(items));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn startup_is_none_when_the_user_id_is_not_cached() {
        let root = temp_root("startup-no-user-id");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        assert_eq!(startup(&paths, "XDevelopers", 0).unwrap(), None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn startup_is_none_when_the_user_id_is_cached_but_the_timeline_is_not() {
        let root = temp_root("startup-no-timeline");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        save_user_id(&paths, "XDevelopers", "2244994945", 0).unwrap();

        assert_eq!(startup(&paths, "XDevelopers", 0).unwrap(), None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn startup_is_none_when_the_cached_user_id_has_gone_stale() {
        let root = temp_root("startup-stale-user-id");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        save_user_id(&paths, "XDevelopers", "2244994945", 0).unwrap();
        save_timeline(&paths, "2244994945", &[item("1")], 0).unwrap();

        let rendered = startup(&paths, "XDevelopers", USER_ID_TTL_SECONDS).unwrap();
        assert_eq!(rendered, None);

        std::fs::remove_dir_all(&root).unwrap();
    }
}
