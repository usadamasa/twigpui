use gpui::{Context, FontWeight, SharedString, Task, Window, div, prelude::*, rgb};

use crate::config::Config;
use crate::x_api::{TimelineItem, XClient};

// Grouped per RGB channel, which is also the digit grouping clippy asks for.
const BG: u32 = 0x15_20_2b;
const BG_HEADER: u32 = 0x1b_28_36;
const BORDER: u32 = 0x38_44_4d;
const TEXT: u32 = 0xf7_f9_f9;
const TEXT_MUTED: u32 = 0x88_99_a6;
const ACCENT: u32 = 0x1d_9b_f0;
const DANGER: u32 = 0xf4_21_2e;

enum TimelineState {
    Loading,
    Loaded(Vec<TimelineItem>),
    Failed(SharedString),
}

pub(crate) struct TimelineView {
    config: Config,
    client: XClient,
    state: TimelineState,
    /// Holding the task keeps the in-flight fetch alive; dropping it cancels.
    fetch: Option<Task<()>>,
}

impl TimelineView {
    pub(crate) fn new(config: Config, _window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let client = XClient::new(config.bearer_token.clone());
        let mut this = Self {
            config,
            client,
            state: TimelineState::Loading,
            fetch: None,
        };
        this.reload(cx);
        this
    }

    /// Every reload spends API credits, so this only runs on explicit action.
    fn reload(&mut self, cx: &mut Context<'_, Self>) {
        self.state = TimelineState::Loading;

        let client = self.client.clone();
        let username = self.config.target_username.clone();
        let max_results = self.config.max_results;

        self.fetch = Some(cx.spawn(async move |this, cx| {
            // The client blocks, so it must not run on the foreground thread.
            let result = cx
                .background_executor()
                .spawn(async move { client.user_timeline(&username, max_results) })
                .await;

            let _ = this.update(cx, |this, cx| {
                this.state = match result {
                    Ok(items) => TimelineState::Loaded(items),
                    Err(error) => TimelineState::Failed(format!("{error:#}").into()),
                };
                cx.notify();
            });
        }));

        cx.notify();
    }

    fn header(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let busy = matches!(self.state, TimelineState::Loading);

        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .px_4()
            .py_3()
            .bg(rgb(BG_HEADER))
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .font_weight(FontWeight::BOLD)
                    .child(format!("@{}", self.config.target_username)),
            )
            .child(
                div()
                    .id("reload")
                    .px_3()
                    .py_1()
                    .rounded_full()
                    .bg(rgb(if busy { BORDER } else { ACCENT }))
                    .text_color(rgb(TEXT))
                    .child(if busy { "Loading…" } else { "Reload" })
                    .on_click(cx.listener(|this, _event, _window, cx| this.reload(cx))),
            )
    }

    fn body(&self) -> impl IntoElement {
        // `overflow_y_scroll` lives on StatefulInteractiveElement, so the
        // element needs an id before it can scroll.
        let content = div()
            .id("timeline")
            .flex()
            .flex_col()
            .flex_1()
            .overflow_y_scroll();

        match &self.state {
            TimelineState::Loading => content.child(notice("Fetching the timeline…", TEXT_MUTED)),
            TimelineState::Failed(message) => content.child(notice(message.clone(), DANGER)),
            TimelineState::Loaded(items) if items.is_empty() => {
                content.child(notice("No posts were returned.", TEXT_MUTED))
            }
            TimelineState::Loaded(items) => content.children(items.iter().map(post_row)),
        }
    }
}

impl Render for TimelineView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .text_sm()
            .child(self.header(cx))
            .child(self.body())
    }
}

fn notice(message: impl Into<SharedString>, color: u32) -> impl IntoElement {
    div()
        .px_4()
        .py_3()
        .text_color(rgb(color))
        .child(message.into())
}

/// `@name`, or nothing at all when the author was missing from the expansion —
/// a bare `@` would read as a broken row.
fn byline(author_username: &str) -> String {
    if author_username.is_empty() {
        String::new()
    } else {
        format!("@{author_username}")
    }
}

fn post_row(item: &TimelineItem) -> impl IntoElement {
    let byline = byline(&item.author_username);

    div()
        .flex()
        .flex_col()
        .gap_1()
        .px_4()
        .py_3()
        .border_b_1()
        .border_color(rgb(BORDER))
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    div()
                        .font_weight(FontWeight::BOLD)
                        .child(item.author_name.clone()),
                )
                .child(div().text_color(rgb(TEXT_MUTED)).child(byline))
                .child(
                    div()
                        .text_color(rgb(TEXT_MUTED))
                        .child(format_timestamp(item.created_at.as_deref())),
                ),
        )
        .child(div().child(item.text.clone()))
}

/// Turn `2026-08-16T09:00:00.000Z` into `2026-08-16 09:00`.
///
/// The API always returns UTC in RFC 3339, so slicing beats pulling in a date
/// library for a label this small.
fn format_timestamp(created_at: Option<&str>) -> String {
    let Some(raw) = created_at else {
        return String::new();
    };
    match raw.split_once('T') {
        Some((date, time)) if time.len() >= 5 => format!("{date} {}", &time[..5]),
        _ => raw.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{byline, format_timestamp};

    #[test]
    fn prefixes_a_byline_with_an_at_sign() {
        assert_eq!(byline("XDevelopers"), "@XDevelopers");
    }

    #[test]
    fn renders_a_missing_author_as_nothing_rather_than_a_bare_at() {
        assert_eq!(byline(""), "");
    }

    #[test]
    fn keeps_a_timestamp_too_short_to_slice() {
        // `&time[..5]` would panic here, so the guard has to hold.
        assert_eq!(format_timestamp(Some("2026-08-16T09")), "2026-08-16T09");
    }

    #[test]
    fn shortens_an_rfc3339_timestamp() {
        assert_eq!(
            format_timestamp(Some("2026-08-16T09:00:00.000Z")),
            "2026-08-16 09:00"
        );
    }

    #[test]
    fn passes_through_an_unexpected_shape() {
        assert_eq!(format_timestamp(Some("yesterday")), "yesterday");
    }

    #[test]
    fn renders_a_missing_timestamp_as_empty() {
        assert_eq!(format_timestamp(None), "");
    }
}
