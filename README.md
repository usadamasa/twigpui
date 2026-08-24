<p align="center">
  <img src="assets/AppIcon.png" alt="twigpui" width="180" height="180">
</p>

<h1 align="center">twigpui</h1>

<p align="center">
  A development-only X (Twitter) timeline viewer, built with Rust and
  <a href="https://crates.io/crates/gpui">gpui</a>.<br>
  macOS only — no other platform is considered.
</p>

## What it does

- Shows your home timeline in a scrollable window, with a reload button and
  a "Load older" button that pages further back.
- Expands reposts and quotes inline, so a repost shows the original text
  rather than a truncated `RT @user: …`.
- Shows reply context ("Replying to @someone") for free, and offers an
  opt-in "Show thread" walk up the parent chain that spells out its worst
  case in requests before you click it.
- Shows reply, repost and like counts as a snapshot from fetch time.
- Posts, replies, quotes, reposts, likes and deletes.
- Renders attached images and author avatars.
- Opens a post, an author, or a prefilled composer in your browser.
- Runs headless: `--fetch-only` prints a single user's posts,
  `--fetch-post` prints one or more posts as JSON, `--usage` prints what the
  API has cost so far.
- Opens the window on a file instead of an account with
  `--fixture fixtures/timeline.json` — no credential, no request, the same
  screen every run (see [docs/operations.md](docs/operations.md)).

Signing in with X is the only way to authenticate. The app-only bearer token
was removed: it could not read the home timeline and could not post, repost,
quote, like or delete, which is most of what this app does.

## Documentation

| File | Contents |
| --- | --- |
| [docs/timeline.md](docs/timeline.md) | What the window shows and how a row is assembled |
| [docs/writing.md](docs/writing.md) | Posting, replying, quoting, reposting, liking, deleting, opening in a browser |
| [docs/media.md](docs/media.md) | Attached images and author avatars |
| [docs/operations.md](docs/operations.md) | Logs, code metrics, tests |
| [.claude/skills/app-bundle](.claude/skills/app-bundle/SKILL.md) | Building the `.app` bundle, release and development |

## Requirements

The `macos-blade` feature is enabled so the build does not need `xcrun metal`,
which ships with full Xcode rather than the Command Line Tools. Rendering goes
through blade instead.


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

### Migrating from the bearer token

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
| `X_LIST_ID` | no | unset (a development build defaults to its own list, #169) | Numeric id of an X List to show in the window **instead of** the home timeline — also `list_id` in `config.toml` (#161) |
| `X_MIN_FETCH_INTERVAL_SECONDS` | no | `60` | Floor on how often a fetch may run, in seconds (#10) |
| `X_THEME` | no | `light` | Color theme: `light`, `dark`, or `system` (follows the OS appearance) — also `theme` in `config.toml` (#19) |
| `X_REQUEST_PRICE` | no | unset | Price per API request, in whatever unit you have in mind — also `request_price` in `config.toml` (#18, see [Usage tracking](.claude/skills/x-api-budget/reference/app-behavior.md#usage-tracking)) |
| `X_DAILY_REQUEST_BUDGET` | no | unset | Daily request-count budget that colors the header's usage line as it's approached — also `daily_request_budget` in `config.toml` (#18) |
| `X_AUTO_SYNC_LIST` | no | `true` | Keep `X_LIST_ID`'s membership mirroring your follows while the app runs — also `auto_sync_list` in `config.toml`. **Spends on a timer**; see below |
| `X_SYNC_INTERVAL_SECONDS` | no | `21600` (6h) | How long the background sync waits between diffs. Values under `900` are rejected — also `sync_interval_seconds` in `config.toml` |
| `X_SYNC_PRUNE_LIMIT_PERCENT` | no | `10` | The most of the list's membership the background sync may remove per diff, in percent. Over it, the removals are held for `--sync-list --apply --prune` to confirm; `100` turns the cap off — also `sync_prune_limit_percent` in `config.toml` (#176) |
| `X_SYNC_WRITES_PER_MINUTE` | no | `2` | How many list writes the background sync sends per minute during a catch-up (#197). `1`–`20`; the ceiling is X's documented write window (300 per 15 min) spread evenly. Raise it only after a run at the default has shown no refusals — also `sync_writes_per_minute` in `config.toml` |
| `X_AUTO_REFRESH` | no | `true` | Poll the timeline for new posts while the window is open — also `auto_refresh` in `config.toml` (#21). `false` and the app sends nothing you did not click |
| `X_AUTO_REFRESH_INTERVAL_SECONDS` | no | `300` (5m) | How long auto-refresh waits between polls. Values below `X_MIN_FETCH_INTERVAL_SECONDS` are rejected — also `auto_refresh_interval_seconds` in `config.toml` |

`.env` is gitignored. Do not commit credentials.

### Auto-refresh

The window polls its timeline every five minutes and, when a poll brings
posts you have not seen, offers them as an **"↑ N new posts"** bar between
the toolbar and the list. Nothing moves until you press it — not the list,
not your scroll position. Pressing it (or `⌘⇧R`, or View → Show New Posts)
shows them and jumps to the top.

That is the deliberate half of the design. A fetch you did not ask for must
not slide the text you are reading down the screen, so a poll never touches
what is displayed; it only fills a buffer the bar offers.

**The cost is smaller than a five-minute timer sounds.** Reads are billed
per post returned and deduplicated within a UTC day, so a day of polling
bills the posts that were genuinely new that day — which is what reading
them costs however they arrive. The one repeated charge is the first head
page after each UTC midnight, bounded by `X_MAX_RESULTS`.

`⌘R` and `⌘⇧R` are deliberate opposites where money is concerned: `⌘R`
buys a fetch, `⌘⇧R` reveals one the timer already paid for.

Turn it off with `X_AUTO_REFRESH=false`. There is no timer left running
behind that switch — the loop is never started at all.

### The background list sync

With `X_LIST_ID` set, the window keeps that list's membership mirroring the
accounts you follow, for as long as it is open. Accounts you follow are
added; accounts you no longer follow are removed. It is on by default and
turned off with `X_AUTO_SYNC_LIST=false`.

**It removes accounts you added to the list by hand.** The list *is* the
mirror — that is the whole contract. If you want a list you curate
yourself, either turn the sync off or point `X_LIST_ID` at a different
list.

**The status bar says what it is doing** (#174). "List sync: up to date"
is the steady state; "List sync: 1100 to go" is a catch-up working
through its plan; "List sync: no list configured" and "List sync:
re-authorize to enable" name the gate a stopped sync is stopped at.
Before this the feature was invisible from the window, and a catch-up
that had hours left looked exactly like nothing happening.

**Clicking it starts a sync**, in any state where one can start. It asks
first, because the reads it buys are the most expensive click in this
app. A sync started this way ignores the interval — that is the point of
the button — but not the rate limit, and not an outstanding plan: if a
catch-up is already part way through, pressing it resumes that plan
rather than paying to diff both sides again.

With `X_AUTO_SYNC_LIST=false` the timer never runs and the button is the
only way a sync happens. That run stops once there is nothing left to do.

**It spends money on a timer.** Every diff reads your whole follow list and
the whole list membership, and both are billed per account returned. At
four diffs a day that is roughly $2 per thousand follows if X's documented
24-hour deduplication covers these reads, and roughly $8 if it does not —
`x-api-budget` has that rule measured for Posts only. This is why the
interval defaults to six hours and refuses anything under fifteen minutes.

The writes are spread out rather than sent in a burst: two a minute by
default (`sync_writes_per_minute`), so a list that is thousands of
accounts behind is caught up over hours rather than minutes. That default
is deliberate. X refuses list additions with a cap
its `x-rate-limit-*` headers do not describe (#193, #197): a burst of
roughly a hundred in twenty minutes tripped it, and it then refused every
write for more than a day. A refusal pauses the catch-up rather than
failing it, and each refusal in a row waits longer than the last — 15
minutes, then 30, an hour, two, four, and six hours from then on — so a
cap that stays down costs a handful of rejected writes a day, not one
every quarter hour. The status bar says "rate limited" for the first
refusal and "refused N× in a row" in red from the second, because twenty
hours of a countdown that keeps restarting looks like waiting, and it is
not. The log has one line per refusal with the 429's headers and body.

Progress is written to `$XDG_STATE_HOME/twigpui/sync_plan.json` after
every single change, so quitting mid-catch-up costs nothing — the next
launch picks up exactly where it stopped.
`$XDG_STATE_HOME/twigpui/sync_state.json` holds when the last diff ran,
which is what stops a relaunch from paying for both reads again, and the
backoff — until when writes are paused and how many refusals in a row —
so a relaunch does not send into the cap either.

It needs the same scopes `--sync-list` does. A session that predates them
is skipped with a line in the log rather than an error on screen — click
"Re-authorize" once and the sync starts without a restart.

The interval is counted from the last diff, not from launch, and that count
is kept on disk. Restarting the app does not trigger a sync: if the last one
ran an hour ago, the next is still five hours out.

A debug build syncs too, and none of the costs above apply to it (#169): it
mirrors the fixed seed screen names into the development list rather than
reading your follow graph, against its own `twigpui-dev` state directory. The
figures in this section are what a `--release` build spends.

### `--sync-list` — mirror your follows into the list (#163)

A List only shows what is in it, so #161's window is only as good as the
list's membership. `--sync-list` diffs the accounts you follow against the
list's members and mirrors one onto the other.

```sh
cargo run --release -- --sync-list          # dry run: read both sides, write a plan, print it
cargo run --release -- --sync-list --apply  # send the additions
cargo run --release -- --sync-list --apply --prune   # …and the removals
```

**`--release` is load-bearing here** (#169). A debug build is the
development profile: it syncs *its* list from four fixed X accounts, not
your list from your follows. Dropping `--release` does not fail — it
quietly syncs the wrong pair, which is exactly what the development profile
is for. See [Development builds](#development-builds).

**A dry run is not free.** Both reads are billed per account returned, so
one against a few thousand follows costs dollars, not cents. Check the
prices in the developer console before the first `--apply` — #162 is open
because this app's own usage numbers count requests, not resources.

The plan is written to `$XDG_STATE_HOME/twigpui/sync_plan.json` and each
entry is marked as it lands, so an interrupted `--apply` resumes from the
file without paying to read either side again. `--apply` with no plan on
file is an error: the dry run is what produces the plan.

Removals need `--prune` **here**. On the CLI a list may hold accounts you
added by hand and deleting them stays your call; the background sync above
prunes unconditionally, because a mirror that only grows is not a mirror.

Both read the same plan file, so a dry run's plan is what the background
sync drains next — including its removals. If you want to look at a diff
without any of it being applied, turn the sync off first.

They share the backoff too. `--apply` while the background sync is backing
off from a refusal (#197) says so on stderr and **sends anyway** — one
deliberate batch is the cheapest way to find out whether the cap has
lifted — and what comes back is recorded for the background sync as well:
a write that lands ends its streak, a refusal lengthens it.

Both sides need scopes this app did not request before #163
(`follows.read`, `list.write`), so an existing session is refused before it
spends anything — launch twigpui and click "Re-authorize" once.

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
in the code (`profile::Profile::loopback_port`) and must match the Portal
registration verbatim. A development build uses `8734` and its own X app;
see [Development builds](#development-builds).

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
`127.0.0.1:8733` (`8734` for a development build) catches the redirect back
(waiting up to two minutes). The two ports never collide, so a sign-in in
one build and a sign-in in the other can be in flight at once. Once
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

A development build appends `twigpui-dev` instead of `twigpui` to all three,
so it shares no file with the installed app — see
[Development builds](#development-builds).

### Development builds

A debug build (`cargo run`, or `./scripts/build-app-bundle.sh --dev`) is a
separate installation from the release build the `.app` bundle ships. It
signs into a separate X app, keeps its own session and cache, and says so in
its window title (#169):

| | release build | debug build |
| --- | --- | --- |
| Directories | `~/.config/twigpui/` etc. | `~/.config/twigpui-dev/` etc. |
| OAuth redirect URI | `http://127.0.0.1:8733/callback` | `http://127.0.0.1:8734/callback` |
| Window title | `twigpui` | `twigpui (dev)` |
| Bundle | `dist/twigpui.app` | `dist/twigpui-dev.app` (see the [`app-bundle` skill](.claude/skills/app-bundle/SKILL.md)) |
| Bundle id | `com.github.usadamasa.twigpui` | `com.github.usadamasa.twigpui.dev` |
| Icon | `assets/AppIcon.png` | the same artwork, desaturated |
| Default `list_id` | none — the home timeline | a throwaway list, in `profile.rs` |
| `--sync-list` source | everyone you follow | four fixed X accounts |

The last two rows are what keep the parts of this app that cost money from
being expensive to work on. A development `--sync-list` that read the real
follow graph would bill a dry run for every account on it (#163), and one
that defaulted to the real List could rewrite it over a forgotten export —
so the development build carries its own list id and its own four-account
source, both in `src/profile.rs`. `X_LIST_ID` and `list_id` still override
the default; there is no override for the sync source, because a
development build spending the real read cost is the thing being prevented.

Which one you get is decided at compile time by `debug_assertions`, with no
flag and no environment variable. That is deliberate: the failure this
guards against is *forgetting*, and a debug binary cannot be talked into
addressing the release installation's tokens or cache. The cost is the one
case where the two disagree — **`cargo run --release` from this checkout
uses the release profile**, and so the installed app's files. Use
`./scripts/build-app-bundle.sh --dev` when you want an optimized-looking
development app; it builds debug on purpose, for exactly this reason.

**Setting one up.** Register a second public client in the X Developer
Portal with `http://127.0.0.1:8734/callback` as its redirect URI, then put
its client id where only the development build will read it:

```sh
mkdir -p ~/.config/twigpui-dev
cat >> ~/.config/twigpui-dev/config.toml <<'EOF'
oauth_client_id = "…the development app's client id…"
EOF
```

`config.toml` rather than `.env`: a `.env` in this checkout is read by
whichever profile runs from here, so a client id left there would follow a
release-profile run into the installed app's state.

### `config.toml`

`$XDG_CONFIG_HOME/twigpui/config.toml` is an optional, hand-edited settings
file with the same keys as the environment variables above:

```toml
target_username = "XDevelopers"
max_results = 20
list_id = "2091351590695588200"
min_fetch_interval_seconds = 60
oauth_client_id = "…"
theme = "light"
request_price = 0.02
daily_request_budget = 500
auto_sync_list = true
sync_interval_seconds = 21600
sync_prune_limit_percent = 10
sync_writes_per_minute = 2
auto_refresh = true
auto_refresh_interval_seconds = 300
```

A missing file is fine — it just means there are no file-level settings.
Precedence is **environment variable > `config.toml` > built-in default**,
so an env var always wins over the file. `oauth_client_id` is safe to check
in — it is a public client id, not a secret.

`list_id` is the number in a list's own URL on x.com
(`https://x.com/i/lists/<list_id>`). Setting it replaces the home timeline
rather than adding a second view: `GET /2/users/:id/timelines/reverse_chronological`
stopped returning followed authors' posts for this account (#157) and nothing
here can fix that, so a List is how a following-shaped feed is read at all.
A non-numeric value fails startup instead of being ignored — silently falling
back to the empty home timeline would look like the list was empty.

A `bearer_token` key is **rejected** rather than ignored (#33): the
credential it named no longer exists, and a file that still carries it would
otherwise look configured while nothing read it.

### Theme

`theme` accepts `light`, `dark`, or `system` (case-insensitive, surrounding
whitespace trimmed), via `X_THEME` or `theme` in `config.toml`, with the same
env > file > default precedence as everything else above. It defaults to
`light`. `system` follows the OS appearance, read once at startup via gpui's
`Window::appearance()`. An unrecognized value is not a startup error — a
typo'd theme is cosmetic, not worth blocking the app over — it falls back to
`light` and prints a warning to stderr naming the value it ignored.


## Keyboard shortcuts

| Key | Action |
| --- | --- |
| `⌘R` | Reload |
| `⌘⇧R` | Show the posts auto-refresh already fetched (spends nothing) |
| `⌘N` | Focus the composer |
| `esc` | Leave the composer (the draft is kept) |
| `⌘↑` | Back to the newest post |
| `⌘Q` | Quit |
| `⌘W` | Close the window |
| `⌘M` | Minimize |

Every one of them is in the menu bar (#99), which is where a macOS user
looks for a keystroke. #58 also printed the first four in a permanent strip
under the header, on the reasoning that a help screen nobody opens
documents nothing; #95 removed that strip, because a line of hints under
the toolbar is not something a native app does, and the menu bar had made
it redundant.

**No binding is a bare printable key, deliberately.** The hazard #58 is
really about is a bare `j`/`k`/`n` firing while you are typing a post;
nothing bound today can, because every binding either carries `⌘` or is a
named key that types nothing (`esc`). Bare keys become worth having once posts can be selected,
and that needs a second key context the composer's focus removes — see
`menu::KEY_CONTEXT`.

**`⌘Q` is the one binding with no key context.** The others answer a
question about the timeline and belong to the view that answers it.
Quitting is not the window's business, and scoping it would mean `⌘Q` doing
nothing whenever focus sat anywhere else — so it is registered globally and
handled on the `App` rather than on the window's root (#99).

**No key posts.** The composer's button is the only way, which is how it
was being used anyway; `⌘↩` was bound for it from #58 until #142 removed
both that and its menu item. Plain `↩` was never bound and still is not,
for the reason that outlives the removal — it has to keep inserting a
newline, and a post is not undoable.

**`esc` moves focus only.** The draft is left exactly as typed: losing it to
a stray key is unrecoverable, and never losing a draft is the composer's
main promise (#14).

**A reload does not move you.** Posts arriving at the top of a list you
are part-way down would otherwise slide everything under your eyes; the
timeline scrolls by however many posts arrived, so the row you were reading
stays where it is. At the very top, nothing moves and the new posts simply
appear (#22).

**A reload says what it did.** A muted line under the header reports how
many posts arrived — including none, which is the case where the screen is
otherwise identical before and after and the press looks like it did
nothing. It counts the same posts the scroll compensates for, so the number
and the movement always agree (#141).

**"Load older" has no shortcut.** Each press pages backwards for one paid
request, and a key that spends money on a mis-hit is not a convenience.
`⌘R` does spend requests, but it is the reload gesture every app shares and
not one anyone hits by accident — and it goes through the same throttle
(#10) and cooldown reporting (#57) as the button, so a held-down `⌘R`
cannot outrun the interval that exists to stop this app spending in a loop.


## Menu bar

| Menu | Items |
| --- | --- |
| twigpui | About twigpui, Quit twigpui |
| File | New Post |
| View | Reload, Back to Top |
| Window | Minimize, Close Window |

Every item dispatches the same action its keystroke does, and macOS draws
the key equivalent beside it from the keymap. One `menu::Shortcut` constant
holds the keystroke, both wordings, **and the action** (#119), and the key
bindings, the on-screen strip and the menu items are all built from it — so
none of the four can be changed for one of them and not the others.

The claim used to be broader than what the code guaranteed. Until #119 the
constant held only the key and the labels; which action each one dispatched
was written out again in both `init` and `menus`, and pairing a label with
the wrong action type compiled cleanly. What is left for a test to catch is
narrower: `menus` still decides which menu an item belongs to, so a
shortcut can carry a menu label and be left out of every menu.

The wordings differ between the two lists on purpose: a menu item is read
on its own ("New Post"), while the strip is read as a row of hints under a
heading ("⌘N Focus the composer").

**The Window menu's name is load-bearing.** gpui hands a menu to AppKit's
`setWindowsMenu_` only when it is called exactly `Window`. Rename it and
`⌘W`/`⌘M` keep working — they are ordinary bindings — but the menu stops
being the one macOS treats as the window list (#109).

**`⌘W` ends the app**, since there is one window — but only because closing
the last window quits explicitly (#139). gpui keeps the process alive on
its own, which is right for an app you can ask for another window; this one
cannot, so `⌘W` used to leave a process running with nothing on screen and
nothing but `⌘Q` able to reach it.

Like `⌘Q`, it does not prompt, and an unsent draft goes with it — the same
hazard `⌘Q` has always had.

