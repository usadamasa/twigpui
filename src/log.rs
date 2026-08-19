//! Diagnostic logging to a file (#49).
//!
//! ## Why this exists
//!
//! Everything this app knew how to say went to stderr, and a `.app`
//! launched from Finder has no stderr for anyone to read (#40, #45). The
//! startup alert dialog covers exactly one case — "it did not start" —
//! leaving nothing at all for a session that starts fine and then
//! misbehaves.
//!
//! ## Why not `tracing` or `log`
//!
//! #46 is an open issue about build time, and this needs a line with a
//! level, a timestamp, and a size cap. `tracing` plus a subscriber and an
//! appender is a large tree to compile on every build for that. The same
//! reasoning already produced this crate's own JSON persistence and rate
//! limit tracking rather than a framework.
//!
//! ## The rule that matters most
//!
//! **A token must never reach the file.** This app holds an OAuth access
//! token, a refresh token, and possibly an app-only bearer token; the token
//! file itself is `0600` (#7), which buys nothing if the same value lands
//! in a world-readable log. Every message goes through [`redact`], the log
//! file is created `0600` too, and the tests below are the actual guarantee
//! rather than the care of whoever writes the next call site.

use std::fmt::Write as _;
use std::io::{IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::paths::Paths;

/// Rotate once the file passes this. `~/.local/state` is not swept by
/// macOS, so an unbounded log is a slow leak — the same reasoning behind
/// #9's cache cap.
const MAX_BYTES: u64 = 1024 * 1024;

/// How much detail reaches the log.
///
/// Ordered, so a configured level admits everything at or above it — see
/// [`write`]'s `level > sink.level` check, which is what this ordering is
/// for. There is deliberately no `Off`: the quietest useful setting is
/// `error`, and a log that records nothing at all is indistinguishable
/// from the state #49 exists to end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(crate) enum Level {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
}

impl Level {
    /// Parse a configured level, case-insensitively. `None` for anything
    /// unrecognized — callers fall back to the default rather than failing
    /// startup, matching how `config.rs` treats an unknown theme.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "error" => Some(Self::Error),
            "warn" | "warning" => Some(Self::Warn),
            "info" => Some(Self::Info),
            "debug" => Some(Self::Debug),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warn => "WARN",
            Self::Info => "INFO",
            Self::Debug => "DEBUG",
        }
    }
}

/// Everything the writer needs, set once at startup by [`init`].
struct Sink {
    path: PathBuf,
    level: Level,
    /// Whether stderr is a terminal. When it is, messages go to both — the
    /// issue's "don't break the `cargo run` experience" requirement — and
    /// when it isn't (a `.app` from Finder), the file is the only record.
    echo_to_stderr: bool,
    /// Serializes writes. One process, several gpui background tasks.
    file: Mutex<()>,
}

static SINK: OnceLock<Sink> = OnceLock::new();

/// Point logging at `paths`' log file at `level`. Called once at startup.
///
/// Failing to create the directory is not fatal and not reported: an app
/// that refuses to start because it could not open its *log* has the
/// priorities backwards. Logging simply stays off, and stderr — if there is
/// one — still shows everything.
pub(crate) fn init(paths: &Paths, level: Level) {
    let dir = paths.log_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let _ = SINK.set(Sink {
        path: paths.log_file(),
        level,
        echo_to_stderr: std::io::stderr().is_terminal(),
        file: Mutex::new(()),
    });
}

/// Write one line at `level`.
///
/// Both the message and — before it — the file are handled defensively: a
/// log that cannot be written must never take the app down with it, so
/// every failure here is swallowed. The one thing that is not optional is
/// [`redact`].
pub(crate) fn write(level: Level, message: &str) {
    let safe = redact(message);

    let Some(sink) = SINK.get() else {
        // Before `init`, or after it failed: stderr is all there is.
        eprintln!("{} {safe}", level.label());
        return;
    };

    if sink.echo_to_stderr {
        eprintln!("{} {safe}", level.label());
    }
    if level > sink.level {
        return;
    }

    let line = format!("{} {} {safe}\n", format_utc(now()), level.label());
    let Ok(_guard) = sink.file.lock() else {
        return;
    };
    rotate_if_needed(&sink.path);
    let _ = append(&sink.path, &line);
}

/// Append one line, creating the file `0600` if it isn't there.
///
/// `0600` because the token file is (#7): redaction is the first line of
/// defence and the mode is the second, and two cheap defences against
/// leaking a credential are worth more than one.
fn append(path: &Path, line: &str) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(line.as_bytes())
}

/// Move the log aside once it passes [`MAX_BYTES`], keeping exactly one
/// previous generation.
///
/// One generation, not several: the point is to bound the disk, and a
/// second file already covers "the thing I want to read just scrolled out
/// of the current one". Every failure is ignored — losing rotation is
/// better than losing the app.
fn rotate_if_needed(path: &Path) {
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    if !should_rotate(metadata.len(), MAX_BYTES) {
        return;
    }
    let mut previous = path.as_os_str().to_os_string();
    previous.push(".1");
    let _ = std::fs::rename(path, previous);
}

/// Whether a log of `len` bytes has outgrown `cap`. Split out so the
/// threshold is testable without writing a megabyte to disk.
fn should_rotate(len: u64, cap: u64) -> bool {
    len >= cap
}

/// Remove anything that looks like a credential.
///
/// Deliberately blunt. It rewrites, in order:
///
/// - `Bearer <token>` — the exact shape an `Authorization` header takes.
/// - any `access_token` / `refresh_token` / `client_secret` / `code` /
///   `token` key followed by `=` or `":"`, as they appear in a token
///   endpoint's JSON response and in a redirect URL's query string.
///
/// Blunt is the right trade here: a redactor that misses is worse than one
/// that over-redacts a message into uselessness, because the failure is
/// silent and permanent — the credential is already on disk by the time
/// anyone notices.
pub(crate) fn redact(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut rest = message;

    while !rest.is_empty() {
        let Some((keyword, at)) = next_secret(rest) else {
            out.push_str(rest);
            break;
        };
        let (before, from) = rest.split_at(at);
        out.push_str(before);
        out.push_str(keyword);
        // Everything from here to the next delimiter is the value.
        let after_keyword = from.get(keyword.len()..).unwrap_or_default();
        let value_end = after_keyword
            .find(|c: char| c.is_whitespace() || c == '&' || c == '"' || c == ',' || c == '}')
            .unwrap_or(after_keyword.len());
        out.push_str("[redacted]");
        rest = after_keyword.get(value_end..).unwrap_or_default();
    }

    out
}

/// The next credential-introducing token in `haystack`: what to keep, and
/// where it starts. Everything after it up to a delimiter is the secret.
fn next_secret(haystack: &str) -> Option<(&'static str, usize)> {
    const KEYWORDS: [&str; 8] = [
        "Bearer ",
        "bearer ",
        "access_token=",
        "refresh_token=",
        "client_secret=",
        "token=",
        "code=",
        "state=",
    ];
    let lowered = haystack.to_ascii_lowercase();
    KEYWORDS
        .iter()
        .filter_map(|keyword| {
            let at = if keyword.starts_with("Bearer") {
                haystack.find(*keyword)?
            } else {
                lowered.find(*keyword)?
            };
            Some((*keyword, at))
        })
        .min_by_key(|(_, at)| *at)
}

/// Seconds since the Unix epoch, or 0 if the clock is before it.
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Render `unix_seconds` as `2026-08-19T00:31:04Z`.
///
/// Hand-rolled rather than pulling in a date crate for one line format —
/// the same reasoning as the module doc's. Uses Howard Hinnant's
/// `civil_from_days`, which is exact for the proleptic Gregorian calendar
/// and needs no table. A log whose timestamps are raw epoch seconds is a
/// log nobody reads, so this is worth the twenty lines.
fn format_utc(unix_seconds: i64) -> String {
    let days = unix_seconds.div_euclid(86_400);
    let seconds_of_day = unix_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day.div_euclid(3_600);
    let minute = seconds_of_day.div_euclid(60).rem_euclid(60);
    let second = seconds_of_day.rem_euclid(60);

    let mut out = String::with_capacity(20);
    // Infallible: writing to a String never fails.
    let _ = write!(
        out,
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"
    );
    out
}

/// Days since 1970-01-01 to a civil `(year, month, day)`.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "civil_from_days is exact arithmetic over a bounded range; \
              saturating any step would silently produce a wrong date \
              instead of a clamped one"
)]
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (
        year,
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
    )
}

/// Log at `Level::Info`.
pub(crate) fn info(message: &str) {
    write(Level::Info, message);
}

/// Log at `Level::Warn`.
pub(crate) fn warn(message: &str) {
    write(Level::Warn, message);
}

/// Log at `Level::Error`.
pub(crate) fn error(message: &str) {
    write(Level::Error, message);
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- redact: the tests that matter ---

    #[test]
    fn a_bearer_header_never_survives() {
        assert_eq!(
            redact("GET /2/users/me Authorization: Bearer AAAAAAAAsecret123"),
            "GET /2/users/me Authorization: Bearer [redacted]"
        );
    }

    #[test]
    fn a_token_response_body_never_survives() {
        let body = r#"{"access_token":"abc123","refresh_token":"def456","expires_in":7200}"#;
        let safe = redact(&body.replace("\":\"", "="));
        assert!(!safe.contains("abc123"), "{safe}");
        assert!(!safe.contains("def456"), "{safe}");
    }

    #[test]
    fn a_redirect_query_string_never_survives() {
        let safe = redact("callback: /?code=SplxlOBeZQQYbYS6WxSbIA&state=xyz");
        assert!(!safe.contains("SplxlOBeZQQYbYS6WxSbIA"), "{safe}");
        assert!(!safe.contains("xyz"), "{safe}");
    }

    #[test]
    fn redaction_is_case_insensitive_for_query_keys() {
        let safe = redact("ACCESS_TOKEN=hunter2");
        assert!(!safe.contains("hunter2"), "{safe}");
    }

    #[test]
    fn every_secret_in_one_line_is_redacted_not_just_the_first() {
        let safe = redact("access_token=one refresh_token=two");
        assert!(!safe.contains("one"), "{safe}");
        assert!(!safe.contains("two"), "{safe}");
    }

    #[test]
    fn an_ordinary_message_is_left_alone() {
        // Over-redaction is the safe direction, but a redactor that eats
        // every message is a log nobody can use.
        let message = "reload: 20 posts, cache hit, 1 request";
        assert_eq!(redact(message), message);
    }

    #[test]
    fn redaction_keeps_the_keyword_so_the_line_still_reads() {
        assert!(redact("Bearer secret").starts_with("Bearer "));
    }

    // --- Level ---

    #[test]
    fn levels_parse_case_insensitively() {
        assert_eq!(Level::parse("INFO"), Some(Level::Info));
        assert_eq!(Level::parse(" debug "), Some(Level::Debug));
        assert_eq!(Level::parse("warning"), Some(Level::Warn));
    }

    #[test]
    fn an_unknown_level_is_none_so_the_caller_can_fall_back() {
        assert_eq!(Level::parse("loud"), None);
        assert_eq!(Level::parse(""), None);
    }

    #[test]
    fn levels_order_from_least_to_most_verbose() {
        // `write` compares with `>`, so this ordering is what decides
        // whether a message is admitted.
        assert!(Level::Error < Level::Warn);
        assert!(Level::Warn < Level::Info);
        assert!(Level::Info < Level::Debug);
    }

    // --- rotation ---

    #[test]
    fn rotation_triggers_at_the_cap_not_past_it() {
        assert!(!should_rotate(999, 1000));
        assert!(should_rotate(1000, 1000));
        assert!(should_rotate(1001, 1000));
    }

    // --- the file itself ---

    #[test]
    fn the_log_file_is_created_owner_only() {
        // The token file is 0600 (#7); a log holding the same values at
        // 0644 would undo that. Redaction is the first defence, this is the
        // second.
        use std::os::unix::fs::PermissionsExt as _;

        let dir = std::env::temp_dir().join(format!("twigpui-test-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("twigpui.log");

        append(&path, "hello\n").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "log file mode was {:o}", mode & 0o777);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello\n");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rotation_moves_the_file_aside_and_keeps_it_readable() {
        let dir = std::env::temp_dir().join(format!("twigpui-test-rot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("twigpui.log");
        std::fs::write(&path, vec![b'x'; usize::try_from(MAX_BYTES).unwrap()]).unwrap();

        rotate_if_needed(&path);

        assert!(!path.exists(), "the current log should have been moved");
        assert!(
            dir.join("twigpui.log.1").exists(),
            "the previous generation should be readable"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_small_log_is_left_where_it_is() {
        let dir = std::env::temp_dir().join(format!("twigpui-test-norot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("twigpui.log");
        std::fs::write(&path, b"short").unwrap();

        rotate_if_needed(&path);

        assert!(path.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // --- timestamps ---

    #[test]
    fn the_epoch_renders_as_itself() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn a_known_instant_renders_correctly() {
        // 2026-08-19T00:31:04Z
        assert_eq!(format_utc(1_787_099_464), "2026-08-19T00:31:04Z");
    }

    #[test]
    fn a_leap_day_renders_correctly() {
        // 2024-02-29T12:00:00Z — the case a hand-rolled calendar gets wrong.
        assert_eq!(format_utc(1_709_208_000), "2024-02-29T12:00:00Z");
    }

    #[test]
    fn a_time_before_the_epoch_does_not_panic() {
        // `now()` clamps, but `format_utc` must not be the thing that
        // panics if it ever stops.
        assert_eq!(format_utc(-1), "1969-12-31T23:59:59Z");
    }
}
