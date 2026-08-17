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
