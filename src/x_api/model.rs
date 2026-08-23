use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A user object as returned under `data` or `includes.users`.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct User {
    pub id: String,
    pub name: String,
    pub username: String,
    /// The account's avatar (#64), present only because `user.fields` asks
    /// for `profile_image_url`. `#[serde(default)]` since an account can
    /// have none, and every fixture that predates #64 omits it.
    #[serde(default)]
    pub profile_image_url: Option<String>,
}

/// A post object as returned under `data`.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Post {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub author_id: Option<String>,
    /// The post(s) this one references — a repost, a quote, a reply, or (per
    /// X's API) some combination of those in a single post, e.g. quoting a
    /// tweet from inside a reply thread (#13). `#[serde(default)]` since a
    /// plain post that references nothing simply omits the field, and every
    /// timeline fixture that predates #13 does exactly that. See
    /// [`TimelineResponse::into_items`] for the precedence used when a post
    /// carries more than one entry.
    #[serde(default)]
    pub referenced_tweets: Vec<ReferencedTweetRef>,
    /// Engagement counts (#67), present only because `tweet.fields` asks for
    /// `public_metrics` — see [`crate::x_api::client`]'s URL builders. `None`
    /// for a response that predates that (fixtures included) or for a post X
    /// declines to report counts for.
    #[serde(default)]
    pub public_metrics: Option<PostMetrics>,
    /// The `entities` object (#70), present only because `tweet.fields`
    /// asks for it. Its `urls` are the only part this crate reads: a post's
    /// text carries `t.co` shortlinks, and `expanded_url` is the only way
    /// to reach the real destination without following a redirect.
    #[serde(default)]
    pub entities: Option<Entities>,
    /// The media attached to this post (#65) — keys only; the media
    /// objects themselves arrive in `includes.media`, joined by
    /// [`post_media`] the same way an author is joined from
    /// `includes.users`.
    #[serde(default)]
    pub attachments: Option<Attachments>,
}

/// A post's `attachments` object (#65). Only `media_keys` is read; polls
/// also live here and are not supported.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Attachments {
    #[serde(default)]
    pub media_keys: Vec<String>,
}

/// One entry in `includes.media` (#65), as returned for the `media.fields`
/// the timeline requests ask for.
///
/// Every field but `media_key` is optional on the wire, and deliberately
/// modelled that way: X omits `url` for a video or animated GIF (only
/// `preview_image_url` is given), omits `alt_text` unless the author wrote
/// one, and has been known to omit dimensions. A missing field must degrade
/// the rendering, never fail the parse — the same rule the rest of this
/// module follows.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Media {
    /// X spells this `media_key`; renamed on the wire because a field
    /// called `media_key` on a struct called `Media` says the same word
    /// twice — clippy's `struct_field_names` objects, and it is right.
    #[serde(rename = "media_key")]
    pub key: String,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub preview_image_url: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub alt_text: Option<String>,
}

/// The subset of X's `entities` object twigpui reads (#70) — just the URLs.
/// Mentions, hashtags and annotations are also in there; serde drops what
/// is not listed, so adding them later is additive.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Entities {
    #[serde(default)]
    pub urls: Vec<UrlEntity>,
}

/// One entry in `entities.urls` (#70). `expanded_url` is the destination
/// the `t.co` shortlink in the post's text stands for; `display_url` is
/// X's own shortened-for-humans rendering of it (`example.com/a/b…`).
///
/// Both are optional on the wire: X omits `expanded_url` for some entities
/// (a media attachment's own `t.co`, notably), and a link with nowhere to
/// go is dropped rather than rendered as a dead chip — see
/// [`post_links`].
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct UrlEntity {
    #[serde(default)]
    pub expanded_url: Option<String>,
    #[serde(default)]
    pub display_url: Option<String>,
}

/// A post's reply/repost/like counts (#67).
///
/// Deserialized from X's `public_metrics` object and serialized into the
/// timeline cache file, the same way [`TimelineItem`] itself is. Every
/// field is renamed on the wire: X spells them `reply_count`,
/// `retweet_count` and `like_count`, but the crate says "repost" rather
/// than "retweet" (#15), and the `_count` suffix reads as noise on a type
/// that holds nothing else. The counts X also sends but this crate ignores
/// (`quote_count`, `bookmark_count`, `impression_count`) are simply not
/// listed — serde drops unknown fields.
///
/// These are a **snapshot taken when the post was fetched**, and nothing
/// refreshes them: an incremental reload sends `since_id`, so a post already
/// on file is never returned again (see `cache::splice`). A row's
/// counts therefore show what was true when it first arrived. Rendering them
/// with a per-row "as of" timestamp was considered and dropped — the row is
/// already dense, and the drift matters less than the counts being visible
/// at all, which is what #67 is about.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PostMetrics {
    #[serde(default, rename = "reply_count")]
    pub replies: u64,
    #[serde(default, rename = "retweet_count")]
    pub reposts: u64,
    #[serde(default, rename = "like_count")]
    pub likes: u64,
}

/// One entry in a post's `referenced_tweets` (#13) — the API's own
/// "this post is a reply/quote/retweet of that other post" annotation. A
/// single post can carry more than one, which is why `Post::referenced_tweets`
/// is a `Vec` rather than an `Option`.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ReferencedTweetRef {
    #[serde(rename = "type")]
    pub kind: ReferenceKind,
    pub id: String,
}

/// The recognized values of `referenced_tweets[].type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReferenceKind {
    Retweeted,
    Quoted,
    RepliedTo,
    /// Forward compatibility: an unrecognized reference type from a future
    /// API revision must not fail parsing the whole post, the same way an
    /// unrecognized cache-file shape is a clean miss rather than an error
    /// (see `cache::load_json`).
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Includes {
    #[serde(default)]
    pub users: Vec<User>,
    /// Referenced posts (#13) — a repost's or quote's real content lives
    /// here, keyed by id, rather than in `data` itself.
    #[serde(default)]
    pub tweets: Vec<Post>,
    /// Attached media (#65), keyed by `media_key` — the same
    /// side-table-plus-keys shape `users` and `tweets` already use.
    #[serde(default)]
    pub media: Vec<Media>,
}

/// The `errors` array X returns alongside partial results, and also the
/// problem-details body it returns for outright failures.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ApiProblem {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub errors: Vec<ApiProblem>,
}

impl ApiProblem {
    /// Best available human-readable description, flattening nested `errors`.
    pub(crate) fn message(&self) -> Option<String> {
        if let Some(detail) = &self.detail {
            return Some(match &self.title {
                Some(title) => format!("{title}: {detail}"),
                None => detail.clone(),
            });
        }
        if let Some(title) = &self.title {
            return Some(title.clone());
        }
        if let Some(reason) = &self.reason {
            return Some(reason.clone());
        }
        self.errors.iter().find_map(ApiProblem::message)
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct UserLookupResponse {
    #[serde(default)]
    pub data: Option<User>,
}

/// The `POST /2/tweets` request body (#14, extended by #16) — the post text
/// plus an optional quote target; no reply/poll support yet (see #12).
///
/// `quote_tweet_id` deliberately reuses this same endpoint/struct rather
/// than a separate quote request type or `Endpoint` variant: X has no
/// dedicated quote endpoint, and splitting the rate-limit tracking for what
/// X treats as one endpoint would only create a second, incorrect window.
/// `skip_serializing_if` keeps it entirely absent (not `null`) for an
/// ordinary post — X may reject a stray null outright.
#[derive(Debug, Serialize)]
pub(crate) struct PostTweetRequest<'a> {
    pub text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quote_tweet_id: Option<&'a str>,
    /// #71: what makes this post a reply. Nested rather than flat because
    /// that is the shape X specifies, and `skip_serializing_if` keeps it
    /// entirely absent for an ordinary post — the same treatment
    /// `quote_tweet_id` gets, for the same reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply: Option<ReplyRequest<'a>>,
}

/// The `reply` object inside [`PostTweetRequest`] (#71).
#[derive(Debug, Serialize)]
pub(crate) struct ReplyRequest<'a> {
    pub in_reply_to_tweet_id: &'a str,
}

/// What `POST /2/tweets` is being asked to publish (#71): the text plus
/// whichever optional target the composer had.
///
/// A struct rather than three positional `Option` parameters threaded
/// through `XClient::create_post` → `post` → `send_post_once`: at three
/// arguments the call sites stop being readable, and two of them are
/// `Option<&str>` that would silently swap without a type error — a
/// mix-up that turns a quote into a reply under someone else's
/// conversation.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Draft<'a> {
    pub text: &'a str,
    pub quote_tweet_id: Option<&'a str>,
    pub reply_to_post_id: Option<&'a str>,
}

impl<'a> Draft<'a> {
    /// The request body for this draft.
    pub(crate) fn to_request(self) -> PostTweetRequest<'a> {
        PostTweetRequest {
            text: self.text,
            quote_tweet_id: self.quote_tweet_id,
            reply: self.reply_to_post_id.map(|id| ReplyRequest {
                in_reply_to_tweet_id: id,
            }),
        }
    }
}

/// The request body shared by `POST /2/users/:id/retweets` (#15) and
/// `POST /2/users/:id/likes` (#68) — the id of the post being acted on;
/// `user_id` (whose repost or like is being created) travels in the URL,
/// not here. One type rather than two identical ones: X specifies the same
/// single-field body for both, so a second copy could only ever drift.
#[derive(Debug, Serialize)]
pub(crate) struct TweetIdRequest<'a> {
    pub tweet_id: &'a str,
}

/// The request body `POST /2/lists/:id/members` (#163) takes — the id of
/// the account being added; the list's own id travels in the URL. Shaped
/// like [`TweetIdRequest`] and separate for the same reason that one is
/// shared: a different field name is a different body.
///
/// **Spec-derived, unverified.** #163 was built without spending a request
/// on `/2/lists/:id/members`, so this field name comes from docs.x.com, not
/// from a 200. `x-api-endpoints` is explicit that the two disagree often
/// enough to plan for.
#[derive(Debug, Serialize)]
pub(crate) struct UserIdRequest<'a> {
    pub user_id: &'a str,
}

/// One page of users, as `GET /2/users/:id/following` and
/// `GET /2/lists/:id/members` return one (#163): the accounts themselves
/// plus the cursor for the next page.
///
/// `data` is `#[serde(default)]` because both endpoints omit it entirely on
/// an empty page rather than sending `[]` — an account that follows nobody,
/// or a list with no members, which is exactly the state a first sync
/// starts from.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct UserPageResponse {
    #[serde(default)]
    pub data: Vec<User>,
    #[serde(default)]
    pub meta: Meta,
}

impl UserPageResponse {
    /// The cursor for the page after this one, or `None` at the end.
    pub(crate) fn next_token(&self) -> Option<&str> {
        self.meta.next_token.as_deref()
    }
}

/// One List as `GET /2/users/:id/owned_lists` returns it (#164): what the
/// picker needs to name a segment and nothing more. `Serialize` too,
/// because the cache and a fixture both write these back out.
///
/// `name` defaults rather than being required: the spec lists it among the
/// default fields, but `x-api-endpoints` is a record of the spec and the
/// API disagreeing, and a picker that fails to parse over one nameless
/// list would take every other list down with it. The renderer falls back
/// to the id (`ui::list_picker::segment_label`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct ListSummary {
    pub id: String,
    #[serde(default)]
    pub name: String,
}

/// The whole response of `GET /2/users/:id/owned_lists` (#164). `data`
/// is absent, not empty, for an account that owns no lists — the same
/// shape [`TimelineResponse`] already tolerates.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct ListPageResponse {
    #[serde(default)]
    pub data: Vec<ListSummary>,
    #[serde(default)]
    pub meta: Meta,
}

impl ListPageResponse {
    /// The cursor for the page after this one, or `None` at the end.
    pub(crate) fn next_token(&self) -> Option<&str> {
        self.meta.next_token.as_deref()
    }
}

/// Pagination info returned alongside `data`. Only `next_token` matters to
/// this crate — it's the cursor `x_api::client::home_timeline_url` sends back
/// as `pagination_token` to fetch the next (older) page, driving #11's "Load
/// older" button.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Meta {
    #[serde(default)]
    pub next_token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct TimelineResponse {
    #[serde(default)]
    pub data: Vec<Post>,
    #[serde(default)]
    pub includes: Includes,
    #[serde(default)]
    pub meta: Meta,
}

/// A post flattened with its author, ready for rendering.
///
/// `Serialize`/`Deserialize` since #9: this is the exact shape persisted to
/// the timeline cache file, so no separate cache-only type is needed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TimelineItem {
    pub id: String,
    pub text: String,
    pub created_at: Option<String>,
    pub author_name: String,
    pub author_username: String,
    /// Set only for a repost (#13): the screen name of whoever's timeline
    /// surfaced it, meant to be shown as a small line above the body —
    /// which, unlike the four fields above, describes the *original* post
    /// once this is `Some`. `#[serde(default)]` so cache files written
    /// before #13 deserialize cleanly with this simply absent (`None`).
    #[serde(default)]
    pub reposted_by: Option<String>,
    /// Set for a quote — including a repost of a quote, per the precedence
    /// documented on [`TimelineResponse::into_items`] (#13): the quoted
    /// post, meant to be rendered as a card under the body.
    /// `#[serde(default)]` for the same cache-compatibility reason as
    /// `reposted_by`.
    #[serde(default)]
    pub quoted: Option<QuotedPost>,
    /// Set for a reply (#12): who this post replies to, and that parent's
    /// own post id — the anchor `ui.rs`'s "Show thread" walk starts from.
    /// Populated at zero extra request cost, since the parent (and, when
    /// expanded, its author) is already in `includes` per #13's
    /// `referenced_tweets.id`/`.author_id` expansions. `#[serde(default)]`
    /// for the same cache-compatibility reason as `reposted_by`/`quoted`.
    #[serde(default)]
    pub replied_to: Option<RepliedTo>,
    /// The counts shown under the body (#67), for whichever post the body
    /// actually holds — the original, not the outer post, once this row is
    /// a repost. `None` when the response carried none, and — like
    /// `reposted_by`/`quoted`/`replied_to` — `#[serde(default)]` so cache
    /// files written before #67 deserialize cleanly.
    #[serde(default)]
    pub metrics: Option<PostMetrics>,
    /// The links in this post's text (#70), expanded out of the `t.co`
    /// shortlinks the text itself carries — for whichever post the body
    /// actually holds, so a repost gets the original's. Empty for a post
    /// with no links, and `#[serde(default)]` so cache files written before
    /// #70 deserialize cleanly, exactly like `metrics` above.
    #[serde(default)]
    pub links: Vec<PostLink>,
    /// The avatar URL for whoever's post the body holds (#64) — the
    /// original's for a repost, matching `author_name`/`author_username`.
    /// `None` when the author never expanded or has no avatar.
    /// `#[serde(default)]` for the same cache-compatibility reason as
    /// `links` above.
    #[serde(default)]
    pub author_avatar_url: Option<String>,
    /// For a repost row (#52): the id of the *original* post, the one whose
    /// text and author this row already displays.
    ///
    /// `id` above stays the retweet activity's own id — it is what keys the
    /// row, the cache, and `replied_to`'s thread walk — but every write
    /// endpoint (`POST /2/users/:id/retweets`, `POST /2/tweets`'s
    /// `quote_tweet_id`, `POST /2/users/:id/likes`) acts on the original.
    /// Without this field #15, #16 and #68 all had to withhold their
    /// buttons on a repost row rather than risk sending the wrong id; see
    /// [`action_post_id`]. Populated from the `retweeted` entry in
    /// `referenced_tweets`, which #13's join already has in hand — no extra
    /// request. `None` for every post that is not a repost.
    ///
    /// `#[serde(default)]` for the usual reason: `cache::load_json` treats
    /// a parse failure as a silent miss, so a missing attribute here would
    /// quietly discard every user's cache and re-fetch it at their expense.
    #[serde(default)]
    pub original_post_id: Option<String>,
    /// The media attached to whichever post the body holds (#65) — the
    /// original's for a repost, matching its text. Empty for a post with
    /// no attachments, and `#[serde(default)]` so cache files written
    /// before #65 deserialize cleanly.
    #[serde(default)]
    pub media: Vec<PostMedia>,
}

/// One attached image, video thumbnail or GIF thumbnail, flattened for
/// rendering (#65).
///
/// `url` is whatever is actually displayable: the image itself for a photo,
/// the `preview_image_url` still for a video or animated GIF — neither of
/// which this app plays (that is deliberately out of scope; the row shows
/// the thumbnail and a badge saying what it is). An entry with nothing
/// displayable at all is dropped by [`post_media`] rather than rendered as
/// a hole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PostMedia {
    pub url: String,
    /// X's own `type` string, kept verbatim (`photo`, `video`,
    /// `animated_gif`, or something newer). Not parsed into an enum: the
    /// only decision it drives is which badge to show, and an unrecognized
    /// value should show no badge rather than fail to parse a whole
    /// timeline.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub alt_text: Option<String>,
}

/// The id of the post a write endpoint should act on for `item` (#52): the
/// original for a repost row, the row's own id otherwise.
///
/// One function rather than the same `unwrap_or` at four call sites, so
/// "which id does a repost act on" has exactly one answer in the codebase.
/// Nested references need no special case: `original_post_id` is set from
/// the `retweeted` reference, and a repost *of a quote* is still a repost
/// of that quote post — acting on it is acting on the thing the row shows.
pub(crate) fn action_post_id(item: &TimelineItem) -> &str {
    item.original_post_id.as_deref().unwrap_or(&item.id)
}

/// One openable link from a post's text (#70), flattened out of
/// [`UrlEntity`] with both halves resolved: `url` is where it actually
/// goes, `label` is what to show for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PostLink {
    pub url: String,
    pub label: String,
}

/// Who a reply is replying to (#12), joined from `includes` the same way a
/// repost's or quote's original is. `author_name`/`author_username` are
/// empty (never the whole field `None`) when the parent's author wasn't
/// resolvable — deleted, protected, or simply not expanded — mirroring
/// [`build_item`]'s existing convention for a missing repost original,
/// rather than hiding the reply context entirely.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RepliedTo {
    pub post_id: String,
    pub author_name: String,
    pub author_username: String,
}

/// A quoted post, flattened with its author, embedded in a [`TimelineItem`]
/// as the source of a quote (#13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct QuotedPost {
    pub author_name: String,
    pub author_username: String,
    pub text: String,
    /// The quoted post's own attached media (#123), joined the same way a
    /// repost's is — the quote card had text only until then, so an image
    /// that *was* the point of the quote did not appear at all.
    /// `#[serde(default)]` for the same cache-compatibility reason as
    /// every other field added after #9.
    #[serde(default)]
    pub media: Vec<PostMedia>,
}

impl TimelineResponse {
    /// `meta.next_token`, if the response carried one — the cursor for
    /// fetching the next (older) page (#11). Reads `&self` rather than
    /// consuming it so callers can check this before [`Self::into_items`]
    /// takes ownership.
    pub(crate) fn next_token(&self) -> Option<&str> {
        self.meta.next_token.as_deref()
    }

    /// Join each post with its author from `includes.users`, and — #13 —
    /// with whatever it references from `includes.tweets`.
    ///
    /// Precedence when a post's `referenced_tweets` carries more than one
    /// entry (X allows this — e.g. quoting a tweet from inside a reply
    /// thread produces both a `quoted` and a `replied_to` entry on the same
    /// post): `retweeted` wins over `quoted`, which wins over `replied_to`.
    /// A repost fully replaces the rendered body with the original post, so
    /// it takes priority over a quote card, which only adds to the body
    /// rather than replacing it; a bare reply reference carries no
    /// rendering of its own yet (thread display is #12), so it never wins
    /// against either. See [`build_item`] and [`quote_of`] for where this
    /// is applied.
    ///
    /// Posts whose author is absent from the expansion still render, with
    /// the author fields left empty — dropping them would silently hide
    /// content. The same is true of a repost whose original is missing from
    /// `includes` (deleted, protected, or simply not expanded): rather than
    /// an empty row, it falls back to the outer post's own — possibly
    /// truncated — text.
    pub(crate) fn into_items(self) -> Vec<TimelineItem> {
        let users: HashMap<&str, &User> = self
            .includes
            .users
            .iter()
            .map(|u| (u.id.as_str(), u))
            .collect();
        let referenced: HashMap<&str, &Post> = self
            .includes
            .tweets
            .iter()
            .map(|post| (post.id.as_str(), post))
            .collect();

        // #65: the media side table, keyed the same way as the two above.
        let media: HashMap<&str, &Media> = self
            .includes
            .media
            .iter()
            .map(|item| (item.key.as_str(), item))
            .collect();

        self.data
            .iter()
            .map(|post| build_item(post, &users, &referenced, &media))
            .collect()
    }
}

/// A post's author name/username from `includes.users`, or a pair of empty
/// strings when the author id is absent or wasn't expanded — the shared
/// lookup behind every author field [`build_item`] and [`quote_of`] fill in.
fn author_fields(post: &Post, users: &HashMap<&str, &User>) -> (String, String, Option<String>) {
    let author = post
        .author_id
        .as_deref()
        .and_then(|id| users.get(id).copied());
    (
        author.map(|u| u.name.clone()).unwrap_or_default(),
        author.map(|u| u.username.clone()).unwrap_or_default(),
        author.and_then(|u| u.profile_image_url.clone()),
    )
}

/// Join one post with its author, and — if it references another post —
/// with that reference too, per the precedence documented on
/// [`TimelineResponse::into_items`].
fn build_item(
    post: &Post,
    users: &HashMap<&str, &User>,
    referenced: &HashMap<&str, &Post>,
    media: &HashMap<&str, &Media>,
) -> TimelineItem {
    let (author_name, author_username, author_avatar_url) = author_fields(post, users);
    let mut item = TimelineItem {
        id: post.id.clone(),
        text: post.text.clone(),
        created_at: post.created_at.clone(),
        author_name,
        author_username,
        reposted_by: None,
        quoted: None,
        replied_to: None,
        metrics: post.public_metrics,
        links: post_links(post),
        author_avatar_url,
        original_post_id: None,
        media: post_media(post, media),
    };

    if let Some(retweet_ref) = post
        .referenced_tweets
        .iter()
        .find(|r| r.kind == ReferenceKind::Retweeted)
    {
        // The outer post's own author is whoever reposted — captured before
        // it's overwritten below with the original's author.
        item.reposted_by = Some(item.author_username.clone());
        // #52: the id every write endpoint needs, available here at no
        // extra cost — set whether or not the original itself expanded,
        // since the id is what was referenced, not what came back.
        item.original_post_id = Some(retweet_ref.id.clone());

        if let Some(original) = referenced.get(retweet_ref.id.as_str()).copied() {
            let (author_name, author_username, avatar) = author_fields(original, users);
            item.text.clone_from(&original.text);
            item.author_name = author_name;
            item.author_username = author_username;
            item.author_avatar_url = avatar;
            // A repost of a quote — or of a reply — carries context that
            // belongs to the original post now shown as the body, not to
            // the (already-consumed) retweet reference on the outer post.
            item.quoted = quote_of(original, users, referenced, media);
            item.replied_to = reply_target(original, users, referenced);
            // #67: the body is the original's, so the counts under it have
            // to be the original's too — the outer repost carries its own.
            item.metrics = original.public_metrics;
            // #70: the links belong to the body, which is the original's.
            item.links = post_links(original);
            // #65: and so does the attached media.
            item.media = post_media(original, media);
        } else {
            // Original is gone from `includes` — keep the outer post's own
            // (possibly truncated `RT @user: …`) text already set above
            // rather than blanking the row, but drop the author fields the
            // same way a post whose author never expanded already does: we
            // know who reposted, not who wrote it.
            item.author_name = String::new();
            item.author_username = String::new();
            item.author_avatar_url = None;
            // Blanked for the same reason as the author fields above: the
            // outer repost's own counts are not the original's (#67).
            item.metrics = None;
            item.links.clear();
            item.media.clear();
        }
    } else {
        if post
            .referenced_tweets
            .iter()
            .any(|r| r.kind == ReferenceKind::Quoted)
        {
            item.quoted = quote_of(post, users, referenced, media);
        }
        // #12: a reply (with or without an attached quote) shows who it's
        // replying to, at zero extra request cost — the parent is already
        // in `includes` per #13's expansions.
        item.replied_to = reply_target(post, users, referenced);
    }

    item
}

/// Who `post` is replying to (#12), if it has a `replied_to` reference —
/// joined from `includes` the same way [`quote_of`] joins a quote's source.
/// `None` only when `post` has no `replied_to` reference at all; a reply
/// whose parent is missing from `includes` (deleted, protected, or simply
/// not expanded) still returns `Some`, with empty author fields — the id
/// alone is enough for `ui.rs`'s "Show thread" to start from, and dropping
/// the reply context entirely would hide something real.
fn reply_target(
    post: &Post,
    users: &HashMap<&str, &User>,
    referenced: &HashMap<&str, &Post>,
) -> Option<RepliedTo> {
    let reply_ref = post
        .referenced_tweets
        .iter()
        .find(|r| r.kind == ReferenceKind::RepliedTo)?;
    let (author_name, author_username, _avatar) = referenced
        .get(reply_ref.id.as_str())
        .map(|parent| author_fields(parent, users))
        .unwrap_or_default();
    Some(RepliedTo {
        post_id: reply_ref.id.clone(),
        author_name,
        author_username,
    })
}

/// The openable links in `post`'s text (#70).
///
/// An entity with no `expanded_url` is dropped: without it there is nothing
/// to open but the `t.co` shortlink already sitting in the text, and a chip
/// that just re-states the shortlink is worse than no chip. Duplicates are
/// dropped too — X repeats an entity when the same link appears twice —
/// keeping the first occurrence, so the order matches the text. The label
/// falls back to the URL itself when X sends no `display_url`.
fn post_links(post: &Post) -> Vec<PostLink> {
    let mut seen: Vec<PostLink> = Vec::new();
    let Some(entities) = post.entities.as_ref() else {
        return seen;
    };
    for entity in &entities.urls {
        let Some(url) = entity.expanded_url.as_ref() else {
            continue;
        };
        if seen.iter().any(|link| &link.url == url) {
            continue;
        }
        seen.push(PostLink {
            url: url.clone(),
            label: entity.display_url.clone().unwrap_or_else(|| url.clone()),
        });
    }
    seen
}

/// The attached media for `post` (#65), joined from `includes.media` by
/// the keys the post carries.
///
/// A key with no matching entry is skipped — X can omit media the caller
/// is not allowed to see — and so is an entry with nothing displayable:
/// `url` for a photo, `preview_image_url` for a video or animated GIF
/// (neither of which this app plays). Order follows the post's own
/// `media_keys`, which is the order the author attached them in.
fn post_media(post: &Post, media: &HashMap<&str, &Media>) -> Vec<PostMedia> {
    let Some(attachments) = post.attachments.as_ref() else {
        return Vec::new();
    };
    attachments
        .media_keys
        .iter()
        .filter_map(|key| media.get(key.as_str()).copied())
        .filter_map(|item| {
            let url = item
                .url
                .clone()
                .or_else(|| item.preview_image_url.clone())?;
            Some(PostMedia {
                url,
                kind: item.kind.clone(),
                width: item.width,
                height: item.height,
                alt_text: item.alt_text.clone(),
            })
        })
        .collect()
}

/// The post `post` quotes, if it has a `quoted` reference and that post is
/// present in `includes.tweets`. `None` either way is a legitimate outcome
/// for [`build_item`] to fall back on (no card, not an error) — a quoted
/// post can be deleted, protected, or simply absent from the expansion.
fn quote_of(
    post: &Post,
    users: &HashMap<&str, &User>,
    referenced: &HashMap<&str, &Post>,
    media: &HashMap<&str, &Media>,
) -> Option<QuotedPost> {
    let quote_ref = post
        .referenced_tweets
        .iter()
        .find(|r| r.kind == ReferenceKind::Quoted)?;
    let quoted_post = referenced.get(quote_ref.id.as_str())?;
    let (author_name, author_username, _avatar) = author_fields(quoted_post, users);
    Some(QuotedPost {
        author_name,
        author_username,
        text: quoted_post.text.clone(),
        media: post_media(quoted_post, media),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMELINE_JSON: &str = r#"{
      "data": [
        {
          "id": "1700000000000000001",
          "text": "hello from the timeline",
          "created_at": "2026-08-16T09:00:00.000Z",
          "author_id": "2244994945"
        },
        {
          "id": "1700000000000000002",
          "text": "a post whose author was not expanded",
          "created_at": "2026-08-16T08:00:00.000Z",
          "author_id": "9999999999"
        }
      ],
      "includes": {
        "users": [
          {
            "id": "2244994945",
            "name": "Developers",
            "username": "XDevelopers",
            "profile_image_url": "https://pbs.twimg.com/profile_images/x.jpg"
          }
        ]
      },
      "meta": { "result_count": 2, "next_token": "abc123" }
    }"#;

    #[test]
    fn joins_posts_with_their_authors() {
        let response: TimelineResponse = serde_json::from_str(TIMELINE_JSON).unwrap();
        let items = response.into_items();

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].text, "hello from the timeline");
        assert_eq!(items[0].author_name, "Developers");
        assert_eq!(items[0].author_username, "XDevelopers");
        assert_eq!(
            items[0].created_at.as_deref(),
            Some("2026-08-16T09:00:00.000Z")
        );
    }

    #[test]
    fn keeps_posts_whose_author_is_missing() {
        let response: TimelineResponse = serde_json::from_str(TIMELINE_JSON).unwrap();
        let items = response.into_items();

        assert_eq!(items[1].id, "1700000000000000002");
        assert_eq!(items[1].author_name, "");
        assert_eq!(items[1].author_username, "");
    }

    #[test]
    fn next_token_is_read_from_meta() {
        // #11: this is the cursor "Load older" resends as `pagination_token`.
        let response: TimelineResponse = serde_json::from_str(TIMELINE_JSON).unwrap();
        assert_eq!(response.next_token(), Some("abc123"));
    }

    #[test]
    fn next_token_is_none_when_meta_omits_it() {
        let response: TimelineResponse =
            serde_json::from_str(r#"{"meta":{"result_count":0}}"#).unwrap();
        assert_eq!(response.next_token(), None);
    }

    #[test]
    fn next_token_is_none_when_meta_is_absent_entirely() {
        let response: TimelineResponse =
            serde_json::from_str(r#"{"data":[{"id":"1","text":"orphan"}]}"#).unwrap();
        assert_eq!(response.next_token(), None);
    }

    #[test]
    fn parses_an_empty_timeline() {
        let response: TimelineResponse =
            serde_json::from_str(r#"{"meta":{"result_count":0}}"#).unwrap();
        assert!(response.into_items().is_empty());
    }

    #[test]
    fn parses_a_user_lookup() {
        let response: UserLookupResponse = serde_json::from_str(
            r#"{"data":{"id":"2244994945","name":"Developers","username":"XDevelopers"}}"#,
        )
        .unwrap();
        assert_eq!(response.data.unwrap().id, "2244994945");
    }

    #[test]
    fn reads_a_problem_details_body() {
        let problem: ApiProblem = serde_json::from_str(
            r#"{"title":"Unauthorized","detail":"Unauthorized","status":401}"#,
        )
        .unwrap();
        assert_eq!(
            problem.message().as_deref(),
            Some("Unauthorized: Unauthorized")
        );
    }

    #[test]
    fn falls_back_from_detail_to_title_to_reason() {
        let title_only: ApiProblem = serde_json::from_str(r#"{"title":"Unauthorized"}"#).unwrap();
        assert_eq!(title_only.message().as_deref(), Some("Unauthorized"));

        let reason_only: ApiProblem =
            serde_json::from_str(r#"{"reason":"client-not-enrolled"}"#).unwrap();
        assert_eq!(
            reason_only.message().as_deref(),
            Some("client-not-enrolled")
        );

        // A body with none of the three has nothing to report, and the caller
        // falls back to the raw text instead.
        let empty: ApiProblem = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.message(), None);
    }

    #[test]
    fn skips_nested_errors_that_say_nothing() {
        let problem: ApiProblem =
            serde_json::from_str(r#"{"errors":[{},{"title":"Not Found Error"}]}"#).unwrap();
        assert_eq!(problem.message().as_deref(), Some("Not Found Error"));
    }

    #[test]
    fn keeps_a_post_whose_author_id_is_absent_entirely() {
        let response: TimelineResponse =
            serde_json::from_str(r#"{"data":[{"id":"1","text":"orphan"}]}"#).unwrap();
        let items = response.into_items();

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].author_username, "");
        assert_eq!(items[0].created_at, None);
    }

    // --- #13: reposts and quotes ---

    const REPOST_JSON: &str = r#"{
      "data": [
        {
          "id": "1800000000000000001",
          "text": "RT @XDevelopers: hello from the timeline",
          "created_at": "2026-08-16T10:00:00.000Z",
          "author_id": "3000000000000000001",
          "referenced_tweets": [
            { "type": "retweeted", "id": "1700000000000000001" }
          ]
        }
      ],
      "includes": {
        "users": [
          { "id": "3000000000000000001", "name": "Reposter One", "username": "reposter1" },
          { "id": "2244994945", "name": "Developers", "username": "XDevelopers" }
        ],
        "tweets": [
          {
            "id": "1700000000000000001",
            "text": "hello from the timeline",
            "created_at": "2026-08-16T09:00:00.000Z",
            "author_id": "2244994945"
          }
        ]
      }
    }"#;

    #[test]
    fn a_repost_renders_as_the_original_posts_author_and_text() {
        let response: TimelineResponse = serde_json::from_str(REPOST_JSON).unwrap();
        let items = response.into_items();

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "hello from the timeline");
        assert_eq!(items[0].author_name, "Developers");
        assert_eq!(items[0].author_username, "XDevelopers");
        assert_eq!(items[0].reposted_by.as_deref(), Some("reposter1"));
        assert_eq!(items[0].quoted, None);
    }

    const QUOTE_JSON: &str = r#"{
      "data": [
        {
          "id": "1800000000000000002",
          "text": "this is worth reading",
          "created_at": "2026-08-16T11:00:00.000Z",
          "author_id": "3000000000000000001",
          "referenced_tweets": [
            { "type": "quoted", "id": "1700000000000000001" }
          ]
        }
      ],
      "includes": {
        "users": [
          { "id": "3000000000000000001", "name": "Reposter One", "username": "reposter1" },
          { "id": "2244994945", "name": "Developers", "username": "XDevelopers" }
        ],
        "tweets": [
          {
            "id": "1700000000000000001",
            "text": "hello from the timeline",
            "created_at": "2026-08-16T09:00:00.000Z",
            "author_id": "2244994945"
          }
        ]
      }
    }"#;

    /// A quote whose *quoted* post carries the media (#123). Same shape
    /// `referenced_tweets.id.attachments.media_keys` produces for a repost
    /// (#104) — the expansion covers both, since both reach their content
    /// through `referenced_tweets`.
    const QUOTE_WITH_MEDIA_JSON: &str = r#"{
      "data": [
        {
          "id": "1800000000000000003",
          "text": "look at this one",
          "created_at": "2026-08-16T11:00:00.000Z",
          "author_id": "3000000000000000001",
          "referenced_tweets": [
            { "type": "quoted", "id": "1700000000000000003" }
          ]
        }
      ],
      "includes": {
        "users": [
          { "id": "3000000000000000001", "name": "Reposter One", "username": "reposter1" },
          { "id": "2244994945", "name": "Developers", "username": "XDevelopers" }
        ],
        "tweets": [
          {
            "id": "1700000000000000003",
            "text": "the quoted post",
            "created_at": "2026-08-16T09:00:00.000Z",
            "author_id": "2244994945",
            "attachments": { "media_keys": ["k-quoted"] }
          }
        ],
        "media": [
          {
            "media_key": "k-quoted",
            "type": "photo",
            "url": "https://pbs.twimg.com/media/quoted.jpg",
            "alt_text": "the quoted post's photo"
          }
        ]
      }
    }"#;

    #[test]
    fn a_quoted_post_carries_its_own_media() {
        // #123: the card showed text only, so an image that *was* the point
        // of the quote did not appear at all.
        let response: TimelineResponse = serde_json::from_str(QUOTE_WITH_MEDIA_JSON).unwrap();
        let items = response.into_items();

        let quoted = items[0].quoted.as_ref().expect("the quote card's post");
        assert_eq!(quoted.text, "the quoted post");
        assert_eq!(quoted.media.len(), 1);
        assert_eq!(
            quoted.media[0].url,
            "https://pbs.twimg.com/media/quoted.jpg"
        );
        assert_eq!(
            quoted.media[0].alt_text.as_deref(),
            Some("the quoted post's photo")
        );
    }

    #[test]
    fn the_quoting_post_does_not_borrow_the_quoted_posts_media() {
        // The outer post has no attachments of its own here. Its own grid
        // must stay empty rather than mirroring the card's.
        let response: TimelineResponse = serde_json::from_str(QUOTE_WITH_MEDIA_JSON).unwrap();
        let items = response.into_items();

        assert!(items[0].media.is_empty());
    }

    #[test]
    fn a_quote_without_media_leaves_the_card_empty() {
        let response: TimelineResponse = serde_json::from_str(QUOTE_JSON).unwrap();
        let items = response.into_items();

        assert!(items[0].quoted.as_ref().expect("a quote").media.is_empty());
    }

    #[test]
    fn a_quote_attaches_the_quoted_post_without_replacing_the_body() {
        let response: TimelineResponse = serde_json::from_str(QUOTE_JSON).unwrap();
        let items = response.into_items();

        assert_eq!(items[0].text, "this is worth reading");
        assert_eq!(items[0].author_username, "reposter1");
        assert_eq!(items[0].reposted_by, None);
        let quoted = items[0].quoted.as_ref().unwrap();
        assert_eq!(quoted.text, "hello from the timeline");
        assert_eq!(quoted.author_name, "Developers");
        assert_eq!(quoted.author_username, "XDevelopers");
    }

    #[test]
    fn a_reply_reference_does_not_change_the_body_but_is_surfaced_as_replied_to() {
        let json = r#"{
          "data": [
            {
              "id": "1800000000000000003",
              "text": "agreed",
              "author_id": "3000000000000000001",
              "referenced_tweets": [
                { "type": "replied_to", "id": "1700000000000000001" }
              ]
            }
          ],
          "includes": {
            "users": [
              { "id": "3000000000000000001", "name": "Reposter One", "username": "reposter1" }
            ]
          }
        }"#;
        let response: TimelineResponse = serde_json::from_str(json).unwrap();
        let items = response.into_items();

        // A reply's own body/author are untouched — unlike a repost, it
        // never replaces them (#13's precedent, unchanged by #12).
        assert_eq!(items[0].text, "agreed");
        assert_eq!(items[0].reposted_by, None);
        assert_eq!(items[0].quoted, None);
        // #12: the reply *is* surfaced, even though the parent itself is
        // absent from `includes.tweets` here — the id alone (for "Show
        // thread") is worth keeping, with empty author fields rather than
        // dropping the context entirely.
        let replied_to = items[0].replied_to.as_ref().unwrap();
        assert_eq!(replied_to.post_id, "1700000000000000001");
        assert_eq!(replied_to.author_name, "");
        assert_eq!(replied_to.author_username, "");
    }

    #[test]
    fn a_reply_shows_who_it_is_replying_to_when_the_parent_is_expanded() {
        // #12: the parent's author is already in `includes` thanks to #13's
        // `referenced_tweets.id.author_id` expansion, so this costs no
        // extra request.
        let json = r#"{
          "data": [
            {
              "id": "1800000000000000006",
              "text": "agreed",
              "author_id": "3000000000000000001",
              "referenced_tweets": [
                { "type": "replied_to", "id": "1700000000000000001" }
              ]
            }
          ],
          "includes": {
            "users": [
              { "id": "3000000000000000001", "name": "Reposter One", "username": "reposter1" },
              { "id": "2244994945", "name": "Developers", "username": "XDevelopers" }
            ],
            "tweets": [
              {
                "id": "1700000000000000001",
                "text": "hello from the timeline",
                "author_id": "2244994945"
              }
            ]
          }
        }"#;
        let response: TimelineResponse = serde_json::from_str(json).unwrap();
        let items = response.into_items();

        let replied_to = items[0].replied_to.as_ref().unwrap();
        assert_eq!(replied_to.post_id, "1700000000000000001");
        assert_eq!(replied_to.author_name, "Developers");
        assert_eq!(replied_to.author_username, "XDevelopers");
    }

    #[test]
    fn a_non_reply_post_has_no_reply_target() {
        let response: TimelineResponse = serde_json::from_str(TIMELINE_JSON).unwrap();
        let items = response.into_items();
        assert_eq!(items[0].replied_to, None);
    }

    #[test]
    fn a_repost_of_a_reply_carries_the_originals_reply_target() {
        // Mirrors #13's "repost of a quote" precedent: once the body
        // becomes the original post's, any reply context worth showing is
        // the *original's* — the outer retweet reference has been fully
        // consumed already.
        let json = r#"{
          "data": [
            {
              "id": "1800000000000000007",
              "text": "RT @quoter1: agreed",
              "author_id": "3000000000000000001",
              "referenced_tweets": [
                { "type": "retweeted", "id": "1700000000000000003" }
              ]
            }
          ],
          "includes": {
            "users": [
              { "id": "3000000000000000001", "name": "Reposter One", "username": "reposter1" },
              { "id": "4000000000000000001", "name": "Quote Author", "username": "quoter1" },
              { "id": "2244994945", "name": "Developers", "username": "XDevelopers" }
            ],
            "tweets": [
              {
                "id": "1700000000000000003",
                "text": "agreed",
                "author_id": "4000000000000000001",
                "referenced_tweets": [
                  { "type": "replied_to", "id": "1700000000000000001" }
                ]
              },
              {
                "id": "1700000000000000001",
                "text": "hello from the timeline",
                "author_id": "2244994945"
              }
            ]
          }
        }"#;
        let response: TimelineResponse = serde_json::from_str(json).unwrap();
        let items = response.into_items();

        assert_eq!(items[0].text, "agreed");
        assert_eq!(items[0].reposted_by.as_deref(), Some("reposter1"));
        let replied_to = items[0].replied_to.as_ref().unwrap();
        assert_eq!(replied_to.post_id, "1700000000000000001");
        assert_eq!(replied_to.author_username, "XDevelopers");
    }

    #[test]
    fn a_repost_whose_original_is_missing_from_includes_falls_back_to_its_own_text() {
        // The referenced post can be deleted, protected, or simply absent
        // from `includes` — this must render something sensible rather than
        // an empty row or a panic.
        let json = r#"{
          "data": [
            {
              "id": "1800000000000000004",
              "text": "RT @someone: a post that was later deleted",
              "author_id": "3000000000000000001",
              "referenced_tweets": [
                { "type": "retweeted", "id": "9999999999999999999" }
              ]
            }
          ],
          "includes": {
            "users": [
              { "id": "3000000000000000001", "name": "Reposter One", "username": "reposter1" }
            ]
          }
        }"#;
        let response: TimelineResponse = serde_json::from_str(json).unwrap();
        let items = response.into_items();

        assert_eq!(items[0].text, "RT @someone: a post that was later deleted");
        assert_eq!(items[0].author_name, "");
        assert_eq!(items[0].author_username, "");
        assert_eq!(items[0].reposted_by.as_deref(), Some("reposter1"));
    }

    const REPOST_OF_QUOTE_JSON: &str = r#"{
      "data": [
        {
          "id": "1800000000000000005",
          "text": "RT @quoter1: this is worth reading",
          "author_id": "3000000000000000001",
          "referenced_tweets": [
            { "type": "retweeted", "id": "1700000000000000002" }
          ]
        }
      ],
      "includes": {
        "users": [
          { "id": "3000000000000000001", "name": "Reposter One", "username": "reposter1" },
          { "id": "4000000000000000001", "name": "Quote Author", "username": "quoter1" },
          { "id": "2244994945", "name": "Developers", "username": "XDevelopers" }
        ],
        "tweets": [
          {
            "id": "1700000000000000002",
            "text": "this is worth reading",
            "author_id": "4000000000000000001",
            "referenced_tweets": [
              { "type": "quoted", "id": "1700000000000000001" }
            ]
          },
          {
            "id": "1700000000000000001",
            "text": "hello from the timeline",
            "author_id": "2244994945"
          }
        ]
      }
    }"#;

    #[test]
    fn a_repost_of_a_quote_carries_the_nested_quote_card() {
        // #13's precedence: retweeted wins the rendered body, but the quote
        // the reposted post itself carries is still worth showing — the
        // card comes from the *reposted* post's own `quoted` reference, not
        // the top-level post's (which has none).
        let response: TimelineResponse = serde_json::from_str(REPOST_OF_QUOTE_JSON).unwrap();
        let items = response.into_items();

        assert_eq!(items[0].text, "this is worth reading");
        assert_eq!(items[0].author_username, "quoter1");
        assert_eq!(items[0].reposted_by.as_deref(), Some("reposter1"));
        let quoted = items[0].quoted.as_ref().unwrap();
        assert_eq!(quoted.text, "hello from the timeline");
        assert_eq!(quoted.author_username, "XDevelopers");
    }

    #[test]
    fn an_unrecognized_reference_type_does_not_fail_parsing() {
        // Forward compatibility: a future API revision adding a new
        // `referenced_tweets[].type` value must not break parsing the whole
        // response, the same way a corrupt cache file is a clean miss.
        let json = r#"{
          "data": [
            {
              "id": "1",
              "text": "future api shape",
              "referenced_tweets": [ { "type": "some_future_type", "id": "2" } ]
            }
          ]
        }"#;
        let response: TimelineResponse = serde_json::from_str(json).unwrap();
        let items = response.into_items();

        assert_eq!(items[0].text, "future api shape");
        assert_eq!(items[0].reposted_by, None);
        assert_eq!(items[0].quoted, None);
    }

    #[test]
    fn a_timeline_item_from_before_13_still_deserializes() {
        // Pre-#13 cache files on disk have none of the new fields — this
        // must keep parsing them rather than throwing every user's cache
        // away (see `cache::load_json`'s doc comment). Deliberately a raw
        // literal rather than trusting the `#[serde(default)]` attributes at
        // a glance.
        let old_format = r#"{
          "id": "1700000000000000001",
          "text": "hello from the timeline",
          "created_at": "2026-08-16T09:00:00.000Z",
          "author_name": "Developers",
          "author_username": "XDevelopers"
        }"#;
        let item: TimelineItem = serde_json::from_str(old_format).unwrap();
        assert_eq!(item.id, "1700000000000000001");
        assert_eq!(item.text, "hello from the timeline");
        assert_eq!(item.author_name, "Developers");
        assert_eq!(item.reposted_by, None);
        assert_eq!(item.quoted, None);
        assert_eq!(item.replied_to, None);
    }

    #[test]
    fn a_timeline_item_from_before_12_still_deserializes() {
        // #12 adds `replied_to` on top of #13's `reposted_by`/`quoted`. A
        // cache file written by a #13-era build has neither key for it —
        // this must not throw the whole cache away (see
        // `cache::load_json`'s doc comment). Deliberately a raw literal
        // rather than trusting `#[serde(default)]` at a glance, mirroring
        // the sibling test above.
        let pre_12_format = r#"{
          "id": "1800000000000000001",
          "text": "RT @XDevelopers: hello from the timeline",
          "created_at": "2026-08-16T10:00:00.000Z",
          "author_name": "Developers",
          "author_username": "XDevelopers",
          "reposted_by": "reposter1"
        }"#;
        let item: TimelineItem = serde_json::from_str(pre_12_format).unwrap();
        assert_eq!(item.id, "1800000000000000001");
        assert_eq!(item.reposted_by.as_deref(), Some("reposter1"));
        assert_eq!(item.quoted, None);
        assert_eq!(item.replied_to, None);
    }

    #[test]
    fn serializes_the_post_tweet_request_body_without_a_quote() {
        // #14/#16: an ordinary post must omit `quote_tweet_id` entirely
        // rather than sending it as `null` — X may reject a stray null
        // outright, so this checks the exact serialized shape, not just
        // that `quote_tweet_id` deserializes back to `None`.
        let request = Draft {
            text: "hello",
            quote_tweet_id: None,
            reply_to_post_id: None,
        }
        .to_request();
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"text":"hello"}"#
        );
    }

    #[test]
    fn serializes_the_post_tweet_request_body_with_a_reply() {
        // #71: a reply is the same endpoint with a nested `reply` object —
        // and the id inside it is what decides which conversation this
        // lands under, so the exact shape is worth pinning.
        let request = Draft {
            text: "hello",
            quote_tweet_id: None,
            reply_to_post_id: Some("1700000000000000001"),
        }
        .to_request();
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"text":"hello","reply":{"in_reply_to_tweet_id":"1700000000000000001"}}"#
        );
    }

    #[test]
    fn serializes_the_post_tweet_request_body_with_a_quote() {
        // #16: `POST /2/tweets` gains `quote_tweet_id` rather than a
        // separate quote endpoint — this is the whole body a quote post
        // sends.
        let request = Draft {
            text: "hello",
            quote_tweet_id: Some("1700000000000000001"),
            reply_to_post_id: None,
        }
        .to_request();
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"text":"hello","quote_tweet_id":"1700000000000000001"}"#
        );
    }

    #[test]
    fn serializes_the_list_member_request_body() {
        // #163: the whole body `XClient::add_list_member` sends.
        let request = UserIdRequest {
            user_id: "2244994945",
        };
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"user_id":"2244994945"}"#
        );
    }

    #[test]
    fn parses_a_page_of_users_with_its_cursor() {
        let body = r#"{
            "data": [
                {"id": "1", "name": "Alice", "username": "alice"},
                {"id": "2", "name": "Bob", "username": "bob"}
            ],
            "meta": {"next_token": "cursor-abc"}
        }"#;
        let page: UserPageResponse = serde_json::from_str(body).unwrap();
        assert_eq!(
            page.data.iter().map(|u| u.id.as_str()).collect::<Vec<_>>(),
            ["1", "2"]
        );
        assert_eq!(page.next_token(), Some("cursor-abc"));
    }

    #[test]
    fn parses_the_last_page_of_users() {
        // No `next_token` is how both endpoints say "that was the end".
        let page: UserPageResponse =
            serde_json::from_str(r#"{"data": [{"id": "1", "name": "A", "username": "a"}]}"#)
                .unwrap();
        assert_eq!(page.next_token(), None);
    }

    #[test]
    fn parses_an_empty_page_of_users() {
        // #163: an account following nobody, or a list with no members,
        // omits `data` rather than sending `[]`. Parsing that as an error
        // would fail the very first sync.
        let page: UserPageResponse =
            serde_json::from_str(r#"{"meta": {"result_count": 0}}"#).unwrap();
        assert!(page.data.is_empty());
        assert_eq!(page.next_token(), None);
    }

    #[test]
    fn serializes_the_shared_tweet_id_request_body() {
        // #15: the whole request body `x_api::client::XClient::create_repost`
        // sends.
        let request = TweetIdRequest {
            tweet_id: "1700000000000000001",
        };
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"tweet_id":"1700000000000000001"}"#
        );
    }

    #[test]
    fn reads_a_nested_errors_body() {
        let problem: ApiProblem = serde_json::from_str(
            r#"{"errors":[{"title":"Not Found Error","detail":"Could not find user."}]}"#,
        )
        .unwrap();
        assert_eq!(
            problem.message().as_deref(),
            Some("Not Found Error: Could not find user.")
        );
    }

    const METRICS_JSON: &str = r#"{
      "data": [
        {
          "id": "1700000000000000001",
          "text": "a post with engagement",
          "created_at": "2026-08-16T09:00:00.000Z",
          "author_id": "2244994945",
          "public_metrics": {
            "retweet_count": 34,
            "reply_count": 12,
            "like_count": 56,
            "quote_count": 7,
            "impression_count": 8900
          }
        },
        {
          "id": "1700000000000000002",
          "text": "a post from a response that predates public_metrics",
          "author_id": "2244994945"
        }
      ],
      "includes": {
        "users": [
          { "id": "2244994945", "name": "Developers", "username": "XDevelopers" }
        ]
      }
    }"#;

    #[test]
    fn reads_public_metrics_into_the_item() {
        // #67: no extra request — these ride along in the timeline response
        // once `tweet.fields` asks for `public_metrics`.
        let response: TimelineResponse = serde_json::from_str(METRICS_JSON).unwrap();
        let items = response.into_items();

        assert_eq!(
            items[0].metrics,
            Some(PostMetrics {
                replies: 12,
                reposts: 34,
                likes: 56,
            })
        );
    }

    #[test]
    fn metrics_are_none_when_the_response_omits_them() {
        // A response that predates #67's `tweet.fields` change — or a post
        // X declines to report counts for — must parse, not fail.
        let response: TimelineResponse = serde_json::from_str(METRICS_JSON).unwrap();
        let items = response.into_items();

        assert_eq!(items[1].metrics, None);
    }

    #[test]
    fn a_repost_shows_the_originals_metrics() {
        // The rendered body is the original post (#13), so the counts shown
        // beneath it have to be the original's too.
        let json = r#"{
          "data": [
            {
              "id": "1700000000000000010",
              "text": "RT @XDevelopers: the original",
              "author_id": "1000000000",
              "public_metrics": { "retweet_count": 1, "reply_count": 0, "like_count": 0 },
              "referenced_tweets": [{ "type": "retweeted", "id": "1700000000000000011" }]
            }
          ],
          "includes": {
            "users": [
              { "id": "1000000000", "name": "Reposter", "username": "reposter" },
              { "id": "2244994945", "name": "Developers", "username": "XDevelopers" }
            ],
            "tweets": [
              {
                "id": "1700000000000000011",
                "text": "the original",
                "author_id": "2244994945",
                "public_metrics": { "retweet_count": 99, "reply_count": 5, "like_count": 400 }
              }
            ]
          }
        }"#;
        let items: TimelineResponse = serde_json::from_str(json).unwrap();
        let items = items.into_items();

        assert_eq!(
            items[0].metrics,
            Some(PostMetrics {
                replies: 5,
                reposts: 99,
                likes: 400,
            })
        );
    }

    #[test]
    fn a_repost_whose_original_is_missing_reports_no_metrics() {
        // Same reasoning as the author fields `build_item` blanks in this
        // case: the outer post's own counts are not the original's.
        let json = r#"{
          "data": [
            {
              "id": "1700000000000000010",
              "text": "RT @XDevelopers: the original",
              "author_id": "1000000000",
              "public_metrics": { "retweet_count": 1, "reply_count": 0, "like_count": 0 },
              "referenced_tweets": [{ "type": "retweeted", "id": "1700000000000000011" }]
            }
          ]
        }"#;
        let response: TimelineResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.into_items()[0].metrics, None);
    }

    const LINKS_JSON: &str = r#"{
      "data": [
        {
          "id": "1700000000000000001",
          "text": "read this https://t.co/abc and this https://t.co/abc again",
          "author_id": "2244994945",
          "entities": {
            "urls": [
              {
                "url": "https://t.co/abc",
                "expanded_url": "https://example.com/an-article",
                "display_url": "example.com/an-article"
              },
              {
                "url": "https://t.co/abc",
                "expanded_url": "https://example.com/an-article",
                "display_url": "example.com/an-article"
              },
              { "url": "https://t.co/xyz" }
            ]
          }
        },
        {
          "id": "1700000000000000002",
          "text": "no links here",
          "author_id": "2244994945"
        }
      ]
    }"#;

    #[test]
    fn expands_the_links_in_a_posts_text() {
        // #70: the text carries t.co shortlinks; `expanded_url` is the only
        // way to the real destination without following a redirect.
        let response: TimelineResponse = serde_json::from_str(LINKS_JSON).unwrap();
        let items = response.into_items();

        assert_eq!(
            items[0].links,
            vec![PostLink {
                url: "https://example.com/an-article".to_string(),
                label: "example.com/an-article".to_string(),
            }],
            "the repeated entity must collapse, and the one with no \
             expanded_url must be dropped"
        );
    }

    #[test]
    fn a_post_with_no_entities_has_no_links() {
        let response: TimelineResponse = serde_json::from_str(LINKS_JSON).unwrap();
        assert!(response.into_items()[1].links.is_empty());
    }

    #[test]
    fn a_link_without_a_display_url_falls_back_to_the_url_itself() {
        let json = r#"{
          "data": [
            {
              "id": "1",
              "text": "t",
              "entities": { "urls": [{ "expanded_url": "https://example.com/x" }] }
            }
          ]
        }"#;
        let response: TimelineResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            response.into_items()[0].links,
            vec![PostLink {
                url: "https://example.com/x".to_string(),
                label: "https://example.com/x".to_string(),
            }]
        );
    }

    #[test]
    fn a_repost_carries_the_originals_links() {
        // The body is the original's text, so its t.co links are the ones
        // the row can actually resolve.
        let json = r#"{
          "data": [
            {
              "id": "10",
              "text": "RT @XDevelopers: read this https://t.co/abc",
              "author_id": "1000000000",
              "referenced_tweets": [{ "type": "retweeted", "id": "11" }]
            }
          ],
          "includes": {
            "users": [
              { "id": "1000000000", "name": "Reposter", "username": "reposter" },
              { "id": "2244994945", "name": "Developers", "username": "XDevelopers" }
            ],
            "tweets": [
              {
                "id": "11",
                "text": "read this https://t.co/abc",
                "author_id": "2244994945",
                "entities": {
                  "urls": [
                    {
                      "expanded_url": "https://example.com/original",
                      "display_url": "example.com/original"
                    }
                  ]
                }
              }
            ]
          }
        }"#;
        let response: TimelineResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            response.into_items()[0].links,
            vec![PostLink {
                url: "https://example.com/original".to_string(),
                label: "example.com/original".to_string(),
            }]
        );
    }

    #[test]
    fn a_repost_whose_original_is_missing_reports_no_links() {
        let json = r#"{
          "data": [
            {
              "id": "10",
              "text": "RT @XDevelopers: read this https://t.co/abc",
              "author_id": "1000000000",
              "entities": {
                "urls": [{ "expanded_url": "https://example.com/outer" }]
              },
              "referenced_tweets": [{ "type": "retweeted", "id": "11" }]
            }
          ]
        }"#;
        let response: TimelineResponse = serde_json::from_str(json).unwrap();
        assert!(response.into_items()[0].links.is_empty());
    }

    #[test]
    fn reads_the_authors_avatar_url() {
        // #64: `user.fields=profile_image_url` puts this in `includes.users`,
        // where the author join already looks.
        let response: TimelineResponse = serde_json::from_str(TIMELINE_JSON).unwrap();
        let items = response.into_items();

        assert_eq!(
            items[0].author_avatar_url.as_deref(),
            Some("https://pbs.twimg.com/profile_images/x.jpg")
        );
        // The second post's author was never expanded, so there is no
        // avatar to show either.
        assert_eq!(items[1].author_avatar_url, None);
    }

    #[test]
    fn a_repost_shows_the_original_authors_avatar() {
        // The byline is the original author's (#13), so the face beside it
        // has to be theirs too.
        let json = r#"{
          "data": [
            {
              "id": "10",
              "text": "RT @XDevelopers: the original",
              "author_id": "1000000000",
              "referenced_tweets": [{ "type": "retweeted", "id": "11" }]
            }
          ],
          "includes": {
            "users": [
              {
                "id": "1000000000",
                "name": "Reposter",
                "username": "reposter",
                "profile_image_url": "https://pbs.twimg.com/reposter_normal.jpg"
              },
              {
                "id": "2244994945",
                "name": "Developers",
                "username": "XDevelopers",
                "profile_image_url": "https://pbs.twimg.com/original_normal.jpg"
              }
            ],
            "tweets": [
              { "id": "11", "text": "the original", "author_id": "2244994945" }
            ]
          }
        }"#;
        let response: TimelineResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            response.into_items()[0].author_avatar_url.as_deref(),
            Some("https://pbs.twimg.com/original_normal.jpg")
        );
    }

    const MEDIA_JSON: &str = r#"{
      "data": [
        {
          "id": "1",
          "text": "with photos",
          "author_id": "2244994945",
          "attachments": { "media_keys": ["k-photo", "k-video", "k-missing"] }
        },
        { "id": "2", "text": "no attachments", "author_id": "2244994945" }
      ],
      "includes": {
        "users": [
          { "id": "2244994945", "name": "Developers", "username": "XDevelopers" }
        ],
        "media": [
          {
            "media_key": "k-photo",
            "type": "photo",
            "url": "https://pbs.twimg.com/media/photo.jpg",
            "width": 1200,
            "height": 675,
            "alt_text": "a chart"
          },
          {
            "media_key": "k-video",
            "type": "video",
            "preview_image_url": "https://pbs.twimg.com/media/still.jpg",
            "width": 1280,
            "height": 720
          }
        ]
      }
    }"#;

    #[test]
    fn joins_attached_media_by_key_in_the_posts_own_order() {
        // #65: the same side-table join `users` and `tweets` already use.
        let response: TimelineResponse = serde_json::from_str(MEDIA_JSON).unwrap();
        let items = response.into_items();

        assert_eq!(items[0].media.len(), 2, "the unmatched key must be skipped");
        assert_eq!(
            items[0].media[0].url,
            "https://pbs.twimg.com/media/photo.jpg"
        );
        assert_eq!(items[0].media[0].alt_text.as_deref(), Some("a chart"));
        assert_eq!(items[0].media[0].kind.as_deref(), Some("photo"));
    }

    #[test]
    fn a_video_falls_back_to_its_preview_still() {
        // This app doesn't play video; the still plus a badge is the whole
        // rendering, so `preview_image_url` is what has to come through.
        let response: TimelineResponse = serde_json::from_str(MEDIA_JSON).unwrap();
        let items = response.into_items();

        assert_eq!(
            items[0].media[1].url,
            "https://pbs.twimg.com/media/still.jpg"
        );
        assert_eq!(items[0].media[1].kind.as_deref(), Some("video"));
    }

    #[test]
    fn media_with_nothing_displayable_is_dropped() {
        // Neither `url` nor `preview_image_url`: there is nothing to draw,
        // and a hole in the grid is worse than one fewer thumbnail.
        let json = r#"{
          "data": [{ "id": "1", "text": "t", "attachments": { "media_keys": ["k"] } }],
          "includes": { "media": [{ "media_key": "k", "type": "photo" }] }
        }"#;
        let response: TimelineResponse = serde_json::from_str(json).unwrap();
        assert!(response.into_items()[0].media.is_empty());
    }

    #[test]
    fn a_post_without_attachments_has_no_media() {
        let response: TimelineResponse = serde_json::from_str(MEDIA_JSON).unwrap();
        assert!(response.into_items()[1].media.is_empty());
    }

    #[test]
    fn a_repost_carries_the_originals_media() {
        // The body is the original's text, so the images under it have to
        // be the original's too.
        let json = r#"{
          "data": [
            {
              "id": "10",
              "text": "RT @XDevelopers: look",
              "author_id": "1000000000",
              "referenced_tweets": [{ "type": "retweeted", "id": "11" }]
            }
          ],
          "includes": {
            "users": [
              { "id": "1000000000", "name": "R", "username": "reposter" },
              { "id": "2244994945", "name": "D", "username": "XDevelopers" }
            ],
            "tweets": [
              {
                "id": "11",
                "text": "look",
                "author_id": "2244994945",
                "attachments": { "media_keys": ["k"] }
              }
            ],
            "media": [
              {
                "media_key": "k",
                "type": "photo",
                "url": "https://pbs.twimg.com/media/original.jpg"
              }
            ]
          }
        }"#;
        let response: TimelineResponse = serde_json::from_str(json).unwrap();
        let items = response.into_items();
        assert_eq!(items[0].media.len(), 1);
        assert_eq!(
            items[0].media[0].url,
            "https://pbs.twimg.com/media/original.jpg"
        );
    }

    #[test]
    fn a_cache_file_written_before_media_existed_still_loads() {
        let item: TimelineItem = serde_json::from_str(
            r#"{"id":"1","text":"cached","created_at":null,"author_name":"a","author_username":"b"}"#,
        )
        .unwrap();
        assert!(item.media.is_empty());
    }

    #[test]
    fn a_repost_carries_the_original_posts_id() {
        // #52: `id` stays the retweet activity's, but every write endpoint
        // needs the original's — which the reference already names.
        let json = r#"{
          "data": [
            {
              "id": "1700000000000000010",
              "text": "RT @XDevelopers: the original",
              "author_id": "1000000000",
              "referenced_tweets": [{ "type": "retweeted", "id": "1700000000000000011" }]
            }
          ],
          "includes": {
            "users": [
              { "id": "1000000000", "name": "Reposter", "username": "reposter" },
              { "id": "2244994945", "name": "Developers", "username": "XDevelopers" }
            ],
            "tweets": [
              { "id": "1700000000000000011", "text": "the original", "author_id": "2244994945" }
            ]
          }
        }"#;
        let response: TimelineResponse = serde_json::from_str(json).unwrap();
        let items = response.into_items();

        assert_eq!(items[0].id, "1700000000000000010");
        assert_eq!(
            items[0].original_post_id.as_deref(),
            Some("1700000000000000011")
        );
        assert_eq!(action_post_id(&items[0]), "1700000000000000011");
    }

    #[test]
    fn a_repost_whose_original_is_missing_still_carries_its_id() {
        // The id comes from the reference, not from the expansion, so a
        // deleted or unexpanded original does not cost the button.
        let json = r#"{
          "data": [
            {
              "id": "10",
              "text": "RT @XDevelopers: gone",
              "author_id": "1000000000",
              "referenced_tweets": [{ "type": "retweeted", "id": "11" }]
            }
          ]
        }"#;
        let response: TimelineResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            response.into_items()[0].original_post_id.as_deref(),
            Some("11")
        );
    }

    #[test]
    fn an_ordinary_post_carries_no_original_id() {
        let response: TimelineResponse = serde_json::from_str(TIMELINE_JSON).unwrap();
        let items = response.into_items();
        assert_eq!(items[0].original_post_id, None);
        assert_eq!(action_post_id(&items[0]), items[0].id);
    }

    #[test]
    fn a_cache_file_written_before_the_original_id_existed_still_loads() {
        // Raw literal on purpose: `cache::load_json` turns a parse failure
        // into a *silent* miss, so a missing `#[serde(default)]` would
        // quietly discard every user's cache and re-fetch it at their
        // expense. Eyeballing the attribute is not the same as checking.
        let item: TimelineItem = serde_json::from_str(
            r#"{"id":"1","text":"cached","created_at":null,"author_name":"a","author_username":"b","reposted_by":"c"}"#,
        )
        .unwrap();
        assert_eq!(item.original_post_id, None);
        assert_eq!(action_post_id(&item), "1");
    }

    #[test]
    fn a_cache_file_written_before_avatars_existed_still_loads() {
        let item: TimelineItem = serde_json::from_str(
            r#"{"id":"1","text":"cached","created_at":null,"author_name":"a","author_username":"b"}"#,
        )
        .unwrap();
        assert_eq!(item.author_avatar_url, None);
    }

    #[test]
    fn a_cache_file_written_before_links_existed_still_loads() {
        let item: TimelineItem = serde_json::from_str(
            r#"{"id":"1","text":"cached","created_at":null,"author_name":"a","author_username":"b"}"#,
        )
        .unwrap();
        assert!(item.links.is_empty());
    }

    #[test]
    fn a_cache_file_written_before_metrics_existed_still_loads() {
        // #9's cache file is this exact type, so a file on disk from before
        // #67 must deserialize with the field simply absent.
        let item: TimelineItem = serde_json::from_str(
            r#"{"id":"1","text":"cached","created_at":null,"author_name":"a","author_username":"b"}"#,
        )
        .unwrap();
        assert_eq!(item.metrics, None);
    }
}
