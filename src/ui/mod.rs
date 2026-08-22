use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use gpui::{
    AnyElement, Context, Entity, FocusHandle, FontWeight, ScrollHandle, SharedString, Subscription,
    Task, Window, div, img, prelude::*, px, rgb,
};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::avatar;
use crate::browser;
use crate::cache;
use crate::compose::{self, ComposeState, ComposeStatus};
use crate::config::Config;
use crate::image_cache;
use crate::like;
use crate::log;
mod reload_policy;
mod render;

// Children rather than siblings of `ui` (#126): a child module can see its
// parent's private items, so `TimelineState`, `ReloadNotice` and
// `TimelineView` itself stay private to `ui` instead of being widened to
// `pub(crate)` merely to be reachable from the file next door. Widening
// them would mean "anything in the crate may touch this", which is the
// opposite of what splitting the file was for.
use reload_policy::{
    CooldownTick, at_the_post_cap, cooldown_label, cooldown_tick, offers_load_older,
    preserved_scroll_target, reload_failure_outcome, reload_gate, reload_start_state,
};
use render::{
    AVATAR_SIZE, MAX_RENDERED_MEDIA, MEDIA_CELL_HEIGHT, author_link, avatar_placeholder, byline,
    compose_error_message, format_timestamp, header_title, like_row, link_row, media_badge,
    media_columns, metrics_label, notice, offers_delete, offers_like, offers_quote,
    offers_reauthorize, offers_reply, offers_repost, open_post_link, quote_card, quote_row,
    reload_notice_banner, render_thread_chain, reply_banner_label, reply_row, reply_target_label,
    repost_banner_label, repost_row, session_notice_banner, sign_in_pill, thread_action_label,
    thread_toggle_row, usage_color, usage_label,
};

use crate::menu::{
    BlurComposer, CloseWindow, FocusComposer, KEY_CONTEXT, Minimize, Reload, ScrollToTop,
    ShowAbout, SubmitPost, shortcuts,
};
use crate::oauth;
use crate::paths::Paths;
use crate::rate_limit;
use crate::repost;
use crate::theme::{self, Theme};
use crate::thread::{self, ThreadChain};
use crate::toggle::{ToggleState, ToggleStatus};
use crate::usage;
use crate::x_api::{
    Draft, PostLink, PostMedia, PostMetrics, QuotedPost, RepliedTo, TimelineItem, XClient,
    action_post_id,
};

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
///
/// A local enum rather than a tuple because `Home` carries the resolved
/// [`cache::MeEntry`] alongside the posts, so the header and `home_user_id`
/// are populated even on a pure cache hit without a second round trip
/// through `/me`. It had a third variant until #33 — `SingleUser`, the
/// shape an app-only bearer token resolved to.
enum StartOutcome {
    NotAuthenticated {
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
    /// The signed-in user's own id, resolved via `GET /2/users/me`. Needed
    /// to call the home-timeline endpoint and to page further back with
    /// [`Self::load_older`]. `None` until `/me` has resolved once.
    home_user_id: Option<String>,
    /// The signed-in user's own screen name (also from `/me`), shown in the
    /// header — see [`header_title`].
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
    /// Downloaded post media (#65), keyed by the media URL — the same
    /// shape as `avatar_paths`, kept as its own map because the two live
    /// in different cache directories and come from different fields.
    media_paths: HashMap<String, PathBuf>,
    /// Holds the in-flight avatar downloads alive (#64). One task walks the
    /// whole visible timeline rather than one per row; reassigning it (a
    /// reload) cancels whatever was still downloading, which the next call
    /// re-collects from the new timeline anyway.
    avatar_fetch: Option<Task<()>>,
    /// Holds the in-flight media downloads alive (#65) — see
    /// `avatar_fetch`, which this mirrors.
    media_fetch: Option<Task<()>>,
    /// The post whose delete confirmation is currently showing (#72), if
    /// any. One at a time: a second "Delete" click elsewhere moves the
    /// prompt rather than opening two. `None` means no row is asking.
    ///
    /// A two-step click rather than a modal because deleting is
    /// irreversible and this app has no dialog machinery — what matters is
    /// that no single click can destroy a post, which this guarantees.
    pending_delete: Option<String>,
    /// Holds an in-flight delete alive (#72).
    delete_task: Option<Task<()>>,
    /// Why the last delete failed (#72), shown on the row that asked.
    /// Keyed by post id so a failure stays attached to its own row.
    delete_failures: HashMap<String, String>,
    open_task: Option<Task<()>>,
    /// Why the last open attempt failed (#70), shown in the header until
    /// the next attempt clears it. `None` is the ordinary case — a
    /// successful open leaves the app with nothing to say.
    open_failure: Option<String>,
    /// Scroll position of the timeline list (#22).
    ///
    /// Read before a reload replaces the list and used afterwards to put
    /// the reader back on the row they were on: prepending posts to a
    /// scrolled list otherwise slides everything down under them.
    list_scroll: ScrollHandle,
    /// Focus for the timeline's own root element (#118).
    ///
    /// gpui resolves an action against the focused element's ancestry, so
    /// without this nothing here is reachable until the composer is
    /// clicked: `cmd-r` matched nothing and the menu bar's Reload / New
    /// Post / Submit Post greyed out or dispatched into nowhere. `Quit`
    /// escaped only by living on the `App`. Focused at startup and
    /// returned to when the composer is left.
    focus_handle: FocusHandle,
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
            media_paths: HashMap::new(),
            avatar_fetch: None,
            media_fetch: None,
            pending_delete: None,
            delete_task: None,
            delete_failures: HashMap::new(),
            open_task: None,
            open_failure: None,
            list_scroll: ScrollHandle::new(),
            focus_handle: cx.focus_handle(),
        };
        // #118: before anything else, so the very first frame has the
        // timeline on the focus path rather than an empty one.
        window.focus(&this.focus_handle);
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
                    let session_notice = resolution.demotion.as_ref().map(oauth::describe_demotion);
                    let Some(credential) = resolution.credential else {
                        return anyhow::Ok(StartOutcome::NotAuthenticated { session_notice });
                    };
                    // #33: the window always shows the home timeline now.
                    // Which timeline to show used to follow from the kind of
                    // credential — the app-only bearer token got a 401 from
                    // the home endpoint — and with that credential gone
                    // there is nothing left to branch on. `SingleUser`
                    // survives for `--fetch-only` and #24's panels.
                    let cached = cache::startup_home(&paths, oauth::unix_now())?;
                    anyhow::Ok(StartOutcome::Home {
                        credential,
                        cached,
                        session_notice,
                    })
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                this.refresh_usage(cx);
                this.refresh_reposted_ids(cx);
                this.refresh_liked_ids(cx);
                match result {
                    Ok(StartOutcome::NotAuthenticated { session_notice }) => {
                        this.session_notice = session_notice.map(SharedString::from);
                        this.state = TimelineState::NotAuthenticated;
                        cx.notify();
                    }
                    Ok(StartOutcome::Home {
                        credential,
                        cached,
                        session_notice,
                    }) => {
                        this.session_notice = session_notice.map(SharedString::from);
                        this.signed_in_with_oauth = true;
                        this.oauth_scope.clone_from(&credential.scope);
                        this.client = Some(XClient::new(credential.token));
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
                // #120: after the match, never before it. `refresh_images`
                // reads `self.state` to decide which avatars and media are
                // missing, so calling it first hands it the *previous*
                // state — `Loading` on startup, which fetches nothing at
                // all, and the outgoing item list on a reload, which
                // fetches the images the last batch wanted. That is why
                // avatars only appeared one reload late. Its siblings above
                // read from disk rather than `state`, so their position
                // does not matter; this one's does.
                this.refresh_images(cx);
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

        // #33: the window always shows the home timeline — see
        // `TimelineSource`'s removal. The single-user endpoint and its cache
        // stay for `--fetch-only`.
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
                this.reloading = false;
                match result {
                    Ok(reloaded) => {
                        this.home_user_id = Some(reloaded.me.id);
                        this.home_username = Some(reloaded.me.username);
                        this.next_page_token = reloaded.next_token;
                        this.keep_the_reader_in_place(&reloaded.items);
                        this.state = TimelineState::Loaded(reloaded.items);
                        this.reload_notice = None;
                        // Same reasoning as the single-user branch above.
                        this.cooldown_ticker = None;
                    }
                    Err(error) => this.apply_reload_failure(&error, cx),
                }
                // After the match, for the reason spelled out in `start`
                // (#120): before it, this fetched the images the *outgoing*
                // item list wanted, leaving every newly arrived row on its
                // placeholder until the reload after this one.
                this.refresh_images(cx);
                cx.notify();
            });
        }));

        cx.notify();
    }

    /// Undo the shove a reload gives a scrolled reader (#22).
    ///
    /// Called with the incoming list *before* `state` is replaced, since
    /// working out how many posts arrived needs both lists. Delegates the
    /// decision to [`preserved_scroll_target`] and does nothing when it
    /// declines — the reader is at the top, where new posts arriving above
    /// nothing is the behaviour they want.
    fn keep_the_reader_in_place(&self, incoming: &[TimelineItem]) {
        let TimelineState::Loaded(previous) = &self.state else {
            return;
        };
        let previous_ids: Vec<&str> = previous.iter().map(|item| item.id.as_str()).collect();
        let new_ids: Vec<&str> = incoming.iter().map(|item| item.id.as_str()).collect();
        if let Some(target) =
            preserved_scroll_target(&previous_ids, &new_ids, self.list_scroll.top_item())
        {
            self.list_scroll.scroll_to_top_of_item(target);
        }
    }

    /// Shared `Err` handling for both of [`Self::reload`]'s fetch branches
    /// and [`Self::load_older`] (#57): existing posts survive a failed
    /// fetch via [`reload_failure_outcome`] — pulled into its own method
    /// partly to keep `reload` itself under clippy's line-count lint, partly
    /// so all three call sites apply the exact same `Option<ReloadNotice>`
    /// (and, since #57's item 3, ticker) handling below rather than three
    /// copies that could drift.
    fn apply_reload_failure(&mut self, error: &anyhow::Error, cx: &mut Context<'_, Self>) {
        // #49: a `.app` launched from Finder has no stderr, so without this
        // a failed reload leaves nothing behind but a banner the user
        // dismissed. `log::redact` runs on the way out — an API error can
        // quote the request that produced it.
        log::error(&format!("reload failed: {error:#}"));
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
                // After the match (#120), same as `start` and `reload`: the
                // page just appended is the one whose images are missing.
                this.refresh_images(cx);
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

                match result {
                    Ok(path) => {
                        let _ = this.update(cx, |this, cx| {
                            this.avatar_paths.insert(url.clone(), path);
                            cx.notify();
                        });
                    }
                    // #49: the row keeps its placeholder either way, but a
                    // silently missing avatar is exactly the kind of thing
                    // that is impossible to investigate afterwards without
                    // a line in the log.
                    Err(error) => log::warn(&format!("avatar fetch failed: {error:#}")),
                }
            }
        }));
    }

    /// Fetch whatever images the visible timeline is missing (#64, #65) —
    /// author avatars and attached media both.
    ///
    /// One entry point rather than two calls at every site that changes the
    /// timeline: the two are wanted at exactly the same moments, and a
    /// caller that remembered one but not the other would leave half the
    /// row waiting for the next reload.
    ///
    /// **Call this after `self.state` has been updated, never before**
    /// (#120). Both halves read `state` to work out what is missing and do
    /// nothing at all unless it is `Loaded`, so calling it first asks the
    /// outgoing item list what it needs: nothing on startup, where `state`
    /// is still `Loading`, and the previous batch's URLs on a reload. The
    /// symptom was avatars that only appeared one reload after the rows
    /// they belonged to. Sibling refreshers at those same call sites
    /// (`refresh_usage`, `refresh_reposted_ids`, `refresh_liked_ids`) read
    /// from disk instead and are order-independent, which is what made this
    /// easy to miss.
    fn refresh_images(&mut self, cx: &mut Context<'_, Self>) {
        self.refresh_avatars(cx);
        self.refresh_media(cx);
    }

    /// Download whatever attached images the visible timeline needs and
    /// doesn't have yet (#65) — [`Self::refresh_avatars`]'s twin, with the
    /// same contract: one task for the whole timeline, one URL at a time on
    /// the background executor, each thumbnail appearing as it lands, and a
    /// failure left absent so the frame stays and the next reload retries.
    ///
    /// Attached media is larger than an avatar but arrives the same way
    /// (`pbs.twimg.com`, no API quota, no credits) and is bounded by the
    /// shared image cache's own size limit.
    fn refresh_media(&mut self, cx: &mut Context<'_, Self>) {
        let TimelineState::Loaded(items) = &self.state else {
            return;
        };
        let mut wanted: Vec<String> = Vec::new();
        for url in items
            .iter()
            // #123: a quoted post's images download by the same path as
            // the row's own. Without this the card renders empty frames
            // that never fill, which is worse than the text-only card it
            // replaced.
            .flat_map(|item| {
                item.media
                    .iter()
                    .chain(item.quoted.iter().flat_map(|quoted| quoted.media.iter()))
            })
            .map(|media| media.url.as_str())
        {
            if !self.media_paths.contains_key(url) && !wanted.iter().any(|seen| seen == url) {
                wanted.push(url.to_string());
            }
        }
        if wanted.is_empty() {
            return;
        }

        let dir = self.paths.media_dir();
        self.media_fetch = Some(cx.spawn(async move |this, cx| {
            for url in wanted {
                let dir = dir.clone();
                let fetch_url = url.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move { image_cache::ensure_cached(&dir, &fetch_url) })
                    .await;

                match result {
                    Ok(path) => {
                        let _ = this.update(cx, |this, cx| {
                            this.media_paths.insert(url.clone(), path);
                            cx.notify();
                        });
                    }
                    Err(error) => log::warn(&format!("media fetch failed: {error:#}")),
                }
            }
        }));
    }

    /// The attached-media grid under one post's body (#65).
    ///
    /// At most [`MAX_RENDERED_MEDIA`] thumbnails, in [`media_columns`]
    /// columns. Each cell is a fixed height so a row's height cannot depend
    /// on which images have finished downloading — a timeline that reflows
    /// under the reader as images land is worse than one showing frames
    /// waiting to be filled.
    fn media_grid(&self, media: &[PostMedia], cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = self.theme;
        let shown: Vec<&PostMedia> = media.iter().take(MAX_RENDERED_MEDIA).collect();
        let columns = media_columns(shown.len());

        let mut grid = div().flex().flex_col().gap_1();
        for chunk in shown.chunks(columns) {
            let mut row = div().flex().gap_1();
            for media in chunk {
                row = row.child(self.media_cell(media, theme, cx));
            }
            grid = grid.child(row);
        }
        grid.into_any_element()
    }

    /// [`Self::media_grid`], or nothing when there is no media to draw
    /// (#123) — what a quote card needs, since most quotes have none and
    /// an empty grid would still add its gap to the card.
    fn media_grid_for(
        &self,
        media: &[PostMedia],
        cx: &mut Context<'_, Self>,
    ) -> Option<AnyElement> {
        (!media.is_empty()).then(|| self.media_grid(media, cx))
    }

    /// One thumbnail: the downloaded image once it has arrived, else a
    /// frame of the same size (#65). Clicking it opens the full image in
    /// the browser (#70) — this app has no lightbox, and a thumbnail with
    /// no way to see it properly is half a feature. A video or animated GIF
    /// shows its still with a badge saying which it is; neither plays here.
    fn media_cell(
        &self,
        media: &PostMedia,
        theme: Theme,
        cx: &mut Context<'_, TimelineView>,
    ) -> AnyElement {
        let url = media.url.clone();

        let inner = match self.media_paths.get(&media.url) {
            Some(path) => img(path.clone())
                .h(MEDIA_CELL_HEIGHT)
                .rounded_md()
                .into_any_element(),
            None => div()
                .h(MEDIA_CELL_HEIGHT)
                .w(MEDIA_CELL_HEIGHT)
                .rounded_md()
                .bg(rgb(theme.border))
                .into_any_element(),
        };

        let mut cell = div()
            .id(SharedString::from(format!("media-{}", media.url)))
            .flex()
            .flex_col()
            .gap_1()
            .child(inner)
            .on_click(cx.listener(move |this, _event, _window, cx| {
                this.open_in_browser(url.clone(), cx);
            }));

        if let Some(badge) = media_badge(media.kind.as_deref()) {
            cell = cell.child(div().text_color(rgb(theme.text_muted)).child(badge));
        }
        if let Some(alt) = media.alt_text.as_ref() {
            // Shown rather than hidden behind a hover: this app has no
            // screen-reader path of its own, and alt text a sighted reader
            // can see is more use than alt text nobody ever reaches.
            cell = cell.child(
                div()
                    .text_color(rgb(theme.text_muted))
                    .child(format!("Alt: {alt}")),
            );
        }
        cell.into_any_element()
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
                .flex_shrink_0()
                .rounded(theme::AVATAR_RADIUS)
                .into_any_element(),
            None => avatar_placeholder(&item.author_name, theme),
        }
    }

    /// Ask for confirmation before deleting `post_id` (#72) — the first
    /// click of the two-step. Replaces any other row's pending prompt, so
    /// only one post is ever a click away from being deleted.
    fn ask_to_delete(&mut self, post_id: String, cx: &mut Context<'_, Self>) {
        self.delete_failures.remove(&post_id);
        self.pending_delete = Some(post_id);
        cx.notify();
    }

    /// Dismiss the delete prompt without deleting anything (#72).
    fn cancel_delete(&mut self, cx: &mut Context<'_, Self>) {
        self.pending_delete = None;
        cx.notify();
    }

    /// Delete `post_id` for real (#72) — the second click.
    ///
    /// On success the post is dropped from the rendered timeline *and* from
    /// the cache file, and the cache is read back to confirm: a row that
    /// vanishes now and returns on the next start is exactly the
    /// looks-like-it-worked failure #54 was about, and the issue calls it
    /// out by name.
    ///
    /// A failed delete leaves the row in place with the API's own message
    /// attached, which is the honest outcome — the post still exists.
    fn confirm_delete(&mut self, post_id: String, cx: &mut Context<'_, Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(user_id) = self.home_user_id.clone() else {
            return;
        };
        // #33: the window only ever shows the home timeline, so that is
        // the cache file a delete has to be removed from.
        let home = true;

        self.pending_delete = None;
        cx.notify();

        let paths = self.paths.clone();
        let request_id = post_id.clone();

        self.delete_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    client.delete_post(&paths, &request_id, oauth::unix_now())?;
                    // Only once X has confirmed the deletion: forgetting it
                    // locally first would hide a post that still exists.
                    cache::forget_post(&paths, home, &user_id, &request_id, oauth::unix_now())
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                this.refresh_usage(cx);
                match result {
                    Ok(remaining) => {
                        this.delete_failures.remove(&post_id);
                        this.state = TimelineState::Loaded(remaining);
                    }
                    Err(error) => {
                        this.delete_failures
                            .insert(post_id.clone(), format!("{error:#}"));
                    }
                }
                cx.notify();
            });
        }));
    }

    /// The delete affordance for one post (#72): "Delete", or the
    /// confirmation pair once it has been clicked, plus whatever the last
    /// attempt failed with.
    fn delete_row(&self, item: &TimelineItem, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = self.theme;
        let asking = self.pending_delete.as_deref() == Some(item.id.as_str());

        let controls = if asking {
            let confirm_id = item.id.clone();
            div()
                .flex()
                .gap_3()
                .child(
                    div()
                        .id(SharedString::from(format!("delete-confirm-{}", item.id)))
                        .text_color(rgb(theme.danger))
                        .child("Delete permanently")
                        .on_click(cx.listener(move |this, _event, _window, cx| {
                            this.confirm_delete(confirm_id.clone(), cx);
                        })),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("delete-cancel-{}", item.id)))
                        .text_color(rgb(theme.text_muted))
                        .child("Cancel")
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.cancel_delete(cx);
                        })),
                )
        } else {
            let ask_id = item.id.clone();
            div().child(
                div()
                    .id(SharedString::from(format!("delete-{}", item.id)))
                    .text_color(rgb(theme.text_muted))
                    .child("Delete")
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.ask_to_delete(ask_id.clone(), cx);
                    })),
            )
        };

        match self.delete_failures.get(&item.id) {
            Some(message) => div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_color(rgb(theme.danger)).child(message.clone()))
                .child(controls)
                .into_any_element(),
            None => controls.into_any_element(),
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
        // #52: the row is keyed by its own id (unique per row, so two
        // reposts of one original don't collide as elements), but the
        // request acts on the original.
        let target = action_post_id(item);
        let state = self.like_state_for(target);
        like_row(&item.id, target, &state, self.theme, cx)
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
        // #52: element id from the row, request target from the original.
        let target = action_post_id(item);
        let state = self.repost_state_for(target);
        repost_row(&item.id, target, &state, self.theme, cx)
    }

    /// Run the interactive PKCE sign-in flow: open the browser, wait for the
    /// loopback callback, exchange the code, persist the tokens, then fall
    /// straight into [`Self::reload`].
    fn sign_in(&mut self, cx: &mut Context<'_, Self>) {
        // #33: `Config::resolve` refuses to start without one, so there is
        // nothing to check here any more.
        let client_id = self.config.oauth_client_id.clone();

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
                    log::info("signed in with OAuth");
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
                    this.client = Some(XClient::new(tokens.access_token));
                    // #57: confirms what the user just did — must not wait
                    // out #10's interval, which exists to suppress polling,
                    // not to gate a direct response to a user action.
                    this.reload(ReloadTrigger::UserAction, cx);
                }
                Err(error) => {
                    log::error(&format!("sign-in failed: {error:#}"));
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
            .child(quote_card(&target.quoted, theme, None))
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

    /// The reply target shown inside the composer, if `compose.reply()` has
    /// one (#71).
    ///
    /// Uses the same [`quote_card`] rendering as a quote target, with an
    /// explicit "Replying to" heading above it — the card alone cannot say
    /// which of the two a draft is, and the difference is not visible after
    /// the fact: a reply lands under a conversation, a quote does not.
    fn composer_reply_card(
        &self,
        target: &compose::ReplyTarget,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let theme = self.theme;
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_color(rgb(theme.text_muted))
                    .child(reply_target_label(&target.replying_to.author_username)),
            )
            .child(quote_card(&target.replying_to, theme, None))
            .child(
                div()
                    .id("compose-remove-reply")
                    .text_color(rgb(theme.accent))
                    .child("Remove reply")
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.compose.clear_reply();
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
            // #71: the reply target, when "Reply" set one. Never both — see
            // `ComposeState::set_reply`.
            .when_some(self.compose.reply(), |column, target| {
                column.child(self.composer_reply_card(target, cx))
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
        // #71: the post this reply answers, if "Reply" set one. Mutually
        // exclusive with the quote above — see `ComposeState::set_reply`.
        let reply_to_post_id = self.compose.reply().map(|target| target.post_id.clone());

        self.submit_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    client.create_post(
                        &paths,
                        Draft {
                            text: &text,
                            quote_tweet_id: quote_tweet_id.as_deref(),
                            reply_to_post_id: reply_to_post_id.as_deref(),
                        },
                        oauth::unix_now(),
                    )
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

    /// The shortcut hint strip under the header (#58) — the answer to the
    /// issue's "how does anyone find out these exist" question.
    ///
    /// A permanent muted line rather than a `?` overlay: there are four
    /// bindings, they fit on one line, and a help screen nobody opens
    /// documents nothing. Built from [`shortcuts`], which the README quotes
    /// too, so a binding cannot be added without the list that announces it.
    fn shortcut_hints(&self) -> impl IntoElement {
        let theme = self.theme;
        let mut row = div().flex().gap_3().px_4().pb_2();
        for (key, label) in shortcuts() {
            row = row.child(
                div()
                    .text_color(rgb(theme.text_muted))
                    .child(format!("{key} {label}")),
            );
        }
        row
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
                    .child(
                        div()
                            .font_weight(FontWeight::BOLD)
                            .child(header_title(self.home_username.as_deref())),
                    )
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
            // #140: `flex_1` takes the *spare* width; it does not permit
            // shrinking below the content, because a flex child's
            // `min-width` defaults to `auto`. Long text therefore pushed
            // the column wider than the row and the overflow was clipped.
            // `min_w_0` is what lets it wrap instead.
            //
            // #103 is why this surfaced when it did: before the avatar got
            // `flex_shrink_0`, the avatar absorbed the overrun by
            // collapsing. Pinning it was right, and left the body as the
            // only place the extra width could go.
            .min_w_0()
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
            // #65: attached images, as thumbnails under the body.
            .when(!item.media.is_empty(), |column| {
                column.child(self.media_grid(&item.media, cx))
            })
            // #13: a quote (including a repost of a quote) embeds its source
            // as a bordered card under the text.
            .when_some(item.quoted.as_ref(), |column, quoted| {
                column.child(quote_card(
                    quoted,
                    theme,
                    self.media_grid_for(&quoted.media, cx),
                ))
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
            // #71: "Reply" — sets the composer's target; nothing is sent
            // until the draft is submitted.
            .when(offers_reply(self.signed_in_with_oauth, item), |column| {
                column.child(reply_row(item, theme, cx))
            })
            // #72: delete — own posts only, and never in one click.
            .when(
                offers_delete(
                    self.signed_in_with_oauth,
                    self.home_user_id.as_deref(),
                    self.home_username.as_deref(),
                    item,
                ),
                |column| column.child(self.delete_row(item, cx)),
            )
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
            .overflow_y_scroll()
            // #22: the handle is what makes the scroll position readable
            // at all. Without it a reload can only ever leave the viewport
            // where it was in *pixels*, which is the wrong place once rows
            // have been inserted above it.
            .track_scroll(&self.list_scroll);

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
/// via `cache::splice`, never merged ahead like a normal reload.
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
            // #58: every binding is scoped to this context rather than
            // registered globally — see `init`.
            .key_context(KEY_CONTEXT)
            // #118: a context only counts while its element is on the
            // window's focus path, and the real root is
            // `gpui_component::Root` (see `main`) — so without this the
            // path stopped above the context and every binding missed.
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Reload, _window, cx| {
                // Same path the header's button takes, including #10's
                // interval and #57's cooldown reporting: a shortcut must
                // not be a way around the throttle that exists to stop
                // this app spending money in a loop.
                this.reload(ReloadTrigger::UserAction, cx);
            }))
            .on_action(cx.listener(|this, _: &SubmitPost, window, cx| {
                this.submit_post(window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusComposer, window, cx| {
                this.compose_input
                    .update(cx, |input, cx| input.focus(window, cx));
            }))
            .on_action(cx.listener(|this, _: &BlurComposer, window, _cx| {
                // Focus only. The draft is left exactly as typed: losing it
                // to a stray `esc` is unrecoverable, and #14 already treats
                // never losing a draft as the composer's main promise.
                //
                // Back to the timeline rather than dropped with
                // `window.blur()` (#118): an empty focus path took the
                // `Timeline` context out of reach, so `esc` disabled the
                // shortcuts and half the menu bar until the next click.
                window.focus(&this.focus_handle);
            }))
            .on_action(cx.listener(|_this, _: &ShowAbout, window, cx| {
                // The receiver is dropped rather than awaited: there is one
                // button, so which one was pressed carries no information.
                // `Quit` is the action that has to reach the `App` — this
                // one only has to reach a window, so it stays here with the
                // rest of them.
                drop(window.prompt(
                    gpui::PromptLevel::Info,
                    "twigpui",
                    Some(&format!(
                        "Version {}\n\nA development-only X timeline viewer \
                         for macOS, built with gpui.",
                        env!("CARGO_PKG_VERSION")
                    )),
                    &["OK"],
                    cx,
                ));
            }))
            .on_action(cx.listener(|this, _: &ScrollToTop, _window, cx| {
                // #22: purely local — no request, no gate, nothing to
                // report. `scroll_to_top_of_item(0)` rather than a pixel
                // offset so it lands on the newest row itself.
                this.list_scroll.scroll_to_top_of_item(0);
                cx.notify();
            }))
            .on_action(cx.listener(|_this, _: &Minimize, window, _cx| {
                window.minimize_window();
            }))
            .on_action(cx.listener(|_this, _: &CloseWindow, window, _cx| {
                // With one window this ends the app, exactly as `cmd-q`
                // does, and like `cmd-q` it does not ask first (#109). An
                // unsent draft goes with it — the same hazard `cmd-q` has
                // always had, not a new one this introduces.
                window.remove_window();
            }))
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(theme.bg))
            .text_color(rgb(theme.text))
            .text_sm()
            .child(self.header(cx))
            // #58: the bindings, stated on screen rather than only in the
            // README — see `shortcut_hints`.
            .child(self.shortcut_hints())
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

#[cfg(test)]
mod tests {
    use super::reload_policy::{preserved_scroll_target, reload_cooldown};
    use super::render::{
        avatar_initial, is_own_post, like_action_label, post_permalink, profile_url,
        repost_action_label,
    };
    use super::{
        ComposeStatus, Cooldown, CooldownTick, PostLink, PostMedia, PostMetrics, ReloadNotice,
        ReloadTrigger, RepliedTo, Theme, ThreadFetchState, TimelineItem, TimelineState,
        ToggleState, action_post_id, at_the_post_cap, byline, compose_error_message,
        cooldown_label, cooldown_tick, format_timestamp, header_title, media_badge, media_columns,
        metrics_label, offers_delete, offers_like, offers_load_older, offers_quote,
        offers_reauthorize, offers_reply, offers_repost, rate_limit, reload_failure_outcome,
        reload_gate, reload_start_state, reply_banner_label, reply_target_label,
        repost_banner_label, thread_action_label, usage, usage_color, usage_label,
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
            original_post_id: None,
            media: Vec::new(),
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

    /// A repost row as #13's join builds one: the body is the original's,
    /// `id` is the retweet activity's, and #52's `original_post_id` is what
    /// every write endpoint should act on.
    fn repost_row_item(row_id: &str, original_id: &str, original_author: &str) -> TimelineItem {
        let mut item = item_with(row_id, original_author, Some("bob"));
        item.original_post_id = Some(original_id.to_string());
        item
    }

    // --- #22: keeping the reader in place across a reload ---

    #[test]
    fn a_reader_at_the_top_is_left_alone() {
        // New posts arriving above nothing is what someone at the top
        // wants to see, so this declines rather than scrolling.
        assert_eq!(
            preserved_scroll_target(&["2", "3"], &["1", "2", "3"], 0),
            None
        );
    }

    #[test]
    fn a_scrolled_reader_is_moved_down_by_the_number_of_new_posts() {
        // Twenty rows down, six posts arrive: without this the viewport
        // stays put and the text under the reader's eyes changes.
        let previous: Vec<String> = (0..30).map(|n| n.to_string()).collect();
        let previous_ids: Vec<&str> = previous.iter().map(String::as_str).collect();
        let fresh = ["a", "b", "c", "d", "e", "f"];
        let new_ids: Vec<&str> = fresh.iter().copied().chain(previous_ids.clone()).collect();

        assert_eq!(
            preserved_scroll_target(&previous_ids, &new_ids, 20),
            Some(26)
        );
    }

    #[test]
    fn a_reload_that_brings_nothing_new_leaves_the_position_alone() {
        assert_eq!(
            preserved_scroll_target(&["1", "2", "3"], &["1", "2", "3"], 7),
            None
        );
    }

    #[test]
    fn only_the_leading_run_of_new_ids_counts() {
        // An id further down is a post that moved, not one that arrived.
        // Scrolling for it would push the reader past what they were on.
        assert_eq!(
            preserved_scroll_target(&["1", "2"], &["new", "1", "also-new", "2"], 5),
            Some(6)
        );
    }

    // --- #65: attached media ---

    #[test]
    fn one_image_is_laid_out_in_a_single_column() {
        assert_eq!(media_columns(1), 1);
    }

    #[test]
    fn two_or_more_images_are_laid_out_in_two_columns() {
        // Three across would each be too narrow to read at the fixed cell
        // height, and X's own maximum of four is two rows of two.
        assert_eq!(media_columns(2), 2);
        assert_eq!(media_columns(3), 2);
        assert_eq!(media_columns(4), 2);
    }

    #[test]
    fn the_column_count_is_never_zero() {
        // `media_grid` passes this straight to `chunks`, which panics on 0.
        assert_eq!(media_columns(0), 1);
    }

    #[test]
    fn a_photo_gets_no_badge() {
        assert_eq!(media_badge(Some("photo")), None);
    }

    #[test]
    fn video_and_gif_say_which_they_are() {
        // Neither plays here, so the badge is the only thing distinguishing
        // a still from a photo.
        assert_eq!(media_badge(Some("video")), Some("Video"));
        assert_eq!(media_badge(Some("animated_gif")), Some("GIF"));
    }

    #[test]
    fn an_unrecognized_media_type_gets_no_badge() {
        // Forward compatibility: something X invents later should render as
        // a bare still, not as a label nobody can interpret.
        assert_eq!(media_badge(Some("hologram")), None);
        assert_eq!(media_badge(None), None);
    }

    // --- #72: delete ---

    #[test]
    fn offers_delete_on_ones_own_post() {
        assert!(offers_delete(
            true,
            Some("me-id"),
            Some("bob"),
            &item_with("1", "bob", None)
        ));
    }

    #[test]
    fn does_not_offer_delete_on_someone_elses_post() {
        // X rejects it, and this is irreversible — no reason to offer a
        // click that can only fail.
        assert!(!offers_delete(
            true,
            Some("me-id"),
            Some("bob"),
            &item_with("1", "alice", None)
        ));
    }

    #[test]
    fn does_not_offer_delete_on_a_repost_row_even_of_ones_own_post() {
        // Unlike every other action since #52, this one stays withheld: the
        // row reads as "my repost", but the delete would destroy the
        // original. Removing a repost is the repost toggle's job.
        let mut item = item_with("activity-id", "bob", Some("bob"));
        item.original_post_id = Some("original-id".to_string());
        assert!(!offers_delete(true, Some("me-id"), Some("bob"), &item));
    }

    #[test]
    fn does_not_offer_delete_before_the_signed_in_id_resolves() {
        assert!(!offers_delete(
            true,
            None,
            Some("bob"),
            &item_with("1", "bob", None)
        ));
    }

    #[test]
    fn does_not_offer_delete_before_the_signed_in_handle_resolves() {
        // `is_own_post` treats an unresolved handle as "not mine", which is
        // the safe direction for an irreversible action.
        assert!(!offers_delete(
            true,
            Some("me-id"),
            None,
            &item_with("1", "bob", None)
        ));
    }

    #[test]
    fn does_not_offer_delete_without_an_oauth_session() {
        assert!(!offers_delete(
            false,
            Some("me-id"),
            Some("bob"),
            &item_with("1", "bob", None)
        ));
    }

    // --- #71: reply ---

    #[test]
    fn offers_reply_once_signed_in_with_oauth() {
        assert!(offers_reply(true, &item_with("1", "alice", None)));
    }

    #[test]
    fn does_not_offer_reply_without_oauth() {
        // The composer itself isn't reachable without OAuth — nowhere for a
        // reply to go.
        assert!(!offers_reply(false, &item_with("1", "alice", None)));
    }

    #[test]
    fn offers_reply_on_ones_own_post() {
        // X accepts replying to yourself, and self-threading is a normal
        // way to write.
        assert!(offers_reply(true, &item_with("1", "me", None)));
    }

    #[test]
    fn reply_target_label_names_the_author() {
        assert_eq!(
            reply_target_label("XDevelopers"),
            "Replying to @XDevelopers"
        );
    }

    #[test]
    fn reply_target_label_without_a_handle() {
        // Same gap `reply_banner_label` already handles: an author who
        // never expanded.
        assert_eq!(reply_target_label(""), "Replying to a post");
    }

    // --- #52: a repost row acts on the original ---

    #[test]
    fn a_repost_row_acts_on_the_original_post_not_the_retweet_activity() {
        let item = repost_row_item("activity-id", "original-id", "alice");
        assert_eq!(action_post_id(&item), "original-id");
    }

    #[test]
    fn an_ordinary_row_acts_on_its_own_id() {
        assert_eq!(action_post_id(&item_with("1", "alice", None)), "1");
    }

    #[test]
    fn offers_repost_on_a_repost_row_now_that_the_original_id_is_carried() {
        // The workaround this replaces withheld the button here, because
        // `item.id` is the retweet activity's id. #52 carries the
        // original's, so the button is safe to offer.
        let item = repost_row_item("activity-id", "original-id", "alice");
        assert!(offers_repost(true, Some("2244994945"), Some("bob"), &item));
    }

    #[test]
    fn offers_quote_on_a_repost_row() {
        let item = repost_row_item("activity-id", "original-id", "alice");
        assert!(offers_quote(true, &item));
    }

    #[test]
    fn offers_like_on_a_repost_row() {
        let item = repost_row_item("activity-id", "original-id", "alice");
        assert!(offers_like(true, Some("me-id"), &item));
    }

    #[test]
    fn a_repost_row_still_withholds_repost_when_the_original_is_ones_own_post() {
        // The `is_own_post` guard now compares against the *original*
        // author, which is whose post would actually be reposted — the
        // reposter's handle is irrelevant to what the API would reject.
        let item = repost_row_item("activity-id", "original-id", "bob");
        assert!(!offers_repost(true, Some("2244994945"), Some("bob"), &item));
    }

    #[test]
    fn replying_from_a_repost_row_answers_the_original_post() {
        // The trap #71 calls out: `in_reply_to_tweet_id` pointing at the
        // retweet activity would hang the reply off a different
        // conversation, and nothing about that failure is visible.
        let item = repost_row_item("activity-id", "original-id", "alice");
        assert_eq!(action_post_id(&item), "original-id");
        assert!(offers_reply(true, &item));
    }

    #[test]
    fn a_repost_rows_permalink_points_at_the_original_post() {
        let item = repost_row_item("activity-id", "original-id", "alice");
        assert_eq!(
            post_permalink(&item.author_username, action_post_id(&item)),
            "https://x.com/alice/status/original-id"
        );
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
    fn header_title_names_the_signed_in_account() {
        assert_eq!(header_title(Some("alice")), "@alice — Home timeline");
    }

    #[test]
    fn header_title_falls_back_before_me_has_resolved() {
        // The only case left since #33: the window always shows the home
        // timeline, so the only unknown is whose it is.
        assert_eq!(header_title(None), "Home timeline");
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
        // `cache::splice` truncates back to the cap, so a click here
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
                original_post_id: None,
                media: Vec::new(),
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
    /// #118: the timeline's own root has to sit on the window's focus
    /// path from the first frame, with no click anywhere.
    ///
    /// This is the property, not the mechanism: gpui resolves an action
    /// against the focused element's ancestry, so an unfocused timeline
    /// takes the `Timeline` key context and every handler under it out of
    /// reach. `cmd-r` matched nothing and the menu bar's Reload / New Post
    /// / Submit Post either greyed out or dispatched into nowhere. Only
    /// `Quit` worked, because it lives on the `App`.
    #[gpui::test]
    fn the_timeline_is_focused_from_the_first_frame(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;

        cx.update(gpui_component::init);
        cx.update(crate::menu::init);

        let timeline_slot = std::rc::Rc::new(std::cell::RefCell::new(None));
        let window = {
            let slot = timeline_slot.clone();
            cx.add_window(move |window, cx| {
                let timeline = cx
                    .new(|cx| super::TimelineView::new(smoke_config(), smoke_paths(), window, cx));
                *slot.borrow_mut() = Some(timeline.clone());
                gpui_component::Root::new(timeline, window, cx)
            })
        };
        let timeline = timeline_slot.borrow().clone().unwrap();
        cx.run_until_parked();

        // Deliberately no click and no `input.focus(..)` before this: the
        // bug was that the app needed one.
        cx.update_window(window.into(), |_, window, cx| {
            let _ = window.draw(cx);
            timeline.update(cx, |view, _cx| {
                assert!(
                    view.focus_handle.is_focused(window),
                    "the timeline root is off the focus path, so its actions are unreachable"
                );
            });
        })
        .unwrap();
    }

    /// #118: leaving the composer must hand focus back rather than drop it.
    ///
    /// `window.blur()` left the window with an empty focus path, which
    /// disabled the shortcuts and half the menu bar until something was
    /// clicked — the same failure as the startup one, reached by pressing
    /// `esc`.
    #[gpui::test]
    fn leaving_the_composer_returns_focus_to_the_timeline(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;

        cx.update(gpui_component::init);
        cx.update(crate::menu::init);

        let timeline_slot = std::rc::Rc::new(std::cell::RefCell::new(None));
        let window = {
            let slot = timeline_slot.clone();
            cx.add_window(move |window, cx| {
                let timeline = cx.new(|cx| {
                    let mut view =
                        super::TimelineView::new(smoke_config(), smoke_paths(), window, cx);
                    view.signed_in_with_oauth = true;
                    view
                });
                *slot.borrow_mut() = Some(timeline.clone());
                gpui_component::Root::new(timeline, window, cx)
            })
        };
        let timeline = timeline_slot.borrow().clone().unwrap();
        cx.run_until_parked();

        cx.update_window(window.into(), |_, window, cx| {
            timeline.update(cx, |view, cx| {
                view.compose_input
                    .update(cx, |input, cx| input.focus(window, cx));
            });
        })
        .unwrap();
        cx.run_until_parked();

        cx.update_window(window.into(), |_, window, cx| {
            let _ = window.draw(cx);
            timeline.update(cx, |view, _cx| {
                assert!(
                    !view.focus_handle.is_focused(window),
                    "the composer should hold focus once focused"
                );
            });
            // The action itself, not `window.focus(..)` directly: the
            // handler is what this is checking, and reproducing its body
            // in the test would pass no matter what the handler did.
            window.dispatch_action(Box::new(crate::menu::BlurComposer), cx);
        })
        .unwrap();
        cx.run_until_parked();

        cx.update_window(window.into(), |_, window, cx| {
            let _ = window.draw(cx);
            timeline.update(cx, |view, _cx| {
                assert!(
                    view.focus_handle.is_focused(window),
                    "focus must return to the timeline, not be dropped"
                );
            });
        })
        .unwrap();
    }

    /// The `Config` the window smoke tests run against.
    fn smoke_config() -> crate::config::Config {
        crate::config::Config {
            oauth_client_id: "client-123".to_string(),
            target_username: "XDevelopers".to_string(),
            max_results: 20,
            min_fetch_interval_seconds: 60,
            theme: crate::theme::ThemeMode::Light,
            log_level: crate::log::Level::default(),
            request_price: None,
            daily_request_budget: None,
        }
    }

    /// `Paths` rooted in a scratch directory, for the window smoke tests.
    fn smoke_paths() -> crate::paths::Paths {
        let home = std::env::temp_dir().join("twigpui-smoke");
        let home = home.display().to_string();
        crate::paths::Paths::from_vars(move |key| (key == "HOME").then(|| home.clone())).unwrap()
    }

    #[gpui::test]
    fn the_window_root_renders_without_panicking(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;

        let home = std::env::temp_dir().join("twigpui-smoke");
        let home = home.display().to_string();
        let paths =
            crate::paths::Paths::from_vars(move |key| (key == "HOME").then(|| home.clone()))
                .unwrap();
        let config = crate::config::Config {
            oauth_client_id: "client-123".to_string(),
            target_username: "XDevelopers".to_string(),
            max_results: 20,
            min_fetch_interval_seconds: 60,
            theme: crate::theme::ThemeMode::Light,
            log_level: crate::log::Level::default(),
            request_price: None,
            daily_request_budget: None,
        };

        cx.update(gpui_component::init);
        // #58: `KeyBinding::new` panics on a keystroke it cannot parse, so
        // running this here turns a typo in a binding into a failing test
        // rather than a crash on the user's first launch.
        cx.update(crate::menu::init);

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
                    original_post_id: None,
                    media: vec![PostMedia {
                        url: "https://pbs.twimg.com/media/one.jpg".to_string(),
                        kind: Some("photo".to_string()),
                        width: Some(1200),
                        height: Some(675),
                        alt_text: Some("a rendered image".to_string()),
                    }],
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
