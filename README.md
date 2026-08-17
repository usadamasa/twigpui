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
Reading the home timeline therefore requires a PKCE browser flow with a local
redirect listener, which is deliberately left for a later milestone.

## Setup

Configuration comes from the environment. Either export it:

```sh
export X_BEARER_TOKEN='…'
cargo run
```

or keep a local `.env`, which `dotenvy` loads into the same variables:

```sh
cp .env.example .env
$EDITOR .env          # fill in X_BEARER_TOKEN
cargo run
```

| Variable | Required | Default | Meaning |
| --- | --- | --- | --- |
| `X_BEARER_TOKEN` | yes | — | App-only Bearer token, used verbatim (keep any `%2F` / `%3D` as-is) |
| `X_TARGET_USERNAME` | no | `XDevelopers` | Screen name to display, without a leading `@` |
| `X_MAX_RESULTS` | no | `20` | Posts per fetch, 5–100 |

`.env` is gitignored. Do not commit credentials.

### File locations (XDG Base Directory)

Everything twigpui persists lives under three directories, resolved per the
[XDG Base Directory
spec](https://specifications.freedesktop.org/basedir-spec/latest/) and
created (mode `0700`) on startup:

| Variable | Default | Holds |
| --- | --- | --- |
| `XDG_CONFIG_HOME` | `~/.config/twigpui/` | `config.toml` |
| `XDG_CACHE_HOME` | `~/.cache/twigpui/` | Response cache (#9) |
| `XDG_STATE_HOME` | `~/.local/state/twigpui/` | OAuth token store (#7) |

An `XDG_*` variable is only honored if it is set to a non-blank absolute
path; a relative or blank value falls back to the default, per spec.

### `config.toml`

`$XDG_CONFIG_HOME/twigpui/config.toml` is an optional, hand-edited settings
file with the same keys as the environment variables above, minus the token:

```toml
target_username = "XDevelopers"
max_results = 20
```

A missing file is fine — it just means there are no file-level settings.
Precedence is **environment variable > `config.toml` > built-in default**,
so an env var always wins over the file. There is no `bearer_token` key:
`config.toml` is a plain file people check into dotfiles repos, so
credentials stay environment-only by design (`X_BEARER_TOKEN` or `.env`) —
a `bearer_token` entry in the file is rejected with an error rather than
silently read.

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

Tests cover response parsing and error mapping against fixture JSON, so they
make no network calls and spend no credits.
