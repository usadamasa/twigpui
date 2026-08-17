use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A user object as returned under `data` or `includes.users`.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct User {
    pub id: String,
    pub name: String,
    pub username: String,
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

        self.data
            .iter()
            .map(|post| build_item(post, &users, &referenced))
            .collect()
    }
}

/// A post's author name/username from `includes.users`, or a pair of empty
/// strings when the author id is absent or wasn't expanded — the shared
/// lookup behind every author field [`build_item`] and [`quote_of`] fill in.
fn author_fields(post: &Post, users: &HashMap<&str, &User>) -> (String, String) {
    let author = post
        .author_id
        .as_deref()
        .and_then(|id| users.get(id).copied());
    (
        author.map(|u| u.name.clone()).unwrap_or_default(),
        author.map(|u| u.username.clone()).unwrap_or_default(),
    )
}

/// Join one post with its author, and — if it references another post —
/// with that reference too, per the precedence documented on
/// [`TimelineResponse::into_items`].
fn build_item(
    post: &Post,
    users: &HashMap<&str, &User>,
    referenced: &HashMap<&str, &Post>,
) -> TimelineItem {
    let (author_name, author_username) = author_fields(post, users);
    let mut item = TimelineItem {
        id: post.id.clone(),
        text: post.text.clone(),
        created_at: post.created_at.clone(),
        author_name,
        author_username,
        reposted_by: None,
        quoted: None,
        replied_to: None,
    };

    if let Some(retweet_ref) = post
        .referenced_tweets
        .iter()
        .find(|r| r.kind == ReferenceKind::Retweeted)
    {
        // The outer post's own author is whoever reposted — captured before
        // it's overwritten below with the original's author.
        item.reposted_by = Some(item.author_username.clone());

        if let Some(original) = referenced.get(retweet_ref.id.as_str()).copied() {
            let (author_name, author_username) = author_fields(original, users);
            item.text.clone_from(&original.text);
            item.author_name = author_name;
            item.author_username = author_username;
            // A repost of a quote — or of a reply — carries context that
            // belongs to the original post now shown as the body, not to
            // the (already-consumed) retweet reference on the outer post.
            item.quoted = quote_of(original, users, referenced);
            item.replied_to = reply_target(original, users, referenced);
        } else {
            // Original is gone from `includes` — keep the outer post's own
            // (possibly truncated `RT @user: …`) text already set above
            // rather than blanking the row, but drop the author fields the
            // same way a post whose author never expanded already does: we
            // know who reposted, not who wrote it.
            item.author_name = String::new();
            item.author_username = String::new();
        }
    } else {
        if post
            .referenced_tweets
            .iter()
            .any(|r| r.kind == ReferenceKind::Quoted)
        {
            item.quoted = quote_of(post, users, referenced);
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
    let (author_name, author_username) = referenced
        .get(reply_ref.id.as_str())
        .map(|parent| author_fields(parent, users))
        .unwrap_or_default();
    Some(RepliedTo {
        post_id: reply_ref.id.clone(),
        author_name,
        author_username,
    })
}

/// The post `post` quotes, if it has a `quoted` reference and that post is
/// present in `includes.tweets`. `None` either way is a legitimate outcome
/// for [`build_item`] to fall back on (no card, not an error) — a quoted
/// post can be deleted, protected, or simply absent from the expansion.
fn quote_of(
    post: &Post,
    users: &HashMap<&str, &User>,
    referenced: &HashMap<&str, &Post>,
) -> Option<QuotedPost> {
    let quote_ref = post
        .referenced_tweets
        .iter()
        .find(|r| r.kind == ReferenceKind::Quoted)?;
    let quoted_post = referenced.get(quote_ref.id.as_str())?;
    let (author_name, author_username) = author_fields(quoted_post, users);
    Some(QuotedPost {
        author_name,
        author_username,
        text: quoted_post.text.clone(),
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
}
