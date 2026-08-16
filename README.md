# twigpui

A development-only X (Twitter) timeline viewer, built with Rust and
[gpui](https://crates.io/crates/gpui). macOS only — no other platform is
considered.

## Status

Milestone 1: fetch and display a timeline.

The app resolves a screen name to a user id, fetches that user's recent posts
with `GET /2/users/:id/tweets`, and renders them in a scrollable list with a
reload button.

## Why not the home timeline (yet)

`GET /2/users/:id/timelines/reverse_chronological` only accepts OAuth 2.0
Authorization Code (user context) — an app-only Bearer token is rejected.
Reading the home timeline therefore requires a PKCE browser flow with a local
redirect listener, which is deliberately left for a later milestone.

## Setup

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
