use gpui::{Context, FontWeight, SharedString, Task, Window, div, prelude::*, rgb};

use crate::cache;
use crate::config::Config;
use crate::oauth;
use crate::paths::Paths;
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
    /// No usable credential yet: no fresh/refreshable stored OAuth session
    /// and no bearer token. Shown at startup before the sign-in flow runs.
    NotAuthenticated,
    /// The interactive "Sign in with X" flow is running — browser opened,
    /// waiting on the loopback callback.
    SigningIn,
    Loading,
    Loaded(Vec<TimelineItem>),
    Failed(SharedString),
}

/// What the header's primary button does, independent of its current label —
/// kept `Copy` so it can be captured into the click closure without
/// borrowing `self.state`.
#[derive(Clone, Copy)]
enum PrimaryAction {
    Reload,
    SignIn,
}

pub(crate) struct TimelineView {
    config: Config,
    paths: Paths,
    /// `None` until a credential is available — see [`TimelineState::NotAuthenticated`].
    client: Option<XClient>,
    state: TimelineState,
    /// Holding the task keeps the in-flight fetch (or startup credential
    /// resolution) alive; dropping it cancels.
    fetch: Option<Task<()>>,
    /// Holding this keeps the interactive sign-in flow alive; assigning a
    /// new one (a second click) drops and so cancels whatever was running,
    /// which also closes the loopback socket — see `oauth::callback`.
    sign_in_flow: Option<Task<()>>,
}

impl TimelineView {
    pub(crate) fn new(
        config: Config,
        paths: Paths,
        _window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let mut this = Self {
            config,
            paths,
            client: None,
            state: TimelineState::Loading,
            fetch: None,
            sign_in_flow: None,
        };
        this.start(cx);
        this
    }

    /// Resolve a credential (stored OAuth session, refreshing if stale, else
    /// the bearer token) before the very first fetch, and — since #9 — try
    /// to render straight from the local cache instead of always reloading.
    /// A cache hit means startup spends no API request at all; a miss falls
    /// through to [`Self::reload`], which does. Runs on the background
    /// executor because it can touch disk and, on a token refresh or cache
    /// miss, the network.
    fn start(&mut self, cx: &mut Context<'_, Self>) {
        self.state = TimelineState::Loading;

        let config = self.config.clone();
        let paths = self.paths.clone();

        self.fetch = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let token = oauth::resolve_access_token(&config, &paths, oauth::unix_now())?;
                    let Some(token) = token else {
                        return anyhow::Ok((None, None));
                    };
                    let cached = cache::startup(&paths, &config.target_username, oauth::unix_now())?;
                    anyhow::Ok((Some(token), cached))
                })
                .await;

            let _ = this.update(cx, |this, cx| match result {
                Ok((Some(token), Some(items))) => {
                    this.client = Some(XClient::new(token));
                    this.state = TimelineState::Loaded(items);
                    cx.notify();
                }
                Ok((Some(token), None)) => {
                    this.client = Some(XClient::new(token));
                    this.reload(cx);
                }
                Ok((None, _)) => {
                    this.state = TimelineState::NotAuthenticated;
                    cx.notify();
                }
                Err(error) => {
                    this.state = TimelineState::Failed(format!("{error:#}").into());
                    cx.notify();
                }
            });
        }));

        cx.notify();
    }

    /// Every reload spends API credits, so this only runs on explicit action.
    /// A no-op (falls back to [`TimelineState::NotAuthenticated`]) if called
    /// without a client — the "Reload" button isn't shown in that state, but
    /// this guards against it anyway rather than assuming the caller got it
    /// right. Goes through [`cache::reload`] rather than a bare fetch: a
    /// cached user id turns this into one request instead of two, and the
    /// result is merged into (and persisted to) the local cache rather than
    /// replacing it outright.
    fn reload(&mut self, cx: &mut Context<'_, Self>) {
        let Some(client) = self.client.clone() else {
            self.state = TimelineState::NotAuthenticated;
            cx.notify();
            return;
        };

        self.state = TimelineState::Loading;

        let paths = self.paths.clone();
        let username = self.config.target_username.clone();
        let max_results = self.config.max_results;

        self.fetch = Some(cx.spawn(async move |this, cx| {
            // The client blocks, so it must not run on the foreground thread.
            let result = cx
                .background_executor()
                .spawn(async move {
                    cache::reload(&paths, &client, &username, max_results, oauth::unix_now())
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                this.state = match result {
                    Ok(reloaded) => TimelineState::Loaded(reloaded.items),
                    Err(error) => TimelineState::Failed(format!("{error:#}").into()),
                };
                cx.notify();
            });
        }));

        cx.notify();
    }

    /// Run the interactive PKCE sign-in flow: open the browser, wait for the
    /// loopback callback, exchange the code, persist the tokens, then fall
    /// straight into [`Self::reload`].
    fn sign_in(&mut self, cx: &mut Context<'_, Self>) {
        let Some(client_id) = self.config.oauth_client_id.clone() else {
            self.state = TimelineState::Failed(
                "X_OAUTH_CLIENT_ID (or oauth_client_id in config.toml) is not set.".into(),
            );
            cx.notify();
            return;
        };

        self.state = TimelineState::SigningIn;
        let paths = self.paths.clone();

        self.sign_in_flow = Some(cx.spawn(async move |this, cx| {
            let executor = cx.background_executor().clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let tokens = oauth::sign_in(&executor, &client_id).await?;
                    oauth::tokens::save(&paths, &tokens)?;
                    anyhow::Ok(tokens)
                })
                .await;

            let _ = this.update(cx, |this, cx| match result {
                Ok(tokens) => {
                    this.client = Some(XClient::new(tokens.access_token));
                    this.reload(cx);
                }
                Err(error) => {
                    this.state = TimelineState::Failed(format!("{error:#}").into());
                    cx.notify();
                }
            });
        }));

        cx.notify();
    }

    fn header(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let (label, busy, action) = match self.state {
            TimelineState::Loading => ("Loading…", true, PrimaryAction::Reload),
            TimelineState::SigningIn => ("Signing in…", true, PrimaryAction::SignIn),
            TimelineState::NotAuthenticated => ("Sign in with X", false, PrimaryAction::SignIn),
            TimelineState::Loaded(_) | TimelineState::Failed(_) => {
                ("Reload", false, PrimaryAction::Reload)
            }
        };

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
                    .id("primary-action")
                    .px_3()
                    .py_1()
                    .rounded_full()
                    .bg(rgb(if busy { BORDER } else { ACCENT }))
                    .text_color(rgb(TEXT))
                    .child(label)
                    .on_click(cx.listener(move |this, _event, _window, cx| match action {
                        PrimaryAction::Reload => this.reload(cx),
                        PrimaryAction::SignIn => this.sign_in(cx),
                    })),
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
            TimelineState::NotAuthenticated => content.child(notice(
                "Not signed in. Click \"Sign in with X\" to continue.",
                TEXT_MUTED,
            )),
            TimelineState::SigningIn => content.child(notice(
                "Waiting for the browser to finish sign-in…",
                TEXT_MUTED,
            )),
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
