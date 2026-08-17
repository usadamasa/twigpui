use gpui::{Context, FontWeight, SharedString, Task, Window, div, prelude::*, rgb};

use crate::cache;
use crate::config::Config;
use crate::oauth;
use crate::paths::Paths;
use crate::rate_limit;
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
    /// Blocked before any request went out (#10). Carries when it becomes
    /// allowed again so the header can render a countdown instead of a bare
    /// error message, and which side imposed the wait — see [`Cooldown`].
    RateLimited {
        reset_at: i64,
        cooldown: Cooldown,
    },
    Failed(SharedString),
}

/// Which side is making the app wait. Both render a countdown, but they are
/// different facts and must not be described with the same words: saying "X
/// rate limited you" when the app is really just honouring its own configured
/// fetch interval would be a plain misstatement of what happened.
#[derive(Clone, Copy)]
enum Cooldown {
    /// `config.min_fetch_interval_seconds` — self-imposed, nothing was sent
    /// and X has said nothing.
    LocalInterval,
    /// X's own rate-limit window, per the tracked `x-rate-limit-*` headers.
    ApiRateLimit,
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
    /// When the last reload was kicked off, so [`Self::reload`] can enforce
    /// `config.min_fetch_interval_seconds` (#10) as a client-side throttle
    /// on the button itself — independent of, and in addition to, whatever
    /// the tracked API rate-limit state says via `rate_limit::decision`.
    /// `None` until the first reload, which is therefore never throttled.
    last_reload_at: Option<i64>,
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
            last_reload_at: None,
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
                    let cached =
                        cache::startup(&paths, &config.target_username, oauth::unix_now())?;
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
    ///
    /// Also enforces `config.min_fetch_interval_seconds` (#10) before
    /// spawning anything: [`reload_cooldown`] is a client-side throttle on
    /// the button itself, checked without touching the network, on top of
    /// (not instead of) whatever the tracked API rate-limit state says once
    /// a request actually goes out.
    fn reload(&mut self, cx: &mut Context<'_, Self>) {
        let Some(client) = self.client.clone() else {
            self.state = TimelineState::NotAuthenticated;
            cx.notify();
            return;
        };

        let now = oauth::unix_now();
        if let Some(reset_at) = reload_cooldown(
            self.last_reload_at,
            self.config.min_fetch_interval_seconds,
            now,
        ) {
            self.state = TimelineState::RateLimited {
                reset_at,
                cooldown: Cooldown::LocalInterval,
            };
            cx.notify();
            return;
        }
        self.last_reload_at = Some(now);

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
                    // #10: a blocked-send carries a known reset time is
                    // shown as a countdown; everything else (including a
                    // rate limit whose 429 carried no usable reset header)
                    // falls back to the plain error message.
                    Err(error) => match error.downcast_ref::<rate_limit::RateLimited>() {
                        Some(rate_limit::RateLimited {
                            reset_at: Some(reset_at),
                        }) => TimelineState::RateLimited {
                            reset_at: *reset_at,
                            cooldown: Cooldown::ApiRateLimit,
                        },
                        _ => TimelineState::Failed(format!("{error:#}").into()),
                    },
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
            TimelineState::Loading => ("Loading…".to_string(), true, PrimaryAction::Reload),
            TimelineState::SigningIn => ("Signing in…".to_string(), true, PrimaryAction::SignIn),
            TimelineState::NotAuthenticated => {
                ("Sign in with X".to_string(), false, PrimaryAction::SignIn)
            }
            // Still wired to `PrimaryAction::Reload`: re-clicking just
            // re-runs the (network-free) rate-limit decision — #10 forbids
            // sleeping out the window, not retrying the cheap local check.
            TimelineState::RateLimited { reset_at, cooldown } => (
                cooldown_label(cooldown, reset_at, oauth::unix_now()),
                true,
                PrimaryAction::Reload,
            ),
            TimelineState::Loaded(_) | TimelineState::Failed(_) => {
                ("Reload".to_string(), false, PrimaryAction::Reload)
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
            TimelineState::RateLimited { reset_at, cooldown } => content.child(notice(
                cooldown_label(*cooldown, *reset_at, oauth::unix_now()),
                DANGER,
            )),
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

/// Countdown text for the reload button while blocked by #10's rate-limit
/// decision. `remaining` is clamped to zero rather than going negative if
/// `reset_at` has (just) passed by the time this renders.
///
/// The two cooldowns read differently on purpose: only one of them is X
/// actually rate limiting this app, and reporting the self-imposed interval
/// as a rate limit would misdescribe what happened.
fn cooldown_label(cooldown: Cooldown, reset_at: i64, now: i64) -> String {
    let remaining = (reset_at - now).max(0);
    match cooldown {
        Cooldown::LocalInterval => format!("Waiting out the fetch interval — {remaining}s"),
        Cooldown::ApiRateLimit => format!("Rate limited by X — retry in {remaining}s"),
    }
}

/// Whether [`TimelineView::reload`] should refuse to run right now, per
/// `config.min_fetch_interval_seconds` (#10). `None` means "go ahead" —
/// either there has never been a reload yet, or the interval since the last
/// one has already elapsed. `Some(reset_at)` means "not yet", carrying when
/// it becomes allowed again, in the same shape [`cooldown_label`] expects.
fn reload_cooldown(
    last_reload_at: Option<i64>,
    min_interval_seconds: u32,
    now: i64,
) -> Option<i64> {
    let last = last_reload_at?;
    let reset_at = last.saturating_add(i64::from(min_interval_seconds));
    (reset_at > now).then_some(reset_at)
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
    use super::{Cooldown, byline, cooldown_label, format_timestamp, reload_cooldown};

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

    #[test]
    fn cooldown_label_counts_down_to_the_reset_time() {
        assert_eq!(
            cooldown_label(Cooldown::ApiRateLimit, 1_060, 1_000),
            "Rate limited by X — retry in 60s"
        );
    }

    #[test]
    fn cooldown_label_clamps_a_reset_time_already_passed() {
        // #10: a countdown that's just crossed zero must read "0s", never a
        // confusing negative number.
        assert_eq!(
            cooldown_label(Cooldown::ApiRateLimit, 1_000, 1_060),
            "Rate limited by X — retry in 0s"
        );
    }

    #[test]
    fn cooldown_label_does_not_blame_x_for_the_local_fetch_interval() {
        // The self-imposed interval blocks a reload before anything is sent,
        // so X has said nothing — calling it a rate limit would be a plain
        // misstatement of what happened.
        let label = cooldown_label(Cooldown::LocalInterval, 1_060, 1_000);
        assert_eq!(label, "Waiting out the fetch interval — 60s");
        assert!(!label.contains("Rate limited"), "{label}");
    }

    #[test]
    fn reload_cooldown_allows_the_very_first_reload() {
        assert_eq!(reload_cooldown(None, 60, 1_000), None);
    }

    #[test]
    fn reload_cooldown_blocks_within_the_configured_interval() {
        assert_eq!(reload_cooldown(Some(1_000), 60, 1_030), Some(1_060));
    }

    #[test]
    fn reload_cooldown_allows_once_the_interval_has_elapsed() {
        assert_eq!(reload_cooldown(Some(1_000), 60, 1_060), None);
        assert_eq!(reload_cooldown(Some(1_000), 60, 1_061), None);
    }
}
