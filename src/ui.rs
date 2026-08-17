use gpui::{Context, FontWeight, SharedString, Task, Window, div, prelude::*, rgb};

use crate::cache;
use crate::config::Config;
use crate::oauth::{self, TimelineSource};
use crate::paths::Paths;
use crate::rate_limit;
use crate::theme::Theme;
use crate::x_api::{TimelineItem, XClient};

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
    /// Resolved once at construction from `config.theme` — see
    /// [`TimelineView::new`]. `Copy`, so it's handed to the free render
    /// helpers below without lifetime noise.
    theme: Theme,
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
    /// Whether the credential in `client` came from an OAuth session rather
    /// than the app-only bearer token (#31). Drives whether the header keeps
    /// offering "Sign in with X": running on a bearer token is a working
    /// state, but a strictly narrower one, so the offer has to stay
    /// reachable instead of only appearing when there is no credential at
    /// all.
    signed_in_with_oauth: bool,
}

impl TimelineView {
    pub(crate) fn new(
        config: Config,
        paths: Paths,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        // Resolved once, here, rather than on every render: `system` needs a
        // live `Window` to read the OS appearance from, and `Theme` is
        // `Copy`, so there is no cost to keeping it around instead of
        // re-resolving it. A `light`/`dark` config value never depends on
        // the window at all.
        let theme = config.theme.resolve(window.appearance());
        let mut this = Self {
            config,
            paths,
            theme,
            client: None,
            state: TimelineState::Loading,
            fetch: None,
            sign_in_flow: None,
            last_reload_at: None,
            signed_in_with_oauth: false,
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
                    let credential = oauth::resolve_credential(&config, &paths, oauth::unix_now())?;
                    let Some(credential) = credential else {
                        return anyhow::Ok((None, None));
                    };
                    let cached =
                        cache::startup(&paths, &config.target_username, oauth::unix_now())?;
                    anyhow::Ok((Some(credential), cached))
                })
                .await;

            let _ = this.update(cx, |this, cx| match result {
                Ok((Some(credential), Some(items))) => {
                    this.signed_in_with_oauth = credential.is_oauth();
                    this.client = Some(XClient::new(credential.token().to_string()));
                    this.state = TimelineState::Loaded(items);
                    cx.notify();
                }
                Ok((Some(credential), None)) => {
                    this.signed_in_with_oauth = credential.is_oauth();
                    this.client = Some(XClient::new(credential.token().to_string()));
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
                    this.signed_in_with_oauth = true;
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

        let theme = self.theme;

        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .px_4()
            .py_3()
            .bg(rgb(theme.bg_header))
            .border_b_1()
            .border_color(rgb(theme.border))
            .child(
                div()
                    .font_weight(FontWeight::BOLD)
                    .child(format!("@{}", self.config.target_username)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    // #31: running on the app-only bearer token is a working
                    // state, so the primary button says "Reload" — but the
                    // offer to upgrade to a user context has to stay
                    // reachable, or the OAuth flow can never be started at
                    // all while a bearer token is configured.
                    .when(
                        offers_sign_in(
                            self.config.oauth_client_id.as_deref(),
                            self.signed_in_with_oauth,
                            &self.state,
                        ),
                        |row| {
                            row.child(
                                div()
                                    .id("sign-in")
                                    .px_3()
                                    .py_1()
                                    .rounded_full()
                                    .border_1()
                                    .border_color(rgb(theme.accent))
                                    .text_color(rgb(theme.accent))
                                    .child("Sign in with X")
                                    .on_click(
                                        cx.listener(|this, _event, _window, cx| this.sign_in(cx)),
                                    ),
                            )
                        },
                    )
                    .child(
                        div()
                            .id("primary-action")
                            .px_3()
                            .py_1()
                            .rounded_full()
                            .bg(rgb(if busy {
                                theme.button_busy_bg
                            } else {
                                theme.accent
                            }))
                            .text_color(rgb(theme.button_label))
                            .child(label)
                            .on_click(cx.listener(move |this, _event, _window, cx| match action {
                                PrimaryAction::Reload => this.reload(cx),
                                PrimaryAction::SignIn => this.sign_in(cx),
                            })),
                    ),
            )
    }

    fn body(&self) -> impl IntoElement {
        let theme = self.theme;

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
                theme.text_muted,
            )),
            TimelineState::SigningIn => content.child(notice(
                "Waiting for the browser to finish sign-in…",
                theme.text_muted,
            )),
            TimelineState::Loading => {
                content.child(notice("Fetching the timeline…", theme.text_muted))
            }
            TimelineState::RateLimited { reset_at, cooldown } => content.child(notice(
                cooldown_label(*cooldown, *reset_at, oauth::unix_now()),
                theme.danger,
            )),
            TimelineState::Failed(message) => content.child(notice(message.clone(), theme.danger)),
            TimelineState::Loaded(items) if items.is_empty() => {
                content.child(notice("No posts were returned.", theme.text_muted))
            }
            TimelineState::Loaded(items) => {
                content.children(items.iter().map(|item| post_row(item, theme)))
            }
        }
    }
}

impl Render for TimelineView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = self.theme;

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(theme.bg))
            .text_color(rgb(theme.text))
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

fn post_row(item: &TimelineItem, theme: Theme) -> impl IntoElement {
    let byline = byline(&item.author_username);

    div()
        .flex()
        .flex_col()
        .gap_1()
        .px_4()
        .py_3()
        .border_b_1()
        .border_color(rgb(theme.border))
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    div()
                        .font_weight(FontWeight::BOLD)
                        .child(item.author_name.clone()),
                )
                .child(div().text_color(rgb(theme.text_muted)).child(byline))
                .child(
                    div()
                        .text_color(rgb(theme.text_muted))
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

/// Whether the header should offer a separate "Sign in with X" button (#31).
///
/// True only when signing in is both possible and would change something: a
/// client id is configured, the current credential is not already an OAuth
/// session, and the primary button is not itself already the sign-in
/// affordance (which it is whenever there is no credential at all, or one is
/// mid-flight) — two identical buttons side by side would be worse than one.
fn offers_sign_in(
    oauth_client_id: Option<&str>,
    signed_in_with_oauth: bool,
    state: &TimelineState,
) -> bool {
    if oauth_client_id.is_none_or(str::is_empty) || signed_in_with_oauth {
        return false;
    }
    !matches!(
        state,
        TimelineState::NotAuthenticated | TimelineState::SigningIn
    )
}

/// The header's title (#11): which account's posts these are, and — since
/// #11 introduces a second mode — which mode is showing, so the user is
/// never left guessing whether they're looking at their own home timeline or
/// one account's posts.
///
/// `source` is `None` only before a credential has resolved, in which case
/// there is nothing to distinguish yet and this falls back to
/// `target_username` (the eventual `SingleUser` display), matching what the
/// header showed before #11. `home_username` is `None` only for the brief
/// window in `Home` mode before `/me` has resolved even once (never true
/// once anything is cached or has loaded).
fn header_title(
    source: Option<TimelineSource>,
    home_username: Option<&str>,
    target_username: &str,
) -> String {
    // TODO(#11): stubbed to always show the pre-#11 single-user title, so
    // the Home-mode tests fail on behavior instead of a missing symbol.
    let _ = (source, home_username);
    format!("@{target_username}")
}

/// Whether the header should offer a "Load older" button (#11): only once a
/// response has actually carried a `meta.next_token` to resume from, and
/// only while the timeline is in a state where clicking it makes sense.
fn offers_load_older(next_page_token: Option<&str>, state: &TimelineState) -> bool {
    // TODO(#11): stubbed to never offer it, so the "token present" test
    // fails on behavior instead of a missing symbol.
    let _ = (next_page_token, state);
    false
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
    use super::{
        Cooldown, TimelineSource, TimelineState, byline, cooldown_label, format_timestamp,
        header_title, offers_load_older, offers_sign_in, reload_cooldown,
    };

    #[test]
    fn offers_sign_in_while_running_on_the_app_only_bearer_token() {
        // #31: the whole point — a bearer token makes the app work, so the
        // primary button says "Reload" and nothing else would ever surface
        // the OAuth flow.
        assert!(offers_sign_in(
            Some("client-id"),
            false,
            &TimelineState::Loaded(Vec::new())
        ));
    }

    #[test]
    fn does_not_offer_sign_in_once_signed_in_with_oauth() {
        assert!(!offers_sign_in(
            Some("client-id"),
            true,
            &TimelineState::Loaded(Vec::new())
        ));
    }

    #[test]
    fn does_not_offer_sign_in_without_a_client_id() {
        // Nothing to sign in with — the button would only ever error.
        assert!(!offers_sign_in(
            None,
            false,
            &TimelineState::Loaded(Vec::new())
        ));
        assert!(!offers_sign_in(
            Some(""),
            false,
            &TimelineState::Loaded(Vec::new())
        ));
    }

    #[test]
    fn does_not_duplicate_the_primary_sign_in_button() {
        // In these two states the primary button already *is* "Sign in with
        // X" / "Signing in…", so a second one beside it is noise.
        assert!(!offers_sign_in(
            Some("client-id"),
            false,
            &TimelineState::NotAuthenticated
        ));
        assert!(!offers_sign_in(
            Some("client-id"),
            false,
            &TimelineState::SigningIn
        ));
    }

    #[test]
    fn header_title_shows_the_target_username_for_single_user_mode() {
        assert_eq!(
            header_title(Some(TimelineSource::SingleUser), None, "XDevelopers"),
            "@XDevelopers"
        );
    }

    #[test]
    fn header_title_shows_the_target_username_before_a_credential_has_resolved() {
        // Matches what the header showed before #11 — nothing to
        // distinguish yet, since there's no credential at all.
        assert_eq!(header_title(None, None, "XDevelopers"), "@XDevelopers");
    }

    #[test]
    fn header_title_shows_the_signed_in_users_own_name_for_home_mode() {
        assert_eq!(
            header_title(Some(TimelineSource::Home), Some("alice"), "XDevelopers"),
            "@alice — Home timeline"
        );
    }

    #[test]
    fn header_title_falls_back_while_home_mode_has_not_learned_the_username_yet() {
        assert_eq!(
            header_title(Some(TimelineSource::Home), None, "XDevelopers"),
            "Home timeline"
        );
    }

    #[test]
    fn offers_load_older_when_a_next_page_token_is_present_and_the_timeline_is_loaded() {
        assert!(offers_load_older(
            Some("cursor-abc"),
            &TimelineState::Loaded(Vec::new())
        ));
    }

    #[test]
    fn does_not_offer_load_older_without_a_next_page_token() {
        assert!(!offers_load_older(None, &TimelineState::Loaded(Vec::new())));
    }

    #[test]
    fn does_not_offer_load_older_while_not_in_the_loaded_state() {
        assert!(!offers_load_older(Some("cursor-abc"), &TimelineState::Loading));
    }

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
