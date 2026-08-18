// `unwrap` in a test is a legible assertion, not a lurking panic — the strict
// lints in Cargo.toml are aimed at the code that actually ships.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod cache;
mod compose;
mod config;
mod oauth;
mod paths;
mod rate_limit;
mod repost;
mod theme;
mod thread;
mod ui;
mod usage;
mod x_api;

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

    // Print the same usage numbers the header shows, as JSON (#18). Reads
    // only `usage.json` under `state_dir` — no network call, so this is
    // safe to run at any time, including while credits are exhausted.
    if std::env::args().any(|arg| arg == "--usage") {
        std::process::exit(usage_only(&config, &paths));
    }

    Application::new().run(move |cx| {
        // #38: registers gpui-component's global keybindings, theme, and
        // other per-App state (see its own `init`'s doc) — required once,
        // before any of its widgets (the composer's text input) can be
        // constructed.
        gpui_component::init(cx);

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
            eprintln!("could not open the window: {error:#}");
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
         X_BEARER_TOKEN / X_OAUTH_CLIENT_ID environment variables — see the \
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
        eprintln!("{}", oauth::describe_demotion(demotion, paths));
    }
    let Some(credential) = resolution.credential else {
        eprintln!(
            "no credential is available. Run twigpui without --fetch-only and use \
             \"Sign in with X\", or set X_BEARER_TOKEN."
        );
        return 1;
    };

    if !credential.is_oauth() {
        // Worth saying out loud: several endpoints this project is heading
        // for (#11, #14–#17) reject an app-only token outright, so a
        // headless run succeeding here does not mean the user context works.
        eprintln!("credential: app-only bearer token (not a signed-in OAuth session)");
    }

    let client = x_api::XClient::new(credential.token().to_string());
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
    use super::applescript_quote;

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
}
