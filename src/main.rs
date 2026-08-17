// `unwrap` in a test is a legible assertion, not a lurking panic — the strict
// lints in Cargo.toml are aimed at the code that actually ships.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod config;
mod paths;
mod ui;
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

    // Headless fetch, for checking credentials and connectivity without a window.
    if std::env::args().any(|arg| arg == "--fetch-only") {
        std::process::exit(fetch_only(&config));
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
            cx.new(|cx| ui::TimelineView::new(config, window, cx))
        });
        if let Err(error) = opened {
            eprintln!("could not open the window: {error:#}");
            cx.quit();
            return;
        }

        cx.activate(true);
    });
}

fn fetch_only(config: &config::Config) -> i32 {
    let client = x_api::XClient::new(config.bearer_token.clone());
    match client.user_timeline(&config.target_username, config.max_results) {
        Ok(items) => {
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
