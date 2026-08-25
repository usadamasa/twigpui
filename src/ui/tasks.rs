//! `TimelineView` のうち金を使う側｡`cx.spawn` を握り､ネットワークか
//! ディスクへ手を伸ばすメソッドをすべて集めた (#137)｡
//!
//! # このファイルが体現している判断
//!
//! #137 は `impl TimelineView` を分割するか､するならどこでかを問うた｡
//! 挙がっていた選択肢は (a) 描画メソッド､(b) 非同期タスクのメソッド､
//! (c) composer の入力処理､(d) 天井を上げつづける､だった｡これは (b)｡
//!
//! (a) の方が山としては大きく､それこそが反対する理由になる｡「要素を
//! 組み立てるメソッド」は UI ファイルの体積の大半を占めるのに､その継ぎ目が
//! *何のため* にあるのかを何も語らない｡ここの継ぎ目は誰でも一文で言える —
//! **このファイルは金の出ていく場所だ** — し､新しいものでもない｡
//! [`super::reload_policy`] がリクエストを出してよいかの判断をすでに
//! 抱えていて､ここはその判断を生き延びたものが実際に送られる場所だ｡
//! 対にして読めば､一つの考えが判断とその実行へ分かれたものに見える｡
//!
//! テストを動かす必要のない山でもある｡ここには何一つ unit test が無い｡
//! `sync/run.rs` と同じ理由で､どの分岐も HTTP リクエストか､その結果が
//! 書かれるファイルだからだ｡`ui/mod.rs` のテストモジュールは元の場所に
//! そのまま残っていて､それが「これは純粋な移動だった」の確認になっている
//! — #126 が使ったのと同じ証拠だ｡
//!
//! # これが何でないか
//!
//! [`super::auto_refresh`] と [`super::list_sync`] が採る型ではない｡
//! あの二つはそれぞれ一つの *機能* — タイマー､その状態､それを駆動する
//! ボタン — をまとめたもので､後から切り出したのではなく最初からそう
//! 書かれている｡このファイルがまとめているのは一つの *種類* であり､
//! 二つの流儀は共存させるつもりでいる｡まるごと自前の機構は自前のファイル
//! を持ち､ウィンドウの既存のふるまいのうち金を使う側はここに住む｡

// 列挙ではなく `super::*` にしているのは [`super::render`] と
// [`super::auto_refresh`] に合わせたもの｡ここは `ui` の子の中で最大で､
// 親が import しているもののほとんどに手を伸ばす｡
use super::*;

impl TimelineView {
    /// 最初の fetch より前に credential (保存済みの OAuth セッション､古け
    /// れば refresh し､無ければ bearer token) を解決し､さらに #9 以降は
    /// 常に reload するのではなくローカルキャッシュからそのまま描画する｡
    /// キャッシュに当たれば起動は API リクエストを一切使わない; 外れたら
    /// [`Self::reload`] へ落ち､そちらは使う｡ディスクに触れ､token の
    /// refresh やキャッシュミスではネットワークにも触れるので background
    /// executor で動かす｡
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
                    // #54: 下の `credential` が何になろうと､常設のバナー
                    // として描画する — demote されたセッションと「一度も
                    // sign in していない」はまったく同じ credential に
                    // 解決されうるが､ユーザーに伝える価値があるのはその
                    // うち片方だけだ｡
                    let session_notice = resolution.demotion.as_ref().map(oauth::describe_demotion);
                    let Some(credential) = resolution.credential else {
                        return anyhow::Ok(StartOutcome::NotAuthenticated { session_notice });
                    };
                    // #161: どの timeline かは `config.list_id` が決め､
                    // 構築時に `self.source` へ解決される｡#33 は分岐を
                    // まるごと消していた (それを決めていた唯一のもの､
                    // app-only の bearer token が無くなったため); #157 が
                    // 一つ戻した｡home timeline が follow 先の post を運ば
                    // なくなり､それを読む手段が List になったからだ｡
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
                        cached,
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
                        this.client = Some(XClient::new(credential.token));
                        // `client` と `oauth_scope` の後に置く｡これらを
                        // 条件にし､借りるからだ; 下の fetch より前に置く｡
                        // どちらにせよ依存していないからだ｡
                        this.start_sync(SyncTrigger::Scheduled, cx);
                        match cached {
                            Some((me, items)) => {
                                this.home_user_id = Some(me.id);
                                this.home_username = Some(me.username);
                                this.state = TimelineState::Loaded(items);
                                cx.notify();
                            }
                            // 上の `SingleUser` と同じ理由｡
                            None => this.reload(ReloadTrigger::Polling, cx),
                        }
                        // #21: `cached` の match の後に置く｡決して前では
                        // ない｡miss の arm は `reload` を呼び､それが
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
    /// い｡client 無しで呼ばれたら何もしない ([`TimelineState::NotAuthenticated`]
    /// へ落ちる) — その状態で "Reload" ボタンは出ないが､呼び出し側が
    /// 正しくやったと決めてかからずここでも守る｡素の fetch ではなく
    /// [`cache::reload`] を通す: user id がキャッシュされていればリクエスト
    /// は 2 回でなく 1 回になり､結果はローカルキャッシュを丸ごと置き換え
    /// るのではなくそこへ merge (して永続化) される｡
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
        let source = self.source.clone();

        // #161: どの endpoint にリクエストを使い､結果がどのキャッシュ
        // ファイルへ落ちるかは `source` が決める｡single-user の endpoint
        // とそのキャッシュは､`--fetch-only` のために対象外のままにする｡
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
                        // #141: scroll の目標と同じ理由で､`state` が
                        // 置き換わる前に求める — 両方の一覧が要る｡
                        let outcome = this.reload_outcome(&reloaded.items);
                        this.state = TimelineState::Loaded(reloaded.items);
                        this.reload_notice = Some(ReloadNotice::Outcome(outcome.into()));
                        // 上の single-user の分岐と同じ理由｡
                        this.cooldown_ticker = None;
                        // #21: この fetch は poll が溜めたものより厳密に
                        // 新しく､新しい post をすでに画面へ出している —
                        // だから pill は､その背後に見えている post を
                        // 差し出すことになってしまう｡
                        this.clear_pending();
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
    pub(super) fn load_older(&mut self, cx: &mut Context<'_, Self>) {
        let (Some(client), Some(user_id), Some(token)) = (
            self.client.clone(),
            self.home_user_id.clone(),
            self.next_page_token.clone(),
        ) else {
            return;
        };

        self.reload_notice = None;
        // `reload` の判定を通った分岐と同じ理由: fetch がこれから出て
        // いくので､まだ刻んでいる cooldown のカウントダウン (無関係な
        // ブロックされた reload によるもの) はもう現在を説明していない｡
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

    /// ヘッダの usage 要約をディスクから読み直す (#18)｡何が引き金だった
    /// かとは独立だ — どの fetch 経路 (reload､"Load older"､"Show thread"
    /// の探索) も追跡している件数を動かしうる｡`x_api::client::XClient::get`
    /// がリクエスト自体の成否によらず実際の HTTP 送信をすべて記録する
    /// からだ｡引き金になった fetch に畳み込まず単独で spawn する:
    /// `usage.json` の読み取りが失敗しても､fetch もろとも失敗させるので
    /// はなく､ヘッダは前に出していたものをそのまま出しつづける｡
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

    /// 見えている timeline が変わるたび､ローカルの repost 記録から
    /// `self.reposted_ids` を読み直す (#15) — [`Self::refresh_usage`] の
    /// 型をそのまま写したものだ: 遅いディスク読み取りが描画を止めないよう
    /// background executor で読み､失敗した読み取りは､乗ってきた fetch
    /// もろとも失敗させるのではなくすでに出ているものを残す｡「これを
    /// repost したか」の出所はプロジェクト内でこのファイルだけなので
    /// (#15 が存在する理由そのもの — X API 自体にそんなフィールドは無い)､
    /// ここでの読み取りが古くても失われても､過小・過大に報告できるのは
    /// *このアプリ自身の* repost だけだ｡他の client からのものは決して
    /// 含まないが､この issue はそれをいずれにせよ対象外としている｡
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

    /// ローカルの like 記録から `self.liked_ids` を読み直す (#68) —
    /// [`Self::refresh_reposted_ids`] の like 側の双子で､メインスレッド
    /// の外で読む点も失敗を致命的にしない点も同じ契約だ｡呼ばれる場所も
    /// まったく同じなので､ある行の like ボタンと repost ボタンが別々の
    /// 時点から種を得ることは決してない｡
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

    /// 一つの post の like 状態を切り替える (#68) — [`Self::toggle_repost`]
    /// の like 側の双子で､楽観的な反転､background でのリクエスト､
    /// 結果 (`like::create`/`like::remove` 自身の辻褄合わせを含む) を
    /// 同じ post ごとの状態へ畳み戻すところまで同じだ｡
    ///
    /// ここで確認する scope は `tweet.write` ではなく `like.write` だ: X は
    /// これらを別々に許可するので､#68 より前に認可されたセッションは
    /// post も repost もできるが like はできない｡それでも #14 の
    /// "Re-authorize" の導線を使い回す｡flow をやり直せば全 scope をまとめ
    /// て要求するからだ｡
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

    /// 見えている timeline が必要としていて､まだ持っていない avatar を
    /// 落としてくる (#64)｡
    ///
    /// [`Self::refresh_reposted_ids`] と同じ場所すべてから呼ばれるので､
    /// ある行の avatar とそのボタンは同じ時点から来る｡取得は background
    /// executor で URL を 1 本ずつ行い､その都度 map を (ひいては view を)
    /// 更新する — 着いたそばから avatar が現れる方が､一番遅い 1 枚を
    /// timeline 全体で待つよりよい｡失敗した URL はただ欠けたままにする｡
    /// 行は placeholder を保ち､次の reload が取り直す; 読み込めなかった
    /// avatar についてユーザーに言うべき有益なことは何も無い｡
    ///
    /// これらのリクエストは X API ではなく `pbs.twimg.com` へ行く: quota
    /// も credit も無く､#18 の usage 追跡が数えるものは何も無い｡
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
                    // #49: どちらにせよ行は placeholder を保つが､黙って
                    // 欠けた avatar は､ログに 1 行無いと後から調べようが
                    // ない類のものそのものだ｡
                    Err(error) => log::warn(&format!("avatar fetch failed: {error:#}")),
                }
            }
        }));
    }

    /// 見えている timeline に欠けている画像を取る (#64, #65) — 著者の
    /// avatar と添付 media の両方だ｡
    ///
    /// timeline を変える箇所すべてで 2 回呼ぶのではなく入口を一つにする:
    /// この二つはまったく同じ瞬間に欲しくなるもので､片方だけ覚えていた
    /// 呼び出し側は行の半分を次の reload まで待たせてしまう｡
    ///
    /// **`self.state` を更新した後に呼ぶこと｡決して前ではない** (#120)｡
    /// 両方とも何が欠けているかを求めるのに `state` を読み､`Loaded` で
    /// なければ何もしないので､先に呼ぶと出ていく側の item 一覧に何が要る
    /// かを尋ねることになる: 起動時は `state` がまだ `Loading` なので何も
    /// 無く､reload では前のバッチの URL になる｡症状は､属している行より
    /// reload 1 回分遅れてしか avatar が現れないことだった｡同じ呼び出し
    /// 箇所にいる兄弟 (`refresh_usage`, `refresh_reposted_ids`,
    /// `refresh_liked_ids`) は代わりにディスクから読み､順序に依存しない｡
    /// それがこれを見落としやすくしていた｡
    pub(super) fn refresh_images(&mut self, cx: &mut Context<'_, Self>) {
        self.refresh_avatars(cx);
        self.refresh_media(cx);
    }

    /// 見えている timeline が必要としていて､まだ持っていない添付画像を
    /// 落としてくる (#65) — [`Self::refresh_avatars`] の双子で契約も同じ
    /// だ: timeline 全体で 1 タスク､background executor で URL を 1 本ずつ､
    /// 各サムネイルは着いたそばから現れ､失敗は欠けたままにするので枠は
    /// 残り､次の reload が取り直す｡
    ///
    /// 添付 media は avatar より大きいが同じ経路で届き
    /// (`pbs.twimg.com`､API の quota も credit も無い)､共有の画像
    /// キャッシュ自身のサイズ上限で抑えられている｡
    fn refresh_media(&mut self, cx: &mut Context<'_, Self>) {
        let TimelineState::Loaded(items) = &self.state else {
            return;
        };
        let mut wanted: Vec<String> = Vec::new();
        for url in items
            .iter()
            // #123: quote された post の画像も､行自身のものと同じ経路で
            // 落ちてくる｡これが無いとカードは永久に埋まらない空の枠を
            // 描くことになり､それが置き換えたテキストだけのカードより
            // 悪い｡
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

    /// 一つの post の repost 状態を切り替える (#15): ボタンは即座に反転し
    /// (ネットワークを待たない — #14 の同期的な `start_submitting` を写し
    /// たものだ)､その後 background executor で実際の create/delete リク
    /// エストを走らせ､解決した結果を — `repost::create`/`repost::remove`
    /// が自分の `Result<bool>` へすでに畳み込んだ辻褄合わせも含めて —
    /// 同じ post ごとの状態へ適用する｡
    ///
    /// client か解決済みの `home_user_id` が無ければ何もしない — repost の
    /// endpoint は *この* アカウントとして振る舞い､その id を解決するのは
    /// `/me` (#11) だけなので､まだなら呼ぶ先が無い｡`tweet.write` の scope
    /// 確認は `submit_post` のものをそのまま写しており､#15 の明示的な
    /// 指示に従って､並立する確認ではなく #14 自身の "Re-authorize" の
    /// 導線を使い回す｡
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

    /// `post_id` を本当に削除する (#72) — 2 回目のクリックだ｡
    ///
    /// 成功したら post は描画中の timeline からも *キャッシュファイルから
    /// も* 落ち､確認のためキャッシュを読み直す: いま消えて次の起動で
    /// 戻ってくる行は､#54 が扱っていた「うまくいったように見える失敗」
    /// そのもので､issue はそれを名指ししている｡
    ///
    /// 失敗した削除は API 自身のメッセージを添えて行をその場に残す｡それ
    /// が正直な結末だ — post はまだ存在している｡
    pub(super) fn confirm_delete(&mut self, post_id: String, cx: &mut Context<'_, Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(user_id) = self.home_user_id.clone() else {
            return;
        };
        // #161: 削除がどのキャッシュファイルから消さねばならないかは､
        // ウィンドウが描画しているものによる｡
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
                    // X が削除を認めてからにする: 先にローカルで忘れると
                    // まだ存在する post を隠すことになる｡
                    cache::forget_post(&paths, &source, &user_id, &request_id, oauth::unix_now())
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                this.refresh_usage(cx);
                match result {
                    Ok(remaining) => {
                        this.delete_failures.remove(&post_id);
                        this.state = TimelineState::Loaded(remaining);
                        // #21: 削除より前に取られた buffer は削除された
                        // post をまだ持っている｡後から適用すると画面へ
                        // 戻してしまう — #72 がキャッシュファイルを書き
                        // 直してまで防いでいる失敗そのものだ｡
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

    /// `url` をシステムのブラウザへ渡す (#70)｡
    ///
    /// click handler ではなく background executor で走らせる: プロセスの
    /// 起動は syscall であり､UI スレッドが待つ理由は無い｡拒否や起動の
    /// 失敗は `open_failure` を通して報告され､行がそれを描く — クリック
    /// が黙って何もしないことこそ､ここで避ける価値のある結末だ｡
    ///
    /// ここで唯一 API のクレジットを使わないメソッドだ｡それでもここに
    /// 在るのは､継ぎ目が「`cx.spawn` を握り､プロセスの外へ手を伸ばす」
    /// であり､ブラウザの起動がまさにそれだからだ — `ui` に置き去りにす
    /// れば､境界は誰にも言い直せないものになってしまう｡
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

    /// 対話的な PKCE の sign-in flow を走らせる: ブラウザを開き､loopback
    /// の callback を待ち､code を交換し､token を永続化して､そのまま
    /// [`Self::reload`] へ落ちる｡
    pub(super) fn sign_in(&mut self, cx: &mut Context<'_, Self>) {
        // #33: `Config::resolve` がこれ無しでは起動を拒むので､ここで
        // 確認することはもう無い｡
        let client_id = self.config.oauth_client_id.clone();

        // flow の開始は見えなかった: 成功は "signed in with OAuth" を､
        // 失敗はエラーをログに残すが､ブラウザが戻ってこなかったクリック
        // は何一つ残さなかった｡
        log::info("sign-in started — opening the browser and waiting for its callback");
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
                    // #14: 新しく許可された scope — 再認可が成功した直後
                    // に `offers_reauthorize` がボタンを出すのをやめるのは
                    // これのおかげだ｡
                    this.oauth_scope.clone_from(&tokens.scope);
                    // #54: 新しい sign-in はバナーが報じていた何であれ
                    // 解消する — 期限切れのセッションが真新しいものを
                    // 越えて期限切れのままではいられない｡
                    this.session_notice = None;
                    // #11: 保存された OAuth セッションは常に home timeline
                    // へ対応する — `TimelineSource::for_credential` を見よ｡
                    this.client = Some(XClient::new(tokens.access_token));
                    // sync が始まりうるもう一方の場所だ｡"Re-authorize" が
                    // 通る経路であり､scope 不足で断られたセッションに
                    // とって効くのはこちらだ: 足りなかった scope がいま
                    // 許可されたところで､これが無いと sync はアプリを
                    // 再起動するまで止まったままになる｡
                    this.start_sync(SyncTrigger::Scheduled, cx);
                    // #21: 同じ理由で auto-refresh が始まりうるもう一方の
                    // 場所だ — ここまで poll が取得に使う client が無かっ
                    // た｡下の reload より前に始める｡そうすればその reload
                    // 自身の `last_reload_at` が最初の poll の起点になる｡
                    this.start_auto_refresh(cx);
                    // #21: セッションの変化は poll が溜めた何よりも新しい
                    // 出所だ — `clear_pending` を見よ｡
                    this.clear_pending();
                    // #57: ユーザーがいまやったことを確かめる — #10 の
                    // 間隔を待ってはならない｡あれは poll を抑えるためで
                    // あり､ユーザー操作への直接の応答を止めるためではない｡
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

    /// composer のいまの下書きを新しい post として送る (#14)｡
    /// [`ComposeState::quote`] が何か持っていればそれを quote する (#16)｡
    ///
    /// [`ComposeState::can_submit`] が是と言わないかぎり — タスクを spawn
    /// もせずネットワークにも触れず — 何もしない｡二重送信を排除している
    /// のもこれだ: `can_submit` は `compose.status` に依存し､すべての
    /// guard を通った直後の文がその status を *同期的に* `Submitting` へ
    /// する｡この関数が gpui の event loop へ戻るより前､`cx.spawn` で
    /// background executor へ譲るより前のことだ｡gpui は次の入力イベントを
    /// 配る前に一つの click handler を最後まで走らせるので､どれだけ速い
    /// 2 回目のクリックでも `submit_post` を再び呼ぶのはこれが戻った後で
    /// あり､その時点で `can_submit` は偽､関数は冒頭で即座に戻る｡タスク
    /// は spawn されず､`submit_task` が飛行中に上書きされることも無い｡
    ///
    /// 下の scope 確認を `ComposeState` の一部にしていないのは意図的だ:
    /// あの型は下書きの *テキスト* しか知らず､セッションの OAuth scope は
    /// 知らない｡だから `tweet.write` の欠落は — 403 が確定しているリク
    /// エストを使う前に — `can_submit` ではなく `ComposeState::refuse` で
    /// ここで断る｡実際の解決策はヘッダの "Re-authorize" ボタンだ
    /// (`offers_reauthorize` を見よ)｡
    ///
    /// このファイルの他のアクションの多くと違い `window` を取るのは､#38
    /// の成功経路がそれを必要とするからだ: `compose_input` 自身のバッファ
    /// を空にする処理は — フィールドの doc を見よ — `InputState::set_value`
    /// を通り､これが `window` を要求する｡この構造体の他のアクションが使う
    /// 素の `cx.spawn`/`update` ではなく `cx.spawn_in`/
    /// `WeakEntity::update_in` を使うのは､まさにそのために `Window` を
    /// `await` を越えて運ぶためだ｡
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
        // #16: "Quote" が対象に据えた post があればそれだ — 上の `text`
        // と同じく､下の closure が `apply_result` の変更を通して暗黙に
        // `self.compose` を動かす前に clone しておく｡
        let quote_tweet_id = self.compose.quote().map(|target| target.post_id.clone());
        // #71: "Reply" が据えていれば､この reply が答える post だ｡上の
        // quote とは排他だ — `ComposeState::set_reply` を見よ｡
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
                    // `apply_result` の `Ok` 分岐は `this.compose` 側の
                    // 写しを消したところだが､`compose_input` は widget
                    // 自身のまったく別のバッファだ (#38) — ユーザーに見え
                    // る入力欄を実際に空にするのはこちらだ｡
                    this.compose_input.update(cx, |state, cx| {
                        state.set_value("", window, cx);
                    });
                    // 成功した post は timeline を変えるので reload へ
                    // 落ちる — ただし #57: これは poll ではなくユーザーが
                    // いまやったことの結果を確かめるもので (post 自体が
                    // すでにリクエストを 1 回使っている)､#10 の間隔に
                    // 止められる危険を冒すのではなく迂回せねばならない｡
                    this.reload(ReloadTrigger::UserAction, cx);
                }
                cx.notify();
            });
        }));
    }
}
