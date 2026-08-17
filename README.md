# twigpui

A development-only X (Twitter) timeline viewer, built with Rust and
[gpui](https://crates.io/crates/gpui). macOS only — no other platform is
considered.

## Status

The app shows one of two timelines, depending on which credential is active
(#11):

- **Signed in with OAuth** (see "Signing in with X" below): your own home
  timeline, `GET /2/users/:id/timelines/reverse_chronological` for the id
  `GET /2/users/me` resolves. A "Load older" button pages further back via
  `meta.next_token`.
- **App-only Bearer token only**: `X_TARGET_USERNAME`'s recent posts via
  `GET /2/users/:id/tweets` — the original milestone-1 view, kept as the
  fallback since the home timeline endpoint rejects an app-only token with
  401 (see "Why the fallback exists" below).

Either way, results render in a scrollable list with a reload button, and the
header names which mode is showing.

**Reposts and quotes are expanded (#13).** The API's raw response truncates a
repost to `RT @user: …`; both timeline requests now also ask for
`referenced_tweets` (`expansions=referenced_tweets.id,referenced_tweets.id.author_id`
plus `tweet.fields=referenced_tweets`), so the referenced post's real text
and author come back in the same response, at no extra request cost. A
repost renders as a small "`@user reposted`" line above the original
author and full text; a quote embeds the quoted post as a bordered card
under its own text. A repost of a quote shows both: the original author's
text as the body, and the quote card the original itself carried. If the
referenced post is deleted, protected, or otherwise missing from the
response's `includes`, the row still renders — a repost falls back to the
API's own (possibly truncated) text with the author left blank, and a quote
just omits the card.

**Reply context and "Show thread" (#12).** A reply shows "Replying to
@someone" for free — the parent's author is already in the same response's
`includes` thanks to the `referenced_tweets` expansion above, so this costs
no extra request. Walking further up the conversation costs real money,
though, so it never happens automatically: each reply instead offers a
"Show thread (up to 5 requests)" toggle that spells out the worst case
before it's clicked. Clicking it walks the parent chain one
`GET /2/tweets?ids=` request per level — each level's id is only known once
the previous one resolves, so they can't be batched — stopping at 5 levels
or the first parent that comes back empty (deleted, protected, or otherwise
absent), whichever comes first. Reaching the cap is reported explicitly
("Reached the 5-level limit…") rather than the thread just quietly trailing
off, and a fetch that errors offers a retry in place. A missing first parent
renders "The parent post is no longer available" instead of an empty gap.
Once walked, a thread is cached (`thread-<reply_id>.json`, see below), so
re-opening the same reply's thread — even after restarting the app — costs
nothing further. Listing the replies *to* a post (the other direction) needs
a different endpoint (`search/recent`) and is out of scope here — see #36.

`--fetch-only` runs the same fetch headlessly (always the single-user view,
regardless of credential) and prints the posts, which is useful for checking
credentials without opening a window:

```sh
cargo run -- --fetch-only
```

## Requirements

The `macos-blade` feature is enabled so the build does not need `xcrun metal`,
which ships with full Xcode rather than the Command Line Tools. Rendering goes
through blade instead.

## Why the single-user fallback exists

`GET /2/users/:id/timelines/reverse_chronological` (the home timeline) only
accepts OAuth 2.0 Authorization Code (user context) — an app-only Bearer
token gets a 401. Signing in with OAuth (below) is what unlocks it. Without a
signed-in session, twigpui falls back to showing `X_TARGET_USERNAME`'s posts
instead of showing nothing.

## Setup

Configuration comes from the environment. Either export it:

```sh
export X_BEARER_TOKEN='…'
cargo run
```

or keep a local `.env`, which `dotenvy` loads into the same variables:

```sh
cp .env.example .env
$EDITOR .env          # fill in X_BEARER_TOKEN or X_OAUTH_CLIENT_ID
cargo run
```

| Variable | Required | Default | Meaning |
| --- | --- | --- | --- |
| `X_BEARER_TOKEN` | see below | — | App-only Bearer token, used verbatim (keep any `%2F` / `%3D` as-is) |
| `X_OAUTH_CLIENT_ID` | see below | — | OAuth 2.0 client id for "Sign in with X" — non-secret, may also live in `config.toml` |
| `X_TARGET_USERNAME` | no | `XDevelopers` | Screen name to display, without a leading `@` |
| `X_MAX_RESULTS` | no | `20` | Posts per fetch, 5–100 |
| `X_MIN_FETCH_INTERVAL_SECONDS` | no | `60` | Floor on how often a fetch may run, in seconds (#10) |
| `X_THEME` | no | `light` | Color theme: `light`, `dark`, or `system` (follows the OS appearance) — also `theme` in `config.toml` (#19) |

At least one of `X_BEARER_TOKEN` or `X_OAUTH_CLIENT_ID` must be set — either
credential alone is enough to run, and having both is fine too. `.env` is
gitignored. Do not commit credentials.

## Signing in with X (OAuth 2.0 Authorization Code + PKCE)

Some endpoints — the home timeline, posting, and anything else that acts as
*you* rather than reading public data — require a user-context OAuth 2.0
session instead of the app-only Bearer token. twigpui gets one with the
Authorization Code + PKCE flow (RFC 6749 + RFC 7636), run entirely from the
window via a "Sign in with X" button.

**Developer Portal prerequisite.** Register a **public client** (no client
secret) in the X Developer Portal, and add this exact redirect URI:

```
http://127.0.0.1:8733/callback
```

X requires an exact match, so the port can't be ephemeral — `8733` is fixed
in the code (`oauth::callback::LOOPBACK_PORT`) and must match the Portal
registration verbatim.

**Client id.** Copy the client id the Portal shows you into `X_OAUTH_CLIENT_ID`
(env or `.env`) or `oauth_client_id` in `config.toml`. It's non-secret — a
public client has no secret to protect — so unlike the bearer token it's
fine to check into a dotfiles repo.

**Scopes.** twigpui requests exactly `tweet.read users.read offline.access`:
enough to read posts, resolve user context, and refresh the session without
re-prompting. `tweet.write` (posting) is not requested — least privilege
until #14 actually needs it.

**What happens when you click "Sign in with X":** the app opens your default
browser at X's consent screen, and a short-lived HTTP listener on
`127.0.0.1:8733` catches the redirect back (waiting up to two minutes). Once
X redirects with an authorization code, twigpui exchanges it for an access
token and a refresh token, then falls straight into a normal reload.

**Where tokens are stored.** `$XDG_STATE_HOME/twigpui/oauth_tokens.json`
(see the table below), written `0600` (owner read/write only) — the access
token, refresh token, and an absolute expiry, in plain JSON. This is a
development-only, single-user app; see issue #7 for why a Keychain wasn't
used instead (ad-hoc builds change signing identity on every rebuild, which
would re-prompt for Keychain access each time).

A stored session is refreshed automatically, shortly before it expires,
whenever the app needs a token — no re-prompting as long as the refresh
token stays valid.

### File locations (XDG Base Directory)

Everything twigpui persists lives under three directories, resolved per the
[XDG Base Directory
spec](https://specifications.freedesktop.org/basedir-spec/latest/) and
created (mode `0700`) on startup:

| Variable | Default | Holds |
| --- | --- | --- |
| `XDG_CONFIG_HOME` | `~/.config/twigpui/` | `config.toml` |
| `XDG_CACHE_HOME` | `~/.cache/twigpui/` | Response cache: `user_ids.json`, `timeline-<user_id>.json` (#9), `me.json`, `home-timeline-<user_id>.json` (#11), `thread-<reply_id>.json` (#12) |
| `XDG_STATE_HOME` | `~/.local/state/twigpui/` | `oauth_tokens.json` (mode `0600`), `rate_limit.json` (#10) |

An `XDG_*` variable is only honored if it is set to a non-blank absolute
path; a relative or blank value falls back to the default, per spec.

### `config.toml`

`$XDG_CONFIG_HOME/twigpui/config.toml` is an optional, hand-edited settings
file with the same keys as the environment variables above, minus the bearer
token:

```toml
target_username = "XDevelopers"
max_results = 20
min_fetch_interval_seconds = 60
oauth_client_id = "…"
theme = "light"
```

A missing file is fine — it just means there are no file-level settings.
Precedence is **environment variable > `config.toml` > built-in default**,
so an env var always wins over the file. There is no `bearer_token` key:
`config.toml` is a plain file people check into dotfiles repos, so that
credential stays environment-only by design (`X_BEARER_TOKEN` or `.env`) —
a `bearer_token` entry in the file is rejected with an error rather than
silently read. `oauth_client_id` is the exception: it's a public client id,
not a secret, so it's fine to check in.

### Theme (#19)

`theme` accepts `light`, `dark`, or `system` (case-insensitive, surrounding
whitespace trimmed), via `X_THEME` or `theme` in `config.toml`, with the same
env > file > default precedence as everything else above. It defaults to
`light`. `system` follows the OS appearance, read once at startup via gpui's
`Window::appearance()`. An unrecognized value is not a startup error — a
typo'd theme is cosmetic, not worth blocking the app over — it falls back to
`light` and prints a warning to stderr naming the value it ignored.

## API cost

The X API bills per request against prepaid credits. A cold reload spends two
requests: one id lookup (`/users/by/username/:username` for the single-user
view, `/users/me` for the home timeline — #11) and one timeline fetch, plus
one more request per "Load older" click, plus **up to five** more per
"Show thread" click on a reply (#12 — one `GET /2/tweets?ids=` request per
parent level, capped as described above). Fetching happens only on an
explicit action — there is no polling or auto-refresh, and since #9,
**opening the app spends nothing at all**: startup renders straight from the
local cache below whenever one exists, with no request in the loop.

When credits run out the API answers `429` with a `UsageCapExceeded` problem
body; the app surfaces that text directly in the window.

## Rate limits (#10)

X's per-endpoint rate limits and the prepaid usage cap above both surface as
HTTP `429`, but they behave nothing alike — retrying a usage-cap `429` never
helps (the account needs topping up), while an ordinary rate limit recovers
once its window resets. twigpui tells them apart and treats each accordingly:

- **What's tracked.** Every response's `x-rate-limit-limit` /
  `-remaining` / `-reset` headers are parsed and kept per endpoint: the
  username lookup, the single-user timeline fetch, `/users/me`, the home
  timeline (#11), and `GET /2/tweets?ids=` (#12, "Show thread") are all
  tracked separately, since X limits each of them separately.
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

## Local cache (#9)

To avoid re-paying for the same content, twigpui keeps a small JSON cache
under `$XDG_CACHE_HOME/twigpui/` (see the file locations table above):

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

## Tests

```sh
cargo test
```

Tests cover response parsing and error mapping against fixture JSON, the
OAuth PKCE math, callback parsing, and token-file handling, the local
cache's TTL, merge, and corruption-recovery logic (including #11's
merge-ahead-vs-append-behind distinction between a normal reload and "Load
older", and that the home-timeline and single-user caches for the same id
never collide), which timeline mode a credential resolves to (#11), the
repost/quote join against `includes.tweets`/`includes.users` and its
precedence when a post carries more than one reference, including a missing
referenced post and a repost-of-a-quote (#13), the reply-context join and its
own repost interaction, the `GET /2/tweets?ids=` URL and its independently
tracked rate-limit endpoint, the thread cache's roundtrip and corruption
recovery, and `thread::assemble_chain`'s ordering, dedup, and 5-level cutoff
— including a partial walk stopped by a missing parent — entirely without
network or disk (#12), and the rate-limit tracker's header parsing,
send/don't-send decision, `429` classification, jittered backoff schedule,
and persistence (#10), so they make no network calls, open no browser, and
spend no credits. The actual code exchange, X's live response shapes
(including `/users/me`, the home timeline's `meta.next_token`, #13's
`referenced_tweets` expansion, and #12's `GET /2/tweets?ids=` response
shape), refresh-token rotation, and the real rate-limit header values X sends
aren't covered by tests — those need a real Developer Portal registration, a
one-time manual sign-in, and (for the last) actually hitting a live rate
limit.
