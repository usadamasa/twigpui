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
    /// TODO(#13): the join below is a stub — it always leaves `reposted_by`
    /// and `quoted` at `None`. Implemented in the follow-up commit; this
    /// exists only so the new fixture tests compile and fail on behavior
    /// rather than on a missing type.
    ///
    /// Posts whose author is absent from the expansion still render, with the
    /// author fields left empty — dropping them would silently hide content.
    pub(crate) fn into_items(self) -> Vec<TimelineItem> {
        let users: HashMap<&str, &User> = self
            .includes
            .users
            .iter()
            .map(|u| (u.id.as_str(), u))
            .collect();

        self.data
            .iter()
            .map(|post| {
                let author = post
                    .author_id
                    .as_deref()
                    .and_then(|id| users.get(id).copied());
                TimelineItem {
                    id: post.id.clone(),
                    text: post.text.clone(),
                    created_at: post.created_at.clone(),
                    author_name: author.map(|u| u.name.clone()).unwrap_or_default(),
                    author_username: author.map(|u| u.username.clone()).unwrap_or_default(),
                    reposted_by: None,
                    quoted: None,
                }
            })
            .collect()
    }
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
    fn a_reply_reference_is_recognized_but_does_not_change_rendering() {
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

        // A reply is recognized as a type but not rendered specially in
        // #13 — thread display is #12's scope — so the body is untouched.
        assert_eq!(items[0].text, "agreed");
        assert_eq!(items[0].reposted_by, None);
        assert_eq!(items[0].quoted, None);
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
