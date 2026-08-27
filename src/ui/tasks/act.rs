//! 読み手が頼んだ書き込み (#241): like､repost､delete､post､sign-in､
//! そしてブラウザを開くこと｡ブラウザ以外はどれも API のクレジットを使う｡

// 列挙ではなく glob にしているのは [`crate::ui::render`] と
// [`crate::ui::auto_refresh`] に合わせたもの｡
use crate::ui::*;

impl TimelineView {
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
    pub(in crate::ui) fn toggle_like(&mut self, post_id: String, cx: &mut Context<'_, Self>) {
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
    pub(in crate::ui) fn toggle_repost(&mut self, post_id: String, cx: &mut Context<'_, Self>) {
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
    pub(in crate::ui) fn confirm_delete(&mut self, post_id: String, cx: &mut Context<'_, Self>) {
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
    pub(in crate::ui) fn open_in_browser(&mut self, url: String, cx: &mut Context<'_, Self>) {
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
    pub(in crate::ui) fn sign_in(&mut self, cx: &mut Context<'_, Self>) {
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
                    let scope = tokens.scope.clone();
                    // #239: 起動時の経路と同じく `Session` を渡す｡ここで
                    // 文字列を渡すと､サインインし直した窓だけが 2 時間で
                    // 401 に落ちる窓に戻る｡
                    anyhow::Ok(oauth::Credential {
                        session: oauth::Session::new(client_id, paths, tokens),
                        scope,
                    })
                })
                .await;

            let _ = this.update(cx, |this, cx| match result {
                Ok(credential) => {
                    log::info("signed in with OAuth");
                    this.signed_in_with_oauth = true;
                    // #14: 新しく許可された scope — 再認可が成功した直後
                    // に `offers_reauthorize` がボタンを出すのをやめるのは
                    // これのおかげだ｡
                    this.oauth_scope.clone_from(&credential.scope);
                    // #54: 新しい sign-in はバナーが報じていた何であれ
                    // 解消する — 期限切れのセッションが真新しいものを
                    // 越えて期限切れのままではいられない｡
                    this.session_notice = None;
                    // #11: 保存された OAuth セッションは常に home timeline
                    // へ対応する — `TimelineSource::for_credential` を見よ｡
                    this.client = Some(XClient::renewing(credential.session));
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
    pub(in crate::ui) fn submit_post(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
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
