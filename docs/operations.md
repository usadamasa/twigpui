# Operations: building, logging, metrics, tests

## Building the `.app` bundle

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

### The development bundle

```sh
./scripts/build-app-bundle.sh --dev
```

Assembles `dist/twigpui-dev.app` instead: the development profile (#169),
with its own XDG directories, its own OAuth callback port, its own bundle
id, and a desaturated icon. See [Development
builds](../README.md#development-builds) for what the split covers and how
to give it a client id.

Two things about it are easy to get wrong:

- **It is a debug build, on purpose.** `Profile::current` reads
  `debug_assertions`, so debug *is* what selects the development
  directories and port. Building this bundle with `--release` would produce
  an app that carries the development name and icon while writing to the
  installed app's files — the exact confusion the split exists to prevent.
- **It never reuses `assets/AppIcon.icns`.** The gray icon is derived from
  `assets/AppIcon.png` with `sips`, so a prebuilt `.icns` — which is the
  release artwork — is skipped rather than copied onto the development
  bundle. With no PNG present, the development bundle builds without a
  custom icon and says so.

Both bundles can be installed side by side; `open` and `cleanshot-capture`
address them by their distinct executable names (`twigpui` and
`twigpui-dev`).

### Configuration for a bundled launch — read this before double-clicking

**A process launched from Finder, Spotlight, or the Dock does not inherit
your shell's environment**, and its working directory is not this
checkout — so `X_OAUTH_CLIENT_ID`, `X_TARGET_USERNAME`, and friends being
exported in your shell profile, or sitting in this repo's `.env`, has no
effect on a bundled launch. Two things follow:

- **`oauth_client_id`** is non-secret (see [`config.toml`](../README.md#configtoml)) and
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

- **The OAuth loopback listener** binds `127.0.0.1:8733` (`8734` for a
  development build, #169) the same way whether twigpui is bundled or not —
  nothing in `oauth::callback` depends on the current working directory or
  the shell environment, only on the port being free. macOS may still prompt ("twigpui would like to accept
  incoming network connections") the first time a *newly signed* binary
  binds a listening socket, and since ad-hoc-signed builds get a fresh
  signing identity on every rebuild (the same reason Keychain isn't used for
  token storage — see "Where tokens are stored" in the [README](../README.md#signing-in-with-x-oauth-20-authorization-code--pkce)), that prompt can
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

## Logs

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

## Code metrics

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

## Looking at the window without an account

```sh
cargo run -- --fixture fixtures/timeline.json
```

Fills the window from a file instead of from X. **No credential is used and
no API request is made** — `--fixture` never builds an `XClient`, and every
paid path in the view goes through one, so a reload, a like, or a thread
walk has nothing to reach. The buttons are drawn and inert.

The point is that the screen is the same every run. Until this existed the
window could only be filled from the response cache or from a paid request,
both of which differ run to run, so "is this laid out correctly?" had no
fixed thing to ask it of and every UI check ended up on #115 as a sentence
for a human to act out.

`fixtures/timeline.json` is built to put the awkward cases on one screen:

| Row | What it is there to show |
| --- | --- |
| Long unbroken paragraph | Body wraps; the avatar stays a 44px square (#140, #103) |
| A URL with no spaces in it | Whether a single long token still overflows |
| Four attachments | The media grid at its widest, with badges (#65) |
| A quote carrying an image | The quote card's own media (#123) |
| A repost carrying an image | The original's media on the outer row (#104) |
| A reply | The "Show thread" toggle and its cost warning (#12) |
| One of your own posts | The only row offering Delete, the one withholding Repost (#15, #72) |
| An author with no name | No bare `@` (#13) |

A test loads this file and asserts those rows are still in it, so an edit
cannot quietly drop the one somebody was relying on.

**Media and avatars still download**, from `pbs.twimg.com` rather than the
API — no quota, no credits. A fixture whose URLs are unreachable draws the
same fixed-size frames it would while they were in flight, which is what a
layout check needs anyway.

Write your own by copying the bundled one. The `items` are
`x_api::TimelineItem` verbatim, so every field added since #9 is optional
and a fixture can spell out only the case it is about.

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
