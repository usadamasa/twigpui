// `unwrap` in a test is a legible assertion, not a lurking panic — the strict
// lints in Cargo.toml are aimed at the code that actually ships.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod cache;
mod config;
mod oauth;
mod paths;
mod rate_limit;
mod theme;
mod thread;
mod ui;
mod usage;
mod x_api;

use gpui::{
    AppContext as _, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size,
};

fn main() {
    let config = match config::Config::from_env() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("configuration error: {error:#}");
            std::process::exit(1);
        }
    };
    // `Config::from_env` already resolved and created these directories —
    // recomputing here (cheap, pure over env vars) avoids threading a
    // `Paths` through `Config` just for the OAuth token store's sake.
    let paths = match paths::Paths::from_env() {
        Ok(paths) => paths,
        Err(error) => {
            eprintln!("configuration error: {error:#}");
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
            cx.new(|cx| ui::TimelineView::new(config, paths, window, cx))
        });
        if let Err(error) = opened {
            eprintln!("could not open the window: {error:#}");
            cx.quit();
            return;
        }

        cx.activate(true);
    });
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
    let credential = match oauth::resolve_credential(config, paths, oauth::unix_now()) {
        Ok(Some(credential)) => credential,
        Ok(None) => {
            eprintln!(
                "no credential is available. Run twigpui without --fetch-only and use \
                 \"Sign in with X\", or set X_BEARER_TOKEN."
            );
            return 1;
        }
        Err(error) => {
            eprintln!("could not resolve a credential: {error:#}");
            return 1;
        }
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
