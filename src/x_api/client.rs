use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use ureq::Agent;

use super::model::{ApiProblem, TimelineItem, TimelineResponse, User, UserLookupResponse};
use crate::paths::Paths;
use crate::rate_limit::{self, Endpoint, RateLimitState};

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

    /// Perform one GET, enforcing #10's central rule first — never send
    /// while the tracked window for `endpoint` reports zero remaining and
    /// its reset time hasn't arrived (see [`rate_limit::decision`]) —
    /// retrying network errors and 5xx with backoff, and persisting
    /// whatever the response's rate-limit headers reveal before returning.
    /// Neither kind of 429 is ever retried here: an ordinary rate limit
    /// recovers on its own schedule (never sooner for having retried), and
    /// a usage-cap 429 never recovers at all.
    ///
    /// Not unit-tested directly — it touches the network and, via `paths`,
    /// the filesystem, the same way `cache::reload` isn't. The pure seams
    /// that carry this behavior's actual test coverage are
    /// `rate_limit::decision`, `rate_limit::backoff_delay`,
    /// `rate_limit::parse_headers`, and `rate_limit::classify_429` (via
    /// [`check_status`], below).
    fn get(&self, paths: &Paths, endpoint: Endpoint, url: &str, now: i64) -> Result<String> {
        let tracked = rate_limit::load(paths, endpoint)?;
        rate_limit::decision(tracked, now).map_err(anyhow::Error::from)?;

        let mut attempt = 0u32;
        loop {
            match self.send_once(url) {
                Ok((status, body, state)) => {
                    // Persisted even on a non-2xx response — an exhausted
                    // window's own 429 is exactly the information #10 needs
                    // tracked so the *next* call refuses to send at all.
                    rate_limit::save(paths, endpoint, state)?;

                    if is_retryable_status(status) && attempt < rate_limit::MAX_RETRIES {
                        attempt += 1;
                        std::thread::sleep(rate_limit::backoff_delay(
                            attempt,
                            rate_limit::random_jitter_fraction(),
                        ));
                        continue;
                    }

                    check_status(status, &body, state.reset_at)?;
                    return Ok(body);
                }
                Err(error) => {
                    if attempt < rate_limit::MAX_RETRIES {
                        attempt += 1;
                        std::thread::sleep(rate_limit::backoff_delay(
                            attempt,
                            rate_limit::random_jitter_fraction(),
                        ));
                        continue;
                    }
                    return Err(error);
                }
            }
        }
    }

    /// One raw HTTP GET: send the request, read the body, and parse
    /// whatever `x-rate-limit-*` headers came back via
    /// [`rate_limit::parse_headers`]. Split out from [`Self::get`] so the
    /// retry loop there can call it more than once.
    fn send_once(&self, url: &str) -> Result<(u16, String, RateLimitState)> {
        let mut response = self
            .agent
            .get(url)
            .header("Authorization", format!("Bearer {}", self.bearer_token))
            .call()
            .with_context(|| format!("request to {url} failed"))?;

        // A closure rather than a free function so it borrows `response`
        // without needing to name ureq's response type: the borrow ends
        // once the last call below returns, freeing `response` for the
        // `body_mut()` call that follows.
        let header = |name: &str| -> Option<String> {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        };
        let state = rate_limit::parse_headers(
            header("x-rate-limit-limit").as_deref(),
            header("x-rate-limit-remaining").as_deref(),
            header("x-rate-limit-reset").as_deref(),
        );

        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .context("could not read the response body")?;
        Ok((status, body, state))
    }

    /// Resolve a screen name to the numeric user id the timeline endpoint needs.
    pub(crate) fn user_id_by_username(
        &self,
        paths: &Paths,
        username: &str,
        now: i64,
    ) -> Result<String> {
        let url = user_lookup_url(username);
        let body = self.get(paths, Endpoint::UserLookup, &url, now)?;

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
        paths: &Paths,
        user_id: &str,
        max_results: u32,
        since_id: Option<&str>,
        now: i64,
    ) -> Result<Vec<TimelineItem>> {
        let url = timeline_url(user_id, max_results, since_id);
        let body = self.get(paths, Endpoint::Timeline, &url, now)?;

        let response: TimelineResponse =
            serde_json::from_str(&body).context("could not parse the timeline response")?;
        Ok(response.into_items())
    }

    /// Resolve the signed-in user's own id and screen name via
    /// `GET /2/users/me` (#11). Requires an OAuth user-context token — an
    /// app-only bearer token gets a 401 here, the same way it does on the
    /// home timeline itself.
    pub(crate) fn me(&self, paths: &Paths, now: i64) -> Result<User> {
        let url = me_url();
        let body = self.get(paths, Endpoint::Me, &url, now)?;

        let response: UserLookupResponse =
            serde_json::from_str(&body).context("could not parse the /me response")?;
        match response.data {
            Some(user) => Ok(user),
            None => match describe_problem(&body) {
                Some(message) => bail!("could not resolve the signed-in user: {message}"),
                None => bail!("could not resolve the signed-in user: the API returned no user"),
            },
        }
    }

    /// Fetch a page of the signed-in user's home timeline (#11), newest
    /// first, alongside `meta.next_token` for #11's "Load older". `since_id`
    /// (an incremental reload) and `pagination_token` (resuming from a prior
    /// `next_token`) are mutually exclusive in practice — see
    /// [`home_timeline_url`] — but the caller decides which one it needs;
    /// this just passes both through.
    pub(crate) fn home_timeline(
        &self,
        paths: &Paths,
        user_id: &str,
        max_results: u32,
        since_id: Option<&str>,
        pagination_token: Option<&str>,
        now: i64,
    ) -> Result<(Vec<TimelineItem>, Option<String>)> {
        let url = home_timeline_url(user_id, max_results, since_id, pagination_token);
        let body = self.get(paths, Endpoint::HomeTimeline, &url, now)?;

        let response: TimelineResponse =
            serde_json::from_str(&body).context("could not parse the home timeline response")?;
        let next_token = response.next_token().map(str::to_string);
        Ok((response.into_items(), next_token))
    }

    /// `GET /2/tweets?ids=` (#12) for a single id — used one level at a time
    /// by the parent-chain walk in `cache::fetch_thread`, since each level's
    /// id is only known once the previous one resolves. Returns whatever
    /// [`TimelineResponse::into_items`] produces for the (at most one) post
    /// the API hands back: empty when the id is missing from `data`
    /// entirely (deleted, protected, or otherwise absent), which the caller
    /// treats as the walk stopping cleanly rather than an error.
    pub(crate) fn tweets_by_id(
        &self,
        paths: &Paths,
        id: &str,
        now: i64,
    ) -> Result<Vec<TimelineItem>> {
        let url = tweets_by_id_url(id);
        let body = self.get(paths, Endpoint::TweetById, &url, now)?;

        let response: TimelineResponse =
            serde_json::from_str(&body).context("could not parse the tweets-by-id response")?;
        Ok(response.into_items())
    }
}

/// Whether a status is worth retrying: server-side (5xx) failures only.
/// Network errors are retried too, but those never reach here as a status
/// code — they short-circuit `Self::get`'s `Err` arm instead. Never true
/// for 429 (not in `500..600`): #10's whole point is that neither kind of
/// 429 is a retry candidate.
fn is_retryable_status(status: u16) -> bool {
    (500..600).contains(&status)
}

fn user_lookup_url(username: &str) -> String {
    format!("{API_BASE}/users/by/username/{username}")
}

/// `GET /2/users/me` (#11) — resolves the signed-in user's own id and screen
/// name. Only meaningful with an OAuth user-context credential; an app-only
/// bearer token gets a 401 here just like the home timeline itself.
fn me_url() -> String {
    format!("{API_BASE}/users/me")
}

/// The home timeline endpoint (#11), with the same expansions as
/// [`timeline_url`] since it returns the same post shape. `since_id` (an
/// incremental reload) and `pagination_token` (#11's "Load older") are
/// mutually exclusive in practice — a reload always starts from the newest
/// cached post, and "Load older" always resumes from the last response's
/// `meta.next_token` — but both are accepted here independently so the
/// pure URL-building logic doesn't need to know which caller it's serving.
fn home_timeline_url(
    user_id: &str,
    max_results: u32,
    since_id: Option<&str>,
    pagination_token: Option<&str>,
) -> String {
    let mut url = format!(
        "{API_BASE}/users/{user_id}/timelines/reverse_chronological\
         ?max_results={max_results}\
         &tweet.fields=created_at,referenced_tweets\
         &expansions=author_id,referenced_tweets.id,referenced_tweets.id.author_id\
         &user.fields=name,username"
    );
    if let Some(id) = since_id {
        url = format!("{url}&since_id={id}");
    }
    if let Some(token) = pagination_token {
        url = format!("{url}&pagination_token={token}");
    }
    url
}

/// The timeline endpoint returns bare post ids unless `expansions` and the
/// `*.fields` parameters ask for more, so the query string is load-bearing.
fn timeline_url(user_id: &str, max_results: u32, since_id: Option<&str>) -> String {
    let base = format!(
        "{API_BASE}/users/{user_id}/tweets\
         ?max_results={max_results}\
         &tweet.fields=created_at,referenced_tweets\
         &expansions=author_id,referenced_tweets.id,referenced_tweets.id.author_id\
         &user.fields=name,username"
    );
    match since_id {
        Some(id) => format!("{base}&since_id={id}"),
        None => base,
    }
}

/// `GET /2/tweets?ids=` (#12), with the same expansions as the timeline
/// endpoints so a fetched parent's own author (and, if it is itself a
/// reply, its own parent's id) comes back in the same response — one id at
/// a time, since the parent-chain walk only learns the next id after this
/// one resolves.
fn tweets_by_id_url(id: &str) -> String {
    // STUB (TDD red phase): missing expansions.
    format!("{API_BASE}/tweets?ids={id}")
}

/// Pull the API's own error text out of a response body, if it has any.
fn describe_problem(body: &str) -> Option<String> {
    serde_json::from_str::<ApiProblem>(body)
        .ok()
        .and_then(|problem| problem.message())
}

/// Validate a response status, translating a non-2xx into an error.
///
/// `reset_at`, when known, is the endpoint's own `x-rate-limit-reset` from
/// this same response — used to fill in [`rate_limit::RateLimited`]'s reset
/// time on an ordinary 429. 401/403/404/other statuses keep the plain-text
/// `anyhow` errors this crate already used before #10; only 429 changed, per
/// #10's design: the two kinds of 429 become distinct types
/// ([`rate_limit::UsageCapExceeded`], [`rate_limit::RateLimited`]) via
/// [`rate_limit::classify_429`], rather than a string comparison here.
fn check_status(status: u16, body: &str, reset_at: Option<i64>) -> Result<()> {
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
        429 => match rate_limit::classify_429(body) {
            rate_limit::RateLimitKind::UsageCapExceeded => {
                Err(rate_limit::UsageCapExceeded { detail }.into())
            }
            rate_limit::RateLimitKind::RateLimited => {
                Err(rate_limit::RateLimited { reset_at }.into())
            }
        },
        _ => bail!("HTTP {status} — {detail}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_success_statuses() {
        assert!(check_status(200, "{}", None).is_ok());
        assert!(check_status(299, "", None).is_ok());
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
        // #13 adds `referenced_tweets` so a repost's/quote's real content
        // and author come back in `includes` without a second request.
        assert_eq!(
            timeline_url("2244994945", 20, None),
            "https://api.x.com/2/users/2244994945/tweets?max_results=20&tweet.fields=created_at,referenced_tweets&expansions=author_id,referenced_tweets.id,referenced_tweets.id.author_id&user.fields=name,username"
        );
    }

    #[test]
    fn builds_the_tweets_by_id_url_with_every_expansion() {
        // #12: the parent-chain walk fetches one id per level, with the
        // same expansions as the timeline endpoints so a fetched parent's
        // own author — and, if it's itself a reply, its own parent's id —
        // comes back in the same response.
        assert_eq!(
            tweets_by_id_url("1700000000000000001"),
            "https://api.x.com/2/tweets?ids=1700000000000000001&tweet.fields=created_at,referenced_tweets&expansions=author_id,referenced_tweets.id,referenced_tweets.id.author_id&user.fields=name,username"
        );
    }

    #[test]
    fn builds_the_me_url() {
        assert_eq!(me_url(), "https://api.x.com/2/users/me");
    }

    #[test]
    fn builds_the_home_timeline_url_with_every_expansion() {
        assert_eq!(
            home_timeline_url("2244994945", 20, None, None),
            "https://api.x.com/2/users/2244994945/timelines/reverse_chronological?max_results=20&tweet.fields=created_at,referenced_tweets&expansions=author_id,referenced_tweets.id,referenced_tweets.id.author_id&user.fields=name,username"
        );
    }

    #[test]
    fn home_timeline_url_appends_since_id_for_an_incremental_reload() {
        assert_eq!(
            home_timeline_url("2244994945", 20, Some("1700000000000000001"), None),
            "https://api.x.com/2/users/2244994945/timelines/reverse_chronological?max_results=20&tweet.fields=created_at,referenced_tweets&expansions=author_id,referenced_tweets.id,referenced_tweets.id.author_id&user.fields=name,username&since_id=1700000000000000001"
        );
    }

    #[test]
    fn home_timeline_url_appends_pagination_token_for_load_older() {
        // #11: "Load older" resends `meta.next_token` from the previous
        // response as `pagination_token`.
        assert_eq!(
            home_timeline_url("2244994945", 20, None, Some("cursor-abc")),
            "https://api.x.com/2/users/2244994945/timelines/reverse_chronological?max_results=20&tweet.fields=created_at,referenced_tweets&expansions=author_id,referenced_tweets.id,referenced_tweets.id.author_id&user.fields=name,username&pagination_token=cursor-abc"
        );
    }

    #[test]
    fn appends_since_id_when_given() {
        // #9: an incremental reload passes the newest cached post id so the
        // API returns only what's new, keeping both the response and the
        // credit cost down.
        assert_eq!(
            timeline_url("2244994945", 20, Some("1700000000000000001")),
            "https://api.x.com/2/users/2244994945/tweets?max_results=20&tweet.fields=created_at,referenced_tweets&expansions=author_id,referenced_tweets.id,referenced_tweets.id.author_id&user.fields=name,username&since_id=1700000000000000001"
        );
    }

    #[test]
    fn explains_an_exhausted_credit_cap() {
        let body =
            r#"{"title":"UsageCapExceeded","detail":"Usage cap exceeded: Monthly product cap"}"#;
        let error = check_status(429, body, None).unwrap_err().to_string();
        assert!(error.contains("429"), "{error}");
        assert!(error.contains("Usage cap exceeded"), "{error}");
    }

    #[test]
    fn a_usage_cap_429_downcasts_to_the_typed_error() {
        // #10: the distinction must be a type, not a string comparison at
        // the call site — this is what lets `ui.rs` (or any other caller)
        // match on it instead of grepping the message.
        let body =
            r#"{"title":"UsageCapExceeded","detail":"Usage cap exceeded: Monthly product cap"}"#;
        let error = check_status(429, body, Some(1_700_000_000)).unwrap_err();
        let typed = error
            .downcast_ref::<rate_limit::UsageCapExceeded>()
            .unwrap();
        assert!(typed.detail.contains("Usage cap exceeded"), "{typed:?}");
    }

    #[test]
    fn an_ordinary_rate_limit_429_downcasts_to_the_typed_error_carrying_the_reset_time() {
        let body = r#"{"title":"TooManyRequests","detail":"Rate limit exceeded"}"#;
        let error = check_status(429, body, Some(1_700_000_000)).unwrap_err();
        let typed = error.downcast_ref::<rate_limit::RateLimited>().unwrap();
        assert_eq!(typed.reset_at, Some(1_700_000_000));
    }

    #[test]
    fn an_ordinary_rate_limit_429_with_no_reset_header_still_downcasts_cleanly() {
        let body = r#"{"title":"TooManyRequests","detail":"Rate limit exceeded"}"#;
        let error = check_status(429, body, None).unwrap_err();
        let typed = error.downcast_ref::<rate_limit::RateLimited>().unwrap();
        assert_eq!(typed.reset_at, None);
    }

    #[test]
    fn explains_a_rejected_token() {
        let error = check_status(401, r#"{"title":"Unauthorized"}"#, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("bearer token was rejected"), "{error}");
    }

    #[test]
    fn falls_back_to_the_raw_body_when_it_is_not_json() {
        let error = check_status(503, "upstream unavailable", None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("upstream unavailable"), "{error}");
    }

    #[test]
    fn reports_an_empty_body_rather_than_nothing() {
        let error = check_status(500, "", None).unwrap_err().to_string();
        assert!(error.contains("empty response body"), "{error}");
    }

    #[test]
    fn treats_5xx_as_retryable_and_4xx_as_not() {
        assert!(is_retryable_status(500));
        assert!(is_retryable_status(599));
        assert!(!is_retryable_status(429));
        assert!(!is_retryable_status(404));
        assert!(!is_retryable_status(200));
    }
}
