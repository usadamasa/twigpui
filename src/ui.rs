use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use gpui::{
    AnyElement, Context, Entity, FontWeight, SharedString, Subscription, Task, Window, div, img,
    prelude::*, px, rgb,
};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::avatar;
use crate::browser;
use crate::cache;
use crate::compose::{self, ComposeState, ComposeStatus};
use crate::config::Config;
use crate::like;
use crate::oauth::{self, TimelineSource};
use crate::paths::Paths;
use crate::rate_limit;
use crate::repost;
use crate::theme::{self, Theme};
use crate::thread::{self, ThreadChain};
use crate::toggle::{ToggleState, ToggleStatus};
use crate::usage;
use crate::x_api::{PostLink, PostMetrics, QuotedPost, RepliedTo, TimelineItem, XClient};

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

#[derive(Debug)]
enum TimelineState {
    /// No usable credential yet: no fresh/refreshable stored OAuth session
    /// and no bearer token. Shown at startup before the sign-in flow runs.
    NotAuthenticated,
    /// The interactive "Sign in with X" flow is running — browser opened,
    /// waiting on the loopback callback.
    SigningIn,
    Loading,
    Loaded(Vec<TimelineItem>),
    /// Nothing has ever loaded, and the most recent attempt to fetch
    /// something ran into a rate limit with a known reset time (#10) — see
    /// [`Cooldown`] for which side imposed it. Since #57, this is no longer
    /// how a cooldown *or a failed reload* is reported while there are
    /// already posts on screen: [`TimelineView::reload_notice`] carries that
    /// independently of `state` instead (mirroring #54's `session_notice`),
    /// so the timeline is never evicted just to make room for a countdown or
    /// an error line. This variant only remains reachable as the fallback
    /// for the narrow case where there is nothing else the body could
    /// render — see [`reload_failure_outcome`].
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cooldown {
    /// `config.min_fetch_interval_seconds` — self-imposed, nothing was sent
    /// and X has said nothing.
    LocalInterval,
    /// X's own rate-limit window, per the tracked `x-rate-limit-*` headers.
    ApiRateLimit,
}

/// A transient notice about the most recent reload attempt, kept
/// independent of `state` for exactly the reason #54's `session_notice`
/// field is (see its doc): a cooldown that blocked the request, or a
/// failure once it ran, describes what just happened to the *request*, not
/// to whatever posts are already on screen — collapsing the two into one
/// `state` is what made #57 possible (a countdown, or an error, evicting a
/// timeline that never actually changed). Cleared the instant a reload
/// succeeds — see [`TimelineView::reload`]'s result handling — so unlike
/// `session_notice` this never outlives what it was reporting.
#[derive(Debug, Clone, PartialEq)]
enum ReloadNotice {
    /// Blocked by a cooldown (#10's own interval, or X's rate limit) before
    /// or while the request was in flight. Carries the same
    /// `reset_at`/`cooldown` pair [`cooldown_label`] already renders from,
    /// so the countdown text is computed fresh at render time rather than
    /// stored — #57's item 3 (making the countdown actually tick) is a
    /// separate, still-open concern.
    Cooldown { reset_at: i64, cooldown: Cooldown },
    /// The request went out and failed for a reason with no known reset
    /// time.
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

/// Whether [`TimelineView::reload`] should honor `config.min_fetch_interval_seconds`
/// (#10) at all. The interval exists to suppress *polling* — it was never
/// meant to block confirming the result of something the user just did on
/// purpose, and #57 was exactly that bug: a post or a sign-in, each already
/// having spent its own request, immediately blocked on the interval it had
/// no reason to observe.
#[derive(Debug, Clone, Copy)]
enum ReloadTrigger {
    /// An unsolicited reload — the startup cache-miss path, or the "Reload"
    /// button. Subject to the configured interval like any other fetch that
    /// wasn't a direct response to a user action.
    Polling,
    /// The direct result of a user action that already spent its own
    /// request (a successful sign-in, a successful post): must never wait
    /// out an interval meant for polling.
    UserAction,
}

/// What [`TimelineView::start`]'s background half found, carried back across
/// the executor boundary to the `update` closure that applies it to `self`.
/// A local enum rather than a tuple because the two credential-bearing modes
/// carry differently shaped cached data (#11): `SingleUser` only ever needed
/// a bare `Option<Vec<TimelineItem>>`, but `Home` also needs the resolved
/// [`cache::MeEntry`] so the header and `home_user_id` can be populated even
/// on a pure cache hit, without a second round trip through `/me`.
enum StartOutcome {
    NotAuthenticated {
        session_notice: Option<String>,
    },
    SingleUser {
        credential: oauth::Credential,
        cached: Option<Vec<TimelineItem>>,
        session_notice: Option<String>,
    },
    Home {
        credential: oauth::Credential,
        cached: Option<(cache::MeEntry, Vec<TimelineItem>)>,
        session_notice: Option<String>,
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
    /// Whether a [`Self::reload`] is currently in flight (#57). Distinct
    /// from `state == TimelineState::Loading`: that variant means *nothing
    /// is displayed yet*, whereas this stays `true` while a reload runs even
    /// when `state` is `Loaded` and keeps showing the previous posts — see
    /// [`reload_start_state`]. Drives the header's busy label; `body` needs
    /// no equivalent check since it renders straight off `state`, which this
    /// flag deliberately leaves untouched.
    reloading: bool,
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
    /// The post composer's draft text and submit status (#14) — see
    /// `compose.rs`'s module doc for why this is its own pure type rather
    /// than fields scattered across this struct. This stays the
    /// authoritative *text* for everything downstream (the counter,
    /// `can_submit`, `submit_post`): it is mirrored from `compose_input`'s
    /// own buffer on every `InputEvent::Change` (#38) — see
    /// [`Self::on_compose_input_event`] — rather than read directly, so
    /// `compose.rs`'s pure logic keeps operating on a plain `&str` and stays
    /// testable without gpui at all, exactly as before this widget existed.
    compose: ComposeState,
    /// The composer's real text-entry widget (#38), replacing the
    /// raw-keystroke reading a `div().on_key_down()` used to do: this is a
    /// `gpui_component::input::InputState`, which implements
    /// `EntityInputHandler` properly, so IME composition (Japanese, Chinese,
    /// Korean), cursor movement, selection, and copy/paste all work. Its
    /// buffer is the one the user actually sees and types into; `compose`
    /// above is kept in sync with it, not the other way around, except for
    /// one deliberate exception — see [`Self::submit_post`]'s success path,
    /// which clears this explicitly since a successful submit's `text.clear()`
    /// on `compose` alone would leave the widget still showing the old draft.
    compose_input: Entity<InputState>,
    /// Keeps `compose_input`'s change subscription alive — dropping it would
    /// silently stop `compose` above from ever being mirrored again. Same
    /// cancel/keep-alive convention as `fetch` and this struct's other
    /// `Task`-holding fields, just for a `Subscription` instead; the leading
    /// underscore (never read, only held) matches how gpui-component names
    /// this exact pattern for its own search-input subscription.
    _compose_input_subscription: Subscription,
    /// Holding this keeps an in-flight `POST /2/tweets` alive, mirroring
    /// `fetch`'s cancel-on-drop contract. In practice this is only ever
    /// assigned once per submit cycle: [`ComposeState::can_submit`] is
    /// false for the entire time one is outstanding, and it's checked
    /// synchronously at the top of [`Self::submit_post`] before anything
    /// here is touched — see that method's doc for why that's what actually
    /// rules out a second submission reaching this field at all.
    submit_task: Option<Task<()>>,
    /// The signed-in session's granted scope, mirrored from the resolved
    /// credential (#14) — `None` for a bearer credential or an OAuth
    /// session whose scope wasn't recorded. Feeds [`offers_reauthorize`]
    /// and [`Self::submit_post`]'s own scope check.
    oauth_scope: Option<String>,
    /// A human-readable explanation of why a stored OAuth session couldn't
    /// be used as-is at the most recent credential resolution (#54) — `None`
    /// when nothing degraded (a fresh or successfully refreshed session, no
    /// stored session at all, or no OAuth involved to begin with). Rendered
    /// as a persistent banner regardless of `state` — see
    /// [`session_notice_banner`] — since the defect this field exists to fix
    /// is precisely that the timeline otherwise renders as if nothing
    /// happened. Set once, in [`Self::start`] (mirroring how the credential
    /// itself is only resolved at startup — see that method's doc); cleared
    /// the moment a fresh sign-in or re-authorize succeeds, in
    /// [`Self::sign_in`].
    session_notice: Option<SharedString>,
    /// A cooldown or a failed reload, kept independent of `state` (#57) —
    /// see [`ReloadNotice`]'s doc for why. `None` whenever the most recent
    /// reload attempt (if any) hasn't been blocked or hasn't failed; set in
    /// [`Self::reload`]'s early-return, [`Self::load_older`]'s, and
    /// [`Self::apply_reload_failure`]'s result-handling paths, cleared the
    /// moment a reload starts or succeeds.
    reload_notice: Option<ReloadNotice>,
    /// Ticks `reload_notice`'s countdown once a second while it holds a live
    /// `ReloadNotice::Cooldown` (#57's item 3) — [`cooldown_label`] only
    /// recomputes its text at render time, so without a periodic
    /// `cx.notify()` the banner would freeze at whatever second happened to
    /// be showing when it was last drawn. `None` whenever no cooldown is
    /// currently ticking. Same cancel-on-drop convention as `fetch`/
    /// `sign_in_flow`: reassigning this (a fresh cooldown superseding a
    /// still-running one) or clearing it (an immediate stop on success or on
    /// a plain failure — see [`Self::apply_reload_failure`]) drops and so
    /// cancels whatever loop was running. See
    /// [`Self::start_cooldown_ticker`] for the loop itself.
    cooldown_ticker: Option<Task<()>>,
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
    /// Every post id this app has reposted, per the local record (#15) —
    /// refreshed from disk whenever the visible timeline changes (see
    /// [`Self::refresh_reposted_ids`]). The default source for
    /// [`Self::repost_state_for`]; `repost_overrides` below takes
    /// precedence for any post already touched this session.
    reposted_ids: HashSet<String>,
    /// Holds the in-flight read from [`Self::refresh_reposted_ids`] alive;
    /// mirrors `usage_refresh`'s cancel-on-drop contract.
    reposted_ids_refresh: Option<Task<()>>,
    /// Per-post repost button state (#15) for any post touched this
    /// session — pending, failed, or a value a finished request has already
    /// confirmed and so is authoritative over `reposted_ids` until the next
    /// refresh catches up. Absent means "use `reposted_ids`'s plain on/off
    /// value" — see [`Self::repost_state_for`].
    repost_overrides: HashMap<String, ToggleState>,
    /// In-flight create/delete repost requests, keyed by post id, mirroring
    /// `thread_fetches`'s cancel-on-drop contract: dropping the view
    /// cancels every still-running toggle along with it.
    repost_tasks: HashMap<String, Task<()>>,
    /// Every post id this app has liked, per the local record (#68) — the
    /// like-side counterpart of `reposted_ids`, kept as its own set because
    /// the two records are separate files written by independent toggles.
    liked_ids: HashSet<String>,
    /// Holds the in-flight read from [`Self::refresh_liked_ids`] alive;
    /// mirrors `reposted_ids_refresh`.
    liked_ids_refresh: Option<Task<()>>,
    /// Per-post like button state (#68) — see `repost_overrides`, which
    /// this mirrors exactly.
    like_overrides: HashMap<String, ToggleState>,
    /// In-flight create/delete like requests, keyed by post id — see
    /// `repost_tasks`.
    like_tasks: HashMap<String, Task<()>>,
    /// Holds the in-flight `open(1)` spawn alive (#70); mirrors
    /// `usage_refresh`'s cancel-on-drop contract. Only one is kept: opening
    /// a second link while the first is still spawning is not something
    /// worth queueing.
    /// Downloaded avatars (#64), keyed by the API's own `profile_image_url`
    /// — the key is the URL as it arrived, not the larger variant actually
    /// fetched, so a row can look itself up without repeating
    /// `avatar::preferred_url`'s guess. Absent means "not downloaded yet",
    /// which renders the placeholder.
    avatar_paths: HashMap<String, PathBuf>,
    /// Holds the in-flight avatar downloads alive (#64). One task walks the
    /// whole visible timeline rather than one per row; reassigning it (a
    /// reload) cancels whatever was still downloading, which the next call
    /// re-collects from the new timeline anyway.
    avatar_fetch: Option<Task<()>>,
    open_task: Option<Task<()>>,
    /// Why the last open attempt failed (#70), shown in the header until
    /// the next attempt clears it. `None` is the ordinary case — a
    /// successful open leaves the app with nothing to say.
    open_failure: Option<String>,
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
        // #38: point gpui-component's own global theme at the same resolved
        // palette before its `Input` widget is constructed below — see
        // `theme::sync_gpui_component_theme`'s doc for why this is needed at
        // all (its colors live in a completely separate global).
        theme::sync_gpui_component_theme(theme, window, cx);

        let compose_input = cx.new(|cx| {
            InputState::new(window, cx)
                .auto_grow(2, 8)
                .placeholder("What's happening?")
        });
        let compose_input_subscription = cx.subscribe(&compose_input, Self::on_compose_input_event);

        let mut this = Self {
            config,
            paths,
            theme,
            client: None,
            state: TimelineState::Loading,
            fetch: None,
            sign_in_flow: None,
            last_reload_at: None,
            reloading: false,
            signed_in_with_oauth: false,
            source: None,
            home_user_id: None,
            home_username: None,
            next_page_token: None,
            threads: HashMap::new(),
            thread_fetches: HashMap::new(),
            compose: ComposeState::new(),
            compose_input,
            _compose_input_subscription: compose_input_subscription,
            submit_task: None,
            oauth_scope: None,
            session_notice: None,
            reload_notice: None,
            cooldown_ticker: None,
            usage_totals: usage::Totals::default(),
            usage_refresh: None,
            reposted_ids: HashSet::new(),
            reposted_ids_refresh: None,
            repost_overrides: HashMap::new(),
            repost_tasks: HashMap::new(),
            liked_ids: HashSet::new(),
            liked_ids_refresh: None,
            like_overrides: HashMap::new(),
            like_tasks: HashMap::new(),
            avatar_paths: HashMap::new(),
            avatar_fetch: None,
            open_task: None,
            open_failure: None,
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
                    let resolution = oauth::resolve_credential(&config, &paths, oauth::unix_now())?;
                    // #54: rendered as a persistent banner regardless of what
                    // `credential` below turns out to be — a demoted session
                    // and "never signed in" can resolve to the exact same
                    // credential, but only one of them is worth telling the
                    // user about.
                    let session_notice = resolution
                        .demotion
                        .as_ref()
                        .map(|demotion| oauth::describe_demotion(demotion, &paths));
                    let Some(credential) = resolution.credential else {
                        return anyhow::Ok(StartOutcome::NotAuthenticated { session_notice });
                    };
                    // #11: decided once, right where the credential itself
                    // resolves — everything downstream (which cache file,
                    // which endpoint, which header text) branches on this
                    // rather than re-deriving it.
                    match TimelineSource::for_credential(&credential) {
                        TimelineSource::SingleUser => {
                            let cached =
                                cache::startup(&paths, &config.target_username, oauth::unix_now())?;
                            anyhow::Ok(StartOutcome::SingleUser {
                                credential,
                                cached,
                                session_notice,
                            })
                        }
                        TimelineSource::Home => {
                            let cached = cache::startup_home(&paths, oauth::unix_now())?;
                            anyhow::Ok(StartOutcome::Home {
                                credential,
                                cached,
                                session_notice,
                            })
                        }
                    }
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                this.refresh_usage(cx);
                this.refresh_reposted_ids(cx);
                this.refresh_liked_ids(cx);
                this.refresh_avatars(cx);
                match result {
                    Ok(StartOutcome::NotAuthenticated { session_notice }) => {
                        this.session_notice = session_notice.map(SharedString::from);
                        this.state = TimelineState::NotAuthenticated;
                        cx.notify();
                    }
                    Ok(StartOutcome::SingleUser {
                        credential,
                        cached,
                        session_notice,
                    }) => {
                        this.session_notice = session_notice.map(SharedString::from);
                        this.signed_in_with_oauth = credential.is_oauth();
                        this.oauth_scope = credential.scope().map(str::to_string);
                        this.source = Some(TimelineSource::SingleUser);
                        this.client = Some(XClient::new(credential.token().to_string()));
                        match cached {
                            Some(items) => {
                                this.state = TimelineState::Loaded(items);
                                cx.notify();
                            }
                            // A cache miss at startup, not a user action —
                            // subject to #10's interval like any other
                            // unsolicited fetch (though `last_reload_at` is
                            // still `None` here, so it never actually waits).
                            None => this.reload(ReloadTrigger::Polling, cx),
                        }
                    }
                    Ok(StartOutcome::Home {
                        credential,
                        cached,
                        session_notice,
                    }) => {
                        this.session_notice = session_notice.map(SharedString::from);
                        this.signed_in_with_oauth = credential.is_oauth();
                        this.oauth_scope = credential.scope().map(str::to_string);
                        this.source = Some(TimelineSource::Home);
                        this.client = Some(XClient::new(credential.token().to_string()));
                        match cached {
                            Some((me, items)) => {
                                this.home_user_id = Some(me.id);
                                this.home_username = Some(me.username);
                                this.state = TimelineState::Loaded(items);
                                cx.notify();
                            }
                            // Same reasoning as `SingleUser` above.
                            None => this.reload(ReloadTrigger::Polling, cx),
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
    /// spawning anything, unless `trigger` is [`ReloadTrigger::UserAction`]
    /// (#57) — see that variant's doc for why some callers must bypass it.
    /// When it does apply, [`reload_cooldown`] is a client-side throttle on
    /// the button itself, checked without touching the network, on top of
    /// (not instead of) whatever the tracked API rate-limit state says once
    /// a request actually goes out.
    ///
    /// Neither a cooldown nor a failed fetch touches `state` while it
    /// already holds posts (#57): [`reload_start_state`] and
    /// [`reload_failure_outcome`] are the pure functions that decide this,
    /// and `reload_notice` carries the cooldown/failure text independently —
    /// see [`ReloadNotice`]'s doc. A reload that hasn't loaded anything yet
    /// still falls back to `TimelineState::Loading`/`RateLimited`/`Failed`,
    /// since there is nothing else the body could render in that case.
    fn reload(&mut self, trigger: ReloadTrigger, cx: &mut Context<'_, Self>) {
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
        if let Some(reset_at) = reload_gate(
            trigger,
            self.last_reload_at,
            self.config.min_fetch_interval_seconds,
            now,
        ) {
            // #57: the cooldown blocked the request before it was even
            // sent, so whatever is already on screen is untouched — this is
            // a notice, not a state change.
            self.reload_notice = Some(ReloadNotice::Cooldown {
                reset_at,
                cooldown: Cooldown::LocalInterval,
            });
            // #57 item 3: without this the banner's countdown freezes at
            // whatever second it happened to render on — see
            // `start_cooldown_ticker`'s doc.
            self.start_cooldown_ticker(cx);
            cx.notify();
            return;
        }
        self.last_reload_at = Some(now);

        self.reload_notice = None;
        // A fresh reload actually going out supersedes whatever cooldown
        // was being counted down (there is nothing left to wait for once
        // the request is in flight) — stop it explicitly rather than
        // leaving it to notice on its next tick, up to a second later.
        self.cooldown_ticker = None;
        self.reloading = true;
        self.state = reload_start_state(std::mem::replace(&mut self.state, TimelineState::Loading));

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
                        this.refresh_reposted_ids(cx);
                        this.refresh_liked_ids(cx);
                        this.refresh_avatars(cx);
                        this.reloading = false;
                        match result {
                            Ok(reloaded) => {
                                // Single-user mode has no pagination cursor —
                                // #11 keeps its "Load older" button reserved
                                // for the home timeline.
                                this.next_page_token = None;
                                this.state = TimelineState::Loaded(reloaded.items);
                                this.reload_notice = None;
                                // #57 item 3: a success means there is
                                // nothing left to count down — stop
                                // immediately rather than let the ticker
                                // notice on its next tick, up to a second
                                // later.
                                this.cooldown_ticker = None;
                            }
                            Err(error) => this.apply_reload_failure(&error, cx),
                        }
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
                        this.refresh_reposted_ids(cx);
                        this.refresh_liked_ids(cx);
                        this.refresh_avatars(cx);
                        this.reloading = false;
                        match result {
                            Ok(reloaded) => {
                                this.home_user_id = Some(reloaded.me.id);
                                this.home_username = Some(reloaded.me.username);
                                this.next_page_token = reloaded.next_token;
                                this.state = TimelineState::Loaded(reloaded.items);
                                this.reload_notice = None;
                                // Same reasoning as the single-user branch above.
                                this.cooldown_ticker = None;
                            }
                            Err(error) => this.apply_reload_failure(&error, cx),
                        }
                        cx.notify();
                    });
                }));
            }
        }

        cx.notify();
    }

    /// Shared `Err` handling for both of [`Self::reload`]'s fetch branches
    /// and [`Self::load_older`] (#57): existing posts survive a failed
    /// fetch via [`reload_failure_outcome`] — pulled into its own method
    /// partly to keep `reload` itself under clippy's line-count lint, partly
    /// so all three call sites apply the exact same `Option<ReloadNotice>`
    /// (and, since #57's item 3, ticker) handling below rather than three
    /// copies that could drift.
    fn apply_reload_failure(&mut self, error: &anyhow::Error, cx: &mut Context<'_, Self>) {
        let (state, notice) = reload_failure_outcome(
            std::mem::replace(&mut self.state, TimelineState::Loading),
            error,
        );
        self.state = state;
        // #57: `reload_failure_outcome` already returns `None` when `state`
        // itself now tells the failure story (`Failed`/`RateLimited`) — see
        // its doc. Passing that straight through, rather than wrapping in
        // `Some`, is what stops the same failure from showing twice.
        self.reload_notice = notice;
        // A rate-limited failure raises a fresh `Cooldown` notice (X's own
        // window, not #10's local one, but the countdown still needs to
        // tick the same way) — start/replace the ticker for it. Any other
        // outcome (`Failed`, or no notice at all) has nothing left to count
        // down, so stop whatever ticker might still be running rather than
        // let it keep polling a notice it no longer applies to.
        if matches!(self.reload_notice, Some(ReloadNotice::Cooldown { .. })) {
            self.start_cooldown_ticker(cx);
        } else {
            self.cooldown_ticker = None;
        }
    }

    /// Ticks `reload_notice`'s countdown once a second (#57's item 3) —
    /// see [`cooldown_ticker`](Self::cooldown_ticker)'s doc for why this
    /// exists at all. Started only when `reload_notice` is actually set to
    /// `ReloadNotice::Cooldown` (there is nothing to count down for a
    /// `Failed` notice), from [`Self::reload`]'s cooldown-gate branch and
    /// from [`Self::apply_reload_failure`].
    ///
    /// The loop re-checks [`cooldown_tick`] against the *current*
    /// `reload_notice` on every wake-up rather than trusting `reset_at`
    /// captured at start time: `reload_notice` can change out from under a
    /// running ticker (a reload succeeds and clears it, or a later failure
    /// replaces it with `Failed`) without anyone reaching back in to cancel
    /// this specific loop, and re-checking is what makes that safe — the
    /// loop simply stops the next time it wakes rather than clobbering
    /// whatever is there by then. It also always terminates: either
    /// `cooldown_tick` returns `NotTicking`/`Elapsed`, or `this.update`
    /// returns `Err` because the view itself has been dropped — there is no
    /// path that loops forever.
    fn start_cooldown_ticker(&mut self, cx: &mut Context<'_, Self>) {
        self.cooldown_ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;

                let Ok(keep_going) = this.update(cx, |this, cx| {
                    match cooldown_tick(this.reload_notice.as_ref(), oauth::unix_now()) {
                        CooldownTick::StillWaiting => {
                            cx.notify();
                            true
                        }
                        CooldownTick::Elapsed => {
                            this.reload_notice = None;
                            cx.notify();
                            false
                        }
                        CooldownTick::NotTicking => false,
                    }
                }) else {
                    // The view has been dropped — nothing left to tick.
                    return;
                };

                if !keep_going {
                    return;
                }
            }
        }));
    }

    /// Fetch the page behind `next_page_token` and append it after what's
    /// already shown (#11's "Load older") — only ever meaningful in
    /// `TimelineSource::Home`, since `SingleUser` mode never sets a token in
    /// the first place. A no-op if any of the three prerequisites (a client,
    /// a known home user id, a token to resume from) is missing.
    ///
    /// Shares [`Self::reload`]'s "don't evict what's already on screen"
    /// fix (#57) via the same pure functions, [`reload_start_state`] and
    /// [`reload_failure_outcome`] — arguably *more* important here than for
    /// a plain reload: this only ever runs once something is already
    /// `Loaded` (see [`offers_load_older`]'s gate), and it's paging
    /// *backwards* from what's currently shown, so losing it mid-request
    /// would be strictly worse than a failed reload starting from nothing.
    /// Reuses `self.reloading` for the busy indicator rather than a
    /// dedicated flag — the header's "Loading…" label is an accurate
    /// description of either fetch, and #57 only asked this call site to
    /// stop discarding posts, not to grow "Load older"-specific chrome (the
    /// row itself carries no separate busy/disabled styling, unchanged from
    /// before this fix).
    fn load_older(&mut self, cx: &mut Context<'_, Self>) {
        let (Some(client), Some(user_id), Some(token)) = (
            self.client.clone(),
            self.home_user_id.clone(),
            self.next_page_token.clone(),
        ) else {
            return;
        };

        self.reload_notice = None;
        // Same reasoning as `reload`'s own gate-passed branch: a fetch is
        // about to go out, so any cooldown countdown still ticking (from an
        // unrelated blocked reload) no longer describes anything current.
        self.cooldown_ticker = None;
        self.reloading = true;
        self.state = reload_start_state(std::mem::replace(&mut self.state, TimelineState::Loading));

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
                this.refresh_reposted_ids(cx);
                this.refresh_liked_ids(cx);
                this.refresh_avatars(cx);
                this.reloading = false;
                match result {
                    Ok((items, next_token)) => {
                        this.next_page_token = next_token;
                        this.state = TimelineState::Loaded(items);
                        this.reload_notice = None;
                        // Same reasoning as `reload`'s success branches above.
                        this.cooldown_ticker = None;
                    }
                    Err(error) => this.apply_reload_failure(&error, cx),
                }
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
                .spawn(async move {
                    usage::load_all(&paths).map(|entries| usage::totals(&entries, now))
                })
                .await;

            if let Ok(totals) = result {
                let _ = this.update(cx, |this, cx| {
                    this.usage_totals = totals;
                    cx.notify();
                });
            }
        }));
    }

    /// Refresh `self.reposted_ids` from the local repost record (#15)
    /// whenever the visible timeline changes — mirrors
    /// [`Self::refresh_usage`]'s pattern exactly: read on the background
    /// executor so a slow disk read never blocks rendering, and a read that
    /// fails just leaves whatever was already shown rather than failing the
    /// fetch it rode in on. This file is the project's only source for "did
    /// I repost this" (#15's whole reason for existing — the X API itself
    /// carries no such field), so a stale or lost read here can only ever
    /// under- or over-report *this app's own* reposts, never one made from
    /// another client, which this issue accepts as out of scope regardless.
    fn refresh_reposted_ids(&mut self, cx: &mut Context<'_, Self>) {
        let paths = self.paths.clone();
        self.reposted_ids_refresh = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { repost::load_all(&paths) })
                .await;

            if let Ok(ids) = result {
                let _ = this.update(cx, |this, cx| {
                    this.reposted_ids = ids;
                    cx.notify();
                });
            }
        }));
    }

    /// Refresh `self.liked_ids` from the local like record (#68) — the
    /// like-side twin of [`Self::refresh_reposted_ids`], with the same
    /// read-off-the-main-thread and failure-is-not-fatal contract. Called
    /// from exactly the same places, so a row's like button and its repost
    /// button are never seeded from different points in time.
    fn refresh_liked_ids(&mut self, cx: &mut Context<'_, Self>) {
        let paths = self.paths.clone();
        self.liked_ids_refresh = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { like::load_all(&paths) })
                .await;

            if let Ok(ids) = result {
                let _ = this.update(cx, |this, cx| {
                    this.liked_ids = ids;
                    cx.notify();
                });
            }
        }));
    }

    /// The like button state to render for `post_id` (#68) — see
    /// [`Self::repost_state_for`], which this mirrors.
    fn like_state_for(&self, post_id: &str) -> ToggleState {
        self.like_overrides
            .get(post_id)
            .cloned()
            .unwrap_or_else(|| ToggleState::new(self.liked_ids.contains(post_id)))
    }

    /// Toggle one post's like state (#68) — the like-side twin of
    /// [`Self::toggle_repost`], down to the optimistic flip, the background
    /// request, and folding the result (including `like::create`/
    /// `like::remove`'s own reconciliation) back onto the same per-post
    /// state.
    ///
    /// The scope checked here is `like.write`, not `tweet.write`: X grants
    /// them separately, so a session authorized before #68 can post and
    /// repost but not like. It reuses #14's "Re-authorize" affordance all
    /// the same, since re-running the flow requests every scope at once.
    fn toggle_like(&mut self, post_id: String, cx: &mut Context<'_, Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(user_id) = self.home_user_id.clone() else {
            return;
        };

        let mut state = self.like_state_for(&post_id);
        if !state.can_toggle() {
            return;
        }

        if !oauth::tokens::has_scope(self.oauth_scope.as_deref(), oauth::tokens::LIKE_WRITE_SCOPE) {
            state.refuse(
                "This session can't like yet — click \"Re-authorize\" above first.".to_string(),
            );
            self.like_overrides.insert(post_id, state);
            cx.notify();
            return;
        }

        let creating = !state.is_on();
        state.start_toggle();
        self.like_overrides.insert(post_id.clone(), state);
        cx.notify();

        let paths = self.paths.clone();
        let update_key = post_id.clone();
        let task_key = post_id.clone();

        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    if creating {
                        like::create(&paths, &client, &user_id, &post_id, oauth::unix_now())
                    } else {
                        like::remove(&paths, &client, &user_id, &post_id, oauth::unix_now())
                    }
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                this.refresh_usage(cx);
                let mut state = this
                    .like_overrides
                    .remove(&update_key)
                    .unwrap_or_else(|| ToggleState::new(!creating));
                state.apply_result(result.map_err(|error| format!("{error:#}")));
                this.like_overrides.insert(update_key.clone(), state);
                this.like_tasks.remove(&update_key);
                cx.notify();
            });
        });
        self.like_tasks.insert(task_key, task);
    }

    /// Download whatever avatars the visible timeline needs and don't have
    /// yet (#64).
    ///
    /// Called wherever [`Self::refresh_reposted_ids`] is, so a row's avatar
    /// and its buttons come from the same point in time. Fetching happens on
    /// the background executor one URL at a time, updating the map (and so
    /// the view) after each — an avatar appearing as it arrives beats the
    /// whole timeline waiting for the slowest one. A URL that fails is
    /// simply left absent, so the row keeps its placeholder and the next
    /// reload retries it; there is nothing useful to say to the user about
    /// an avatar that didn't load.
    ///
    /// These requests go to `pbs.twimg.com`, not the X API: no quota, no
    /// credits, nothing for #18's usage tracking to count.
    fn refresh_avatars(&mut self, cx: &mut Context<'_, Self>) {
        let TimelineState::Loaded(items) = &self.state else {
            return;
        };
        let mut wanted: Vec<String> = Vec::new();
        for url in items
            .iter()
            .filter_map(|item| item.author_avatar_url.as_deref())
        {
            if !self.avatar_paths.contains_key(url) && !wanted.iter().any(|seen| seen == url) {
                wanted.push(url.to_string());
            }
        }
        if wanted.is_empty() {
            return;
        }

        let paths = self.paths.clone();
        self.avatar_fetch = Some(cx.spawn(async move |this, cx| {
            for url in wanted {
                let paths = paths.clone();
                let fetch_url = url.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move { avatar::ensure_cached(&paths, &fetch_url) })
                    .await;

                if let Ok(path) = result {
                    let _ = this.update(cx, |this, cx| {
                        this.avatar_paths.insert(url.clone(), path);
                        cx.notify();
                    });
                }
            }
        }));
    }

    /// One post's author avatar (#64): the downloaded image once it is on
    /// disk, else [`avatar_placeholder`] — the two are the same size, so a
    /// row does not reflow when the image lands.
    fn avatar(&self, item: &TimelineItem, theme: Theme) -> AnyElement {
        let cached = item
            .author_avatar_url
            .as_deref()
            .and_then(|url| self.avatar_paths.get(url));

        match cached {
            Some(path) => img(path.clone())
                .size(AVATAR_SIZE)
                .rounded_full()
                .into_any_element(),
            None => avatar_placeholder(&item.author_name, theme),
        }
    }

    /// Hand `url` to the system browser (#70).
    ///
    /// Runs on the background executor rather than in the click handler:
    /// spawning a process is a syscall the UI thread has no reason to wait
    /// on. A refusal or a failure to launch is reported through
    /// `open_failure`, which the row renders — a click that silently does
    /// nothing is the one outcome worth avoiding here.
    fn open_in_browser(&mut self, url: String, cx: &mut Context<'_, Self>) {
        self.open_failure = None;
        self.open_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { browser::open(&url) })
                .await;

            if let Err(error) = result {
                let _ = this.update(cx, |this, cx| {
                    this.open_failure = Some(format!("{error:#}"));
                    cx.notify();
                });
            }
        }));
    }

    /// The like/unlike toggle for one post (#68), rendered whenever
    /// [`offers_like`] allows it for `item`.
    fn like_button(&self, item: &TimelineItem, cx: &mut Context<'_, Self>) -> AnyElement {
        let state = self.like_state_for(&item.id);
        like_row(&item.id, &state, self.theme, cx)
    }

    /// The button state to render for `post_id` (#15): whatever this
    /// session already knows (in flight, failed, or a value a finished
    /// request already confirmed) if there is one, else the plain on/off
    /// value from the local record `refresh_reposted_ids` last read.
    fn repost_state_for(&self, post_id: &str) -> ToggleState {
        self.repost_overrides
            .get(post_id)
            .cloned()
            .unwrap_or_else(|| ToggleState::new(self.reposted_ids.contains(post_id)))
    }

    /// Toggle one post's repost state (#15): flip the button immediately
    /// (never waiting on the network — mirrors #14's synchronous
    /// `start_submitting`), then run the actual create/delete request on
    /// the background executor and apply whatever it resolves to —
    /// including any error-reconciliation `repost::create`/`repost::remove`
    /// already folded into their own `Result<bool>` — back onto the same
    /// per-post state.
    ///
    /// No-ops without a client or a resolved `home_user_id` — the repost
    /// endpoints act as *this* account, whose id only `/me` (#11) resolves,
    /// so there is nothing to call yet if it hasn't. The `tweet.write`
    /// scope check mirrors `submit_post`'s exactly, reusing #14's own
    /// "Re-authorize" affordance rather than a parallel check, per #15's
    /// explicit instruction.
    fn toggle_repost(&mut self, post_id: String, cx: &mut Context<'_, Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(user_id) = self.home_user_id.clone() else {
            return;
        };

        let mut state = self.repost_state_for(&post_id);
        if !state.can_toggle() {
            return;
        }

        if !oauth::tokens::has_scope(
            self.oauth_scope.as_deref(),
            oauth::tokens::TWEET_WRITE_SCOPE,
        ) {
            state.refuse(
                "This session can't repost yet — click \"Re-authorize\" above first.".to_string(),
            );
            self.repost_overrides.insert(post_id, state);
            cx.notify();
            return;
        }

        let creating = !state.is_on();
        state.start_toggle();
        self.repost_overrides.insert(post_id.clone(), state);
        cx.notify();

        let paths = self.paths.clone();
        let update_key = post_id.clone();
        let task_key = post_id.clone();

        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    if creating {
                        repost::create(&paths, &client, &user_id, &post_id, oauth::unix_now())
                    } else {
                        repost::remove(&paths, &client, &user_id, &post_id, oauth::unix_now())
                    }
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                this.refresh_usage(cx);
                let mut state = this
                    .repost_overrides
                    .remove(&update_key)
                    .unwrap_or_else(|| ToggleState::new(!creating));
                state.apply_result(result.map_err(|error| format!("{error:#}")));
                this.repost_overrides.insert(update_key.clone(), state);
                this.repost_tasks.remove(&update_key);
                cx.notify();
            });
        });
        self.repost_tasks.insert(task_key, task);
    }

    /// The repost/un-repost toggle for one post (#15), rendered whenever
    /// [`offers_repost`] allows it for `item`.
    fn repost_button(&self, item: &TimelineItem, cx: &mut Context<'_, Self>) -> AnyElement {
        let state = self.repost_state_for(&item.id);
        repost_row(&item.id, &state, self.theme, cx)
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
                    // #14: the freshly granted scope — this is what makes
                    // `offers_reauthorize` stop offering the button right
                    // after a successful re-authorization.
                    this.oauth_scope.clone_from(&tokens.scope);
                    // #54: a fresh sign-in fixes whatever the banner was
                    // reporting — an expired session can't stay expired past
                    // a brand-new one.
                    this.session_notice = None;
                    // #11: a stored OAuth session always maps to the home
                    // timeline — see `TimelineSource::for_credential`.
                    this.source = Some(TimelineSource::Home);
                    this.client = Some(XClient::new(tokens.access_token));
                    // #57: confirms what the user just did — must not wait
                    // out #10's interval, which exists to suppress polling,
                    // not to gate a direct response to a user action.
                    this.reload(ReloadTrigger::UserAction, cx);
                }
                Err(error) => {
                    this.state = TimelineState::Failed(format!("{error:#}").into());
                    cx.notify();
                }
            });
        }));

        cx.notify();
    }

    /// Mirror `compose_input`'s buffer into `self.compose` on every
    /// `InputEvent::Change` (#38) — see the `compose_input` field doc for
    /// why the mirror exists at all rather than `compose.rs` reading the
    /// widget directly. `PressEnter`/`Focus`/`Blur` carry nothing this view
    /// needs: multi-line mode already turns Enter into a newline inside the
    /// widget itself (`InputState::enter`), so `PressEnter` here would only
    /// ever fire for a plain scroll-into-view, not a submit.
    // `Context::subscribe`'s callback bound requires `Entity<T2>` by value,
    // not `&Entity<T2>` — there's nothing to change on this end.
    #[allow(clippy::needless_pass_by_value)]
    fn on_compose_input_event(
        &mut self,
        input: Entity<InputState>,
        event: &InputEvent,
        cx: &mut Context<'_, Self>,
    ) {
        if let InputEvent::Change = event {
            self.compose.set_text(input.read(cx).value().to_string());
            cx.notify();
        }
    }

    /// The quote target shown inside the composer, if `compose.quote()` has
    /// one (#16). Reuses #13's [`quote_card`] rendering rather than a second
    /// one, with a "Remove quote" control added below it so a mis-click on
    /// "Quote" doesn't force discarding the whole draft — that goes through
    /// `ComposeState::clear_quote`, never `submit_post`, so the draft text
    /// is untouched either way.
    fn composer_quote_card(
        &self,
        target: &compose::QuoteTarget,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let theme = self.theme;
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(quote_card(&target.quoted, theme))
            .child(
                div()
                    .id("compose-remove-quote")
                    .text_color(rgb(theme.accent))
                    .child("Remove quote")
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.compose.clear_quote();
                        cx.notify();
                    })),
            )
    }

    /// The post composer (#14): a real text input (#38), character counter,
    /// and submit button. Shown whenever the session is signed in with
    /// OAuth — see [`Render::render`]'s doc on why a missing `tweet.write`
    /// scope doesn't hide this entirely. #16 adds the quote target card, when
    /// one is set — see [`Self::composer_quote_card`].
    fn composer(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = self.theme;
        let text = self.compose.text().to_string();
        let length = compose::weighted_length(&text);
        let over_limit = length > compose::MAX_WEIGHTED_LENGTH;
        let can_submit = self.compose.can_submit();
        let is_submitting = self.compose.is_submitting();
        let counter_color = if over_limit {
            theme.danger
        } else {
            theme.text_muted
        };

        div()
            .flex()
            .flex_col()
            .gap_2()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(rgb(theme.border))
            // Refuses edits while a submit is in flight, mirroring the
            // submit button's own disabled state below — see
            // `ComposeState::can_submit`'s doc for why that matters.
            .child(Input::new(&self.compose_input).disabled(is_submitting))
            // #16: the quote target, when "Quote" set one — see
            // `composer_quote_card`'s doc.
            .when_some(self.compose.quote(), |column, target| {
                column.child(self.composer_quote_card(target, cx))
            })
            .when_some(
                compose_error_message(self.compose.status()),
                |column, message| column.child(div().text_color(rgb(theme.danger)).child(message)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_color(rgb(counter_color))
                            .child(format!("{length}/{}", compose::MAX_WEIGHTED_LENGTH)),
                    )
                    .child(
                        div()
                            .id("compose-submit")
                            .px_3()
                            .py_1()
                            .rounded_full()
                            .bg(rgb(if can_submit {
                                theme.accent
                            } else {
                                theme.button_busy_bg
                            }))
                            .text_color(rgb(theme.button_label))
                            .child(if is_submitting { "Posting…" } else { "Post" })
                            // #14's double-submit guard, part two: while a
                            // submit is in flight (or the draft is blank/
                            // over-length) the button carries no click
                            // handler at all, not just a disabled-looking
                            // style — `submit_post` re-checks the same
                            // condition regardless, but this is what stops
                            // the click from ever reaching it.
                            .when(can_submit, |button| {
                                button.on_click(cx.listener(|this, _event, window, cx| {
                                    this.submit_post(window, cx);
                                }))
                            }),
                    ),
            )
    }

    /// Submit the composer's current draft as a new post (#14), quoting
    /// whatever [`ComposeState::quote`] currently holds, if anything (#16).
    ///
    /// Refuses to do anything — without spawning a task or touching the
    /// network — unless [`ComposeState::can_submit`] says yes. This is
    /// also what rules out a double submit: `can_submit` depends on
    /// `compose.status`, and the very next statement below (once every
    /// guard has passed) sets that status to `Submitting` *synchronously*,
    /// before this function returns to gpui's event loop or yields to the
    /// background executor via `cx.spawn`. gpui runs one click handler to
    /// completion before dispatching the next input event, so a second
    /// click — however fast — calls `submit_post` again only after this
    /// one has already returned, by which point `can_submit` is false and
    /// the function returns immediately at the top. No task is spawned, and
    /// `submit_task` is never overwritten mid-flight.
    ///
    /// The scope check below is deliberately not part of `ComposeState`:
    /// that type only knows about the draft *text*, not the session's
    /// OAuth scope, so a missing `tweet.write` is refused here — before
    /// spending a request that's guaranteed to 403 — via
    /// `ComposeState::refuse` rather than `can_submit`. The header's
    /// "Re-authorize" button (see `offers_reauthorize`) is the actual fix.
    ///
    /// Takes `window` (unlike most of this file's other actions) because
    /// #38's success path needs it: clearing `compose_input`'s own buffer —
    /// see the field's doc — goes through `InputState::set_value`, which
    /// requires one. `cx.spawn_in`/`WeakEntity::update_in` (rather than the
    /// plain `cx.spawn`/`update` this struct's other actions use) carry a
    /// `Window` across the `await` for exactly that.
    fn submit_post(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if !self.compose.can_submit() {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        if !oauth::tokens::has_scope(
            self.oauth_scope.as_deref(),
            oauth::tokens::TWEET_WRITE_SCOPE,
        ) {
            self.compose.refuse(
                "This session can't post yet — click \"Re-authorize\" above first.".to_string(),
            );
            cx.notify();
            return;
        }

        self.compose.start_submitting();
        cx.notify();

        let paths = self.paths.clone();
        let text = self.compose.text().to_string();
        // #16: whichever post (if any) "Quote" set as the target — cloned
        // out before the closure below moves `self.compose` implicitly via
        // `apply_result`'s mutation, same as `text` above.
        let quote_tweet_id = self.compose.quote().map(|target| target.post_id.clone());

        self.submit_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    client.create_post(&paths, &text, quote_tweet_id.as_deref(), oauth::unix_now())
                })
                .await;

            let _ = this.update_in(cx, |this, window, cx| {
                let succeeded = result.is_ok();
                this.compose
                    .apply_result(result.map_err(|error| format!("{error:#}")));
                if succeeded {
                    // `apply_result`'s `Ok` branch just cleared the mirror in
                    // `this.compose`, but `compose_input` is the widget's own,
                    // entirely separate buffer (#38) — this is what actually
                    // empties the box the user sees.
                    this.compose_input.update(cx, |state, cx| {
                        state.set_value("", window, cx);
                    });
                    // A successful post changes the timeline, so fall into
                    // a reload — but #57: this is confirming the result of
                    // what the user just did (and the post itself already
                    // spent a request), not polling, so it must bypass
                    // #10's interval rather than risk being blocked by it.
                    this.reload(ReloadTrigger::UserAction, cx);
                }
                cx.notify();
            });
        }));
    }

    fn header(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        // #57: checked ahead of `state` rather than folded into its match —
        // a reload in flight while posts are already showing leaves `state`
        // as `Loaded` (see `reload_start_state`), so this is the only signal
        // that a fetch is running in that case.
        let (label, busy, action) = if self.reloading {
            ("Loading…".to_string(), true, PrimaryAction::Reload)
        } else {
            match self.state {
                TimelineState::Loading => ("Loading…".to_string(), true, PrimaryAction::Reload),
                TimelineState::SigningIn => {
                    ("Signing in…".to_string(), true, PrimaryAction::SignIn)
                }
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
                        |row| row.child(sign_in_pill("sign-in", "Sign in with X", theme, cx)),
                    )
                    // #14: an already-signed-in session from before #14
                    // holds no `tweet.write` scope — #31's exact lesson
                    // repeats here (an already-active session hides its own
                    // upgrade path) unless this stays reachable regardless
                    // of what the primary button currently says.
                    .when(
                        offers_reauthorize(self.signed_in_with_oauth, self.oauth_scope.as_deref()),
                        |row| row.child(sign_in_pill("reauthorize", "Re-authorize", theme, cx)),
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
                                PrimaryAction::Reload => this.reload(ReloadTrigger::Polling, cx),
                                PrimaryAction::SignIn => this.sign_in(cx),
                            })),
                    ),
            )
    }

    fn post_row(&self, item: &TimelineItem, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = self.theme;
        let byline = byline(&item.author_username);

        // #64: the avatar sits in its own column to the left, so the body
        // below is built separately and then placed beside it.
        let body = div()
            .flex()
            .flex_col()
            .flex_1()
            .gap_1()
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
                    // #70: the author name and handle open the profile on
                    // x.com. `profile_url` returns `None` when the username
                    // never expanded, in which case they stay plain text
                    // rather than becoming a link to nowhere.
                    .child(author_link(item, theme, cx))
                    .child(div().text_color(rgb(theme.text_muted)).child(byline))
                    .child(
                        div()
                            .text_color(rgb(theme.text_muted))
                            .child(format_timestamp(item.created_at.as_deref())),
                    )
                    // #70: the post itself, on x.com.
                    .child(open_post_link(item, theme, cx)),
            )
            .child(div().child(item.text.clone()))
            // #67: reply/repost/like counts, muted and only when there is
            // something to show. They ride along in the timeline response,
            // so this costs no extra request — but they are a snapshot from
            // when the row was fetched (see `x_api::model::PostMetrics`).
            .when_some(
                item.metrics.as_ref().and_then(metrics_label),
                |column, label| column.child(div().text_color(rgb(theme.text_muted)).child(label)),
            )
            // #70: the links in the body, expanded out of the `t.co`
            // shortlinks the text carries — see `link_row`'s doc for why
            // they sit under the text rather than inside it.
            .when(!item.links.is_empty(), |column| {
                column.child(link_row(&item.links, theme, cx))
            })
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
            // #15: repost/un-repost — see `offers_repost`'s doc for exactly
            // which posts get one.
            .when(
                offers_repost(
                    self.signed_in_with_oauth,
                    self.home_user_id.as_deref(),
                    self.home_username.as_deref(),
                    item,
                ),
                |column| column.child(self.repost_button(item, cx)),
            )
            // #16: "Quote" — see `offers_quote`'s doc for exactly which
            // posts get one (a repost row is withheld for the same reason
            // `offers_repost` withholds its own button).
            .when(offers_quote(self.signed_in_with_oauth, item), |column| {
                column.child(quote_row(item, theme, cx))
            })
            // #68: like/unlike — see `offers_like`'s doc for which posts
            // get one. Unlike repost, this is offered on one's own posts.
            .when(
                offers_like(
                    self.signed_in_with_oauth,
                    self.home_user_id.as_deref(),
                    item,
                ),
                |column| column.child(self.like_button(item, cx)),
            );

        div()
            .flex()
            .gap_3()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(rgb(theme.border))
            .child(self.avatar(item, theme))
            .child(body)
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
            // #54: shown regardless of `state` — the whole defect this
            // fixes is a timeline that renders as if nothing happened, so
            // this banner has to survive independently of whatever `body`
            // below is currently showing.
            .when_some(self.session_notice.clone(), |column, message| {
                column.child(session_notice_banner(message, theme))
            })
            // #57: same reasoning as `session_notice` above — a cooldown or
            // a failed reload must survive independently of `body`, which by
            // this point may well still be showing the previous posts.
            .when_some(self.reload_notice.clone(), |column, notice| {
                column.child(reload_notice_banner(&notice, theme, oauth::unix_now()))
            })
            // #70: a link that failed to open. Same banner treatment as the
            // two above, for the same reason: a click that appears to do
            // nothing is the outcome worth ruling out, and the timeline
            // below has nothing to say about it.
            .when_some(self.open_failure.clone(), |column, message| {
                column.child(session_notice_banner(SharedString::from(message), theme))
            })
            // #14: posting requires OAuth regardless of scope — a missing
            // `tweet.write` scope is caught inside `submit_post` itself
            // (with the header's "Re-authorize" button as the fix), rather
            // than hiding the whole composer and leaving no way to
            // discover why it's gone.
            .when(self.signed_in_with_oauth, |column| {
                column.child(self.composer(cx))
            })
            .child(self.body(cx))
    }
}

/// An outlined pill in the header that starts the sign-in flow.
///
/// #31 (upgrade away from the app-only bearer token) and #14 (the session
/// predates `tweet.write`) are different reasons to reach the same place, so
/// the two buttons differ only in their label — worth one helper rather than
/// two near-identical builder chains that have to be kept in step.
fn sign_in_pill(
    id: &'static str,
    label: &'static str,
    theme: Theme,
    cx: &mut Context<'_, TimelineView>,
) -> impl IntoElement {
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_full()
        .border_1()
        .border_color(rgb(theme.accent))
        .text_color(rgb(theme.accent))
        .child(label)
        .on_click(cx.listener(|this, _event, _window, cx| this.sign_in(cx)))
}

fn notice(message: impl Into<SharedString>, color: u32) -> impl IntoElement {
    div()
        .px_4()
        .py_3()
        .text_color(rgb(color))
        .child(message.into())
}

/// The persistent "your session expired" banner (#54): a distinct row
/// between the header and everything else, rather than folded into
/// [`TimelineView::body`]'s `state`-keyed match — the whole point is that it
/// has to keep showing even while `body` renders a perfectly normal loaded
/// timeline (on the bearer-token fallback), which is exactly the state #54
/// was filed from.
fn session_notice_banner(message: SharedString, theme: Theme) -> impl IntoElement {
    div()
        .px_4()
        .py_2()
        .bg(rgb(theme.bg_header))
        .border_b_1()
        .border_color(rgb(theme.border))
        .text_color(rgb(theme.danger))
        .child(message)
}

/// The reload cooldown/failure banner (#57) — styled identically to, and
/// drawn right next to, [`session_notice_banner`] for the same reason that
/// one is independent of `body`: a cooldown or a failed refresh describes
/// the most recent *request*, not whatever posts are (or aren't) currently
/// shown, and must never read as "there is nothing here" when there is.
fn reload_notice_banner(notice: &ReloadNotice, theme: Theme, now: i64) -> impl IntoElement {
    let message = match *notice {
        ReloadNotice::Cooldown { reset_at, cooldown } => cooldown_label(cooldown, reset_at, now),
        ReloadNotice::Failed(ref message) => message.to_string(),
    };
    div()
        .px_4()
        .py_2()
        .bg(rgb(theme.bg_header))
        .border_b_1()
        .border_color(rgb(theme.border))
        .text_color(rgb(theme.danger))
        .child(message)
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
    match usage::estimated_amount(today, request_price) {
        Some(amount) => format!("Today: {today} req (~{amount:.2}) · Total: {total} req"),
        None => format!("Today: {today} req · Total: {total} req"),
    }
}

/// Which theme slot the usage line renders in: `warning`/`danger` as
/// today's count approaches or crosses `daily_request_budget`, matching the
/// severities [`usage::budget_status`] returns; the same muted slot
/// timestamps and bylines already use once there is nothing to flag.
fn usage_color(status: usage::BudgetStatus, theme: Theme) -> u32 {
    match status {
        usage::BudgetStatus::Ok => theme.text_muted,
        usage::BudgetStatus::Near => theme.warning,
        usage::BudgetStatus::Exceeded => theme.danger,
    }
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

/// The composer's error line, if its status has one to show (#14) — `None`
/// for `Idle`/`Submitting`, so the composer renders no extra row in either
/// of those states.
fn compose_error_message(status: &ComposeStatus) -> Option<SharedString> {
    match status {
        ComposeStatus::Failed(message) => Some(SharedString::from(message.clone())),
        ComposeStatus::Idle | ComposeStatus::Submitting => None,
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

/// Whether the header should offer to re-authorize (#14): the session is
/// already an OAuth one — `offers_sign_in` already covers "not OAuth at
/// all" — but its recorded scope doesn't include what posting needs.
/// Mirrors `offers_sign_in`'s shape rather than folding into it: the two
/// affordances are mutually exclusive by construction (this requires
/// `signed_in_with_oauth`, `offers_sign_in` requires its opposite) and read
/// differently ("Sign in" vs "Re-authorize") — #31's actual lesson was
/// "don't hide the affordance", not "there must be only one button".
///
/// Checks every write scope the app can need, not just #14's: #68 added
/// `like.write`, which X grants separately, so a session authorized before
/// #68 holds `tweet.write` alone. Without this, `toggle_like`'s refusal
/// would point at a "Re-authorize" button that was not being rendered.
fn offers_reauthorize(signed_in_with_oauth: bool, oauth_scope: Option<&str>) -> bool {
    signed_in_with_oauth
        && !(oauth::tokens::has_scope(oauth_scope, oauth::tokens::TWEET_WRITE_SCOPE)
            && oauth::tokens::has_scope(oauth_scope, oauth::tokens::LIKE_WRITE_SCOPE))
}

/// Whether post `item` should offer a repost/un-repost toggle (#15).
///
/// Requires a signed-in OAuth session whose own id has resolved
/// (`home_user_id`, via `/me` — #11): the repost endpoints act as *this*
/// account, and there is nothing to call without it. Withheld for a post
/// that is itself already a repost (`item.reposted_by.is_some()`): a
/// repost-of-a-repost row's `item.id` is the *retweet activity's own* post
/// id (see `x_api::model::build_item`), not the original content's id the
/// repost endpoints actually need, and `TimelineItem` currently carries no
/// separate field for the original — offering the button there would risk
/// sending the wrong id (see the implementation report for this
/// deliberate, documented gap). Withheld for one's own post, matching the
/// API's own rejection (#15) — see [`is_own_post`].
fn offers_repost(
    signed_in_with_oauth: bool,
    home_user_id: Option<&str>,
    home_username: Option<&str>,
    item: &TimelineItem,
) -> bool {
    signed_in_with_oauth
        && home_user_id.is_some()
        && item.reposted_by.is_none()
        && !is_own_post(home_username, &item.author_username)
}

/// Whether `author_username` is the signed-in account's own (#15) — the API
/// rejects reposting your own post, and checking here saves a
/// guaranteed-failing request, mirroring #14's client-side character-limit
/// check. `home_username: None` (not yet resolved) never withholds the
/// button: safer to let an occasional same-account repost through to the
/// API's own rejection than to hide the button for every post before the
/// signed-in identity is known. Case-insensitive since `home_username`
/// (from `/me`) and `author_username` (from the timeline expansion) are
/// resolved independently.
fn is_own_post(home_username: Option<&str>, author_username: &str) -> bool {
    home_username.is_some_and(|home| home.eq_ignore_ascii_case(author_username))
}

/// The repost/un-repost toggle for one post (#15): "Repost" when not
/// reposted, "Reposted" once it is — both clickable (a repost is
/// reversible, so the button doubles as its own undo), styled like
/// [`thread_toggle_row`]. Disabled — no click handler at all, matching
/// #14's double-submit guard — while a request is in flight; a failed
/// attempt shows its message above the (still clickable) toggle, offering a
/// retry.
fn repost_row(
    post_id: &str,
    state: &ToggleState,
    theme: Theme,
    cx: &mut Context<'_, TimelineView>,
) -> AnyElement {
    let label = repost_action_label(state);
    let color = if state.is_on() {
        theme.accent
    } else {
        theme.text_muted
    };

    let toggle = div()
        .id(SharedString::from(format!("repost-{post_id}")))
        .text_color(rgb(color))
        .child(label)
        .when(state.can_toggle(), |element| {
            let id = post_id.to_string();
            element.on_click(cx.listener(move |this, _event, _window, cx| {
                this.toggle_repost(id.clone(), cx);
            }))
        });

    if let ToggleStatus::Failed(message) = state.status() {
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

/// The clickable label for [`repost_row`] (#15): the pending direction
/// while a request is in flight, else the plain on/off label.
fn repost_action_label(state: &ToggleState) -> &'static str {
    if matches!(state.status(), ToggleStatus::Pending) {
        if state.is_on() {
            "Reposting…"
        } else {
            "Removing repost…"
        }
    } else if state.is_on() {
        "Reposted"
    } else {
        "Repost"
    }
}

/// The like/unlike toggle for one post (#68): "Like" when not liked,
/// "Liked" once it is — both clickable, styled like [`repost_row`], which
/// this mirrors down to the disabled-while-pending rule and the failure
/// message rendered above a still-clickable toggle.
fn like_row(
    post_id: &str,
    state: &ToggleState,
    theme: Theme,
    cx: &mut Context<'_, TimelineView>,
) -> AnyElement {
    let label = like_action_label(state);
    let color = if state.is_on() {
        theme.accent
    } else {
        theme.text_muted
    };

    let toggle = div()
        .id(SharedString::from(format!("like-{post_id}")))
        .text_color(rgb(color))
        .child(label)
        .when(state.can_toggle(), |element| {
            let id = post_id.to_string();
            element.on_click(cx.listener(move |this, _event, _window, cx| {
                this.toggle_like(id.clone(), cx);
            }))
        });

    if let ToggleStatus::Failed(message) = state.status() {
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

/// The clickable label for [`like_row`] (#68): the pending direction while
/// a request is in flight, else the plain on/off label.
fn like_action_label(state: &ToggleState) -> &'static str {
    if matches!(state.status(), ToggleStatus::Pending) {
        if state.is_on() {
            "Liking…"
        } else {
            "Unliking…"
        }
    } else if state.is_on() {
        "Liked"
    } else {
        "Like"
    }
}

/// Whether post `item` should offer a like/unlike toggle (#68).
///
/// Requires a signed-in OAuth session whose own id has resolved
/// (`home_user_id`, via `/me` — #11), for the same reason [`offers_repost`]
/// does: the likes endpoints act as *this* account.
///
/// Two deliberate differences from [`offers_repost`]:
///
/// - **No [`is_own_post`] check.** X rejects reposting your own post but
///   accepts liking it, so #68 explicitly instructs against carrying #15's
///   guard over.
/// - **The repost-row guard stays.** A row that is itself a repost
///   (`item.reposted_by.is_some()`) still gets no button: `item.id` is the
///   retweet activity's own id, not the original content's, and
///   `TimelineItem` carries no separate field for the original — liking it
///   would send the wrong id. That gap is #52, shared with #15/#16.
fn offers_like(
    signed_in_with_oauth: bool,
    home_user_id: Option<&str>,
    item: &TimelineItem,
) -> bool {
    signed_in_with_oauth && home_user_id.is_some() && item.reposted_by.is_none()
}

/// The author's name, as a link to their profile on x.com (#70) — or as
/// plain bold text when the username never expanded and [`profile_url`]
/// has nowhere to point.
fn author_link(
    item: &TimelineItem,
    theme: Theme,
    cx: &mut Context<'_, TimelineView>,
) -> AnyElement {
    let name = div()
        .font_weight(FontWeight::BOLD)
        .child(item.author_name.clone());

    match profile_url(&item.author_username) {
        Some(url) => name
            .id(SharedString::from(format!("profile-{}", item.id)))
            .text_color(rgb(theme.accent))
            .on_click(cx.listener(move |this, _event, _window, cx| {
                this.open_in_browser(url.clone(), cx);
            }))
            .into_any_element(),
        None => name.into_any_element(),
    }
}

/// The "Open in X" affordance on one post's byline row (#70) — always
/// offered, since [`post_permalink`] has an id-only fallback for a post
/// whose author never expanded.
fn open_post_link(
    item: &TimelineItem,
    theme: Theme,
    cx: &mut Context<'_, TimelineView>,
) -> impl IntoElement {
    let url = post_permalink(&item.author_username, &item.id);
    div()
        .id(SharedString::from(format!("open-{}", item.id)))
        .text_color(rgb(theme.text_muted))
        .child("Open in X")
        .on_click(cx.listener(move |this, _event, _window, cx| {
            this.open_in_browser(url.clone(), cx);
        }))
}

/// The links from one post's text, as clickable chips under the body (#70).
///
/// Under the text rather than inside it: X's own text carries `t.co`
/// shortlinks, so making the link clickable *in place* would mean splitting
/// the body into interleaved text and link elements, and gpui lays each
/// child out as its own block — the paragraph would stop wrapping as one
/// piece. A row of chips beneath keeps the body intact and still gets the
/// user to the destination, which is what the issue asks for. Each chip is
/// labelled with X's own `display_url` (`example.com/a/b…`), so what is
/// shown matches what the text says even though what is opened is the
/// expanded destination.
fn link_row(links: &[PostLink], theme: Theme, cx: &mut Context<'_, TimelineView>) -> AnyElement {
    let mut row = div().flex().flex_col().gap_1();
    for link in links {
        let url = link.url.clone();
        row = row.child(
            div()
                .id(SharedString::from(format!("link-{url}")))
                .text_color(rgb(theme.accent))
                .child(link.label.clone())
                .on_click(cx.listener(move |this, _event, _window, cx| {
                    this.open_in_browser(url.clone(), cx);
                })),
        );
    }
    row.into_any_element()
}

/// Whether post `item` should offer a "Quote" action (#16).
///
/// Requires the composer to even be reachable — `signed_in_with_oauth`,
/// mirroring [`Render::render`]'s own gate on `self.composer` — since
/// quoting has nowhere to go without one. Withheld for a post that is
/// itself already a repost (`item.reposted_by.is_some()`), for exactly the
/// reason [`offers_repost`] withholds its own button there: `item.id` is
/// the retweet activity's own id, not the original content's, and
/// `TimelineItem` carries no separate field for the original — see
/// [`offers_repost`]'s doc and #52, which tracks fixing that for both
/// buttons together. Unlike [`offers_repost`], quoting one's own post *is*
/// allowed (#16's design decision — the API doesn't reject it the way it
/// rejects reposting yourself), so there is no `is_own_post` check here.
fn offers_quote(signed_in_with_oauth: bool, item: &TimelineItem) -> bool {
    signed_in_with_oauth && item.reposted_by.is_none()
}

/// The "Quote" action for one post (#16), rendered whenever [`offers_quote`]
/// allows it for `item`. Unlike #15's repost toggle this is a one-shot,
/// purely local action, not a per-post request: clicking it only loads the
/// composer's quote target (`ComposeState::set_quote`) so the card renders
/// there — nothing is sent to X until the composer's own "Post" button is
/// clicked, exactly like an ordinary draft.
fn quote_row(item: &TimelineItem, theme: Theme, cx: &mut Context<'_, TimelineView>) -> AnyElement {
    let post_id = item.id.clone();
    let quoted = QuotedPost {
        author_name: item.author_name.clone(),
        author_username: item.author_username.clone(),
        text: item.text.clone(),
    };

    div()
        .id(SharedString::from(format!("quote-{}", item.id)))
        .text_color(rgb(theme.text_muted))
        .child("Quote")
        .on_click(cx.listener(move |this, _event, _window, cx| {
            this.compose.set_quote(compose::QuoteTarget {
                post_id: post_id.clone(),
                quoted: quoted.clone(),
            });
            cx.notify();
        }))
        .into_any_element()
}

/// Classify a failed reload/load-older's error into the [`ReloadNotice`] it
/// should raise (#57) — the single place that decides "rate limit with a
/// known reset time" vs "plain failure", shared by [`map_reload_error`] (the
/// fallback for when there's nothing else to show) and
/// [`reload_failure_outcome`] (the common case, once there's a timeline that
/// must survive the failure).
///
/// #10: a blocked-send carries a known reset time and is shown as a
/// countdown; everything else (including a rate limit whose 429 carried no
/// usable reset header) falls back to the plain error message.
fn reload_notice_for_error(error: &anyhow::Error) -> ReloadNotice {
    match error.downcast_ref::<rate_limit::RateLimited>() {
        Some(rate_limit::RateLimited {
            reset_at: Some(reset_at),
        }) => ReloadNotice::Cooldown {
            reset_at: *reset_at,
            cooldown: Cooldown::ApiRateLimit,
        },
        _ => ReloadNotice::Failed(format!("{error:#}").into()),
    }
}

/// Map a failed reload's error to the state that should show it, for when
/// there is nothing else on screen to fall back to. The only caller left as
/// of #57 is [`reload_failure_outcome`], and only once it has confirmed
/// there is no loaded timeline this failure would otherwise evict —
/// `TimelineView::reload` and `TimelineView::load_older` both reach this
/// exclusively through that path now, never directly.
fn map_reload_error(error: &anyhow::Error) -> TimelineState {
    match reload_notice_for_error(error) {
        ReloadNotice::Cooldown { reset_at, cooldown } => {
            TimelineState::RateLimited { reset_at, cooldown }
        }
        ReloadNotice::Failed(message) => TimelineState::Failed(message),
    }
}

/// What a failed fetch should do to `state`, and which notice (if any) it
/// should raise (#57) — shared by [`TimelineView::reload`] (via
/// `TimelineView::apply_reload_failure`) and `TimelineView::load_older`, a
/// pure function so "an existing timeline survives a failed fetch" can be
/// unit tested without gpui. A failed refresh is not evidence that whatever
/// is already loaded is wrong, so whenever `state` already holds posts, they
/// are returned untouched and the failure becomes a notice, via
/// [`reload_notice_for_error`] — this is the only branch that returns
/// `Some`. When there is nothing being displayed yet, the failure instead
/// becomes the state itself — the same [`map_reload_error`] mapping every
/// other failed fetch in this file uses — and the notice comes back `None`:
/// `state` (`Failed`/`RateLimited`) is already telling the body what
/// happened, so a banner repeating the identical message would just be a
/// duplicated failure on screen.
fn reload_failure_outcome(
    state: TimelineState,
    error: &anyhow::Error,
) -> (TimelineState, Option<ReloadNotice>) {
    match state {
        TimelineState::Loaded(items) => (
            TimelineState::Loaded(items),
            Some(reload_notice_for_error(error)),
        ),
        _ => (map_reload_error(error), None),
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

/// Whether [`TimelineView::reload`] should refuse to run right now, given
/// `trigger` (#57). `ReloadTrigger::UserAction` bypasses [`reload_cooldown`]
/// entirely and always returns `None` — see [`ReloadTrigger`]'s doc for why
/// a post-submit or sign-in reload must never be blocked by an interval that
/// exists to suppress polling, not to gate a direct response to something
/// the user just did. `ReloadTrigger::Polling` defers to `reload_cooldown`
/// unchanged.
fn reload_gate(
    trigger: ReloadTrigger,
    last_reload_at: Option<i64>,
    min_interval_seconds: u32,
    now: i64,
) -> Option<i64> {
    match trigger {
        ReloadTrigger::Polling => reload_cooldown(last_reload_at, min_interval_seconds, now),
        ReloadTrigger::UserAction => None,
    }
}

/// What `state` should become right before spawning a fetch (#57) — shared
/// by [`TimelineView::reload`] and `TimelineView::load_older`, a pure
/// function so "an existing timeline survives a fetch in progress" can be
/// unit tested without gpui. Fetching a fresh copy is not evidence the
/// previous one is stale or wrong, so whenever `previous` already holds
/// posts, this leaves them in place; the header's busy indicator comes from
/// `TimelineView::reloading` instead, set alongside this rather than folded
/// into `state` (see that field's doc). Only when there is nothing loaded
/// yet does this fall back to `TimelineState::Loading`, matching the
/// pre-#57 behavior for the one case where there is nothing to lose.
fn reload_start_state(previous: TimelineState) -> TimelineState {
    match previous {
        TimelineState::Loaded(items) => TimelineState::Loaded(items),
        _ => TimelineState::Loading,
    }
}

/// What one wake-up of [`TimelineView::start_cooldown_ticker`]'s loop should
/// do (#57's item 3), given the current `reload_notice` and the time — the
/// pure decision behind that loop, factored out so it's unit-testable
/// without gpui's timer; the loop itself just matches on this and either
/// keeps going or returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CooldownTick {
    /// Nothing to tick: `reload_notice` is `None`, or holds a `Failed`
    /// notice with no countdown to advance — either it was never a
    /// cooldown, or something else (a success, a plain failure) has
    /// already replaced it since the ticker started. The loop should stop
    /// without touching `reload_notice`.
    NotTicking,
    /// Still inside the cooldown window: re-notify so the banner's
    /// countdown advances, then wait another second.
    StillWaiting,
    /// `reset_at` has passed. The loop should clear `reload_notice` — the
    /// banner disappearing, and "Reload" becoming clickable again, is the
    /// user-visible "done waiting" signal — and stop.
    Elapsed,
}

/// Pure core of [`CooldownTick`]'s decision — see its doc for what each
/// variant means the loop should do.
fn cooldown_tick(notice: Option<&ReloadNotice>, now: i64) -> CooldownTick {
    match notice {
        Some(ReloadNotice::Cooldown { reset_at, .. }) if *reset_at > now => {
            CooldownTick::StillWaiting
        }
        Some(ReloadNotice::Cooldown { .. }) => CooldownTick::Elapsed,
        Some(ReloadNotice::Failed(_)) | None => CooldownTick::NotTicking,
    }
}

/// How big an author avatar renders (#64). One constant because the
/// placeholder has to match the image exactly — a row that reflows when the
/// download lands is worse than no avatar at all.
const AVATAR_SIZE: gpui::Pixels = px(44.0);

/// What stands in for an avatar that hasn't downloaded, failed, or never
/// existed (#64): a filled circle carrying the author's initial.
///
/// An initial rather than a blank disc, since it already distinguishes most
/// consecutive authors in a timeline — which is the whole point of #64 —
/// before any image arrives. An author whose name never expanded gets the
/// bare circle; there is no character to show and inventing one would be
/// worse than the gap.
fn avatar_placeholder(author_name: &str, theme: Theme) -> AnyElement {
    let initial = avatar_initial(author_name);

    div()
        .size(AVATAR_SIZE)
        .rounded_full()
        .bg(rgb(theme.border))
        .flex()
        .items_center()
        .justify_center()
        .text_color(rgb(theme.text_muted))
        .child(initial)
        .into_any_element()
}

/// The character an avatar placeholder shows for `author_name` (#64):
/// its first, uppercased. Empty for an author whose name never expanded —
/// the circle then stands alone rather than showing a made-up initial.
///
/// `char`-wise, not byte-wise, so a name starting with a multi-byte
/// character (which plenty do) is neither split mid-character nor skipped.
/// `to_uppercase` is the Unicode one, which can yield more than one
/// character for some scripts; that is left as-is rather than truncated,
/// since cutting a cased expansion in half produces something wrong rather
/// than something short.
fn avatar_initial(author_name: &str) -> String {
    author_name
        .chars()
        .next()
        .map(|first| first.to_uppercase().to_string())
        .unwrap_or_default()
}

/// x.com's canonical URL for one post (#70), built from what
/// [`TimelineItem`] already carries — no request, no API involvement at
/// all.
///
/// `author_username` is empty for a post whose author never expanded (see
/// `x_api::model::build_item`), and `x.com//status/…` would 404. X's own
/// id-only form, `x.com/i/web/status/:id`, resolves the author server-side,
/// so the link still works rather than being withheld exactly when the app
/// knows least about the post.
fn post_permalink(author_username: &str, post_id: &str) -> String {
    if author_username.is_empty() {
        format!("https://x.com/i/web/status/{post_id}")
    } else {
        format!("https://x.com/{author_username}/status/{post_id}")
    }
}

/// x.com's URL for one account (#70), or `None` when the username never
/// resolved — unlike a post there is no id-only fallback to reach for, so
/// the affordance is withheld instead of pointing somewhere wrong.
fn profile_url(author_username: &str) -> Option<String> {
    (!author_username.is_empty()).then(|| format!("https://x.com/{author_username}"))
}

/// The engagement line shown under a post's body (#67), or `None` when the
/// post has no engagement at all — three zeros on every fresh post would be
/// noise, and #67 asks for counts that stay out of the way.
///
/// Zero counts are dropped individually for the same reason, so a post with
/// only likes reads "3 likes" rather than "0 replies · 0 reposts · 3 likes".
fn metrics_label(metrics: &PostMetrics) -> Option<String> {
    let parts: Vec<String> = [
        (metrics.replies, "reply", "replies"),
        (metrics.reposts, "repost", "reposts"),
        (metrics.likes, "like", "likes"),
    ]
    .into_iter()
    .filter(|(count, _, _)| *count > 0)
    .map(|(count, singular, plural)| {
        let noun = if count == 1 { singular } else { plural };
        format!("{} {noun}", compact_count(count))
    })
    .collect();

    if parts.is_empty() {
        return None;
    }
    Some(parts.join(" · "))
}

/// Abbreviate a count the way X's own UI does — `12345` becomes `12.3K` —
/// so a popular post cannot push the timestamp and byline around by being
/// seven digits wide. A trailing `.0` is dropped (`1000` is `1K`, not
/// `1.0K`); below 1000 the number is shown as-is.
fn compact_count(count: u64) -> String {
    #[expect(
        clippy::cast_precision_loss,
        reason = "an abbreviated count is approximate by construction"
    )]
    fn scaled(count: u64, unit: u64, suffix: char) -> String {
        let value = count as f64 / unit as f64;
        // One decimal, truncated rather than rounded, so the label never
        // claims more engagement than the post actually has.
        let tenths = (value * 10.0).floor() / 10.0;
        if (tenths.fract()).abs() < f64::EPSILON {
            format!("{}{suffix}", tenths.trunc())
        } else {
            format!("{tenths:.1}{suffix}")
        }
    }

    match count {
        0..1_000 => count.to_string(),
        1_000..1_000_000 => scaled(count, 1_000, 'K'),
        _ => scaled(count, 1_000_000, 'M'),
    }
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
        ComposeStatus, Cooldown, CooldownTick, PostLink, PostMetrics, ReloadNotice, ReloadTrigger,
        RepliedTo, Theme, ThreadFetchState, TimelineItem, TimelineSource, TimelineState,
        ToggleState, at_the_post_cap, avatar_initial, byline, compose_error_message,
        cooldown_label, cooldown_tick, format_timestamp, header_title, is_own_post,
        like_action_label, metrics_label, offers_like, offers_load_older, offers_quote,
        offers_reauthorize, offers_repost, offers_sign_in, post_permalink, profile_url, rate_limit,
        reload_cooldown, reload_failure_outcome, reload_gate, reload_start_state,
        reply_banner_label, repost_action_label, repost_banner_label, thread_action_label, usage,
        usage_color, usage_label,
    };

    fn item_with(id: &str, author_username: &str, reposted_by: Option<&str>) -> TimelineItem {
        TimelineItem {
            id: id.to_string(),
            text: String::new(),
            created_at: None,
            author_name: String::new(),
            author_username: author_username.to_string(),
            reposted_by: reposted_by.map(str::to_string),
            quoted: None,
            replied_to: None,
            metrics: None,
            links: Vec::new(),
            author_avatar_url: None,
        }
    }

    #[test]
    fn metrics_label_lists_replies_reposts_and_likes() {
        assert_eq!(
            metrics_label(&PostMetrics {
                replies: 12,
                reposts: 34,
                likes: 56,
            })
            .as_deref(),
            Some("12 replies · 34 reposts · 56 likes")
        );
    }

    #[test]
    fn metrics_label_omits_the_counts_that_are_zero() {
        // #67: a row that only got likes should say so, not carry two zeros
        // along for the ride.
        assert_eq!(
            metrics_label(&PostMetrics {
                replies: 0,
                reposts: 0,
                likes: 3,
            })
            .as_deref(),
            Some("3 likes")
        );
    }

    #[test]
    fn metrics_label_is_singular_for_one() {
        assert_eq!(
            metrics_label(&PostMetrics {
                replies: 1,
                reposts: 1,
                likes: 1,
            })
            .as_deref(),
            Some("1 reply · 1 repost · 1 like")
        );
    }

    #[test]
    fn metrics_label_is_absent_when_nothing_has_happened_yet() {
        // A post with no engagement gets no line at all — three zeros are
        // noise on every fresh post in the timeline.
        assert_eq!(metrics_label(&PostMetrics::default()), None);
    }

    #[test]
    fn metrics_label_abbreviates_large_counts() {
        assert_eq!(
            metrics_label(&PostMetrics {
                replies: 1000,
                reposts: 12_345,
                likes: 2_400_000,
            })
            .as_deref(),
            Some("1K replies · 12.3K reposts · 2.4M likes")
        );
    }

    // --- #64: avatars ---

    #[test]
    fn an_avatar_initial_is_the_uppercased_first_character() {
        assert_eq!(avatar_initial("Developers"), "D");
        assert_eq!(avatar_initial("developers"), "D");
    }

    #[test]
    fn an_avatar_initial_handles_a_multi_byte_first_character() {
        // Byte-slicing here would panic or produce mojibake.
        assert_eq!(avatar_initial("うさだ"), "う");
        assert_eq!(avatar_initial("Émile"), "É");
    }

    #[test]
    fn there_is_no_avatar_initial_without_a_name() {
        // An author whose name never expanded — the circle stands alone
        // rather than showing an invented character.
        assert_eq!(avatar_initial(""), "");
    }

    // --- #70: opening links ---

    #[test]
    fn a_post_permalink_uses_the_authors_handle() {
        assert_eq!(
            post_permalink("XDevelopers", "1700000000000000001"),
            "https://x.com/XDevelopers/status/1700000000000000001"
        );
    }

    #[test]
    fn a_post_permalink_falls_back_to_the_id_only_form() {
        // A post whose author never expanded still has to be reachable —
        // `x.com//status/…` would 404, X's own `/i/web/` form does not.
        assert_eq!(
            post_permalink("", "1700000000000000001"),
            "https://x.com/i/web/status/1700000000000000001"
        );
    }

    #[test]
    fn a_permalink_is_something_the_browser_helper_will_actually_open() {
        // The two halves have to agree: a URL this builds and `browser`
        // then refuses would be a click that does nothing.
        assert!(crate::browser::is_openable(&post_permalink(
            "XDevelopers",
            "1"
        )));
        assert!(crate::browser::is_openable(&post_permalink("", "1")));
        assert!(crate::browser::is_openable(
            &profile_url("XDevelopers").unwrap()
        ));
    }

    #[test]
    fn a_profile_url_uses_the_handle() {
        assert_eq!(
            profile_url("XDevelopers").as_deref(),
            Some("https://x.com/XDevelopers")
        );
    }

    #[test]
    fn there_is_no_profile_url_without_a_handle() {
        // Unlike a post there is no id-only fallback, so the affordance is
        // withheld rather than pointed somewhere wrong.
        assert_eq!(profile_url(""), None);
    }

    // --- #68: like ---

    #[test]
    fn offers_like_on_an_ordinary_post() {
        assert!(offers_like(
            true,
            Some("me-id"),
            &item_with("1", "alice", None)
        ));
    }

    #[test]
    fn offers_like_on_ones_own_post() {
        // #68 is explicit: X rejects reposting your own post but accepts
        // liking it, so `is_own_post` must not be carried over from #15.
        assert!(offers_like(
            true,
            Some("me-id"),
            &item_with("1", "me", None)
        ));
    }

    #[test]
    fn does_not_offer_like_on_a_row_that_is_itself_a_repost() {
        // `item.id` is the retweet activity's id, not the original's — the
        // same gap (#52) that withholds the repost and quote buttons here.
        assert!(!offers_like(
            true,
            Some("me-id"),
            &item_with("1", "alice", Some("bob"))
        ));
    }

    #[test]
    fn does_not_offer_like_before_the_signed_in_id_resolves() {
        assert!(!offers_like(true, None, &item_with("1", "alice", None)));
    }

    #[test]
    fn does_not_offer_like_without_an_oauth_session() {
        assert!(!offers_like(
            false,
            Some("me-id"),
            &item_with("1", "alice", None)
        ));
    }

    #[test]
    fn like_action_label_offers_to_like_when_not_liked() {
        assert_eq!(like_action_label(&ToggleState::new(false)), "Like");
    }

    #[test]
    fn like_action_label_shows_liked_once_it_is() {
        assert_eq!(like_action_label(&ToggleState::new(true)), "Liked");
    }

    #[test]
    fn like_action_label_shows_the_pending_direction() {
        let mut liking = ToggleState::new(false);
        liking.start_toggle();
        assert_eq!(like_action_label(&liking), "Liking…");

        let mut unliking = ToggleState::new(true);
        unliking.start_toggle();
        assert_eq!(like_action_label(&unliking), "Unliking…");
    }

    #[test]
    fn offers_reauthorize_for_a_session_that_predates_the_like_scope() {
        // #68: `like.write` is granted separately, so a session from before
        // it can post and repost but not like — and must be told how to fix
        // that, since `toggle_like`'s refusal points at this very button.
        assert!(offers_reauthorize(
            true,
            Some("tweet.read users.read tweet.write offline.access")
        ));
    }

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

    // --- offers_reauthorize (#14) ---

    #[test]
    fn offers_reauthorize_when_signed_in_with_oauth_but_missing_the_write_scope() {
        // #14: the exact scenario #7's originally-minimal scope request
        // creates — a real, working OAuth session that simply can't post.
        assert!(offers_reauthorize(
            true,
            Some("tweet.read users.read offline.access")
        ));
    }

    #[test]
    fn offers_reauthorize_when_the_scope_was_never_recorded() {
        // A pre-#14 token: "unknown" is treated the same as "insufficient".
        assert!(offers_reauthorize(true, None));
    }

    #[test]
    fn does_not_offer_reauthorize_once_every_write_scope_is_granted() {
        // `like.write` joined the set in #68; a session holding only
        // `tweet.write` is now genuinely under-scoped, which the test above
        // pins down.
        assert!(!offers_reauthorize(
            true,
            Some("tweet.read tweet.write like.write offline.access")
        ));
    }

    #[test]
    fn does_not_offer_reauthorize_without_an_oauth_session() {
        // Not signed in with OAuth at all — `offers_sign_in` is the
        // relevant affordance here, not this one.
        assert!(!offers_reauthorize(false, None));
    }

    // --- compose_error_message (#14) ---

    #[test]
    fn compose_error_message_is_none_while_idle() {
        assert_eq!(compose_error_message(&ComposeStatus::Idle), None);
    }

    #[test]
    fn compose_error_message_is_none_while_submitting() {
        assert_eq!(compose_error_message(&ComposeStatus::Submitting), None);
    }

    #[test]
    fn compose_error_message_surfaces_a_failed_submits_message() {
        let status = ComposeStatus::Failed("network error".to_string());
        assert_eq!(
            compose_error_message(&status).map(|message| message.to_string()),
            Some("network error".to_string())
        );
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
                metrics: None,
                links: Vec::new(),
                author_avatar_url: None,
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

    // --- reload_gate (#57) ---

    #[test]
    fn reload_gate_polling_blocks_within_the_configured_interval() {
        // Same shape `reload_cooldown` itself blocks on — `Polling` must
        // defer to it unchanged.
        assert_eq!(
            reload_gate(ReloadTrigger::Polling, Some(1_000), 60, 1_030),
            Some(1_060)
        );
    }

    #[test]
    fn reload_gate_user_action_bypasses_the_interval_even_when_polling_would_block() {
        // The core fix for #57's primary symptom: a post-submit reload must
        // go through immediately, even though the exact same
        // `last_reload_at`/`now` pair blocks a `Polling` reload above.
        assert_eq!(
            reload_gate(ReloadTrigger::UserAction, Some(1_000), 60, 1_030),
            None
        );
    }

    // --- cooldown_tick (#57 item 3) ---

    #[test]
    fn cooldown_tick_keeps_waiting_before_reset_at() {
        let notice = ReloadNotice::Cooldown {
            reset_at: 1_060,
            cooldown: Cooldown::LocalInterval,
        };
        assert_eq!(
            cooldown_tick(Some(&notice), 1_030),
            CooldownTick::StillWaiting
        );
    }

    #[test]
    fn cooldown_tick_has_elapsed_once_reset_at_has_passed() {
        let notice = ReloadNotice::Cooldown {
            reset_at: 1_060,
            cooldown: Cooldown::ApiRateLimit,
        };
        assert_eq!(cooldown_tick(Some(&notice), 1_061), CooldownTick::Elapsed);
    }

    #[test]
    fn cooldown_tick_has_elapsed_exactly_at_reset_at() {
        // Mirrors `reload_cooldown`'s own `>` boundary: blocked strictly
        // before `reset_at`, allowed (here: elapsed) from `reset_at` on.
        let notice = ReloadNotice::Cooldown {
            reset_at: 1_060,
            cooldown: Cooldown::LocalInterval,
        };
        assert_eq!(cooldown_tick(Some(&notice), 1_060), CooldownTick::Elapsed);
    }

    #[test]
    fn cooldown_tick_is_not_ticking_without_a_notice() {
        assert_eq!(cooldown_tick(None, 1_000), CooldownTick::NotTicking);
    }

    #[test]
    fn cooldown_tick_is_not_ticking_for_a_failed_notice() {
        // A `Failed` notice carries no countdown to advance — the ticker
        // must stop rather than poll it forever.
        let notice = ReloadNotice::Failed("boom".into());
        assert_eq!(
            cooldown_tick(Some(&notice), 1_000),
            CooldownTick::NotTicking
        );
    }

    // --- reload_start_state (#57) ---

    #[test]
    fn reload_start_state_keeps_existing_posts_in_place() {
        let items = vec![item_with("1", "alice", None)];
        match reload_start_state(TimelineState::Loaded(items.clone())) {
            TimelineState::Loaded(got) => assert_eq!(got, items),
            other => panic!("expected existing posts to survive, got {other:?}"),
        }
    }

    #[test]
    fn reload_start_state_falls_back_to_loading_when_nothing_was_shown() {
        assert!(matches!(
            reload_start_state(TimelineState::NotAuthenticated),
            TimelineState::Loading
        ));
    }

    // --- reload_failure_outcome (#57) ---

    #[test]
    fn reload_failure_outcome_keeps_existing_posts_on_a_plain_failure() {
        let items = vec![item_with("1", "alice", None)];
        let error = anyhow::anyhow!("network exploded");
        let (state, notice) = reload_failure_outcome(TimelineState::Loaded(items.clone()), &error);
        match state {
            TimelineState::Loaded(got) => assert_eq!(got, items),
            other => panic!("existing posts must survive a failed reload, got {other:?}"),
        }
        assert_eq!(
            notice,
            Some(ReloadNotice::Failed("network exploded".to_string().into()))
        );
    }

    #[test]
    fn reload_failure_outcome_keeps_existing_posts_on_a_rate_limited_failure() {
        let items = vec![item_with("1", "alice", None)];
        let error: anyhow::Error = rate_limit::RateLimited {
            reset_at: Some(1_500),
        }
        .into();
        let (state, notice) = reload_failure_outcome(TimelineState::Loaded(items.clone()), &error);
        match state {
            TimelineState::Loaded(got) => assert_eq!(got, items),
            other => panic!("existing posts must survive a rate-limited reload, got {other:?}"),
        }
        assert_eq!(
            notice,
            Some(ReloadNotice::Cooldown {
                reset_at: 1_500,
                cooldown: Cooldown::ApiRateLimit,
            })
        );
    }

    #[test]
    fn reload_failure_outcome_falls_back_to_failed_state_when_nothing_was_shown() {
        let error = anyhow::anyhow!("network exploded");
        let (state, notice) = reload_failure_outcome(TimelineState::Loading, &error);
        assert!(matches!(state, TimelineState::Failed(_)));
        // #57: the state itself already says what went wrong — a banner
        // saying the exact same thing would be a duplicated failure.
        assert_eq!(notice, None);
    }

    #[test]
    fn reload_failure_outcome_falls_back_to_rate_limited_state_when_nothing_was_shown() {
        let error: anyhow::Error = rate_limit::RateLimited {
            reset_at: Some(1_500),
        }
        .into();
        let (state, notice) = reload_failure_outcome(TimelineState::NotAuthenticated, &error);
        assert!(matches!(
            state,
            TimelineState::RateLimited {
                reset_at: 1_500,
                cooldown: Cooldown::ApiRateLimit,
            }
        ));
        // Same reasoning as the plain-failure case above: `RateLimited`
        // already carries the countdown, so no separate notice is needed.
        assert_eq!(notice, None);
    }

    // --- `TimelineView::load_older` reuses the same pure functions (#57) ---
    //
    // `load_older` only ever runs once `state` is already `Loaded` (see
    // `offers_load_older`'s gate on the "Load older" row), so these pin the
    // specific two-item, paging-backwards shape that call site actually
    // hits, rather than re-asserting the single-item cases above.

    #[test]
    fn load_older_keeps_the_current_page_visible_while_its_fetch_is_in_flight() {
        // Before #57, `load_older` set `state = TimelineState::Loading`
        // unconditionally, which — via `TimelineView::body`'s match — wiped
        // the page the user was paging through, and the "Load older" row
        // along with it, for the whole request.
        let items = vec![item_with("1", "alice", None), item_with("2", "bob", None)];
        match reload_start_state(TimelineState::Loaded(items.clone())) {
            TimelineState::Loaded(got) => assert_eq!(got, items),
            other => panic!("load_older must keep the current page visible, got {other:?}"),
        }
    }

    #[test]
    fn load_older_keeps_the_current_page_when_paging_backwards_fails() {
        // Before #57, a failed "Load older" request replaced `state` via
        // `map_reload_error`, discarding everything the user had already
        // paged through — worse than a plain reload failure, since nothing
        // about the posts already shown was actually wrong.
        let items = vec![item_with("1", "alice", None), item_with("2", "bob", None)];
        let error = anyhow::anyhow!("network exploded");
        let (state, notice) = reload_failure_outcome(TimelineState::Loaded(items.clone()), &error);
        match state {
            TimelineState::Loaded(got) => assert_eq!(got, items),
            other => panic!("load_older must keep the current page, got {other:?}"),
        }
        assert_eq!(
            notice,
            Some(ReloadNotice::Failed("network exploded".to_string().into()))
        );
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

    // --- offers_repost / is_own_post (#15) ---

    #[test]
    fn offers_repost_once_signed_in_with_a_resolved_home_id_on_someone_elses_post() {
        let item = item_with("1", "alice", None);
        assert!(offers_repost(true, Some("2244994945"), Some("bob"), &item));
    }

    #[test]
    fn does_not_offer_repost_without_oauth() {
        let item = item_with("1", "alice", None);
        assert!(!offers_repost(
            false,
            Some("2244994945"),
            Some("bob"),
            &item
        ));
    }

    #[test]
    fn does_not_offer_repost_before_home_user_id_resolves() {
        // #11: the repost endpoints act as *this* account, whose id only
        // `/me` resolves — nothing to call yet without it.
        let item = item_with("1", "alice", None);
        assert!(!offers_repost(true, None, Some("bob"), &item));
    }

    #[test]
    fn does_not_offer_repost_on_a_row_that_is_itself_already_a_repost() {
        // #15: the row's `item.id` is the retweet activity's own id, not the
        // original content's — there's no id to call the endpoint with
        // safely (see `offers_repost`'s doc).
        let item = item_with("1", "alice", Some("bob"));
        assert!(!offers_repost(true, Some("2244994945"), Some("bob"), &item));
    }

    #[test]
    fn does_not_offer_repost_on_ones_own_post() {
        let item = item_with("1", "bob", None);
        assert!(!offers_repost(true, Some("2244994945"), Some("bob"), &item));
    }

    // --- offers_quote (#16) ---

    #[test]
    fn offers_quote_once_signed_in_on_an_ordinary_post() {
        let item = item_with("1", "alice", None);
        assert!(offers_quote(true, &item));
    }

    #[test]
    fn does_not_offer_quote_without_oauth() {
        // The composer itself isn't reachable without OAuth (see
        // `Render::render`'s gate) — nowhere for a quote to go.
        let item = item_with("1", "alice", None);
        assert!(!offers_quote(false, &item));
    }

    #[test]
    fn does_not_offer_quote_on_a_row_that_is_itself_already_a_repost() {
        // #16: same reason `offers_repost` withholds its own button —
        // `item.id` is the retweet activity's own id, not the original
        // content's `quote_tweet_id` would need.
        let item = item_with("1", "alice", Some("bob"));
        assert!(!offers_quote(true, &item));
    }

    #[test]
    fn offers_quote_on_ones_own_post() {
        // Unlike `offers_repost`, quoting your own post is allowed — the
        // API doesn't reject it, per #16's design decision, so this must
        // stay `true` even though the equivalent repost case is `false`.
        let item = item_with("1", "bob", None);
        assert!(offers_quote(true, &item));
    }

    #[test]
    fn is_own_post_matches_case_insensitively() {
        assert!(is_own_post(Some("Bob"), "bob"));
        assert!(is_own_post(Some("bob"), "BOB"));
    }

    #[test]
    fn is_own_post_is_false_when_home_username_is_unknown() {
        assert!(!is_own_post(None, "bob"));
    }

    #[test]
    fn is_own_post_is_false_for_a_different_author() {
        assert!(!is_own_post(Some("bob"), "alice"));
    }

    // --- repost_action_label (#15) ---

    #[test]
    fn repost_action_label_offers_to_repost_when_not_reposted() {
        assert_eq!(repost_action_label(&ToggleState::new(false)), "Repost");
    }

    #[test]
    fn repost_action_label_shows_reposted_once_it_is() {
        assert_eq!(repost_action_label(&ToggleState::new(true)), "Reposted");
    }

    #[test]
    fn repost_action_label_shows_the_pending_direction() {
        let mut creating = ToggleState::new(false);
        creating.start_toggle();
        assert_eq!(repost_action_label(&creating), "Reposting…");

        let mut deleting = ToggleState::new(true);
        deleting.start_toggle();
        assert_eq!(repost_action_label(&deleting), "Removing repost…");
    }

    /// The startup path #55 fell straight through (#59).
    ///
    /// Everything below `cargo run` built and unit-tested clean, but nobody
    /// had opened the window, so a panic that only fires once something
    /// renders reached `main` untouched. gpui's test platform draws to
    /// nothing (`TestWindow::draw` is a no-op), so this needs neither a GPU
    /// nor the window server -- yet it still walks the same element tree the
    /// real window would, which is exactly where `gpui_component`'s widgets
    /// reach back up for the window root.
    #[gpui::test]
    fn the_window_root_renders_without_panicking(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;

        let home = std::env::temp_dir().join("twigpui-smoke");
        let home = home.display().to_string();
        let paths =
            crate::paths::Paths::from_vars(move |key| (key == "HOME").then(|| home.clone()))
                .unwrap();
        let config = crate::config::Config {
            bearer_token: None,
            oauth_client_id: None,
            target_username: "XDevelopers".to_string(),
            max_results: 20,
            min_fetch_interval_seconds: 60,
            theme: crate::theme::ThemeMode::Light,
            request_price: None,
            daily_request_budget: None,
        };

        cx.update(gpui_component::init);

        // Held so the composer's input can be focused below: `add_window`
        // hands back a handle to the *root* view, which is deliberately the
        // `Root` wrapper here, not the timeline inside it.
        let timeline_slot = std::rc::Rc::new(std::cell::RefCell::new(None));
        let window = {
            let slot = timeline_slot.clone();
            cx.add_window(move |window, cx| {
                let timeline = cx.new(|cx| {
                    let mut view = super::TimelineView::new(config, paths, window, cx);
                    // #55 hid behind this flag: the composer -- the one widget
                    // that reaches back up for the window root -- is only
                    // rendered for an OAuth session, so every run on a bearer
                    // token missed it.
                    view.signed_in_with_oauth = true;
                    // Resolving the signed-in id is what unlocks the
                    // per-row action buttons (`offers_repost`,
                    // `offers_like`), so without it the walk below skips
                    // them entirely.
                    view.home_user_id = Some("2244994945".to_string());
                    view
                });
                *slot.borrow_mut() = Some(timeline.clone());
                // The line whose absence aborted the app at startup (#55).
                gpui_component::Root::new(timeline, window, cx)
            })
        };
        let timeline = timeline_slot.borrow().clone().unwrap();

        // The composer only reaches for the window root once its input is
        // focused, which is what the app does as soon as the user clicks it.
        cx.update_window(window.into(), |_, window, cx| {
            timeline.update(cx, |view, cx| {
                view.compose_input
                    .update(cx, |input, cx| input.focus(window, cx));
            });
        })
        .unwrap();

        cx.run_until_parked();

        // Give the body a post to draw. An empty timeline renders none of
        // `post_row`, so without this the walk below never reaches the
        // banners, the quote card, the action buttons or #67's metrics line
        // -- exactly the kind of blind spot #59 was written to close. It has
        // to happen *after* the startup task settles: that task ends by
        // assigning `state` itself (`NotAuthenticated`, with no credential
        // configured here) and would otherwise wipe this.
        cx.update(|cx| {
            timeline.update(cx, |view, cx| {
                view.state = TimelineState::Loaded(vec![TimelineItem {
                    id: "1700000000000000001".to_string(),
                    text: "a rendered post".to_string(),
                    created_at: Some("2026-08-16T09:00:00.000Z".to_string()),
                    author_name: "Developers".to_string(),
                    author_username: "XDevelopers".to_string(),
                    reposted_by: None,
                    quoted: None,
                    replied_to: None,
                    metrics: Some(PostMetrics {
                        replies: 12,
                        reposts: 34,
                        likes: 5600,
                    }),
                    links: vec![PostLink {
                        url: "https://example.com/an-article".to_string(),
                        label: "example.com/an-article".to_string(),
                    }],
                    author_avatar_url: Some(
                        "https://pbs.twimg.com/profile_images/1/a_normal.jpg".to_string(),
                    ),
                }]);
                cx.notify();
            });
        });

        // Opening a window is not enough: nothing has rendered yet, and the
        // panic #55 is about only fires once the element tree is walked.
        for _ in 0..2 {
            cx.update_window(window.into(), |_, window, cx| {
                let _ = window.draw(cx);
            })
            .unwrap();
        }
    }
}
