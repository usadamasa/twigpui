# Operations: logging, metrics, tests

## Building the `.app` bundle

Moved to the `app-bundle` skill
(`.claude/skills/app-bundle/SKILL.md`), which is where this repository keeps
its operational how-to — alongside `code-metrics-ratchet` and
`fixture-visual-check`.

```sh
./scripts/build-app-bundle.sh          # dist/twigpui.app     (release)
./scripts/build-app-bundle.sh --dev    # dist/twigpui-dev.app (development, #169)
```

The skill covers the icon pipeline, what a Finder launch does and does not
inherit, and why the development bundle is a debug build on purpose. For
what the two profiles differ in beyond the bundle itself, see [Development
builds](../README.md#development-builds).

## Logs

`$XDG_STATE_HOME/twigpui/logs/twigpui.log`, with one rotated predecessor at
`twigpui.log.1`. `state`, not `cache`: a log deleted on the next boot
answers no questions about what happened yesterday.

A debug build writes under `twigpui-dev` instead (#169), so a `cargo run`
you are trying to read the log of is not in the file above.

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
