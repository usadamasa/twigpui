use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use ureq::Agent;

use super::model::{
    ApiProblem, Draft, TimelineItem, TimelineResponse, TweetIdRequest, User, UserLookupResponse,
};
use crate::paths::Paths;
use crate::rate_limit::{self, Endpoint, RateLimitState};
use crate::usage;

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
    /// #18: every actual HTTP send is counted via [`usage::record_request`],
    /// right before [`Self::send_once`] — including retries. X bills each
    /// send it receives regardless of the outcome (a 5xx still reached the
    /// server), so a retried request is counted once per attempt, not once
    /// per logical call. A request refused before ever sending by
    /// [`rate_limit::decision`] above is not counted, since nothing went
    /// out. This is the one place in the whole crate `usage.rs` is written
    /// from, by design: nothing can spend a request without going through
    /// this method first.
    ///
    /// Not unit-tested directly — it touches the network and, via `paths`,
    /// the filesystem, the same way `cache::reload` isn't. The pure seams
    /// that carry this behavior's actual test coverage are
    /// `rate_limit::decision`, `rate_limit::backoff_delay`,
    /// `rate_limit::parse_headers`, `rate_limit::classify_429` (via
    /// [`check_status`], below), and `usage::record`.
    fn get(&self, paths: &Paths, endpoint: Endpoint, url: &str, now: i64) -> Result<String> {
        Self::send_with_retry(paths, endpoint, now, || self.send_once(url))
    }

    /// Perform one `POST /2/tweets` (#14, `quote_tweet_id` added by #16),
    /// sharing every rate-limit and retry rule [`Self::get`] already follows
    /// — see [`Self::send_with_retry`], which the two now share so #10's
    /// central rule stays in exactly one place regardless of HTTP method.
    fn post(
        &self,
        paths: &Paths,
        endpoint: Endpoint,
        url: &str,
        draft: Draft<'_>,
        now: i64,
    ) -> Result<String> {
        Self::send_with_retry(paths, endpoint, now, || self.send_post_once(url, draft))
    }

    /// Perform one DELETE (#15's un-repost), sharing every rate-limit/retry
    /// rule [`Self::get`]/[`Self::post`] already follow via
    /// [`Self::send_with_retry`] — #10's central rule applies identically
    /// regardless of HTTP method, and DELETE gains it here rather than a
    /// parallel retry loop being written just for un-reposting.
    fn delete(&self, paths: &Paths, endpoint: Endpoint, url: &str, now: i64) -> Result<String> {
        Self::send_with_retry(paths, endpoint, now, || self.send_delete_once(url))
    }

    /// The retry/persist loop shared by [`Self::get`] and [`Self::post`]:
    /// enforce #10's rate-limit decision before sending anything, then
    /// retry a network error or 5xx with backoff (never either kind of
    /// 429 — see the doc on [`is_retryable_status`]), persisting whatever
    /// the tracked window looks like after every attempt whether or not it
    /// succeeded.
    ///
    /// Not unit-tested directly for the same reason `get`/`post` aren't —
    /// it touches the network and, via `paths`, the filesystem. The pure
    /// seams that carry this behavior's actual test coverage are
    /// `rate_limit::decision`, `rate_limit::backoff_delay`,
    /// `rate_limit::parse_headers`, and `rate_limit::classify_429` (via
    /// [`check_status`]).
    fn send_with_retry(
        paths: &Paths,
        endpoint: Endpoint,
        now: i64,
        send_once: impl Fn() -> Result<(u16, String, RateLimitState)>,
    ) -> Result<String> {
        let tracked = rate_limit::load(paths, endpoint)?;
        rate_limit::decision(tracked, now).map_err(anyhow::Error::from)?;

        let mut attempt = 0u32;
        loop {
            match send_once() {
                Ok((status, body, state)) => {
                    // Counted here rather than before the send, because a
                    // response coming back is the only evidence available
                    // that X actually processed (and so billed) the request.
                    // Counting up front would charge the user for every
                    // connection that never arrived — and a flaky network
                    // retries up to `MAX_RETRIES` times, so one reload could
                    // invent five requests that were never made. Counted per
                    // send, not per call, since a retried request is billed
                    // again. The remaining inaccuracy is the opposite case: a
                    // request X processed whose response was lost in transit
                    // is billed but not counted here.
                    usage::record_request(paths, endpoint, now)?;

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

    /// One raw HTTP POST for `POST /2/tweets` (#14, `quote_tweet_id` added
    /// by #16), mirroring [`Self::send_once`]'s shape so
    /// [`Self::send_with_retry`] can treat the two identically. `send_json`
    /// (the `ureq` `json` feature, already a dependency) both serializes
    /// [`super::model::PostTweetRequest`] and sets `Content-Type: application/json`.
    fn send_post_once(&self, url: &str, draft: Draft<'_>) -> Result<(u16, String, RateLimitState)> {
        let mut response = self
            .agent
            .post(url)
            .header("Authorization", format!("Bearer {}", self.bearer_token))
            .send_json(draft.to_request())
            .with_context(|| format!("request to {url} failed"))?;

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

    /// One raw HTTP DELETE (#15), mirroring [`Self::send_once`]'s shape
    /// exactly so [`Self::send_with_retry`] can treat it identically — no
    /// request body, unlike [`Self::send_post_once`]/[`Self::send_tweet_id_once`].
    fn send_delete_once(&self, url: &str) -> Result<(u16, String, RateLimitState)> {
        let mut response = self
            .agent
            .delete(url)
            .header("Authorization", format!("Bearer {}", self.bearer_token))
            .call()
            .with_context(|| format!("request to {url} failed"))?;

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

    /// One raw HTTP POST for the two endpoints whose body is a single
    /// `tweet_id` — `POST /2/users/:id/retweets` (#15) and
    /// `POST /2/users/:id/likes` (#68) — mirroring [`Self::send_post_once`]'s
    /// shape but serializing [`TweetIdRequest`] instead of
    /// [`super::model::PostTweetRequest`]. Kept separate from `send_post_once` rather than
    /// parameterizing it over the body type: the duplication here is a
    /// handful of lines, not the retry/rate-limit logic that actually needs
    /// sharing (that lives in [`Self::send_with_retry`], used identically by
    /// all three).
    fn send_tweet_id_once(
        &self,
        url: &str,
        tweet_id: &str,
    ) -> Result<(u16, String, RateLimitState)> {
        let mut response = self
            .agent
            .post(url)
            .header("Authorization", format!("Bearer {}", self.bearer_token))
            .send_json(TweetIdRequest { tweet_id })
            .with_context(|| format!("request to {url} failed"))?;

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

    /// `GET /2/tweets?ids=` (#12). `ids` is whatever the caller already
    /// joined with commas — X's own query parameter accepts a
    /// comma-separated list natively, so there is nothing here to loop
    /// over. Two callers rely on that: `cache::fetch_thread`'s parent-chain
    /// walk passes exactly one id at a time (each level's id is only known
    /// once the previous one resolves), while `main::fetch_post` (#42)
    /// joins every id from `--fetch-post` into a single call, so looking up
    /// five posts still costs exactly one request. Returns whatever
    /// [`TimelineResponse::into_items`] produces for whichever of the
    /// requested posts the API hands back: fewer entries than ids requested
    /// (down to zero) when some are missing from `data` entirely (deleted,
    /// protected, or otherwise absent) — the parent-chain walk treats that
    /// as stopping cleanly rather than an error, and `--fetch-post` reports
    /// the shortfall on stderr rather than failing outright.
    pub(crate) fn tweets_by_id(
        &self,
        paths: &Paths,
        ids: &str,
        now: i64,
    ) -> Result<Vec<TimelineItem>> {
        let url = tweets_by_id_url(ids);
        let body = self.get(paths, Endpoint::TweetById, &url, now)?;

        let response: TimelineResponse =
            serde_json::from_str(&body).context("could not parse the tweets-by-id response")?;
        Ok(response.into_items())
    }

    /// `POST /2/tweets` (#14, `quote_tweet_id` added by #16) — submit the
    /// composer's draft as a new post, optionally quoting `quote_tweet_id`.
    /// Tracked under its own `Endpoint::CreatePost` (#10): X limits posting
    /// separately from every read endpoint above, so sharing a bucket with
    /// any of them would corrupt both — and #16 deliberately reuses this
    /// same endpoint/tracking rather than adding a new `Endpoint` variant,
    /// since X has no separate quote endpoint to track independently.
    /// Returns nothing on success — `ui.rs` falls into a normal reload
    /// afterward (subject to #10's own interval, like any other reload)
    /// rather than this call handing back the created post's own fields,
    /// which nothing here currently needs.
    pub(crate) fn create_post(&self, paths: &Paths, draft: Draft<'_>, now: i64) -> Result<()> {
        let url = create_post_url();
        self.post(paths, Endpoint::CreatePost, &url, draft, now)?;
        Ok(())
    }

    /// `POST /2/users/:id/retweets` (#15) — repost `source_tweet_id` as
    /// `user_id` (the signed-in account's own id, from `/me` — #11).
    /// Tracked under its own `Endpoint::CreateRepost` (#10): X limits
    /// creating a repost separately from every other endpoint, so sharing a
    /// bucket with any of them would corrupt the tracked state for both.
    /// Returns nothing on success — `repost::create` decides what to
    /// persist from whether this call succeeded or, on a recognized
    /// conflict, from `repost::reconcile_from_error`.
    pub(crate) fn create_repost(
        &self,
        paths: &Paths,
        user_id: &str,
        source_tweet_id: &str,
        now: i64,
    ) -> Result<()> {
        let url = create_repost_url(user_id);
        Self::send_with_retry(paths, Endpoint::CreateRepost, now, || {
            self.send_tweet_id_once(&url, source_tweet_id)
        })?;
        Ok(())
    }

    /// `DELETE /2/users/:id/retweets/:source_tweet_id` (#15) — undo a
    /// repost. Tracked under its own `Endpoint::DeleteRepost` (#10),
    /// independent of `CreateRepost`: X limits create and delete
    /// separately, and #18's usage tracking needs independent counts for
    /// the same reason.
    pub(crate) fn delete_repost(
        &self,
        paths: &Paths,
        user_id: &str,
        source_tweet_id: &str,
        now: i64,
    ) -> Result<()> {
        let url = delete_repost_url(user_id, source_tweet_id);
        self.delete(paths, Endpoint::DeleteRepost, &url, now)?;
        Ok(())
    }

    /// `POST /2/users/:id/likes` (#68) — like a post as `user_id`. Tracked
    /// under its own `Endpoint::CreateLike` (#10, #18) for the same reason
    /// every other write endpoint is: X limits each on its own schedule,
    /// and a like is billed exactly like a read, so it has to be counted.
    ///
    /// Requires the `like.write` scope; a session without it gets a 403,
    /// which `ui.rs` heads off before spending the request.
    pub(crate) fn create_like(
        &self,
        paths: &Paths,
        user_id: &str,
        tweet_id: &str,
        now: i64,
    ) -> Result<()> {
        let url = create_like_url(user_id);
        Self::send_with_retry(paths, Endpoint::CreateLike, now, || {
            self.send_tweet_id_once(&url, tweet_id)
        })?;
        Ok(())
    }

    /// `DELETE /2/tweets/:id` (#72) — delete one's own post.
    ///
    /// Irreversible, so `ui.rs` only calls this behind an explicit
    /// confirmation. Nothing here enforces that the post is the signed-in
    /// account's: X rejects deleting someone else's, and `offers_delete`
    /// already withholds the affordance client-side rather than spending a
    /// guaranteed-failing request.
    pub(crate) fn delete_post(&self, paths: &Paths, post_id: &str, now: i64) -> Result<()> {
        let url = delete_post_url(post_id);
        self.delete(paths, Endpoint::DeletePost, &url, now)?;
        Ok(())
    }

    /// `DELETE /2/users/:id/likes/:tweet_id` (#68) — unlike. See
    /// [`Self::create_like`]; tracked independently under
    /// `Endpoint::DeleteLike`.
    pub(crate) fn delete_like(
        &self,
        paths: &Paths,
        user_id: &str,
        tweet_id: &str,
        now: i64,
    ) -> Result<()> {
        let url = delete_like_url(user_id, tweet_id);
        self.delete(paths, Endpoint::DeleteLike, &url, now)?;
        Ok(())
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
         &tweet.fields=created_at,entities,public_metrics,referenced_tweets\
         &expansions=author_id,referenced_tweets.id,referenced_tweets.id.author_id\
         &user.fields=name,profile_image_url,username"
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
         &tweet.fields=created_at,entities,public_metrics,referenced_tweets\
         &expansions=author_id,referenced_tweets.id,referenced_tweets.id.author_id\
         &user.fields=name,profile_image_url,username"
    );
    match since_id {
        Some(id) => format!("{base}&since_id={id}"),
        None => base,
    }
}

/// `GET /2/tweets?ids=` (#12), with the same expansions as the timeline
/// endpoints so a fetched post's own author (and, if it is itself a reply,
/// its own parent's id) comes back in the same response. `ids` is inserted
/// verbatim — a single id for the parent-chain walk (which only learns the
/// next id after this one resolves, so it never has more than one in hand
/// at a time), or a comma-separated list for `--fetch-post` (#42), since
/// X's own query parameter already accepts either without this crate doing
/// any joining or looping of its own.
///
/// Unlike the timeline builders above, this one does not ask for
/// `public_metrics` (#67) or `entities` (#70): a walked parent renders as a
/// [`crate::thread::ThreadItem`], which shows neither counts nor links —
/// and `--fetch-post`'s JSON output is [`TimelineItem`] as-is, which came
/// from the very same response shape, so there is no second URL builder to
/// give those fields to it instead.
fn tweets_by_id_url(ids: &str) -> String {
    format!(
        "{API_BASE}/tweets\
         ?ids={ids}\
         &tweet.fields=created_at,referenced_tweets\
         &expansions=author_id,referenced_tweets.id,referenced_tweets.id.author_id\
         &user.fields=name,profile_image_url,username"
    )
}

/// `POST /2/tweets` (#14) — no query string, unlike every `GET` above.
fn create_post_url() -> String {
    format!("{API_BASE}/tweets")
}

/// `POST /2/users/:id/retweets` (#15) — `user_id` is the signed-in
/// account's own id (`/me`, #11); the target post's id travels in the JSON
/// body ([`TweetIdRequest`]), not the URL.
fn create_repost_url(user_id: &str) -> String {
    format!("{API_BASE}/users/{user_id}/retweets")
}

/// `DELETE /2/users/:id/retweets/:source_tweet_id` (#15) — the only
/// endpoint in this crate where the *acted-on* resource's own id is a URL
/// path segment rather than a query parameter or JSON body field.
fn delete_repost_url(user_id: &str, source_tweet_id: &str) -> String {
    format!("{API_BASE}/users/{user_id}/retweets/{source_tweet_id}")
}

/// `DELETE /2/tweets/:id` (#72) — unlike every other write endpoint here,
/// this one names no user: X infers the account from the credential and
/// rejects a post that is not its own.
fn delete_post_url(post_id: &str) -> String {
    format!("{API_BASE}/tweets/{post_id}")
}

/// `POST /2/users/:id/likes` (#68) — `user_id` is the signed-in account's
/// own id (`/me`, #11); the target post's id travels in the JSON body
/// ([`TweetIdRequest`]), not the URL, exactly as for a repost.
fn create_like_url(user_id: &str) -> String {
    format!("{API_BASE}/users/{user_id}/likes")
}

/// `DELETE /2/users/:id/likes/:tweet_id` (#68) — the acted-on post's id is
/// a URL path segment here, mirroring [`delete_repost_url`].
fn delete_like_url(user_id: &str, tweet_id: &str) -> String {
    format!("{API_BASE}/users/{user_id}/likes/{tweet_id}")
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
            "https://api.x.com/2/users/2244994945/tweets?max_results=20&tweet.fields=created_at,entities,public_metrics,referenced_tweets&expansions=author_id,referenced_tweets.id,referenced_tweets.id.author_id&user.fields=name,profile_image_url,username"
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
            "https://api.x.com/2/tweets?ids=1700000000000000001&tweet.fields=created_at,referenced_tweets&expansions=author_id,referenced_tweets.id,referenced_tweets.id.author_id&user.fields=name,profile_image_url,username"
        );
    }

    #[test]
    fn builds_the_tweets_by_id_url_with_a_comma_joined_id_list() {
        // #42: `--fetch-post` joins every requested id with commas before
        // calling `tweets_by_id`, relying on `ids` landing in the query
        // string verbatim rather than this builder looping over it — X's
        // own `ids=` parameter already accepts a comma-separated list, so
        // three ids still cost exactly one request.
        assert_eq!(
            tweets_by_id_url("1,2,3"),
            "https://api.x.com/2/tweets?ids=1,2,3&tweet.fields=created_at,referenced_tweets&expansions=author_id,referenced_tweets.id,referenced_tweets.id.author_id&user.fields=name,profile_image_url,username"
        );
    }

    #[test]
    fn builds_the_create_post_url_with_no_query_string() {
        // #14: unlike every GET above, POST /2/tweets takes no query
        // parameters — the post text travels in the JSON body instead.
        assert_eq!(create_post_url(), "https://api.x.com/2/tweets");
    }

    #[test]
    fn builds_the_create_repost_url() {
        assert_eq!(
            create_repost_url("2244994945"),
            "https://api.x.com/2/users/2244994945/retweets"
        );
    }

    #[test]
    fn builds_the_delete_repost_url_with_the_source_tweet_id_as_a_path_segment() {
        assert_eq!(
            delete_repost_url("2244994945", "1700000000000000001"),
            "https://api.x.com/2/users/2244994945/retweets/1700000000000000001"
        );
    }

    #[test]
    fn builds_the_delete_post_url() {
        assert_eq!(
            delete_post_url("1700000000000000001"),
            "https://api.x.com/2/tweets/1700000000000000001"
        );
    }

    #[test]
    fn builds_the_create_like_url() {
        assert_eq!(
            create_like_url("2244994945"),
            "https://api.x.com/2/users/2244994945/likes"
        );
    }

    #[test]
    fn builds_the_delete_like_url_with_the_tweet_id_as_a_path_segment() {
        assert_eq!(
            delete_like_url("2244994945", "1700000000000000001"),
            "https://api.x.com/2/users/2244994945/likes/1700000000000000001"
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
            "https://api.x.com/2/users/2244994945/timelines/reverse_chronological?max_results=20&tweet.fields=created_at,entities,public_metrics,referenced_tweets&expansions=author_id,referenced_tweets.id,referenced_tweets.id.author_id&user.fields=name,profile_image_url,username"
        );
    }

    #[test]
    fn home_timeline_url_appends_since_id_for_an_incremental_reload() {
        assert_eq!(
            home_timeline_url("2244994945", 20, Some("1700000000000000001"), None),
            "https://api.x.com/2/users/2244994945/timelines/reverse_chronological?max_results=20&tweet.fields=created_at,entities,public_metrics,referenced_tweets&expansions=author_id,referenced_tweets.id,referenced_tweets.id.author_id&user.fields=name,profile_image_url,username&since_id=1700000000000000001"
        );
    }

    #[test]
    fn home_timeline_url_appends_pagination_token_for_load_older() {
        // #11: "Load older" resends `meta.next_token` from the previous
        // response as `pagination_token`.
        assert_eq!(
            home_timeline_url("2244994945", 20, None, Some("cursor-abc")),
            "https://api.x.com/2/users/2244994945/timelines/reverse_chronological?max_results=20&tweet.fields=created_at,entities,public_metrics,referenced_tweets&expansions=author_id,referenced_tweets.id,referenced_tweets.id.author_id&user.fields=name,profile_image_url,username&pagination_token=cursor-abc"
        );
    }

    #[test]
    fn appends_since_id_when_given() {
        // #9: an incremental reload passes the newest cached post id so the
        // API returns only what's new, keeping both the response and the
        // credit cost down.
        assert_eq!(
            timeline_url("2244994945", 20, Some("1700000000000000001")),
            "https://api.x.com/2/users/2244994945/tweets?max_results=20&tweet.fields=created_at,entities,public_metrics,referenced_tweets&expansions=author_id,referenced_tweets.id,referenced_tweets.id.author_id&user.fields=name,profile_image_url,username&since_id=1700000000000000001"
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
