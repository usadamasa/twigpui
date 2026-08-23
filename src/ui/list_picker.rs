//! The toolbar's list picker (#164): which timeline the window shows, how
//! that choice survives a relaunch, and where the segments get their names.
//!
//! #161 made a List the window's primary source, but one List, decided at
//! startup from `config.list_id`. #95 had already drawn the segmented
//! control it would be switched from — with one segment and no click
//! handler, because there was nothing to switch *to*. This file supplies
//! the other segments and the click.
//!
//! Laid out like [`super::list_sync`]: pure functions with their tests
//! first, then an `impl TimelineView` block for the parts that touch the
//! window or spend a request.
//!
//! # Where the money is
//!
//! Switching spends nothing. The window renders whatever `source`'s cache
//! file holds, and only a list that has never been read — no cache file at
//! all — falls through to the ordinary reload, which is the same request a
//! first launch makes. Switching back and forth between lists already
//! read costs zero requests, however often; the issue's third completion
//! criterion is exactly that, and
//! `switching_between_cached_sources_sends_nothing` in `ui`'s tests holds
//! it.
//!
//! Naming the segments does cost one request: `GET /2/users/:id/owned_lists`
//! bills per list returned (`x-api-budget`). It is never sent on the
//! window's own initiative — the picker offers a button that says what it
//! costs, and the result is cached without a TTL, so the only way to spend
//! it again is to press the button again.
//!
//! # What wins at startup
//!
//! The saved selection beats `config.list_id`, and so beats `X_LIST_ID`
//! too. That is the reverse of the usual "environment over file" rule and
//! it is deliberate: the dev profile always has a default list
//! (`Profile::default_list_id`), so if the configuration won, a dev build
//! would snap back to that list on every launch and the picker's choice
//! would never outlive the window. The configuration is where the window
//! *starts* until someone picks; a pick is a later, more specific decision.

use std::path::Path;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

// Spelled out rather than `use super::*`, for [`super::list_sync`]'s reason.
use super::render::{Addressable as _, tab_segment, tab_trough};
use super::{
    AnyElement, Context, IntoElement as _, ParentElement as _, ReloadNotice, ReloadTrigger,
    Startup, StatefulInteractiveElement as _, Styled as _, TimelineState, TimelineView, div, log,
    oauth, rgb,
};
use crate::cache::{self, TimelineSource};
use crate::paths::Paths;
use crate::theme;
use crate::x_api::ListSummary;

/// What the picker remembers between launches: the timeline that was
/// showing when the window last switched.
///
/// Its own type rather than a `Serialize` on [`TimelineSource`], because
/// this one is written to disk and read back by a later build. The cache
/// module's history (#97, a schema version added and removed) is the
/// warning: an on-disk shape must not be whatever an internal enum's
/// derive happens to emit today. The `kind` tag keeps a third variant
/// from ever being mistaken for a list id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Selection {
    /// `GET /2/users/:id/timelines/reverse_chronological` — Home.
    Home,
    /// `GET /2/lists/:id/tweets` for this list.
    List {
        /// The list's id, bare digits.
        id: String,
    },
}

impl Selection {
    /// The selection that names `source`.
    pub(super) fn of(source: &TimelineSource) -> Self {
        match source {
            TimelineSource::Home => Self::Home,
            TimelineSource::List(id) => Self::List { id: id.clone() },
        }
    }

    /// The source this selection names, or `None` if the file's list id is
    /// nothing `Config::resolve` would have accepted — a hand-edited file
    /// should fall back, not build a request URL out of whatever it said.
    fn into_source(self) -> Option<TimelineSource> {
        match self {
            Self::Home => Some(TimelineSource::Home),
            Self::List { id } => (!id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
                .then_some(TimelineSource::List(id)),
        }
    }
}

/// The whole contents of [`Paths::selection_file`].
///
/// One field, and a struct anyway — [`crate::sync::SyncState`]'s reason:
/// the next thing worth remembering about the picker should not have to
/// change the file's shape to get in.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) struct SelectionState {
    /// The last timeline switched to, or `None` if the picker has never
    /// been used.
    #[serde(default)]
    pub selected: Option<Selection>,
}

/// Read the picker's saved choice back from `path`.
///
/// Infallible, like [`crate::sync::load_state`] and for the same reason:
/// losing this costs one click, so a missing or corrupt file is the
/// default rather than an error that stops the window opening.
pub(crate) fn load_selection(path: &Path) -> SelectionState {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return SelectionState::default();
    };
    serde_json::from_str(&contents).unwrap_or_default()
}

/// Write the picker's choice to `path`.
pub(crate) fn save_selection(path: &Path, state: &SelectionState) -> Result<()> {
    let json = serde_json::to_string_pretty(state).context("could not serialize the selection")?;
    std::fs::write(path, json).with_context(|| format!("could not write {}", path.display()))
}

/// Which timeline the window opens on (#161, #164).
///
/// The saved selection first, then `config.list_id`, then Home — see the
/// module doc for why the file beats the configuration. A saved list is
/// honored whether or not the account still owns it: any list id can be
/// read, and #161's configured list need not be an owned one either.
pub(super) fn initial_source(
    saved: Option<Selection>,
    configured_list_id: Option<&str>,
) -> TimelineSource {
    if let Some(source) = saved.and_then(Selection::into_source) {
        return source;
    }
    match configured_list_id {
        Some(list_id) => TimelineSource::List(list_id.to_string()),
        None => TimelineSource::Home,
    }
}

/// The saved selection a window starting `startup` should honor: the file's
/// for a live window, none for a fixture. A fixture is the same screen
/// every time by definition (`fixture-visual-check`), and a state file
/// left behind by the last live run must not be able to change which
/// segment it draws lifted. The write side is gated the same way, by
/// `TimelineView::selection_file` being `None`.
pub(super) fn saved_selection_for(startup: &Startup, paths: &Paths) -> Option<Selection> {
    match startup {
        Startup::Live => load_selection(&paths.selection_file()).selected,
        Startup::Fixture(_) => None,
    }
}

/// The lists the picker last fetched, or none if it never has — and none,
/// with a log line, if the cache file could not be read. A picker that
/// cannot name any list still has a Home segment and a button to fetch
/// the rest, so an unreadable cache is not worth failing the window over.
pub(super) fn cached_lists_or_empty(paths: &Paths) -> Vec<ListSummary> {
    match cache::cached_owned_lists(paths) {
        Ok(lists) => lists.unwrap_or_default(),
        Err(error) => {
            log::warn(&format!("could not read the cached lists: {error:#}"));
            Vec::new()
        }
    }
}

/// One segment of the picker: what it is called, what it switches to, and
/// whether it is the one showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Segment {
    /// The element name a window test can find the segment by
    /// (`render::Addressable`).
    pub name: String,
    /// What the segment says.
    pub label: String,
    /// What clicking it switches the window to.
    pub source: TimelineSource,
    /// Whether this is the timeline currently showing.
    pub selected: bool,
}

/// The picker's segments, in drawing order: Home, then every owned list as
/// the API ordered them, then — only if the window is showing a list it
/// does not own, #161's configured list being the usual case — that list,
/// so the selected segment always exists to be lifted out of the trough.
pub(super) fn segments(current: &TimelineSource, owned: &[ListSummary]) -> Vec<Segment> {
    let mut segments = vec![segment(TimelineSource::Home, "Home".to_string(), current)];
    for list in owned {
        segments.push(segment(
            TimelineSource::List(list.id.clone()),
            segment_label(list),
            current,
        ));
    }
    if let TimelineSource::List(id) = current
        && !owned.iter().any(|list| &list.id == id)
    {
        segments.push(segment(current.clone(), "List".to_string(), current));
    }
    segments
}

fn segment(source: TimelineSource, label: String, current: &TimelineSource) -> Segment {
    Segment {
        name: segment_name(&source),
        label,
        selected: &source == current,
        source,
    }
}

/// The element name of the segment that switches to `source`.
pub(super) fn segment_name(source: &TimelineSource) -> String {
    match source {
        TimelineSource::Home => "tab-home".to_string(),
        TimelineSource::List(id) => format!("tab-list-{id}"),
    }
}

/// What a list's segment says: its name, or its id when the API sent none
/// (`ListSummary::name`'s doc has why that is tolerated at all). An id is
/// an ugly label but a usable one; a blank segment is neither.
pub(super) fn segment_label(list: &ListSummary) -> String {
    if list.name.trim().is_empty() {
        list.id.clone()
    } else {
        list.name.clone()
    }
}

/// Whether the toolbar offers the button that fetches the lists: only
/// with a client to spend through and an id to ask about. A fixture
/// window has neither, which is what keeps a fixture free of charge.
pub(super) fn offers_list_fetch(has_client: bool, user_known: bool) -> bool {
    has_client && user_known
}

/// The fetch button's text. It names its price in every resting state,
/// per `x-api-budget`'s rule for clicks that send requests.
pub(super) fn lists_button_label(has_lists: bool, fetching: bool) -> &'static str {
    if fetching {
        "Loading lists…"
    } else if has_lists {
        "Refresh lists (1 request)"
    } else {
        "Load lists (1 request)"
    }
}

/// Whether a switch has to wait for startup to finish.
///
/// `start` reads the cache for the source it was given and then puts the
/// result on screen; a switch in the middle of that would have the
/// startup's rows appear under the new segment. The tell is `Loading`
/// with no client yet — once either changes, startup has settled.
pub(super) fn switch_waits_for_startup(state: &TimelineState, has_client: bool) -> bool {
    matches!(state, TimelineState::Loading) && !has_client
}

impl TimelineView {
    /// The toolbar's segmented control (#95), now with every segment
    /// clickable (#164).
    pub(super) fn list_picker(&self, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = self.theme;
        let mut trough = tab_trough(theme);
        for segment in segments(&self.source, &self.owned_lists) {
            let source = segment.source;
            trough = trough.child(
                tab_segment(&segment.label, segment.selected, theme)
                    .addressable(segment.name)
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.switch_source(source.clone(), cx);
                    })),
            );
        }
        trough.into_any_element()
    }

    /// The button that fetches the lists' names (#164), or `None` when
    /// there is nothing to fetch them with — see [`offers_list_fetch`].
    /// Plain text while a fetch is in flight: a second click would only
    /// buy the same page twice.
    pub(super) fn lists_control(&self, cx: &mut Context<'_, Self>) -> Option<AnyElement> {
        if !offers_list_fetch(self.client.is_some(), self.home_user_id.is_some()) {
            return None;
        }
        let fetching = self.lists_fetch.is_some();
        let theme = self.theme;
        let control = div()
            .text_size(theme::TEXT_META)
            .text_color(rgb(theme.text_muted))
            .child(lists_button_label(!self.owned_lists.is_empty(), fetching));
        if fetching {
            return Some(control.into_any_element());
        }
        Some(
            control
                .addressable("load-lists")
                .on_click(cx.listener(|this, _event, _window, cx| this.fetch_owned_lists(cx)))
                .into_any_element(),
        )
    }

    /// Show `source` instead of whatever is showing (#164).
    ///
    /// Everything that belonged to the previous source goes with it: the
    /// in-flight reload or "Load older" (its result would land under the
    /// wrong segment), the pagination cursor (a Home cursor means nothing
    /// to a list endpoint), the poll buffer (`clear_pending`'s doc), open
    /// threads, and the scroll position. Then the new source's cache is
    /// put on screen, and only if there is none does this spend the same
    /// reload a first launch would.
    ///
    /// The auto-refresh loop is restarted rather than left running: it
    /// captured the old source when it started, and would keep polling it
    /// — writing the old list's posts over the new one's screen.
    pub(super) fn switch_source(&mut self, source: TimelineSource, cx: &mut Context<'_, Self>) {
        if self.source == source || switch_waits_for_startup(&self.state, self.client.is_some()) {
            return;
        }
        self.source = source;
        self.fetch = None;
        self.reloading = false;
        self.next_page_token = None;
        self.reload_notice = None;
        self.clear_pending();
        self.threads.clear();
        self.thread_fetches.clear();
        self.list_scroll.scroll_to_top_of_item(0);

        // Gated the same way the read side is: a fixture's segments name
        // lists that do not exist, and remembering one would send the
        // next live launch to reload a 404.
        if let Some(selection_file) = &self.selection_file {
            let remembered = SelectionState {
                selected: Some(Selection::of(&self.source)),
            };
            if let Err(error) = save_selection(selection_file, &remembered) {
                log::warn(&format!(
                    "could not remember the selected timeline: {error:#}"
                ));
            }
        }

        let cached = self.home_user_id.as_deref().and_then(|user_id| {
            cache::load_primary_timeline(&self.paths, &self.source, user_id).unwrap_or_else(
                |error| {
                    log::warn(&format!("could not read the cached timeline: {error:#}"));
                    None
                },
            )
        });
        match cached {
            Some(items) => self.state = TimelineState::Loaded(items),
            None => self.reload(ReloadTrigger::UserAction, cx),
        }
        self.start_auto_refresh(cx);
        // After `state` is replaced, for `start`'s reason (#120).
        self.refresh_images(cx);
        cx.notify();
    }

    /// Spend the one request that names the lists (#164), and cache what
    /// comes back. Refuses while one is already in flight.
    pub(super) fn fetch_owned_lists(&mut self, cx: &mut Context<'_, Self>) {
        if self.lists_fetch.is_some() {
            return;
        }
        let (Some(client), Some(user_id)) = (self.client.clone(), self.home_user_id.clone()) else {
            return;
        };
        let paths = self.paths.clone();

        self.lists_fetch = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let now = oauth::unix_now();
                    let (lists, next_token) = client.owned_lists(&paths, &user_id, None, now)?;
                    if next_token.is_some() {
                        // One page is the picker's whole vocabulary — see
                        // `XClient::owned_lists`. Said out loud rather than
                        // silently truncated.
                        log::warn("the account owns more lists than one page holds; the picker shows the first 100");
                    }
                    cache::save_owned_lists(&paths, &lists, now)?;
                    anyhow::Ok(lists)
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                this.lists_fetch = None;
                this.refresh_usage(cx);
                match result {
                    Ok(lists) => this.owned_lists = lists,
                    Err(error) => {
                        log::error(&format!("could not load the owned lists: {error:#}"));
                        this.reload_notice = Some(ReloadNotice::Failed(
                            format!("Could not load lists: {error:#}").into(),
                        ));
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(id: &str, name: &str) -> ListSummary {
        ListSummary {
            id: id.to_string(),
            name: name.to_string(),
        }
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("twigpui-selection-{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("selection.json")
    }

    // --- the saved selection ---

    #[test]
    fn a_selection_round_trips_through_its_file() {
        let path = scratch("roundtrip");
        let state = SelectionState {
            selected: Some(Selection::List {
                id: "2091351590695588200".to_string(),
            }),
        };
        save_selection(&path, &state).unwrap();
        assert_eq!(load_selection(&path), state);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn the_file_names_its_kind_rather_than_leaking_an_enum_shape() {
        // The on-disk shape is a contract with later builds, so it is
        // pinned here rather than left to whatever the derive emits.
        let path = scratch("shape");
        save_selection(
            &path,
            &SelectionState {
                selected: Some(Selection::Home),
            },
        )
        .unwrap();
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["selected"]["kind"], "home");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_missing_selection_file_is_the_default() {
        let path = scratch("missing");
        let _ = std::fs::remove_file(&path);
        assert_eq!(load_selection(&path), SelectionState::default());
    }

    #[test]
    fn a_corrupt_selection_file_is_the_default() {
        // Losing the choice costs one click; failing the window over it
        // would cost more.
        let path = scratch("corrupt");
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(load_selection(&path), SelectionState::default());
        std::fs::remove_file(&path).unwrap();
    }

    // --- which timeline the window opens on ---

    #[test]
    fn nothing_configured_and_nothing_saved_reads_the_home_timeline() {
        assert_eq!(initial_source(None, None), TimelineSource::Home);
    }

    #[test]
    fn a_configured_list_replaces_the_home_timeline() {
        // Replaces, not supplements (#161): #157 left nothing in the home
        // timeline worth falling back to.
        assert_eq!(
            initial_source(None, Some("2091351590695588200")),
            TimelineSource::List("2091351590695588200".to_string())
        );
    }

    #[test]
    fn a_saved_selection_beats_the_configured_list() {
        // The dev profile always configures a list; if that won, a dev
        // build would forget the picker's choice on every launch.
        assert_eq!(
            initial_source(
                Some(Selection::List {
                    id: "7".to_string()
                }),
                Some("2091351590695588200")
            ),
            TimelineSource::List("7".to_string())
        );
        assert_eq!(
            initial_source(Some(Selection::Home), Some("2091351590695588200")),
            TimelineSource::Home
        );
    }

    #[test]
    fn a_saved_list_id_that_is_not_digits_falls_back_to_the_configuration() {
        // The same rule `Config::resolve` applies to `list_id`, applied to
        // a file someone can edit by hand.
        assert_eq!(
            initial_source(
                Some(Selection::List {
                    id: "not-a-list".to_string()
                }),
                Some("2091351590695588200")
            ),
            TimelineSource::List("2091351590695588200".to_string())
        );
        assert_eq!(
            initial_source(Some(Selection::List { id: String::new() }), None),
            TimelineSource::Home
        );
    }

    #[test]
    fn a_selection_names_the_source_it_was_taken_from() {
        assert_eq!(Selection::of(&TimelineSource::Home), Selection::Home);
        assert_eq!(
            Selection::of(&TimelineSource::List("7".to_string())),
            Selection::List {
                id: "7".to_string()
            }
        );
    }

    // --- the segments ---

    #[test]
    fn home_comes_first_then_the_owned_lists_in_api_order() {
        let current = TimelineSource::List("2".to_string());
        let segments = segments(&current, &[list("2", "second"), list("1", "first")]);
        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.label.as_str())
                .collect::<Vec<_>>(),
            vec!["Home", "second", "first"]
        );
        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.selected)
                .collect::<Vec<_>>(),
            vec![false, true, false]
        );
        assert_eq!(segments[1].name, "tab-list-2");
        assert_eq!(segments[0].name, "tab-home");
        assert_eq!(segments[0].source, TimelineSource::Home);
    }

    #[test]
    fn a_list_the_account_does_not_own_still_gets_a_segment_while_showing() {
        // #161's configured list need not be an owned one; without this
        // the trough would have no lifted segment at all.
        let current = TimelineSource::List("9".to_string());
        let segments = segments(&current, &[list("1", "mine")]);
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[2].label, "List");
        assert!(segments[2].selected);
        assert_eq!(segments[2].source, current);
    }

    #[test]
    fn with_no_lists_cached_the_picker_is_just_home() {
        let segments = segments(&TimelineSource::Home, &[]);
        assert_eq!(segments.len(), 1);
        assert!(segments[0].selected);
    }

    #[test]
    fn a_nameless_list_is_labelled_by_its_id() {
        assert_eq!(segment_label(&list("7", "")), "7");
        assert_eq!(segment_label(&list("7", "   ")), "7");
        assert_eq!(segment_label(&list("7", "rust")), "rust");
    }

    // --- the fetch button ---

    #[test]
    fn the_fetch_is_offered_only_with_a_client_and_a_known_user() {
        assert!(offers_list_fetch(true, true));
        assert!(!offers_list_fetch(false, true), "a fixture window");
        assert!(!offers_list_fetch(true, false), "before /me has resolved");
    }

    #[test]
    fn the_fetch_button_names_its_price() {
        assert_eq!(lists_button_label(false, false), "Load lists (1 request)");
        assert_eq!(lists_button_label(true, false), "Refresh lists (1 request)");
        assert_eq!(lists_button_label(true, true), "Loading lists…");
    }

    #[test]
    fn a_fixture_window_ignores_the_saved_selection() {
        // The same screen every time, whatever the last live run left in
        // the state directory.
        let home = std::env::temp_dir().join("twigpui-selection-fixture");
        let home_str = home.display().to_string();
        let paths = Paths::from_vars(move |key| (key == "HOME").then(|| home_str.clone())).unwrap();
        paths.ensure_dirs().unwrap();
        save_selection(
            &paths.selection_file(),
            &SelectionState {
                selected: Some(Selection::Home),
            },
        )
        .unwrap();

        assert_eq!(
            saved_selection_for(&Startup::Live, &paths),
            Some(Selection::Home)
        );
        let fixture = crate::fixture::Fixture {
            signed_in_as: crate::fixture::FixtureUser {
                id: "1".to_string(),
                username: "a".to_string(),
            },
            items: Vec::new(),
            pending: Vec::new(),
            lists: Vec::new(),
        };
        assert_eq!(
            saved_selection_for(&Startup::Fixture(Box::new(fixture)), &paths),
            None
        );
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn a_switch_waits_only_while_startup_has_neither_client_nor_screen() {
        assert!(switch_waits_for_startup(&TimelineState::Loading, false));
        assert!(!switch_waits_for_startup(&TimelineState::Loading, true));
        assert!(!switch_waits_for_startup(
            &TimelineState::Loaded(Vec::new()),
            false
        ));
        assert!(!switch_waits_for_startup(
            &TimelineState::NotAuthenticated,
            false
        ));
    }
}
