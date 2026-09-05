//! ウィンドウの最初の一画面 (#146, #241)｡[`Startup`] がデータの出どころを
//! 決め､[`TimelineView::new`] がそれを受けて view を組む｡fixture だけを
//! 描く経路 (`show_fixture`) もここ｡
//!
//! `ui/mod.rs` にあったものをそのまま移した｡

use gpui::WindowBackgroundAppearance;

use super::lane;
use super::*;

/// ウィンドウの最初の一画面がどこから来るか (#146)｡
///
/// この enum が引く継ぎ目こそが要点である: これができるまで
/// [`TimelineView::new`] はいつも [`TimelineView::start`] へ直行していて､
/// そこで credential を解決し､レスポンスの cache を読み､空で返って
/// きたら fetch していた｡view へ timeline を渡す方法は無かった — view が
/// 自分で取りに行っていた｡
///
/// そこで今は `main` がデータの出どころを決め､view は渡されたものを
/// 描く｡これがアカウント無しで画面を再現可能にしている
/// ものである｡
#[derive(Debug)]
pub(crate) enum Startup {
    /// credential を解決し､cache を読み､ファイルに何も無ければ fetch する｡
    /// #146 より前はすべての起動がこうしていたし､普通の起動は今もこう
    /// している｡
    Live,
    /// この post を描いて終わる｡
    ///
    /// **このモードでは `XClient` を一切組み立てない**｡これは慣習ではなく､
    /// fixture が何の課金も起こしえない理由そのものである: この view の
    /// 課金される経路はすべて `self.client` の向こうにあり､そこには
    /// 届く先が何も無い｡
    Fixture(Box<Fixture>),
}

impl Startup {
    /// この起動の window が､画面がロックされていても (occluded でも) 描き
    /// 続けるべきかどうか — fork した gpui の patch (Cargo.toml の
    /// `[patch.crates-io]`) が読むスイッチの値｡
    ///
    /// fixture の window は撮られるためにある｡upstream の gpui はロック中に
    /// 開いた window を 1 フレームも描かず､capture が真っ黒になる｡live の
    /// window は入れない: 隠れているあいだ描画を止める upstream の挙動は
    /// 本番にとって正しい｡
    ///
    /// `main` が `open_window` の **前** に `gpui::set_draw_while_occluded`
    /// へ渡す｡gpui は platform の window を作ってから root view を組むので､
    /// view の構築中に入れたのでは window 生成時の判定に間に合わない｡
    pub(crate) fn draws_while_occluded(&self) -> bool {
        matches!(self, Self::Fixture(_))
    }
}

/// この起動が持って立ち上がる window state (#267)｡
///
/// fixture は state ファイルを読まない (`window_state_file` が `None`) ので､
/// 透過は fixture 自身の宣言で決まる｡`TimelineView::new` はこの直後に
/// `apply_translucency` で platform の window へ効かせるし､`show_fixture` は
/// `window` を受け取らないので､あちらで立てても届かない｡
fn startup_window_state(
    startup: &Startup,
    file: Option<&std::path::Path>,
) -> window_state::WindowState {
    let mut state = window_state::load_or_default(file);
    if let Startup::Fixture(fixture) = startup {
        state.translucent = fixture.translucent;
    }
    state
}

impl TimelineView {
    pub(crate) fn new(
        config: Config,
        paths: Paths,
        startup: Startup,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        // 毎回の render ではなく､ここで一度だけ解決する: `system` は OS の
        // appearance を読むのに生きた `Window` を要るし､`Theme` は `Copy`
        // なので､解決し直す代わりに持ち回っても何の代償も無い｡
        // `light`/`dark` の config 値はそもそもウィンドウに一切
        // 依存しない｡
        let theme = config.theme.resolve(window.appearance());
        // #38: 下で `Input` ウィジェットを組み立てる前に､gpui-component 自身
        // の global theme を同じ解決済みパレットへ向ける — そもそもなぜこれ
        // が要るのかは `theme::sync_gpui_component_theme` の doc を見よ
        // (その色はまったく別の global に居る)｡
        theme::sync_gpui_component_theme(theme, window, cx);

        let (compose_input, compose_input_subscription) = Self::compose_input(window, cx);

        // #161/#164/#43: 下で `config` が move される前に取っておく｡
        let sources = source_picker::initial_sources(
            source_picker::saved_selection_for(&startup, &paths),
            config.list_id.as_deref(),
        );
        let owned_lists = source_picker::cached_lists_or_empty(&paths);
        let selection_file = matches!(startup, Startup::Live).then(|| paths.selection_file());
        // #211: 読み取り側 (`window_state::initial_bounds`) と同じ条件で
        // 塞ぐ｡fixture を撮るために広げたウィンドウが､次の live 起動の
        // 大きさを決めてはならない｡
        let window_state_file = matches!(startup, Startup::Live).then(|| paths.window_state_file());
        let (window_bounds_subscription, window_activation_subscription) =
            Self::watch_window(window, cx);
        // #22: `source` と同じく､下で `config` が move される前に取る｡
        let follow = FollowMode::from_config(config.follow_new_posts);

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
            sources,
            item_provenance: HashMap::new(),
            source_picker_open: source_picker::SourcePickerVisibility::default(),
            owned_lists,
            lists_fetch: None,
            selection_file,
            // #267: 矩形は `main` が `initial_bounds` で読んで開く位置に
            // 使った｡トグルはここで読み､下で `apply_translucency` が効かせる｡
            window_state: startup_window_state(&startup, window_state_file.as_deref()),
            window_state_file,
            _window_bounds_subscription: window_bounds_subscription,
            _window_activation_subscription: window_activation_subscription,
            window_state_save: None,
            next_page_token: None,
            threads: HashMap::new(),
            thread_fetches: HashMap::new(),
            compose: ComposeState::new(),
            compose_input,
            _compose_input_subscription: compose_input_subscription,
            submit_task: None,
            oauth_scope: None,
            session_notice: None,
            auto_refresh_notice: None,
            reload_notice: None,
            cooldown_ticker: None,
            usage_totals: usage::Totals::default(),
            usage_refresh: None,
            auto_sync: None,
            // #174: 正直な出発点｡まだ何もサインインしておらず､それは
            // gate の一つでもある｡そう言うほうが､一度も走っていない
            // "idle" よりましだ｡
            sync_status: SyncStatus::Off(SyncOff::NotSignedIn),
            pending_sync: false,
            sync_plan_pending: 0,
            sync_fade: Fade::Hidden,
            sync_fade_task: None,
            auto_refresh: None,
            refresh_situation: None,
            countdown_ticker: None,
            pending: None,
            follow,
            glide: None,
            unseen: 0,
            toast: Toast::HIDDEN,
            toast_fade_task: None,
            scroller: scroll::Scroller::default(),
            scroll_motion: None,
            fixture_arrival: None,
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
            media_failed: HashSet::new(),
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
        // #118: 何よりも先に｡最初のフレームから focus の経路に空のものでは
        // なく timeline が乗るようにするため｡
        window.focus(&this.focus_handle);
        // #267: 前回のトグルを platform の window へ効かせる｡`main` は
        // `WindowOptions` に書かず､live も fixture もここを通る｡
        this.apply_translucency(window);
        this.apply_floating(window);
        match startup {
            Startup::Live => this.start(cx),
            Startup::Fixture(fixture) => this.show_fixture(*fixture, cx),
        }
        this.refresh_usage(cx);
        this
    }

    /// composer の本物のテキスト入力 (#38) と､打鍵を `compose` へ写す購読｡
    fn compose_input(
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> (Entity<InputState>, Subscription) {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .auto_grow(2, 8)
                .placeholder("What's happening?")
        });
        let subscription = cx.subscribe(&input, Self::on_compose_input_event);
        (input, subscription)
    }

    /// ウィンドウの矩形 (#211) とフォーカスの出入り (#267) の購読｡
    ///
    /// 矩形は動くたびに覚える｡フォーカスは透過が入っている間だけ描き直す
    /// — 切れていれば見た目が変わらないので､描き直す理由が無い｡
    fn watch_window(
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> (Subscription, Subscription) {
        let bounds =
            cx.observe_window_bounds(window, |this, window, cx| this.remember_bounds(window, cx));
        let activation = cx.observe_window_activation(window, |this, _window, cx| {
            if this.window_state.translucent {
                cx.notify();
            }
        });
        (bounds, activation)
    }

    /// ウィンドウが止まったと見なすまでの間 (#211)｡
    ///
    /// ドラッグの最中は移動と resize の通知が 1 フレームごとに来る｡その
    /// たびに書けば 1 回の移動で数十回の write になるので､最後の 1 回だけ
    /// 残す｡`cmd-q` がこの間に入ると最後の変更は落ちるが､その代償は
    /// ウィンドウを一度置き直すことでしかない｡
    pub(super) const WINDOW_STATE_DEBOUNCE: Duration = Duration::from_millis(400);

    /// ウィンドウの今の矩形を覚える (#211)｡
    ///
    /// [`Context::observe_window_bounds`] から呼ばれる｡doc は "resized" と
    /// 言うが macOS の移動も同じ購読へ流れるので､位置の変更もここへ来る｡
    ///
    /// 保存するのは矩形だけで､最大化やフルスクリーンであったことは覚え
    /// ない — [`crate::window_state`] のモジュール doc を見よ｡
    fn remember_bounds(&mut self, window: &Window, cx: &mut Context<'_, Self>) {
        self.window_state.bounds = Some(window_state::SavedBounds::from(window.window_bounds()));
        self.persist_window_state(cx);
    }

    /// `window_state` の今の中身をファイルへ書く (#211, #267)｡fixture の
    /// ウィンドウには書く先が無いので何もしない｡
    ///
    /// 矩形の変更もトグルもここを通る｡トグルは 1 回きりの操作だが､debounce
    /// を挟んでも代償は 400ms 遅れて書かれることだけで､経路が 2 本になる
    /// より安い｡
    fn persist_window_state(&mut self, cx: &mut Context<'_, Self>) {
        let Some(path) = self.window_state_file.clone() else {
            return;
        };
        let state = self.window_state;
        // 前の task を落とすことが debounce になる｡gpui の `Task` は
        // drop で取り消されるので､連続する通知のうち最後の 1 つだけが
        // 時間を待ち切る｡
        self.window_state_save.take();
        self.window_state_save = Some(cx.spawn(async move |_this, cx| {
            cx.background_executor()
                .timer(Self::WINDOW_STATE_DEBOUNCE)
                .await;
            if let Err(error) = window_state::save(&path, &state) {
                log::warn(&format!("could not remember the window state: {error:#}"));
            }
        }));
    }

    /// Window メニューの Translucent (#267)｡反転させ､platform の window へ
    /// 効かせ､どちらへ倒れたかをバナーで言い､覚える｡
    pub(super) fn toggle_translucent(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.window_state.translucent = !self.window_state.translucent;
        self.apply_translucency(window);
        let outcome = if self.window_state.translucent {
            "Translucent while another window is in front."
        } else {
            "Opaque."
        };
        self.reload_notice = Some(ReloadNotice::Outcome(outcome.into()));
        self.persist_window_state(cx);
        cx.notify();
    }

    /// Window メニューの Float on Top (#267)｡[`Self::toggle_translucent`] と
    /// 同じ形: 反転させ､効かせ､言い､覚える｡
    pub(super) fn toggle_float_on_top(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.window_state.float_on_top = !self.window_state.float_on_top;
        self.apply_floating(window);
        let outcome = if self.window_state.float_on_top {
            "Floating on top of other windows."
        } else {
            "Back among the other windows."
        };
        self.reload_notice = Some(ReloadNotice::Outcome(outcome.into()));
        self.persist_window_state(cx);
        cx.notify();
    }

    /// `window_state.float_on_top` を platform の window に伝える (#267)｡
    ///
    /// `Window::set_floating` は gpui の fork の patch (Cargo.toml の
    /// `[patch.crates-io]`) にしか無い｡upstream 0.2.2 の `WindowKind` は
    /// 開くときに決めるもので､macOS では `Floating` も `Normal` と同じ
    /// level に置かれる — 開いた後に切り替える口はこれだけだ｡
    fn apply_floating(&self, window: &Window) {
        window.set_floating(self.window_state.float_on_top);
    }

    /// `window_state.translucent` を platform の window に伝える (#267)｡
    ///
    /// 不透明のときは `Opaque` へ戻す: gpui は透過の window で renderer の
    /// 経路を変える (`update_transparency`) ので､不透明の間は元の経路に
    /// 居させる｡透けるときの背景の色そのものは `theme::bg_alpha` が決め､
    /// render が塗る — これは「後ろを見せてよいか」の許可だけだ｡
    ///
    /// `Blurred` ではなく `Transparent`｡両方を fixture で撮って比べた: blur
    /// は timeline は読みやすいが後ろの窓は色の染みにしかならず､「後ろの
    /// 窓を隠さない」という #267 の目的が果たせない｡透過なら後ろの文字が
    /// 読め､手前の文字も 70% の地の上で読める｡
    fn apply_translucency(&self, window: &Window) {
        window.set_background_appearance(if self.window_state.translucent {
            WindowBackgroundAppearance::Transparent
        } else {
            WindowBackgroundAppearance::Opaque
        });
    }

    /// 今この瞬間の背景の不透明度 (#267)｡本体も toolbar も status bar も
    /// これで塗る — 帯だけが不透明に残ると､透けた一覧の上に板が浮く｡行の中に
    /// 埋め込まれた post の面 (引用カード､スレッドの行､composer のカード) も
    /// 同じもので塗る｡render が 1 回読んで枠と行へ渡す｡
    pub(super) fn bg_alpha(&self, window: &Window) -> u8 {
        theme::bg_alpha(self.window_state.translucent, window.is_window_active())
    }

    /// fixture が未読の post を抑えておく時間｡それを運んできたはずの poll
    /// を模擬するまでの長さである (#22)｡ウィンドウを画面に出して手を
    /// ホイールから離すには十分に長く､眺めるのが面倒にならない程度には
    /// 短く｡
    pub(super) const FIXTURE_ARRIVAL_SECONDS: u64 = 5;

    /// fixture だけを描き､他は何も描かない (#146)｡
    ///
    /// `client` は `None` のままで､これがこれを単に安いのではなく無料に
    /// している: この view のリクエストはすべてそこを通るので､ここから
    /// 課金へ至る経路が無い — reload も､like も､thread の辿りも｡client を
    /// 要るボタンは単に何もしない｡
    ///
    /// それでも `signed_in_with_oauth` と scope は設定する｡それらが門番を
    /// している affordance こそが､見るものの大半だからだ｡サインアウト状態の
    /// timeline として描かれた fixture では､確かめる価値のある行が
    /// 欠けてしまう｡
    fn show_fixture(&mut self, fixture: Fixture, cx: &mut Context<'_, Self>) {
        self.signed_in_with_oauth = true;
        // #156: `like_state_for`/`repost_state_for` が読む on/off の素の値は
        // 本物なら `refresh_liked_ids`/`refresh_reposted_ids` が埋めるが､
        // fixture は `client` が無く fetch を一度も走らせないので､ここで
        // 入れた値は誰にも上書きされない｡永続ファイルにも触らない｡
        self.liked_ids = fixture.liked.into_iter().collect();
        self.reposted_ids = fixture.reposted.into_iter().collect();
        // アプリが要求するすべての scope｡scope が足りないせいで affordance
        // が引っ込むことのないようにする｡fixture は決して fetch しないが
        // `list.read` (#161) はここに要る: `offers_reauthorize` が読むのは
        // ネットワークではなく scope なので､これを外すと list モードの
        // fixture すべてに "Re-authorize" ボタンが出た — レイアウトを
        // 見比べるための画面に常駐する備品である｡
        self.oauth_scope = Some(format!(
            "{} {} {}",
            oauth::tokens::TWEET_WRITE_SCOPE,
            oauth::tokens::LIKE_WRITE_SCOPE,
            oauth::tokens::LIST_READ_SCOPE
        ));
        self.home_user_id = Some(fixture.signed_in_as.id);
        self.home_username = Some(fixture.signed_in_as.username);
        self.owned_lists = fixture.lists;
        self.source_picker_open = if fixture.picker_open {
            source_picker::SourcePickerVisibility::Open
        } else {
            source_picker::SourcePickerVisibility::Closed
        };
        // #43: `sources` が空なら単一 source (Home) のままの元の挙動を
        // 保つ — `compose` の created_at ソートを経由すると、`created_at`
        // 無しの item を末尾へ沈める既存の並び替え規則が単一選択の
        // フィクスチャにも掛かってしまい、書いた順を信じているケースを
        // 壊しかねないため、経由しない。
        let sources: Vec<cache::TimelineSource> = fixture
            .sources
            .into_iter()
            .filter_map(source_picker::Selection::into_source)
            .collect();
        if sources.is_empty() {
            self.sources = vec![cache::TimelineSource::Home];
            self.state = TimelineState::Loaded(fixture.items);
        } else {
            let per_source: Vec<(cache::TimelineSource, Vec<TimelineItem>)> = sources
                .iter()
                .cloned()
                .map(|source| {
                    let items = match &source {
                        cache::TimelineSource::Home => fixture.items.clone(),
                        cache::TimelineSource::List(id) => {
                            fixture.list_items.get(id).cloned().unwrap_or_default()
                        }
                    };
                    (source, items)
                })
                .collect();
            let composed = lane::compose_with_provenance(per_source);
            self.sources = sources;
            self.item_provenance = composed.provenance;
            self.state = TimelineState::Loaded(composed.items);
        }
        // #21: 本物の poll のバッファと同じやり方で､同じ純粋関数から組む —
        // fixture が供給するのは件数ではなく post なので､bar が poll には
        // 言えないことを言うことはありえない｡バッファは表示することになる
        // 一覧の全体を持つ｡それは fixture の未読の post に､すでに画面に
        // 出ているものが続いたものである｡
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
            let waiting = pending_after_poll(&displayed, combined);
            if self.follow.is_following() {
                // #22: follow が on のとき､fixture に見せてほしいのは到着
                // そのものだ — だから pill をあらかじめ埋めるのではなく､
                // post を抑えておき､本物の poll が使う戸口を通らせる｡
                // 起動して手を触れずにいれば流れ込んでくるのが見える;
                // 先に下へスクロールしておけば､同じ配達が代わりに pill へ
                // 着く｡
                self.fixture_arrival = waiting.map(|waiting| {
                    cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(Duration::from_secs(Self::FIXTURE_ARRIVAL_SECONDS))
                            .await;
                        let _ = this.update(cx, |this, cx| this.present_poll(waiting, cx));
                    })
                });
            } else {
                self.pending = waiting;
            }
        }
        // #205: sync の状態｡`show_sync` を通すので､行が出るかどうかも
        // 出入りのフェードも本物の tick とまったく同じ経路を通る｡
        if let Some(sync) = fixture.sync {
            self.show_fixture_sync(&sync, cx);
        }
        // #214: footer のカウントダウン｡本物のウィンドウならループが
        // 最初の起床で写すものを､ループの無い (client の無い) fixture では
        // 開いた時刻を起点にして置く｡footer はこれを本物と同じ関数で読む
        // ので､fixture が本物には出ない文言を描くことはない｡
        if self.config.auto_refresh {
            self.refresh_situation = Some(Situation {
                last_reload_at: None,
                started_at: oauth::unix_now(),
                interval_seconds: self.config.auto_refresh_interval_seconds,
                busy: false,
                activity: Activity::Present,
                resumed_at: None,
            });
            self.start_countdown_ticker(cx);
        }
        // アバターと添付画像は本物と同じ経路で着く｡ただし fixture が
        // 書くのは fixture の隣のファイルで (#234)､`image_cache` はそれを
        // fetch せずそのまま返す｡ネットワークへ出ないので､オフラインでも
        // 起動のたびに同じ画面になり､WARN も出ない｡
        self.refresh_images(cx);
        cx.notify();
    }
}
