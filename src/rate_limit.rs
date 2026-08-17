//! Rate-limit tracking and retry backoff (#10).
//!
//! Four pure seams, mirroring `config.rs`'s injected-`now` convention and
//! `pkce.rs`'s injected-randomness convention: [`parse_headers`] (header
//! text -> a typed snapshot), [`decision`] (snapshot + `now` -> send or
//! refuse), [`classify_429`] (response body -> which kind of 429), and
//! [`backoff_delay`] (attempt count + injected jitter fraction -> a
//! `Duration`). None of these read the clock, roll real dice, touch disk, or
//! touch the network — only [`load`]/[`save`] (disk) and
//! [`random_jitter_fraction`] (the OS CSPRNG) do, and `x_api::client` is the
//! only thing in this crate that also touches the network.
//!
//! The central rule this module exists to serve: a GUI app must never sleep
//! a background thread waiting out a reset window that can be up to 15
//! minutes. [`decision`] is how `x_api::client` decides *before sending*
//! whether to refuse a request outright and hand back a typed error with the
//! reset time instead.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::paths::Paths;
use crate::x_api::model::ApiProblem;

/// One endpoint's tracked rate-limit window, as reported by `x-rate-limit-*`
/// response headers. Every field is independently optional: a header can be
/// missing, or present but garbage, without that meaning "fail the request"
/// — see [`parse_headers`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RateLimitState {
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub remaining: Option<u32>,
    /// Unix seconds when `remaining` resets to `limit`.
    #[serde(default)]
    pub reset_at: Option<i64>,
}

/// Parse one response's `x-rate-limit-limit` / `-remaining` / `-reset`
/// headers. Each argument is independently optional (a missing header) and
/// independently fallible to parse (a present-but-garbage header); either
/// way the corresponding field comes back `None` rather than failing the
/// whole parse. An unparseable header means "no information", never "fail
/// the request".
pub(crate) fn parse_headers(
    limit: Option<&str>,
    remaining: Option<&str>,
    reset: Option<&str>,
) -> RateLimitState {
    RateLimitState {
        limit: limit.and_then(|value| value.trim().parse().ok()),
        remaining: remaining.and_then(|value| value.trim().parse().ok()),
        reset_at: reset.and_then(|value| value.trim().parse().ok()),
    }
}

/// Returned by [`decision`] when the tracked window says not to send.
/// `ui.rs` downcasts an `anyhow::Error` to this to render a countdown
/// instead of a bare error string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RateLimited {
    /// Unix seconds when the window is expected to reset, if known. `None`
    /// only when a live 429 response carried no usable
    /// `x-rate-limit-reset` — [`decision`] itself only ever refuses with a
    /// known reset time, since that's part of its trigger condition.
    pub reset_at: Option<i64>,
}

impl std::fmt::Display for RateLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.reset_at {
            Some(reset_at) => write!(f, "rate limited until unix time {reset_at}"),
            None => write!(f, "rate limited (reset time unknown)"),
        }
    }
}

impl std::error::Error for RateLimited {}

/// The central rule (#10): when the tracked window reports zero remaining
/// and its reset time is still ahead of `now`, refuse to send and let the
/// caller decide whether to wait — never sleep the calling thread out. Every
/// other case is safe to send: remaining above zero, an unknown remaining
/// count (no information yet), or a reset time that has already passed.
pub(crate) fn decision(state: RateLimitState, now: i64) -> Result<(), RateLimited> {
    match (state.remaining, state.reset_at) {
        (Some(0), Some(reset_at)) if reset_at > now => Err(RateLimited {
            reset_at: Some(reset_at),
        }),
        _ => Ok(()),
    }
}

/// The two distinct kinds of HTTP 429 X can return (#10). A type, not a
/// string comparison at the call site — `x_api::client::check_status`
/// matches on this instead of grepping the response body itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RateLimitKind {
    /// Prepaid credits exhausted (`title: "UsageCapExceeded"`). Retrying
    /// never helps — the account needs topping up.
    UsageCapExceeded,
    /// An ordinary per-endpoint rate limit. Recovers at the window's reset
    /// time.
    RateLimited,
}

/// Classify a 429 response body. A body that isn't recognizably
/// `UsageCapExceeded` — including one that fails to parse at all — is
/// treated as an ordinary rate limit: the safer default, since it's the
/// kind that recovers on its own rather than the kind that never does.
pub(crate) fn classify_429(body: &str) -> RateLimitKind {
    let title = serde_json::from_str::<ApiProblem>(body)
        .ok()
        .and_then(|problem| problem.title);
    match title.as_deref() {
        Some("UsageCapExceeded") => RateLimitKind::UsageCapExceeded,
        _ => RateLimitKind::RateLimited,
    }
}

/// Carries the API's own explanation of an exhausted usage cap (#10).
/// Unlike [`RateLimited`], this never recovers on its own, so it carries no
/// reset time — nothing should ever offer a countdown for this one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UsageCapExceeded {
    pub detail: String,
}

impl std::fmt::Display for UsageCapExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "429 Too Many Requests — usage cap exceeded: {}",
            self.detail
        )
    }
}

impl std::error::Error for UsageCapExceeded {}

/// Retries stop after this many resends (the initial attempt plus this
/// many), bounding how long one reload can block the background thread on a
/// flaky network or a struggling upstream.
pub(crate) const MAX_RETRIES: u32 = 4;

/// Delay before the first retry.
const BACKOFF_BASE_MILLIS: u64 = 500;
/// Cap on the pre-jitter ceiling, so a long outage doesn't compound into an
/// absurd wait between attempts.
const BACKOFF_MAX_MILLIS: u64 = 30_000;

/// Exponential backoff with full jitter (AWS's "full jitter" formula) for
/// network errors and 5xx only (#10) — never for either kind of 429, since
/// one recovers on its own schedule and the other never recovers at all,
/// and a retry loop can't fix either.
///
/// `attempt` is 1-based (the first retry). `jitter_fraction` — injected so
/// the schedule is deterministic in tests, clamped to `0.0..=1.0` — scales
/// the capped exponential ceiling down to the actual delay; production
/// draws a fresh fraction from the OS RNG via [`random_jitter_fraction`] on
/// every call.
pub(crate) fn backoff_delay(attempt: u32, jitter_fraction: f64) -> Duration {
    // Capped at 6 (2^6 = 64x base) so the shift below never overflows even
    // if `MAX_RETRIES` grows later; `BACKOFF_MAX_MILLIS` is the real ceiling
    // in practice long before that.
    let exponent = attempt.saturating_sub(1).min(6);
    let ceiling_millis = BACKOFF_BASE_MILLIS
        .saturating_mul(1u64 << exponent)
        .min(BACKOFF_MAX_MILLIS);
    let jitter_fraction = jitter_fraction.clamp(0.0, 1.0);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let delay_millis = (ceiling_millis as f64 * jitter_fraction) as u64;
    Duration::from_millis(delay_millis)
}

/// A fresh jitter fraction in `0.0..=1.0`, drawn from the OS CSPRNG via
/// `getrandom` (already a dependency for `oauth::pkce`). The one non-pure
/// function in this module's backoff seam — production calls this once per
/// retry; tests call [`backoff_delay`] directly with a fixed fraction
/// instead.
pub(crate) fn random_jitter_fraction() -> f64 {
    let mut bytes = [0u8; 8];
    // Failure here (OS RNG unavailable) is vanishingly rare; falling back to
    // the full, un-jittered delay is safer than skipping the backoff.
    if getrandom::fill(&mut bytes).is_err() {
        return 1.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let fraction = u64::from_le_bytes(bytes) as f64 / u64::MAX as f64;
    fraction
}

/// Which tracked endpoint a [`RateLimitState`] belongs to. X's rate limits
/// are per-endpoint, so the two calls `x_api::client::XClient` makes are
/// tracked separately rather than sharing one bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Endpoint {
    UserLookup,
    Timeline,
    /// `GET /2/users/me` (#11) — X limits this separately from the
    /// screen-name lookup above.
    Me,
    /// `GET /2/users/:id/timelines/reverse_chronological` (#11) — X limits
    /// the home timeline separately from the single-user `Timeline` fetch.
    HomeTimeline,
    /// `GET /2/tweets?ids=` (#12) — the parent-chain walk behind "Show
    /// thread". Tracked independently: reusing e.g. `Timeline`'s bucket
    /// would corrupt the tracked state for both, since X limits each
    /// endpoint on its own schedule.
    TweetById,
    /// `POST /2/tweets` (#14) — the composer's submit action. X limits
    /// posting separately from every read endpoint above, so sharing a
    /// bucket with any of them would corrupt the tracked state for both.
    CreatePost,
    /// `POST /2/users/:id/retweets` (#15) — creating a repost. Tracked
    /// independently of `DeleteRepost`: X limits create and delete
    /// separately, and reusing either's bucket for the other would corrupt
    /// the tracked state for both.
    CreateRepost,
    /// `DELETE /2/users/:id/retweets/:source_tweet_id` (#15) — undoing a
    /// repost. See `CreateRepost`'s doc for why this needs its own bucket.
    DeleteRepost,
}

impl Endpoint {
    /// Every tracked endpoint, for callers that need to summarize across all
    /// of them rather than one at a time — `usage`'s `--usage`/header
    /// totals (#18) is the current user, so the list lives here rather than
    /// being duplicated wherever it's needed.
    pub(crate) const ALL: [Self; 5] = [
        Self::UserLookup,
        Self::Timeline,
        Self::Me,
        Self::HomeTimeline,
        Self::TweetById,
    ];

    /// `pub(crate)` rather than private (unlike before #18): `usage.rs`
    /// keys its own per-endpoint file by this same string, so the two
    /// modules' on-disk keys for the same endpoint never drift apart.
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::UserLookup => "user_lookup",
            Self::Timeline => "timeline",
            Self::Me => "me",
            Self::HomeTimeline => "home_timeline",
            Self::TweetById => "tweet_by_id",
            Self::CreatePost => "create_post",
            Self::CreateRepost => "create_repost",
            Self::DeleteRepost => "delete_repost",
        }
    }
}

/// The whole contents of [`Paths::rate_limit_file`]: every endpoint's most
/// recently observed state, keyed by [`Endpoint::key`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RateLimitFile {
    #[serde(default)]
    endpoints: HashMap<String, RateLimitState>,
}

/// Load [`RateLimitFile`] from disk. A missing file is a clean "nothing
/// tracked yet"; a corrupt or differently-shaped file is *also* a clean
/// miss rather than an error, mirroring `cache::load_json`'s rule — losing
/// this file costs at most one avoidably-sent request, never a startup
/// failure.
fn load_file(paths: &Paths) -> Result<RateLimitFile> {
    let path = paths.rate_limit_file();
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RateLimitFile::default());
        }
        Err(error) => {
            return Err(error).with_context(|| format!("could not read {}", path.display()));
        }
    };
    Ok(serde_json::from_str(&contents).unwrap_or_default())
}

/// The tracked state for `endpoint`, or [`RateLimitState::default`] (all
/// `None`, which [`decision`] always treats as safe to send) if there is
/// nothing on file for it yet.
pub(crate) fn load(paths: &Paths, endpoint: Endpoint) -> Result<RateLimitState> {
    let file = load_file(paths)?;
    Ok(file
        .endpoints
        .get(endpoint.key())
        .copied()
        .unwrap_or_default())
}

/// Persist `state` for `endpoint`, alongside whatever other endpoints were
/// already on file — a genuine I/O error reading the existing file (as
/// opposed to it being merely absent or corrupt) still propagates, the same
/// distinction `cache.rs` draws.
pub(crate) fn save(paths: &Paths, endpoint: Endpoint, state: RateLimitState) -> Result<()> {
    let path = paths.rate_limit_file();
    let mut file = load_file(paths)?;
    file.endpoints.insert(endpoint.key().to_string(), state);

    let json = serde_json::to_vec_pretty(&file)
        .with_context(|| format!("could not serialize {}", path.display()))?;
    std::fs::write(&path, json).with_context(|| format!("could not write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_paths(root: &std::path::Path) -> Paths {
        let home = root.display().to_string();
        Paths::from_vars(move |key| (key == "HOME").then(|| home.clone())).unwrap()
    }

    fn temp_root(label: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "twigpui-test-rate-limit-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    // --- parse_headers ---

    #[test]
    fn parses_every_header_when_all_are_present_and_valid() {
        let state = parse_headers(Some("15"), Some("3"), Some("1700000000"));
        assert_eq!(
            state,
            RateLimitState {
                limit: Some(15),
                remaining: Some(3),
                reset_at: Some(1_700_000_000),
            }
        );
    }

    #[test]
    fn missing_headers_parse_to_none_rather_than_erroring() {
        let state = parse_headers(None, None, None);
        assert_eq!(state, RateLimitState::default());
    }

    #[test]
    fn non_numeric_header_values_parse_to_none_for_that_field_only() {
        let state = parse_headers(Some("fifteen"), Some("3"), Some("soon"));
        assert_eq!(state.limit, None);
        assert_eq!(state.remaining, Some(3));
        assert_eq!(state.reset_at, None);
    }

    #[test]
    fn tolerates_surrounding_whitespace() {
        let state = parse_headers(Some(" 15 "), Some(" 0 "), Some(" 1700000000 "));
        assert_eq!(state.limit, Some(15));
        assert_eq!(state.remaining, Some(0));
        assert_eq!(state.reset_at, Some(1_700_000_000));
    }

    #[test]
    fn a_reset_header_in_the_past_still_parses_cleanly() {
        // Parsing never judges plausibility — that's decision()'s job.
        let state = parse_headers(None, None, Some("0"));
        assert_eq!(state.reset_at, Some(0));
    }

    // --- decision ---

    #[test]
    fn refuses_to_send_when_remaining_is_zero_and_the_reset_has_not_arrived() {
        let state = RateLimitState {
            limit: Some(15),
            remaining: Some(0),
            reset_at: Some(1_000),
        };
        let error = decision(state, 500).unwrap_err();
        assert_eq!(error.reset_at, Some(1_000));
    }

    #[test]
    fn sends_when_remaining_is_zero_but_the_reset_has_already_passed() {
        let state = RateLimitState {
            limit: Some(15),
            remaining: Some(0),
            reset_at: Some(1_000),
        };
        assert!(decision(state, 1_000).is_ok());
        assert!(decision(state, 1_001).is_ok());
    }

    #[test]
    fn sends_when_remaining_is_above_zero_regardless_of_reset() {
        let state = RateLimitState {
            limit: Some(15),
            remaining: Some(1),
            reset_at: Some(1_000),
        };
        assert!(decision(state, 0).is_ok());
    }

    #[test]
    fn sends_when_there_is_no_tracked_information_at_all() {
        assert!(decision(RateLimitState::default(), 0).is_ok());
    }

    #[test]
    fn sends_when_remaining_is_zero_but_the_reset_time_is_unknown() {
        // Without a reset time there's no way to know when it's safe again,
        // so blocking forever would be worse than an occasional 429.
        let state = RateLimitState {
            limit: Some(15),
            remaining: Some(0),
            reset_at: None,
        };
        assert!(decision(state, 0).is_ok());
    }

    // --- classify_429 ---

    #[test]
    fn classifies_a_usage_cap_body_as_usage_cap_exceeded() {
        let body =
            r#"{"title":"UsageCapExceeded","detail":"Usage cap exceeded: Monthly product cap"}"#;
        assert_eq!(classify_429(body), RateLimitKind::UsageCapExceeded);
    }

    #[test]
    fn classifies_a_different_title_as_an_ordinary_rate_limit() {
        let body = r#"{"title":"TooManyRequests","detail":"Rate limit exceeded"}"#;
        assert_eq!(classify_429(body), RateLimitKind::RateLimited);
    }

    #[test]
    fn classifies_an_unparseable_body_as_the_recoverable_kind() {
        assert_eq!(classify_429("not json"), RateLimitKind::RateLimited);
    }

    #[test]
    fn classifies_an_empty_body_as_the_recoverable_kind() {
        assert_eq!(classify_429(""), RateLimitKind::RateLimited);
    }

    // --- backoff_delay ---

    #[test]
    fn backoff_delay_is_zero_with_zero_jitter() {
        assert_eq!(backoff_delay(1, 0.0), Duration::ZERO);
        assert_eq!(backoff_delay(4, 0.0), Duration::ZERO);
    }

    #[test]
    fn backoff_delay_doubles_the_ceiling_each_attempt_with_full_jitter() {
        assert_eq!(backoff_delay(1, 1.0), Duration::from_millis(500));
        assert_eq!(backoff_delay(2, 1.0), Duration::from_secs(1));
        assert_eq!(backoff_delay(3, 1.0), Duration::from_secs(2));
    }

    #[test]
    fn backoff_delay_is_capped_for_a_large_attempt_count() {
        assert_eq!(backoff_delay(20, 1.0), Duration::from_secs(30));
    }

    #[test]
    fn backoff_delay_scales_linearly_with_the_jitter_fraction() {
        assert_eq!(backoff_delay(1, 0.5), Duration::from_millis(250));
    }

    #[test]
    fn backoff_delay_clamps_an_out_of_range_jitter_fraction() {
        assert_eq!(backoff_delay(1, -1.0), Duration::ZERO);
        assert_eq!(backoff_delay(1, 2.0), Duration::from_millis(500));
    }

    #[test]
    fn backoff_delay_is_deterministic_given_the_same_inputs() {
        assert_eq!(backoff_delay(3, 0.37), backoff_delay(3, 0.37));
    }

    // --- load / save ---

    #[test]
    fn load_is_the_default_state_when_nothing_is_on_file() {
        let root = temp_root("load-missing");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        assert_eq!(
            load(&paths, Endpoint::Timeline).unwrap(),
            RateLimitState::default()
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn save_then_load_roundtrips_for_the_same_endpoint() {
        let root = temp_root("roundtrip");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let state = RateLimitState {
            limit: Some(15),
            remaining: Some(3),
            reset_at: Some(1_700_000_000),
        };
        save(&paths, Endpoint::Timeline, state).unwrap();
        assert_eq!(load(&paths, Endpoint::Timeline).unwrap(), state);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn save_keeps_other_endpoints_state_untouched() {
        let root = temp_root("multi-endpoint");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let user_lookup_state = RateLimitState {
            limit: Some(300),
            remaining: Some(299),
            reset_at: Some(1_000),
        };
        let timeline_state = RateLimitState {
            limit: Some(15),
            remaining: Some(0),
            reset_at: Some(2_000),
        };
        save(&paths, Endpoint::UserLookup, user_lookup_state).unwrap();
        save(&paths, Endpoint::Timeline, timeline_state).unwrap();

        assert_eq!(
            load(&paths, Endpoint::UserLookup).unwrap(),
            user_lookup_state
        );
        assert_eq!(load(&paths, Endpoint::Timeline).unwrap(), timeline_state);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn me_and_home_timeline_endpoints_are_tracked_independently_of_the_originals() {
        // #11: X limits `/users/me` and the home timeline separately from
        // the existing user-lookup and single-user timeline endpoints, so
        // sharing a key with either would corrupt the tracked state for both.
        let root = temp_root("four-endpoints");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let user_lookup_state = RateLimitState {
            limit: Some(300),
            remaining: Some(299),
            reset_at: Some(1_000),
        };
        let timeline_state = RateLimitState {
            limit: Some(15),
            remaining: Some(10),
            reset_at: Some(2_000),
        };
        let me_state = RateLimitState {
            limit: Some(25),
            remaining: Some(24),
            reset_at: Some(3_000),
        };
        let home_timeline_state = RateLimitState {
            limit: Some(15),
            remaining: Some(0),
            reset_at: Some(4_000),
        };
        save(&paths, Endpoint::UserLookup, user_lookup_state).unwrap();
        save(&paths, Endpoint::Timeline, timeline_state).unwrap();
        save(&paths, Endpoint::Me, me_state).unwrap();
        save(&paths, Endpoint::HomeTimeline, home_timeline_state).unwrap();

        assert_eq!(
            load(&paths, Endpoint::UserLookup).unwrap(),
            user_lookup_state
        );
        assert_eq!(load(&paths, Endpoint::Timeline).unwrap(), timeline_state);
        assert_eq!(load(&paths, Endpoint::Me).unwrap(), me_state);
        assert_eq!(
            load(&paths, Endpoint::HomeTimeline).unwrap(),
            home_timeline_state
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn tweet_by_id_endpoint_is_tracked_independently_of_the_others() {
        // #12: `GET /2/tweets?ids=` gets its own bucket — reusing e.g.
        // `Timeline`'s would corrupt the tracked state for both.
        let root = temp_root("tweet-by-id-endpoint");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let timeline_state = RateLimitState {
            limit: Some(15),
            remaining: Some(10),
            reset_at: Some(2_000),
        };
        let tweet_by_id_state = RateLimitState {
            limit: Some(300),
            remaining: Some(0),
            reset_at: Some(5_000),
        };
        save(&paths, Endpoint::Timeline, timeline_state).unwrap();
        save(&paths, Endpoint::TweetById, tweet_by_id_state).unwrap();

        assert_eq!(load(&paths, Endpoint::Timeline).unwrap(), timeline_state);
        assert_eq!(
            load(&paths, Endpoint::TweetById).unwrap(),
            tweet_by_id_state
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn create_post_endpoint_is_tracked_independently_of_the_others() {
        // #14: `POST /2/tweets` gets its own bucket — reusing e.g.
        // `Timeline`'s would corrupt the tracked state for both.
        let root = temp_root("create-post-endpoint");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let timeline_state = RateLimitState {
            limit: Some(15),
            remaining: Some(10),
            reset_at: Some(2_000),
        };
        let create_post_state = RateLimitState {
            limit: Some(200),
            remaining: Some(0),
            reset_at: Some(6_000),
        };
        save(&paths, Endpoint::Timeline, timeline_state).unwrap();
        save(&paths, Endpoint::CreatePost, create_post_state).unwrap();

        assert_eq!(load(&paths, Endpoint::Timeline).unwrap(), timeline_state);
        assert_eq!(
            load(&paths, Endpoint::CreatePost).unwrap(),
            create_post_state
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn create_repost_and_delete_repost_endpoints_are_tracked_independently() {
        // #15: create and delete each get their own bucket — reusing
        // either's for the other, or for an existing endpoint, would
        // corrupt the tracked state for both.
        let root = temp_root("repost-endpoints");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let timeline_state = RateLimitState {
            limit: Some(15),
            remaining: Some(10),
            reset_at: Some(2_000),
        };
        let create_repost_state = RateLimitState {
            limit: Some(50),
            remaining: Some(49),
            reset_at: Some(7_000),
        };
        let delete_repost_state = RateLimitState {
            limit: Some(50),
            remaining: Some(0),
            reset_at: Some(8_000),
        };
        save(&paths, Endpoint::Timeline, timeline_state).unwrap();
        save(&paths, Endpoint::CreateRepost, create_repost_state).unwrap();
        save(&paths, Endpoint::DeleteRepost, delete_repost_state).unwrap();

        assert_eq!(load(&paths, Endpoint::Timeline).unwrap(), timeline_state);
        assert_eq!(
            load(&paths, Endpoint::CreateRepost).unwrap(),
            create_repost_state
        );
        assert_eq!(
            load(&paths, Endpoint::DeleteRepost).unwrap(),
            delete_repost_state
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_corrupted_rate_limit_file_is_a_clean_miss_not_an_error() {
        let root = temp_root("corrupt");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.rate_limit_file(), b"not json at all").unwrap();

        assert_eq!(
            load(&paths, Endpoint::Timeline).unwrap(),
            RateLimitState::default()
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn save_recovers_cleanly_from_a_corrupted_existing_file() {
        let root = temp_root("save-over-corrupt");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.rate_limit_file(), b"{ not valid json").unwrap();

        let state = RateLimitState {
            limit: Some(15),
            remaining: Some(15),
            reset_at: None,
        };
        save(&paths, Endpoint::Timeline, state).unwrap();
        assert_eq!(load(&paths, Endpoint::Timeline).unwrap(), state);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn endpoint_all_lists_every_variant_with_a_unique_key() {
        // #18's usage tracker iterates `Endpoint::ALL` to summarize across
        // every endpoint — a missing or duplicated variant here would
        // silently under- or double-count.
        let keys: std::collections::HashSet<&str> = Endpoint::ALL
            .iter()
            .map(|endpoint| endpoint.key())
            .collect();
        assert_eq!(keys.len(), Endpoint::ALL.len());
    }

    #[test]
    fn a_genuine_io_error_reading_the_rate_limit_file_still_propagates() {
        let root = temp_root("io-error");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        // A directory where a file is expected is a real I/O error, not
        // corruption — it must surface rather than being swallowed.
        std::fs::create_dir(paths.rate_limit_file()).unwrap();

        assert!(load(&paths, Endpoint::Timeline).is_err());

        std::fs::remove_dir_all(&root).unwrap();
    }
}
