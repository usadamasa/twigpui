use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use ureq::Agent;

use super::model::{ApiProblem, TimelineItem, TimelineResponse, UserLookupResponse};

const API_BASE: &str = "https://api.x.com/2";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Blocking client for the read endpoints reachable with an app-only Bearer token.
///
/// Every call is billed against the account's API credits, so the UI fetches
/// only on explicit user action (initial load and the reload button).
#[derive(Clone)]
pub(crate) struct XClient {
    agent: Agent,
    bearer_token: String,
}

impl XClient {
    pub(crate) fn new(bearer_token: String) -> Self {
        let config = Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
            // Read the body ourselves so failures carry the API's own explanation
            // instead of a bare status code.
            .http_status_as_error(false)
            .build();
        Self {
            agent: config.into(),
            bearer_token,
        }
    }

    fn get(&self, url: &str) -> Result<(u16, String)> {
        let mut response = self
            .agent
            .get(url)
            .header("Authorization", format!("Bearer {}", self.bearer_token))
            .call()
            .with_context(|| format!("request to {url} failed"))?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .context("could not read the response body")?;
        Ok((status, body))
    }

    /// Resolve a screen name to the numeric user id the timeline endpoint needs.
    pub(crate) fn user_id_by_username(&self, username: &str) -> Result<String> {
        let url = user_lookup_url(username);
        let (status, body) = self.get(&url)?;
        check_status(status, &body)?;

        let response: UserLookupResponse =
            serde_json::from_str(&body).context("could not parse the user lookup response")?;
        match response.data {
            Some(user) => Ok(user.id),
            // A 200 with no `data` is how X reports an unknown screen name.
            None => match describe_problem(&body) {
                Some(message) => bail!("could not resolve @{username}: {message}"),
                None => bail!("could not resolve @{username}: the API returned no user"),
            },
        }
    }

    /// Fetch recent posts for `user_id` directly (the caller already
    /// resolved the screen name — see `cache::reload`), newest first.
    /// `since_id`, when given, asks the API to return only posts newer than
    /// it, keeping both the response and the credit cost down on an
    /// incremental reload.
    pub(crate) fn timeline(
        &self,
        user_id: &str,
        max_results: u32,
        since_id: Option<&str>,
    ) -> Result<Vec<TimelineItem>> {
        let url = timeline_url(user_id, max_results, since_id);
        let (status, body) = self.get(&url)?;
        check_status(status, &body)?;

        let response: TimelineResponse =
            serde_json::from_str(&body).context("could not parse the timeline response")?;
        Ok(response.into_items())
    }
}

fn user_lookup_url(username: &str) -> String {
    format!("{API_BASE}/users/by/username/{username}")
}

/// The timeline endpoint returns bare post ids unless `expansions` and the
/// `*.fields` parameters ask for more, so the query string is load-bearing.
fn timeline_url(user_id: &str, max_results: u32, since_id: Option<&str>) -> String {
    let base = format!(
        "{API_BASE}/users/{user_id}/tweets\
         ?max_results={max_results}\
         &tweet.fields=created_at\
         &expansions=author_id\
         &user.fields=name,username"
    );
    match since_id {
        Some(id) => format!("{base}&since_id={id}"),
        None => base,
    }
}

/// Pull the API's own error text out of a response body, if it has any.
fn describe_problem(body: &str) -> Option<String> {
    serde_json::from_str::<ApiProblem>(body)
        .ok()
        .and_then(|problem| problem.message())
}

fn check_status(status: u16, body: &str) -> Result<()> {
    if (200..300).contains(&status) {
        return Ok(());
    }

    let detail = describe_problem(body).unwrap_or_else(|| {
        let snippet: String = body.chars().take(400).collect();
        if snippet.is_empty() {
            "(empty response body)".to_string()
        } else {
            snippet
        }
    });

    match status {
        401 => bail!("401 Unauthorized — the bearer token was rejected: {detail}"),
        403 => bail!("403 Forbidden — this app cannot access the endpoint: {detail}"),
        404 => bail!("404 Not Found — {detail}"),
        429 => bail!("429 Too Many Requests — rate limit or credit cap reached: {detail}"),
        _ => bail!("HTTP {status} — {detail}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_success_statuses() {
        assert!(check_status(200, "{}").is_ok());
        assert!(check_status(299, "").is_ok());
    }

    #[test]
    fn builds_the_user_lookup_url() {
        assert_eq!(
            user_lookup_url("XDevelopers"),
            "https://api.x.com/2/users/by/username/XDevelopers"
        );
    }

    #[test]
    fn builds_the_timeline_url_with_every_expansion() {
        // Spelled out on one line on purpose: the implementation splits the
        // query across `\` line continuations, and repeating that trick here
        // would hide exactly the stray-whitespace bug this guards against.
        assert_eq!(
            timeline_url("2244994945", 20, None),
            "https://api.x.com/2/users/2244994945/tweets?max_results=20&tweet.fields=created_at&expansions=author_id&user.fields=name,username"
        );
    }

    #[test]
    fn appends_since_id_when_given() {
        // #9: an incremental reload passes the newest cached post id so the
        // API returns only what's new, keeping both the response and the
        // credit cost down.
        assert_eq!(
            timeline_url("2244994945", 20, Some("1700000000000000001")),
            "https://api.x.com/2/users/2244994945/tweets?max_results=20&tweet.fields=created_at&expansions=author_id&user.fields=name,username&since_id=1700000000000000001"
        );
    }

    #[test]
    fn explains_an_exhausted_credit_cap() {
        let body =
            r#"{"title":"UsageCapExceeded","detail":"Usage cap exceeded: Monthly product cap"}"#;
        let error = check_status(429, body).unwrap_err().to_string();
        assert!(error.contains("429"), "{error}");
        assert!(error.contains("Usage cap exceeded"), "{error}");
    }

    #[test]
    fn explains_a_rejected_token() {
        let error = check_status(401, r#"{"title":"Unauthorized"}"#)
            .unwrap_err()
            .to_string();
        assert!(error.contains("bearer token was rejected"), "{error}");
    }

    #[test]
    fn falls_back_to_the_raw_body_when_it_is_not_json() {
        let error = check_status(503, "upstream unavailable")
            .unwrap_err()
            .to_string();
        assert!(error.contains("upstream unavailable"), "{error}");
    }

    #[test]
    fn reports_an_empty_body_rather_than_nothing() {
        let error = check_status(500, "").unwrap_err().to_string();
        assert!(error.contains("empty response body"), "{error}");
    }
}
