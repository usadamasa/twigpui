use anyhow::{Context as _, Result, bail};
use serde::Deserialize;
use std::path::Path;

use crate::log;
use crate::paths::Paths;
use crate::profile::Profile;
use crate::theme::ThemeMode;

/// Runtime configuration, resolved with environment variable > `config.toml`
/// > built-in default precedence.
#[derive(Debug, Clone)]
pub(crate) struct Config {
    /// OAuth 2.0 client id for the PKCE sign-in flow (#7). Non-secret — a
    /// public OAuth client has no client secret — so this may live in
    /// `config.toml`.
    ///
    /// Required since #33 dropped the app-only bearer token: it is the only
    /// way to authenticate now, so a missing one is a startup failure
    /// rather than one of two alternatives.
    pub oauth_client_id: String,
    /// Screen name whose posts are shown, without a leading `@`.
    pub target_username: String,
    /// Posts requested per fetch. The X API accepts 5..=100.
    pub max_results: u32,
    /// Floor on how often a fetch may run, in seconds (#10).
    ///
    /// Read by two things that both had to agree about it, which is why it
    /// was plumbed through before either existed: `ui::reload_policy::reload_gate`
    /// refuses a `Polling` reload inside this window, and #21's
    /// `auto_refresh_interval_seconds` is validated never to fall below it
    /// — a cadence under this floor would be a timer every tick of which
    /// is refused before it sends anything.
    pub min_fetch_interval_seconds: u32,
    /// Color theme (#19): `light`, `dark`, or `system` (follows the OS
    /// appearance). Defaults to `light`; an unrecognized value falls back to
    /// the default rather than failing startup — see [`Config::resolve`].
    pub theme: ThemeMode,
    /// How much detail reaches the log file (#49). Defaults to
    /// [`log::Level::Info`]; an unrecognized value falls back to it rather
    /// than failing startup, exactly like `theme`.
    pub log_level: log::Level,
    /// Price per API request (#18), in whatever unit the operator has in
    /// mind — this crate never assumes a currency. `None` by default: the
    /// per-request price depends on the account's plan and there is no way
    /// to know it from here, so no estimated amount is ever shown unless
    /// this is explicitly configured. See `usage.rs`'s module doc.
    pub request_price: Option<f64>,
    /// The list whose timeline fills the window (#161), or `None` to show
    /// the home timeline as every launch did before it.
    ///
    /// `GET /2/users/:id/timelines/reverse_chronological` stopped returning
    /// followed authors' posts for this account (#157) and no change here
    /// can fix that, so a list is how the app reads a following-shaped feed
    /// at all. Validated to be all ASCII digits by [`Config::resolve`]: the
    /// value is interpolated into a URL path segment.
    pub list_id: Option<String>,
    /// Daily request-count budget (#18): once today's total across every
    /// tracked endpoint approaches or reaches this, the header's usage line
    /// switches to a warning/danger color — see `usage::budget_status`.
    /// Deliberately a request count, not a monetary amount: unlike
    /// `request_price`, this always has a value to compare against (request
    /// counts are always known), so it works whether or not a price is
    /// configured.
    pub daily_request_budget: Option<u32>,
    /// Whether the window keeps `list_id`'s membership mirroring the
    /// accounts this app follows while it runs.
    ///
    /// On by default, and effective only once a `list_id` is configured and
    /// the session carries the scopes `sync::missing_scope` asks for — both
    /// of which are already deliberate acts. Turning it off is how someone
    /// keeps a configured list under their own hand instead.
    ///
    /// **This spends money on a timer**, which is why the interval below is
    /// long and why the README says so out loud.
    pub auto_sync_list: bool,
    /// How long the background sync waits between diffs, in seconds.
    ///
    /// Not `min_fetch_interval_seconds`: that one throttles a reload that
    /// costs a page of posts, this one paces a pair of full reads that cost
    /// one billed resource per followed account.
    pub sync_interval_seconds: u32,
    /// The most of the list's membership the background sync may remove
    /// in one plan, in percent (#176).
    ///
    /// A follow read that comes back short with a 200 reads as a mass
    /// unfollow, and the background sync prunes without asking. Over this
    /// share the removals are held in the plan file for
    /// `--sync-list --apply --prune` to confirm — see
    /// `sync::schedule::prune_allowed`. 0..=100: `100` turns the cap off,
    /// `0` makes the background sync additive only. The CLI is never
    /// capped.
    pub sync_prune_limit_percent: u8,
    /// How many writes one background-sync tick may send, at one tick a
    /// minute — the catch-up's sustained pace (#197).
    ///
    /// The default is deliberately slow: the cap that locked #197 out for
    /// 24 hours followed roughly seven writes a minute, and its size is
    /// still unmeasured. This knob exists for the measurement's other
    /// direction — run at the default for a while, see no refusal, raise
    /// it and watch the log. A refusal is not an accident either way:
    /// `sync::state`'s backoff ladder absorbs it and the log records what
    /// the cap said. 1..=[`MAX_SYNC_WRITES_PER_MINUTE`].
    pub sync_writes_per_minute: u8,
    /// Whether the window polls its timeline for new posts while it runs
    /// (#21).
    ///
    /// On by default. Off means the app sends nothing it was not clicked
    /// into sending — #21's completion condition names that outcome
    /// specifically, so it is a hard guarantee rather than a tendency: see
    /// `TimelineView::start_auto_refresh`, which returns before spawning
    /// anything at all when this is false.
    pub auto_refresh: bool,
    /// How long auto-refresh waits between polls, in seconds (#21).
    ///
    /// Distinct from `min_fetch_interval_seconds` the way
    /// `sync_interval_seconds` is: that one is a *floor* under any fetch,
    /// this one is the cadence a poll actually runs at, and it is validated
    /// never to fall below the floor — see [`resolve_auto_refresh_interval`].
    pub auto_refresh_interval_seconds: u32,
    /// Whether a poll's new posts flow onto the screen by themselves when
    /// the reader is already at the top (#22).
    ///
    /// On by default — the point of #177's experience is a timeline that
    /// keeps moving without being asked. Off, and every poll goes through
    /// the pill instead, whatever the scroll position. Purely
    /// presentational: this switch never changes what is fetched or when —
    /// that is `auto_refresh`'s job — only what the window does with a
    /// fetch that already happened. The seed for
    /// `TimelineView::follow_new_posts`, which the View menu toggles at
    /// runtime without writing back here.
    pub follow_new_posts: bool,
}

const DEFAULT_USERNAME: &str = "XDevelopers";
const DEFAULT_MAX_RESULTS: u32 = 20;
const MAX_RESULTS_RANGE: std::ops::RangeInclusive<u32> = 5..=100;
/// 60s: comfortably above the per-window cost of a single reload (one or
/// two requests) against even X's tighter per-endpoint rate-limit windows,
/// while still being responsive to a human clicking the reload button.
const DEFAULT_MIN_FETCH_INTERVAL_SECONDS: u32 = 60;

/// 6 hours between diffs.
///
/// Both sides of the diff bill per returned resource, so one diff of a
/// few thousand follows is dollars. X documents that resources are
/// deduplicated within a 24-hour UTC day, which would make every diff
/// after the day's first nearly free — but `x-api-budget` has that
/// measured for Posts only, not for Users or Owned Reads. This interval is
/// picked so the unverified half cannot cost much either way: four diffs a
/// day is roughly $2 per thousand follows if dedup holds and roughly $8 if
/// it does not.
const DEFAULT_SYNC_INTERVAL_SECONDS: u32 = 21_600;

/// The shortest interval the sync will accept.
///
/// A floor rather than a warning, because the failure it prevents is not
/// recoverable by noticing it later: `X_SYNC_INTERVAL_SECONDS=60` typed
/// where `6000` was meant would, if dedup turns out not to apply to Users,
/// buy both full reads sixty times an hour against a prepaid balance. 15
/// minutes is far below any cadence this feature has a use for and still
/// two orders of magnitude away from that.
const MIN_SYNC_INTERVAL_SECONDS: u32 = 900;

/// 10%: the background sync may delete a tenth of the list per plan (#176).
///
/// Conservative on purpose. A real mass unfollow is rare and has the CLI to
/// fall back on; a follow read that comes back short is the failure nobody
/// sees until the list is empty. Small lists feel it more — 1 of 15 is over
/// the line — and that is accepted rather than patched with an absolute
/// floor: a false hold costs one CLI command, a false pass costs the list.
const DEFAULT_SYNC_PRUNE_LIMIT_PERCENT: u8 = 10;

/// 2 writes a minute: the background sync's default catch-up pace (#197).
///
/// Chosen from the one measurement there is — a hidden cap on
/// `POST /2/lists/:id/members` engaged after roughly seven writes a minute
/// and stayed down for 24 hours — to be the pace that does not trip it,
/// not the fastest pace it allows. Raising it is what
/// `sync_writes_per_minute` is for, once a run at this default has shown
/// no refusals.
const DEFAULT_SYNC_WRITES_PER_MINUTE: u8 = 2;

/// The most `sync_writes_per_minute` accepts: 20/min is X's *documented*
/// write window (300 per 15 minutes) spread evenly. Above it the tracked
/// rate-limit window would start refusing sends anyway, so a larger value
/// only buys refusals — `25` typed where `2` was meant should be an error
/// with the key named, not a burst.
const MAX_SYNC_WRITES_PER_MINUTE: u8 = 20;

/// 5 minutes between auto-refresh polls (#21).
///
/// Chosen from what a poll actually bills rather than from how fresh a
/// timeline could theoretically be. A poll re-reads the head page —
/// `GET /2/lists/:id/tweets` takes no `since_id`, so there is no cheaper
/// request to send — and reads bill per returned resource, deduplicated
/// within a UTC day. So in steady state a day's polling costs the posts
/// that were genuinely new that day, which is what reading them costs
/// however they arrive; the only repeated charge is the head page once
/// after each UTC midnight, bounded by `max_results`.
///
/// That makes the interval a responsiveness knob rather than a spending
/// one, and five minutes is the point where a timeline feels live without
/// the window sending a request every time someone glances at it.
const DEFAULT_AUTO_REFRESH_INTERVAL_SECONDS: u32 = 300;

/// The file-level settings loaded from `config.toml`.
///
/// Every field is `Option` and `#[serde(default)]` applies, and the struct
/// deliberately does not use `deny_unknown_fields`: future issues (#19's
/// theme, #24's layout) add keys incrementally, and an older binary reading
/// a newer file must not choke on keys it doesn't know about yet.
#[derive(Debug, Default, Deserialize)]
struct FileSettings {
    #[serde(default)]
    target_username: Option<String>,
    #[serde(default)]
    max_results: Option<u32>,
    #[serde(default)]
    min_fetch_interval_seconds: Option<u32>,
    /// Non-secret (see [`Config::oauth_client_id`]), so — unlike
    /// this key is allowed in `config.toml`.
    #[serde(default)]
    oauth_client_id: Option<String>,
    /// Raw `theme` value (#19), parsed by [`Config::resolve`] rather than
    /// here so an unrecognized value can fall back to the default instead of
    /// failing the whole file load.
    #[serde(default)]
    theme: Option<String>,
    /// Raw `log_level` value (#49), parsed the same way and for the same
    /// reason as `theme`. This is the setting that matters for a `.app`
    /// launched from Finder, where no environment variable set in a shell
    /// is visible (#40).
    #[serde(default)]
    log_level: Option<String>,
    /// Non-secret (see [`Config::request_price`]'s doc), so this key is
    /// allowed in `config.toml` like `oauth_client_id` above.
    #[serde(default)]
    request_price: Option<f64>,
    /// Non-secret, same reasoning as `request_price`.
    #[serde(default)]
    daily_request_budget: Option<u32>,
    /// Non-secret, same reasoning as `request_price`. This is the key that
    /// matters for a `.app` launched from Finder, where no shell variable
    /// is visible (#40) — the same reason `log_level` belongs here.
    #[serde(default)]
    auto_sync_list: Option<bool>,
    /// Non-secret, same reasoning as `request_price`.
    #[serde(default)]
    sync_interval_seconds: Option<u32>,
    /// Non-secret, same reasoning as `request_price`. `u32` rather than
    /// `u8` so a `300` in the file is refused by `resolve` with the key
    /// named, not by serde with a type error.
    #[serde(default)]
    sync_prune_limit_percent: Option<u32>,
    /// Non-secret, same reasoning as `request_price`. `u32` for
    /// `sync_prune_limit_percent`'s reason.
    #[serde(default)]
    sync_writes_per_minute: Option<u32>,
    /// Non-secret, same reasoning as `request_price`. Like `auto_sync_list`
    /// above, this is the key that matters for a `.app` launched from
    /// Finder, where no shell variable is visible (#40) — and it is the one
    /// switch that makes the window stop sending anything on its own.
    #[serde(default)]
    auto_refresh: Option<bool>,
    /// Non-secret, same reasoning as `request_price`.
    #[serde(default)]
    auto_refresh_interval_seconds: Option<u32>,
    /// Non-secret, same reasoning as `request_price`.
    #[serde(default)]
    follow_new_posts: Option<bool>,
    /// Raw `list_id` value (#161). Non-secret — a list id is visible in the
    /// list's own URL on x.com — so it belongs in `config.toml` like every
    /// key above. Validated by [`Config::resolve`] rather than here, so the
    /// error can name whichever of the two sources it came from.
    #[serde(default)]
    list_id: Option<String>,
    /// Present only so [`Config::resolve`] can reject a file that still
    /// carries one. It was a credential that must never sit in a
    /// dotfiles-repo file; since #33 it is not a credential at all, and
    /// silently ignoring it would leave someone believing they are
    /// configured when they are not. Kept as an untyped `toml::Value` so
    /// any shape under this key still triggers the check instead of failing
    /// with a deserialize error.
    #[serde(default)]
    bearer_token: Option<toml::Value>,
}

impl FileSettings {
    /// Load settings from `path`. A missing file is not an error — it just
    /// means there are no file-level settings yet. A malformed file is an
    /// error whose message names `path`.
    fn load(path: &Path) -> Result<Self> {
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(error).with_context(|| format!("could not read {}", path.display()));
            }
        };
        toml::from_str(&contents).with_context(|| format!("could not parse {}", path.display()))
    }
}

impl Config {
    pub(crate) fn from_env() -> Result<Self> {
        // A missing .env is fine — the variables may come from the real environment.
        let _ = dotenvy::dotenv();
        let paths = Paths::from_env()?;
        // This is the real startup path, and the only place the one-time
        // Time Machine exclusion is worth its ~1s subprocess.
        if paths.ensure_dirs()? {
            paths.exclude_cache_from_backups();
        }
        let file = FileSettings::load(&paths.settings_file())?;
        Self::resolve(|key| std::env::var(key).ok(), file)
    }

    /// Parse and validate the settings from an arbitrary variable lookup and
    /// already-loaded file settings.
    ///
    /// Split out from [`Config::from_env`] so the rules below can be tested
    /// without `set_var`, which is `unsafe` and races the other test threads.
    ///
    /// Resolves against the profile this binary was compiled as, which is
    /// what every caller outside the tests wants. The tests that care about
    /// a specific profile's defaults (#169) use
    /// [`Config::resolve_for_profile`] instead.
    fn resolve(var: impl Fn(&str) -> Option<String>, file: FileSettings) -> Result<Self> {
        Self::resolve_for_profile(var, file, Profile::current())
    }

    /// [`Config::resolve`] against an arbitrary profile (#169), mirroring
    /// the seam [`Paths::for_profile`] uses for the same reason: a default
    /// that differs per profile can only be pinned by naming one.
    fn resolve_for_profile(
        var: impl Fn(&str) -> Option<String>,
        file: FileSettings,
        profile: Profile,
    ) -> Result<Self> {
        // #33 removed the app-only bearer token. Someone upgrading still has
        // the key in their file, and ignoring it would leave them believing
        // they are configured when nothing reads it — so say what happened
        // and what to do instead.
        if file.bearer_token.is_some() {
            bail!(
                "bearer_token is no longer supported (#33): app-only access could not read \
                 the home timeline or write anything, so twigpui now signs in with X. \
                 Remove the key from config.toml and set oauth_client_id (or \
                 X_OAUTH_CLIENT_ID) instead."
            );
        }

        let oauth_client_id = var("X_OAUTH_CLIENT_ID")
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
            .or_else(|| {
                file.oauth_client_id
                    .map(|c| c.trim().to_string())
                    .filter(|c| !c.is_empty())
            });

        // The only credential there is since #33, so a missing one is a
        // startup failure rather than one of two alternatives.
        let Some(oauth_client_id) = oauth_client_id else {
            bail!(
                "no oauth_client_id is configured. Set X_OAUTH_CLIENT_ID, or add \
                 oauth_client_id = \"…\" to config.toml, then click \"Sign in with X\"."
            );
        };

        let target_username = var("X_TARGET_USERNAME")
            .filter(|u| !u.trim().is_empty())
            .or_else(|| file.target_username.filter(|u| !u.trim().is_empty()))
            .unwrap_or_else(|| DEFAULT_USERNAME.to_string());
        let target_username = target_username.trim().trim_start_matches('@').to_string();

        let (max_results, max_results_source) = match var("X_MAX_RESULTS") {
            Some(raw) => {
                let value = raw
                    .trim()
                    .parse::<u32>()
                    .with_context(|| format!("X_MAX_RESULTS is not a number: {raw:?}"))?;
                (value, "X_MAX_RESULTS")
            }
            None => match file.max_results {
                Some(value) => (value, "max_results in config.toml"),
                None => (DEFAULT_MAX_RESULTS, "the default"),
            },
        };
        if !MAX_RESULTS_RANGE.contains(&max_results) {
            bail!(
                "{max_results_source} must be between {} and {}, got {max_results}",
                MAX_RESULTS_RANGE.start(),
                MAX_RESULTS_RANGE.end()
            );
        }

        let min_fetch_interval_seconds = match var("X_MIN_FETCH_INTERVAL_SECONDS") {
            Some(raw) => raw.trim().parse::<u32>().with_context(|| {
                format!("X_MIN_FETCH_INTERVAL_SECONDS is not a number: {raw:?}")
            })?,
            None => file
                .min_fetch_interval_seconds
                .unwrap_or(DEFAULT_MIN_FETCH_INTERVAL_SECONDS),
        };
        if min_fetch_interval_seconds == 0 {
            bail!(
                "X_MIN_FETCH_INTERVAL_SECONDS (or min_fetch_interval_seconds in config.toml) \
                 must be greater than 0"
            );
        }

        let theme = resolve_theme(&var, file.theme);

        let log_level = resolve_log_level(&var, file.log_level);

        let list_id = resolve_list_id(&var, file.list_id, profile)?;

        let request_price = resolve_request_price(&var, file.request_price)?;
        let daily_request_budget = resolve_daily_request_budget(&var, file.daily_request_budget)?;

        let auto_sync_list = resolve_switch("X_AUTO_SYNC_LIST", &var, file.auto_sync_list)?;
        let sync_interval_seconds = resolve_sync_interval(&var, file.sync_interval_seconds)?;
        let sync_prune_limit_percent =
            resolve_sync_prune_limit(&var, file.sync_prune_limit_percent)?;
        let sync_writes_per_minute =
            resolve_sync_writes_per_minute(&var, file.sync_writes_per_minute)?;

        let auto_refresh = resolve_switch("X_AUTO_REFRESH", &var, file.auto_refresh)?;
        let follow_new_posts = resolve_switch("X_FOLLOW_NEW_POSTS", &var, file.follow_new_posts)?;
        // Takes `min_fetch_interval_seconds` because the floor it enforces
        // is that one — see [`resolve_auto_refresh_interval`].
        let auto_refresh_interval_seconds = resolve_auto_refresh_interval(
            &var,
            file.auto_refresh_interval_seconds,
            min_fetch_interval_seconds,
        )?;

        Ok(Self {
            oauth_client_id,
            target_username,
            max_results,
            min_fetch_interval_seconds,
            theme,
            log_level,
            request_price,
            list_id,
            daily_request_budget,
            auto_sync_list,
            sync_interval_seconds,
            sync_prune_limit_percent,
            sync_writes_per_minute,
            auto_refresh,
            auto_refresh_interval_seconds,
            follow_new_posts,
        })
    }
}

/// Resolve `list_id` (#161): env > file > the profile's own default (#169),
/// the same layering as everything else, with a blank value on either side
/// treated as unset — an `X_LIST_ID=` left behind in a shell should mean
/// "fall through", not a request to `/2/lists//tweets`.
///
/// The profile default is where a development build picks up its throwaway
/// list without anything being configured; the release profile has none, so
/// there it still resolves to "no list, read the home timeline". See
/// [`Profile::default_list_id`].
///
/// A non-empty value that is not all ASCII digits is a startup failure
/// rather than a warn-and-ignore (unlike `theme` and `log_level`): those
/// two are cosmetic, whereas this one decides which timeline is fetched,
/// and silently falling back to the home timeline would leave someone
/// believing they are reading their list when they are reading the feed
/// #157 found empty. The error names whichever source the value came from,
/// so it points at the thing to edit.
fn resolve_list_id(
    var: impl Fn(&str) -> Option<String>,
    file_value: Option<String>,
    profile: Profile,
) -> Result<Option<String>> {
    let (raw, source) = match var("X_LIST_ID")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        Some(value) => (value, "X_LIST_ID"),
        None => match file_value
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            Some(value) => (value, "list_id in config.toml"),
            // Not run through the digit check below: it is a literal in
            // this crate, pinned by `profile.rs`'s own tests, not
            // something a user typed.
            None => return Ok(profile.default_list_id().map(str::to_string)),
        },
    };

    if !raw.chars().all(|c| c.is_ascii_digit()) {
        bail!("{source} must be a numeric list id, got {raw:?}");
    }
    Ok(Some(raw))
}

/// Resolve one of the on-by-default boolean switches: env > file > on.
///
/// Rejects an unrecognized value rather than falling back the way `theme`
/// does. A typo'd theme is cosmetic; a typo'd `X_AUTO_SYNC_LIST=flase` read
/// as the default would leave a paid background loop running for someone
/// who was trying to switch it off. `X_FOLLOW_NEW_POSTS` costs nothing
/// either way, but reading `flase` as "on" would still silently ignore
/// what the person wrote.
fn resolve_switch(
    key: &str,
    var: impl Fn(&str) -> Option<String>,
    file_value: Option<bool>,
) -> Result<bool> {
    let Some(raw) = var(key)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return Ok(file_value.unwrap_or(true));
    };
    match raw.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => bail!("{key} must be true or false, got {raw:?}"),
    }
}

/// Resolve `sync_interval_seconds`: env > file > [`DEFAULT_SYNC_INTERVAL_SECONDS`],
/// refusing anything under [`MIN_SYNC_INTERVAL_SECONDS`].
///
/// The floor's error names what the number buys rather than just the
/// bound, because the mistake it catches is a decimal point, and "must be
/// at least 900" does not tell someone why 60 was a bad idea.
fn resolve_sync_interval(
    var: impl Fn(&str) -> Option<String>,
    file_value: Option<u32>,
) -> Result<u32> {
    let (seconds, source) = match var("X_SYNC_INTERVAL_SECONDS")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        Some(raw) => (
            raw.parse::<u32>()
                .with_context(|| format!("X_SYNC_INTERVAL_SECONDS is not a number: {raw:?}"))?,
            "X_SYNC_INTERVAL_SECONDS",
        ),
        None => match file_value {
            Some(seconds) => (seconds, "sync_interval_seconds in config.toml"),
            None => return Ok(DEFAULT_SYNC_INTERVAL_SECONDS),
        },
    };

    if seconds < MIN_SYNC_INTERVAL_SECONDS {
        bail!(
            "{source} must be at least {MIN_SYNC_INTERVAL_SECONDS} seconds, got {seconds}. \
             Each sync reads the whole follow list and the whole list membership, and both \
             bill per account returned — this floor is what stops a mistyped interval from \
             buying them over and over."
        );
    }
    Ok(seconds)
}

/// Resolve `sync_prune_limit_percent` (#176): env > file >
/// [`DEFAULT_SYNC_PRUNE_LIMIT_PERCENT`], refusing anything over 100.
///
/// A ceiling rather than a clamp: `150` is not "off", it is a number that
/// was meant to be something else, and reading it as 100 would turn the
/// cap off for the one person who was trying to set it.
fn resolve_sync_prune_limit(
    var: impl Fn(&str) -> Option<String>,
    file_value: Option<u32>,
) -> Result<u8> {
    let (percent, source) = match var("X_SYNC_PRUNE_LIMIT_PERCENT")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        Some(raw) => (
            raw.parse::<u32>()
                .with_context(|| format!("X_SYNC_PRUNE_LIMIT_PERCENT is not a number: {raw:?}"))?,
            "X_SYNC_PRUNE_LIMIT_PERCENT",
        ),
        None => match file_value {
            Some(percent) => (percent, "sync_prune_limit_percent in config.toml"),
            None => return Ok(DEFAULT_SYNC_PRUNE_LIMIT_PERCENT),
        },
    };

    u8::try_from(percent)
        .ok()
        .filter(|percent| *percent <= 100)
        .with_context(|| {
            format!("{source} must be at most 100 (a share of the list, in percent), got {percent}")
        })
}

/// Resolve `sync_writes_per_minute` (#197): env > file >
/// [`DEFAULT_SYNC_WRITES_PER_MINUTE`], refusing 0 and anything over
/// [`MAX_SYNC_WRITES_PER_MINUTE`].
///
/// 0 is refused rather than read as "off" — `auto_sync_list` is the switch
/// for that, and a pace of zero would be a sync that claims to run while
/// never draining its plan. The ceiling is a ceiling for
/// [`MAX_SYNC_WRITES_PER_MINUTE`]'s reason: beyond it only the refusals
/// get faster.
fn resolve_sync_writes_per_minute(
    var: impl Fn(&str) -> Option<String>,
    file_value: Option<u32>,
) -> Result<u8> {
    let (writes, source) = match var("X_SYNC_WRITES_PER_MINUTE")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        Some(raw) => (
            raw.parse::<u32>()
                .with_context(|| format!("X_SYNC_WRITES_PER_MINUTE is not a number: {raw:?}"))?,
            "X_SYNC_WRITES_PER_MINUTE",
        ),
        None => match file_value {
            Some(writes) => (writes, "sync_writes_per_minute in config.toml"),
            None => return Ok(DEFAULT_SYNC_WRITES_PER_MINUTE),
        },
    };

    u8::try_from(writes)
        .ok()
        .filter(|writes| (1..=MAX_SYNC_WRITES_PER_MINUTE).contains(writes))
        .with_context(|| {
            format!(
                "{source} must be between 1 and {MAX_SYNC_WRITES_PER_MINUTE} (X's documented \
                 write window is 300 per 15 minutes), got {writes}"
            )
        })
}

/// Resolve `auto_refresh_interval_seconds` (#21): env > file >
/// [`DEFAULT_AUTO_REFRESH_INTERVAL_SECONDS`], refusing anything below
/// `min_fetch_interval_seconds`.
///
/// The floor is not about money — [`DEFAULT_AUTO_REFRESH_INTERVAL_SECONDS`]
/// explains why a poll is nearly free — but about the loop working at all.
/// Every poll goes through `ui::reload_policy::reload_gate` as
/// `ReloadTrigger::Polling`, which refuses a fetch within
/// `min_fetch_interval_seconds` of the last one. Set the cadence below that
/// floor and every single tick is refused before it sends anything: the
/// window would be running a timer that can never do its job, and nothing
/// on screen would say so. Rejecting it at startup is the only place that
/// mismatch is visible.
///
/// Equal to the floor is accepted — a poll scheduled exactly when the gate
/// reopens is the tightest cadence that still works.
fn resolve_auto_refresh_interval(
    var: &impl Fn(&str) -> Option<String>,
    file_value: Option<u32>,
    min_fetch_interval_seconds: u32,
) -> Result<u32> {
    let (seconds, source) = match var("X_AUTO_REFRESH_INTERVAL_SECONDS")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        Some(raw) => (
            raw.parse::<u32>().with_context(|| {
                format!("X_AUTO_REFRESH_INTERVAL_SECONDS is not a number: {raw:?}")
            })?,
            "X_AUTO_REFRESH_INTERVAL_SECONDS",
        ),
        None => match file_value {
            Some(seconds) => (seconds, "auto_refresh_interval_seconds in config.toml"),
            None => return Ok(DEFAULT_AUTO_REFRESH_INTERVAL_SECONDS),
        },
    };

    if seconds < min_fetch_interval_seconds {
        bail!(
            "{source} must be at least {min_fetch_interval_seconds} seconds — the value of \
             min_fetch_interval_seconds — got {seconds}. A poll scheduled inside that floor is \
             refused before it sends anything, so auto-refresh would never actually run."
        );
    }
    Ok(seconds)
}

/// Resolve `request_price` (#18): env > file > unset, the same precedence
/// every other setting in [`Config::resolve`] uses — split out from there
/// only to keep that function under clippy's line-count lint, not because
/// the logic itself is reused elsewhere.
///
/// Unlike every numeric setting `Config::resolve` handles inline, a
/// *missing* value here is the normal case, not something to default away
/// — see [`Config::request_price`]'s doc for why there is no built-in
/// default. Still validated when present, from either source: a negative
/// or non-finite price would silently corrupt every estimated amount
/// downstream.
/// Resolve `theme` (#19): env > file > default.
///
/// Unlike the numeric settings, an unrecognized value here must not
/// `bail!` — a typo'd theme is cosmetic, not a reason to block startup —
/// so it falls back to the default and warns via `eprintln!`, the
/// project's established pattern for non-fatal notices (see `main.rs`).
/// [`resolve_log_level`] below does the same thing for the same reason.
///
/// A free function rather than inline in [`Config::resolve_for_profile`]
/// only to keep that one under clippy's line-count lint, like every other
/// `resolve_*` here.
fn resolve_theme(var: &impl Fn(&str) -> Option<String>, file_value: Option<String>) -> ThemeMode {
    var("X_THEME")
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .or_else(|| {
            file_value
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
        })
        .and_then(|raw| {
            ThemeMode::parse(&raw).or_else(|| {
                eprintln!(
                    "warning: unrecognized theme {raw:?} (expected light, dark, or \
                     system); using {} instead",
                    ThemeMode::default()
                );
                None
            })
        })
        .unwrap_or_default()
}

/// Resolve the log level (#49): `TWIGPUI_LOG` wins over `config.toml`'s
/// `log_level`, and an unrecognized value warns and falls back to the
/// default rather than blocking startup — the same shape as `theme` (#19),
/// for the same reason: neither is worth refusing to run over.
///
/// The warning goes to stderr rather than the log, because this runs
/// *before* `log::init` — the level it produces is what `init` is waiting
/// for. In a `.app` launched from Finder nobody sees it, which is also the
/// case where a bad value is least likely to be a surprise: environment
/// variables set in a shell are not visible there, so it came from
/// `config.toml`, which the user just edited.
fn resolve_log_level(
    var: &impl Fn(&str) -> Option<String>,
    file_value: Option<String>,
) -> log::Level {
    var("TWIGPUI_LOG")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            file_value
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .and_then(|raw| {
            log::Level::parse(&raw).or_else(|| {
                eprintln!(
                    "warning: unrecognized log_level {raw:?} (expected error, warn, info, or debug); using info instead"
                );
                None
            })
        })
        .unwrap_or_default()
}

fn resolve_request_price(
    var: &impl Fn(&str) -> Option<String>,
    file_value: Option<f64>,
) -> Result<Option<f64>> {
    let (value, source) = match var("X_REQUEST_PRICE") {
        Some(raw) => {
            let value = raw
                .trim()
                .parse::<f64>()
                .with_context(|| format!("X_REQUEST_PRICE is not a number: {raw:?}"))?;
            (Some(value), "X_REQUEST_PRICE")
        }
        None => (file_value, "request_price in config.toml"),
    };
    if let Some(value) = value
        && (!value.is_finite() || value < 0.0)
    {
        bail!("{source} must be a non-negative number, got {value}");
    }
    Ok(value)
}

/// Resolve `daily_request_budget` (#18): env > file > unset. Split out for
/// the same reason as [`resolve_request_price`]. No validation beyond
/// parsing as `u32`: every value in that range (including zero) is
/// meaningful to `usage::budget_status`.
fn resolve_daily_request_budget(
    var: &impl Fn(&str) -> Option<String>,
    file_value: Option<u32>,
) -> Result<Option<u32>> {
    match var("X_DAILY_REQUEST_BUDGET") {
        Some(raw) => Ok(Some(raw.trim().parse::<u32>().with_context(|| {
            format!("X_DAILY_REQUEST_BUDGET is not a number: {raw:?}")
        })?)),
        None => Ok(file_value),
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, DEFAULT_MAX_RESULTS, DEFAULT_USERNAME, FileSettings};
    use crate::profile::Profile;
    use crate::theme::ThemeMode;

    /// Build a lookup over a fixed `(key, value)` table.
    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    #[test]
    fn fills_in_the_defaults_when_only_the_client_id_is_set() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings::default(),
        )
        .unwrap();

        assert_eq!(config.oauth_client_id, "client-123");
        assert_eq!(config.target_username, DEFAULT_USERNAME);
        assert_eq!(config.max_results, DEFAULT_MAX_RESULTS);
        assert_eq!(
            config.min_fetch_interval_seconds,
            super::DEFAULT_MIN_FETCH_INTERVAL_SECONDS
        );
        assert_eq!(config.theme, ThemeMode::default());
    }

    // #33 made the client id the only credential, so this is a hard
    // failure again — as it was before #7 introduced the second one.
    #[test]
    fn rejects_when_no_client_id_is_configured() {
        let error = Config::resolve(vars(&[]), FileSettings::default())
            .unwrap_err()
            .to_string();
        assert!(error.contains("X_OAUTH_CLIENT_ID"), "{error}");
        assert!(
            !error.contains("X_BEARER_TOKEN"),
            "the message must not point at a credential that no longer exists: {error}"
        );
    }

    // A blank token must still count as "not configured" rather than being
    // used verbatim — but with an oauth_client_id present, that no longer
    // --- #49: log level ---

    #[test]
    fn the_log_level_defaults_to_info() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.log_level, crate::log::Level::Info);
    }

    #[test]
    fn the_log_level_comes_from_the_environment_when_set() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("TWIGPUI_LOG", "debug"),
            ]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.log_level, crate::log::Level::Debug);
    }

    #[test]
    fn the_environments_log_level_wins_over_the_files() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("TWIGPUI_LOG", "error"),
            ]),
            FileSettings {
                log_level: Some("debug".to_string()),
                ..FileSettings::default()
            },
        )
        .unwrap();
        assert_eq!(config.log_level, crate::log::Level::Error);
    }

    #[test]
    fn the_files_log_level_is_used_when_the_environment_is_silent() {
        // The case that actually matters: a `.app` launched from Finder
        // sees no environment variable set in a shell (#40).
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings {
                log_level: Some("warn".to_string()),
                ..FileSettings::default()
            },
        )
        .unwrap();
        assert_eq!(config.log_level, crate::log::Level::Warn);
    }

    #[test]
    fn an_unrecognized_log_level_falls_back_rather_than_failing_startup() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123"), ("TWIGPUI_LOG", "loud")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.log_level, crate::log::Level::Info);
    }

    #[test]
    fn treats_a_blank_client_id_as_unset_rather_than_a_literal_value() {
        let error = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "   ")]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("X_OAUTH_CLIENT_ID"), "{error}");
    }

    #[test]
    fn trims_the_client_id() {
        // A value pasted into .env often carries a trailing newline, and it
        // goes into the authorize URL verbatim.
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "  client-123\n")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.oauth_client_id, "client-123");
    }

    #[test]
    fn trims_the_oauth_client_id() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "  client-123\n")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.oauth_client_id, "client-123");
    }

    #[test]
    fn resolve_reads_the_oauth_client_id_from_the_file_when_env_is_unset() {
        let file = FileSettings {
            oauth_client_id: Some("file-client".to_string()),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[]), file).unwrap();
        assert_eq!(config.oauth_client_id, "file-client");
    }

    #[test]
    fn resolve_prefers_the_env_oauth_client_id_over_the_file() {
        let file = FileSettings {
            oauth_client_id: Some("file-client".to_string()),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "env-client")]), file).unwrap();
        assert_eq!(config.oauth_client_id, "env-client");
    }

    #[test]
    fn strips_a_leading_at_from_the_username() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_TARGET_USERNAME", " @XDevelopers "),
            ]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.target_username, "XDevelopers");
    }

    #[test]
    fn falls_back_to_the_default_username_when_blank() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_TARGET_USERNAME", "  "),
            ]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.target_username, DEFAULT_USERNAME);
    }

    #[test]
    fn parses_max_results() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_MAX_RESULTS", " 42 "),
            ]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.max_results, 42);
    }

    #[test]
    fn rejects_a_non_numeric_max_results() {
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_MAX_RESULTS", "lots"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("not a number"), "{error}");
    }

    #[test]
    fn accepts_both_ends_of_the_api_range() {
        for raw in ["5", "100"] {
            let config = Config::resolve(
                vars(&[("X_OAUTH_CLIENT_ID", "client-123"), ("X_MAX_RESULTS", raw)]),
                FileSettings::default(),
            )
            .unwrap();
            assert_eq!(config.max_results.to_string(), raw);
        }
    }

    #[test]
    fn rejects_max_results_outside_the_api_range() {
        for raw in ["4", "101"] {
            let error = Config::resolve(
                vars(&[("X_OAUTH_CLIENT_ID", "client-123"), ("X_MAX_RESULTS", raw)]),
                FileSettings::default(),
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains("between 5 and 100"), "{raw}: {error}");
        }
    }

    // --- config.toml layering (env > file > default) ---

    #[test]
    fn resolve_reads_target_username_from_the_file_when_env_is_unset() {
        let file = FileSettings {
            target_username: Some("FileUser".to_string()),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file).unwrap();
        assert_eq!(config.target_username, "FileUser");
    }

    #[test]
    fn resolve_prefers_the_env_target_username_over_the_file() {
        let file = FileSettings {
            target_username: Some("FileUser".to_string()),
            ..FileSettings::default()
        };
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_TARGET_USERNAME", "EnvUser"),
            ]),
            file,
        )
        .unwrap();
        assert_eq!(config.target_username, "EnvUser");
    }

    #[test]
    fn resolve_reads_max_results_from_the_file_when_env_is_unset() {
        let file = FileSettings {
            max_results: Some(42),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file).unwrap();
        assert_eq!(config.max_results, 42);
    }

    #[test]
    fn resolve_prefers_the_env_max_results_over_the_file() {
        let file = FileSettings {
            max_results: Some(42),
            ..FileSettings::default()
        };
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123"), ("X_MAX_RESULTS", "7")]),
            file,
        )
        .unwrap();
        assert_eq!(config.max_results, 7);
    }

    #[test]
    fn resolve_rejects_a_file_max_results_outside_the_api_range() {
        let file = FileSettings {
            max_results: Some(4),
            ..FileSettings::default()
        };
        let error = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file)
            .unwrap_err()
            .to_string();
        assert!(error.contains("between 5 and 100"), "{error}");
        assert!(error.contains("config.toml"), "{error}");
    }

    #[test]
    fn resolve_rejects_a_bearer_token_left_in_the_file() {
        // #33: someone upgrading still has the key. Ignoring it would leave
        // them believing they are configured when nothing reads it, so this
        // is a hard failure that names the replacement — and, as before,
        // never echoes the value itself into the message.
        let file = FileSettings {
            bearer_token: Some(toml::Value::String("leaked".to_string())),
            ..FileSettings::default()
        };
        let error = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file)
            .unwrap_err()
            .to_string();
        assert!(error.contains("no longer supported"), "{error}");
        assert!(error.contains("oauth_client_id"), "{error}");
        assert!(!error.contains("leaked"), "{error}");
    }

    // --- min_fetch_interval_seconds layering (env > file > default, #10) ---

    #[test]
    fn resolve_reads_min_fetch_interval_from_the_file_when_env_is_unset() {
        let file = FileSettings {
            min_fetch_interval_seconds: Some(120),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file).unwrap();
        assert_eq!(config.min_fetch_interval_seconds, 120);
    }

    #[test]
    fn resolve_prefers_the_env_min_fetch_interval_over_the_file() {
        let file = FileSettings {
            min_fetch_interval_seconds: Some(120),
            ..FileSettings::default()
        };
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_MIN_FETCH_INTERVAL_SECONDS", "30"),
            ]),
            file,
        )
        .unwrap();
        assert_eq!(config.min_fetch_interval_seconds, 30);
    }

    #[test]
    fn resolve_rejects_a_min_fetch_interval_of_zero() {
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_MIN_FETCH_INTERVAL_SECONDS", "0"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("X_MIN_FETCH_INTERVAL_SECONDS"), "{error}");
    }

    #[test]
    fn resolve_rejects_a_non_numeric_min_fetch_interval() {
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_MIN_FETCH_INTERVAL_SECONDS", "soon"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("not a number"), "{error}");
    }

    // --- theme layering (env > file > default, #19) ---

    #[test]
    fn resolve_parses_the_theme_from_env() {
        for (raw, expected) in [
            ("light", ThemeMode::Light),
            ("dark", ThemeMode::Dark),
            ("system", ThemeMode::System),
        ] {
            let config = Config::resolve(
                vars(&[("X_OAUTH_CLIENT_ID", "client-123"), ("X_THEME", raw)]),
                FileSettings::default(),
            )
            .unwrap();
            assert_eq!(config.theme, expected, "{raw}");
        }
    }

    #[test]
    fn resolve_theme_is_case_insensitive_and_trims_whitespace() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123"), ("X_THEME", "  DARK\n")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.theme, ThemeMode::Dark);
    }

    #[test]
    fn resolve_reads_the_theme_from_the_file_when_env_is_unset() {
        let file = FileSettings {
            theme: Some("dark".to_string()),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file).unwrap();
        assert_eq!(config.theme, ThemeMode::Dark);
    }

    #[test]
    fn resolve_prefers_the_env_theme_over_the_file() {
        let file = FileSettings {
            theme: Some("dark".to_string()),
            ..FileSettings::default()
        };
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123"), ("X_THEME", "light")]),
            file,
        )
        .unwrap();
        assert_eq!(config.theme, ThemeMode::Light);
    }

    #[test]
    fn resolve_falls_back_to_the_default_theme_when_unset() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.theme, ThemeMode::default());
    }

    // An unrecognized theme must not fail startup (#19) — it falls back to
    // the default. This must hold for both the env and the file source.

    #[test]
    fn resolve_falls_back_to_the_default_theme_on_an_unrecognized_env_value() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_THEME", "solarized"),
            ]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.theme, ThemeMode::default());
    }

    #[test]
    fn resolve_falls_back_to_the_default_theme_on_an_unrecognized_file_value() {
        let file = FileSettings {
            theme: Some("solarized".to_string()),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file).unwrap();
        assert_eq!(config.theme, ThemeMode::default());
    }

    #[test]
    fn resolve_falls_back_to_the_default_theme_when_blank() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123"), ("X_THEME", "   ")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.theme, ThemeMode::default());
    }

    // --- request_price / daily_request_budget (#18) ---

    #[test]
    fn request_price_and_daily_budget_are_unset_by_default() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.request_price, None);
        assert_eq!(config.daily_request_budget, None);
    }

    #[test]
    fn parses_the_request_price_from_env() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_REQUEST_PRICE", "0.015"),
            ]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.request_price, Some(0.015));
    }

    #[test]
    fn rejects_a_non_numeric_request_price() {
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_REQUEST_PRICE", "free"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("X_REQUEST_PRICE"), "{error}");
    }

    #[test]
    fn rejects_a_negative_request_price() {
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_REQUEST_PRICE", "-0.01"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("X_REQUEST_PRICE"), "{error}");
    }

    #[test]
    fn resolve_reads_the_request_price_from_the_file_when_env_is_unset() {
        let file = FileSettings {
            request_price: Some(0.02),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file).unwrap();
        assert_eq!(config.request_price, Some(0.02));
    }

    #[test]
    fn resolve_prefers_the_env_request_price_over_the_file() {
        let file = FileSettings {
            request_price: Some(0.02),
            ..FileSettings::default()
        };
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_REQUEST_PRICE", "0.05"),
            ]),
            file,
        )
        .unwrap();
        assert_eq!(config.request_price, Some(0.05));
    }

    #[test]
    fn resolve_rejects_a_negative_request_price_from_the_file() {
        let file = FileSettings {
            request_price: Some(-1.0),
            ..FileSettings::default()
        };
        let error = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file)
            .unwrap_err()
            .to_string();
        assert!(error.contains("request_price"), "{error}");
    }

    #[test]
    fn parses_the_daily_request_budget_from_env() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_DAILY_REQUEST_BUDGET", "500"),
            ]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.daily_request_budget, Some(500));
    }

    #[test]
    fn rejects_a_non_numeric_daily_request_budget() {
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_DAILY_REQUEST_BUDGET", "lots"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("X_DAILY_REQUEST_BUDGET"), "{error}");
    }

    #[test]
    fn resolve_reads_the_daily_request_budget_from_the_file_when_env_is_unset() {
        let file = FileSettings {
            daily_request_budget: Some(200),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file).unwrap();
        assert_eq!(config.daily_request_budget, Some(200));
    }

    #[test]
    fn resolve_prefers_the_env_daily_request_budget_over_the_file() {
        let file = FileSettings {
            daily_request_budget: Some(200),
            ..FileSettings::default()
        };
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_DAILY_REQUEST_BUDGET", "50"),
            ]),
            file,
        )
        .unwrap();
        assert_eq!(config.daily_request_budget, Some(50));
    }

    #[test]
    fn file_settings_load_returns_defaults_when_the_file_is_missing() {
        let path = std::env::temp_dir().join(format!(
            "twigpui-test-missing-config-{}.toml",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        let settings = FileSettings::load(&path).unwrap();
        assert!(settings.target_username.is_none());
        assert!(settings.max_results.is_none());
    }

    #[test]
    fn file_settings_load_errors_naming_the_path_on_malformed_toml() {
        let path = std::env::temp_dir().join(format!(
            "twigpui-test-malformed-config-{}.toml",
            std::process::id()
        ));
        std::fs::write(&path, "??? not valid toml ???").unwrap();

        let error = FileSettings::load(&path).unwrap_err().to_string();
        assert!(error.contains(&path.display().to_string()), "{error}");

        std::fs::remove_file(&path).unwrap();
    }

    // --- #161: the list id that selects the window's primary source ---

    #[test]
    fn no_list_id_is_configured_by_default() {
        // Absent means "show the home timeline", which is what every launch
        // did before #161. Nothing about that path changes until a list id
        // is set on purpose. Named against the release profile because #169
        // gives the development one a default, and this is the assertion
        // about the build people install.
        let config = Config::resolve_for_profile(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings::default(),
            Profile::Release,
        )
        .unwrap();
        assert_eq!(config.list_id, None);
    }

    #[test]
    fn the_dev_profile_defaults_to_its_own_list() {
        // #169: a development build reads the throwaway list without
        // anything configured, so `--sync-list` cannot rewrite the real
        // one just because an export was forgotten.
        let config = Config::resolve_for_profile(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings::default(),
            Profile::Dev,
        )
        .unwrap();
        assert_eq!(
            config.list_id.as_deref(),
            Profile::Dev.default_list_id(),
            "the dev default must survive config resolution"
        );
    }

    #[test]
    fn a_configured_list_id_still_wins_over_the_dev_default() {
        let config = Config::resolve_for_profile(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123"), ("X_LIST_ID", "111")]),
            FileSettings::default(),
            Profile::Dev,
        )
        .unwrap();
        assert_eq!(config.list_id.as_deref(), Some("111"));
    }

    #[test]
    fn reads_the_list_id_from_the_environment() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_LIST_ID", " 2091351590695588200 "),
            ]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.list_id.as_deref(), Some("2091351590695588200"));
    }

    #[test]
    fn resolve_reads_the_list_id_from_the_file_when_env_is_unset() {
        // The `.app` launched from Finder sees no shell environment (#40),
        // so the file is the only way to configure this there.
        let file = FileSettings {
            list_id: Some("2091351590695588200".to_string()),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file).unwrap();
        assert_eq!(config.list_id.as_deref(), Some("2091351590695588200"));
    }

    #[test]
    fn resolve_prefers_the_env_list_id_over_the_file() {
        let file = FileSettings {
            list_id: Some("111".to_string()),
            ..FileSettings::default()
        };
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123"), ("X_LIST_ID", "222")]),
            file,
        )
        .unwrap();
        assert_eq!(config.list_id.as_deref(), Some("222"));
    }

    #[test]
    fn a_blank_list_id_is_the_same_as_not_setting_one() {
        // Otherwise an empty `X_LIST_ID=` left over in a shell would build
        // `/2/lists//tweets` and spend a request on a 404.
        let config = Config::resolve_for_profile(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123"), ("X_LIST_ID", "   ")]),
            FileSettings::default(),
            Profile::Release,
        )
        .unwrap();
        assert_eq!(config.list_id, None);
    }

    #[test]
    fn rejects_a_list_id_that_is_not_all_digits() {
        // This value is interpolated into a URL path segment, so anything
        // that is not a snowflake id is rejected here rather than sent.
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_LIST_ID", "../users/me"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("X_LIST_ID"), "{error}");
    }

    #[test]
    fn a_rejected_list_id_names_the_file_key_when_that_is_where_it_came_from() {
        let file = FileSettings {
            list_id: Some("not-an-id".to_string()),
            ..FileSettings::default()
        };
        let error = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file)
            .unwrap_err()
            .to_string();
        assert!(error.contains("list_id in config.toml"), "{error}");
    }

    // --- auto_sync_list (env > file > on) ---

    #[test]
    fn the_background_sync_is_on_unless_someone_turns_it_off() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings::default(),
        )
        .unwrap();
        assert!(config.auto_sync_list);
    }

    #[test]
    fn resolve_reads_auto_sync_list_from_the_file_when_env_is_unset() {
        let file = FileSettings {
            auto_sync_list: Some(false),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file).unwrap();
        assert!(!config.auto_sync_list);
    }

    #[test]
    fn resolve_prefers_the_env_auto_sync_list_over_the_file() {
        let file = FileSettings {
            auto_sync_list: Some(true),
            ..FileSettings::default()
        };
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_AUTO_SYNC_LIST", "false"),
            ]),
            file,
        )
        .unwrap();
        assert!(!config.auto_sync_list);
    }

    #[test]
    fn auto_sync_list_takes_the_usual_spellings_of_off() {
        for raw in ["false", "FALSE", "0", "no", "off", " off "] {
            let config = Config::resolve(
                vars(&[
                    ("X_OAUTH_CLIENT_ID", "client-123"),
                    ("X_AUTO_SYNC_LIST", raw),
                ]),
                FileSettings::default(),
            )
            .unwrap();
            assert!(!config.auto_sync_list, "{raw:?}");
        }
    }

    #[test]
    fn resolve_rejects_an_auto_sync_list_it_does_not_understand() {
        // Not a fall-back-to-default like `theme`: a typo'd theme is
        // cosmetic, whereas a typo here would leave a paid loop running for
        // someone who was trying to switch it off.
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_AUTO_SYNC_LIST", "flase"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("X_AUTO_SYNC_LIST"), "{error}");
        assert!(error.contains("flase"), "{error}");
    }

    // --- sync_interval_seconds (env > file > default, with a floor) ---

    #[test]
    fn the_sync_interval_defaults_to_six_hours() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.sync_interval_seconds, 21_600);
    }

    #[test]
    fn resolve_reads_the_sync_interval_from_the_file_when_env_is_unset() {
        let file = FileSettings {
            sync_interval_seconds: Some(3_600),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file).unwrap();
        assert_eq!(config.sync_interval_seconds, 3_600);
    }

    #[test]
    fn resolve_prefers_the_env_sync_interval_over_the_file() {
        let file = FileSettings {
            sync_interval_seconds: Some(3_600),
            ..FileSettings::default()
        };
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_SYNC_INTERVAL_SECONDS", "43200"),
            ]),
            file,
        )
        .unwrap();
        assert_eq!(config.sync_interval_seconds, 43_200);
    }

    #[test]
    fn resolve_rejects_a_sync_interval_below_the_floor() {
        // The decimal-point mistake: 60 where 6000 was meant. Both full
        // reads bill per account returned, so this one is not a typo
        // anybody gets to find out about later.
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_SYNC_INTERVAL_SECONDS", "60"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("at least 900"), "{error}");
        assert!(error.contains("bill per account"), "{error}");
    }

    #[test]
    fn a_rejected_sync_interval_names_the_file_key_when_that_is_where_it_came_from() {
        let file = FileSettings {
            sync_interval_seconds: Some(0),
            ..FileSettings::default()
        };
        let error = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("sync_interval_seconds in config.toml"),
            "{error}"
        );
    }

    #[test]
    fn resolve_rejects_a_non_numeric_sync_interval() {
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_SYNC_INTERVAL_SECONDS", "soon"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("is not a number"), "{error}");
    }

    // --- #197: sync_writes_per_minute (env > file > 2, 1..=20) ---

    #[test]
    fn the_write_pace_defaults_to_two_a_minute() {
        // The measured basis: a hidden cap engaged at roughly seven writes
        // a minute and stayed down for 24 hours. The default is the pace
        // that does not trip it, not the fastest allowed.
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.sync_writes_per_minute, 2);
    }

    #[test]
    fn resolve_reads_the_write_pace_from_the_file_when_env_is_unset() {
        let file = FileSettings {
            sync_writes_per_minute: Some(5),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file).unwrap();
        assert_eq!(config.sync_writes_per_minute, 5);
    }

    #[test]
    fn resolve_prefers_the_env_write_pace_over_the_file() {
        let file = FileSettings {
            sync_writes_per_minute: Some(5),
            ..FileSettings::default()
        };
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_SYNC_WRITES_PER_MINUTE", "10"),
            ]),
            file,
        )
        .unwrap();
        assert_eq!(config.sync_writes_per_minute, 10);
    }

    #[test]
    fn resolve_rejects_a_write_pace_of_zero() {
        // 0 is not "off" — `auto_sync_list` is. A pace of zero would be a
        // sync that claims to run while never draining its plan.
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_SYNC_WRITES_PER_MINUTE", "0"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("between 1 and 20"), "{error}");
    }

    #[test]
    fn resolve_rejects_a_write_pace_past_the_documented_window() {
        // 20/min is 300 per 15 minutes spread evenly — X's documented
        // window. Above it the tracked window refuses sends anyway, so a
        // larger value only buys refusals.
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_SYNC_WRITES_PER_MINUTE", "21"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("between 1 and 20"), "{error}");
    }

    #[test]
    fn the_documented_window_pace_itself_is_accepted() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_SYNC_WRITES_PER_MINUTE", "20"),
            ]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.sync_writes_per_minute, 20);
    }

    #[test]
    fn a_rejected_write_pace_names_the_file_key_when_that_is_where_it_came_from() {
        let file = FileSettings {
            sync_writes_per_minute: Some(120),
            ..FileSettings::default()
        };
        let error = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("sync_writes_per_minute in config.toml"),
            "{error}"
        );
    }

    #[test]
    fn resolve_rejects_a_non_numeric_write_pace() {
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_SYNC_WRITES_PER_MINUTE", "fast"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("is not a number"), "{error}");
    }

    // --- #176: sync_prune_limit_percent (env > file > 10, at most 100) ---

    #[test]
    fn the_prune_limit_defaults_to_ten_percent() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings::default(),
        )
        .unwrap();
        assert_eq!(config.sync_prune_limit_percent, 10);
    }

    #[test]
    fn resolve_reads_the_prune_limit_from_the_file_when_env_is_unset() {
        let file = FileSettings {
            sync_prune_limit_percent: Some(25),
            ..FileSettings::default()
        };
        let config = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file).unwrap();
        assert_eq!(config.sync_prune_limit_percent, 25);
    }

    #[test]
    fn resolve_prefers_the_env_prune_limit_over_the_file() {
        let file = FileSettings {
            sync_prune_limit_percent: Some(25),
            ..FileSettings::default()
        };
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_SYNC_PRUNE_LIMIT_PERCENT", "100"),
            ]),
            file,
        )
        .unwrap();
        assert_eq!(config.sync_prune_limit_percent, 100);
    }

    #[test]
    fn resolve_rejects_a_prune_limit_over_one_hundred_percent() {
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_SYNC_PRUNE_LIMIT_PERCENT", "150"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("at most 100"), "{error}");
    }

    #[test]
    fn a_rejected_prune_limit_names_the_file_key_when_that_is_where_it_came_from() {
        let file = FileSettings {
            sync_prune_limit_percent: Some(101),
            ..FileSettings::default()
        };
        let error = Config::resolve(vars(&[("X_OAUTH_CLIENT_ID", "client-123")]), file)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("sync_prune_limit_percent in config.toml"),
            "{error}"
        );
    }

    #[test]
    fn resolve_rejects_a_non_numeric_prune_limit() {
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_SYNC_PRUNE_LIMIT_PERCENT", "half"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("is not a number"), "{error}");
    }

    // --- #21: auto-refresh ---

    #[test]
    fn auto_refresh_is_on_by_default() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings::default(),
        )
        .unwrap();

        assert!(config.auto_refresh);
        assert_eq!(
            config.auto_refresh_interval_seconds,
            super::DEFAULT_AUTO_REFRESH_INTERVAL_SECONDS
        );
    }

    #[test]
    fn auto_refresh_can_be_switched_off_from_the_environment() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_AUTO_REFRESH", "off"),
            ]),
            FileSettings::default(),
        )
        .unwrap();

        assert!(!config.auto_refresh);
    }

    #[test]
    fn auto_refresh_can_be_switched_off_from_the_file() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings {
                auto_refresh: Some(false),
                ..FileSettings::default()
            },
        )
        .unwrap();

        assert!(!config.auto_refresh);
    }

    #[test]
    fn the_environment_wins_over_the_file_for_auto_refresh() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_AUTO_REFRESH", "true"),
            ]),
            FileSettings {
                auto_refresh: Some(false),
                ..FileSettings::default()
            },
        )
        .unwrap();

        assert!(config.auto_refresh);
    }

    // Same reasoning as `X_AUTO_SYNC_LIST`: a typo read as the default
    // would leave a paid timer running for someone switching it off.
    #[test]
    fn rejects_an_unrecognized_auto_refresh_value() {
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_AUTO_REFRESH", "flase"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("X_AUTO_REFRESH"), "{error}");
    }

    #[test]
    fn reads_the_auto_refresh_interval_from_the_environment() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_AUTO_REFRESH_INTERVAL_SECONDS", "900"),
            ]),
            FileSettings::default(),
        )
        .unwrap();

        assert_eq!(config.auto_refresh_interval_seconds, 900);
    }

    #[test]
    fn reads_the_auto_refresh_interval_from_the_file_when_env_is_unset() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings {
                auto_refresh_interval_seconds: Some(600),
                ..FileSettings::default()
            },
        )
        .unwrap();

        assert_eq!(config.auto_refresh_interval_seconds, 600);
    }

    // The interval this loop is paced by cannot be shorter than the one
    // `reload_gate` refuses inside — every tick would be blocked before it
    // sent anything, and auto-refresh would silently never happen.
    #[test]
    fn rejects_an_auto_refresh_interval_below_the_fetch_interval() {
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_MIN_FETCH_INTERVAL_SECONDS", "120"),
                ("X_AUTO_REFRESH_INTERVAL_SECONDS", "60"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("X_AUTO_REFRESH_INTERVAL_SECONDS"), "{error}");
        assert!(error.contains("120"), "{error}");
    }

    #[test]
    fn accepts_an_auto_refresh_interval_equal_to_the_fetch_interval() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_MIN_FETCH_INTERVAL_SECONDS", "120"),
                ("X_AUTO_REFRESH_INTERVAL_SECONDS", "120"),
            ]),
            FileSettings::default(),
        )
        .unwrap();

        assert_eq!(config.auto_refresh_interval_seconds, 120);
    }

    // The floor names the file key when that is where the value came
    // from, mirroring `resolve_sync_interval`'s two-source message.
    #[test]
    fn the_auto_refresh_interval_floor_names_the_file_key() {
        let error = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings {
                auto_refresh_interval_seconds: Some(10),
                ..FileSettings::default()
            },
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("auto_refresh_interval_seconds in config.toml"),
            "{error}"
        );
    }

    #[test]
    fn rejects_a_non_numeric_auto_refresh_interval() {
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_AUTO_REFRESH_INTERVAL_SECONDS", "often"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("is not a number"), "{error}");
    }

    // --- #22: follow new posts ---

    #[test]
    fn following_new_posts_is_on_by_default() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings::default(),
        )
        .unwrap();

        assert!(config.follow_new_posts);
    }

    #[test]
    fn following_new_posts_can_be_switched_off_from_the_environment() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_FOLLOW_NEW_POSTS", "off"),
            ]),
            FileSettings::default(),
        )
        .unwrap();

        assert!(!config.follow_new_posts);
    }

    #[test]
    fn following_new_posts_can_be_switched_off_from_the_file() {
        let config = Config::resolve(
            vars(&[("X_OAUTH_CLIENT_ID", "client-123")]),
            FileSettings {
                follow_new_posts: Some(false),
                ..FileSettings::default()
            },
        )
        .unwrap();

        assert!(!config.follow_new_posts);
    }

    #[test]
    fn the_environment_wins_over_the_file_for_following_new_posts() {
        let config = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_FOLLOW_NEW_POSTS", "true"),
            ]),
            FileSettings {
                follow_new_posts: Some(false),
                ..FileSettings::default()
            },
        )
        .unwrap();

        assert!(config.follow_new_posts);
    }

    // Unlike `X_AUTO_REFRESH`, a typo here costs nothing — the switch is
    // about presentation, not spending. It is still rejected, for the
    // plainer reason that reading `flase` as "on" silently ignores what
    // the person wrote.
    #[test]
    fn rejects_an_unrecognized_follow_new_posts_value() {
        let error = Config::resolve(
            vars(&[
                ("X_OAUTH_CLIENT_ID", "client-123"),
                ("X_FOLLOW_NEW_POSTS", "flase"),
            ]),
            FileSettings::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("X_FOLLOW_NEW_POSTS"), "{error}");
    }
}
