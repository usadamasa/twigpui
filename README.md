# twigpui

A development-only X (Twitter) timeline viewer, built with Rust and
[gpui](https://crates.io/crates/gpui). macOS only — no other platform is
considered.

## Status

Milestone 1: fetch and display a timeline.

The app resolves a screen name to a user id, fetches that user's recent posts
with `GET /2/users/:id/tweets`, and renders them in a scrollable list with a
reload button.

`--fetch-only` runs the same fetch headlessly and prints the posts, which is
useful for checking credentials without opening a window:

```sh
cargo run -- --fetch-only
```

## Requirements

The `macos-blade` feature is enabled so the build does not need `xcrun metal`,
which ships with full Xcode rather than the Command Line Tools. Rendering goes
through blade instead.

## Why not the home timeline (yet)

`GET /2/users/:id/timelines/reverse_chronological` only accepts OAuth 2.0
Authorization Code (user context) — an app-only Bearer token is rejected.
Signing in with OAuth (below) is the prerequisite for that endpoint; reading
the home timeline itself is a later milestone.

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
| `XDG_CACHE_HOME` | `~/.cache/twigpui/` | Response cache (#9) |
| `XDG_STATE_HOME` | `~/.local/state/twigpui/` | `oauth_tokens.json`, mode `0600` |

An `XDG_*` variable is only honored if it is set to a non-blank absolute
path; a relative or blank value falls back to the default, per spec.

### `config.toml`

`$XDG_CONFIG_HOME/twigpui/config.toml` is an optional, hand-edited settings
file with the same keys as the environment variables above, minus the bearer
token:

```toml
target_username = "XDevelopers"
max_results = 20
oauth_client_id = "…"
```

A missing file is fine — it just means there are no file-level settings.
Precedence is **environment variable > `config.toml` > built-in default**,
so an env var always wins over the file. There is no `bearer_token` key:
`config.toml` is a plain file people check into dotfiles repos, so that
credential stays environment-only by design (`X_BEARER_TOKEN` or `.env`) —
a `bearer_token` entry in the file is rejected with an error rather than
silently read. `oauth_client_id` is the exception: it's a public client id,
not a secret, so it's fine to check in.

## API cost

The X API bills per request against prepaid credits. Each reload spends two
requests: one user lookup and one timeline fetch. Fetching happens only on
startup and on an explicit reload — there is no polling or auto-refresh.

When credits run out the API answers `429` with a `UsageCapExceeded` problem
body; the app surfaces that text directly in the window.

## Tests

```sh
cargo test
```

Tests cover response parsing and error mapping against fixture JSON, plus the
OAuth PKCE math, callback parsing, and token-file handling, so they make no
network calls, open no browser, and spend no credits. The actual code
exchange, X's live response shapes, and refresh-token rotation aren't
covered by tests — those need a real Developer Portal registration and a
one-time manual sign-in.
