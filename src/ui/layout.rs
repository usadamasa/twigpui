//! ウィンドウの組み立て (#241): timeline の本体 (`body`) と､枠・バナー・
//! composer・本体・sync の行・status bar を縦に積む [`Render`] の impl｡
//! 枠そのものは `chrome.rs`､1 行の post は `post_row.rs`｡
//!
//! `ui/mod.rs` にあったものをそのまま移した｡

use super::*;

impl TimelineView {
    fn body(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = self.theme;

        // `overflow_y_scroll` は StatefulInteractiveElement 側にあるので､
        // スクロールさせるには要素に先に id が要る｡
        let content = div()
            .addressable("timeline")
            .flex()
            .flex_col()
            .flex_1()
            .overflow_y_scroll()
            // #22: そもそもスクロール位置を読めるようにしているのがこの
            // ハンドルだ｡これが無ければリロードはビューポートを*ピクセル*の
            // 位置に留めることしかできず､上に行が挿入された後ではそこは
            // 間違った場所になる｡
            .track_scroll(&self.list_scroll)
            // #175: 端を越えて引いたぶんだけ一覧をずらす — 最上部では
            // 下へ､末尾では上へ｡offset は gpui が prepaint で clamp する
            // ので､端の向こうは offset ではなく位置で見せるしかない｡
            .relative()
            .top(px(self.scroller.shift()));

        let list = match &self.state {
            // ツールバー側のボタンを指す文ではなく､ボタンそのものを置く｡
            // リストのタブが増えるとそのボタンは右端の外へ押し出されるし
            // (#192)､唯一の案内が見えないボタンの名を挙げるだけのサインアウト
            // 済みウィンドウは､画面からは復帰しようがない — X が更新を拒んだ
            // セッションで 2026-08-24 に実測した状態だ｡
            TimelineState::NotAuthenticated => content.child(
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .child(
                        div()
                            .text_color(rgb(theme.text_muted))
                            .child("Not signed in."),
                    )
                    .child(sign_in_pill("sign-in-body", "Sign in with X", theme, cx)),
            ),
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
                // `.children(items.iter().map(...))` ではなく素のループにして
                // ある｡`post_row` は (#12 の "Show thread" のクリック
                // ハンドラのために) `cx` を要求するし､`.map` が呼ぶ `FnMut`
                // クロージャは､自分が捕捉した `cx` から借りた値を返り値の要素へ
                // 逃がせない｡
                let mut rows: Vec<AnyElement> = Vec::with_capacity(items.len());
                for item in items {
                    rows.push(self.post_row(item, cx));
                }
                content
                    .children(rows)
                    // #11: 再開に使う `meta.next_token` をレスポンスが実際に
                    // 運んできて初めて出す｡しかも取得するページの分だけ上限の
                    // 下に余地があるあいだだけだ｡
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
        };

        // #175: ずれない wrapper｡band で一覧がずれても外へは描かせず､
        // ホイールを横取りする canvas はここに重ねる — ずれた一覧に
        // 重ねると､跳ねている最中に露出した端の上で入力が死ぬ｡
        div()
            .flex()
            .flex_col()
            .flex_1()
            .relative()
            .overflow_hidden()
            .child(list)
            .child(Self::wheel_capture(cx))
            // #206: 新着の toast｡一覧の外､ずれない wrapper に重ねるので
            // scroll しても下端に留まる｡`when_some` なので無いときは
            // 要素そのものが無い｡
            .when_some(self.toast(cx), ParentElement::child)
    }
}

/// リストの後ろに足す "Load older" の行 (#11)｡見た目は [`notice`] と同じだが
/// クリックできる — `cache::splice` 経由で､すでに表示されているものの*後ろ*へ
/// post を足すのであって､通常のリロードのように前へマージすることはない｡
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
        // #206: toast の件数には書き手が多いので､見直すのは描画の頭で —
        // `fade_toast` の doc を見る｡`body` がこの結果を読む｡
        self.fade_toast(cx);
        // #214: 枠の文言はウィンドウの幅で選ぶ｡toolbar と footer が別々の
        // 段にならないよう､ここで 1 回決めて両方へ渡す｡
        let density = countdown::density(window.viewport_size().width);

        div()
            // #58: どのバインディングもグローバルに登録するのではなく､この
            // コンテキストへ閉じてある — `init` を見る｡
            .key_context(KEY_CONTEXT)
            // #118: コンテキストが効くのは､その要素がウィンドウのフォーカス
            // パス上にあるあいだだけで､本当のルートは
            // `gpui_component::Root` だ (`main` を見る) — なのでこれが無いと
            // パスがコンテキストの手前で止まり､全バインディングが外れた｡
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Reload, _window, cx| {
                // ヘッダーのボタンが通るのと同じ経路｡#10 の間隔と #57 の
                // クールダウン報告も含む｡ショートカットが､このアプリが
                // ループで金を使うのを止めるためにあるスロットルの抜け道に
                // なってはいけない｡
                this.reload(ReloadTrigger::UserAction, cx);
            }))
            .on_action(cx.listener(|this, _: &SyncList, _window, cx| {
                // #248: footer の入口と同じ経路｡ダイアログが確認を挟み､
                // 始められない状態ではその理由を出す (`ask_to_sync`)｡
                this.ask_to_sync(cx);
            }))
            .on_action(cx.listener(|this, _: &FocusComposer, window, cx| {
                this.compose_input
                    .update(cx, |input, cx| input.focus(window, cx));
            }))
            .on_action(cx.listener(|this, _: &BlurComposer, window, _cx| {
                // フォーカスだけ｡下書きは打ったとおりに残す｡誤爆した `esc` で
                // 失うと取り返しがつかないし､#14 はすでに下書きを絶対に
                // 失わないことを composer の主たる約束としている｡
                //
                // `window.blur()` で落とすのではなく timeline へ戻す (#118)｡
                // フォーカスパスが空になると `Timeline` コンテキストへ手が
                // 届かなくなり､次のクリックまで `esc` がショートカットと
                // メニューバーの半分を無効にしていた｡
                window.focus(&this.focus_handle);
            }))
            .on_action(cx.listener(|_this, _: &ShowAbout, window, cx| {
                // レシーバは待たずに落とす｡ボタンは 1 つしかないので､どれが
                // 押されたかは何の情報も運ばない｡`App` まで届く必要がある
                // アクションは `Quit` のほうで､こちらはウィンドウへ届けば
                // よいだけなので､残りと一緒にここへ置いてある｡
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
                // #21: バーのクリックハンドラへキーボードから届く経路｡無料だ
                // — タイマーがすでに払った取得を見せるだけで､無いときは何も
                // しない｡
                this.apply_pending(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleFollowNewPosts, _window, cx| {
                // #22: メニューバーはチェックマークを描けないので､切り替えが
                // どちらへ倒れたかは､リロード完了時に使うバナーで報告する —
                // 失敗ではないほうのバリアントなので `Outcome` を使う｡
                this.follow = this.follow.flipped();
                let outcome = if this.follow.is_following() {
                    "Following new posts."
                } else {
                    "Not following — new posts will wait behind the pill."
                };
                this.reload_notice = Some(ReloadNotice::Outcome(outcome.into()));
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ScrollToTop, _window, cx| {
                // #22: 完全にローカル — 理由は `jump_to_top` の doc に｡
                this.jump_to_top(cx);
            }))
            .on_action(cx.listener(|_this, _: &Minimize, window, _cx| {
                window.minimize_window();
            }))
            .on_action(cx.listener(|_this, _: &CloseWindow, window, _cx| {
                // ウィンドウが 1 つなのでこれはアプリを終える｡`cmd-q` と
                // まったく同じで､`cmd-q` と同様に先に確認もしない (#109)｡
                // 未送信の下書きも道連れになる — `cmd-q` が昔から持っていた
                // 危険と同じもので､これが新しく持ち込むものではない｡
                window.remove_window();
            }))
            .flex()
            .flex_col()
            .size_full()
            // #205: 手動 sync のダイアログの覆いが `absolute` で寄る先｡無いと
            // ウィンドウではなく最も近い配置済みの祖先が基準になる｡
            .relative()
            .bg(rgb(theme.bg))
            .text_color(rgb(theme.text))
            .text_size(theme::TEXT_BODY)
            .child(self.header(density, cx))
            // #54: `state` に関わらず出す — これが直す不具合はまさに､何も
            // 起きなかったかのように描かれる timeline なので､このバナーは下の
            // `body` が今何を出していようと独立して生き残らねばならない｡
            .when_some(self.session_notice.clone(), |column, message| {
                column.child(session_notice_banner("banner-session", message, theme))
            })
            // #239: 止まった auto-refresh｡上の `session_notice` とまったく
            // 同じ理屈で出す — timeline は起動時に読んだ post を出したまま
            // でいられるので､取得がもう走っていないことを言えるのはここ
            // しかない｡
            .when_some(self.auto_refresh_notice.clone(), |column, message| {
                column.child(session_notice_banner("banner-auto-refresh", message, theme))
            })
            // #57: 上の `session_notice` と同じ理屈 — クールダウンや失敗した
            // リロードは `body` とは独立に生き残らねばならない｡この時点の
            // `body` は前の post を出したままである可能性が十分にある｡
            .when_some(self.reload_notice.clone(), |column, notice| {
                column.child(reload_notice_banner(&notice, theme, oauth::unix_now()))
            })
            // #21 の "N new posts" はここに座っていた｡#206 で `body` の
            // 下端に重なる toast へ移った — 報告ではなく申し出なので､
            // バナーの列ではなく timeline の上に住む｡
            // #70: 開けなかったリンク｡上の 2 つと同じバナーの扱いで､理由も
            // 同じだ｡何も起きていないように見えるクリックこそ潰す価値のある
            // 結末で､下の timeline はそれについて何も言えない｡
            .when_some(self.open_failure.clone(), |column, message| {
                column.child(session_notice_banner(
                    "banner-open-failure",
                    SharedString::from(message),
                    theme,
                ))
            })
            // #14: 投稿は scope に関わらず OAuth を要求する — `tweet.write`
            // scope が欠けている場合は `submit_post` 自身の中で捕まえる
            // (直し方はヘッダーの "Re-authorize" ボタン)｡composer ごと隠して
            // なぜ消えたのかを知る手立てを残さない､という形は取らない｡
            .when(self.signed_in_with_oauth, |column| {
                column.child(self.composer(window, cx))
            })
            .child(self.body(cx))
            // #205: sync が今していることは footer の 1 段上｡`when_some` なので
            // 無いときは行そのものが無い｡高さ 0 の要素を置き続けるのではない｡
            .when_some(self.sync_row(), ParentElement::child)
            // #95: ステータスバー｡ヘッダーがツールバーになった今､累計の
            // リクエスト数が住んでいるのはここだ｡
            .child(self.status_bar(density))
            // #205: 手動 sync の確認｡`absolute` なので列の中で場所を取らず
            // ウィンドウ全体を覆う｡最後の子なのは重なり順のため｡
            .when_some(self.sync_dialog(cx), ParentElement::child)
    }
}
