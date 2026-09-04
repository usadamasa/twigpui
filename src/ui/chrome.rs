//! ウィンドウの枠 (#95, #241): 上端の toolbar (`header`) と下端の帯
//! (`status_bar`)｡timeline そのものは `layout.rs`､1 行の post は
//! `post_row.rs`｡
//!
//! `ui/mod.rs` にあったものをそのまま移した｡

use super::*;

impl TimelineView {
    pub(super) fn header(
        &self,
        density: countdown::Density,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        // #57: `state` の match に畳み込まず､その手前で判定する — post が
        // すでに出ている間の進行中の reload は `state` を `Loaded` のままに
        // する (`reload_start_state` を見よ) ので､その場合に fetch が走って
        // いることを示す信号はこれだけである｡
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
                // 今も `PrimaryAction::Reload` に繋ぐ: クリックし直しても
                // (ネットワーク不要の) rate-limit 判定が走り直るだけだ — #10 が
                // 禁じるのは window を寝て過ごすことで､安い判定の再実行ではない｡
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
        let (next_refresh, _) = self.countdown_labels(oauth::unix_now(), density);

        div()
            .flex()
            .items_center()
            .gap_3()
            // #95: 二行の masthead ではなく toolbar である｡タイトルの下に
            // 居たリクエスト数は `status_bar` へ移り､残るのは一行 — なので
            // この帯は､二行を積んだときに要る高さへ詰め物をするのではなく､
            // macOS の toolbar と同じ寸法にしてある｡
            .h(theme::TOOLBAR_HEIGHT)
            .px(theme::ROW_PAD_X)
            .bg(rgb(theme.bg_header))
            .border_b_1()
            .border_color(rgb(theme.border))
            // #95 の枠に #192/#43 の pull-down trigger: 幅は最大 160px の
            // 固定で個数に依存しないので、旧 segmented control が要った
            // `overflow_hidden` はもう trigger 自体には要らない —
            // ヘッダタイトルだけを縮められるよう内側にだけ残す｡ドロップ
            // ダウン本体は `deferred()` で画面の最前面に描かれるので、この
            // 行の `overflow_hidden` の影響は受けない｡
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(self.source_picker_trigger(cx))
                    .children(self.source_picker_menu(cx))
                    .child(
                        div()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .child(header_title_element(self.home_username.as_deref(), theme)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .ml_auto()
                    // #14: #14 より前からサインイン済みの session は
                    // `tweet.write` scope を持たない — 主ボタンが今何と
                    // 言っていようとこれが届くところに残らないかぎり､#31 の
                    // 教訓がそのまま繰り返される (すでに有効な session が
                    // 自分の格上げ経路を隠してしまう)｡
                    .when(
                        offers_reauthorize(
                            self.signed_in_with_oauth,
                            self.oauth_scope.as_deref(),
                            self.sources
                                .iter()
                                .any(|source| matches!(source, cache::TimelineSource::List(_))),
                        ),
                        |row| row.child(sign_in_pill("reauthorize", "Re-authorize", theme, cx)),
                    )
                    // #214: 次のポーリングまで｡reload のアイコンの隣に
                    // 置くのは､それがこの期限に押されるボタンだからで､
                    // footer に置くと 429px で post の数が右端から落ちる
                    // からでもある (`countdown` のモジュール doc を見よ)｡
                    .when_some(next_refresh, |row, label| {
                        row.child(
                            div()
                                .addressable("auto-refresh-countdown")
                                // #156, D10: HIG の "Keep actions with text
                                // labels separate" — 記号 (reload) との間隔を
                                // クラスタ全体の gap_2 (8px) より広げる｡
                                // クラスタを gap_3 へ上げると 3 箇所すべてが
                                // 4px 増えて 429px の余裕を食うので､この
                                // 要素だけへ mr_1 (4px) を足す｡
                                .mr_1()
                                .text_size(theme::TEXT_META)
                                .text_color(rgb(theme.text_tertiary))
                                .child(label),
                        )
                    })
                    .children(self.reload_cost_control())
                    .child(self.primary_action_control(&label, busy, action, cx)),
            )
    }

    /// #43: 選択中の source が複数のとき reload の値段を出す (`×N`) —
    /// `x-api-budget` の「押す前に最悪ケースを見せる」を守る｡1 件のときは
    /// 何も出さない (今の暗黙の 1 request のまま変える理由が無い)｡
    fn reload_cost_control(&self) -> Option<AnyElement> {
        if self.sources.len() <= 1 {
            return None;
        }
        let theme = self.theme;
        Some(
            div()
                .addressable("reload-cost")
                .text_size(theme::TEXT_META)
                .text_color(rgb(theme.text_tertiary))
                .child(format!("×{}", self.sources.len()))
                .into_any_element(),
        )
    }

    /// toolbar の唯一の action: reload､あるいはまだ session が無いときは
    /// サインイン (#95)｡
    ///
    /// 二つがまったく似ていないのは意図的だ｡reload はアイコンである —
    /// この操作は不変で頻繁で､どのアプリも共有する記号で名指されるので､
    /// 枠付きのボタンに書き下すと毎フレームの隅が timeline より騒がしく
    /// なった｡言うことのある状態 ("Loading…"､rate limit のカウントダウン)
    /// のために `label` は今も在るが､それらはすでに `body` と #57 のバナー
    /// 経由で読み手に届くので､ここではアイコンを暗くする
    /// だけである｡
    ///
    /// サインインは言葉と塗りを保つ: session が無ければウィンドウで他に
    /// できることは無いし､ラベルの無い字形は､アプリが自分を説明せねば
    /// ならないまさにその瞬間に謎かけになる｡
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
            PrimaryAction::Reload => icon_button(
                "primary-action",
                assets::RELOAD_ICON,
                if busy {
                    theme.text_tertiary
                } else {
                    theme.text_muted
                },
                if busy {
                    label.to_string()
                } else {
                    "Reload".to_string()
                },
                !busy,
                theme,
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
                    // #156: accent の上に control_hover/control_pressed を
                    // 重ねる｡`hover()` は下地を置き換えるので (D2)､合成後の
                    // 色をその場で `blend` して渡す — パレットに専用の
                    // hover 色は増やさない｡
                    button
                        .bg(rgb(theme.accent))
                        .text_color(rgb(theme.button_label))
                        .cursor_pointer()
                        .hover(|style| {
                            style.bg(rgb(theme.accent).blend(rgba(theme.control_hover_overlay)))
                        })
                        .active(|style| {
                            style.bg(rgb(theme.accent).blend(rgba(theme.control_pressed_overlay)))
                        })
                })
                .child(label.to_string())
                .on_click(on_click)
                .into_any_element(),
        }
    }

    /// ウィンドウの下端に沿う帯 (#95)｡
    ///
    /// #95 まではリクエスト数がウィンドウのタイトルの下に居て､毎フレーム
    /// 最初に読まれる座をアカウント名と奪い合っていた｡macOS はウィンドウの
    /// 累計を代わりに status bar に置く — Finder の項目数が同じ考えだ —
    /// ので､こちらもそこへ置く｡#18 の段階的な色づけは移動しても変わらない:
    /// 数は今も `daily_request_budget` へ近づけば `warning` になり､
    /// 超えれば `danger` に
    /// なる｡
    ///
    /// 保持している post の数は timeline が読み込まれてからしか出さない｡
    /// サインイン中や取得中には出せる数が無いし､"0 / 200" は答えの無い
    /// 問いではなく空の cache のように読めてしまう｡
    pub(super) fn status_bar(&self, density: countdown::Density) -> impl IntoElement {
        let theme = self.theme;

        // #18: リクエスト数は常に出す; 見積り金額は `request_price` が
        // 設定されているときだけ後ろに足す (`usage_label` の doc を
        // 見よ)｡
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
        // #214: 次の sync まで｡`countdown` が決め､無ければ出さない｡
        // auto-refresh のほうは toolbar (`header`) に居る｡
        let (_, next_sync) = self.countdown_labels(oauth::unix_now(), density);

        div()
            // #205: sync の行が「footer の 1 段上」に居ることをテストが
            // 読み返せるように名前を持つ｡帯そのものに名前が要るのは､
            // 中の区画の bounds では帯の上端が分からないからだ｡
            .addressable("status-bar")
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
            // #214: リクエスト数の隣に次の sync の時刻｡#174 から #248 までは
            // ここに "Sync list…" の入口が居て､時刻はその隣だった｡入口は
            // メニューへ移り (`menu::SyncList`)､時刻だけが残る — 同じ種類の
            // 事実 (timeline ではなくアプリについての累計) の隣なのは
            // 変わらない｡
            //
            // この margin は､どう読めようとも行の `gap_3` と重複しては
            // いない｡ここはウィンドウで唯一､裸のテキスト span が二つ兄弟に
            // なる場所で — 他はどこも子が自分の padding を持つ — 画面上で
            // gap はそれらをまったく引き離さない: "Total: 11 req" と
            // "Next sync in …" は "11 reqNext sync" のようにくっついて
            // 描かれる (#182 の "11 reqList sync" と同じ)｡gap を `gap_8` へ
            // 上げても何も変わらないので､間隔はここで実際に効くと示せる
            // 場所から来なければならない｡
            //
            // #184: この margin は今テストの下にある｡どちらの segment にも
            // 名前が付いているので､ウィンドウのテストが配置後の bounds を
            // 読み返して､それらが接していないことを要求できる — それこそが
            // 欠陥そのもので､このコメントを書いた時点ではスクリーンショット
            // 以外に捕まえる手が無かったものである｡
            //
            // 文言は `density` が幅で選ぶ｡それでも帯に入りきらないとき最初に
            // 譲るのはこれだ: `min_w(0)` が無いと flex item は中身より狭く
            // なれず､代わりに右端の post の数がウィンドウの外へ押し出される｡
            // `truncate` は切れた側に "…" を出す — 読めない数字より､読めて
            // いないと分かるほうがよい (`countdown` のモジュール doc)｡
            .when_some(next_sync, |bar, label| {
                bar.child(
                    div()
                        .addressable("status-sync-next")
                        .ml(theme::ROW_PAD_X)
                        .min_w(px(0.))
                        .truncate()
                        .text_color(rgb(theme.text_tertiary))
                        .child(label),
                )
            })
            .when_some(kept, |bar, kept| {
                bar.child(
                    div()
                        .addressable("status-kept")
                        .ml_auto()
                        .text_color(rgb(theme.text_tertiary))
                        .child(countdown::kept_label(
                            kept,
                            cache::MAX_CACHED_POSTS,
                            density,
                        )),
                )
            })
    }
}
