use std::collections::HashMap;

use serde::Deserialize;

/// A user object as returned under `data` or `includes.users`.
#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub id: String,
    pub name: String,
    pub username: String,
}

/// A post object as returned under `data`.
#[derive(Debug, Clone, Deserialize)]
pub struct Post {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub author_id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Includes {
    #[serde(default)]
    pub users: Vec<User>,
}

/// The `errors` array X returns alongside partial results, and also the
/// problem-details body it returns for outright failures.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ApiProblem {
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
    pub fn message(&self) -> Option<String> {
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
        self.errors.iter().find_map(|e| e.message())
    }
}

#[derive(Debug, Deserialize)]
pub struct UserLookupResponse {
    #[serde(default)]
    pub data: Option<User>,
}

#[derive(Debug, Default, Deserialize)]
pub struct TimelineResponse {
    #[serde(default)]
    pub data: Vec<Post>,
    #[serde(default)]
    pub includes: Includes,
}

/// A post flattened with its author, ready for rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineItem {
    pub id: String,
    pub text: String,
    pub created_at: Option<String>,
    pub author_name: String,
    pub author_username: String,
}

impl TimelineResponse {
    /// Join each post with its author from `includes.users`.
    ///
    /// Posts whose author is absent from the expansion still render, with the
    /// author fields left empty — dropping them would silently hide content.
    pub fn into_items(self) -> Vec<TimelineItem> {
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
      "meta": { "result_count": 2 }
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
        assert_eq!(problem.message().as_deref(), Some("Unauthorized: Unauthorized"));
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
