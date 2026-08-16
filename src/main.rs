mod config;
mod ui;
mod x_api;

use gpui::{Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};

fn main() {
    let config = match config::Config::from_env() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("configuration error: {error:#}");
            std::process::exit(1);
        }
    };

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

        cx.open_window(options, |window, cx| {
            cx.new(|cx| ui::TimelineView::new(config, window, cx))
        })
        .expect("could not open the window");

        cx.activate(true);
    });
}
