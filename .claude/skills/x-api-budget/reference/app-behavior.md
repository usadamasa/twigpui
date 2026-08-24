# twigpui の課金まわりの挙動 — リクエスト数・レートリミット・消費集計・キャッシュ

`../SKILL.md` が判断の規範、`pricing.md` が X 側の課金仕様 (単価・出典・実測)。
このファイルは **twigpui が何をしているか**。

以下は英語のまま。#4 以来のドキュメントを畳んだもので、書き直す理由がない。

## Requests per action

Reads are billed per resource returned (see `pricing.md`), but the request
count still governs rate limits and the actions that really are per-request.

A cold reload sends two: one id lookup
(`/users/by/username/:username` for the single-user view, `/users/me` for the
home timeline — #11) and one timeline fetch, plus one more per "Load older"
click, plus **up to five** more per "Show thread" click on a reply (#12 — one
`GET /2/tweets?ids=` request per parent level). Fetching happens only on an
explicit action — there is no polling or auto-refresh, and since #9,
**opening the app spends nothing at all**: startup renders straight from the
local cache below whenever one exists, with no request in the loop.

Reposting spends one request; un-reposting spends one more. Liking and
unliking (#68) cost the same, one request each, and so does deleting a post
(#72).

`--fetch-post` (#42) sends exactly one request per run, however many post ids
are given — they all ride in a single `GET /2/tweets?ids=` request's
comma-separated `ids=` parameter, never one request per id. The billing,
though, still follows the number of posts that come back.

When credits run out the API answers `429` with a `UsageCapExceeded` problem
body; the app surfaces that text directly in the window.

## `--sync-list`, and why a debug build syncs something else

`--sync-list` (#163) is the one action here whose read cost has no ceiling:
its dry run pages through the whole follow graph and the whole list
membership, and both are billed per account returned. Against a few
thousand follows that is dollars for a run that writes nothing.

Since #169, that read only happens in a **release** build. A debug build is
the development profile, and it replaces the follow-graph read with four
fixed screen names (`DEV_SYNC_SEED` in `src/profile.rs`), resolved through
the same 30-day `user_ids.json` cache a reload uses — four billed lookups a
month, then nothing. It also defaults `list_id` to a throwaway list, so a
development `--apply` cannot rewrite the real one.

The consequence to keep in mind when reading or writing docs: **a
`--sync-list` example without `--release` is a development sync.** It does
not fail, it syncs the other pair. The list membership read is paginated
and billed in both profiles — only the follow-graph side is stood in for.

### The background sync reads the members side from a local mirror (#173)

`GET /2/lists/:id/members` is the dearer side: the docs' pricing widget
puts it at the Users rate ($0.010/resource) with no Owned Read discount,
against $0.001 for `/2/users/:id/following` (`pricing.md`). After the
first full read this app is the only writer of the list, so the scheduled
diff takes the members side from `sync_members.json` and pays for the
follow side only. The mirror is re-read from X when it is older than
`sync_members_refresh_seconds` (7 days by default; `0` turns the mirror
off), and every write the sync sends is recorded in it as it lands.

Three diffs never take the mirror and leave a fresh one behind: the CLI
dry run, a sync started from the status bar (once it gets as far as a
diff — an outstanding plan is drained first, #174), and the first
scheduled diff after the mirror is discarded. It is discarded whenever a
mirror-derived plan looks wrong — removals over the prune cap, or a write
X refuses — so a stale mirror costs one extra full read, never a retry
loop. A full read that replaces a mirror logs how many accounts the two
disagreed on; that line is the only place drift becomes visible.

## Rate limits

X's per-endpoint rate limits and the prepaid usage cap above both surface as
HTTP `429`, but they behave nothing alike — retrying a usage-cap `429` never
helps (the account needs topping up), while an ordinary rate limit recovers
once its window resets. twigpui tells them apart and treats each accordingly:

- **What's tracked.** Every response's `x-rate-limit-limit` /
  `-remaining` / `-reset` headers are parsed and kept per endpoint: the
  username lookup, the single-user timeline fetch, `/users/me`, the home
  timeline (#11), `GET /2/tweets?ids=` (#12, "Show thread"),
  `POST /2/tweets` (#14, posting), reposting/un-reposting (#15,
  `POST`/`DELETE /2/users/:id/retweets…`, tracked as two separate endpoints
  since X limits creating and deleting a repost independently), and
  liking/unliking (#68, `POST`/`DELETE /2/users/:id/likes…`, two endpoints
  for the same reason) are all tracked separately, since X limits each of
  them separately.
- **The app refuses to send rather than waiting.** If the tracked remaining
  count is zero and the reset time hasn't arrived yet, twigpui does **not**
  send the request — a GUI app has no business sleeping a background thread
  for up to 15 minutes on the chance a click resolves itself. Instead the
  reload button shows a countdown to the reset time, and clicking again
  before then just re-checks the (free, local) tracked state rather than
  hitting the network.
- **Retries.** A network error or a `5xx` response is retried with
  exponential backoff and jitter, up to a small attempt cap. Neither kind of
  `429` is ever retried — one recovers on its own schedule regardless of
  retrying, and the other never recovers at all.
- **Where it's kept.** `$XDG_STATE_HOME/twigpui/rate_limit.json` (state, not
  cache — a process restart doesn't reset X's window, so losing this file
  risks firing a request straight into one that's already exhausted). A
  missing or corrupt file is a clean "nothing tracked yet", the same way a
  broken response cache is (see below), never a startup failure.
- **Minimum fetch interval.** `X_MIN_FETCH_INTERVAL_SECONDS` (or
  `min_fetch_interval_seconds` in `config.toml`, default `60`) is a
  client-side floor on how often the reload button itself may fire,
  independent of what the tracked API state above says. The button counts
  down for this too, but says so in different words — "Waiting out the fetch
  interval" rather than "Rate limited by X" — because in this case nothing
  was sent and X has said nothing. #21's auto-refresh will use the same
  setting once it lands.

## Usage tracking

**What this section describes does not match how X actually bills.** It
counts requests; reads are billed per resource (`pricing.md`), so the numbers
below understate reads by one to two orders of magnitude. #162 tracks the fix.
Until then, treat the counts as "how many times the app called out", not "what
this cost".

Two ways to see the real figure: the Developer Console's Usage / Billing
breakdown, or `GET /2/usage/tweets` — both described in `pricing.md`.

twigpui counts every request it actually sends and persists the counts, so a
running total is visible both in the window and from the command line.

**What's counted.** Every actual HTTP send counted from `x_api::client`'s one
central `get` method — including retries: a request retried after a network
error or a `5xx` counts once per attempt, since each one is a real send that
reaches (or attempts to reach) the API and is billed accordingly. A request
`#10`'s rate-limit tracker refuses to send in the first place is **not**
counted, since nothing went out. Counts are kept per endpoint (the same five
`Endpoint`s #10 already tracks separately) and summed for the totals shown
below.

**Where it's kept.** `$XDG_STATE_HOME/twigpui/usage.json` (state, not cache —
see the rate-limit section above for why that distinction matters here too).
Each endpoint's entry holds an all-time total and a count for the current UTC
day. A missing or corrupt file is a clean "nothing tracked yet", the same way
a broken response cache or rate-limit file is — never a startup failure.

**Day boundary: UTC.** "Today" resets at UTC midnight, not the machine's
local midnight, for two reasons: the X API's own `created_at` timestamps are
already UTC, so this keeps "today" meaning one consistent thing throughout
the app; and Rust's standard library has no reliable way to read the local
UTC offset without pulling in a date/time crate, which this project does not
otherwise need. No new dependency was added for this feature — a day
boundary is just `unix_seconds.div_euclid(86_400)` on the same Unix
timestamp every other module already uses. The tradeoff: someone west of UTC
sees "today" roll over mid-afternoon local time, not at their own midnight.

**No amount is ever shown unless a price is configured.** The per-request
price depends on the account's plan, and there is no way to know it from
here — so by default the header and `--usage` (below) show request *counts*
only. Setting `X_REQUEST_PRICE` (or `request_price` in `config.toml`), in
whatever unit you have in mind, turns those counts into an estimated amount
(`count × price`). A wrong, guessed price would be worse than no price at
all, so twigpui never invents a built-in default.

**Budget coloring.** `X_DAILY_REQUEST_BUDGET` (or `daily_request_budget` in
`config.toml`) is a request-count budget, not a monetary one — deliberately,
so it works whether or not a price is configured, since request counts are
always known. Once today's total across every endpoint reaches 80% of the
budget the header's usage line switches to a warning color; at or past the
budget itself, it switches to the same color used for errors and rate-limit
countdowns.

**In the window.** The header always shows a compact line under the title:
today's request count and the all-time total, with an estimated amount
appended when a price is configured — e.g. `Today: 3 req (~0.06) · Total: 42
req` or, with no price set, `Today: 3 req · Total: 42 req`.

**From the command line: `--usage`.** Prints the same numbers as JSON to
stdout and exits, without opening a window or making any network call (it
only reads `usage.json`):

```sh
cargo run -- --usage
```

```json
{
  "endpoints": {
    "user_lookup": { "total": 12, "today": 3 },
    "timeline": { "total": 12, "today": 3 },
    "me": { "total": 0, "today": 0 },
    "home_timeline": { "total": 0, "today": 0 },
    "tweet_by_id": { "total": 0, "today": 0 }
  },
  "total": {
    "total_requests": 24,
    "today_requests": 6,
    "price_per_request": null,
    "estimated_amount_total": null,
    "estimated_amount_today": null,
    "daily_budget": null,
    "budget_status": "ok"
  }
}
```

JSON rather than a bespoke text format, since the project already depends on
`serde_json` for everything else it persists, and a machine-readable
consumer (a script, another tool) needs structure to parse rather than a
format it has to scrape. `price_per_request` and every `estimated_amount_*`
field are `null` unless `X_REQUEST_PRICE` is configured, matching the
header's own rule. `budget_status` is `"ok"`, `"near"` (80% of the configured
budget), or `"exceeded"`; it's always `"ok"` when no budget is configured.

## Local cache

To avoid re-paying for the same content, twigpui keeps a small JSON cache
under `$XDG_CACHE_HOME/twigpui/` (see the [file locations table](../../../../README.md#file-locations-xdg-base-directory)):

| File | Holds | TTL |
| --- | --- | --- |
| `user_ids.json` | Every screen name resolved so far, mapped to its numeric user id | 30 days |
| `timeline-<user_id>.json` | One user's cached posts (single-user mode), newest first, plus when they were fetched | none — see below |
| `me.json` | The signed-in user's own id and screen name, from `/users/me` (#11) | 30 days |
| `home-timeline-<user_id>.json` | That user's cached home timeline (#11), newest first — a deliberately separate file from `timeline-<user_id>.json` for the same id, since the two hold different content | none — see below |
| `thread-<reply_id>.json` | One reply's already-walked parent chain (#12), keyed by the reply's own post id | none — a post's parents never change once posted |

**User ids — including the signed-in user's own, from `/users/me` — are
effectively permanent**, so caching the lookup is what turns a reload from
two requests into one: once an id is cached and still within its 30-day TTL,
a reload skips straight to the timeline-fetch request.

**Timelines have no TTL**, in either mode. Freshness is bounded by an explicit
reload, not by age — the cache is trusted at startup no matter how old it is,
since the whole point is that opening the window costs nothing. Reloading
passes the newest cached post's id as the API's `since_id`, so the response
only contains what's actually new; those posts are merged ahead of what's
already cached (any id already on file is dropped rather than duplicated),
and the result is capped at **500 posts per user**, oldest dropped first —
`~/.cache` isn't purged automatically by macOS the way `~/Library/Caches` is,
so this cap is twigpui's own. The home timeline's "Load older" button works
the other direction: it appends older posts (via `meta.next_token`) *behind*
what's cached rather than merging them ahead, and the same 500-post cap still
applies.

**A broken cache never blocks startup.** If a cache file fails to parse — or
was written by a version of twigpui with a different file shape — it's
treated as a plain cache miss and silently rebuilt on the next reload, rather
than as an error.

**Time Machine.** `cache_dir` is excluded from Time Machine backups via
`tmutil addexclusion` the first time it's created (best-effort; a failure
here, e.g. `tmutil` missing, never blocks startup).

**Clearing the cache by hand:**

```sh
rm -rf "$XDG_CACHE_HOME/twigpui"   # or ~/.cache/twigpui if XDG_CACHE_HOME is unset
```

The directory is recreated automatically on the next run; the next startup
after that falls back to a full reload since there's nothing cached yet.

**When you have to.** A `since_id`/`pagination_token` walk only ever asks
the API for posts *outside* the cached range, so a row already on file is
never re-fetched. Change what a field holds — add one to `TimelineItem`, or
widen `expansions` the way #104 did to make a repost's images load — and
every row already cached keeps the old, emptier value indefinitely.
Deleting the files is what forces them through again. It costs nothing but
the reload that was going to happen anyway: an empty cache makes `since_id`
return `None`, and the fetch that follows is the same single request.

`splice` covers part of this on its own, filling a cached row's missing
fields from the incoming copy whenever the same id turns up again — a page
boundary, a `since_id` overlap. The rows in between are the ones that need
the `rm`.

#97 automated it with a schema version stamped on write and checked on
read. It was removed again: for a single-user development tool the constant
was one more thing to remember to bump, with the same failure mode as
forgetting to delete the files, and it threw away 500 rows of scrollback
every time it fired.

