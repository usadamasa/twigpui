//! Request-count usage tracking (#18): what each tracked endpoint has cost,
//! cumulatively and today, persisted across restarts under `state_dir`.
//!
//! Reuses `rate_limit::Endpoint` — X limits (and this module counts) the
//! same five endpoints separately, so a parallel enum here would just be
//! `Endpoint` with the serial numbers filed off. [`Endpoint::ALL`] lets this
//! module (and `main.rs`'s `--usage`) iterate every tracked endpoint without
//! duplicating the list.
//!
//! Four pure seams, mirroring `rate_limit.rs`'s own convention: [`record`]
//! (a stored count + an injected `now` -> the next count, including the
//! "today" bucket's rollover), [`today_count`] (a stored count + `now` ->
//! what "today" reads as *right now*, without mutating anything — covers
//! reading the file after midnight but before the next request writes to
//! it), [`estimated_amount`] (a request count + an optional configured
//! price -> an optional amount, `None` whenever no price is configured —
//! never a guessed number in its place), and [`budget_status`] (today's
//! total + an optional configured budget -> which of three severities the
//! header should render). [`build_report`] composes all four into the same
//! shape both the header and `--usage` show, so there is exactly one place
//! that decides what "the usage numbers" means. Only [`load`]/[`save`]
//! (disk) and `x_api::client::XClient::get` (network, via
//! [`record_request`]) touch anything outside memory.
//!
//! ## Day boundary: UTC, not local time
//!
//! "Today" resets at UTC midnight, not the machine's local midnight. Two
//! reasons:
//!
//! 1. The X API itself reports `created_at` in UTC (see
//!    `ui::format_timestamp`'s doc comment) — X's own notion of "a day" for
//!    this account's data is already UTC, so tracking spend against the same
//!    boundary keeps "today" meaning one consistent thing everywhere in this
//!    app, rather than two different clocks for two kinds of "today".
//! 2. Rust's standard library has no reliable way to read the local UTC
//!    offset without a date/time crate (`chrono`, `time`, ...), and this
//!    crate does not currently depend on one. UTC needs none: a day
//!    boundary is exactly `unix_seconds.div_euclid(86_400)`, computed on the
//!    same `i64` Unix timestamp `oauth::unix_now()` already hands every
//!    other module here. No new dependency was needed for this issue.
//!
//! The tradeoff this accepts: someone west of UTC sees "today" roll over
//! mid-afternoon local time, not at their own midnight. Documented here and
//! in the README rather than left implicit.

use std::collections::HashMap;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::paths::Paths;
use crate::rate_limit::Endpoint;

/// Seconds in a day — the unit [`epoch_day`] buckets Unix timestamps into.
const SECONDS_PER_DAY: i64 = 24 * 60 * 60;

/// Fraction of a configured budget at which [`budget_status`] starts
/// warning rather than waiting for the budget to be fully exhausted — the
/// whole point of a budget setting is to see it coming, not to find out
/// only at the moment it's crossed.
const NEAR_BUDGET_RATIO: f64 = 0.8;

/// One endpoint's tracked request counts: all-time, and the current UTC
/// day's — see the module doc for why UTC. Every field defaults to zero so
/// an endpoint with no entry on file yet (or a file from an older version
/// missing a field) reads as "never called" rather than failing to parse.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EndpointUsage {
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub today: u64,
    /// The UTC epoch day (see [`epoch_day`]) that `today` was last reset
    /// for. Compared against the current epoch day, not stored as a literal
    /// date, so no date-formatting or parsing is ever needed.
    #[serde(default)]
    pub today_epoch_day: i64,
}

/// The UTC epoch day `now` (Unix seconds) falls in. Two timestamps map to
/// the same day exactly when a UTC midnight does not separate them.
pub(crate) fn epoch_day(now: i64) -> i64 {
    0
}

/// What `entry.today` reads as *right now*, without mutating anything: the
/// stored count while `now` is still within the UTC day it was last reset
/// for, else zero — covers reading the file after midnight but before the
/// next [`record`] call would actually perform the reset on disk.
pub(crate) fn today_count(entry: EndpointUsage, now: i64) -> u64 {
    entry.today
}

/// Record one more request against `entry` at `now`: `total` always
/// increments; `today` increments from zero if `now` has crossed into a new
/// UTC day since `entry.today_epoch_day`, or from its current value
/// otherwise. `saturating_add` rather than a bare `+`: a `u64` overflowing
/// is astronomically unlikely for a request counter, but silently wrapping
/// to zero would be a worse failure mode than saturating.
pub(crate) fn record(entry: EndpointUsage, now: i64) -> EndpointUsage {
    entry
}

/// Turn `count` into an estimated amount, in whatever unit
/// `price_per_request` is denominated in (the app never assumes a
/// currency) — `None` unless a price is configured. This is the one rule
/// #18 exists to enforce: a plausible-looking but wrong number is worse
/// than no number at all, so there is no built-in default price anywhere in
/// this crate.
pub(crate) fn estimated_amount(count: u64, price_per_request: Option<f64>) -> Option<f64> {
    None
}

/// Which of three severities today's usage falls into, relative to an
/// optional configured daily budget. `Ok` whenever no budget is configured
/// at all — there is nothing to warn against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BudgetStatus {
    /// No budget configured, or today's count is comfortably under it.
    Ok,
    /// Today's count has reached [`NEAR_BUDGET_RATIO`] of the budget but not
    /// yet the budget itself.
    Near,
    /// Today's count has reached or passed the budget.
    Exceeded,
}

/// Classify `today_total` against `daily_budget` — see [`BudgetStatus`].
/// `budget == 0` is treated as already exceeded by any non-negative count,
/// rather than dividing by zero.
pub(crate) fn budget_status(today_total: u64, daily_budget: Option<u32>) -> BudgetStatus {
    BudgetStatus::Ok
}

/// The whole contents of [`Paths::usage_file`]: every endpoint's tracked
/// counts, keyed by [`Endpoint::key`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct UsageFile {
    #[serde(default)]
    endpoints: HashMap<String, EndpointUsage>,
}

/// Load [`UsageFile`] from disk. A missing file is a clean "nothing tracked
/// yet"; a corrupt or differently-shaped file is *also* a clean miss rather
/// than an error, mirroring `rate_limit::load_file` and
/// `cache::load_json`'s shared rule — losing this file costs at most the
/// cumulative counter, never a startup failure.
fn load_file(paths: &Paths) -> Result<UsageFile> {
    let path = paths.usage_file();
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(UsageFile::default());
        }
        Err(error) => {
            return Err(error).with_context(|| format!("could not read {}", path.display()));
        }
    };
    Ok(serde_json::from_str(&contents).unwrap_or_default())
}

/// The tracked usage for `endpoint`, or [`EndpointUsage::default`] (all
/// zero) if there is nothing on file for it yet.
pub(crate) fn load(paths: &Paths, endpoint: Endpoint) -> Result<EndpointUsage> {
    let file = load_file(paths)?;
    Ok(file
        .endpoints
        .get(endpoint.key())
        .copied()
        .unwrap_or_default())
}

/// The tracked usage for every endpoint in [`Endpoint::ALL`], reading the
/// file once rather than once per endpoint — used by the header refresh and
/// `--usage`, both of which always want every endpoint at once.
pub(crate) fn load_all(paths: &Paths) -> Result<HashMap<Endpoint, EndpointUsage>> {
    let file = load_file(paths)?;
    Ok(Endpoint::ALL
        .into_iter()
        .map(|endpoint| {
            let usage = file
                .endpoints
                .get(endpoint.key())
                .copied()
                .unwrap_or_default();
            (endpoint, usage)
        })
        .collect())
}

/// Persist `usage` for `endpoint`, alongside whatever other endpoints were
/// already on file — a genuine I/O error reading the existing file (as
/// opposed to it being merely absent or corrupt) still propagates, the same
/// distinction `rate_limit::save` and `cache.rs` draw.
pub(crate) fn save(paths: &Paths, endpoint: Endpoint, usage: EndpointUsage) -> Result<()> {
    let path = paths.usage_file();
    let mut file = load_file(paths)?;
    file.endpoints.insert(endpoint.key().to_string(), usage);

    let json = serde_json::to_vec_pretty(&file)
        .with_context(|| format!("could not serialize {}", path.display()))?;
    std::fs::write(&path, json).with_context(|| format!("could not write {}", path.display()))
}

/// Record one request against `endpoint` at `now`, touching disk — the one
/// non-pure function in this module's counting seam, called once per actual
/// HTTP send from `x_api::client::XClient::get` (including retries, since
/// each is separately billed — see that function's own doc comment).
pub(crate) fn record_request(paths: &Paths, endpoint: Endpoint, now: i64) -> Result<()> {
    let current = load(paths, endpoint)?;
    let updated = record(current, now);
    save(paths, endpoint, updated)
}

/// Summed counts across every tracked endpoint — what the header and
/// `--usage` both show as "the" usage number, rather than five separate
/// per-endpoint figures nobody asked for at a glance. `today` applies
/// [`today_count`]'s rollover per entry before summing, so a summary read
/// right after UTC midnight (before the next request writes anything) shows
/// zero rather than yesterday's stale count.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Totals {
    pub total: u64,
    pub today: u64,
}

pub(crate) fn totals(entries: &HashMap<Endpoint, EndpointUsage>, now: i64) -> Totals {
    entries
        .values()
        .fold(Totals::default(), |acc, entry| Totals {
            total: acc.total.saturating_add(entry.total),
            today: acc.today.saturating_add(today_count(*entry, now)),
        })
}

/// One endpoint's counts, as shown in [`UsageReport`] — `today` already has
/// [`today_count`]'s rollover applied.
#[derive(Debug, Serialize)]
pub(crate) struct EndpointReport {
    pub total: u64,
    pub today: u64,
}

/// The aggregate figures in [`UsageReport`]: totals across every endpoint,
/// plus whatever `estimated_amount`/`budget_status` derive from them and
/// the caller's configured price/budget.
#[derive(Debug, Serialize)]
pub(crate) struct TotalsReport {
    pub total_requests: u64,
    pub today_requests: u64,
    pub price_per_request: Option<f64>,
    pub estimated_amount_total: Option<f64>,
    pub estimated_amount_today: Option<f64>,
    pub daily_budget: Option<u32>,
    pub budget_status: &'static str,
}

/// The full machine-readable usage report (#18): what `--usage` prints as
/// JSON, and the same numbers the header renders — one function producing
/// both, so there is no way for the two to drift apart.
#[derive(Debug, Serialize)]
pub(crate) struct UsageReport {
    pub endpoints: HashMap<String, EndpointReport>,
    pub total: TotalsReport,
}

/// Compose [`UsageReport`] from every tracked endpoint's stored counts, an
/// injected `now`, and the caller's configured price/budget (both entirely
/// optional — see the module doc). Pure: takes the already-loaded `entries`
/// rather than reading `paths` itself, so it's testable without touching
/// disk, matching this module's other three seams.
pub(crate) fn build_report(
    entries: &HashMap<Endpoint, EndpointUsage>,
    now: i64,
    price_per_request: Option<f64>,
    daily_budget: Option<u32>,
) -> UsageReport {
    let endpoints = entries
        .iter()
        .map(|(endpoint, usage)| {
            (
                endpoint.key().to_string(),
                EndpointReport {
                    total: usage.total,
                    today: today_count(*usage, now),
                },
            )
        })
        .collect();

    let totals = totals(entries, now);
    let status = match budget_status(totals.today, daily_budget) {
        BudgetStatus::Ok => "ok",
        BudgetStatus::Near => "near",
        BudgetStatus::Exceeded => "exceeded",
    };

    UsageReport {
        endpoints,
        total: TotalsReport {
            total_requests: totals.total,
            today_requests: totals.today,
            price_per_request,
            estimated_amount_total: estimated_amount(totals.total, price_per_request),
            estimated_amount_today: estimated_amount(totals.today, price_per_request),
            daily_budget,
            budget_status: status,
        },
    }
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
            "twigpui-test-usage-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    /// Float equality without tripping `clippy::float_cmp` at the call
    /// site — every value compared here is chosen to be exactly
    /// representable, so this is a belt-and-braces epsilon rather than a
    /// tolerance that could mask a real bug.
    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    // --- epoch_day ---

    #[test]
    fn epoch_day_is_zero_at_the_unix_epoch() {
        assert_eq!(epoch_day(0), 0);
    }

    #[test]
    fn epoch_day_stays_the_same_within_one_utc_day() {
        assert_eq!(epoch_day(0), epoch_day(86_399));
    }

    #[test]
    fn epoch_day_advances_exactly_at_utc_midnight() {
        assert_eq!(epoch_day(86_400), epoch_day(0) + 1);
    }

    // --- today_count (rollover, read-only) ---

    #[test]
    fn today_count_reads_the_stored_value_within_the_same_utc_day() {
        let entry = EndpointUsage {
            total: 10,
            today: 3,
            today_epoch_day: 0,
        };
        assert_eq!(today_count(entry, 86_399), 3);
    }

    #[test]
    fn today_count_reads_zero_once_a_new_utc_day_has_started() {
        // The file hasn't been written since the day rolled over, but a
        // reader (the header, `--usage`) still must not show yesterday's
        // stale count.
        let entry = EndpointUsage {
            total: 10,
            today: 3,
            today_epoch_day: 0,
        };
        assert_eq!(today_count(entry, 86_400), 0);
    }

    // --- record ---

    #[test]
    fn record_increments_both_total_and_today_within_the_same_day() {
        let entry = EndpointUsage {
            total: 5,
            today: 2,
            today_epoch_day: 0,
        };
        let updated = record(entry, 100);
        assert_eq!(updated.total, 6);
        assert_eq!(updated.today, 3);
        assert_eq!(updated.today_epoch_day, 0);
    }

    #[test]
    fn record_resets_today_to_one_when_crossing_the_utc_day_boundary() {
        let entry = EndpointUsage {
            total: 5,
            today: 2,
            today_epoch_day: 0,
        };
        let updated = record(entry, 86_400);
        assert_eq!(updated.total, 6, "total keeps accumulating regardless");
        assert_eq!(updated.today, 1, "today starts over at exactly one");
        assert_eq!(updated.today_epoch_day, 1);
    }

    #[test]
    fn record_pins_the_exact_boundary_second() {
        // 86_399 is still "day 0" (23:59:59 UTC); 86_400 is "day 1"
        // (00:00:00 UTC) — the rollover must land on exactly this second,
        // not one either side of it.
        let entry = EndpointUsage {
            total: 0,
            today: 0,
            today_epoch_day: 0,
        };
        assert_eq!(record(entry, 86_399).today, 1);
        assert_eq!(record(entry, 86_399).today_epoch_day, 0);

        let rolled = record(entry, 86_400);
        assert_eq!(rolled.today, 1);
        assert_eq!(rolled.today_epoch_day, 1);
    }

    #[test]
    fn record_starts_a_fresh_entry_at_one() {
        let updated = record(EndpointUsage::default(), 0);
        assert_eq!(updated.total, 1);
        assert_eq!(updated.today, 1);
    }

    // --- estimated_amount ---

    #[test]
    fn estimated_amount_is_none_without_a_configured_price() {
        // #18's core rule: no price configured means no amount shown,
        // ever — never a guessed number.
        assert_eq!(estimated_amount(42, None), None);
    }

    #[test]
    fn estimated_amount_multiplies_count_by_the_configured_price() {
        let amount = estimated_amount(4, Some(2.5)).unwrap();
        assert!(approx_eq(amount, 10.0), "{amount}");
    }

    #[test]
    fn estimated_amount_is_zero_for_zero_requests_even_with_a_price() {
        let amount = estimated_amount(0, Some(2.5)).unwrap();
        assert!(approx_eq(amount, 0.0), "{amount}");
    }

    // --- budget_status ---

    #[test]
    fn budget_status_is_ok_without_a_configured_budget() {
        assert_eq!(budget_status(1_000_000, None), BudgetStatus::Ok);
    }

    #[test]
    fn budget_status_is_ok_comfortably_under_the_budget() {
        assert_eq!(budget_status(5, Some(10)), BudgetStatus::Ok);
    }

    #[test]
    fn budget_status_is_near_at_the_warning_threshold() {
        // 8/10 = 0.8 = NEAR_BUDGET_RATIO exactly.
        assert_eq!(budget_status(8, Some(10)), BudgetStatus::Near);
    }

    #[test]
    fn budget_status_is_ok_just_below_the_warning_threshold() {
        assert_eq!(budget_status(7, Some(10)), BudgetStatus::Ok);
    }

    #[test]
    fn budget_status_is_exceeded_at_the_budget() {
        assert_eq!(budget_status(10, Some(10)), BudgetStatus::Exceeded);
    }

    #[test]
    fn budget_status_is_exceeded_past_the_budget() {
        assert_eq!(budget_status(11, Some(10)), BudgetStatus::Exceeded);
    }

    #[test]
    fn budget_status_treats_a_zero_budget_as_already_exceeded() {
        assert_eq!(budget_status(0, Some(0)), BudgetStatus::Exceeded);
    }

    // --- load / save / load_all ---

    #[test]
    fn load_is_the_default_usage_when_nothing_is_on_file() {
        let root = temp_root("load-missing");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        assert_eq!(
            load(&paths, Endpoint::Timeline).unwrap(),
            EndpointUsage::default()
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn save_then_load_roundtrips_for_the_same_endpoint() {
        let root = temp_root("roundtrip");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let usage = EndpointUsage {
            total: 7,
            today: 2,
            today_epoch_day: 3,
        };
        save(&paths, Endpoint::Timeline, usage).unwrap();
        assert_eq!(load(&paths, Endpoint::Timeline).unwrap(), usage);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn save_keeps_other_endpoints_usage_untouched() {
        let root = temp_root("multi-endpoint");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let user_lookup_usage = EndpointUsage {
            total: 3,
            today: 1,
            today_epoch_day: 0,
        };
        let timeline_usage = EndpointUsage {
            total: 9,
            today: 4,
            today_epoch_day: 0,
        };
        save(&paths, Endpoint::UserLookup, user_lookup_usage).unwrap();
        save(&paths, Endpoint::Timeline, timeline_usage).unwrap();

        assert_eq!(
            load(&paths, Endpoint::UserLookup).unwrap(),
            user_lookup_usage
        );
        assert_eq!(load(&paths, Endpoint::Timeline).unwrap(), timeline_usage);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn load_all_returns_every_tracked_endpoint_including_untouched_ones() {
        let root = temp_root("load-all");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        let timeline_usage = EndpointUsage {
            total: 2,
            today: 2,
            today_epoch_day: 0,
        };
        save(&paths, Endpoint::Timeline, timeline_usage).unwrap();

        let all = load_all(&paths).unwrap();
        assert_eq!(all.len(), Endpoint::ALL.len());
        assert_eq!(all[&Endpoint::Timeline], timeline_usage);
        assert_eq!(all[&Endpoint::Me], EndpointUsage::default());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_corrupted_usage_file_is_a_clean_miss_not_an_error() {
        let root = temp_root("corrupt");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.usage_file(), b"not json at all").unwrap();

        assert_eq!(
            load(&paths, Endpoint::Timeline).unwrap(),
            EndpointUsage::default()
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn save_recovers_cleanly_from_a_corrupted_existing_file() {
        let root = temp_root("save-over-corrupt");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.usage_file(), b"{ not valid json").unwrap();

        let usage = EndpointUsage {
            total: 1,
            today: 1,
            today_epoch_day: 0,
        };
        save(&paths, Endpoint::Timeline, usage).unwrap();
        assert_eq!(load(&paths, Endpoint::Timeline).unwrap(), usage);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_genuine_io_error_reading_the_usage_file_still_propagates() {
        let root = temp_root("io-error");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();
        // A directory where a file is expected is a real I/O error, not
        // corruption — it must surface rather than being swallowed.
        std::fs::create_dir(paths.usage_file()).unwrap();

        assert!(load(&paths, Endpoint::Timeline).is_err());

        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- record_request (the thin I/O wrapper) ---

    #[test]
    fn record_request_persists_an_incremented_count() {
        let root = temp_root("record-request");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        record_request(&paths, Endpoint::Timeline, 0).unwrap();
        record_request(&paths, Endpoint::Timeline, 100).unwrap();

        let usage = load(&paths, Endpoint::Timeline).unwrap();
        assert_eq!(usage.total, 2);
        assert_eq!(usage.today, 2);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn record_request_rolls_today_over_across_a_day_boundary() {
        let root = temp_root("record-request-rollover");
        let paths = test_paths(&root);
        paths.ensure_dirs().unwrap();

        record_request(&paths, Endpoint::Timeline, 86_399).unwrap();
        record_request(&paths, Endpoint::Timeline, 86_400).unwrap();

        let usage = load(&paths, Endpoint::Timeline).unwrap();
        assert_eq!(usage.total, 2, "total never resets");
        assert_eq!(usage.today, 1, "today resets across the UTC boundary");

        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- totals ---

    #[test]
    fn totals_sums_every_endpoints_stored_counts() {
        let mut entries = HashMap::new();
        entries.insert(
            Endpoint::UserLookup,
            EndpointUsage {
                total: 3,
                today: 1,
                today_epoch_day: 0,
            },
        );
        entries.insert(
            Endpoint::Timeline,
            EndpointUsage {
                total: 5,
                today: 2,
                today_epoch_day: 0,
            },
        );

        let totals = totals(&entries, 0);
        assert_eq!(totals.total, 8);
        assert_eq!(totals.today, 3);
    }

    #[test]
    fn totals_applies_the_rollover_per_entry_before_summing() {
        let mut entries = HashMap::new();
        entries.insert(
            Endpoint::UserLookup,
            EndpointUsage {
                total: 3,
                today: 1,
                today_epoch_day: 0,
            },
        );

        // Read from the next UTC day: `today` must roll over to zero even
        // though nothing has been recorded since.
        let totals = totals(&entries, 86_400);
        assert_eq!(totals.total, 3);
        assert_eq!(totals.today, 0);
    }

    // --- build_report ---

    #[test]
    fn build_report_shows_counts_without_amounts_when_no_price_is_configured() {
        let mut entries = HashMap::new();
        entries.insert(
            Endpoint::Timeline,
            EndpointUsage {
                total: 4,
                today: 4,
                today_epoch_day: 0,
            },
        );

        let report = build_report(&entries, 0, None, None);
        assert_eq!(report.total.total_requests, 4);
        assert_eq!(report.total.today_requests, 4);
        assert_eq!(report.total.price_per_request, None);
        assert_eq!(report.total.estimated_amount_total, None);
        assert_eq!(report.total.estimated_amount_today, None);
        assert_eq!(report.total.budget_status, "ok");
        assert_eq!(report.endpoints["timeline"].total, 4);
    }

    #[test]
    fn build_report_includes_estimated_amounts_once_a_price_is_configured() {
        let mut entries = HashMap::new();
        entries.insert(
            Endpoint::Timeline,
            EndpointUsage {
                total: 4,
                today: 2,
                today_epoch_day: 0,
            },
        );

        let report = build_report(&entries, 0, Some(2.5), None);
        assert!(approx_eq(
            report.total.estimated_amount_total.unwrap(),
            10.0
        ));
        assert!(approx_eq(report.total.estimated_amount_today.unwrap(), 5.0));
    }

    #[test]
    fn build_report_reflects_the_budget_status_string() {
        let mut entries = HashMap::new();
        entries.insert(
            Endpoint::Timeline,
            EndpointUsage {
                total: 10,
                today: 10,
                today_epoch_day: 0,
            },
        );

        let report = build_report(&entries, 0, None, Some(10));
        assert_eq!(report.total.budget_status, "exceeded");
        assert_eq!(report.total.daily_budget, Some(10));
    }
}
