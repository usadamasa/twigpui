//! ウィンドウの枠 (#95, #241): 上端の toolbar (`header`) と下端の帯
//! (`status_bar`)｡timeline そのものは `layout.rs`､1 行の post は
//! `post_row.rs`｡
//!
//! `ui/mod.rs` にあったものをそのまま移した｡

use super::*;

impl TimelineView {
    pub(super) fn header(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
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
            // #95 の枠に #164 の segment: Home と所有するすべての list —
            // ウィンドウより広い picker が右側の操作 (サインイン､reload) を
            // 画面の外へ押し出すのではなく､縮んで切り取られるように包んで
            // ある｡flex item を中身が望むより狭くできるのが `min_w(0)` で､
            // これが無いと 560px で 11 個のタブが "Sign in with X" を
            // ウィンドウの外へ追い出し､body の文はそれをクリックせよと
            // 言っていた｡切り取られたタブをもっとうまく見せること
            // (スクロール､ドロップダウン) は #192 の仕事｡
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .child(self.list_picker(cx))
                    .children(self.lists_control(cx))
                    .child(header_title_element(self.home_username.as_deref(), theme)),
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
                            matches!(self.source, cache::TimelineSource::List(_)),
                        ),
                        |row| row.child(sign_in_pill("reauthorize", "Re-authorize", theme, cx)),
                    )
                    .child(self.primary_action_control(&label, busy, action, cx)),
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
    pub(super) fn status_bar(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
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
        // #214: 次の更新まで｡どちらも `countdown` が決め､無ければ出さない｡
        let (next_refresh, next_sync) = self.countdown_labels(oauth::unix_now());

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
            // #174: list sync を 1 回始める手段｡toolbar ではなくリクエスト数の
            // 隣に置くのは､同じ種類の事実 (timeline ではなくアプリについての
            // 累計) だから｡
            //
            // #205: sync が何をしているかは上の行へ移った｡ここに残るのは
            // 入口だけで､文言は状態によらず動かない｡
            //
            // この margin は､どう読めようとも行の `gap_3` と重複しては
            // いない｡ここはウィンドウで唯一､裸のテキスト span が二つ兄弟に
            // なる場所で — 他はどこも子が自分の padding を持つ — 画面上で
            // gap はそれらをまったく引き離さない: "Total: 11 req" と
            // "List sync: …" は "11 reqList sync" のようにくっついて
            // 描かれる｡gap を `gap_8` へ上げても何も変わらないので､間隔は
            // ここで実際に効くと示せる場所から来なければ
            // ならない｡
            //
            // #184: この margin は今テストの下にある｡どちらの segment にも
            // 名前が付いているので､ウィンドウのテストが配置後の bounds を
            // 読み返して､それらが接していないことを要求できる — それこそが
            // 欠陥そのもので､このコメントを書いた時点ではスクリーンショット
            // 以外に捕まえる手が無かった
            // ものである｡
            .child(
                div()
                    .addressable("status-sync")
                    .ml(theme::ROW_PAD_X)
                    .child(self.sync_segment(cx)),
            )
            // #214: 入口の隣に次の時刻｡入口とは別の要素にしてある — 入口は
            // クリックできる (`ask_to_sync`) ので､同じ要素に足すと当たりが
            // 文言の分だけ広がる｡margin は上と同じ理由で `gap` に頼らない｡
            .when_some(next_sync, |bar, label| {
                bar.child(
                    div()
                        .addressable("status-sync-next")
                        .ml(theme::ROW_PAD_X)
                        .text_color(rgb(theme.text_tertiary))
                        .child(label),
                )
            })
            // 右端は timeline についての 2 つ: 次のポーリングと､保持して
            // いる post の数｡どちらも出ないことがあるので､`ml_auto` は
            // 個々ではなく包みに付ける — 両方に付けると余白が 2 つに割れ､
            // 片方が帯の真ん中に浮く｡
            .child(
                div()
                    .flex()
                    .items_center()
                    .ml_auto()
                    .text_color(rgb(theme.text_tertiary))
                    .when_some(next_refresh, |right, label| {
                        right.child(div().addressable("status-refresh").child(label))
                    })
                    .when_some(kept, |right, kept| {
                        right.child(
                            div()
                                .addressable("status-kept")
                                .ml(theme::ROW_PAD_X)
                                .child(format!("{kept} / {} posts kept", cache::MAX_CACHED_POSTS)),
                        )
                    }),
            )
    }
}
