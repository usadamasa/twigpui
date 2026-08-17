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

## Building the `.app` bundle (#40)

`cargo run` only opens a window from a terminal, in this checkout. For
something Spotlight, Launchpad, and the Dock can all see:

```sh
./scripts/build-app-bundle.sh
```

This builds a release binary, assembles `dist/twigpui.app`, writes an
`Info.plist` (bundle id, name, executable, package type,
`NSHighResolutionCapable`, and `CFBundleVersion` /
`CFBundleShortVersionString` read straight from `Cargo.toml`'s `version` via
`cargo metadata` — never hand-duplicated), and signs it ad hoc
(`codesign -s -`). Ad-hoc signing only, per this project's non-goals
(macOS-only, development use — no notarization, no Developer ID): it exists
solely so Gatekeeper doesn't refuse a bundle built on your own machine, not
to make it distributable. Move the result wherever you like, e.g.:

```sh
mv dist/twigpui.app /Applications/
```

**Icon.** The script looks for `assets/AppIcon.icns` (used as-is) or
`assets/AppIcon.png` (a 1024×1024 source it turns into a full `.icns` via
`sips`/`iconutil`). Neither exists in this repository — producing real
artwork wasn't practical without a design tool — so a bundle built as-is has
no `CFBundleIconFile` and macOS shows it with the generic app icon rather
than a dangling reference to a file that isn't there. Drop either file into
`assets/` and rebuild to get a custom one.

### Configuration for a bundled launch — read this before double-clicking

**A process launched from Finder, Spotlight, or the Dock does not inherit
your shell's environment**, and its working directory is not this
checkout — so `X_OAUTH_CLIENT_ID`, `X_TARGET_USERNAME`, and friends being
exported in your shell profile, or sitting in this repo's `.env`, has no
effect on a bundled launch. Two things follow:

- **`oauth_client_id`** is non-secret (see "`config.toml`" below) and
  already works from `$XDG_CONFIG_HOME/twigpui/config.toml` (default
  `~/.config/twigpui/config.toml`) regardless of how twigpui is started —
  `HOME` is set for every process launchd starts on your behalf, which is
  all `Paths::from_env` needs (`XDG_*` being unset just falls back to the
  same defaults a terminal launch with no `XDG_*` exported would use). Put
  your client id there and "Sign in with X" works from the bundle with zero
  environment setup. Once signed in, the session persists to
  `$XDG_STATE_HOME/twigpui/oauth_tokens.json` and needs no environment
  variable on the next bundled launch either.
- **`X_BEARER_TOKEN` cannot go in `config.toml`.** It's a secret, and
  `config.toml` is a plain file people check into dotfiles repos — putting a
  bearer token there is rejected with an explicit error rather than silently
  accepted. If you only have a bearer token and no OAuth client id, a
  bundled launch genuinely has no dotfiles-safe place left to read it from.
  The workaround is `launchctl setenv X_BEARER_TOKEN '…'`, which injects it
  into every GUI app's environment for the rest of the login session (not
  just twigpui's, and not persisted across logout or reboot) — or keep
  running the plain binary from a terminal, where `.env` and the shell
  environment still apply. The clean fix is signing in with OAuth instead,
  which needs nothing but the client id above; #33 tracks retiring the
  bearer token entirely, which this issue does not implement.
- **A missing or invalid configuration now names where to look.**
  Previously, a configuration error before the window opened only printed
  to stderr — which a bundled launch has no terminal to show, so the
  process just silently exited. It now also raises a native alert
  (`osascript … display alert`) that names the resolved `config.toml` path,
  whenever stderr isn't a terminal.

### What a bundled launch changes, and what it doesn't

- **The OAuth loopback listener** binds `127.0.0.1:8733` the same way
  whether twigpui is bundled or not — nothing in `oauth::callback` depends
  on the current working directory or the shell environment, only on the
  port being free. macOS may still prompt ("twigpui would like to accept
  incoming network connections") the first time a *newly signed* binary
  binds a listening socket, and since ad-hoc-signed builds get a fresh
  signing identity on every rebuild (the same reason Keychain isn't used for
  token storage — see "Where tokens are stored" below), that prompt can
  reappear after each rebuild rather than being remembered once and for all.
- **`macos-blade` and WindowServer.** A bundled `.app` launched by
  Finder/Dock/Spotlight has a normal user WindowServer session, the same as
  a binary launched from Terminal.app — nothing about being bundled changes
  that connection. This could not be verified from the environment that
  wrote this bundling script: it runs under a sandbox with no WindowServer
  access (`gpui` panics there with `NoSupportedDeviceFound` even for
  `cargo run`), so building `dist/twigpui.app` and inspecting its layout,
  `Info.plist`, and code signature was possible, but actually launching it
  was not. A human double-clicking the built `.app` is the only way to
  confirm the window opens.

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
| `X_REQUEST_PRICE` | no | unset | Price per API request, in whatever unit you have in mind — also `request_price` in `config.toml` (#18, see "Usage tracking" below) |
| `X_DAILY_REQUEST_BUDGET` | no | unset | Daily request-count budget that colors the header's usage line as it's approached — also `daily_request_budget` in `config.toml` (#18) |

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

**Scopes.** twigpui requests `tweet.read users.read tweet.write
offline.access`: enough to read posts, resolve user context, post (#14), and
refresh the session without re-prompting.

**If you signed in before #14,** your stored session predates `tweet.write`
and can't post yet — the API rejects `POST /2/tweets` with a 403 it has no
way to fix on its own. twigpui records the scope granted with each session
(and treats a session from before that existed as "unknown," never as "assume
it's fine"), so the header shows a **"Re-authorize"** button next to the
usual reload/sign-in controls whenever the current session is missing
`tweet.write`. Clicking it re-runs the same sign-in flow above end to end —
new browser consent screen, new tokens — and nothing else in the app changes
as a result.

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
| `XDG_STATE_HOME` | `~/.local/state/twigpui/` | `oauth_tokens.json` (mode `0600`), `rate_limit.json` (#10), `usage.json` (#18), `reposted_posts.json` (#15) |

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
request_price = 0.02
daily_request_budget = 500
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

## Posting (#14)

Signed in with OAuth and re-authorized per the "Scopes" note above, a
composer appears under the header: a draft area, a `weighted/280` character
counter, and a "Post" button. Submitting calls `POST /2/tweets`; success
clears the draft and falls into a normal reload (subject to the fetch
interval below, like any other reload) so the new post shows up; failure —
network error, rate limit, character limit, missing scope — leaves the draft
exactly as typed. There is no way to lose what you wrote by a request
failing.

**Typing (#38).** The draft area is a real text-entry widget from the
[`gpui-component`](https://crates.io/crates/gpui-component) crate
(`gpui_component::input::Input`, backed by `InputState`, which implements
gpui's `EntityInputHandler` properly) rather than twigpui reading raw key
events itself. IME composition — typing Japanese, Chinese, Korean, or any
other input method — works, along with cursor movement, text selection, and
copy/paste/undo/redo. The box grows with the draft (2–8 lines) instead of
scrolling a fixed height. It is still a plain text composer, not a full
client: there's no @mention/#hashtag autocomplete and no media attachments,
and Enter inserts a newline rather than submitting — only the "Post" button
does that.

**Character counting.** X's 280-character limit is enforced client-side
before anything is sent, using an approximation of X's own "weighted
length" — not the exact `twitter-text` algorithm. Any `http://`/`https://`
token counts as a flat 23 regardless of its real length (matching X's
own link-shortening), and characters in the common CJK/hangul/fullwidth
Unicode blocks count double; everything else counts as one character per
codepoint. This does not reproduce every range `twitter-text` weights
doubly (a few rarer supplementary-plane CJK extensions and symbol blocks
are left out), but it never *undercounts* relative to a plain character
count, so the one thing this exists to prevent — spending a request on a
post X will reject outright — still holds; the gap can only make it stop a
draft earlier than X's own counter would, never later.

**Double submission.** The submit button is only clickable at all while
there's something postable and nothing already in flight; a click sets the
composer to "Submitting" synchronously, before any network call starts, so
a second click before the first request resolves has nothing to do.

**Rate limiting.** `POST /2/tweets` is tracked as its own endpoint (#10) —
see "Rate limits" below.

## Quoting a post (#16)

Most posts show a "Quote" action alongside their text — see "Which posts
don't get one" below for the one exception. Clicking it doesn't send
anything by itself: it loads the composer with that post as the quote
target, rendered as a bordered card under the draft area — the same card
#13 already uses to show a quote in the timeline (see "Reposts and quotes
are expanded" under "Status" above). Typing and submitting from there works
exactly like an ordinary post — same character counter, same
double-submission guard, same "never lose the draft on failure" guarantee —
except the request now also carries `quote_tweet_id`. There is no separate
quote endpoint: `POST /2/tweets` accepts an optional `quote_tweet_id`, so
quoting reuses the same request `POST /2/tweets` already sends for a plain
post (see #14 above) and the same `Endpoint::CreatePost` rate-limit tracking
(#10) — a dedicated quote endpoint would only split that tracking
incorrectly, since X itself treats this as one endpoint. A post without a
quote never sends the field at all (not even as `null`) — only a post
actually being quoted carries it.

**Canceling a quote.** "Remove quote" appears next to the card and clears
just the quote target, leaving the draft text untouched — a mis-click on
"Quote" doesn't force discarding whatever was already typed. The draft
reverts to an ordinary post; submitting from there sends no
`quote_tweet_id` at all.

**Quoting your own post is allowed.** Unlike reposting (#15, below), X's API
doesn't reject quoting your own post, so twigpui doesn't check for it
client-side either.

**Which posts don't get one.** A post that is itself already a repost in
your timeline doesn't offer "Quote", for the same reason it doesn't offer
"Repost" (see "Which posts don't get a button" under "Reposting" below):
its own post id is the retweet activity's, not the original content's, and
`quote_tweet_id` needs the original. twigpui doesn't currently resolve that
original id for a displayed repost row (#52 tracks fixing that for both
buttons together), so it withholds the action there rather than risk
quoting the wrong post. Quoting the original post directly, wherever else
it appears in the timeline, is unaffected.

## Reposting and un-reposting (#15)

Most posts show a "Repost" / "Reposted" toggle — see "Which posts don't get
one" below for the two exceptions. Clicking "Repost" sends
`POST /2/users/:id/retweets`; clicking "Reposted" (to undo it) sends
`DELETE /2/users/:id/retweets/:source_tweet_id`. The button flips
immediately on click — optimistic update, no waiting on the network to see
something change — and reverts if the request fails. There is no
confirmation dialog: a repost is a reversible action, the same way no other
X client asks before one either.

**twigpui cannot tell whether you've already reposted a post from the API
itself.** X API v2's timeline response carries no field for this — unlike
v1.1's `retweeted`, there is no v2 equivalent — and checking per-post via
`GET /2/tweets/:id/retweeted_by` would cost one request per visible post,
which is out of the question for an app whose entire cache exists to avoid
spend. So twigpui keeps its own local record instead:
`$XDG_STATE_HOME/twigpui/reposted_posts.json`, holding every post id *this
app* has reposted.

**Reposts made in other clients — the official app, the web, anywhere but
here — are never reflected.** The button can only ever show what twigpui
itself has done; a post you reposted from your phone still shows "Repost"
here. This is a deliberate, accepted tradeoff for a workable button state
at zero request cost, not a bug. Losing `reposted_posts.json` costs more
than a lost cache entry would: every post reposted before the loss reverts
to showing "Repost" again, so clicking it risks sending a duplicate — see
"Recovering from a stale record" below for why that's recoverable rather
than silently wrong.

**Recovering from a stale record.** If the local record disagrees with
reality — reposting something already reposted, or un-reposting something
that isn't — the API returns an error rather than silently succeeding.
twigpui recognizes that specific conflict, matched case-insensitively
against the API's own error text ("already retweeted" / "have not
retweeted"), and corrects the local record to match instead of showing it
as a failure. Any other error rolls the button back to its state before the
click and shows the message, offering a retry.

**Which posts don't get a button.** Your own posts don't — the API rejects
reposting yourself, and twigpui checks this client-side first rather than
spending a guaranteed-failing request. A post that is itself already a
repost in your timeline doesn't either: it renders with the *original*
post's text and author (see "Reposts and quotes are expanded" above), but
its own post id is still the retweet activity's, not the original content's
— and the repost endpoints act on the original. twigpui doesn't currently
resolve that original id for a displayed repost row, so it withholds the
button there rather than risk sending the wrong id. Reposting the original
post directly, wherever else it appears in the timeline, is unaffected.

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

Reposting spends one request; un-reposting spends one more.

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
  timeline (#11), `GET /2/tweets?ids=` (#12, "Show thread"),
  `POST /2/tweets` (#14, posting), and reposting/un-reposting (#15,
  `POST`/`DELETE /2/users/:id/retweets…`, tracked as two separate endpoints
  since X limits creating and deleting a repost independently) are all
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

## Usage tracking (#18)

The X API bills per request against prepaid credits (see "API cost" above),
but until now there was no way to see what had actually been spent short of
hitting `429`. twigpui now counts every request it actually sends and
persists the counts, so the running total is visible both in the window and
from the command line.

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
and persistence (#10), the scope check behind #14's "Re-authorize" button
(sufficient / insufficient / never-recorded scope), that an old-format
`TokenSet` with no `scope` field at all still deserializes rather than
logging the user out, the composer's weighted-length character counter and
draft validation (boundary length, over-limit, empty, whitespace-only), its
submit state machine (a failed submit keeps the draft and stays retryable;
a successful one clears it), the `POST /2/tweets` request body, the local
repost record's roundtrip and corruption recovery, the create/delete repost
URLs and their independently tracked rate-limit endpoints, the repost
button's optimistic-update state machine (including that a failed toggle
rolls back to exactly its pre-click value, and that a reconciled outcome
can commit a value that disagrees with the optimistic guess), the
conflict-message interpretation behind #15's "already reposted"/"not
reposted" recovery, #16's `quote_tweet_id` request body (both with and
without a quote, since the field must be entirely absent — not `null` —
for an ordinary post), the composer's quote-target state machine (setting,
clearing without touching the draft, and that a failed submit keeps the
draft and the quote target together while a successful one clears both),
and which posts offer the "Quote" action (withheld on a repost row, for the
same `item.id`-ambiguity reason as #15's repost button, but allowed on
one's own post unlike reposting), so they make no network calls, open no
browser, and spend no credits. The actual code exchange, X's live response
shapes (including `/users/me`, the home timeline's `meta.next_token`, #13's
`referenced_tweets` expansion, #12's `GET /2/tweets?ids=` response shape,
#14's live `POST /2/tweets` response, #15's live repost/un-repost responses
and their exact conflict-error wording, and #16's live response to a
`quote_tweet_id`-carrying post — including whether X accepts quoting your
own post the way its documentation implies), refresh-token rotation, and
the real rate-limit header values X sends aren't covered by tests — those
need a real Developer Portal registration, a one-time manual sign-in, and
(for repost) actually hitting a live rate limit or a live already-reposted
conflict.
