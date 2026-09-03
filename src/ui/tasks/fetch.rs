//! timeline を埋める読み取り (#241): 起動､reload､"Load older"､
//! "Show thread"｡どれも API のクレジットを使う｡

// 列挙ではなく glob にしているのは [`crate::ui::render`] と
// [`crate::ui::auto_refresh`] に合わせたもの｡`ui` が import しているものの
// ほとんどに手を伸ばす｡
use crate::ui::lane;
use crate::ui::*;

impl TimelineView {
    /// 最初の fetch より前に credential (保存済みの OAuth セッション､古け
    /// れば refresh し､無ければ bearer token) を解決し､さらに #9 以降は
    /// 常に reload するのではなくローカルキャッシュからそのまま描画する｡
    /// キャッシュに当たれば起動は API リクエストを一切使わない; 外れたら
    /// [`Self::reload_sources`] へ落ち､そちらは使う｡ディスクに触れ､token の
    /// refresh やキャッシュミスではネットワークにも触れるので background
    /// executor で動かす｡
    ///
    /// #43: `cache::startup_primary` の all-or-nothing (`/me` と *その 1
    /// source* のキャッシュが両方揃わないと `None`) は N ソースにそのまま
    /// 使えない｡代わりに `/me` だけ解決し､`update` クロージャ側でキャッシュ
    /// のある source だけ合成して即座に描き､欠けている source だけを
    /// [`Self::reload_sources`] へ回す (§3.5 の「on にする」規則を起動時にも
    /// 適用する)｡
    pub(in crate::ui) fn start(&mut self, cx: &mut Context<'_, Self>) {
        self.state = TimelineState::Loading;

        let config = self.config.clone();
        let paths = self.paths.clone();

        self.fetch = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let resolution = oauth::resolve_credential(&config, &paths, oauth::unix_now())?;
                    // #54: 下の `credential` が何になろうと､常設のバナー
                    // として描画する — demote されたセッションと「一度も
                    // sign in していない」はまったく同じ credential に
                    // 解決されうるが､ユーザーに伝える価値があるのはその
                    // うち片方だけだ｡
                    let session_notice = resolution.demotion.as_ref().map(oauth::describe_demotion);
                    let Some(credential) = resolution.credential else {
                        return anyhow::Ok(StartOutcome::NotAuthenticated { session_notice });
                    };
                    let me = cache::cached_me(&paths, oauth::unix_now())?;
                    anyhow::Ok(StartOutcome::Home {
                        credential,
                        me,
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
                        // どちらの行も 2026-08-24 に由来する: X が更新を
                        // 拒んだセッションのせいで､何もできないウィンドウ
                        // についてログが語るのは "starting twigpui" だけ
                        // だった｡バナーは画面で理由を言ったが､ファイルは
                        // 何も言わなかった (#199 の規則を auth に適用)｡
                        match &session_notice {
                            Some(notice) => log::warn(notice),
                            None => log::info("no stored session"),
                        }
                        log::warn("not signed in — waiting for \"Sign in with X\"");
                        this.session_notice = session_notice.map(SharedString::from);
                        this.state = TimelineState::NotAuthenticated;
                        cx.notify();
                    }
                    Ok(StartOutcome::Home {
                        credential,
                        me,
                        session_notice,
                    }) => {
                        // credential が解決できた上での demotion (bearer
                        // への fallback) は静かな方だ — 上の arm と同じ
                        // 理由で 1 行残す価値がある｡
                        if let Some(notice) = &session_notice {
                            log::warn(notice);
                        }
                        this.session_notice = session_notice.map(SharedString::from);
                        this.signed_in_with_oauth = true;
                        this.oauth_scope.clone_from(&credential.scope);
                        this.client = Some(XClient::renewing(credential.session));
                        // `client` と `oauth_scope` の後に置く｡これらを
                        // 条件にし､借りるからだ; 下の fetch より前に置く｡
                        // どちらにせよ依存していないからだ｡
                        this.start_sync(SyncTrigger::Scheduled, cx);
                        match me {
                            Some(me) => {
                                this.home_user_id = Some(me.id.clone());
                                this.home_username = Some(me.username);
                                let composed = lane::load_composite_timeline(
                                    &this.paths,
                                    &this.sources,
                                    &me.id,
                                );
                                this.item_provenance = composed.provenance;
                                this.state = TimelineState::Loaded(composed.items);
                                cx.notify();
                                // #43: キャッシュが無い source だけ埋める｡
                                // 1 つも欠けていなければ reload は起きない｡
                                let missing =
                                    lane::missing_sources(&this.paths, &this.sources, &me.id);
                                if !missing.is_empty() {
                                    this.reload_sources(missing, ReloadTrigger::Polling, cx);
                                }
                            }
                            // `/me` が未解決なら合成のしようが無いので通常の
                            // reload へ落ちる — 上の `Some` 分岐と同じ理由｡
                            None => this.reload(ReloadTrigger::Polling, cx),
                        }
                        // #21: `me` の match の後に置く｡決して前ではない｡
                        // miss の arm は reload を呼び､それが
                        // `last_reload_at` — 最初の poll を測る起点 — を
                        // 立てる｡先に始めるとループはウィンドウが開いた
                        // 時刻を起点にしてしまい､着いたばかりの fetch の
                        // 1 間隔後に poll を 1 回買うことになる｡
                        this.start_auto_refresh(cx);
                    }
                    Err(error) => {
                        this.state = TimelineState::Failed(format!("{error:#}").into());
                        cx.notify();
                    }
                }
                // #120: match の後に置く｡決して前ではない｡`refresh_images`
                // はどの avatar と media が欠けているかを決めるのに
                // `self.state` を読むので､先に呼ぶと *前の* state を
                // 渡すことになる — 起動時なら `Loading` で何も取らず､
                // reload なら出ていく側の item 一覧で､前のバッチが欲し
                // がった画像を取る｡avatar が reload 1 回分遅れて現れて
                // いたのはこれが理由だ｡上にある兄弟たちは `state` では
                // なくディスクから読むので位置は関係ない; これは関係する｡
                this.refresh_images(cx);
            });
        }));

        cx.notify();
    }

    /// reload は毎回 API のクレジットを使うので､明示的な操作でしか走らな
    /// い｡選択中の全 source を対象にする [`Self::reload_sources`] の薄い
    /// ラッパー — 呼び出し側のほとんどはこれで十分で､対象を選びたいのは
    /// [`Self::start`] の欠損補充だけだ｡
    pub(in crate::ui) fn reload(&mut self, trigger: ReloadTrigger, cx: &mut Context<'_, Self>) {
        let sources = self.sources.clone();
        self.reload_sources(sources, trigger, cx);
    }

    /// `sources` を対象に reload の credit を使う (#43)｡client 無しで
    /// 呼ばれたら何もしない ([`TimelineState::NotAuthenticated`] へ落ちる)
    /// — その状態で "Reload" ボタンは出ないが､呼び出し側が正しくやったと
    /// 決めてかからずここでも守る｡素の fetch ではなく [`lane::reload_all`]
    /// (内部で [`cache::reload_primary`]) を通す: user id がキャッシュ
    /// されていればリクエストは 2 回でなく 1 回になり､結果はローカル
    /// キャッシュを丸ごと置き換えるのではなくそこへ merge (して永続化)
    /// される｡
    ///
    /// さらに､何かを spawn する前に `config.min_fetch_interval_seconds`
    /// (#10) を課す｡ただし `trigger` が [`ReloadTrigger::UserAction`] (#57)
    /// のときは除く — なぜ一部の呼び出し側がこれを迂回しなければならない
    /// かはその variant の doc を見よ｡課される場合､[`reload_cooldown`] は
    /// ネットワークに触れずに判定するボタン自身へのクライアント側の絞りで
    /// あり､実際にリクエストが出たときに追跡している API の rate-limit
    /// 状態が言うことの代わりではなく､その上に乗る｡
    ///
    /// cooldown も失敗した fetch も､`state` がすでに post を持っている
    /// 間はそれに触れない (#57): これを決める純粋関数が
    /// [`reload_start_state`] と [`reload_failure_outcome`] で､
    /// cooldown/失敗の文言は `reload_notice` が独立に運ぶ —
    /// [`ReloadNotice`] の doc を見よ｡まだ何も読み込めていない reload は
    /// `TimelineState::Loading`/`RateLimited`/`Failed` へ落ちる｡その場合
    /// body が描けるものは他に無いからだ｡
    ///
    /// 完了ハンドラは `sources` (捕獲した集合) ではなく `this.sources`
    /// (完了時点の集合) で再合成する: 直列 fetch の途中でユーザーが
    /// source を off にしたら､もう表示しないはずの source の post を
    /// 着地させないため (opus-advisor A-4)｡`next_page_token` は
    /// `this.sources.len() == 1` のときだけ書く — 複数選択中は常に `None`
    /// にする不変条件 (§3.6, opus-advisor B-6)｡部分失敗
    /// ([`lane::reload_all`] のドキュメントを見よ) は取れた分を合成して
    /// 1 回だけ画面を更新し､`reload_notice` に失敗数を添える｡
    pub(in crate::ui) fn reload_sources(
        &mut self,
        sources: Vec<cache::TimelineSource>,
        trigger: ReloadTrigger,
        cx: &mut Context<'_, Self>,
    ) {
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
            // #57: cooldown はリクエストが送られる前に止めているので､
            // すでに画面にあるものはそのままだ — これは notice であって
            // state の変更ではない｡
            self.reload_notice = Some(ReloadNotice::Cooldown {
                reset_at,
                cooldown: Cooldown::LocalInterval,
            });
            // #57 の項目 3: これが無いとバナーのカウントダウンは描画され
            // た瞬間の秒で止まったままになる — `start_cooldown_ticker` の
            // doc を見よ｡
            self.start_cooldown_ticker(cx);
            cx.notify();
            return;
        }
        self.last_reload_at = Some(now);

        self.reload_notice = None;
        // 新しい reload が実際に出ていくなら､カウントダウン中だった
        // cooldown は用済みになる (リクエストが飛んでしまえば待つものは
        // 何も残らない) — 次の tick で気づくのに任せて最大 1 秒放置する
        // のではなく､明示的に止める｡
        self.cooldown_ticker = None;
        self.reloading = true;
        self.state = reload_start_state(std::mem::replace(&mut self.state, TimelineState::Loading));

        let paths = self.paths.clone();
        let max_results = self.config.max_results;

        self.fetch = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    lane::reload_all(&paths, &client, &sources, max_results, oauth::unix_now())
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                this.refresh_usage(cx);
                this.refresh_reposted_ids(cx);
                this.refresh_liked_ids(cx);
                this.reloading = false;
                match result {
                    Ok(outcome) => {
                        // `successes > 0` (`lane::reload_all` の契約) なら
                        // `me` は必ず `Some`｡
                        if let Some(me) = outcome.me {
                            this.home_user_id = Some(me.id.clone());
                            this.home_username = Some(me.username);
                            this.next_page_token = if this.sources.len() == 1 {
                                outcome.next_token
                            } else {
                                None
                            };
                            let composed =
                                lane::load_composite_timeline(&this.paths, &this.sources, &me.id);
                            this.keep_the_reader_in_place(&composed.items);
                            // #141: scroll の目標と同じ理由で､`state` が
                            // 置き換わる前に求める — 両方の一覧が要る｡
                            let label = this.reload_outcome(&composed.items);
                            this.state = TimelineState::Loaded(composed.items);
                            this.item_provenance = composed.provenance;
                            this.reload_notice = Some(ReloadNotice::Outcome(
                                partial_failure_label(label, outcome.failures, outcome.successes)
                                    .into(),
                            ));
                            // 上の single-user の分岐と同じ理由｡
                            this.cooldown_ticker = None;
                            // #21: この fetch は poll が溜めたものより厳密に
                            // 新しく､新しい post をすでに画面へ出している —
                            // だから pill は､その背後に見えている post を
                            // 差し出すことになってしまう｡
                            this.clear_pending();
                        }
                    }
                    Err(error) => this.apply_reload_failure(&error, cx),
                }
                // match の後に置く｡理由は `start` (#120) に書いたとおり:
                // 前に置くと *出ていく側* の item 一覧が欲しがった画像を
                // 取ってしまい､新しく着いた行はすべて次の reload まで
                // placeholder のままになる｡
                this.refresh_images(cx);
                cx.notify();
            });
        }));

        cx.notify();
    }

    /// 終わった reload が自分について何と言うべきか (#141)｡
    ///
    /// [`Self::keep_the_reader_in_place`] と同じく､`state` が置き換わる
    /// 前に届いた一覧を渡して呼ぶ｡理由も同じで､件数は二つの一覧の差だ
    /// からだ｡
    ///
    /// 最初の読み込みには比べる前の一覧が無いので､その中身はすべて新着
    /// として数える — 実際そのとおりだ｡
    fn reload_outcome(&self, incoming: &[TimelineItem]) -> String {
        let previous: Vec<&str> = match &self.state {
            TimelineState::Loaded(items) => items.iter().map(|item| item.id.as_str()).collect(),
            _ => Vec::new(),
        };
        let new_ids: Vec<&str> = incoming.iter().map(|item| item.id.as_str()).collect();
        reload_outcome_label(newly_arrived(&previous, &new_ids))
    }

    /// scroll している読み手を reload が突き飛ばすのを取り消す (#22)｡
    ///
    /// `state` が置き換わる *前* に､届いた一覧を渡して呼ぶ｡何件届いたか
    /// を求めるのに両方の一覧が要るからだ｡判断は
    /// [`preserved_scroll_target`] に委ね､断られたら何もしない — 読み手
    /// は先頭にいて､何も無いところの上へ新着が来るのは望みどおりの
    /// ふるまいだ｡
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

    /// [`Self::reload`] の二つの fetch 分岐と [`Self::load_older`] で共有
    /// する `Err` 処理 (#57): 既存の post は [`reload_failure_outcome`] を
    /// 通して失敗した fetch を生き延びる — 独立したメソッドに切り出したの
    /// は､半分は `reload` 自体を clippy の行数 lint の下に収めるため､
    /// 半分は下の `Option<ReloadNotice>` (と #57 の項目 3 以降は ticker) の
    /// 扱いを､ずれうる 3 つの写しではなく 3 箇所すべてでまったく同じに
    /// するためだ｡
    fn apply_reload_failure(&mut self, error: &anyhow::Error, cx: &mut Context<'_, Self>) {
        // #49: Finder から起動した `.app` に stderr は無いので､これが
        // 無いと失敗した reload は､ユーザーが閉じたバナーのほかに何も
        // 残さない｡出ていく途中で `log::redact` が走る — API のエラーは
        // それを生んだリクエストを引用しうる｡
        log::error(&format!("reload failed: {error:#}"));
        let (state, notice) = reload_failure_outcome(
            std::mem::replace(&mut self.state, TimelineState::Loading),
            error,
        );
        self.state = state;
        // #57: `state` 自身が失敗を語るようになった場合
        // (`Failed`/`RateLimited`)､`reload_failure_outcome` はすでに
        // `None` を返す — その doc を見よ｡それを `Some` で包まずそのまま
        // 通すことが､同じ失敗が二度表示されるのを止めている｡
        self.reload_notice = notice;
        // rate limit による失敗は新しい `Cooldown` notice を立てる
        // (#10 のローカルなものではなく X 自身の window だが､カウント
        // ダウンは同じように刻む必要がある) — そのために ticker を開始/
        // 置換する｡それ以外の結末 (`Failed`､あるいは notice 無し) には
        // 数え下げるものが残っていないので､もう当てはまらない notice を
        // 見つづけさせるのではなく､走っているかもしれない ticker を止める｡
        if matches!(self.reload_notice, Some(ReloadNotice::Cooldown { .. })) {
            self.start_cooldown_ticker(cx);
        } else {
            self.cooldown_ticker = None;
        }
    }

    /// `reload_notice` のカウントダウンを 1 秒ごとに刻む (#57 の項目 3) —
    /// そもそもなぜこれが在るのかは
    /// [`cooldown_ticker`](Self::cooldown_ticker) の doc を見よ｡開始する
    /// のは `reload_notice` が実際に `ReloadNotice::Cooldown` になって
    /// いるときだけで (`Failed` の notice には数え下げるものが無い)､
    /// [`Self::reload`] の cooldown 判定の分岐と
    /// [`Self::apply_reload_failure`] から呼ばれる｡
    ///
    /// ループは開始時に捉えた `reset_at` を信じるのではなく､起きるたび
    /// *その時点の* `reload_notice` に対して [`cooldown_tick`] を引き直
    /// す: `reload_notice` は､誰かがこのループを名指しで cancel しに来
    /// なくても､走っている ticker の足元で変わりうる (reload が成功して
    /// 消える､後の失敗が `Failed` で置き換える)｡引き直すことがそれを
    /// 安全にしている — ループはその時点で在るものを踏み潰すのではなく､
    /// 次に起きたときにただ止まる｡必ず終わりもする: `cooldown_tick` が
    /// `NotTicking`/`Elapsed` を返すか､view 自体が drop されて
    /// `this.update` が `Err` を返すかのどちらかで､永久に回る経路は無い｡
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
                    // view が drop された — 刻むものは何も残っていない｡
                    return;
                };

                if !keep_going {
                    return;
                }
            }
        }));
    }

    /// `next_page_token` の先のページを取り､すでに表示されているものの
    /// 後ろへ足す (#11 の "Load older") — 意味を持つのは
    /// `TimelineSource::Home` のときだけだ｡`SingleUser` モードはそもそも
    /// token を立てない｡三つの前提 (client､判明済みの home user id､
    /// 再開するための token) のどれかが欠けていたら何もしない｡
    ///
    /// [`Self::reload`] の「すでに画面にあるものを追い出さない」修正
    /// (#57) を､同じ純粋関数 [`reload_start_state`] と
    /// [`reload_failure_outcome`] を通して共有する — ここでは素の reload
    /// より *もっと* 重要だとも言える: これは何かがすでに `Loaded` に
    /// なってからしか走らず ([`offers_load_older`] の条件を見よ)､いま
    /// 表示されているものから *遡って* ページを繰っているので､リクエスト
    /// の途中でそれを失うのは､何も無いところから始めた reload が失敗する
    /// より厳密に悪い｡busy 表示には専用のフラグではなく `self.reloading`
    /// を使い回す — ヘッダの "Loading…" というラベルはどちらの fetch の
    /// 説明としても正確だし､#57 がこの呼び出し箇所に求めたのは post を
    /// 捨てるのをやめることであって､"Load older" 専用の装飾を生やすこと
    /// ではない (行そのものは busy/disabled の別スタイルを持たず､この
    /// 修正の前から変わっていない)｡
    pub(in crate::ui) fn load_older(&mut self, cx: &mut Context<'_, Self>) {
        // #43: 複数選択中はそもそも `next_page_token` が常に `None`
        // (`reload_sources` を見よ) なので､下の `let else` で自然に
        // 何もしない｡`[TimelineSource; 1]` を明示的に要求はしないが､
        // 単一選択という前提は `offers_load_older` (`sources.len() == 1`
        // を条件に足した) がボタンの表示で守っている｡
        let (Some(client), Some(user_id), Some(token), [source]) = (
            self.client.clone(),
            self.home_user_id.clone(),
            self.next_page_token.clone(),
            self.sources.as_slice(),
        ) else {
            return;
        };
        let source = source.clone();

        self.reload_notice = None;
        // `reload` の判定を通った分岐と同じ理由: fetch がこれから出て
        // いくので､まだ刻んでいる cooldown のカウントダウン (無関係な
        // ブロックされた reload によるもの) はもう現在を説明していない｡
        self.cooldown_ticker = None;
        self.reloading = true;
        self.state = reload_start_state(std::mem::replace(&mut self.state, TimelineState::Loading));

        let paths = self.paths.clone();
        let max_results = self.config.max_results;

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
                        // 上の `reload` の成功分岐と同じ理由｡
                        this.cooldown_ticker = None;
                        // #21: このページが足される前に取られた buffer は
                        // それを含まないので､後から適用するとクリックを
                        // 黙って取り消してしまう｡
                        this.clear_pending();
                    }
                    Err(error) => this.apply_reload_failure(&error, cx),
                }
                // match の後に置く (#120)｡`start` や `reload` と同じで､
                // 画像が欠けているのはいま足したページだ｡
                this.refresh_images(cx);
                cx.notify();
            });
        }));

        cx.notify();
    }

    /// 一つの reply のために "Show thread" のクレジットを使う (#12): 親の
    /// chain を辿り (取得済みならキャッシュから､でなければネットワーク
    /// から､最大 `thread::MAX_THREAD_DEPTH` リクエスト)､結果を描画する｡
    /// client 無しでは何もしない — その状態で toggle は出ないが､
    /// [`Self::reload`] の流儀に合わせてここでも守る｡
    ///
    /// `reply_post_id` は展開される側の reply (キャッシュ/状態のキー);
    /// `first_parent_id` はその直接の親の id — ただで判明している
    /// `TimelineItem::replied_to` の `post_id` — で､そこから辿り始める｡
    pub(in crate::ui) fn show_thread(
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
}
