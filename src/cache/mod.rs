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
//!
//! ## Cached rows can outlive the code that wrote them
//!
//! A `since_id`/`pagination_token` walk only ever asks the API for posts
//! *outside* the cached range, so a row already on file is never
//! re-fetched. Change what a field holds — add one to `TimelineItem`, or
//! widen `expansions` the way #104 did for a repost's media — and every
//! row already cached keeps the old, emptier value indefinitely.
//!
//! Two things address that, and neither is automatic:
//!
//! - [`splice`] fills a cached row's missing fields from the incoming copy
//!   when the same id turns up again, which covers a page boundary or a
//!   `since_id` overlap but not the rows in between.
//! - **Deleting the cache files by hand covers the rest.** They live under
//!   `Paths::cache_dir`; removing them costs nothing but the one reload
//!   that was going to happen anyway, since an empty cache makes
//!   `since_id` return `None`.
//!
//! #97 automated the second half with a schema version stamped on write
//! and checked on read. It was removed again: for a single-user
//! development tool the constant was one more thing to remember to bump,
//! with the same failure mode as forgetting to delete the files, and it
//! discarded 500 rows of scrollback each time it fired.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

mod timeline;

// A child module, not a sibling (#117, following #126's split of `ui`):
// nothing here needs widening for `timeline` to see it, and the
// re-exports below keep every caller's path unchanged -- `cache::splice`
// stayed `cache::splice`.
pub(crate) use timeline::{Side, since_id, splice, without_post};

use crate::paths::Paths;
use crate::thread::{self, ThreadChain};
use crate::x_api::{ListSummary, TimelineItem, XClient};

/// How long a cached screen-name → user-id mapping stays usable before a
/// reload re-resolves it via the API. User ids are effectively permanent, so
/// this is generous — the whole point is to turn a reload's two requests
/// into one for as long as possible.
const USER_ID_TTL_SECONDS: i64 = 30 * 24 * 60 * 60;

/// Per-user cap on how many cached posts are kept, oldest dropped first.
/// `~/.cache` is not purged automatically by macOS the way `~/Library/Caches`
/// is, so without this an actively reloaded user's cache would grow forever.
///
/// `ui.rs` reads this too: at the cap, [`splice`] would throw away
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

/// The cached result of `GET /2/users/:id/owned_lists` (#164): every list
/// the signed-in account owns, in the order the API returned them, which
/// is the order the picker draws them in.
///
/// No TTL, unlike [`MeEntry`]. A user id never changes, so its cache can
/// expire on a clock; a list's name can change any day, and no clock
/// knows which. The picker's own refresh is the only thing that moves
/// this — the same rule [`load_timeline`] follows, where staleness is
/// bounded by an explicit reload rather than by age. `cached_at` is
/// recorded anyway so the file says when it was last believed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct OwnedListsEntry {
    lists: Vec<ListSummary>,
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

/// The lists the picker last fetched (#164), or `None` if it never has —
/// which is the picker's cue to offer the fetch rather than to spend it.
pub(crate) fn cached_owned_lists(paths: &Paths) -> Result<Option<Vec<ListSummary>>> {
    let entry: Option<OwnedListsEntry> = load_json(&paths.owned_lists_file())?;
    Ok(entry.map(|entry| entry.lists))
}

/// Persist the lists `GET /2/users/:id/owned_lists` returned (#164).
pub(crate) fn save_owned_lists(paths: &Paths, lists: &[ListSummary], now: i64) -> Result<()> {
    let entry = OwnedListsEntry {
        lists: lists.to_vec(),
        cached_at: now,
    };
    save_json(&paths.owned_lists_file(), &entry)
}

/// The cached timeline for `user_id`, newest-first, or `None` if there is
/// nothing usable cached (missing or corrupt file). Unlike the user-id
/// cache, there is no TTL here — staleness is bounded by an explicit
/// reload, never by age alone, matching the issue's "render from cache,
/// only an explicit reload spends credits" decision.
pub(crate) fn load_timeline(paths: &Paths, user_id: &str) -> Result<Option<Vec<TimelineItem>>> {
    load_timeline_file(&paths.timeline_file(user_id))
}

/// Read one timeline cache file, whichever of the two it is (#92).
fn load_timeline_file(path: &Path) -> Result<Option<Vec<TimelineItem>>> {
    let file: Option<TimelineCacheFile> = load_json(path)?;
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
    save_timeline_file(&paths.timeline_file(user_id), items, now)
}

/// Write one timeline cache file (#92) — [`load_timeline_file`]'s
/// counterpart.
fn save_timeline_file(path: &Path, items: &[TimelineItem], now: i64) -> Result<()> {
    let file = TimelineCacheFile {
        fetched_at: now,
        items: items.to_vec(),
    };
    save_json(path, &file)
}

/// Which timeline fills the window (#161).
///
/// The name is a revival: a `TimelineSource` existed until #33, when the
/// app-only bearer token went away and left nothing to branch on. #157 put
/// a branch back — `GET /2/users/:id/timelines/reverse_chronological`
/// stopped returning followed authors' posts for this account, and a List
/// is how a following-shaped feed is read at all now.
///
/// Two variants, deliberately. Choosing among several lists is #164 and
/// blending sources into one lane is #43; both want more than this, and
/// neither is served by guessing at the shape here first. The single-user
/// timeline (`--fetch-only`) is not a variant: it is fetched by
/// [`reload`], never shown in the window, and giving it one would mean
/// every match arm below carrying a case the window cannot reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TimelineSource {
    /// `GET /2/users/:id/timelines/reverse_chronological` (#11), cached per
    /// signed-in user id. What every launch shows with no list configured.
    Home,
    /// `GET /2/lists/:id/tweets` (#161), cached per list id.
    List(String),
}

impl TimelineSource {
    /// Which cache file this source's posts belong in.
    ///
    /// `user_id` is the signed-in account's own id, and only [`Self::Home`]
    /// uses it: a list's contents are the same whoever reads them, so
    /// keying its cache by the reader would write the same posts to a
    /// second file the moment a different account opened the same list.
    fn cache_file(&self, paths: &Paths, user_id: &str) -> PathBuf {
        match self {
            Self::Home => paths.home_timeline_file(user_id),
            Self::List(list_id) => paths.list_timeline_file(list_id),
        }
    }
}

/// The cached timeline for whichever source the window is showing (#161),
/// newest-first, or `None` if there is nothing usable cached. Mirrors
/// [`load_timeline`] exactly, but reads the file
/// [`TimelineSource::cache_file`] picks — a distinct file per source, so a
/// single-user timeline cached for the same id is never read back as home
/// content (#11), and a list's posts never land on top of the home
/// timeline (#161).
pub(crate) fn load_primary_timeline(
    paths: &Paths,
    source: &TimelineSource,
    user_id: &str,
) -> Result<Option<Vec<TimelineItem>>> {
    load_timeline_file(&source.cache_file(paths, user_id))
}

/// Persist `items` as `source`'s cache. Mirrors [`save_timeline`], writing
/// to whichever file [`TimelineSource::cache_file`] picks instead.
pub(crate) fn save_primary_timeline(
    paths: &Paths,
    source: &TimelineSource,
    user_id: &str,
    items: &[TimelineItem],
    now: i64,
) -> Result<()> {
    save_timeline_file(&source.cache_file(paths, user_id), items, now)
}

/// Render the window's timeline straight from cache: `Some` only when both
/// `/me` and `source`'s own timeline are already cached (and `/me` is still
/// within its TTL) — mirrors [`startup`], but for the window's primary
/// source (#11, extended to lists by #161). Returns the resolved
/// [`MeEntry`] alongside the items so the caller (`ui.rs`) can populate the
/// header and the id needed for "Load older" even on a cache-only render.
///
/// `/me` is required in list mode too, even though no list request needs
/// it: the header names the signed-in account, and liking or reposting from
/// a list row calls endpoints that take the signed-in id in their path.
pub(crate) fn startup_primary(
    paths: &Paths,
    source: &TimelineSource,
    now: i64,
) -> Result<Option<(MeEntry, Vec<TimelineItem>)>> {
    let Some(me) = cached_me(paths, now)? else {
        return Ok(None);
    };
    let Some(items) = load_primary_timeline(paths, source, &me.id)? else {
        return Ok(None);
    };
    Ok(Some((me, items)))
}

/// Drop `post_id` from the cached timeline on disk and return what is left
/// (#72).
///
/// Deleting a post from X but leaving it in the cache is the failure mode
/// the issue warns about: the row disappears until the next start, then
/// comes back — the app looking like it worked when it did not. So this
/// rewrites the file and then **reads it back**, returning what is actually
/// on disk rather than what was just written; a write that silently did
/// nothing shows up as the post still being present.
///
/// `source` selects which cache file to touch — the same post can sit in
/// the home timeline and in a list at once, and only the one being
/// displayed is what the user just acted on.
///
/// This used to be a `home: bool` whose `false` arm reached the
/// single-user cache. Nothing passed `false`: that cache is written only
/// by [`reload`], which serves `--fetch-only`, and a headless fetch has no
/// delete affordance to reach this from. #161 replaced the flag with
/// [`TimelineSource`] rather than growing it a third state for a path the
/// window cannot take.
///
/// A missing cache file is not an error: there is nothing to remove, and
/// the post is gone from X either way.
pub(crate) fn forget_post(
    paths: &Paths,
    source: &TimelineSource,
    user_id: &str,
    post_id: &str,
    now: i64,
) -> Result<Vec<TimelineItem>> {
    // Resolved once (#92). The selector used to be branched on twice, with
    // each arm naming both a load and a save, so writing to one file and
    // reading the other back was expressible — which would have defeated
    // the read-back above: it is meant to prove *this* write landed.
    let path = source.cache_file(paths, user_id);

    let Some(cached) = load_timeline_file(&path)? else {
        return Ok(Vec::new());
    };
    let remaining = without_post(cached, post_id);
    save_timeline_file(&path, &remaining, now)?;
    Ok(load_timeline_file(&path)?.unwrap_or_default())
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
/// [`load_timeline`], [`since_id`], [`splice`], [`save_timeline`])
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
    let items = splice(cached, fresh, Side::Ahead);
    save_timeline(paths, &user_id, &items, now)?;
    Ok(Reloaded {
        items,
        user_id_cache_hit,
    })
}

/// What a reload of the window's primary timeline spent (#11, #161): the
/// merged, capped timeline to render, the resolved [`MeEntry`] itself (so
/// `ui.rs` can populate the header and remember the id for a later "Load
/// older"), and the response's `meta.next_token`, if any.
///
/// Unlike [`Reloaded`], this carries no `me_cache_hit` flag: nothing in this
/// crate currently reports per-reload request cost for the window's path
/// the way `main.rs`'s `--fetch-only` does for [`Reloaded`] via
/// `user_id_cache_hit`, so tracking it here would be dead weight. Add it back
/// if a caller needs it.
#[derive(Debug)]
pub(crate) struct ReloadedPrimary {
    pub items: Vec<TimelineItem>,
    pub me: MeEntry,
    pub next_token: Option<String>,
}

/// Spend the credits a reload of the window's timeline is allowed to spend:
/// resolve `/me` (from cache if fresh, else one API request, then cached
/// for next time), fetch a page from `source`, merge it ahead of what's
/// cached (never appended behind — that's [`load_older_primary`]'s job),
/// persist the result, and return it alongside `meta.next_token`.
///
/// **Only [`TimelineSource::Home`] fetches incrementally.** It passes
/// `since_id` so the API returns nothing already on file;
/// `GET /2/lists/:id/tweets` accepts no such parameter, so a list reload
/// always re-reads the head page. [`splice`] merges by id either way, so
/// the difference is in what is billed, not in what is rendered — see
/// [`XClient::list_timeline`].
///
/// Mirrors [`reload`], the single-user equivalent. Not unit-tested directly
/// for the same reason `reload` isn't — it makes real HTTP requests through
/// `client`. Everything it composes is tested standalone.
pub(crate) fn reload_primary(
    paths: &Paths,
    client: &XClient,
    source: &TimelineSource,
    max_results: u32,
    now: i64,
) -> Result<ReloadedPrimary> {
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

    let cached = load_primary_timeline(paths, source, &me.id)?.unwrap_or_default();
    let (fresh, next_token) = match source {
        TimelineSource::Home => {
            client.home_timeline(paths, &me.id, max_results, since_id(&cached), None, now)?
        }
        TimelineSource::List(list_id) => {
            client.list_timeline(paths, list_id, max_results, None, now)?
        }
    };
    let items = splice(cached, fresh, Side::Ahead);
    save_primary_timeline(paths, source, &me.id, &items, now)?;
    Ok(ReloadedPrimary {
        items,
        me,
        next_token,
    })
}

/// Spend one request to fetch the page *behind* `pagination_token` (#11's
/// "Load older"): append it after what's cached — [`Side::Behind`], never
/// [`Side::Ahead`] — persist the combined result, and return it
/// alongside the next `meta.next_token` (`None` once there's nothing further
/// back). `user_id` is the caller's responsibility to supply — `ui.rs` keeps
/// it around from the last [`reload_primary`] or [`startup_primary`], since
/// this function has no reason to re-resolve `/me` just to page further back
/// through content it's already showing.
///
/// Paging back through a list works the same way: `pagination_token` is the
/// one parameter `GET /2/lists/:id/tweets` shares with the home timeline,
/// so this is the one direction where the two sources are not asymmetric.
///
/// Not unit-tested directly, for the same reason [`reload_primary`] isn't.
pub(crate) fn load_older_primary(
    paths: &Paths,
    client: &XClient,
    source: &TimelineSource,
    user_id: &str,
    max_results: u32,
    pagination_token: &str,
    now: i64,
) -> Result<(Vec<TimelineItem>, Option<String>)> {
    let cached = load_primary_timeline(paths, source, user_id)?.unwrap_or_default();
    let (older, next_token) = match source {
        TimelineSource::Home => client.home_timeline(
            paths,
            user_id,
            max_results,
            None,
            Some(pagination_token),
            now,
        )?,
        TimelineSource::List(list_id) => {
            client.list_timeline(paths, list_id, max_results, Some(pagination_token), now)?
        }
    };
    let items = splice(cached, older, Side::Behind);
    save_primary_timeline(paths, source, user_id, &items, now)?;
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
            metrics: None,
            links: Vec::new(),
            author_avatar_url: None,
            original_post_id: None,
            media: Vec::new(),
        }
    }

    fn ids(items: &[TimelineItem]) -> Vec<&str> {
        items.iter().map(|item| item.id.as_str()).collect()
    }

    /// [`item`] with `created_at` set to a real-shaped, fixed-width
    /// timestamp string, for the #102 ordering tests below.
    fn item_at(id: &str, created_at: &str) -> TimelineItem {
        let mut built = item(id);
        built.created_at = Some(created_at.to_string());
        built
    }

    /// A fixed-width `created_at` string that increases with `n`, in the
    /// same `YYYY-MM-DDTHH:MM:SS.mmmZ` shape the API actually sends. Used
    /// instead of hand-writing distinct timestamp literals per test so the
    /// ordering under test (`n` larger => string compares greater) is
    /// obviously correct by construction.
    fn ts(n: u32) -> String {
        format!(
            "2026-01-01T{:02}:{:02}:{:02}.000Z",
            n / 3600,
            (n / 60) % 60,
            n % 60
        )
    }

    fn test_paths(root: &Path) -> Paths {
        let home = root.display().to_string();
        Paths::from_vars(move |key| (key == "HOME").then(|| home.clone())).unwrap()
    }

    fn temp_root(label: &str) -> PathBuf {
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

    // --- #72: deleting a post ---

    #[test]
    fn without_post_drops_only_the_named_post() {
        let items = vec![item("1"), item("2"), item("3")];
        assert_eq!(ids(&without_post(items, "2")), ["1", "3"]);
    }

    #[test]
    fn without_post_leaves_an_unknown_id_alone() {
        let items = vec![item("1"), item("2")];
        assert_eq!(ids(&without_post(items, "nonexistent")), ["1", "2"]);
    }

    #[test]
    fn forget_post_rewrites_the_displayed_cache_and_reads_it_back() {
        // The issue's actual completion criterion: gone from the cache too,
        // so it cannot come back on the next start. Asserted by reading the
        // file again rather than trusting the write.
        let root = temp_root("forget-home");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        save_primary_timeline(
            &paths,
            &TimelineSource::Home,
            "me",
            &[item("1"), item("2")],
            0,
        )
        .unwrap();

        let remaining = forget_post(&paths, &TimelineSource::Home, "me", "1", 1).unwrap();
        assert_eq!(ids(&remaining), ["2"]);
        assert_eq!(
            ids(&load_primary_timeline(&paths, &TimelineSource::Home, "me")
                .unwrap()
                .unwrap()),
            ["2"]
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn forget_post_touches_only_the_displayed_timelines_file() {
        // The same post can sit in both caches; only the one the user was
        // looking at is what they acted on.
        let root = temp_root("forget-one-file");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        save_primary_timeline(&paths, &TimelineSource::Home, "me", &[item("1")], 0).unwrap();
        save_timeline(&paths, "me", &[item("1")], 0).unwrap();

        forget_post(&paths, &TimelineSource::Home, "me", "1", 1).unwrap();

        assert!(
            load_primary_timeline(&paths, &TimelineSource::Home, "me")
                .unwrap()
                .unwrap()
                .is_empty()
        );
        assert_eq!(ids(&load_timeline(&paths, "me").unwrap().unwrap()), ["1"]);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn forget_post_is_not_an_error_when_no_cache_file_exists() {
        let root = temp_root("forget-missing");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        assert!(
            forget_post(&paths, &TimelineSource::Home, "me", "1", 1)
                .unwrap()
                .is_empty()
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn since_id_is_none_for_an_empty_cache() {
        assert_eq!(since_id(&[]), None);
    }

    #[test]
    fn since_id_is_the_first_and_therefore_newest_cached_post() {
        let cached = vec![item("300"), item("200"), item("100")];
        assert_eq!(since_id(&cached), Some("300"));
    }

    // --- splice ahead (#92, formerly merge_timeline) ---

    #[test]
    fn splice_ahead_places_fresh_posts_before_cached_posts() {
        let fresh = vec![item("3"), item("2")];
        let cached = vec![item("1")];
        let merged = splice(cached, fresh, Side::Ahead);
        assert_eq!(ids(&merged), vec!["3", "2", "1"]);
    }

    #[test]
    fn splice_ahead_drops_a_fresh_post_whose_id_is_already_cached() {
        // The API can hand back a post that's already on file; the cached
        // copy stays put rather than being duplicated.
        let fresh = vec![item("3"), item("2")];
        let cached = vec![item("2"), item("1")];
        let merged = splice(cached, fresh, Side::Ahead);
        assert_eq!(ids(&merged), vec!["3", "2", "1"]);
    }

    #[test]
    fn splice_ahead_keeps_the_result_ordered_newest_first() {
        let fresh = vec![item("6"), item("5"), item("4")];
        let cached = vec![item("3"), item("2"), item("1")];
        let merged = splice(cached, fresh, Side::Ahead);
        assert_eq!(ids(&merged), vec!["6", "5", "4", "3", "2", "1"]);
    }

    #[test]
    fn splice_ahead_truncates_to_the_500_post_cap() {
        let fresh = vec![item("502"), item("501")];
        let cached: Vec<_> = (1..=500).rev().map(|n| item(&n.to_string())).collect();
        let merged = splice(cached, fresh, Side::Ahead);
        assert_eq!(merged.len(), 500);
        assert_eq!(merged.first().unwrap().id, "502");
        // The two oldest cached posts ("2" and "1") were pushed out by the cap.
        assert!(!ids(&merged).contains(&"1"));
        assert!(!ids(&merged).contains(&"2"));
        assert_eq!(merged.last().unwrap().id, "3");
    }

    #[test]
    fn splice_keeps_the_cached_copy_of_a_duplicate_in_both_directions() {
        // #92: the two functions this replaced both kept what was already
        // on file, and that is load-bearing rather than incidental — a
        // post's metrics (#67) are a snapshot from when it was fetched, so
        // keeping the cached copy is what stops a reload shuffling the
        // counts on rows the user is already looking at.
        let mut cached_copy = item("1");
        cached_copy.text = "on file".to_string();
        let mut incoming_copy = item("1");
        incoming_copy.text = "just fetched".to_string();

        let ahead = splice(
            vec![cached_copy.clone()],
            vec![incoming_copy.clone()],
            Side::Ahead,
        );
        assert_eq!(ahead.len(), 1);
        assert_eq!(ahead[0].text, "on file");

        let behind = splice(vec![cached_copy], vec![incoming_copy], Side::Behind);
        assert_eq!(behind.len(), 1);
        assert_eq!(behind[0].text, "on file");
    }

    // --- splice merges a recurring id's missing fields (#97) ---

    #[test]
    fn splice_fills_a_missing_optional_field_from_the_incoming_copy() {
        let mut cached_copy = item("1");
        cached_copy.author_avatar_url = None;
        let mut incoming_copy = item("1");
        incoming_copy.author_avatar_url = Some("https://example.com/avatar.png".to_string());

        let merged = splice(vec![cached_copy], vec![incoming_copy], Side::Ahead);
        assert_eq!(
            merged[0].author_avatar_url.as_deref(),
            Some("https://example.com/avatar.png")
        );
    }

    #[test]
    fn splice_keeps_the_cached_metrics_snapshot_instead_of_the_incoming_one() {
        // #67: metrics are a snapshot from when the post was first fetched.
        // The merge rule ("cached Some wins") must not special-case this —
        // it falls out of the same rule that fills a missing
        // author_avatar_url, and this test is what proves it does.
        let mut cached_copy = item("1");
        cached_copy.metrics = Some(crate::x_api::PostMetrics {
            likes: 1,
            reposts: 2,
            replies: 3,
        });
        let mut incoming_copy = item("1");
        incoming_copy.metrics = Some(crate::x_api::PostMetrics {
            likes: 100,
            reposts: 200,
            replies: 300,
        });

        let merged = splice(vec![cached_copy], vec![incoming_copy], Side::Ahead);
        assert_eq!(merged[0].metrics.as_ref().unwrap().likes, 1);
    }

    #[test]
    fn splice_fills_metrics_when_the_cached_copy_has_none() {
        let cached_copy = item("1");
        assert_eq!(cached_copy.metrics, None);
        let mut incoming_copy = item("1");
        incoming_copy.metrics = Some(crate::x_api::PostMetrics {
            likes: 5,
            reposts: 6,
            replies: 7,
        });

        let merged = splice(vec![cached_copy], vec![incoming_copy], Side::Ahead);
        assert_eq!(merged[0].metrics.as_ref().unwrap().likes, 5);
    }

    #[test]
    fn splice_fills_an_empty_links_vec_from_the_incoming_copy() {
        let cached_copy = item("1");
        assert!(cached_copy.links.is_empty());
        let mut incoming_copy = item("1");
        incoming_copy.links = vec![crate::x_api::PostLink {
            url: "https://example.com".to_string(),
            label: "example.com".to_string(),
        }];

        let merged = splice(vec![cached_copy], vec![incoming_copy], Side::Ahead);
        assert_eq!(merged[0].links.len(), 1);
        assert_eq!(merged[0].links[0].label, "example.com");
    }

    // --- splice behind (#92, formerly append_older) ---

    #[test]
    fn splice_behind_places_older_posts_after_cached_posts() {
        let cached = vec![item("3"), item("2")];
        let older = vec![item("1")];
        let merged = splice(cached, older, Side::Behind);
        assert_eq!(ids(&merged), vec!["3", "2", "1"]);
    }

    #[test]
    fn splice_behind_drops_an_older_post_whose_id_is_already_cached() {
        // The page boundary can overlap: the API can hand back a post
        // that's already on file, and it must not be duplicated.
        let cached = vec![item("3"), item("2")];
        let older = vec![item("2"), item("1")];
        let merged = splice(cached, older, Side::Behind);
        assert_eq!(ids(&merged), vec!["3", "2", "1"]);
    }

    #[test]
    fn splice_behind_keeps_the_result_ordered_newest_first() {
        let cached = vec![item("6"), item("5"), item("4")];
        let older = vec![item("3"), item("2"), item("1")];
        let merged = splice(cached, older, Side::Behind);
        assert_eq!(ids(&merged), vec!["6", "5", "4", "3", "2", "1"]);
    }

    #[test]
    fn splice_behind_truncates_to_the_500_post_cap() {
        let cached: Vec<_> = (3..=502).rev().map(|n| item(&n.to_string())).collect();
        let older = vec![item("2"), item("1")];
        let merged = splice(cached, older, Side::Behind);
        assert_eq!(merged.len(), 500);
        assert_eq!(merged.first().unwrap().id, "502");
        // The two oldest fetched posts ("2" and "1") are pushed out by the cap.
        assert!(!ids(&merged).contains(&"1"));
        assert!(!ids(&merged).contains(&"2"));
        assert_eq!(merged.last().unwrap().id, "3");
    }

    // --- splice orders the result by created_at, not by fetch order (#102) ---

    #[test]
    fn splice_ahead_orders_by_created_at_even_when_the_fresh_batch_is_older() {
        // A `since_id` reload's own posts are expected to be newer than
        // what's cached, but this must not be an assumption baked into the
        // result — if it ever isn't (clock skew, a backfilled post), the
        // splice still has to land in created_at order rather than fresh-
        // batch-always-first.
        let cached = vec![item_at("2", &ts(20))];
        let fresh = vec![item_at("3", &ts(10))];
        let merged = splice(cached, fresh, Side::Ahead);
        assert_eq!(ids(&merged), vec!["2", "3"]);
    }

    #[test]
    fn splice_behind_orders_by_created_at_even_when_the_older_batch_is_newer() {
        // Mirrors the Ahead case above for a "Load older" page.
        let cached = vec![item_at("5", &ts(10))];
        let older = vec![item_at("4", &ts(20))];
        let merged = splice(cached, older, Side::Behind);
        assert_eq!(ids(&merged), vec!["4", "5"]);
    }

    #[test]
    fn splice_sort_is_stable_for_equal_created_at() {
        // Same created_at down to the string: the relative order the API
        // returned them in (here, the pre-sort concatenation order) must
        // survive, which is exactly what a stable sort guarantees and
        // `sort_unstable_by` would not.
        let cached = vec![item_at("1", &ts(10))];
        let fresh = vec![item_at("3", &ts(10)), item_at("2", &ts(10))];
        let merged = splice(cached, fresh, Side::Ahead);
        assert_eq!(ids(&merged), vec!["3", "2", "1"]);
    }

    #[test]
    fn splice_sinks_a_missing_created_at_row_to_the_end() {
        // A row with no created_at (#97's old cache rows, or a malformed
        // response) must not sort ahead of a row that has one, even when
        // fetch order would otherwise place it first.
        let cached = vec![item_at("2", &ts(10))];
        let fresh = vec![item("3")]; // created_at: None
        let merged = splice(cached, fresh, Side::Ahead);
        assert_eq!(ids(&merged), vec!["2", "3"]);
    }

    #[test]
    fn splice_preserves_relative_order_among_multiple_missing_created_at_rows() {
        // Two None rows must not swap relative to each other just because
        // sorting moved them both past a row that does have a created_at.
        let fresh = vec![item("9")]; // created_at: None
        let cached = vec![item_at("5", &ts(10)), item("8")]; // second: None
        // Pre-sort concatenation (Side::Ahead) is ["9", "5", "8"]: a None
        // row ahead of a Some row, and another None row behind it.
        let merged = splice(cached, fresh, Side::Ahead);
        assert_eq!(ids(&merged), vec!["5", "9", "8"]);
    }

    #[test]
    fn splice_sorts_before_capping_so_the_500_cap_drops_the_oldest_rows() {
        // Regression guard for ordering the truncate after the sort: build
        // a batch where the naive "concatenate, then cut at 500" order
        // would keep the wrong rows. `fresh` sits at the front of the
        // Side::Ahead concatenation (positions 0-1) but is, by created_at,
        // older than every cached row. If truncate ran before the sort (or
        // the sort didn't run at all), the cap would keep these two and
        // drop the two oldest *cached* rows instead — even though those
        // cached rows are chronologically newer than `fresh`.
        let cached: Vec<_> = (1000..1500)
            .rev()
            .map(|n| item_at(&n.to_string(), &ts(n)))
            .collect();
        let fresh = vec![item_at("502", &ts(2)), item_at("501", &ts(1))];
        let merged = splice(cached, fresh, Side::Ahead);
        assert_eq!(merged.len(), 500);
        assert_eq!(merged.first().unwrap().id, "1499");
        assert_eq!(merged.last().unwrap().id, "1000");
        // The chronologically-oldest rows (the fresh ones) are the ones
        // the cap drops, not the two oldest cached rows.
        assert!(!ids(&merged).contains(&"502"));
        assert!(!ids(&merged).contains(&"501"));
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

    // --- cached_owned_lists / save_owned_lists (#164) ---

    fn list(id: &str, name: &str) -> ListSummary {
        ListSummary {
            id: id.to_string(),
            name: name.to_string(),
        }
    }

    #[test]
    fn owned_lists_round_trip_in_the_order_the_api_returned_them() {
        let root = temp_root("owned-lists-roundtrip");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let lists = vec![list("2", "second"), list("1", "first")];
        save_owned_lists(&paths, &lists, 100).unwrap();
        assert_eq!(cached_owned_lists(&paths).unwrap(), Some(lists));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn owned_lists_never_expire_by_age() {
        // #164: staleness is bounded by the picker's explicit refresh,
        // the way `load_timeline` is bounded by an explicit reload —
        // a list renamed last month is still the right list to switch to.
        let root = temp_root("owned-lists-old");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        save_owned_lists(&paths, &[list("1", "old name")], 0).unwrap();
        assert_eq!(
            cached_owned_lists(&paths).unwrap(),
            Some(vec![list("1", "old name")])
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn owned_lists_are_none_when_the_file_is_missing() {
        let root = temp_root("owned-lists-missing");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        assert_eq!(cached_owned_lists(&paths).unwrap(), None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_corrupted_owned_lists_file_is_a_clean_miss_not_an_error() {
        let root = temp_root("owned-lists-corrupt");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.owned_lists_file(), b"not json at all").unwrap();

        assert_eq!(cached_owned_lists(&paths).unwrap(), None);

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

    #[test]
    fn a_cache_file_carrying_a_schema_version_key_still_reads_back() {
        // Every cache file on disk right now was written while
        // `schema_version` existed (#97, dropped again since). Serde
        // ignores unknown fields, so those files still load — but that is
        // a property of the derive rather than an intention anyone wrote
        // down, and if it stopped holding the whole cache would go quiet
        // rather than loudly: `load_json` turns a parse failure into
        // `Ok(None)`, which reads exactly like an empty cache.
        let root = temp_root("timeline-legacy-schema-version-key");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        std::fs::write(
            paths.timeline_file("2244994945"),
            br#"{"schema_version": 2, "fetched_at": 0, "items": []}"#,
        )
        .unwrap();

        assert_eq!(
            load_timeline(&paths, "2244994945").unwrap(),
            Some(Vec::new()),
            "a file with the old key must load, not read as an empty cache"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_timeline_cache_file_written_by_this_version_reads_back() {
        let root = temp_root("timeline-current-schema-version");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let items = vec![item("1")];
        save_timeline(&paths, "2244994945", &items, 0).unwrap();

        assert_eq!(load_timeline(&paths, "2244994945").unwrap(), Some(items));

        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- load_primary_timeline / save_primary_timeline ---

    #[test]
    fn load_primary_timeline_is_none_when_the_file_is_missing() {
        let root = temp_root("home-timeline-missing");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        assert_eq!(
            load_primary_timeline(&paths, &TimelineSource::Home, "2244994945").unwrap(),
            None
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn save_primary_timeline_then_load_primary_timeline_roundtrips() {
        let root = temp_root("home-timeline-roundtrip");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let items = vec![item("2"), item("1")];
        save_primary_timeline(&paths, &TimelineSource::Home, "2244994945", &items, 1_000).unwrap();
        let loaded = load_primary_timeline(&paths, &TimelineSource::Home, "2244994945").unwrap();
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
        save_primary_timeline(
            &paths,
            &TimelineSource::Home,
            "123",
            &[item("home-timeline-post")],
            0,
        )
        .unwrap();

        assert_eq!(
            load_timeline(&paths, "123").unwrap().unwrap()[0].id,
            "single-user-post"
        );
        assert_eq!(
            load_primary_timeline(&paths, &TimelineSource::Home, "123")
                .unwrap()
                .unwrap()[0]
                .id,
            "home-timeline-post"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- #161: the list source ---

    #[test]
    fn a_lists_cache_and_the_home_cache_do_not_collide() {
        // #161: the window shows one or the other, and switching between
        // them must not have the newcomer overwrite what the other had.
        let root = temp_root("list-vs-home");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let list = TimelineSource::List("2091351590695588200".to_string());
        save_primary_timeline(&paths, &TimelineSource::Home, "me", &[item("home-post")], 0)
            .unwrap();
        save_primary_timeline(&paths, &list, "me", &[item("list-post")], 0).unwrap();

        assert_eq!(
            ids(&load_primary_timeline(&paths, &TimelineSource::Home, "me")
                .unwrap()
                .unwrap()),
            ["home-post"]
        );
        assert_eq!(
            ids(&load_primary_timeline(&paths, &list, "me").unwrap().unwrap()),
            ["list-post"]
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_lists_cache_is_keyed_by_the_list_not_the_reader() {
        // The same list read by two accounts is the same posts, so the
        // signed-in id must not appear in the filename — otherwise the
        // second account re-fetches everything the first already paid for.
        let root = temp_root("list-not-per-reader");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let list = TimelineSource::List("2091351590695588200".to_string());
        save_primary_timeline(&paths, &list, "alice", &[item("1")], 0).unwrap();

        assert_eq!(
            ids(&load_primary_timeline(&paths, &list, "bob")
                .unwrap()
                .unwrap()),
            ["1"]
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn two_lists_keep_separate_caches() {
        let root = temp_root("two-lists");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let one = TimelineSource::List("111".to_string());
        let two = TimelineSource::List("222".to_string());
        save_primary_timeline(&paths, &one, "me", &[item("from-one")], 0).unwrap();
        save_primary_timeline(&paths, &two, "me", &[item("from-two")], 0).unwrap();

        assert_eq!(
            ids(&load_primary_timeline(&paths, &one, "me").unwrap().unwrap()),
            ["from-one"]
        );
        assert_eq!(
            ids(&load_primary_timeline(&paths, &two, "me").unwrap().unwrap()),
            ["from-two"]
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn re_reading_the_whole_head_page_adds_no_rows() {
        // #161's cost of having no `since_id`: every list reload returns
        // the same head page. `splice` has to absorb a batch that overlaps
        // the cache completely, or the timeline grows a duplicate of itself
        // on every reload.
        let cached = vec![item("3"), item("2"), item("1")];
        let head_page_again = vec![item("3"), item("2"), item("1")];

        let spliced = splice(cached, head_page_again, Side::Ahead);

        assert_eq!(ids(&spliced), ["3", "2", "1"]);
    }

    #[test]
    fn forget_post_removes_a_post_from_a_lists_cache() {
        let root = temp_root("forget-list");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let list = TimelineSource::List("2091351590695588200".to_string());
        save_primary_timeline(&paths, &list, "me", &[item("1"), item("2")], 0).unwrap();

        let remaining = forget_post(&paths, &list, "me", "1", 1).unwrap();
        assert_eq!(ids(&remaining), ["2"]);
        assert_eq!(
            ids(&load_primary_timeline(&paths, &list, "me").unwrap().unwrap()),
            ["2"]
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn startup_primary_renders_a_list_from_cache() {
        let root = temp_root("startup-list");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let list = TimelineSource::List("2091351590695588200".to_string());
        save_me(&paths, "2244994945", "alice", 0).unwrap();
        save_primary_timeline(&paths, &list, "2244994945", &[item("1")], 0).unwrap();

        let (me, items) = startup_primary(&paths, &list, 0).unwrap().unwrap();
        assert_eq!(me.username, "alice");
        assert_eq!(ids(&items), ["1"]);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn startup_primary_is_none_for_a_list_with_only_the_home_cache_on_file() {
        // Configuring a list on an install that has been running on the
        // home timeline must not render the home timeline's posts under a
        // list's name.
        let root = temp_root("startup-list-miss");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        save_me(&paths, "2244994945", "alice", 0).unwrap();
        save_primary_timeline(&paths, &TimelineSource::Home, "2244994945", &[item("1")], 0)
            .unwrap();

        let list = TimelineSource::List("2091351590695588200".to_string());
        assert_eq!(startup_primary(&paths, &list, 0).unwrap(), None);

        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- startup_primary ---

    #[test]
    fn startup_primary_renders_from_cache_when_both_me_and_the_timeline_are_cached() {
        let root = temp_root("startup-home-hit");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        save_me(&paths, "2244994945", "alice", 0).unwrap();
        let items = vec![item("2"), item("1")];
        save_primary_timeline(&paths, &TimelineSource::Home, "2244994945", &items, 0).unwrap();

        let rendered = startup_primary(&paths, &TimelineSource::Home, 0).unwrap();
        let (me, rendered_items) = rendered.unwrap();
        assert_eq!(me.id, "2244994945");
        assert_eq!(me.username, "alice");
        assert_eq!(rendered_items, items);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn startup_primary_is_none_when_me_is_not_cached() {
        let root = temp_root("startup-home-no-me");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        assert!(
            startup_primary(&paths, &TimelineSource::Home, 0)
                .unwrap()
                .is_none()
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn startup_primary_is_none_when_me_is_cached_but_the_timeline_is_not() {
        let root = temp_root("startup-home-no-timeline");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        save_me(&paths, "2244994945", "alice", 0).unwrap();

        assert!(
            startup_primary(&paths, &TimelineSource::Home, 0)
                .unwrap()
                .is_none()
        );

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
}
