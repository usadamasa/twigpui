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

Signing in with X is the only way to authenticate. The app-only bearer token
was removed: it could not read the home timeline and could not post, repost,
quote, like or delete, which is most of what this app does.

## Documentation

| File | Contents |
| --- | --- |
| [docs/timeline.md](docs/timeline.md) | What the window shows and how a row is assembled |
| [docs/writing.md](docs/writing.md) | Posting, replying, quoting, reposting, liking, deleting, opening in a browser |
| [docs/media.md](docs/media.md) | Attached images and author avatars |
| [docs/api-budget.md](docs/api-budget.md) | Request cost per action, rate limits, usage tracking, local cache |
| [docs/operations.md](docs/operations.md) | Building the `.app` bundle, logs, code metrics, tests |

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
| `X_MIN_FETCH_INTERVAL_SECONDS` | no | `60` | Floor on how often a fetch may run, in seconds (#10) |
| `X_THEME` | no | `light` | Color theme: `light`, `dark`, or `system` (follows the OS appearance) — also `theme` in `config.toml` (#19) |
| `X_REQUEST_PRICE` | no | unset | Price per API request, in whatever unit you have in mind — also `request_price` in `config.toml` (#18, see [Usage tracking](docs/api-budget.md#usage-tracking)) |
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
| `⌘↩` | Post the draft |
| `⌘N` | Focus the composer |
| `esc` | Leave the composer (the draft is kept) |
| `⌘Q` | Quit |
| `⌘W` | Close the window |
| `⌘M` | Minimize |

The first four are shown on screen, under the header — four bindings fit on
one line, and a help screen nobody opens documents nothing. `⌘Q`, `⌘W` and
`⌘M` are left off that strip: it is for what this app does that another one
would not, and those three are macOS gestures every app shares.

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


## Menu bar

| Menu | Items |
| --- | --- |
| twigpui | About twigpui, Quit twigpui |
| File | New Post, Submit Post |
| View | Reload |
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

**`⌘W` ends the app**, since there is one window, exactly as `⌘Q` does. It
does not prompt, and an unsent draft goes with it — the same hazard `⌘Q` has
always had.

