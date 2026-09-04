//! post の composer (#14, #241): 入力欄､quote と reply の対象の card､
//! 文字数カウンタ､submit ボタン｡入力ウィジェットの変化を `self.compose` へ
//! 写す購読もここ｡
//!
//! `ui/mod.rs` にあったものをそのまま移した｡

use super::*;

impl TimelineView {
    /// `InputEvent::Change` のたびに `compose_input` のバッファを
    /// `self.compose` へ写す (#38) — `compose.rs` がウィジェットを直接読む
    /// のではなくそもそもこの写しが在る理由は､`compose_input` フィールドの
    /// doc を見よ｡`PressEnter`/`Focus`/`Blur` はこの view に要るものを何も
    /// 運ばない: 複数行モードではウィジェット自身の中ですでに Enter が改行に
    /// なる (`InputState::enter`) ので､ここでの `PressEnter` は submit では
    /// なく素の scroll-into-view でしか発火しない｡
    // `Context::subscribe` のコールバックの境界は `&Entity<T2>` ではなく
    // `Entity<T2>` を値で要求する — こちら側で変えられるものは無い｡
    #[allow(clippy::needless_pass_by_value)]
    pub(super) fn on_compose_input_event(
        &mut self,
        input: Entity<InputState>,
        event: &InputEvent,
        cx: &mut Context<'_, Self>,
    ) {
        if let InputEvent::Change = event {
            self.compose.set_text(input.read(cx).value().to_string());
            cx.notify();
        }
    }

    /// `compose.quote()` が持っていれば､composer の中に出す quote の対象
    /// (#16)｡二つ目を作らず #13 の [`quote_card`] の描画を再利用し､その下に
    /// "Remove quote" の操作を足してある｡"Quote" の押し間違いで下書き全体を
    /// 捨てずに済むようにするためだ — それは `submit_post` ではなく必ず
    /// `ComposeState::clear_quote` を通るので､どちらにせよ下書きの本文には
    /// 手が触れない｡
    fn composer_quote_card(
        &self,
        target: &compose::QuoteTarget,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let theme = self.theme;
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(quote_card(&target.quoted, theme, None))
            .child(
                div()
                    .addressable("compose-remove-quote")
                    .text_color(rgb(theme.accent))
                    .cursor_pointer()
                    .child("Remove quote")
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.compose.clear_quote();
                        cx.notify();
                    })),
            )
    }

    /// `compose.reply()` が持っていれば､composer の中に出す reply の対象
    /// (#71)｡
    ///
    /// quote の対象と同じ [`quote_card`] の描画を使い､その上に明示的な
    /// "Replying to" の見出しを置く — card だけでは下書きが二つのどちらな
    /// のか言えないし､その違いは後からでは見えない: reply は会話の下に
    /// 着くが､quote はそうではない｡
    fn composer_reply_card(
        &self,
        target: &compose::ReplyTarget,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let theme = self.theme;
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_color(rgb(theme.text_muted))
                    .child(reply_target_label(&target.replying_to.author_username)),
            )
            .child(quote_card(&target.replying_to, theme, None))
            .child(
                div()
                    .addressable("compose-remove-reply")
                    .text_color(rgb(theme.accent))
                    .cursor_pointer()
                    .child("Remove reply")
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.compose.clear_reply();
                        cx.notify();
                    })),
            )
    }

    /// post の composer (#14): 本物のテキスト入力 (#38)､文字数カウンタ､
    /// submit ボタン｡session が OAuth でサインインしていれば出る —
    /// `tweet.write` scope が無くてもこれを丸ごと隠さない理由は
    /// [`Render::render`] の doc を見よ｡#16 で quote の対象の card が
    /// 設定されていれば加わる — [`Self::composer_quote_card`] を見よ｡
    pub(super) fn composer(
        &self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let theme = self.theme;
        let text = self.compose.text().to_string();
        let length = compose::weighted_length(&text);
        let over_limit = length > compose::MAX_WEIGHTED_LENGTH;
        let can_submit = self.compose.can_submit();
        let is_submitting = self.compose.is_submitting();
        let counter_color = if over_limit {
            theme.danger
        } else {
            theme.text_muted
        };
        // #95: カウンタと Post ボタンは入力欄が使われはじめてから現れる｡
        // 何もしていないウィンドウには､誰も書いていない post のための
        // 件数とボタンではなく､静かな一行だけが出るようにするためだ｡
        //
        // 空でない下書きがあれば focus に関わらず出しつづける｡下書きが
        // あるのにボタンを隠すと､それを送る唯一の道が入力欄をクリック
        // し直す先に隠れてしまう — #14 は下書きを決して失わないことを
        // composer の主たる約束としているのに､隠れた送信ボタンはそれを
        // 黙って破る｡
        let showing_controls = self.compose_input.focus_handle(cx).is_focused(window)
            || !text.trim().is_empty()
            || is_submitting;

        div()
            .flex()
            .flex_col()
            .gap_2()
            .px(theme::ROW_PAD_X)
            .py(theme::ROW_PAD_Y)
            .border_b_1()
            .border_color(rgb(theme.border))
            // submit が進行中の間は編集を拒む｡下の submit ボタン自身の
            // 無効状態に倣う — なぜそれが大事なのかは
            // `ComposeState::can_submit` の doc を見よ｡
            //
            // #153: カウンタとボタンを隠す条件がそのまま､入力欄を 1 行に
            // 畳む条件でもある｡畳むのは高さの上書きだけで､ウィジェットも
            // その中の下書きも作り直さない — 畳んだ状態は定義から空なので､
            // 固定の高さが `auto_grow` と喧嘩することは無い｡
            .child(
                div().addressable("compose-input").child(
                    Input::new(&self.compose_input)
                        .disabled(is_submitting)
                        .when(!showing_controls, |input| {
                            input.h(theme::COMPOSER_FOLDED_HEIGHT)
                        }),
                ),
            )
            // #16: "Quote" が設定していれば quote の対象 —
            // `composer_quote_card` の doc を見よ｡
            .when_some(self.compose.quote(), |column, target| {
                column.child(self.composer_quote_card(target, cx))
            })
            // #71: "Reply" が設定していれば reply の対象｡両方になることは
            // 決してない — `ComposeState::set_reply` を見よ｡
            .when_some(self.compose.reply(), |column, target| {
                column.child(self.composer_reply_card(target, cx))
            })
            .when_some(
                compose_error_message(self.compose.status()),
                |column, message| column.child(div().text_color(rgb(theme.danger)).child(message)),
            )
            .when(showing_controls, |composer| {
                composer.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                // #95: 本文ではなく､操作の脇に添える
                                // 読み取り値｡
                                .text_size(theme::TEXT_META)
                                .text_color(rgb(counter_color))
                                .child(format!("{length}/{}", compose::MAX_WEIGHTED_LENGTH)),
                        )
                        .child(
                            div()
                                .addressable("compose-submit")
                                .px_2()
                                .py_1()
                                .rounded(theme::RADIUS_CONTROL)
                                // #95: これは *本当に* default ボタンである
                                // — composer の存在意義そのものだ — ので､
                                // 押せる間は accent の塗りを保つ｡変えたのは
                                // もう一方の状態だ: 押せないボタンは以前
                                // 濃い灰色の塗りつぶしで､それは off の操作
                                // ではなく単に色が違う操作に見える｡macOS は
                                // 代わりに塗りを
                                // 抜く｡
                                .when(can_submit, |button| {
                                    // #156: 主ボタンと同じ blend —
                                    // `can_submit` は `is_submitting` を
                                    // 含むので "Posting…" のときは塗らない｡
                                    button
                                        .bg(rgb(theme.accent))
                                        .text_color(rgb(theme.button_label))
                                        .cursor_pointer()
                                        .hover(|style| {
                                            style.bg(rgb(theme.accent)
                                                .blend(rgba(theme.control_hover_overlay)))
                                        })
                                        .active(|style| {
                                            style.bg(rgb(theme.accent)
                                                .blend(rgba(theme.control_pressed_overlay)))
                                        })
                                })
                                .when(!can_submit, |button| {
                                    button
                                        .border_1()
                                        .border_color(rgb(theme.border))
                                        .text_color(rgb(theme.text_tertiary))
                                })
                                .text_size(theme::TEXT_META)
                                .child(if is_submitting { "Posting…" } else { "Post" })
                                // #14 の二重送信ガード､その二: submit が
                                // 進行中の間 (あるいは下書きが空か長さ超過
                                // のとき) ボタンは無効に見える見た目だけで
                                // なく､click ハンドラをそもそも持たない —
                                // `submit_post` はどのみち同じ条件を再確認
                                // するが､click がそこへ届くこと自体を
                                // 止めているのはこちらである｡
                                .when(can_submit, |button| {
                                    button.on_click(cx.listener(|this, _event, window, cx| {
                                        this.submit_post(window, cx);
                                    }))
                                }),
                        ),
                )
            })
    }
}
