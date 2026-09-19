//! ウィンドウ下端の帯 (#95, #241): `status_bar`｡timeline そのものは
//! `layout.rs`､1 行の post は `post_row.rs`｡
//!
//! header (上端の toolbar) は #282 で撤去した｡そこに居た要素はそれぞれ
//! 行き先を持つ: source picker はメニューバーの `Sources` メニューへ､
//! auto-refresh のカウントダウンと reload のアイコンはこの `status_bar` へ､
//! Re-authorize は `layout.rs` の `notice_banners` へ､サインインは body の
//! `sign-in-body` pill へ｡
//!
//! `ui/mod.rs` にあったものをそのまま移した｡

use super::*;

impl TimelineView {
    /// footer の reload アイコンが今どう見えるべきか (#57, #282)｡`None` は
    /// アイコンをまったく出さない — session がまだ無い
    /// (`NotAuthenticated`) かサインイン中で､それを進める手段は body の
    /// pill だけだからだ (#282 の前は同じ状態を header のサインインボタンが
    /// 描いていた)｡
    fn primary_action_state(&self) -> Option<(String, bool)> {
        // #57: `state` の match に畳み込まず､その手前で判定する — post が
        // すでに出ている間の進行中の reload は `state` を `Loaded` のままに
        // する (`reload_start_state` を見よ) ので､その場合に fetch が走って
        // いることを示す信号はこれだけである｡
        if self.reloading {
            return Some(("Loading…".to_string(), true));
        }
        match self.state {
            TimelineState::Loading => Some(("Loading…".to_string(), true)),
            TimelineState::SigningIn | TimelineState::NotAuthenticated => None,
            // クリックし直しても (ネットワーク不要の) rate-limit 判定が
            // 走り直るだけだ — #10 が禁じるのは window を寝て過ごすことで､
            // 安い判定の再実行ではない｡
            TimelineState::RateLimited { reset_at, cooldown } => {
                Some((cooldown_label(cooldown, reset_at, oauth::unix_now()), true))
            }
            TimelineState::Loaded(_) | TimelineState::Failed(_) => {
                Some(("Reload".to_string(), false))
            }
        }
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

    /// footer の reload アイコン (#282)｡この操作は不変で頻繁で､どのアプリも
    /// 共有する記号で名指されるので､枠付きのボタンに書き下すと毎フレームの
    /// 隅が timeline より騒がしくなった｡言うことのある状態 ("Loading…"､
    /// rate limit のカウントダウン) のために `label` は今も在るが､それらは
    /// すでに `body` と #57 のバナー経由で読み手に届くので､ここではアイコンを
    /// 暗くするだけである｡
    fn primary_action_control(
        &self,
        label: &str,
        busy: bool,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = self.theme;
        icon_button(
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
        .on_click(cx.listener(|this, _event, _window, cx| this.reload(ReloadTrigger::Polling, cx)))
        .into_any_element()
    }

    /// ウィンドウの下端に沿う帯 (#95)｡
    ///
    /// #95 まではリクエスト数がウィンドウのタイトルの下に居て､毎フレーム
    /// 最初に読まれる座をアカウント名と奪い合っていた｡macOS はウィンドウの
    /// 累計を代わりに status bar に置く — Finder の項目数が同じ考えだ —
    /// ので､こちらもそこへ置く｡#18 の段階的な色づけは移動しても変わらない:
    /// 数は今も `daily_post_budget` へ近づけば `warning` になり､超えれば
    /// `danger` になる｡#162 でここが見せる数字自体が Posts の resource 数へ
    /// 変わった (`usage_label` の doc を見よ) — 色付けの規則自体は変わって
    /// いない｡
    ///
    /// #282: header の撤去に伴い､reload の値段 (`×N`) とアイコン､
    /// auto-refresh のカウントダウンもここへ移ってきた｡置き場所の理由は
    /// `countdown` のモジュール doc の「置き場所と幅」を見よ｡
    pub(super) fn status_bar(
        &self,
        density: countdown::Density,
        bg_alpha: u8,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let theme = self.theme;
        let primary_action = self.primary_action_state();

        // #162: Posts の resource 数を常に出す; 見積り金額 (USD､常に
        // 設定されている単価から) を隣に添える (`usage_label` の doc を
        // 見よ)｡
        let usage_status =
            usage::budget_status(self.usage_totals.today, self.config.daily_post_budget);
        let usage_text = usage_label(
            self.usage_totals.today,
            self.usage_totals.total,
            self.config.post_resource_price,
        );
        // #214, #282: 次の sync と次の auto-refresh､両方の期限をここで
        // 決める｡`countdown` が計算し､それぞれ無ければ出さない｡
        let (next_refresh, next_sync) = self.countdown_labels(oauth::unix_now(), density);

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
            .bg(rgba(theme::with_alpha(theme.bg_header, bg_alpha)))
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
            // なれず､代わりに右側のクラスタ (auto-refresh の期限や reload
            // アイコン) がウィンドウの外へ押し出される｡`truncate` は切れた
            // 側に "…" を出す — 読めない数字より､読めていないと分かる
            // ほうがよい (`countdown` のモジュール doc)｡
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
            // #282: 保持数の区画 (`status-kept`) が消えたので､右側の
            // クラスタ (auto-refresh の期限 / reload の値段 / reload
            // アイコン) が `ml_auto` を引き継いで右端へ寄る｡3 つとも
            // 出ないことがある (countdown はループが無ければ `None`､
            // `×N` は source が 1 つなら出ない､reload アイコンは
            // `NotAuthenticated`/`SigningIn` の間出ない) ので 1 つの
            // 箱に包む — 中身が空でも `ml_auto` だけの空の div が残る
            // だけで害は無い｡
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .ml_auto()
                    .when_some(next_refresh, |cluster, label| {
                        cluster.child(
                            div()
                                .addressable("auto-refresh-countdown")
                                // #156: HIG の "Keep actions with text
                                // labels separate" — 記号 (reload) との
                                // 間隔を帯の `gap_3` (12px) よりさらに
                                // 広げる｡
                                .mr_1()
                                .text_size(theme::TEXT_META)
                                .text_color(rgb(theme.text_tertiary))
                                .child(label),
                        )
                    })
                    .children(self.reload_cost_control())
                    // #282: NotAuthenticated / SigningIn のあいだ session
                    // を進める手段は body の `sign-in-body` pill だけだ｡
                    // ここで reload のアイコンを出すと､押しても何も起き
                    // ないボタンが並んでしまう —
                    // `primary_action_state` がその間 `None` を返す理由｡
                    .when_some(primary_action, |cluster, (label, busy)| {
                        cluster.child(self.primary_action_control(&label, busy, cx))
                    }),
            )
    }
}
