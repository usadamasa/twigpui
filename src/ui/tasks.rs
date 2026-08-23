//! The side of `TimelineView` that spends: every method that holds a
//! `cx.spawn` and reaches the network or the disk (#137).
//!
//! # The decision this file is
//!
//! #137 asked whether to split `impl TimelineView` and, if so, where. The
//! options it listed were (a) the rendering methods, (b) the async task
//! methods, (c) the composer's input handling, or (d) keep raising the
//! ceiling. This is (b).
//!
//! (a) is the larger pile, and that is the argument against it: "the
//! methods that build elements" is most of a UI file by volume and says
//! nothing about what the seam is *for*. The seam here is one anybody can
//! state in a sentence — **this file is where the money goes** — and it is
//! not a new one. [`super::reload_policy`] already holds the decisions
//! about whether a request may go out; this is where the ones that
//! survived that decision are actually sent. The pair reads as one idea
//! split across a judgement and its execution.
//!
//! It is also the pile with no tests to move. Nothing here is unit
//! tested, for `sync/run.rs`'s reason: every branch is an HTTP request or
//! a file a request's result is written to. `ui/mod.rs`'s test module
//! stayed exactly where it was, which is how "this was a pure move" was
//! checked — the same evidence #126 used.
//!
//! # What this is not
//!
//! Not the pattern [`super::auto_refresh`] and [`super::list_sync`] use.
//! Those two group one *feature* each — a timer, its state, and the
//! button that drives it — and each was written that way from the start
//! rather than carved out afterwards. This file groups one *kind*, and
//! the two conventions are meant to coexist: a new mechanism that is
//! wholly its own gets its own file, and the spending half of the
//! window's existing behaviour lives here.

// `super::*` rather than a list, matching [`super::render`] and
// [`super::auto_refresh`]: this is the largest of `ui`'s children and
// reaches almost everything the parent imports.
use super::*;

impl TimelineView {
    /// Resolve a credential (stored OAuth session, refreshing if stale, else
    /// the bearer token) before the very first fetch, and — since #9 — try
    /// to render straight from the local cache instead of always reloading.
    /// A cache hit means startup spends no API request at all; a miss falls
    /// through to [`Self::reload`], which does. Runs on the background
    /// executor because it can touch disk and, on a token refresh or cache
    /// miss, the network.
    pub(super) fn start(&mut self, cx: &mut Context<'_, Self>) {
        self.state = TimelineState::Loading;

        let config = self.config.clone();
        let paths = self.paths.clone();
        let source = self.source.clone();

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
                    // #161: which timeline this is comes from
                    // `config.list_id`, resolved into `self.source` at
                    // construction. #33 had removed the branch entirely
                    // (the app-only bearer token, the one thing that used
                    // to decide it, was gone); #157 put one back, because
                    // the home timeline stopped carrying followed authors'
                    // posts and a List is the way to read them now.
                    let cached = cache::startup_primary(&paths, &source, oauth::unix_now())?;
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
                        // After `client` and `oauth_scope`, which it gates
                        // on and borrows from; before the fetch below,
                        // which it does not depend on either way.
                        this.start_sync(SyncTrigger::Scheduled, cx);
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
                        // #21: after the `cached` match, never before it.
                        // The miss arm calls `reload`, which sets
                        // `last_reload_at` — the anchor the first poll is
                        // measured from. Started first, the loop would
                        // anchor on the window opening instead and buy a
                        // poll an interval after a fetch that had only
                        // just landed.
                        this.start_auto_refresh(cx);
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
    pub(super) fn reload(&mut self, trigger: ReloadTrigger, cx: &mut Context<'_, Self>) {
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
        let source = self.source.clone();

        // #161: `source` decides which endpoint this spends its request on
        // and which cache file the result lands in. The single-user
        // endpoint and its cache stay out of it, for `--fetch-only`.
        self.fetch = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    cache::reload_primary(&paths, &client, &source, max_results, oauth::unix_now())
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
                        // #141: worked out before `state` is replaced,
                        // for the same reason the scroll target is — it
                        // takes both lists.
                        let outcome = this.reload_outcome(&reloaded.items);
                        this.state = TimelineState::Loaded(reloaded.items);
                        this.reload_notice = Some(ReloadNotice::Outcome(outcome.into()));
                        // Same reasoning as the single-user branch above.
                        this.cooldown_ticker = None;
                        // #21: this fetch is strictly fresher than
                        // whatever a poll buffered, and it has already put
                        // the new posts on screen — so the pill would be
                        // offering posts that are visible behind it.
                        this.clear_pending();
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

    /// What the finished reload should say for itself (#141).
    ///
    /// Called with the incoming list before `state` is replaced, like
    /// [`Self::keep_the_reader_in_place`] and for the same reason: the
    /// count is the difference between the two lists.
    ///
    /// A first load has no previous list to compare against, so everything
    /// in it counts as new — which is what it is.
    fn reload_outcome(&self, incoming: &[TimelineItem]) -> String {
        let previous: Vec<&str> = match &self.state {
            TimelineState::Loaded(items) => items.iter().map(|item| item.id.as_str()).collect(),
            _ => Vec::new(),
        };
        let new_ids: Vec<&str> = incoming.iter().map(|item| item.id.as_str()).collect();
        reload_outcome_label(newly_arrived(&previous, &new_ids))
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
    pub(super) fn load_older(&mut self, cx: &mut Context<'_, Self>) {
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
        let source = self.source.clone();

        self.fetch = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    cache::load_older_primary(
                        &paths,
                        &client,
                        &source,
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
                        // #21: a buffer fetched before this page was
                        // appended does not contain it, so applying one
                        // afterwards would silently undo the click.
                        this.clear_pending();
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
    pub(super) fn show_thread(
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
    pub(super) fn refresh_usage(&mut self, cx: &mut Context<'_, Self>) {
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
    pub(super) fn toggle_like(&mut self, post_id: String, cx: &mut Context<'_, Self>) {
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
    pub(super) fn refresh_images(&mut self, cx: &mut Context<'_, Self>) {
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
    pub(super) fn toggle_repost(&mut self, post_id: String, cx: &mut Context<'_, Self>) {
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
    pub(super) fn confirm_delete(&mut self, post_id: String, cx: &mut Context<'_, Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(user_id) = self.home_user_id.clone() else {
            return;
        };
        // #161: the cache file a delete has to be removed from is
        // whichever one the window is rendering.
        let source = self.source.clone();

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
                    cache::forget_post(&paths, &source, &user_id, &request_id, oauth::unix_now())
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                this.refresh_usage(cx);
                match result {
                    Ok(remaining) => {
                        this.delete_failures.remove(&post_id);
                        this.state = TimelineState::Loaded(remaining);
                        // #21: a buffer fetched before the delete still
                        // holds the deleted post. Applying one afterwards
                        // would put it back on screen — the exact failure
                        // #72 rewrites the cache file to prevent.
                        this.clear_pending();
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

    /// Hand `url` to the system browser (#70).
    ///
    /// Runs on the background executor rather than in the click handler:
    /// spawning a process is a syscall the UI thread has no reason to wait
    /// on. A refusal or a failure to launch is reported through
    /// `open_failure`, which the row renders — a click that silently does
    /// nothing is the one outcome worth avoiding here.
    ///
    /// The one method here that spends no API credits. It is here anyway
    /// because the seam is "holds a `cx.spawn` and reaches outside the
    /// process", and a browser launch is that — leaving it behind in `ui`
    /// would make the boundary something nobody could restate.
    pub(super) fn open_in_browser(&mut self, url: String, cx: &mut Context<'_, Self>) {
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

    /// Run the interactive PKCE sign-in flow: open the browser, wait for the
    /// loopback callback, exchange the code, persist the tokens, then fall
    /// straight into [`Self::reload`].
    pub(super) fn sign_in(&mut self, cx: &mut Context<'_, Self>) {
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
                    // The other place the sync can start. This is the path
                    // "Re-authorize" takes, and it is the one that matters
                    // for a session that was refused for a missing scope:
                    // the scope it was missing has just been granted, and
                    // without this the sync would stay off until the app
                    // was restarted.
                    this.start_sync(SyncTrigger::Scheduled, cx);
                    // #21: the other place auto-refresh can start, for the
                    // same reason — until this point there was no client
                    // for a poll to fetch with. Started before the reload
                    // below, whose own `last_reload_at` is then what the
                    // first poll anchors on.
                    this.start_auto_refresh(cx);
                    // #21: a session change is a fresher source than
                    // anything a poll left buffered — see `clear_pending`.
                    this.clear_pending();
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
    pub(super) fn submit_post(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
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
}
