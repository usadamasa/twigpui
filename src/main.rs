//! twigpui — a development-only X (Twitter) timeline viewer for macOS,
//! built with gpui.
//!
//! This binary crate is the whole application: `ui` renders the window,
//! `menu` holds the key bindings and the menu bar, `x_api` talks to X,
//! `cache`/`usage`/`rate_limit` keep the per-request billing under control,
//! and this module is the entry point plus the headless `--fetch-only` /
//! `--fetch-post` / `--usage` paths.

// `unwrap` in a test is a legible assertion, not a lurking panic — the strict
// lints in Cargo.toml are aimed at the code that actually ships. #47 extends
// the same reasoning to three more: indexing a fixture by a literal index,
// slicing a literal string, and `panic!` in a `match` arm that must not be
// reached are all *assertions* in a test. A test that panics is a test that
// failed, which is the mechanism working — not a lurking crash on remote
// input, which is what these lints exist to find in `src/`.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic
    )
)]

mod avatar;
mod browser;
mod cache;
mod compose;
mod config;
mod image_cache;
mod like;
mod log;
mod menu;
mod oauth;
mod paths;
mod rate_limit;
mod repost;
mod theme;
mod thread;
mod toggle;
mod ui;
mod usage;
mod x_api;

use std::collections::HashSet;
use std::io::IsTerminal as _;

use gpui::{
    AppContext as _, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size,
};

fn main() {
    let config = match config::Config::from_env() {
        Ok(config) => config,
        Err(error) => {
            report_startup_error(&format!("{error:#}"));
            std::process::exit(1);
        }
    };
    // `Config::from_env` already resolved and created these directories —
    // recomputing here (cheap, pure over env vars) avoids threading a
    // `Paths` through `Config` just for the OAuth token store's sake.
    let paths = match paths::Paths::from_env() {
        Ok(paths) => paths,
        Err(error) => {
            report_startup_error(&format!("{error:#}"));
            std::process::exit(1);
        }
    };

    // Headless fetch, for checking credentials and connectivity without a window.
    if std::env::args().any(|arg| arg == "--fetch-only") {
        std::process::exit(fetch_only(&config, &paths));
    }

    // Headless single-post lookup (#42): `--fetch-post <id-or-url>[,...]`.
    // Collected once into a `Vec` (rather than `std::env::args().any(...)`,
    // like the boolean flags above) since this flag needs the value that
    // follows it, not just whether it's present.
    let args: Vec<String> = std::env::args().collect();
    match fetch_post_arg(&args, "--fetch-post") {
        FetchPostArg::Absent => {}
        FetchPostArg::Value(arg) => std::process::exit(fetch_post(&config, &paths, arg)),
        FetchPostArg::MissingValue => {
            eprintln!(
                "--fetch-post requires a value: a post id, a status URL \
                 (https://x.com/<user>/status/<id>), or a comma-separated \
                 list of either."
            );
            std::process::exit(1);
        }
    }

    // Print the same usage numbers the header shows, as JSON (#18). Reads
    // only `usage.json` under `state_dir` — no network call, so this is
    // safe to run at any time, including while credits are exhausted.
    if std::env::args().any(|arg| arg == "--usage") {
        std::process::exit(usage_only(&config, &paths));
    }

    // #49: from here on, anything worth knowing goes to the log file as
    // well as stderr — which is the only record at all for a `.app`
    // launched from Finder, where stderr goes nowhere (#40, #45).
    log::init(&paths, config.log_level);
    log::info("starting twigpui");

    Application::new().run(move |cx| {
        // #38: registers gpui-component's global keybindings, theme, and
        // other per-App state (see its own `init`'s doc) — required once,
        // before any of its widgets (the composer's text input) can be
        // constructed.
        gpui_component::init(cx);
        // #58: twigpui's own key bindings, registered alongside
        // gpui-component's for the same reason — once, before the window
        // that dispatches to them exists.
        menu::init(cx);
        // #99: the menu bar, and the one action behind it that the window
        // cannot own. A menu item dispatches into the focused window, so
        // Reload/New Post/Submit Post reach the timeline's own handlers —
        // but quitting has to work with no window focused at all, which is
        // what `App::on_action` registers and a handler on the window's
        // root would not. Both run before the window opens: an app whose
        // window fails to open (below) still has a menu bar.
        cx.on_action(|_: &menu::Quit, cx| cx.quit());
        cx.set_menus(menu::menus());

        let bounds = Bounds::centered(None, size(px(560.0), px(820.0)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("twigpui".into()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let opened = cx.open_window(options, |window, cx| {
            let timeline = cx.new(|cx| ui::TimelineView::new(config, paths, window, cx));
            // #38: gpui-component's widgets reach back up to the window's
            // root expecting to find its `Root` there — its text input asks
            // for it on the very first render, and `Root::read` panics
            // outright if the root view is anything else. Making the
            // timeline the root directly aborted the app at startup.
            cx.new(|cx| gpui_component::Root::new(timeline, window, cx))
        });
        if let Err(error) = opened {
            log::error(&format!("could not open the window: {error:#}"));
            cx.quit();
            return;
        }

        cx.activate(true);
    });
}

/// Report a fatal startup error both to stderr and — when there is no
/// terminal attached to read stderr from — as a native alert dialog.
///
/// A plain `eprintln!` is enough for `cargo run`, but a `.app` launched from
/// Finder/Spotlight/Dock (#40) has no terminal at all: stderr goes nowhere
/// anyone will see, so the process would otherwise just vanish with no
/// visible symptom, which is exactly the "unexplained blank window" failure
/// #40 calls out. `osascript` is used rather than a `gpui` window because
/// this runs *before* `Application::new()` — there is no window server
/// connection yet to hang a `gpui` alert off of, but `osascript` needs
/// nothing from this process beyond being a normal macOS app.
///
/// The message always names where `config.toml` belongs, since that's the
/// concrete fix for the most common cause (no credential configured at
/// all) — see the README's "Setup" and "`config.toml`" sections.
fn report_startup_error(message: &str) {
    let config_hint = paths::Paths::from_env().map_or_else(
        |_| "~/.config/twigpui/config.toml".to_string(),
        |paths| paths.settings_file().display().to_string(),
    );
    let full_message = format!(
        "twigpui could not start: {message} Configuration lives in \
         {config_hint} (non-secret settings, e.g. oauth_client_id) or the \
         X_OAUTH_CLIENT_ID environment variable — see the \
         README's Setup section."
    );
    eprintln!("configuration error: {full_message}");

    if std::io::stderr().is_terminal() {
        // A terminal is attached (a `cargo run` / direct-binary launch) —
        // the eprintln! above is already visible, so a dialog on top would
        // just be noise.
        return;
    }

    let script = format!(
        "display alert \"twigpui\" message {} as critical",
        applescript_quote(&full_message)
    );
    // Best-effort: if `osascript` itself is missing or fails, there is
    // nothing more this process can do to surface the error, and it must
    // still exit non-zero rather than hang waiting on a dialog that can't
    // appear.
    let _ = std::process::Command::new("osascript")
        .arg("-e")
        .arg(script)
        .status();
}

/// Escape `text` for use as a double-quoted `AppleScript` string literal.
/// `AppleScript` has no `\n` escape and a raw line break inside a string
/// literal is a syntax error, so embedded newlines are flattened to spaces
/// rather than escaped.
fn applescript_quote(text: &str) -> String {
    let flattened = text.replace(['\n', '\r'], " ");
    let escaped = flattened.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Resolve a token the same way the window does at startup, but without ever
/// opening a browser — `--fetch-only` is meant to run headless, e.g. in a
/// script checking whether credentials are still valid.
///
/// Unlike opening the window, this always spends at least one API request:
/// its whole point per the README is checking that the credential and
/// connectivity actually work, which a cache-only render can't verify. It
/// still goes through `cache::reload` rather than a bare fetch, so a cached
/// user id turns that into one request instead of two, and an incremental
/// `since_id` keeps the response small — see the eprintln! below for which
/// happened.
fn fetch_only(config: &config::Config, paths: &paths::Paths) -> i32 {
    let resolution = match oauth::resolve_credential(config, paths, oauth::unix_now()) {
        Ok(resolution) => resolution,
        Err(error) => {
            eprintln!("could not resolve a credential: {error:#}");
            return 1;
        }
    };
    // #54: a stored session that couldn't be refreshed is worth saying out
    // loud here too — headless runs have no header banner to show it in
    // instead.
    if let Some(demotion) = &resolution.demotion {
        eprintln!("{}", oauth::describe_demotion(demotion));
    }
    let Some(credential) = resolution.credential else {
        // #33: signing in is the only way to get a credential now, and it
        // needs a browser — which a headless run has no business opening.
        eprintln!(
            "no signed-in session is available. Run twigpui without --fetch-only and click \
             \"Sign in with X\" once; this flag reuses the session that leaves behind."
        );
        return 1;
    };

    let client = x_api::XClient::new(credential.token);
    match cache::reload(
        paths,
        &client,
        &config.target_username,
        config.max_results,
        oauth::unix_now(),
    ) {
        Ok(cache::Reloaded {
            items,
            user_id_cache_hit,
        }) => {
            eprintln!(
                "cache: user id {} ({} request{} spent)",
                if user_id_cache_hit {
                    "cache hit"
                } else {
                    "cache miss, resolved via the API"
                },
                if user_id_cache_hit { 1 } else { 2 },
                if user_id_cache_hit { "" } else { "s" }
            );
            println!("{} post(s) from @{}", items.len(), config.target_username);
            for item in &items {
                println!(
                    "\n[{}] {} (@{})\n{}",
                    item.created_at.as_deref().unwrap_or("-"),
                    item.author_name,
                    item.author_username,
                    item.text
                );
            }
            0
        }
        Err(error) => {
            eprintln!("fetch failed: {error:#}");
            1
        }
    }
}

/// The three states looking for `--fetch-post` (#42) in argv can land in:
/// the flag never appeared, it appeared with nothing after it — e.g.
/// `twigpui --fetch-post` as the very last argument — or it appeared with a
/// value. A named enum rather than `Option<Option<&str>>`: clippy's own
/// `option_option` lint rejects the nested form precisely because three
/// states read more clearly as three variants than as `None` /
/// `Some(None)` / `Some(Some(_))`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FetchPostArg<'a> {
    Absent,
    MissingValue,
    Value(&'a str),
}

/// Locate `flag` in `args` and classify what follows it as a
/// [`FetchPostArg`]. Generic over the flag name only so a test can
/// exercise it without depending on the literal `--fetch-post` string;
/// nothing else in this crate calls it with a different flag.
fn fetch_post_arg<'a>(args: &'a [String], flag: &str) -> FetchPostArg<'a> {
    let Some(index) = args.iter().position(|arg| arg == flag) else {
        return FetchPostArg::Absent;
    };
    // `saturating_add` (#47): argv cannot be `usize::MAX` long, but the
    // saturating form makes that a lookup that finds nothing rather than
    // an overflow.
    match args.get(index.saturating_add(1)) {
        Some(value) => FetchPostArg::Value(value.as_str()),
        None => FetchPostArg::MissingValue,
    }
}

/// Pull a post id out of one `--fetch-post` (#42) token: either the id
/// itself (all-digit, once trimmed), or a status URL's id segment —
/// `.../status/<id>`, followed by anything that isn't itself a digit
/// (`/photo/1`, a `?s=...` query string, or nothing). Works for both
/// `x.com` and `twitter.com` links, since both share the same
/// `/status/<id>` path shape; the scheme and host are never checked. This
/// only shapes which token is even worth sending to the API — it doesn't
/// validate that the id is real, which the request itself is the actual
/// check for. `None` for anything else, including empty input (e.g. a
/// stray comma in the argument).
fn extract_post_id(token: &str) -> Option<String> {
    let trimmed = token.trim();
    if !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_digit()) {
        return Some(trimmed.to_string());
    }
    let after_marker = trimmed.split("/status/").nth(1)?;
    let id: String = after_marker
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    if id.is_empty() { None } else { Some(id) }
}

/// Parse `--fetch-post`'s (#42) argument into the post ids it names — a
/// bare id, a full status URL, or a comma-separated mix of either, per the
/// issue's own "決めること" table choosing to accept both rather than ids
/// only (a link is what's actually at hand most of the time). Each token is
/// resolved by [`extract_post_id`]; the first one that isn't recognizable
/// fails the whole parse rather than being silently dropped, since a
/// shorter-than-expected id list would make the single request this fires
/// off cover fewer posts than asked for, with no indication why.
fn parse_post_ids(arg: &str) -> Result<Vec<String>, String> {
    arg.split(',')
        .map(|token| {
            extract_post_id(token).ok_or_else(|| format!("could not find a post id in {token:?}"))
        })
        .collect()
}

/// Which of `requested` never showed up in `items` — deleted, protected, or
/// otherwise absent from the API's response — in `requested`'s own order.
/// Feeds only the "N of M found" line `fetch_post` prints to stderr; stdout
/// always prints exactly what came back, with no placeholder entries for
/// the rest.
fn missing_ids(requested: &[String], items: &[x_api::TimelineItem]) -> Vec<String> {
    let present: HashSet<&str> = items.iter().map(|item| item.id.as_str()).collect();
    requested
        .iter()
        .filter(|id| !present.contains(id.as_str()))
        .cloned()
        .collect()
}

/// Render fetched posts as pretty-printed JSON for `--fetch-post` (#42)'s
/// stdout — the only output format; see `fetch_post`'s doc comment for why
/// no human-readable mode was added alongside it. A thin wrapper around
/// `serde_json::to_string_pretty`, pulled out on its own so a test can
/// check the shape of what gets printed without going through
/// `fetch_post`'s network call, the same reasoning `usage_only` below
/// already applies to its own JSON output.
fn render_fetch_post_json(items: &[x_api::TimelineItem]) -> serde_json::Result<String> {
    serde_json::to_string_pretty(items)
}

/// Fetch one or more posts by id and print them as JSON to stdout, for
/// `--fetch-post <id-or-url>[,...]` (#42) — e.g. so a Claude Code session
/// can read a specific post's text without a human pasting it in, since
/// `x.com` itself returns 402 to `WebFetch` (the issue's own motivation).
///
/// Goes through [`x_api::XClient::tweets_by_id`] — the same call
/// `cache::fetch_thread`'s parent-chain walk uses (#12) — passed every
/// requested id joined into one comma-separated `ids=` value rather than
/// one request per id. `GET /2/tweets?ids=` already accepts a
/// comma-separated list on the wire, so this reuses that call unmodified:
/// fetching five ids still costs exactly one request, tracked under the
/// same `Endpoint::TweetById` (#10) as `cache::fetch_thread` and counted by
/// #18's usage tracking automatically, since both go through
/// [`x_api::XClient::get`] the same way every other read does.
///
/// **Deliberately bypasses the timeline cache (#9) entirely — reads
/// nothing from it, writes nothing to it.** That cache exists to avoid
/// re-fetching the same account's timeline on every reload, a
/// repeated-access pattern an arbitrary post id doesn't share: it is
/// typically looked up once, from wherever it was linked, and never again.
/// Caching it would mean either a new file keyed by id with no eviction
/// story, or shoehorning an unrelated post into the per-account timeline
/// file it has no relationship to. The simplest defensible choice is to
/// always spend the one request this costs and never persist the result.
///
/// Always spends exactly one request regardless of how many ids are
/// requested (see the `tweets_by_id` reasoning above), reported on stderr
/// alongside how many of the requested ids actually came back — mirroring
/// `fetch_only`'s cache-hit/miss line — so the cost and the yield are never
/// ambiguous, which is the issue's own completion condition. Only JSON is
/// printed to stdout: the issue's motivation is a tool reading this output,
/// not a human at a terminal, the same choice `--usage` already made below
/// for the same reason, so no separate `--json` flag or human-readable
/// default was added.
fn fetch_post(config: &config::Config, paths: &paths::Paths, arg: &str) -> i32 {
    let ids = match parse_post_ids(arg) {
        Ok(ids) => ids,
        Err(message) => {
            eprintln!("--fetch-post: {message}");
            return 1;
        }
    };

    let resolution = match oauth::resolve_credential(config, paths, oauth::unix_now()) {
        Ok(resolution) => resolution,
        Err(error) => {
            eprintln!("could not resolve a credential: {error:#}");
            return 1;
        }
    };
    // #54: a stored session that couldn't be refreshed is worth saying out
    // loud here too — headless runs have no header banner to show it in
    // instead.
    if let Some(demotion) = &resolution.demotion {
        eprintln!("{}", oauth::describe_demotion(demotion));
    }
    let Some(credential) = resolution.credential else {
        // See `fetch_only`'s equivalent: signing in needs a browser (#33).
        eprintln!(
            "no signed-in session is available. Run twigpui without --fetch-post and click \
             \"Sign in with X\" once; this flag reuses the session that leaves behind."
        );
        return 1;
    };

    let client = x_api::XClient::new(credential.token);
    let joined_ids = ids.join(",");
    match client.tweets_by_id(paths, &joined_ids, oauth::unix_now()) {
        Ok(items) => {
            let missing = missing_ids(&ids, &items);
            let missing_note = if missing.is_empty() {
                String::new()
            } else {
                format!(" (missing: {})", missing.join(", "))
            };
            eprintln!(
                "1 API request spent, {} of {} post(s) found{missing_note}",
                items.len(),
                ids.len(),
            );
            match render_fetch_post_json(&items) {
                Ok(json) => {
                    println!("{json}");
                    0
                }
                Err(error) => {
                    eprintln!("could not serialize the fetched post(s): {error}");
                    1
                }
            }
        }
        Err(error) => {
            eprintln!("fetch failed: {error:#}");
            1
        }
    }
}

/// Print `usage::build_report`'s numbers as JSON to stdout (#18) — the
/// point of persisting request counts under `state_dir` in the first place
/// is that an external tool can read the same numbers the header shows,
/// without opening a window. JSON rather than a bespoke text format: the
/// project already depends on `serde_json` for everything else it
/// persists, and a machine-readable consumer needs structure to parse
/// rather than a format it has to scrape.
fn usage_only(config: &config::Config, paths: &paths::Paths) -> i32 {
    let entries = match usage::load_all(paths) {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!("could not read usage data: {error:#}");
            return 1;
        }
    };

    let report = usage::build_report(
        &entries,
        oauth::unix_now(),
        config.request_price,
        config.daily_request_budget,
    );

    match serde_json::to_string_pretty(&report) {
        Ok(json) => {
            println!("{json}");
            0
        }
        Err(error) => {
            eprintln!("could not serialize the usage report: {error:#}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applescript_quote_escapes_backslashes_and_quotes() {
        assert_eq!(
            applescript_quote(r#"say "hi" \ bye"#),
            r#""say \"hi\" \\ bye""#
        );
    }

    #[test]
    fn applescript_quote_flattens_embedded_newlines() {
        assert_eq!(
            applescript_quote("line one\nline two"),
            "\"line one line two\""
        );
    }

    // --- #42: --fetch-post argument parsing ---

    #[test]
    fn fetch_post_arg_is_absent_when_the_flag_is_absent() {
        let args: Vec<String> = ["twigpui", "--fetch-only"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(fetch_post_arg(&args, "--fetch-post"), FetchPostArg::Absent);
    }

    #[test]
    fn fetch_post_arg_is_missing_value_when_the_flag_has_no_value() {
        let args: Vec<String> = ["twigpui", "--fetch-post"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(
            fetch_post_arg(&args, "--fetch-post"),
            FetchPostArg::MissingValue
        );
    }

    #[test]
    fn fetch_post_arg_returns_the_following_argument() {
        let args: Vec<String> = ["twigpui", "--fetch-post", "123,456"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(
            fetch_post_arg(&args, "--fetch-post"),
            FetchPostArg::Value("123,456")
        );
    }

    #[test]
    fn extract_post_id_accepts_a_bare_id() {
        assert_eq!(
            extract_post_id("1700000000000000001"),
            Some("1700000000000000001".to_string())
        );
    }

    #[test]
    fn extract_post_id_trims_surrounding_whitespace() {
        assert_eq!(
            extract_post_id("  1700000000000000001  "),
            Some("1700000000000000001".to_string())
        );
    }

    #[test]
    fn extract_post_id_reads_the_id_out_of_an_x_status_url() {
        assert_eq!(
            extract_post_id("https://x.com/jack/status/20"),
            Some("20".to_string())
        );
    }

    #[test]
    fn extract_post_id_reads_the_id_out_of_a_twitter_status_url() {
        assert_eq!(
            extract_post_id("https://twitter.com/jack/status/20"),
            Some("20".to_string())
        );
    }

    #[test]
    fn extract_post_id_stops_at_a_trailing_path_segment() {
        assert_eq!(
            extract_post_id("https://x.com/jack/status/20/photo/1"),
            Some("20".to_string())
        );
    }

    #[test]
    fn extract_post_id_stops_at_a_trailing_query_string() {
        assert_eq!(
            extract_post_id("https://x.com/jack/status/20?s=20"),
            Some("20".to_string())
        );
    }

    #[test]
    fn extract_post_id_rejects_neither_an_id_nor_a_status_url() {
        assert_eq!(extract_post_id("not-a-post"), None);
    }

    #[test]
    fn extract_post_id_rejects_empty_input() {
        assert_eq!(extract_post_id(""), None);
        assert_eq!(extract_post_id("   "), None);
    }

    #[test]
    fn parse_post_ids_accepts_a_single_id() {
        assert_eq!(parse_post_ids("20"), Ok(vec!["20".to_string()]));
    }

    #[test]
    fn parse_post_ids_splits_a_comma_separated_list() {
        assert_eq!(
            parse_post_ids("20,30"),
            Ok(vec!["20".to_string(), "30".to_string()])
        );
    }

    #[test]
    fn parse_post_ids_accepts_a_mix_of_ids_and_urls_with_whitespace() {
        assert_eq!(
            parse_post_ids(" 20 , https://x.com/jack/status/30 "),
            Ok(vec!["20".to_string(), "30".to_string()])
        );
    }

    #[test]
    fn parse_post_ids_rejects_an_empty_argument() {
        assert!(parse_post_ids("").is_err());
    }

    #[test]
    fn parse_post_ids_rejects_a_token_that_is_neither_an_id_nor_a_url() {
        assert!(parse_post_ids("20,not-a-post").is_err());
    }

    fn item(id: &str) -> x_api::TimelineItem {
        x_api::TimelineItem {
            id: id.to_string(),
            text: format!("text of {id}"),
            created_at: None,
            author_name: format!("Author {id}"),
            author_username: format!("author{id}"),
            reposted_by: None,
            quoted: None,
            replied_to: None,
            metrics: None,
            links: Vec::new(),
            author_avatar_url: None,
            original_post_id: None,
            media: Vec::new(),
        }
    }

    #[test]
    fn missing_ids_is_empty_when_every_requested_id_came_back() {
        let requested = vec!["1".to_string(), "2".to_string()];
        let items = vec![item("1"), item("2")];
        assert!(missing_ids(&requested, &items).is_empty());
    }

    #[test]
    fn missing_ids_reports_ids_absent_from_the_response() {
        let requested = vec!["1".to_string(), "2".to_string(), "3".to_string()];
        let items = vec![item("1"), item("3")];
        assert_eq!(missing_ids(&requested, &items), vec!["2".to_string()]);
    }

    #[test]
    fn render_fetch_post_json_prints_the_fetched_posts_as_a_json_array() {
        let items = vec![item("1")];
        let json = render_fetch_post_json(&items).unwrap();
        assert!(json.contains("\"id\": \"1\""));
        assert!(json.contains("\"text\": \"text of 1\""));
        assert!(json.contains("\"author_username\": \"author1\""));
    }

    #[test]
    fn render_fetch_post_json_prints_an_empty_array_for_no_posts() {
        let items: Vec<x_api::TimelineItem> = Vec::new();
        assert_eq!(render_fetch_post_json(&items).unwrap(), "[]");
    }
}
