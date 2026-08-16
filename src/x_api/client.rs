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
pub struct XClient {
    agent: Agent,
    bearer_token: String,
}

impl XClient {
    pub fn new(bearer_token: String) -> Self {
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
    pub fn user_id_by_username(&self, username: &str) -> Result<String> {
        let url = format!("{API_BASE}/users/by/username/{username}");
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

    /// Fetch recent posts authored by `username`, newest first.
    pub fn user_timeline(&self, username: &str, max_results: u32) -> Result<Vec<TimelineItem>> {
        let user_id = self.user_id_by_username(username)?;
        let url = format!(
            "{API_BASE}/users/{user_id}/tweets\
             ?max_results={max_results}\
             &tweet.fields=created_at\
             &expansions=author_id\
             &user.fields=name,username"
        );
        let (status, body) = self.get(&url)?;
        check_status(status, &body)?;

        let response: TimelineResponse =
            serde_json::from_str(&body).context("could not parse the timeline response")?;
        Ok(response.into_items())
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
    }

    #[test]
    fn explains_an_exhausted_credit_cap() {
        let body = r#"{"title":"UsageCapExceeded","detail":"Usage cap exceeded: Monthly product cap"}"#;
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
