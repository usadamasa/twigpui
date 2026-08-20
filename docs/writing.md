# Writing: posting, replying, quoting, reposting, liking, deleting

Everything this app can send back to X. For what it costs per action,
see [API budget](api-budget.md).

## Posting

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

## Quoting a post

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

## Deleting your own posts

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

## Replying

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

## Reposting and un-reposting

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

## Opening things in a browser

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

## Liking and unliking

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

