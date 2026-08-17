use std::collections::HashMap;

use gpui::{AnyElement, Context, FontWeight, SharedString, Task, Window, div, prelude::*, rgb};

use crate::cache;
use crate::config::Config;
use crate::oauth::{self, TimelineSource};
use crate::paths::Paths;
use crate::rate_limit;
use crate::theme::Theme;
use crate::thread::{self, ThreadChain};
use crate::usage;
use crate::x_api::{QuotedPost, RepliedTo, TimelineItem, XClient};

/// What's known about one reply's "Show thread" walk (#12), keyed by the
/// reply's own post id in [`TimelineView::threads`]. Absent from that map
/// means "not requested yet" — the toggle still offers to fetch.
enum ThreadFetchState {
    Loading,
    Loaded(ThreadChain),
    /// Carries the error text so a retry click can be offered in its place
    /// rather than leaving the row stuck.
    Failed(SharedString),
}

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

/// What [`TimelineView::start`]'s background half found, carried back across
/// the executor boundary to the `update` closure that applies it to `self`.
/// A local enum rather than a tuple because the two credential-bearing modes
/// carry differently shaped cached data (#11): `SingleUser` only ever needed
/// a bare `Option<Vec<TimelineItem>>`, but `Home` also needs the resolved
/// [`cache::MeEntry`] so the header and `home_user_id` can be populated even
/// on a pure cache hit, without a second round trip through `/me`.
enum StartOutcome {
    NotAuthenticated,
    SingleUser {
        credential: oauth::Credential,
        cached: Option<Vec<TimelineItem>>,
    },
    Home {
        credential: oauth::Credential,
        cached: Option<(cache::MeEntry, Vec<TimelineItem>)>,
    },
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
    /// Which timeline this view shows (#11) — decided once, alongside
    /// `client`, from the resolved credential via
    /// [`oauth::TimelineSource::for_credential`]. `None` until a credential
    /// is resolved, mirroring `client`.
    source: Option<TimelineSource>,
    /// The signed-in user's own id, resolved via `GET /2/users/me`. Needed to
    /// call the home-timeline endpoint and to page further back with
    /// [`Self::load_older`]. Populated whenever `source` is
    /// `TimelineSource::Home`; stays `None` for `SingleUser`, which has no
    /// use for it.
    home_user_id: Option<String>,
    /// The signed-in user's own screen name (also from `/me`), shown in the
    /// header instead of `config.target_username` while `source` is
    /// `TimelineSource::Home` — see [`header_title`].
    home_username: Option<String>,
    /// `meta.next_token` from the most recent home-timeline response, if
    /// any (#11). Drives whether the "Load older" button appears — see
    /// [`offers_load_older`]. `None` whenever there is nothing further back
    /// to fetch, or nothing has come from the network yet: a cache-only
    /// render carries no token, since the cursor is not itself persisted.
    next_page_token: Option<String>,
    /// "Show thread" fetches (#12), keyed by the reply's own post id — a
    /// map rather than a single slot since more than one visible reply can
    /// have its thread open at once. Absent means "not requested yet".
    threads: HashMap<String, ThreadFetchState>,
    /// In-flight thread walks, keyed the same way as `threads`, mirroring
    /// `fetch`'s cancel-on-drop contract: dropping the view cancels every
    /// still-running walk along with it.
    thread_fetches: HashMap<String, Task<()>>,
    /// Request-count totals across every tracked endpoint (#18), shown in
    /// the header — see [`Self::refresh_usage`]. Zero until the first
    /// refresh completes, which is a truthful "nothing observed yet" rather
    /// than a placeholder, since `usage::Totals::default()` is exactly what
    /// an empty `usage.json` reads as too.
    usage_totals: usage::Totals,
    /// Holding this keeps the header's usage refresh alive; mirrors `fetch`'s
    /// cancel-on-drop contract. Reassigning it (another refresh) drops and
    /// so cancels whatever read was still running.
    usage_refresh: Option<Task<()>>,
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
            source: None,
            home_user_id: None,
            home_username: None,
            next_page_token: None,
            threads: HashMap::new(),
            thread_fetches: HashMap::new(),
            usage_totals: usage::Totals::default(),
            usage_refresh: None,
        };
        this.start(cx);
        this.refresh_usage(cx);
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
                        return anyhow::Ok(StartOutcome::NotAuthenticated);
                    };
                    // #11: decided once, right where the credential itself
                    // resolves — everything downstream (which cache file,
                    // which endpoint, which header text) branches on this
                    // rather than re-deriving it.
                    match TimelineSource::for_credential(&credential) {
                        TimelineSource::SingleUser => {
                            let cached =
                                cache::startup(&paths, &config.target_username, oauth::unix_now())?;
                            anyhow::Ok(StartOutcome::SingleUser { credential, cached })
                        }
                        TimelineSource::Home => {
                            let cached = cache::startup_home(&paths, oauth::unix_now())?;
                            anyhow::Ok(StartOutcome::Home { credential, cached })
                        }
                    }
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                this.refresh_usage(cx);
                match result {
                Ok(StartOutcome::NotAuthenticated) => {
                    this.state = TimelineState::NotAuthenticated;
                    cx.notify();
                }
                Ok(StartOutcome::SingleUser { credential, cached }) => {
                    this.signed_in_with_oauth = credential.is_oauth();
                    this.source = Some(TimelineSource::SingleUser);
                    this.client = Some(XClient::new(credential.token().to_string()));
                    match cached {
                        Some(items) => {
                            this.state = TimelineState::Loaded(items);
                            cx.notify();
                        }
                        None => this.reload(cx),
                    }
                }
                Ok(StartOutcome::Home { credential, cached }) => {
                    this.signed_in_with_oauth = credential.is_oauth();
                    this.source = Some(TimelineSource::Home);
                    this.client = Some(XClient::new(credential.token().to_string()));
                    match cached {
                        Some((me, items)) => {
                            this.home_user_id = Some(me.id);
                            this.home_username = Some(me.username);
                            this.state = TimelineState::Loaded(items);
                            cx.notify();
                        }
                        None => this.reload(cx),
                    }
                }
                Err(error) => {
                    this.state = TimelineState::Failed(format!("{error:#}").into());
                    cx.notify();
                }
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
        // `source` is always set alongside `client` (see `start` and
        // `sign_in`), so this is defensive rather than a real branch.
        let Some(source) = self.source else {
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
        let max_results = self.config.max_results;

        match source {
            TimelineSource::SingleUser => {
                let username = self.config.target_username.clone();
                self.fetch = Some(cx.spawn(async move |this, cx| {
                    // The client blocks, so it must not run on the foreground thread.
                    let result = cx
                        .background_executor()
                        .spawn(async move {
                            cache::reload(
                                &paths,
                                &client,
                                &username,
                                max_results,
                                oauth::unix_now(),
                            )
                        })
                        .await;

                    let _ = this.update(cx, |this, cx| {
                        this.refresh_usage(cx);
                        this.state = match result {
                            Ok(reloaded) => {
                                // Single-user mode has no pagination cursor —
                                // #11 keeps its "Load older" button reserved
                                // for the home timeline.
                                this.next_page_token = None;
                                TimelineState::Loaded(reloaded.items)
                            }
                            Err(error) => map_reload_error(&error),
                        };
                        cx.notify();
                    });
                }));
            }
            TimelineSource::Home => {
                self.fetch = Some(cx.spawn(async move |this, cx| {
                    let result = cx
                        .background_executor()
                        .spawn(async move {
                            cache::reload_home(&paths, &client, max_results, oauth::unix_now())
                        })
                        .await;

                    let _ = this.update(cx, |this, cx| {
                        this.refresh_usage(cx);
                        this.state = match result {
                            Ok(reloaded) => {
                                this.home_user_id = Some(reloaded.me.id);
                                this.home_username = Some(reloaded.me.username);
                                this.next_page_token = reloaded.next_token;
                                TimelineState::Loaded(reloaded.items)
                            }
                            Err(error) => map_reload_error(&error),
                        };
                        cx.notify();
                    });
                }));
            }
        }

        cx.notify();
    }

    /// Fetch the page behind `next_page_token` and append it after what's
    /// already shown (#11's "Load older") — only ever meaningful in
    /// `TimelineSource::Home`, since `SingleUser` mode never sets a token in
    /// the first place. A no-op if any of the three prerequisites (a client,
    /// a known home user id, a token to resume from) is missing.
    fn load_older(&mut self, cx: &mut Context<'_, Self>) {
        let (Some(client), Some(user_id), Some(token)) = (
            self.client.clone(),
            self.home_user_id.clone(),
            self.next_page_token.clone(),
        ) else {
            return;
        };

        self.state = TimelineState::Loading;

        let paths = self.paths.clone();
        let max_results = self.config.max_results;

        self.fetch = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    cache::load_older_home(
                        &paths,
                        &client,
                        &user_id,
                        max_results,
                        &token,
                        oauth::unix_now(),
                    )
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                this.refresh_usage(cx);
                this.state = match result {
                    Ok((items, next_token)) => {
                        this.next_page_token = next_token;
                        TimelineState::Loaded(items)
                    }
                    Err(error) => map_reload_error(&error),
                };
                cx.notify();
            });
        }));

        cx.notify();
    }

    /// Spend "Show thread"'s credits for one reply (#12): walk its parent
    /// chain (from cache if already fetched, else the network, up to
    /// `thread::MAX_THREAD_DEPTH` requests) and render the result. A no-op
    /// without a client — the toggle isn't shown in that state, but this
    /// guards against it anyway, matching [`Self::reload`]'s convention.
    ///
    /// `reply_post_id` is the reply being expanded (the cache/state key);
    /// `first_parent_id` is its immediate parent's id — `TimelineItem::replied_to`'s
    /// `post_id`, already known for free — where the walk starts.
    fn show_thread(
        &mut self,
        reply_post_id: String,
        first_parent_id: String,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(client) = self.client.clone() else {
            return;
        };

        self.threads
            .insert(reply_post_id.clone(), ThreadFetchState::Loading);
        cx.notify();

        let paths = self.paths.clone();
        let key = reply_post_id.clone();
        let fetch_key = reply_post_id.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    cache::fetch_thread(
                        &paths,
                        &client,
                        &reply_post_id,
                        &first_parent_id,
                        oauth::unix_now(),
                    )
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                this.refresh_usage(cx);
                let state = match result {
                    Ok(chain) => ThreadFetchState::Loaded(chain),
                    Err(error) => ThreadFetchState::Failed(format!("{error:#}").into()),
                };
                this.threads.insert(key.clone(), state);
                this.thread_fetches.remove(&key);
                cx.notify();
            });
        });
        self.thread_fetches.insert(fetch_key, task);
    }

    /// Refresh the header's usage summary from disk (#18), independent of
    /// whatever triggered it — every fetch path (a reload, "Load older", a
    /// "Show thread" walk) can have moved the tracked counts, since
    /// `x_api::client::XClient::get` records every actual HTTP send
    /// regardless of whether the request itself succeeded. Spawned on its
    /// own rather than folded into the fetch that triggered it: if reading
    /// `usage.json` fails, the header just keeps showing whatever it showed
    /// before, rather than failing the fetch along with it.
    fn refresh_usage(&mut self, cx: &mut Context<'_, Self>) {
        let paths = self.paths.clone();
        self.usage_refresh = Some(cx.spawn(async move |this, cx| {
            let now = oauth::unix_now();
            let result = cx
                .background_executor()
                .spawn(
                    async move { usage::load_all(&paths).map(|entries| usage::totals(&entries, now)) },
                )
                .await;

            if let Ok(totals) = result {
                let _ = this.update(cx, |this, cx| {
                    this.usage_totals = totals;
                    cx.notify();
                });
            }
        }));
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
                    // #11: a stored OAuth session always maps to the home
                    // timeline — see `TimelineSource::for_credential`.
                    this.source = Some(TimelineSource::Home);
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

        // #18: request counts are always shown; an estimated amount is
        // appended only when `request_price` is configured (see
        // `usage_label`'s doc), and the line's color escalates as today's
        // count approaches or crosses `daily_request_budget` (see
        // `usage_color`'s doc).
        let usage_status =
            usage::budget_status(self.usage_totals.today, self.config.daily_request_budget);
        let usage_text = usage_label(
            self.usage_totals.today,
            self.usage_totals.total,
            self.config.request_price,
        );

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
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().font_weight(FontWeight::BOLD).child(header_title(
                        self.source,
                        self.home_username.as_deref(),
                        &self.config.target_username,
                    )))
                    .child(
                        div()
                            .text_color(rgb(usage_color(usage_status, theme)))
                            .child(usage_text),
                    ),
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

    fn post_row(&self, item: &TimelineItem, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = self.theme;
        let byline = byline(&item.author_username);

        div()
            .flex()
            .flex_col()
            .gap_1()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(rgb(theme.border))
            // #13: a repost shows who reposted it as a small line above the
            // body, which by this point already holds the *original* post
            // (see `TimelineResponse::into_items`'s join) — not the outer
            // post's own author/text.
            .when_some(item.reposted_by.as_deref(), |row, reposted_by| {
                row.child(
                    div()
                        .text_color(rgb(theme.text_muted))
                        .child(repost_banner_label(reposted_by)),
                )
            })
            // #12: who this post is replying to, shown at zero extra
            // request cost — the parent's author is already in `includes`
            // per #13's expansions (see `x_api::model::reply_target`).
            .when_some(item.replied_to.as_ref(), |row, replied_to| {
                row.child(
                    div()
                        .text_color(rgb(theme.text_muted))
                        .child(reply_banner_label(replied_to)),
                )
            })
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
            // #13: a quote (including a repost of a quote) embeds its source
            // as a bordered card under the text.
            .when_some(item.quoted.as_ref(), |column, quoted| {
                column.child(quote_card(quoted, theme))
            })
            // #12: "Show thread" — only offered for a reply, since that's
            // the only case with a parent to walk.
            .when_some(item.replied_to.as_ref(), |column, replied_to| {
                column.child(self.thread_section(&item.id, replied_to, cx))
            })
            .into_any_element()
    }

    /// The "Show thread" toggle, loading/error state, or assembled chain for
    /// one reply (#12) — whichever `self.threads.get(reply_post_id)` says is
    /// current. Split out from [`Self::post_row`] only for readability; it
    /// still needs `cx` for the toggle's click handler.
    fn thread_section(
        &self,
        reply_post_id: &str,
        replied_to: &RepliedTo,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = self.theme;

        let state = self.threads.get(reply_post_id);

        if let Some(ThreadFetchState::Loaded(chain)) = state {
            return render_thread_chain(chain, theme);
        }
        if matches!(state, Some(ThreadFetchState::Loading)) {
            return div()
                .text_color(rgb(theme.text_muted))
                .child("Loading thread…")
                .into_any_element();
        }

        // Reachable states here: `None` (never requested) and `Failed` —
        // both offer a clickable toggle, just with different labels; see
        // `thread_action_label`.
        let label = thread_action_label(state).unwrap_or_default();
        let toggle = thread_toggle_row(
            reply_post_id.to_string(),
            replied_to.post_id.clone(),
            label,
            theme,
            cx,
        );

        if let Some(ThreadFetchState::Failed(message)) = state {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_color(rgb(theme.danger)).child(message.clone()))
                .child(toggle)
                .into_any_element()
        } else {
            toggle.into_any_element()
        }
    }

    fn body(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
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
                // A plain loop rather than `.children(items.iter().map(...))`:
                // `post_row` needs `cx` (for #12's "Show thread" click
                // handler), and a `FnMut` closure invoked by `.map` can't let
                // a value borrowed from its own captured `cx` escape into the
                // returned element.
                let mut rows: Vec<AnyElement> = Vec::with_capacity(items.len());
                for item in items {
                    rows.push(self.post_row(item, cx));
                }
                content
                    .children(rows)
                    // #11: only offered once a response has actually carried
                    // a `meta.next_token` to resume from, and only while
                    // there is room under the cap for the page it would
                    // fetch.
                    .when(
                        offers_load_older(self.next_page_token.as_deref(), &self.state),
                        |list| list.child(load_older_row(theme, cx)),
                    )
                    .when(at_the_post_cap(&self.state), |list| {
                        list.child(notice(
                            format!(
                                "Showing the most recent {} posts — that is as far back as \
                                 twigpui keeps.",
                                cache::MAX_CACHED_POSTS
                            ),
                            theme.text_muted,
                        ))
                    })
            }
        }
    }
}

/// The "Load older" row appended after the list (#11), styled like
/// [`notice`] but clickable — appending posts *behind* what's already shown
/// via `cache::append_older`, never merged ahead like a normal reload.
fn load_older_row(theme: Theme, cx: &mut Context<'_, TimelineView>) -> impl IntoElement {
    div()
        .id("load-older")
        .px_4()
        .py_3()
        .text_color(rgb(theme.accent))
        .child("Load older")
        .on_click(cx.listener(|this, _event, _window, cx| this.load_older(cx)))
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
            .child(self.body(cx))
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

/// "@name reposted", or "Reposted" alone when the reposting user's screen
/// name was missing from the expansion — mirrors [`byline`]'s empty-author
/// fallback rather than rendering a bare `@`.
fn repost_banner_label(reposted_by: &str) -> String {
    if reposted_by.is_empty() {
        "Reposted".to_string()
    } else {
        format!("@{reposted_by} reposted")
    }
}

/// The quoted source, embedded as a bordered card under a quote's own text
/// (#13). Reuses `bg_header` for the fill rather than adding a new color
/// slot — it's already the app's "distinct region" background (the header
/// bar), and the card sits directly on `theme.bg`, so it reads as a clearly
/// separate block without needing its own palette entry.
fn quote_card(quoted: &QuotedPost, theme: Theme) -> impl IntoElement {
    let byline = byline(&quoted.author_username);

    div()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .mt_1()
        .rounded_md()
        .border_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.bg_header))
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    div()
                        .font_weight(FontWeight::BOLD)
                        .child(quoted.author_name.clone()),
                )
                .child(div().text_color(rgb(theme.text_muted)).child(byline)),
        )
        .child(div().child(quoted.text.clone()))
}

/// "Replying to @name", or a generic fallback when the parent's author
/// wasn't resolvable (deleted, protected, or simply not expanded) — mirrors
/// [`repost_banner_label`]'s empty-author fallback rather than rendering a
/// bare "Replying to @" (#12).
fn reply_banner_label(replied_to: &RepliedTo) -> String {
    if replied_to.author_username.is_empty() {
        "Replying to a post".to_string()
    } else {
        format!("Replying to @{}", replied_to.author_username)
    }
}

/// The clickable label for [`thread_toggle_row`], or `None` when the current
/// state (nothing yet loaded but a fetch is running) has no toggle at all —
/// [`TimelineView::thread_section`] renders a plain "Loading thread…" notice
/// for that case instead. `state: None` means "never requested" (offer to
/// fetch, spelling out the worst-case cost up front per #12's "cost must be
/// predictable" requirement); `Some(Failed(_))` offers a retry.
fn thread_action_label(state: Option<&ThreadFetchState>) -> Option<&'static str> {
    match state {
        None => Some("Show thread (up to 5 requests)"),
        Some(ThreadFetchState::Failed(_)) => Some("Retry"),
        Some(ThreadFetchState::Loading | ThreadFetchState::Loaded(_)) => None,
    }
}

/// The clickable "Show thread" / "Retry" row (#12), styled like
/// [`load_older_row`] — a link-colored, clickable line rather than a full
/// button, since it's a secondary action on an already-rendered post.
fn thread_toggle_row(
    reply_post_id: String,
    first_parent_id: String,
    label: &str,
    theme: Theme,
    cx: &mut Context<'_, TimelineView>,
) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("show-thread-{reply_post_id}")))
        .text_color(rgb(theme.accent))
        .child(label.to_string())
        .on_click(cx.listener(move |this, _event, _window, cx| {
            this.show_thread(reply_post_id.clone(), first_parent_id.clone(), cx);
        }))
}

/// The assembled parent chain (#12), oldest ancestor first, each rendered
/// like [`quote_card`] for visual consistency with the other "embedded post"
/// treatment already in this file. An empty, uncapped chain only happens
/// when the very first parent fetch found nothing (deleted, protected, or
/// otherwise absent) — #12's "must render sensibly" requirement — so that
/// case gets its own message rather than silently showing nothing.
fn render_thread_chain(chain: &ThreadChain, theme: Theme) -> AnyElement {
    if chain.items.is_empty() && !chain.capped {
        return div()
            .text_color(rgb(theme.text_muted))
            .child("The parent post is no longer available.")
            .into_any_element();
    }

    div()
        .flex()
        .flex_col()
        .gap_1()
        .children(
            chain
                .items
                .iter()
                .map(|thread_item| thread_row(thread_item, theme)),
        )
        .when(chain.capped, |column| {
            column.child(div().text_color(rgb(theme.text_muted)).child(format!(
                "Reached the {}-level limit — earlier replies in this thread \
                         aren't shown.",
                thread::MAX_THREAD_DEPTH
            )))
        })
        .into_any_element()
}

fn thread_row(thread_item: &thread::ThreadItem, theme: Theme) -> impl IntoElement {
    let byline = byline(&thread_item.author_username);

    div()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .rounded_md()
        .border_1()
        .border_color(rgb(theme.border))
        .bg(rgb(theme.bg_header))
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    div()
                        .font_weight(FontWeight::BOLD)
                        .child(thread_item.author_name.clone()),
                )
                .child(div().text_color(rgb(theme.text_muted)).child(byline)),
        )
        .child(div().child(thread_item.text.clone()))
}

/// The header's compact usage summary (#18): request counts are always
/// shown, unconditionally — an estimated amount is appended only once
/// `request_price` is configured, per the issue's core rule that a guessed
/// price is worse than showing no price at all.
fn usage_label(today: u64, total: u64, request_price: Option<f64>) -> String {
    let _ = (today, total, request_price);
    // STUB: real formatting lands with the rest of #18's implementation.
    String::new()
}

/// Which theme slot the usage line renders in: `warning`/`danger` as
/// today's count approaches or crosses `daily_request_budget`, matching the
/// severities [`usage::budget_status`] returns; the same muted slot
/// timestamps and bylines already use once there is nothing to flag.
fn usage_color(status: usage::BudgetStatus, theme: Theme) -> u32 {
    let _ = status;
    // STUB: real mapping lands with the rest of #18's implementation.
    theme.text_muted
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

/// Map a failed reload/load-older's error to the state that should show it —
/// shared by every fetch path in this file (single-user reload, home-timeline
/// reload, and "Load older") so the #10 rate-limit-countdown behavior stays
/// in exactly one place rather than being copy-pasted per branch.
///
/// #10: a blocked-send carries a known reset time and is shown as a
/// countdown; everything else (including a rate limit whose 429 carried no
/// usable reset header) falls back to the plain error message.
fn map_reload_error(error: &anyhow::Error) -> TimelineState {
    match error.downcast_ref::<rate_limit::RateLimited>() {
        Some(rate_limit::RateLimited {
            reset_at: Some(reset_at),
        }) => TimelineState::RateLimited {
            reset_at: *reset_at,
            cooldown: Cooldown::ApiRateLimit,
        },
        _ => TimelineState::Failed(format!("{error:#}").into()),
    }
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
    match (source, home_username) {
        (Some(TimelineSource::Home), Some(username)) => format!("@{username} — Home timeline"),
        (Some(TimelineSource::Home), None) => "Home timeline".to_string(),
        (Some(TimelineSource::SingleUser) | None, _) => format!("@{target_username}"),
    }
}

/// Whether the header should offer a "Load older" button (#11): only once a
/// response has actually carried a `meta.next_token` to resume from, and
/// only while the timeline is in a state where clicking it makes sense.
///
/// Withheld at the post cap, which is the part that matters for money.
/// `cache::append_older` truncates back down to `MAX_CACHED_POSTS`, so at the
/// cap a click would spend a real API request and then discard every post it
/// bought — a paid no-op, in a project whose entire cache exists to avoid
/// exactly that. [`at_the_post_cap`] renders an explanation in its place so
/// the button does not just silently vanish.
fn offers_load_older(next_page_token: Option<&str>, state: &TimelineState) -> bool {
    match state {
        TimelineState::Loaded(items) => {
            next_page_token.is_some() && items.len() < cache::MAX_CACHED_POSTS
        }
        _ => false,
    }
}

/// Whether the loaded timeline has hit the cap that [`offers_load_older`]
/// stops at, so the body can say why there is nothing further back.
fn at_the_post_cap(state: &TimelineState) -> bool {
    matches!(state, TimelineState::Loaded(items) if items.len() >= cache::MAX_CACHED_POSTS)
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
        Cooldown, RepliedTo, Theme, ThreadFetchState, TimelineItem, TimelineSource, TimelineState,
        at_the_post_cap, byline, cooldown_label, format_timestamp, header_title, offers_load_older,
        offers_sign_in, reload_cooldown, reply_banner_label, repost_banner_label,
        thread_action_label, usage, usage_color, usage_label,
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
    fn does_not_offer_load_older_at_the_post_cap() {
        // `cache::append_older` truncates back to the cap, so a click here
        // would spend a real API request and discard everything it bought.
        let full: Vec<_> = (0..crate::cache::MAX_CACHED_POSTS)
            .map(|n| TimelineItem {
                id: n.to_string(),
                text: String::new(),
                created_at: None,
                author_name: String::new(),
                author_username: String::new(),
                reposted_by: None,
                quoted: None,
                replied_to: None,
            })
            .collect();
        let state = TimelineState::Loaded(full);

        assert!(!offers_load_older(Some("cursor-abc"), &state));
        // ...and the body explains itself rather than the button just
        // disappearing.
        assert!(at_the_post_cap(&state));
    }

    #[test]
    fn is_not_at_the_post_cap_below_it() {
        assert!(!at_the_post_cap(&TimelineState::Loaded(Vec::new())));
    }

    #[test]
    fn does_not_offer_load_older_while_not_in_the_loaded_state() {
        assert!(!offers_load_older(
            Some("cursor-abc"),
            &TimelineState::Loading
        ));
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
    fn labels_a_repost_with_who_reposted_it() {
        assert_eq!(repost_banner_label("reposter1"), "@reposter1 reposted");
    }

    #[test]
    fn labels_a_repost_generically_when_the_reposter_is_missing() {
        // Mirrors byline's empty-author fallback (#13): a bare "@ reposted"
        // would read as broken.
        assert_eq!(repost_banner_label(""), "Reposted");
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

    #[test]
    fn labels_a_reply_with_who_it_is_replying_to() {
        let replied_to = RepliedTo {
            post_id: "1".to_string(),
            author_name: "Developers".to_string(),
            author_username: "XDevelopers".to_string(),
        };
        assert_eq!(reply_banner_label(&replied_to), "Replying to @XDevelopers");
    }

    #[test]
    fn labels_a_reply_generically_when_the_parent_author_is_missing() {
        // Mirrors repost_banner_label's empty-author fallback (#12): a bare
        // "Replying to @" would read as broken.
        let replied_to = RepliedTo {
            post_id: "1".to_string(),
            author_name: String::new(),
            author_username: String::new(),
        };
        assert_eq!(reply_banner_label(&replied_to), "Replying to a post");
    }

    #[test]
    fn offers_to_show_the_thread_with_the_worst_case_cost_spelled_out() {
        // #12: the cost must be predictable *before* spending it — the
        // label itself says how many requests a click can cost.
        assert_eq!(
            thread_action_label(None),
            Some("Show thread (up to 5 requests)")
        );
    }

    #[test]
    fn offers_a_retry_after_a_failed_thread_fetch() {
        let state = ThreadFetchState::Failed("boom".into());
        assert_eq!(thread_action_label(Some(&state)), Some("Retry"));
    }

    #[test]
    fn offers_no_toggle_while_loading_or_once_loaded() {
        assert_eq!(thread_action_label(Some(&ThreadFetchState::Loading)), None);
        let loaded = ThreadFetchState::Loaded(crate::thread::ThreadChain::default());
        assert_eq!(thread_action_label(Some(&loaded)), None);
    }

    // --- usage_label / usage_color (#18) ---

    #[test]
    fn usage_label_shows_counts_only_without_a_configured_price() {
        assert_eq!(usage_label(3, 42, None), "Today: 3 req · Total: 42 req");
    }

    #[test]
    fn usage_label_appends_an_estimated_amount_once_a_price_is_configured() {
        assert_eq!(
            usage_label(4, 40, Some(2.5)),
            "Today: 4 req (~10.00) · Total: 40 req"
        );
    }

    #[test]
    fn usage_label_shows_zero_counts_plainly() {
        assert_eq!(usage_label(0, 0, None), "Today: 0 req · Total: 0 req");
    }

    #[test]
    fn usage_color_is_muted_within_budget() {
        let theme = Theme::light();
        assert_eq!(
            usage_color(usage::BudgetStatus::Ok, theme),
            theme.text_muted
        );
    }

    #[test]
    fn usage_color_is_the_warning_slot_near_the_budget() {
        let theme = Theme::light();
        assert_eq!(usage_color(usage::BudgetStatus::Near, theme), theme.warning);
    }

    #[test]
    fn usage_color_is_the_danger_slot_once_the_budget_is_exceeded() {
        let theme = Theme::light();
        assert_eq!(
            usage_color(usage::BudgetStatus::Exceeded, theme),
            theme.danger
        );
    }
}
