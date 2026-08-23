# Timeline: what the window shows and how it is assembled

The app shows your own home timeline —
`GET /2/users/:id/timelines/reverse_chronological` for the id
`GET /2/users/me` resolves — in a scrollable list with a reload button. A
"Load older" button pages further back via `meta.next_token`.

**Signing in with X is now the only way to authenticate** (#33). The
app-only bearer token was removed: it could not read the home timeline and
could not post, repost, quote, like or delete, which is most of what this
app does. See [Migrating from the bearer token](../README.md#migrating-from-the-bearer-token) if you were using one.

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
Once walked, a thread is cached (`thread-<reply_id>.json`, see [Local cache](../.claude/skills/x-api-budget/reference/app-behavior.md#local-cache)), so
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

