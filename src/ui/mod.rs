use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use gpui::{
    AnyElement, Context, Div, Entity, FocusHandle, Focusable as _, FontWeight, ScrollHandle,
    SharedString, Subscription, Task, Window, div, img, prelude::*, px, rgb, svg,
};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::assets;
use crate::avatar;
use crate::browser;
use crate::cache;
use crate::compose::{self, ComposeState, ComposeStatus};
use crate::config::Config;
use crate::fixture::Fixture;
use crate::image_cache;
use crate::like;
use crate::log;
mod auto_refresh;
mod list_picker;
mod list_sync;
mod reload_policy;
mod render;
mod tasks;

// Children rather than siblings of `ui` (#126): a child module can see its
// parent's private items, so `TimelineState`, `ReloadNotice` and
// `TimelineView` itself stay private to `ui` instead of being widened to
// `pub(crate)` merely to be reachable from the file next door. Widening
// them would mean "anything in the crate may touch this", which is the
// opposite of what splitting the file was for.
use auto_refresh::{Pending, pending_after_poll, pending_label};
use list_sync::{SyncOff, SyncStatus, SyncTrigger};
use reload_policy::{
    CooldownTick, at_the_post_cap, cooldown_label, cooldown_tick, newly_arrived, offers_load_older,
    preserved_scroll_target, reload_failure_outcome, reload_gate, reload_outcome_label,
    reload_start_state,
};
use render::Addressable as _;
use render::{
    AVATAR_SIZE, MAX_RENDERED_MEDIA, MEDIA_CELL_HEIGHT, author_link, avatar_placeholder, byline,
    compose_error_message, format_timestamp, header_title, like_row, link_row, media_badge,
    media_columns, new_posts_bar, notice, offers_delete, offers_like, offers_quote,
    offers_reauthorize, offers_reply, offers_repost, open_post_link, quote_card, quote_row,
    reload_notice_banner, render_thread_chain, reply_banner_label, reply_row, reply_target_label,
    repost_banner_label, repost_row, session_notice_banner, sign_in_pill, thread_action_label,
    thread_toggle_row, usage_color, usage_label, with_count,
};
use render::{RowCounts, row_counts};

use crate::menu::{
    BlurComposer, CloseWindow, FocusComposer, KEY_CONTEXT, Minimize, Reload, ScrollToTop,
    ShowAbout, ShowNewPosts,
};
use crate::oauth;
use crate::paths::Paths;
use crate::rate_limit;
use crate::repost;
use crate::sync;
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
    /// The request went out and came back (#141) — how many posts it
    /// brought, including none.
    ///
    /// The other two variants report that something went wrong, and until
    /// this one a successful reload said nothing at all: the header's
    /// button flicked to `Loading…` and back, which on a fast response is
    /// a frame or two, and is not where anyone is looking after `cmd-r`.
    /// Rendered in the muted color rather than `danger`, since it is the
    /// one variant that is not a problem.
    Outcome(SharedString),
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

/// Where the window's first screenful comes from (#146).
///
/// The seam this enum draws is the point: until it existed,
/// [`TimelineView::new`] always went straight to [`TimelineView::start`],
/// which resolves a credential, reads the response cache and fetches when
/// that comes back empty. There was no way to hand the view a timeline —
/// the view went and got one.
///
/// So `main` now decides where the data comes from and the view renders
/// whatever it is given, which is what makes a screen reproducible without
/// an account.
#[derive(Debug)]
pub(crate) enum Startup {
    /// Resolve a credential, read the cache, fetch if there is nothing on
    /// file. What every launch did before #146 and what an ordinary launch
    /// still does.
    Live,
    /// Draw these posts and stop.
    ///
    /// **No `XClient` is ever built in this mode**, which is not a
    /// convention but the reason a fixture cannot cost anything: every
    /// paid path in this view is behind `self.client`, and there is
    /// nothing there to reach.
    Fixture(Box<Fixture>),
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
    /// Which timeline fills the window (#161): decided in [`Self::new`]
    /// by [`list_picker::initial_source`], reassigned only by
    /// [`Self::switch_source`] (#164), which also resets everything below
    /// that belongs to one source rather than to the window.
    ///
    /// Read by every path that touches the timeline: [`Self::start`],
    /// [`Self::reload`], [`Self::load_older`] and [`Self::confirm_delete`]
    /// all take it so the cache file they read, the endpoint they spend a
    /// request on, and the file a delete rewrites are the same source.
    source: cache::TimelineSource,
    /// The lists the picker can name (#164), from its cache or its last
    /// fetch. Empty until the fetch button has been pressed once.
    owned_lists: Vec<crate::x_api::ListSummary>,
    /// The in-flight fetch of `owned_lists`, if any; `fetch`'s
    /// cancel-on-drop contract, and what stops a second click.
    lists_fetch: Option<Task<()>>,
    /// Where a switch is remembered ([`Paths::selection_file`]), or `None`
    /// for a fixture window — see [`list_picker::saved_selection_for`].
    selection_file: Option<PathBuf>,
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
    /// Holds the background list sync alive — the loop that keeps
    /// `config.list_id`'s membership mirroring the accounts this app
    /// follows. Started from [`Self::start`] and again from
    /// [`Self::sign_in`], since a re-authorization is what grants the
    /// scopes the sync may have been refused for. Reassigning drops the
    /// previous loop — `usage_refresh`'s cancel-on-drop contract — so
    /// there is never more than one of them working the same plan file,
    /// and the last one retires with the window.
    auto_sync: Option<Task<()>>,
    /// What the list sync is doing, for the status bar to report (#174).
    ///
    /// Written by the loop after every tick and by the gates before it
    /// starts; read only by [`Self::status_bar`]. Until #174 the whole
    /// feature was invisible from the window: a stopped sync, a sync
    /// eleven hundred writes behind, and a sync with nothing to do all
    /// looked exactly alike, which is to say like nothing at all.
    sync_status: SyncStatus,
    /// Whether the status bar is asking to confirm a manual sync (#174) —
    /// the two-step behind the most expensive click in this window, in
    /// the shape `pending_delete` uses. See `list_sync`'s module doc.
    pending_sync: bool,
    /// Holds the auto-refresh loop alive (#21) — the timer that polls the
    /// timeline for new posts while the window is open. Its own slot
    /// rather than `fetch`'s, deliberately: assigning `fetch` from here
    /// would cancel whatever reload the reader had just started, and the
    /// two are not alternatives. Same cancel-on-drop contract as
    /// `auto_sync`, and never spawned at all when `config.auto_refresh` is
    /// off — see [`Self::start_auto_refresh`], which is what makes #21's
    /// "switch it off and the app sends nothing" a guarantee rather than a
    /// tendency.
    auto_refresh: Option<Task<()>>,
    /// What the most recent poll fetched, held back from the screen until
    /// the reader asks for it (#21) — see [`Pending`] and
    /// [`pending_after_poll`].
    ///
    /// The whole reason auto-refresh does not simply replace `state`: a
    /// fetch nobody asked for must not move the text under a reader's
    /// eyes. `keep_the_reader_in_place` compensates the scroll for a
    /// reload they pressed, which is a different situation — they are
    /// expecting the list to change. Here nothing changes until the pill
    /// is pressed.
    ///
    /// `None` whenever there is nothing waiting: no poll has landed, the
    /// last one brought nothing new, or something has since replaced the
    /// timeline from a fresher source — see [`Self::clear_pending`] for
    /// which paths do that and why a stale buffer is not merely useless
    /// but wrong.
    pending: Option<Pending>,
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
        startup: Startup,
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

        // #161/#164: taken before `config` is moved below.
        let source = list_picker::initial_source(
            list_picker::saved_selection_for(&startup, &paths),
            config.list_id.as_deref(),
        );
        let owned_lists = list_picker::cached_lists_or_empty(&paths);
        let selection_file = matches!(startup, Startup::Live).then(|| paths.selection_file());

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
            source,
            owned_lists,
            lists_fetch: None,
            selection_file,
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
            auto_sync: None,
            // #174: the truthful starting point. Nothing is signed in
            // yet, which is one of the gates, and saying so beats an
            // "idle" that has never run.
            sync_status: SyncStatus::Off(SyncOff::NotSignedIn),
            pending_sync: false,
            auto_refresh: None,
            pending: None,
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
        match startup {
            Startup::Live => this.start(cx),
            Startup::Fixture(fixture) => this.show_fixture(*fixture, cx),
        }
        this.refresh_usage(cx);
        this
    }

    /// Render a fixture and nothing else (#146).
    ///
    /// `client` stays `None`, which is what makes this free rather than
    /// merely cheap: every request in this view goes through it, so there
    /// is no path from here to a charge — not a reload, not a like, not a
    /// thread walk. Buttons that need a client are simply inert.
    ///
    /// `signed_in_with_oauth` and the scope are set anyway, because the
    /// affordances they gate are most of what there is to look at. A
    /// fixture drawn as a signed-out timeline would be missing the rows
    /// worth checking.
    fn show_fixture(&mut self, fixture: Fixture, cx: &mut Context<'_, Self>) {
        self.signed_in_with_oauth = true;
        // Every scope the app requests, so no affordance is withheld for
        // want of one. `list.read` (#161) belongs here even though a
        // fixture never fetches: `offers_reauthorize` reads the scope, not
        // the network, so leaving it out put a "Re-authorize" button on
        // every list-mode fixture — a permanent fixture of a screen meant
        // for comparing layouts.
        self.oauth_scope = Some(format!(
            "{} {} {}",
            oauth::tokens::TWEET_WRITE_SCOPE,
            oauth::tokens::LIKE_WRITE_SCOPE,
            oauth::tokens::LIST_READ_SCOPE
        ));
        self.home_user_id = Some(fixture.signed_in_as.id);
        self.home_username = Some(fixture.signed_in_as.username);
        self.owned_lists = fixture.lists;
        self.state = TimelineState::Loaded(fixture.items);
        // #21: built the same way a real poll's buffer is, from the same
        // pure function — the fixture supplies the posts, not the count,
        // so the bar cannot say something a poll could not. `pending`
        // holds the whole list it would display, which is the fixture's
        // unseen posts followed by the ones already on screen.
        if !fixture.pending.is_empty() {
            let displayed: Vec<&str> = match &self.state {
                TimelineState::Loaded(items) => items.iter().map(|item| item.id.as_str()).collect(),
                _ => Vec::new(),
            };
            let combined: Vec<TimelineItem> = fixture
                .pending
                .iter()
                .cloned()
                .chain(match &self.state {
                    TimelineState::Loaded(items) => items.clone(),
                    _ => Vec::new(),
                })
                .collect();
            self.pending = pending_after_poll(&displayed, combined);
        }
        // Avatars and attached images still download, from `pbs.twimg.com`
        // rather than the API — no quota, no credits (see `avatar`). A
        // fixture whose URLs are unreachable renders the same frames it
        // would while they were still in flight, which is what a layout
        // check needs anyway.
        self.refresh_images(cx);
        cx.notify();
    }

    /// The like button state to render for `post_id` (#68) — see
    /// [`Self::repost_state_for`], which this mirrors.
    fn like_state_for(&self, post_id: &str) -> ToggleState {
        self.like_overrides
            .get(post_id)
            .cloned()
            .unwrap_or_else(|| ToggleState::new(self.liked_ids.contains(post_id)))
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
                .rounded(theme::RADIUS_THUMB)
                .into_any_element(),
            None => div()
                .h(MEDIA_CELL_HEIGHT)
                .w(MEDIA_CELL_HEIGHT)
                .rounded(theme::RADIUS_THUMB)
                .bg(rgb(theme.border))
                .into_any_element(),
        };

        let mut cell = div()
            .addressable(format!("media-{}", media.url))
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
                        .addressable(format!("delete-confirm-{}", item.id))
                        .text_color(rgb(theme.danger))
                        .child("Delete permanently")
                        .on_click(cx.listener(move |this, _event, _window, cx| {
                            this.confirm_delete(confirm_id.clone(), cx);
                        })),
                )
                .child(
                    div()
                        .addressable(format!("delete-cancel-{}", item.id))
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
                    .addressable(format!("delete-{}", item.id))
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

    /// The repost/un-repost toggle for one post (#15), rendered whenever
    /// [`offers_repost`] allows it for `item`.
    fn repost_button(&self, item: &TimelineItem, cx: &mut Context<'_, Self>) -> AnyElement {
        // #52: element id from the row, request target from the original.
        let target = action_post_id(item);
        let state = self.repost_state_for(target);
        repost_row(&item.id, target, &state, self.theme, cx)
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
                    .addressable("compose-remove-quote")
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
                    .addressable("compose-remove-reply")
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
    fn composer(&self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
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
        // #95: the counter and the Post button appear once the field is
        // being used, so an idle window shows one quiet line instead of a
        // count and a button for a post nobody is writing.
        //
        // A non-empty draft keeps them regardless of focus. Hiding the
        // button while a draft exists would leave the only way to send it
        // behind clicking back into the field — and #14 treats never
        // losing a draft as the composer's main promise, which a hidden
        // send button quietly breaks.
        let showing_controls = self.compose_input.focus_handle(cx).is_focused(window)
            || !text.trim().is_empty()
            || is_submitting;

        div()
            .flex()
            .flex_col()
            .gap_2()
            .px(theme::ROW_PAD_X)
            .py(theme::ROW_PAD_Y)
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
            .when(showing_controls, |composer| {
                composer.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                // #95: a readout beside a control, not body
                                // text.
                                .text_size(theme::TEXT_META)
                                .text_color(rgb(counter_color))
                                .child(format!("{length}/{}", compose::MAX_WEIGHTED_LENGTH)),
                        )
                        .child(
                            div()
                                .addressable("compose-submit")
                                .px_2()
                                .py_1()
                                .rounded(theme::RADIUS_CONTROL)
                                // #95: this one *is* a default button — it is
                                // the composer's whole point — so it keeps the
                                // accent fill while it can be pressed. What
                                // changes is the other state: an unpressable
                                // button used to be a solid dark grey block,
                                // which reads as a control that is merely a
                                // different color rather than one that is off.
                                // macOS drains the fill instead.
                                .when(can_submit, |button| {
                                    button
                                        .bg(rgb(theme.accent))
                                        .text_color(rgb(theme.button_label))
                                })
                                .when(!can_submit, |button| {
                                    button
                                        .border_1()
                                        .border_color(rgb(theme.border))
                                        .text_color(rgb(theme.text_tertiary))
                                })
                                .text_size(theme::TEXT_META)
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
            })
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

        div()
            .flex()
            .items_center()
            .gap_3()
            // #95: a toolbar, not a two-line masthead. The request count
            // that used to sit under the title moved to `status_bar`, which
            // leaves one line — so the strip is sized like a macOS toolbar
            // rather than padded to whatever two stacked lines needed.
            .h(theme::TOOLBAR_HEIGHT)
            .px(theme::ROW_PAD_X)
            .bg(rgb(theme.bg_header))
            .border_b_1()
            .border_color(rgb(theme.border))
            // #95's frame, #164's segments: Home and every owned list.
            .child(self.list_picker(cx))
            .children(self.lists_control(cx))
            .child(
                div()
                    .text_size(theme::TEXT_META)
                    .text_color(rgb(theme.text_tertiary))
                    .child(header_title(self.home_username.as_deref())),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .ml_auto()
                    // #14: an already-signed-in session from before #14
                    // holds no `tweet.write` scope — #31's exact lesson
                    // repeats here (an already-active session hides its own
                    // upgrade path) unless this stays reachable regardless
                    // of what the primary button currently says.
                    .when(
                        offers_reauthorize(
                            self.signed_in_with_oauth,
                            self.oauth_scope.as_deref(),
                            matches!(self.source, cache::TimelineSource::List(_)),
                        ),
                        |row| row.child(sign_in_pill("reauthorize", "Re-authorize", theme, cx)),
                    )
                    .child(self.primary_action_control(&label, busy, action, cx)),
            )
    }

    /// The toolbar's one action: reload, or sign in when there is no
    /// session yet (#95).
    ///
    /// The two look nothing alike on purpose. Reload is an icon — the
    /// action is constant, frequent, and named by a symbol every app
    /// shares, so spelling it out in a bordered button made the corner of
    /// every frame louder than the timeline. Its `label` still exists for
    /// the states that have something to say ("Loading…", a rate-limit
    /// countdown), but those already reach the reader through `body` and
    /// #57's banner, so here they only dim the icon.
    ///
    /// Sign-in keeps its words and its fill: with no session there is
    /// nothing else to do in the window, and an unlabelled glyph would be
    /// a puzzle at exactly the moment the app has to explain itself.
    fn primary_action_control(
        &self,
        label: &str,
        busy: bool,
        action: PrimaryAction,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let on_click = cx.listener(move |this, _event, _window, cx| match action {
            PrimaryAction::Reload => this.reload(ReloadTrigger::Polling, cx),
            PrimaryAction::SignIn => this.sign_in(cx),
        });

        match action {
            PrimaryAction::Reload => div()
                .addressable("primary-action")
                .p_1()
                .rounded(theme::RADIUS_CONTROL)
                .child(
                    svg()
                        .path(assets::RELOAD_ICON)
                        .size(theme::ICON_SIZE)
                        .text_color(rgb(if busy {
                            theme.text_tertiary
                        } else {
                            theme.text_muted
                        })),
                )
                .on_click(on_click)
                .into_any_element(),
            PrimaryAction::SignIn => div()
                .addressable("primary-action")
                .px_2()
                .py_1()
                .rounded(theme::RADIUS_CONTROL)
                .text_size(theme::TEXT_META)
                .when(busy, |button| {
                    button
                        .border_1()
                        .border_color(rgb(theme.border))
                        .text_color(rgb(theme.text_tertiary))
                })
                .when(!busy, |button| {
                    button
                        .bg(rgb(theme.accent))
                        .text_color(rgb(theme.button_label))
                })
                .child(label.to_string())
                .on_click(on_click)
                .into_any_element(),
        }
    }

    /// The strip along the bottom of the window (#95).
    ///
    /// Until #95 the request count sat under the window title, where it
    /// competed with the account name to be the first thing read on every
    /// frame. macOS keeps a window's running totals in a status bar
    /// instead — Finder's item count is the same idea — so that is where
    /// this one goes. #18's escalating color survives the move unchanged:
    /// the count still turns `warning` as it approaches
    /// `daily_request_budget` and `danger` once it is past.
    ///
    /// The kept-post count is only shown once a timeline has loaded. While
    /// signing in or fetching there is no number to give, and "0 / 200"
    /// would read as an empty cache rather than an unanswered question.
    fn status_bar(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = self.theme;

        // #18: request counts are always shown; an estimated amount is
        // appended only when `request_price` is configured (see
        // `usage_label`'s doc).
        let usage_status =
            usage::budget_status(self.usage_totals.today, self.config.daily_request_budget);
        let usage_text = usage_label(
            self.usage_totals.today,
            self.usage_totals.total,
            self.config.request_price,
        );
        let kept = match self.state {
            TimelineState::Loaded(ref items) => Some(items.len()),
            _ => None,
        };

        div()
            .flex()
            .items_center()
            .gap_3()
            .h(theme::STATUS_BAR_HEIGHT)
            .px(theme::ROW_PAD_X)
            .bg(rgb(theme.bg_header))
            .border_t_1()
            .border_color(rgb(theme.border))
            .text_size(theme::TEXT_META)
            .child(
                div()
                    .addressable("status-usage")
                    .text_color(rgb(usage_color(usage_status, theme)))
                    .child(usage_text),
            )
            // #174: what the list sync is doing, and — when it is
            // something to press — the way to start one. Beside the
            // request count rather than off in the toolbar because it is
            // the same kind of fact: a running total about the app rather
            // than about the timeline.
            //
            // The margin is not redundant with the row's `gap_3`, however
            // it reads. This is the only place in the window where two
            // bare text spans are siblings — everywhere else the children
            // carry their own padding — and on screen the gap does not
            // separate them at all: "Total: 11 req" and "List sync: …"
            // render touching, as "11 reqList sync". Raising the gap to
            // `gap_8` changes nothing, so the spacing has to come from
            // somewhere that demonstrably works here.
            //
            // #184: the margin is now under a test. Both segments are
            // named, so a window test can read their laid-out bounds back
            // and require that they do not touch — which is the whole
            // defect, and which nothing but a screenshot could catch when
            // this comment was written.
            .child(
                div()
                    .addressable("status-sync")
                    .ml(theme::ROW_PAD_X)
                    .child(self.sync_segment(cx)),
            )
            .when_some(kept, |bar, kept| {
                bar.child(
                    div()
                        .ml_auto()
                        .text_color(rgb(theme.text_tertiary))
                        .child(format!("{kept} / {} posts kept", cache::MAX_CACHED_POSTS)),
                )
            })
    }

    fn post_row(&self, item: &TimelineItem, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = self.theme;
        let byline = byline(&item.author_username);

        let counts = row_counts(item.metrics.as_ref());

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
            // #95: one meta line. The author, the byline, the timestamp,
            // and whichever of "reposted" / "replying to" applies all sit
            // together — until #95 the last two were their own full-width
            // lines above the name, which pushed a two-line post to four.
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_1()
                    .text_size(theme::TEXT_META)
                    // #70: the author name and handle open the profile on
                    // x.com. `profile_url` returns `None` when the username
                    // never expanded, in which case they stay plain text
                    // rather than becoming a link to nowhere.
                    .child(author_link(item, theme, cx))
                    .child(div().text_color(rgb(theme.text_muted)).child(byline))
                    .child(
                        div()
                            .text_color(rgb(theme.text_tertiary))
                            .child(format_timestamp(item.created_at.as_deref())),
                    )
                    // #13: a repost says who reposted it — the body by this
                    // point already holds the *original* post (see
                    // `TimelineResponse::into_items`'s join), not the outer
                    // post's own author.
                    .when_some(item.reposted_by.as_deref(), |line, reposted_by| {
                        line.child(
                            div()
                                .text_color(rgb(theme.text_tertiary))
                                .child(format!("· {}", repost_banner_label(reposted_by))),
                        )
                    })
                    // #12: who this post is replying to, shown at zero extra
                    // request cost — the parent's author is already in
                    // `includes` per #13's expansions.
                    .when_some(item.replied_to.as_ref(), |line, replied_to| {
                        line.child(
                            div()
                                .text_color(rgb(theme.text_tertiary))
                                .child(format!("· {}", reply_banner_label(replied_to))),
                        )
                    }),
            )
            .child(div().child(item.text.clone()))
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
            // #95: every action on one horizontal line, each carrying its
            // own count. This is the issue's main complaint — the same set
            // used to stack one label per line down the row.
            .child(self.action_row(item, &counts, cx))
            // #12: "Show thread" — only offered for a reply, since that's
            // the only case with a parent to walk. Deliberately not part of
            // `action_row`: a loaded thread expands into a whole chain of
            // posts, which cannot sit inside a one-line strip.
            .when_some(item.replied_to.as_ref(), |column, replied_to| {
                column.child(self.thread_section(&item.id, replied_to, cx))
            });

        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .gap_2()
                    .px(theme::ROW_PAD_X)
                    .py(theme::ROW_PAD_Y)
                    .child(self.avatar(item, theme))
                    .child(body),
            )
            // #95: the separator starts where the text does rather than
            // running under the avatar, which is the inset macOS's own
            // lists (Mail, Messages) use. It is a sibling of the row rather
            // than the row's own bottom border so the inset does not have
            // to be re-stated as padding.
            .child(
                div()
                    .h(px(1.0))
                    .ml(theme::SEPARATOR_INSET)
                    .bg(rgb(theme.border)),
            )
            .into_any_element()
    }

    /// Every action for one post on a single horizontal line (#95).
    ///
    /// Which actions appear is unchanged — each `offers_*` predicate still
    /// decides — but they now sit side by side with their engagement count
    /// beside them instead of stacking one per line above a separate
    /// metrics line. A like/repost whose request failed still renders its
    /// message, which grows this strip downward for that one row; that is
    /// `like_row`/`repost_row`'s own doing and is left alone here.
    fn action_row(
        &self,
        item: &TimelineItem,
        counts: &RowCounts,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = self.theme;

        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_4()
            .text_size(theme::TEXT_META)
            .text_color(rgb(theme.text_muted))
            // #71: "Reply" — sets the composer's target; nothing is sent
            // until the draft is submitted.
            .when(offers_reply(self.signed_in_with_oauth, item), |row| {
                row.child(with_count(
                    reply_row(item, theme, cx),
                    counts.replies.as_deref(),
                    theme,
                ))
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
                |row| {
                    row.child(with_count(
                        self.repost_button(item, cx),
                        counts.reposts.as_deref(),
                        theme,
                    ))
                },
            )
            // #68: like/unlike — see `offers_like`'s doc for which posts
            // get one. Unlike repost, this is offered on one's own posts.
            .when(
                offers_like(
                    self.signed_in_with_oauth,
                    self.home_user_id.as_deref(),
                    item,
                ),
                |row| {
                    row.child(with_count(
                        self.like_button(item, cx),
                        counts.likes.as_deref(),
                        theme,
                    ))
                },
            )
            // #16: "Quote" — see `offers_quote`'s doc for exactly which
            // posts get one (a repost row is withheld for the same reason
            // `offers_repost` withholds its own button).
            .when(offers_quote(self.signed_in_with_oauth, item), |row| {
                row.child(quote_row(item, theme, cx))
            })
            // #70: the post itself, on x.com.
            .child(open_post_link(item, theme, cx))
            // #72: delete — own posts only, and never in one click.
            .when(
                offers_delete(
                    self.signed_in_with_oauth,
                    self.home_user_id.as_deref(),
                    self.home_username.as_deref(),
                    item,
                ),
                |row| row.child(self.delete_row(item, cx)),
            )
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
            .addressable("timeline")
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
        .addressable("load-older")
        .px_4()
        .py_3()
        .text_color(rgb(theme.accent))
        .child("Load older")
        .on_click(cx.listener(|this, _event, _window, cx| this.load_older(cx)))
}

impl Render for TimelineView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
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
            .on_action(cx.listener(|this, _: &ShowNewPosts, _window, cx| {
                // #21: the bar's click handler, reached by keyboard. Free
                // — it shows a fetch the timer already paid for, and does
                // nothing at all when there is none.
                this.apply_pending(cx);
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
            .text_size(theme::TEXT_BODY)
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
            // #21: what auto-refresh fetched and is holding back. Beside
            // the banners rather than inside `body` for the same reason
            // they are — see `new_posts_bar` — except that this one is the
            // offer itself, not a report about one.
            .when_some(
                self.pending.as_ref().map(|pending| pending.count),
                |column, count| column.child(new_posts_bar(count, theme, cx)),
            )
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
                column.child(self.composer(window, cx))
            })
            .child(self.body(cx))
            // #95: the status bar, which is where the running request
            // count lives now that the header is a toolbar.
            .child(self.status_bar(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::reload_policy::{
        newly_arrived, preserved_scroll_target, reload_cooldown, reload_outcome_label,
    };
    use super::render::{
        avatar_initial, is_own_post, like_action_label, post_permalink, profile_url,
        repost_action_label,
    };
    use super::{
        ComposeStatus, Cooldown, CooldownTick, Fixture, PostLink, PostMedia, PostMetrics,
        ReloadNotice, ReloadTrigger, RepliedTo, RowCounts, Startup, SyncStatus, Theme,
        ThreadFetchState, TimelineItem, TimelineState, ToggleState, action_post_id,
        at_the_post_cap, byline, compose_error_message, cooldown_label, cooldown_tick,
        format_timestamp, header_title, media_badge, media_columns, offers_delete, offers_like,
        offers_load_older, offers_quote, offers_reauthorize, offers_reply, offers_repost,
        rate_limit, reload_failure_outcome, reload_gate, reload_start_state, reply_banner_label,
        reply_target_label, repost_banner_label, row_counts, thread_action_label, usage,
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
            original_post_id: None,
            media: Vec::new(),
        }
    }

    #[test]
    fn a_count_rides_beside_each_action() {
        // #95: the counts used to be one line of prose under the body.
        // They are now three separate labels, one per action, so each has
        // to come back on its own.
        let counts = row_counts(Some(&PostMetrics {
            replies: 12,
            reposts: 34,
            likes: 56,
        }));
        assert_eq!(counts.replies.as_deref(), Some("12"));
        assert_eq!(counts.reposts.as_deref(), Some("34"));
        assert_eq!(counts.likes.as_deref(), Some("56"));
    }

    #[test]
    fn a_zero_count_is_nothing_rather_than_a_zero() {
        // #67's rule, carried over by #95: a row that only got likes shows
        // one number, not two zeros beside the other actions.
        let counts = row_counts(Some(&PostMetrics {
            replies: 0,
            reposts: 0,
            likes: 3,
        }));
        assert_eq!(counts.replies, None);
        assert_eq!(counts.reposts, None);
        assert_eq!(counts.likes.as_deref(), Some("3"));
    }

    #[test]
    fn a_post_with_no_engagement_yet_carries_no_counts() {
        assert_eq!(
            row_counts(Some(&PostMetrics::default())),
            RowCounts::default()
        );
    }

    #[test]
    fn a_post_whose_metrics_never_expanded_carries_no_counts() {
        // `metrics: None` is a different thing from all-zero metrics — the
        // response simply did not include them — but the row renders the
        // same either way, and this is the case that used to be handled by
        // `when_some` at the call site.
        assert_eq!(row_counts(None), RowCounts::default());
    }

    #[test]
    fn a_large_count_is_abbreviated() {
        // Still #67's rule: seven digits beside an action would push the
        // rest of the strip around.
        let counts = row_counts(Some(&PostMetrics {
            replies: 1000,
            reposts: 12_345,
            likes: 2_400_000,
        }));
        assert_eq!(counts.replies.as_deref(), Some("1K"));
        assert_eq!(counts.reposts.as_deref(), Some("12.3K"));
        assert_eq!(counts.likes.as_deref(), Some("2.4M"));
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
            Some("tweet.read users.read tweet.write offline.access"),
            false
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

    // --- #141: saying what a reload did ---

    #[test]
    fn a_reload_that_brought_nothing_says_so() {
        // The case a reader is most likely to read as "my press did not
        // register": the screen is identical before and after.
        assert_eq!(reload_outcome_label(0), "No new posts.");
    }

    #[test]
    fn one_new_post_is_not_reported_in_the_plural() {
        assert_eq!(reload_outcome_label(1), "1 new post.");
        assert_eq!(reload_outcome_label(6), "6 new posts.");
    }

    #[test]
    fn the_outcome_counts_the_same_posts_the_scroll_does() {
        // Both read the leading run of unseen ids, so a reload cannot say
        // "3 new posts" while scrolling past a different number of them.
        let previous = ["1", "2", "3"];
        let new_ids = ["a", "b", "1", "2", "3"];

        assert_eq!(newly_arrived(&previous, &new_ids), 2);
        assert_eq!(
            preserved_scroll_target(&previous, &new_ids, 5),
            Some(7),
            "the scroll must move by exactly what the message claims"
        );
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
            Some("tweet.read users.read offline.access"),
            false
        ));
    }

    #[test]
    fn offers_reauthorize_when_the_scope_was_never_recorded() {
        // A pre-#14 token: "unknown" is treated the same as "insufficient".
        assert!(offers_reauthorize(true, None, false));
    }

    #[test]
    fn does_not_offer_reauthorize_once_every_write_scope_is_granted() {
        // `like.write` joined the set in #68; a session holding only
        // `tweet.write` is now genuinely under-scoped, which the test above
        // pins down.
        assert!(!offers_reauthorize(
            true,
            Some("tweet.read tweet.write like.write offline.access"),
            false
        ));
    }

    #[test]
    fn offers_reauthorize_for_a_session_that_predates_the_list_scope() {
        // #161: configuring a list on a session authorized before #167
        // added `list.read` gets a 403 from the only endpoint the window
        // reads. The button is the whole explanation, so it has to appear.
        assert!(offers_reauthorize(
            true,
            Some("tweet.read tweet.write like.write offline.access"),
            true
        ));
    }

    #[test]
    fn does_not_offer_reauthorize_for_a_list_once_list_read_is_granted() {
        assert!(!offers_reauthorize(
            true,
            Some("tweet.read tweet.write like.write list.read offline.access"),
            true
        ));
    }

    #[test]
    fn does_not_ask_for_list_read_when_no_list_is_configured() {
        // Someone reading the home timeline can never reach the 403, so
        // nagging them about a scope they do not use is noise.
        assert!(!offers_reauthorize(
            true,
            Some("tweet.read tweet.write like.write offline.access"),
            false
        ));
    }

    #[test]
    fn does_not_offer_reauthorize_without_an_oauth_session() {
        // Not signed in with OAuth at all — `offers_sign_in` is the
        // relevant affordance here, not this one.
        assert!(!offers_reauthorize(false, None, false));
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
        // Only the account. Which timeline is showing is the tab bar's to
        // say since #95, and saying it twice in one 44px strip was how the
        // toolbar ran out of room.
        assert_eq!(header_title(Some("alice")), "@alice");
    }

    #[test]
    fn header_title_falls_back_before_me_has_resolved() {
        // The only case left since #33: the window always shows the home
        // timeline, so the only unknown is whose it is. Until `/me`
        // answers there is no account to name, and the app's own name is
        // what a macOS toolbar carries in its place.
        assert_eq!(header_title(None), "twigpui");
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
                let timeline = cx.new(|cx| {
                    super::TimelineView::new(
                        smoke_config(),
                        smoke_paths(),
                        Startup::Live,
                        window,
                        cx,
                    )
                });
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
                    let mut view = super::TimelineView::new(
                        smoke_config(),
                        smoke_paths(),
                        Startup::Live,
                        window,
                        cx,
                    );
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
            list_id: None,
            // Off in the smoke tests: they render a window, and a paid
            // background loop is not part of what they are checking.
            auto_sync_list: false,
            sync_interval_seconds: 21_600,
            // Off for the same reason (#21).
            auto_refresh: false,
            auto_refresh_interval_seconds: 300,
        }
    }

    /// `Paths` rooted in a scratch directory, for the window smoke tests.
    fn smoke_paths() -> crate::paths::Paths {
        let home = std::env::temp_dir().join("twigpui-smoke");
        let home = home.display().to_string();
        crate::paths::Paths::from_vars(move |key| (key == "HOME").then(|| home.clone())).unwrap()
    }

    // --- #146 層 3: what a window can be asked without drawing it ---
    //
    // gpui does not run layout on the test platform, so nothing about
    // spacing, wrapping or size is assertable here — #182 is the standing
    // reminder of what that costs, and `--fixture` plus a screenshot
    // (#146's layer 2) is the only check that applies to those.
    //
    // What *is* observable is state and dispatch. So these tests cover
    // the half of the window that has nothing to do with pixels: which
    // action reaches which method, what a keystroke changes, and — since
    // most of this file spends money — which paths are guaranteed not to.
    //
    // Every one of them dispatches the real action rather than calling
    // the handler's body, for the reason
    // `leaving_the_composer_returns_focus_to_the_timeline` already
    // states: a test that reproduces the body passes no matter what the
    // handler is actually wired to.

    /// A window filled from `fixture`, and the view inside it.
    ///
    /// Extracted because the three tests above already triplicate this
    /// block and the ones below would have made it nine copies. Returns
    /// the handle as well as the view: dispatching an action needs the
    /// window, asserting the result needs the view.
    fn fixture_window(
        cx: &mut gpui::TestAppContext,
        fixture: Fixture,
    ) -> (
        gpui::WindowHandle<gpui_component::Root>,
        gpui::Entity<super::TimelineView>,
    ) {
        window_with(cx, smoke_paths(), Startup::Fixture(Box::new(fixture)))
    }

    /// A window started `startup` against `paths`, and the view inside it —
    /// what [`fixture_window`] is, minus the two things a live window
    /// wants to choose (#164's `a_switch_is_remembered_on_disk_at_once`
    /// starts live under its own directory so nothing else writes there).
    fn window_with(
        cx: &mut gpui::TestAppContext,
        paths: crate::paths::Paths,
        startup: Startup,
    ) -> (
        gpui::WindowHandle<gpui_component::Root>,
        gpui::Entity<super::TimelineView>,
    ) {
        use gpui::AppContext as _;

        cx.update(gpui_component::init);
        cx.update(crate::menu::init);

        let slot = std::rc::Rc::new(std::cell::RefCell::new(None));
        let window = {
            let slot = slot.clone();
            cx.add_window(move |window, cx| {
                let timeline = cx
                    .new(|cx| super::TimelineView::new(smoke_config(), paths, startup, window, cx));
                *slot.borrow_mut() = Some(timeline.clone());
                gpui_component::Root::new(timeline, window, cx)
            })
        };
        let timeline = slot.borrow().clone().unwrap();
        cx.run_until_parked();
        (window, timeline)
    }

    /// A fixture with `shown` already on screen and `waiting` held back —
    /// the shape #21's "N new posts" bar exists for.
    fn fixture_with(shown: &[&str], waiting: &[&str]) -> Fixture {
        Fixture {
            signed_in_as: crate::fixture::FixtureUser {
                id: "5685672".to_string(),
                username: "usadamasa".to_string(),
            },
            items: shown
                .iter()
                .map(|id| item_with(id, "someone", None))
                .collect(),
            pending: waiting
                .iter()
                .map(|id| item_with(id, "someone", None))
                .collect(),
            lists: Vec::new(),
        }
    }

    /// The ids the window is currently rendering.
    fn shown_ids(view: &super::TimelineView) -> Vec<String> {
        match &view.state {
            TimelineState::Loaded(items) => items.iter().map(|item| item.id.clone()).collect(),
            other => panic!("expected a loaded timeline, got {other:?}"),
        }
    }

    /// #146: a fixture window builds no `XClient` at all.
    ///
    /// `show_fixture`'s doc calls this "not a convention but the reason a
    /// fixture cannot cost anything" — every paid path in this view goes
    /// through `self.client`, so an absent one is what makes a screenshot
    /// free. Until now that was a sentence; this is the enforcement.
    #[gpui::test]
    fn a_fixture_window_holds_no_client_to_spend_with(cx: &mut gpui::TestAppContext) {
        let (_window, timeline) = fixture_window(cx, fixture_with(&["2", "1"], &[]));

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert!(
                    view.client.is_none(),
                    "a fixture must not be able to reach the API"
                );
            });
        });
    }

    /// #21: the fixture's held-back posts become the bar's count.
    #[gpui::test]
    fn a_fixtures_waiting_posts_fill_the_new_posts_buffer(cx: &mut gpui::TestAppContext) {
        let (_window, timeline) = fixture_window(cx, fixture_with(&["2", "1"], &["4", "3"]));

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert_eq!(view.pending.as_ref().map(|pending| pending.count), Some(2));
                assert_eq!(
                    shown_ids(view),
                    ["2", "1"],
                    "a poll's posts must not reach the screen on their own"
                );
            });
        });
    }

    /// #21: pressing "Show New Posts" is what puts them on screen.
    ///
    /// The gap this closes is a real one: the bar and its `cmd-shift-r`
    /// binding could not be exercised by hand from a session with no way
    /// to click a desktop window, so until #146 the whole click path was
    /// unverified. `dispatch_action` covers it from `on_action` down — it
    /// goes through the same registration a keystroke does. The step above
    /// that, whether a coordinate lands on the bar, is #184's test below.
    #[gpui::test]
    fn showing_new_posts_moves_them_onto_the_timeline(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;

        let (window, timeline) = fixture_window(cx, fixture_with(&["2", "1"], &["4", "3"]));

        cx.update_window(window.into(), |_, window, cx| {
            let _ = window.draw(cx);
            window.dispatch_action(Box::new(crate::menu::ShowNewPosts), cx);
        })
        .unwrap();
        cx.run_until_parked();

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert_eq!(shown_ids(view), ["4", "3", "2", "1"]);
                assert!(
                    view.pending.is_none(),
                    "the buffer must be emptied, or the bar keeps offering posts already shown"
                );
            });
        });
    }

    /// #184: the same reveal, reached by a click on the bar itself.
    ///
    /// This is the layer above #183. The test above dispatches the action
    /// directly, which leaves one step unverified: whether a mouse at some
    /// coordinate lands on the bar at all. Here nothing is dispatched — the
    /// bar's own bounds are looked up from the frame that was just drawn,
    /// a click is simulated at their centre, and gpui's hit test is what
    /// has to find `on_click`. The assertions are deliberately identical
    /// to the dispatch test's, so a pass means the two paths agree.
    ///
    /// The coordinate is never written down. `render::Addressable` gives
    /// the bar one name, `debug_bounds` reads back where that name was
    /// actually laid out, and the click follows — so moving the bar in
    /// `render.rs` moves the click with it.
    #[gpui::test]
    fn clicking_the_new_posts_bar_moves_them_onto_the_timeline(cx: &mut gpui::TestAppContext) {
        let (window, timeline) = fixture_window(cx, fixture_with(&["2", "1"], &["4", "3"]));

        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let bar = visual
            .debug_bounds("new-posts")
            .expect("the bar has to be laid out before a click can reach it");
        visual.simulate_click(bar.center(), gpui::Modifiers::none());

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert_eq!(shown_ids(view), ["4", "3", "2", "1"]);
                assert!(
                    view.pending.is_none(),
                    "the buffer must be emptied, or the bar keeps offering posts already shown"
                );
            });
        });
    }

    /// #184: what makes the test above mean anything.
    ///
    /// A simulated click that reached `on_click` no matter where it
    /// landed would pass the previous test while proving nothing about
    /// the hit test. This clicks the middle of the timeline instead —
    /// below the bar and clear of every row, since a fixture's two posts
    /// sit at the top — and requires the buffer to still be waiting.
    /// Together the pair says the coordinate is what decides, which is
    /// the step #183's `dispatch_action` skips.
    ///
    /// The miss is addressed the same way the hit is, rather than by
    /// offsetting the bar's centre by some number of pixels: a literal
    /// offset is a coordinate written down, and it would start landing on
    /// the bar again the moment the window or the bar changed height.
    #[gpui::test]
    fn clicking_the_timeline_below_the_bar_reveals_nothing(cx: &mut gpui::TestAppContext) {
        let (window, timeline) = fixture_window(cx, fixture_with(&["2", "1"], &["4", "3"]));

        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let body = visual
            .debug_bounds("timeline")
            .expect("the timeline has to be laid out before a click can land in it");
        visual.simulate_click(body.center(), gpui::Modifiers::none());

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert_eq!(shown_ids(view), ["2", "1"]);
                assert_eq!(
                    view.pending.as_ref().map(|pending| pending.count),
                    Some(2),
                    "a click outside the bar must leave the offer standing"
                );
            });
        });
    }

    /// #182, retroactively: the status bar's two segments do not touch.
    ///
    /// This is the test #182 was merged without. `Total: 11 req` and
    /// `List sync: …` rendered as `11 reqList sync` — the row's `gap_3`
    /// does not separate two bare text spans, and raising it to `gap_8`
    /// changed nothing, so the fix was an explicit margin. A screenshot
    /// was the only way to see either the defect or the fix.
    ///
    /// It was not the only way. Layout runs under the test platform;
    /// what `TestWindow::draw` skips is turning a `Scene` into pixels.
    /// So the laid-out bounds are real, and a spacing this test can read
    /// is a spacing an assertion can hold (#184). Deliberately `>`, not a
    /// specific gap: the defect was the two boxes meeting, and pinning
    /// the exact margin would make every deliberate spacing change a test
    /// failure.
    #[gpui::test]
    fn the_status_bars_segments_keep_apart(cx: &mut gpui::TestAppContext) {
        let (window, _timeline) = fixture_window(cx, fixture_with(&["2", "1"], &[]));

        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let usage = visual
            .debug_bounds("status-usage")
            .expect("the request count is always shown");
        let sync = visual
            .debug_bounds("status-sync")
            .expect("the sync segment is always shown");

        assert!(
            sync.left() > usage.right(),
            "the two segments run together, which reads as `11 reqList sync` \
             on screen: usage ends at {:?}, sync starts at {:?}",
            usage.right(),
            sync.left()
        );
    }

    /// #21: pressing it again changes nothing.
    ///
    /// Asserts the timeline is *identical*, not merely that nothing
    /// crashed. `apply_pending` early-returns on an empty buffer today;
    /// the regression this guards is someone later making it set `state`
    /// unconditionally, which would blank the screen on a second press.
    #[gpui::test]
    fn showing_new_posts_with_none_waiting_leaves_the_timeline_alone(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::AppContext as _;

        let (window, timeline) = fixture_window(cx, fixture_with(&["2", "1"], &["3"]));

        for _ in 0..2 {
            cx.update_window(window.into(), |_, window, cx| {
                let _ = window.draw(cx);
                window.dispatch_action(Box::new(crate::menu::ShowNewPosts), cx);
            })
            .unwrap();
            cx.run_until_parked();
        }

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert_eq!(shown_ids(view), ["3", "2", "1"]);
            });
        });
    }

    /// #21: `cmd-shift-r` spends nothing.
    ///
    /// The pairing with `cmd-r` is the whole design — one buys a fetch,
    /// the other reveals one already paid for — and `menu.rs` says so in
    /// prose. This is the part of that claim a test can hold: after the
    /// dispatch there is still no client, and `last_reload_at` has not
    /// moved, so nothing went out and nothing was even attempted.
    #[gpui::test]
    fn showing_new_posts_sends_nothing(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;

        let (window, timeline) = fixture_window(cx, fixture_with(&["2", "1"], &["3"]));

        cx.update_window(window.into(), |_, window, cx| {
            let _ = window.draw(cx);
            window.dispatch_action(Box::new(crate::menu::ShowNewPosts), cx);
        })
        .unwrap();
        cx.run_until_parked();

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert!(view.client.is_none());
                assert!(
                    view.last_reload_at.is_none(),
                    "showing a buffered fetch must not count as one"
                );
            });
        });
    }

    // --- #164: the toolbar's list picker ---

    /// A fixture whose picker has `lists` to name, on top of `shown`.
    fn fixture_with_lists(shown: &[&str], lists: &[(&str, &str)]) -> Fixture {
        let mut fixture = fixture_with(shown, &[]);
        fixture.lists = lists
            .iter()
            .map(|(id, name)| crate::x_api::ListSummary {
                id: (*id).to_string(),
                name: (*name).to_string(),
            })
            .collect();
        fixture
    }

    /// Write `ids` as `list_id`'s cached timeline in the smoke directory,
    /// so a switch to it has something to render without a client.
    fn cache_list(list_id: &str, ids: &[&str]) {
        let paths = smoke_paths();
        paths.ensure_dirs().unwrap();
        let items: Vec<TimelineItem> = ids
            .iter()
            .map(|id| item_with(id, "someone", None))
            .collect();
        crate::cache::save_primary_timeline(
            &paths,
            &crate::cache::TimelineSource::List(list_id.to_string()),
            "5685672",
            &items,
            0,
        )
        .unwrap();
    }

    /// A drawn window and its visual context, for the click tests below.
    fn drawn(
        cx: &mut gpui::TestAppContext,
        fixture: Fixture,
    ) -> (gpui::VisualTestContext, gpui::Entity<super::TimelineView>) {
        let (window, timeline) = fixture_window(cx, fixture);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });
        (visual, timeline)
    }

    /// #164: every segment is laid out, Home first, and none of them
    /// overlap — the same claim `the_status_bars_segments_keep_apart`
    /// makes about the status bar, for the reason it gives.
    #[gpui::test]
    fn the_picker_lays_out_home_and_every_list_left_to_right(cx: &mut gpui::TestAppContext) {
        let (mut visual, _timeline) = drawn(
            cx,
            fixture_with_lists(&["1"], &[("9101", "Following mirror"), ("9102", "Rust")]),
        );

        let home = visual
            .debug_bounds("tab-home")
            .expect("Home is always a segment");
        let first = visual
            .debug_bounds("tab-list-9101")
            .expect("the first fixture list is a segment");
        let second = visual
            .debug_bounds("tab-list-9102")
            .expect("the second fixture list is a segment");
        assert!(first.left() >= home.right(), "{home:?} then {first:?}");
        assert!(second.left() >= first.right(), "{first:?} then {second:?}");
    }

    /// #164: a fixture window has no client, so it must not offer the one
    /// button in the toolbar that spends a request.
    #[gpui::test]
    fn a_fixture_window_offers_no_list_fetch(cx: &mut gpui::TestAppContext) {
        let (mut visual, _timeline) = drawn(cx, fixture_with_lists(&["1"], &[("9101", "Rust")]));
        assert!(
            visual.debug_bounds("load-lists").is_none(),
            "a window with no client must not offer to fetch lists"
        );
    }

    /// #164: the same window with a client *does* offer it, laid out to
    /// the right of the picker and inside the toolbar.
    ///
    /// The one place the button is ever drawn is a signed-in live window,
    /// which no test can build — so this hands a fixture window a client
    /// (a token string; `XClient::new` sends nothing) and redraws. Without
    /// it the button's first render is the user's first launch, which is
    /// how "the button is missing" got reported.
    #[gpui::test]
    fn a_signed_in_window_offers_the_list_fetch_beside_the_picker(cx: &mut gpui::TestAppContext) {
        let (mut visual, timeline) = drawn(cx, fixture_with_lists(&["1"], &[("9101", "Rust")]));
        cx.update(|cx| {
            timeline.update(cx, |view, cx| {
                view.client = Some(crate::x_api::XClient::new("token".to_string()));
                cx.notify();
            });
        });
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let button = visual
            .debug_bounds("load-lists")
            .expect("a window with a client and a known user offers the fetch");
        let last_segment = visual
            .debug_bounds("tab-list-9101")
            .expect("the fixture list is a segment");
        assert!(
            button.left() >= last_segment.right(),
            "the button sits after the picker: {last_segment:?} then {button:?}"
        );
        assert!(
            button.size.width > gpui::px(0.0) && button.size.height > gpui::px(0.0),
            "the button has a size: {button:?}"
        );
    }

    /// #164, the issue's second completion criterion: switching between
    /// timelines that are already cached sends nothing.
    ///
    /// Two lists, both cached ahead of time; the window is clicked from
    /// one to the other and back, and after each click it shows exactly
    /// the cached rows. There is still no client and `last_reload_at` has
    /// not moved, so nothing went out and nothing was attempted — the
    /// same evidence `showing_new_posts_sends_nothing` relies on.
    #[gpui::test]
    fn switching_between_cached_sources_sends_nothing(cx: &mut gpui::TestAppContext) {
        cache_list("9111", &["12", "11"]);
        cache_list("9112", &["22", "21"]);
        let (mut visual, timeline) = drawn(
            cx,
            fixture_with_lists(&["1"], &[("9111", "first"), ("9112", "second")]),
        );

        for (segment, expected) in [
            ("tab-list-9111", ["12", "11"]),
            ("tab-list-9112", ["22", "21"]),
            ("tab-list-9111", ["12", "11"]),
        ] {
            let bounds = visual
                .debug_bounds(segment)
                .expect("the segment has to be laid out before a click can reach it");
            visual.simulate_click(bounds.center(), gpui::Modifiers::none());
            cx.run_until_parked();

            cx.update(|cx| {
                timeline.update(cx, |view, _cx| {
                    assert_eq!(shown_ids(view), expected, "after clicking {segment}");
                    assert!(view.client.is_none());
                    assert!(
                        view.last_reload_at.is_none(),
                        "a switch to a cached list must not count as a fetch"
                    );
                });
            });
            // Redraw so the next lookup sees the segment lifted where it
            // now is, not where the previous frame put it.
            visual.update(|window, cx| {
                let _ = window.draw(cx);
            });
        }
    }

    /// #164: the click lands on the segment, and the switch resets what
    /// belonged to the previous source — here, the poll buffer, which
    /// would otherwise offer the old list's posts over the new one.
    #[gpui::test]
    fn clicking_a_segment_switches_the_source_and_drops_the_old_buffer(
        cx: &mut gpui::TestAppContext,
    ) {
        cache_list("9121", &["32", "31"]);
        let mut fixture = fixture_with_lists(&["2", "1"], &[("9121", "Rust")]);
        fixture.pending = vec![item_with("3", "someone", None)];
        let (mut visual, timeline) = drawn(cx, fixture);

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert_eq!(view.source, crate::cache::TimelineSource::Home);
                assert!(view.pending.is_some(), "the fixture's buffer is waiting");
            });
        });

        let segment = visual
            .debug_bounds("tab-list-9121")
            .expect("the segment has to be laid out before a click can reach it");
        visual.simulate_click(segment.center(), gpui::Modifiers::none());
        cx.run_until_parked();

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert_eq!(
                    view.source,
                    crate::cache::TimelineSource::List("9121".to_string())
                );
                assert_eq!(shown_ids(view), ["32", "31"]);
                assert!(
                    view.pending.is_none(),
                    "a buffer polled against Home must not be offered over a list"
                );
                assert!(view.next_page_token.is_none());
            });
        });
    }

    /// #164: the choice outlives the window — it is on disk the moment
    /// the segment is clicked, not at some later save point that a crash
    /// could skip.
    ///
    /// A *live* window, under its own directory: a fixture never writes
    /// the file (the test after this one), and the smoke directory is
    /// shared with every other window test, so a file asserted on there
    /// would be raced by whichever test clicked last.
    #[gpui::test]
    fn a_switch_is_remembered_on_disk_at_once(cx: &mut gpui::TestAppContext) {
        let home = std::env::temp_dir().join("twigpui-smoke-live-switch");
        let _ = std::fs::remove_dir_all(&home);
        let home_str = home.display().to_string();
        let paths =
            crate::paths::Paths::from_vars(move |key| (key == "HOME").then(|| home_str.clone()))
                .unwrap();
        paths.ensure_dirs().unwrap();
        crate::cache::save_owned_lists(
            &paths,
            &[crate::x_api::ListSummary {
                id: "9131".to_string(),
                name: "Rust".to_string(),
            }],
            0,
        )
        .unwrap();

        // No token under this HOME, so startup settles at
        // `NotAuthenticated` with no client — past the startup gate, and
        // still unable to spend anything on the cache miss that follows.
        let (window, timeline) = window_with(cx, paths.clone(), Startup::Live);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert!(matches!(view.state, TimelineState::NotAuthenticated));
                assert!(view.client.is_none());
            });
        });

        let segment = visual
            .debug_bounds("tab-list-9131")
            .expect("the segment has to be laid out before a click can reach it");
        visual.simulate_click(segment.center(), gpui::Modifiers::none());
        cx.run_until_parked();

        let remembered = super::list_picker::load_selection(&paths.selection_file());
        assert_eq!(
            remembered.selected,
            Some(super::list_picker::Selection::List {
                id: "9131".to_string()
            })
        );
        std::fs::remove_dir_all(&home).unwrap();
    }

    /// #164: a fixture's segments name lists that do not exist, so a click
    /// on one must leave no file behind — or the next live launch would
    /// open on a list it cannot read and pay for the reload that finds
    /// out.
    #[gpui::test]
    fn a_fixture_switch_leaves_no_selection_behind(cx: &mut gpui::TestAppContext) {
        cache_list("9151", &["51"]);
        let selection_file = smoke_paths().selection_file();
        let _ = std::fs::remove_file(&selection_file);
        let (mut visual, timeline) = drawn(cx, fixture_with_lists(&["1"], &[("9151", "Rust")]));

        let segment = visual
            .debug_bounds("tab-list-9151")
            .expect("the segment has to be laid out before a click can reach it");
        visual.simulate_click(segment.center(), gpui::Modifiers::none());
        cx.run_until_parked();

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert_eq!(shown_ids(view), ["51"], "the switch itself still happens");
            });
        });
        assert!(
            !selection_file.exists(),
            "a fixture window wrote {}",
            selection_file.display()
        );
    }

    /// #164: clicking the segment that is already lifted is a no-op — the
    /// timeline is identical afterwards, not merely still loaded.
    #[gpui::test]
    fn clicking_the_showing_segment_changes_nothing(cx: &mut gpui::TestAppContext) {
        let (mut visual, timeline) =
            drawn(cx, fixture_with_lists(&["2", "1"], &[("9141", "Rust")]));

        let home = visual
            .debug_bounds("tab-home")
            .expect("Home is always a segment");
        visual.simulate_click(home.center(), gpui::Modifiers::none());
        cx.run_until_parked();

        cx.update(|cx| {
            timeline.update(cx, |view, _cx| {
                assert_eq!(view.source, crate::cache::TimelineSource::Home);
                assert_eq!(shown_ids(view), ["2", "1"]);
            });
        });
    }

    /// #174: the sync segment cannot be armed while the sync is stopped.
    ///
    /// A money guard, not a tidiness one. `ask_to_sync` is the first of
    /// the two clicks that spend a full read of both the follow list and
    /// the list membership; a fixture window is stopped at
    /// `SyncOff::NotSignedIn`, and arming there would put a "Sync anyway?"
    /// button on a window that has no credential to sync with.
    #[gpui::test]
    fn a_stopped_sync_cannot_be_armed(cx: &mut gpui::TestAppContext) {
        let (_window, timeline) = fixture_window(cx, fixture_with(&["1"], &[]));

        cx.update(|cx| {
            timeline.update(cx, |view, cx| {
                assert!(matches!(view.sync_status, SyncStatus::Off(_)));
                view.ask_to_sync(cx);
                assert!(
                    !view.pending_sync,
                    "a window with no credential must not offer to spend a sync"
                );
            });
        });
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
            list_id: None,
            // Off in the smoke tests: they render a window, and a paid
            // background loop is not part of what they are checking.
            auto_sync_list: false,
            sync_interval_seconds: 21_600,
            // Off for the same reason (#21).
            auto_refresh: false,
            auto_refresh_interval_seconds: 300,
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
                    let mut view =
                        super::TimelineView::new(config, paths, Startup::Live, window, cx);
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
