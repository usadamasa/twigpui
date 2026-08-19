<p align="center">
  <img src="assets/AppIcon.png" alt="twigpui" width="180" height="180">
</p>

<h1 align="center">twigpui</h1>

<p align="center">
  A development-only X (Twitter) timeline viewer, built with Rust and
  <a href="https://crates.io/crates/gpui">gpui</a>.<br>
  macOS only — no other platform is considered.
</p>

## Status

The app shows your own home timeline —
`GET /2/users/:id/timelines/reverse_chronological` for the id
`GET /2/users/me` resolves — in a scrollable list with a reload button. A
"Load older" button pages further back via `meta.next_token`.

**Signing in with X is now the only way to authenticate** (#33). The
app-only bearer token was removed: it could not read the home timeline and
could not post, repost, quote, like or delete, which is most of what this
app does. See "Migrating from the bearer token" below if you were using one.

`--fetch-only` still fetches `X_TARGET_USERNAME`'s posts via
`GET /2/users/:id/tweets`; that endpoint works fine with an OAuth token, and
dropping the app-only *credential* is not the same as dropping the
single-user *view*.

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

**Reply, repost and like counts (#67).** Each post shows how much
engagement it got — `12 replies · 34 reposts · 5.6K likes` — in a muted line
under the body. Like the reply context above, this costs no extra request:
both timeline requests simply add `public_metrics` to `tweet.fields`, so the
numbers arrive inside the response already being paid for. Counts that are
zero are left out, and a post with no engagement at all gets no line, so a
fresh timeline is not a wall of zeros. Large numbers are abbreviated
(`12.3K`, `2.4M`) to keep a popular post from pushing the byline around.

Note that these are a **snapshot from when the post was fetched**, and
nothing refreshes them. A reload asks for posts newer than the newest one on
file (`since_id`), so a row already in the cache is never returned again and
keeps the counts it arrived with. Reposts show the *original* post's counts,
matching the body, which is the original's text too.

`--fetch-only` runs the same fetch headlessly (always the single-user view,
regardless of credential) and prints the posts, which is useful for checking
credentials without opening a window:

```sh
cargo run -- --fetch-only
```

**Fetching one specific post: `--fetch-post` (#42).** Sometimes what's
wanted isn't the timeline at all, but a single post referenced from
elsewhere — e.g. so a Claude Code session can read a post's text, since
`x.com` itself returns 402 to `WebFetch` and a human would otherwise have to
paste the text in by hand. `--fetch-post` takes a post id, a full status URL
(`https://x.com/<user>/status/<id>` or the `twitter.com` alias), or a
comma-separated list of either, and prints the fetched post(s) as JSON to
stdout — no window, no human-readable mode, since the point is a tool
reading the output, the same reasoning `--usage` already applies to its own
output:

```sh
cargo run -- --fetch-post 1700000000000000001
cargo run -- --fetch-post https://x.com/jack/status/20
cargo run -- --fetch-post 20,30,40
```

Every id goes into a single `GET /2/tweets?ids=` request — X's own query
parameter already accepts a comma-separated list, so fetching several posts
at once still costs exactly **one** request, reported on stderr along with
how many of the requested ids actually came back (a missing one is usually
deleted or protected). Each printed post carries the same repost/quote/reply
context (`reposted_by`/`quoted`/`replied_to`) the timeline itself joins in
(#12, #13), at no extra request cost.

`--fetch-post` never touches the timeline cache (#9) — nothing is read from
it, nothing is written to it. That cache exists to avoid re-fetching the
same account's timeline on every reload, a repeated-access pattern an
arbitrary post id doesn't share: it's typically looked up once, from
wherever it was linked, so the simplest defensible choice is to always spend
the one request and never persist the result.

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

**Icon.** `assets/AppIcon.png` is the source (#85, the crab holding a bird
at the top of this file). The script resizes it into the ten sizes
`iconutil` wants and writes the `.icns` into the bundle — `sips` and
`iconutil` both ship with macOS, so there is nothing to install.

`assets/AppIcon.icns` takes precedence if you drop one in, and is used
as-is. With neither file present the bundle simply carries no
`CFBundleIconFile` and macOS shows the generic app icon — the script never
writes a dangling reference to a file it did not copy in.

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
- **A `bearer_token` left in `config.toml` is rejected outright** (#33).
  The key is no longer read, and silently ignoring it would leave you
  believing you are configured when nothing reads it — so startup fails with
  a message naming `oauth_client_id` as the replacement. The value itself is
  never echoed into that message.
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

## Setup

An OAuth client id is required. There is no other credential (#33).

**Recommended: put `oauth_client_id` in `config.toml`.** It's the only
non-secret credential twigpui has (a public OAuth client has no client
secret), so it is fine to check into a dotfiles repo — and unlike an
exported environment variable, it's picked up no matter how
twigpui is started. An exported `X_OAUTH_CLIENT_ID` is invisible to a
different terminal, a fresh shell that never sourced your profile, and a
`.app` launched from Finder/Spotlight/Dock (#40) — none of which inherit your
shell's environment. That gap is exactly how #54 happened: a stored session
expired, couldn't refresh without the client id in that particular shell, and
the app quietly kept working in a degraded, read-only mode with nothing on
screen explaining why.

```sh
mkdir -p ~/.config/twigpui
cat >> ~/.config/twigpui/config.toml <<'EOF'
oauth_client_id = "…"
EOF
cargo run
```

(`~/.config/twigpui/config.toml` is the default path —
`$XDG_CONFIG_HOME/twigpui/config.toml` if that's set. See "`config.toml`"
below for the full path resolution and every other key the file accepts.)

Or keep a local `.env`, which `dotenvy` loads into the environment, if
you'd rather not use `config.toml`:

```sh
cp .env.example .env
$EDITOR .env          # fill in X_OAUTH_CLIENT_ID
cargo run
```

### Migrating from the bearer token (#33)

`X_BEARER_TOKEN` is gone. If that is what you had configured:

1. Set `X_OAUTH_CLIENT_ID`, or add `oauth_client_id = "…"` to
   `config.toml` — the client id of a public OAuth client from the X
   Developer Portal. It is not a secret.
2. Remove any `bearer_token` key from `config.toml`. Startup fails while it
   is still there, on purpose: ignoring it would leave you believing you are
   configured when nothing reads it.
3. Run twigpui and click **Sign in with X** once. The session persists to
   `$XDG_STATE_HOME/twigpui/oauth_tokens.json`, and every later launch —
   including `--fetch-only` and `--fetch-post`, which never open a browser —
   reuses it.

App-only access could not read the home timeline (401) and could not post,
repost, quote, like or delete. Keeping it meant a second credential path
through config resolution, the timeline source, and every affordance in the
header, in exchange for a strictly less capable app.

An environment variable always overrides the same key in `config.toml` (see
"`config.toml`" below) — handy for a one-off override, but not a substitute
for putting `oauth_client_id` in the file if you want it to survive across
terminals and launch methods without having to remember to export it again
each time.

| Variable | Required | Default | Meaning |
| --- | --- | --- | --- |
| `X_OAUTH_CLIENT_ID` | **yes** | — | OAuth 2.0 client id for "Sign in with X" — non-secret, may also live in `config.toml` as `oauth_client_id` |
| `X_TARGET_USERNAME` | no | `XDevelopers` | Screen name `--fetch-only` fetches, without a leading `@` |
| `X_MAX_RESULTS` | no | `20` | Posts per fetch, 5–100 |
| `X_MIN_FETCH_INTERVAL_SECONDS` | no | `60` | Floor on how often a fetch may run, in seconds (#10) |
| `X_THEME` | no | `light` | Color theme: `light`, `dark`, or `system` (follows the OS appearance) — also `theme` in `config.toml` (#19) |
| `X_REQUEST_PRICE` | no | unset | Price per API request, in whatever unit you have in mind — also `request_price` in `config.toml` (#18, see "Usage tracking" below) |
| `X_DAILY_REQUEST_BUDGET` | no | unset | Daily request-count budget that colors the header's usage line as it's approached — also `daily_request_budget` in `config.toml` (#18) |

`.env` is gitignored. Do not commit credentials.

## Signing in with X (OAuth 2.0 Authorization Code + PKCE)

Everything this app does — the home timeline, posting, replying, liking,
deleting — acts as *you* rather than reading public data, so all of it needs
a user-context OAuth 2.0 session. twigpui gets one with the Authorization
Code + PKCE flow (RFC 6749 + RFC 7636), run entirely from the window via a
"Sign in with X" button.

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
public client has no secret to protect — so it is fine to check into a
dotfiles repo.

**Scopes.** twigpui requests `tweet.read users.read tweet.write like.write
offline.access`: enough to read posts, resolve user context, post (#14),
like (#68), and refresh the session without re-prompting.

**If you signed in before #14 or #68,** your stored session predates
`tweet.write` or `like.write` and can't post or like yet — the API rejects
the write with a 403 it has no way to fix on its own. twigpui records the
scope granted with each session (and treats a session from before that
existed as "unknown," never as "assume it's fine"), so the header shows a
**"Re-authorize"** button next to the usual reload/sign-in controls whenever
the current session is missing any write scope the app needs. Clicking it
re-runs the same sign-in flow above end to end — new browser consent screen,
new tokens, every scope at once — and nothing else in the app changes as a
result.

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
| `XDG_CACHE_HOME` | `~/.cache/twigpui/` | Response cache: `user_ids.json`, `timeline-<user_id>.json` (#9), `me.json`, `home-timeline-<user_id>.json` (#11), `thread-<reply_id>.json` (#12), `avatars/` (#64), `media/` (#65) |
| `XDG_STATE_HOME` | `~/.local/state/twigpui/` | `oauth_tokens.json` (mode `0600`), `rate_limit.json` (#10), `usage.json` (#18), `reposted_posts.json` (#15), `liked_posts.json` (#68), `logs/` (#49) |

An `XDG_*` variable is only honored if it is set to a non-blank absolute
path; a relative or blank value falls back to the default, per spec.

### `config.toml`

`$XDG_CONFIG_HOME/twigpui/config.toml` is an optional, hand-edited settings
file with the same keys as the environment variables above:

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
so an env var always wins over the file. `oauth_client_id` is safe to check
in — it is a public client id, not a secret.

A `bearer_token` key is **rejected** rather than ignored (#33): the
credential it named no longer exists, and a file that still carries it would
otherwise look configured while nothing read it.

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
before anything is sent, using X's own "weighted length" rule (per the
open-source `twitter-text` library): any `http://`/`https://` token counts
as a flat 23 regardless of its real length (matching X's own
link-shortening), and every codepoint counts as 1 if it falls in a short
list of "low weight" ranges (plain ASCII, Latin-1, the rest of Latin
Extended/Greek/Cyrillic, and a few punctuation ranges) or 2 otherwise —
CJK ideographs, hangul, hiragana/katakana (fullwidth and halfwidth alike),
fullwidth forms, emoji, and everything else not on the low-weight list
(#61).

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

**Quoting a repost row quotes the original** (#52) — which is also the text
and author the quote card would show, since that is what the row renders.

## Deleting your own posts (#72)

Your own posts show a "Delete" action; nobody else's does, since X rejects
deleting someone else's and there is no point spending a request that can
only fail.

**Deleting takes two clicks, never one.** "Delete" replaces itself with
"Delete permanently" / "Cancel". The action is irreversible, so no single
click can destroy a post. Only one row can be asking at a time — clicking
"Delete" elsewhere moves the prompt rather than opening a second one.

**It leaves the cache too.** A post deleted from X but left in
`timeline-<id>.json` disappears from the window and comes back on the next
start: the app looking like it worked when it didn't. So a successful
delete rewrites the cache file and then **reads it back**, rendering what is
actually on disk rather than what was just written. The local cache is only
touched after X has confirmed the deletion — forgetting it first would hide
a post that still exists.

**A repost row gets no Delete**, unlike every other action since #52. Such
a row displays someone's original post, so on a repost of your *own* post
the delete would destroy the original from a row that reads as "my repost".
Removing a repost is the repost toggle's job (#15), and conflating the two
on an irreversible action is not worth the risk.

`DELETE /2/tweets/:id`, `tweet.write` scope, one request, counted as spend
like every other write.

## Replying (#71)

Every post shows a "Reply" action. Clicking it sets the composer's target
and nothing else — no request goes out until the draft is submitted, the
same way "Quote" works. The composer then shows "Replying to @someone"
above a card of the post being answered, with "Remove reply" to clear it
without losing what was already typed. Submitting sends the same
`POST /2/tweets` as an ordinary post, with a nested
`reply.in_reply_to_tweet_id`; the scope (`tweet.write`) and the cost (one
request) are identical.

**A draft is a reply or a quote, never both.** X's API would accept one that
is both, but this composer refuses to build it: the two look almost
identical in a small composer, and sending the wrong one is not a visible
mistake — a reply lands under a conversation, a quote does not. Clicking
"Reply" while a quote is set is therefore a switch, not an addition, and the
draft text survives either way.

**Replying from a repost row answers the original post** (#52) — the id sent
as `in_reply_to_tweet_id` is the original's, not the retweet activity's.
Getting that wrong would hang the reply off a different conversation
entirely, with nothing about the failure visible afterwards.

**Nothing is re-fetched afterwards beyond the usual reload.** "Show thread"
(#12) walks a post's *ancestors*, and a new reply is a descendant, so no
cached chain becomes stale by posting one. The reply itself shows up in the
timeline reload that every successful post already triggers.

## Reposting and un-reposting (#15)

Every post but your own shows a "Repost" / "Reposted" toggle — see "Which
posts don't get a button" below. Clicking "Repost" sends
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
spending a guaranteed-failing request. For a repost row that check looks at
the *original* author, since that is whose post would actually be reposted.

**Repost rows are operable (#52).** A row that is itself a repost renders
with the original post's text and author, but its own post id is the
retweet activity's, not the original content's — and every write endpoint
acts on the original. `TimelineItem` now carries that original id,
populated from the same `referenced_tweets` reference the expansion already
used, so Repost, Quote and Like all work on a repost row and send the right
id. Before this they were withheld there, which mattered: a home timeline
is mostly reposts.

## Attached images (#65)

A post's attached images render as thumbnails under its text. Videos and
animated GIFs show their still with a "Video" / "GIF" badge — neither plays
here; that is deliberately out of scope. Clicking a thumbnail opens the full
image in the browser (#70), since this app has no lightbox.

`attachments.media_keys` and the `media.fields` join ride along in the
timeline request already being made, so **no extra request and no extra
cost** — only a larger response. The images themselves come from
`pbs.twimg.com` and share the download-once cache layer with avatars (see
below), under `$XDG_CACHE_HOME/twigpui/media/`.

**Thumbnail cells are a fixed height**, not sized from each image's own
`width`/`height`. A row whose height depends on which images have finished
downloading reflows under the reader as they land, which is worse than
showing frames waiting to be filled. One image renders across a single
column, two or more in two columns — three across would each be too narrow
to read, and X's own maximum of four is two rows of two.

**Alt text is shown, not hidden behind a hover.** This app has no
screen-reader path of its own, so alt text a sighted reader can actually see
is more use than alt text nobody ever reaches.

## Author avatars (#64)

Each row shows the author's profile image, 44pt and circular, to the left of
the byline. `user.fields=profile_image_url` rides along in the timeline
request that was already being made, so the URL costs nothing extra; the
image itself is fetched from `pbs.twimg.com`, which is **not** the X API —
no quota, no credits, nothing for the usage tracking to count.

**Downloaded once, off the UI thread.** Images are cached under
`$XDG_CACHE_HOME/twigpui/avatars/`, keyed by a hash of the URL (X reuses the
same basename across accounts, so anything shorter would collide). A
timeline where one author posts ten times downloads one image. Fetching runs
on the background executor one URL at a time, and each avatar appears as it
lands rather than the timeline waiting for the slowest.

**Until then — or if it fails — a placeholder** the exact same size holds
the space, carrying the author's initial. A row never reflows when an image
arrives, and an author whose name never expanded gets the bare circle rather
than an invented character. A failed download is simply left absent and
retried on the next reload; there is nothing useful to tell the user about
an avatar that didn't load.

**Size.** X's `profile_image_url` ends in `_normal` (48×48), which blurs on
a Retina display, so twigpui asks for the `_400x400` variant instead. That
suffix convention is X's own and not promised by the API, so a URL that
doesn't match is used unchanged, and a rewritten URL that fails falls back
to the original rather than leaving the row without a face.

Losing the avatar cache costs one re-download per author and nothing else —
it is cache, not state.

## Keyboard shortcuts (#58)

| Key | Action |
| --- | --- |
| `⌘R` | Reload |
| `⌘↩` | Post the draft |
| `⌘N` | Focus the composer |
| `esc` | Leave the composer (the draft is kept) |

The same list is shown on screen, under the header — four bindings fit on
one line, and a help screen nobody opens documents nothing.

**Every binding carries a modifier, deliberately.** The hazard #58 is really
about is a bare `j`/`k`/`n` firing while you are typing a post; nothing
bound today can. Bare keys become worth having once posts can be selected,
and that needs a second key context the composer's focus removes — see
`ui::KEY_CONTEXT`.

**`⌘↩` posts; plain `↩` does not.** Enter has to keep inserting a newline,
and a post is not undoable.

**`esc` moves focus only.** The draft is left exactly as typed: losing it to
a stray key is unrecoverable, and never losing a draft is the composer's
main promise (#14).

**"Load older" has no shortcut.** Each press pages backwards for one paid
request, and a key that spends money on a mis-hit is not a convenience.
`⌘R` does spend requests, but it is the reload gesture every app shares and
not one anyone hits by accident — and it goes through the same throttle
(#10) and cooldown reporting (#57) as the button, so a held-down `⌘R`
cannot outrun the interval that exists to stop this app spending in a loop.

## Opening things in a browser (#70)

Every post row carries three ways out of the app, and none of them costs an
API request — the URLs are built from what the timeline response already
carried:

- **"Open in X"** on the byline row opens the post itself
  (`x.com/{handle}/status/{id}`; for a post whose author never expanded,
  X's own id-only form `x.com/i/web/status/{id}`, which resolves the author
  server-side rather than leaving the row with no way out).
- **The author's name** opens their profile (`x.com/{handle}`). It stays
  plain text when the handle never expanded, since there is no id-only
  fallback to point at.
- **Links in the post's text** appear as clickable lines under the body.

**Why the links sit under the text rather than inside it.** X's post text
carries `t.co` shortlinks, and the real destination only comes from
`entities.urls[].expanded_url` — which #70 adds to `tweet.fields`, so it
arrives inside the request already being paid for. Making a link clickable
*in place* would mean splitting the body into interleaved text and link
elements, and gpui lays each child out as its own block, so the paragraph
would stop wrapping as one piece. Each line is labelled with X's own
`display_url` (`example.com/a/b…`), so what is shown matches what the text
says even though what opens is the expanded destination. An entity with no
`expanded_url` (a media attachment's own `t.co`, for instance) is dropped
rather than shown as a link back to the shortlink.

**How it opens.** `open(1)` is invoked directly through
`std::process::Command` — never through a shell, so nothing in a URL that
came from a post can be read as syntax — and only `http://` and `https://`
URLs are handed to it. That last part is not decoration: `open` will act on
a local path, on another app's registered scheme, and above all on a
leading `-`, which it would read as one of its own flags. A URL that fails
that check, or a browser that fails to launch, shows a banner rather than a
click that silently does nothing.

## Liking and unliking (#68)

Most posts show a "Like" / "Liked" toggle. Clicking "Like" sends
`POST /2/users/:id/likes`; clicking "Liked" (to undo it) sends
`DELETE /2/users/:id/likes/:tweet_id`. It behaves exactly like the repost
toggle above — optimistic flip on click, rollback on failure, no
confirmation dialog — and shares its machinery with it (`src/toggle.rs`).

Everything the repost section says about the local record applies here
verbatim, with `liked_posts.json` in place of `reposted_posts.json`: X API
v2 reports no "did I like this" field either, likes made in other clients
are never reflected, and a stale record is corrected from the API's own
error text ("already liked" / "have not liked") rather than surfacing as a
failure.

**Two differences from reposting.** Your own posts *do* get a Like button —
X rejects reposting yourself but accepts liking yourself. And liking needs
the `like.write` scope, which X grants separately from `tweet.write`: a
session authorized before #68 can post and repost but not like, and the
header's "Re-authorize" button (#14) is what fixes it. Re-running the
sign-in flow requests every scope at once.

## Logs (#49)

`$XDG_STATE_HOME/twigpui/logs/twigpui.log`, with one rotated predecessor at
`twigpui.log.1`. `state`, not `cache`: a log deleted on the next boot
answers no questions about what happened yesterday.

```sh
tail -f ~/.local/state/twigpui/logs/twigpui.log
```

**This exists because a `.app` has no stderr.** Launched from Finder or
Spotlight, everything the app has to say goes nowhere (#40, #45). The
startup alert covers exactly one case — "it did not start" — and nothing at
all for a session that starts fine and then misbehaves. Run from a terminal,
messages still go to stderr *as well*, so `cargo run` behaves as it always
did.

**Tokens never reach the file.** Every message is redacted before it is
written: `Bearer <token>`, and any `access_token` / `refresh_token` /
`client_secret` / `token` / `code` / `state` value in a query string or JSON
body, all become `[redacted]`. The redactor is deliberately blunt —
over-redacting costs a confusing log line, missing costs a credential on
disk, and that failure is silent and permanent. Tests are the guarantee, not
the care of whoever writes the next call site. The file is also created
`0600`, matching the token store (#7).

**It cannot grow without bound.** At 1 MiB the current log is moved to
`.log.1` and a fresh one starts; exactly one previous generation is kept.
`~/.local/state` is not swept by macOS, so an uncapped log is a slow leak —
the same reasoning as #9's cache cap.

**Level** comes from `TWIGPUI_LOG` (`error`, `warn`, `info`, `debug`), or
from `log_level` in `config.toml` when that is unset, defaulting to `info`.
The `config.toml` setting is the one that matters for a bundled `.app`,
which never sees an environment variable set in a shell. An unrecognized
value warns and falls back rather than blocking startup, exactly like
`theme`.

**No logging framework.** `tracing` plus a subscriber and an appender is a
large tree to compile on every build (#46 is an open issue about exactly
that) for a line with a level, a timestamp and a size cap. The headless
`--fetch-only` / `--fetch-post` / `--usage` paths keep writing to stderr
directly: they only ever run from a terminal, and a one-shot command has no
business leaving a file behind.

## Code metrics (#48)

```sh
bash scripts/code-metrics.sh
```

Prints file sizes (split into implementation and test lines), the fifteen
longest functions, and any `clippy::cognitive_complexity` hits. CI runs it
in the Lint job and writes the output to the run summary.

**One metric gates: file size.** `scripts/code-metrics.sh --check` compares
each file's implementation lines against `metrics-baseline.tsv` and fails if
one is over its ceiling — or missing from the baseline entirely, so new code
cannot dodge the check by living in a new file. CI runs it.

The ceilings are today's numbers rounded up to the next 50, not targets.
Growth stays possible; it just cannot be silent, because crossing a ceiling
means editing `metrics-baseline.tsv` in the same pull request, where a
reviewer sees it. Lower a ceiling whenever a file shrinks below it.

The other two metrics report only. Function length is already enforced by
`clippy::too_many_lines` (denied via `pedantic`, #47) and cognitive
complexity has no hits at clippy's default threshold — gating either would
add a second check for something already checked, or for nothing.

Built from bash, awk and clippy alone rather than a metrics tool, because
every such tool needs an install step on every CI run and build time is a
live concern (#46). When one earns that time, this script is what it
replaces.

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

Reposting spends one request; un-reposting spends one more. Liking and
unliking (#68) cost the same, one request each, and so does deleting a post
(#72).

`--fetch-post` (#42) spends exactly one request per run, however many post
ids are given — they all ride in a single `GET /2/tweets?ids=` request's
comma-separated `ids=` parameter, never one request per id.

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
logging the user out, which of #54's three reasons a stale stored session
gets demoted for (no `oauth_client_id` configured, no refresh token on the
session, or X rejecting an attempted refresh outright) and the message
`describe_demotion` builds for each (including that it names the resolved
`config.toml` path for the no-client-id case, since that's the one with an
actual file to point at), the composer's weighted-length character counter and
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
conflict. X actually rejecting a refresh attempt (#54's `SessionDemotion::Rejected`
branch inside `resolve_credential` itself, as opposed to `describe_demotion`'s
formatting of it, which is tested) is in the same boat — it needs the token
endpoint to actually answer with a revoked-or-expired-beyond-recovery error,
which nothing in this test suite can trigger without a live session.
