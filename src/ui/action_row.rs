//! 一つの post の action の帯 (#156): reply / repost / like / quote / open /
//! delete｡`post_row.rs` から切り出した — あちらがサイズの天井に近づいた
//! ので､行の中で `self` の状態を読む描画メソッドのうち action 周りだけを
//! こちらへ移した｡振る舞いは変えていない純粋な移動｡

use super::*;

impl TimelineView {
    /// `post_id` について描く like ボタンの状態 (#68) — これが倣っている
    /// [`Self::repost_state_for`] を見よ｡
    pub(super) fn like_state_for(&self, post_id: &str) -> ToggleState {
        self.like_overrides
            .get(post_id)
            .cloned()
            .unwrap_or_else(|| ToggleState::new(self.liked_ids.contains(post_id)))
    }

    /// `post_id` を削除する前に確認を求める (#72) — 二段構えの一度目の
    /// クリック｡他の行の確認待ちを置き換えるので､削除まであと一クリックの
    /// post は常に一つだけである｡
    fn ask_to_delete(&mut self, post_id: String, cx: &mut Context<'_, Self>) {
        self.delete_failures.remove(&post_id);
        self.pending_delete = Some(post_id);
        cx.notify();
    }

    /// 何も削除せずに delete の確認を引っ込める (#72)｡
    fn cancel_delete(&mut self, cx: &mut Context<'_, Self>) {
        self.pending_delete = None;
        cx.notify();
    }

    /// 一つの post の delete の affordance (#72): "Delete"､あるいは
    /// クリックされたあとの確認の二つ組｡直近の試みが失敗していれば､その
    /// 理由も添える｡
    fn delete_row(&self, item: &TimelineItem, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = self.theme;
        let asking = self.pending_delete.as_deref() == Some(item.id.as_str());

        let controls = if asking {
            let confirm_id = item.id.clone();
            div()
                .flex()
                .gap_3()
                .child(
                    div()
                        .addressable(format!("delete-confirm-{}", item.id))
                        .text_color(rgb(theme.danger))
                        .child("Delete permanently")
                        .on_click(cx.listener(move |this, _event, _window, cx| {
                            this.confirm_delete(confirm_id.clone(), cx);
                        })),
                )
                .child(
                    div()
                        .addressable(format!("delete-cancel-{}", item.id))
                        .text_color(rgb(theme.text_muted))
                        .child("Cancel")
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.cancel_delete(cx);
                        })),
                )
        } else {
            let ask_id = item.id.clone();
            div().child(
                div()
                    .addressable(format!("delete-{}", item.id))
                    .text_color(rgb(theme.text_muted))
                    .child("Delete")
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.ask_to_delete(ask_id.clone(), cx);
                    })),
            )
        };

        match self.delete_failures.get(&item.id) {
            Some(message) => div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_color(rgb(theme.danger)).child(message.clone()))
                .child(controls)
                .into_any_element(),
            None => controls.into_any_element(),
        }
    }

    /// 一つの post の like/unlike の toggle (#68)｡[`offers_like`] が `item`
    /// について許すときに描く｡
    fn like_button(&self, item: &TimelineItem, cx: &mut Context<'_, Self>) -> AnyElement {
        // #52: 行は自分自身の id を鍵にするが (行ごとに一意なので､一つの
        // 原文への二つの repost が要素として衝突しない)､リクエストが作用
        // するのは原文のほうである｡
        let target = action_post_id(item);
        let state = self.like_state_for(target);
        like_row(&item.id, target, &state, self.theme, cx)
    }

    /// `post_id` について描くボタンの状態 (#15): このセッションがすでに
    /// 知っていること (進行中､失敗､あるいは完了したリクエストが確定させた
    /// 値) があればそれ｡無ければ `refresh_reposted_ids` が最後に読んだ
    /// ローカルの記録の素の on/off の値｡
    pub(super) fn repost_state_for(&self, post_id: &str) -> ToggleState {
        self.repost_overrides
            .get(post_id)
            .cloned()
            .unwrap_or_else(|| ToggleState::new(self.reposted_ids.contains(post_id)))
    }

    /// 一つの post の repost/un-repost の toggle (#15)｡[`offers_repost`] が
    /// `item` について許すときに描く｡
    fn repost_button(&self, item: &TimelineItem, cx: &mut Context<'_, Self>) -> AnyElement {
        // #52: 要素の id は行から､リクエストの対象は原文から取る｡
        let target = action_post_id(item);
        let state = self.repost_state_for(target);
        repost_row(&item.id, target, &state, self.theme, cx)
    }

    /// 一つの post のすべての action を横一行に (#95)｡
    ///
    /// どの action が現れるかは変わっていない — 決めるのは今も各 `offers_*`
    /// の述語である — が､今は一行一つずつ積み上がって別の metrics 行の上に
    /// 並ぶのではなく､engagement の件数を脇に添えて横に並ぶ｡リクエストが
    /// 失敗した like/repost は今もそのメッセージを描き､その行についてはこの
    /// 帯が下へ伸びる; それは `like_row`/`repost_row` 自身の仕業で､ここでは
    /// そのままにしてある｡
    pub(super) fn action_row(
        &self,
        item: &TimelineItem,
        counts: &RowCounts,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = self.theme;

        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_4()
            .text_size(theme::TEXT_META)
            .text_color(rgb(theme.text_muted))
            // #71: "Reply" — composer の対象を設定する; 下書きが submit
            // されるまで何も送られない｡
            .when(offers_reply(self.signed_in_with_oauth, item), |row| {
                row.child(with_count(
                    reply_row(item, theme, cx),
                    counts.replies.as_deref(),
                    theme,
                ))
            })
            // #15: repost/un-repost — どの post に付くかは `offers_repost`
            // の doc をきっちり見よ｡
            .when(
                offers_repost(
                    self.signed_in_with_oauth,
                    self.home_user_id.as_deref(),
                    self.home_username.as_deref(),
                    item,
                ),
                |row| {
                    row.child(with_count(
                        self.repost_button(item, cx),
                        counts.reposts.as_deref(),
                        theme,
                    ))
                },
            )
            // #68: like/unlike — どの post に付くかは `offers_like` の doc を
            // 見よ｡repost と違い､これは自分自身の post にも提示される｡
            .when(
                offers_like(
                    self.signed_in_with_oauth,
                    self.home_user_id.as_deref(),
                    item,
                ),
                |row| {
                    row.child(with_count(
                        self.like_button(item, cx),
                        counts.likes.as_deref(),
                        theme,
                    ))
                },
            )
            // #16: "Quote" — どの post に付くかは `offers_quote` の doc を
            // きっちり見よ (repost の行が控えられるのは､`offers_repost` が
            // 自分のボタンを控えるのと同じ理由による)｡
            .when(offers_quote(self.signed_in_with_oauth, item), |row| {
                row.child(quote_row(item, theme, cx))
            })
            // #70: post そのものを x.com で開く｡
            .child(open_post_link(item, theme, cx))
            // #72: delete — 自分の post のみ､そして決して一クリックでは行わない｡
            .when(
                offers_delete(
                    self.signed_in_with_oauth,
                    self.home_user_id.as_deref(),
                    self.home_username.as_deref(),
                    item,
                ),
                |row| row.child(self.delete_row(item, cx)),
            )
            .into_any_element()
    }
}
