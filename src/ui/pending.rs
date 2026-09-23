//! ポーリングが取ってきたものを､読み手を邪魔せずに画面へ届ける (#21)｡
//!
//! [`super::auto_refresh`] から切り出した｡あちらはいつポーリングし､いつ
//! やめるかを持つ｡こちらは終わったポーリングの答えを受け取ってからの
//! こと — マージ済みの timeline を [`Pending`] バッファに預け､pill で
//! 件数を差し出し､押されたら画面へ合流させ､古くなったら捨てる｡読み手が
//! 最上部にいるときにバッファを飛ばす経路 (#22) は [`super::follow`] の
//! 担当で､ここからはそれを呼ぶだけだ｡
//!
//! `impl` より上はすべて純粋で､gpui 抜きでユニットテストできる｡

// `use super::*` ではなく書き下す｡理由は `super::auto_refresh` の前置きと同じ｡
use super::auto_refresh::{Poll, halting_reason};
use super::follow::{at_top, follows};
use super::{
    Context, SharedString, TimelineItem, TimelineState, TimelineView, lane, log, newly_arrived,
};

/// ポーリングが取ってきた､まだ読み手に見せていない post (#21)｡
#[derive(Debug)]
pub(super) struct Pending {
    /// ポーリングが返してきたマージ済み timeline の全体で､新しい行だけ
    /// ではない — `cache::reload_primary` はキャッシュと新しいバッチを
    /// 継ぎ合わせて返すし､読み手が求めたときに表示すべきなのはその結合
    /// 済みのリストだ｡新しい行だけを持っていたら､"Load older" が足した
    /// ものをすべて落としてしまう｡
    pub items: Vec<TimelineItem>,
    /// そのうち画面にあるものと比べて新しいのが何件か｡pill が数えるのは
    /// これで､ゼロには決してならない — [`pending_after_poll`] を見よ｡
    pub count: usize,
}

/// 終わったポーリングが読み手のために残すもの｡
///
/// `None` はポーリングが新着を見つけなかったということで､これが普通の
/// 結果であり､画面をまったく触らないでいなければならない: pill も
/// バナーも scroll も無し｡数分おきに "no new posts" と報告するポーリング
/// は読み手が頼んでいないノイズだ｡自分で押した reload (#141) はそう言う
/// が､それはまさに答えを待っているからで､こちらとは違う｡
///
/// 数え方は [`newly_arrived`] — 手動 reload 自身の件数と scroll の補正が
/// 使うのと同じ先頭連続の規則なので､pill が押して実際に現れるより多くの
/// post を約束することは決してない｡
pub(super) fn pending_after_poll(
    displayed: &[&str],
    incoming: Vec<TimelineItem>,
) -> Option<Pending> {
    let incoming_ids: Vec<&str> = incoming.iter().map(|item| item.id.as_str()).collect();
    let count = newly_arrived(displayed, &incoming_ids);
    if count == 0 {
        return None;
    }
    Some(Pending {
        items: incoming,
        count,
    })
}

/// pill が言うこと｡
///
/// "(s)" ではなく単数形と複数形を書き分ける｡これは
/// `reload_policy::reload_outcome_label` に合わせたもので､意図的に
/// それと同じように読めるようにしてある: 2 つは post が届きうる 2 つの
/// 方向から同じ事実を報告しているので､読み手はいま自分がどちらを見て
/// いるのかに気づかされる必要が無い｡
pub(super) fn pending_label(count: usize) -> String {
    match count {
        1 => "1 new post".to_string(),
        n => format!("{n} new posts"),
    }
}

/// 届け方のうち純粋になれない半分: ポーリングの答えをウィンドウがどう
/// 扱うかと､バッファが空になる 2 通りの経路 (#21)｡子モジュールは親の
/// 非公開項目を見られるので､`TimelineView` のフィールドは `ui` に閉じた
/// ままでよい｡
impl TimelineView {
    /// 終わったポーリングがウィンドウに対してすること (#21)｡
    ///
    /// 意図的に静かだ｡ポーリングは読み手が頼んだものではないので､画面を
    /// 取ってはいけない: `state` は触らない､scroll 位置も触らない､
    /// そして `reload_notice` — カウントダウンを含め､読み手自身の最後の
    /// reload のものだ — をここで書くことは決してない｡成功したポーリングに
    /// できるのは `pending` を埋めることだけで､それを差し出すのは pill､
    /// 他に動くものは無い｡
    ///
    /// 失敗したポーリングはもっと何もしない: ログに出して捨てる｡reload の
    /// 経路がバナーを上げるのは､答えを聞こうと待っている人がいるからだ｡
    /// こちらを待っている人はいないし､数分前のネットワークの瞬断は､
    /// 問題無い timeline の上に赤い行を出すほどのものではない｡`usage` は
    /// どちらにせよ更新する — parse できたかどうかに関わらず､リクエストは
    /// 送られて課金されている｡
    ///
    /// `next_page_token` は意図的に更新しない｡これは "Load older" の
    /// カーソルで､読み手がどこまで遡ったかを表す｡背後で取った先頭ページ
    /// は､scroll の途中でそれを巻き戻してしまう｡
    ///
    /// 例外は 1 つだけで､#239 が足した: [`halting_reason`] が「次の 1 回も
    /// 同じ答えだ」と言う拒否なら､ループを止めてバナーを出す ([`Poll::Halt`])｡
    /// 上の「黙って捨てる」が守っているのは一時的な失敗で読み手を煩わせない
    /// ことであり､**取得が止まったこと自体を隠すこと** ではない｡
    pub(super) fn apply_poll(
        &mut self,
        result: anyhow::Result<lane::ReloadOutcome>,
        cx: &mut Context<'_, Self>,
    ) -> Poll {
        self.refresh_usage(cx);
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(error) => {
                // `log::redact` は出ていく途中で走る — API のエラーは
                // それを生んだリクエストを引用しうる｡
                log::error(&format!("auto-refresh poll failed: {error:#}"));
                let Some(reason) = halting_reason(&error) else {
                    return Poll::Continue;
                };
                log::warn(&format!("auto-refresh stopped: {reason}"));
                self.auto_refresh_notice = Some(SharedString::from(reason));
                // #214: 来ないポーリングを数え続けない｡
                self.refresh_situation = None;
                cx.notify();
                return Poll::Halt;
            }
        };
        // #43: `outcome.me` は常に解決済み (`ReloadOutcome` の doc を見よ)｡
        // 部分失敗はここでは無視して静かに続ける — `apply_poll` の doc が
        // 言うとおり poll は失敗を画面に出さない｡
        //
        // ヘッダーはサインイン中のアカウントを名指しし､いくつかの操作は
        // その id を必要とする｡ポーリングはどちらもただで解決するので､
        // 起動時の fetch が埋められなかったなら､ここで埋めてしまってよい｡
        self.home_user_id = Some(outcome.me.id.clone());
        self.home_username = Some(outcome.me.username);

        let composed = lane::load_composite_timeline(&self.paths, &self.sources, &outcome.me.id);
        self.item_provenance = composed.provenance;

        let displayed: Vec<&str> = match &self.state {
            TimelineState::Loaded(items) => items.iter().map(|item| item.id.as_str()).collect(),
            _ => Vec::new(),
        };
        let Some(pending) = pending_after_poll(&displayed, composed.items) else {
            // 新着無し｡notice すら出さない — このメソッドの doc を見よ｡
            return Poll::Continue;
        };
        self.present_poll(pending, cx);
        Poll::Continue
    }

    /// ポーリングの新着 post が画面上で何になるか (#21, #22): 流し込みか､
    /// 差し出しか｡どちらかを決めるのは [`follows`] — スイッチを入れたまま
    /// 最上部にいる読み手には [`Self::follow`]､それ以外には pill｡そして
    /// ポーリングは決して画面を取らないという [`Self::apply_poll`] の doc
    /// は､その人たちにはそのまま一言一句当てはまる｡
    ///
    /// pill のバッファのために画像を先読みすることはしない｡
    /// `refresh_avatars`/`refresh_media` は何が足りないかを `self.state`
    /// を読んで決めるし､どちらも代入するとキャンセルされる単一の task
    /// スロットを持つ — バッファの画像を先にダウンロードするなら､別の
    /// ところから読むよう教えるか､表示中の timeline 自身のダウンロードを
    /// タイマーでキャンセルするかのどちらかになる｡[`Self::apply_pending`]
    /// は行が実際に画面に出た瞬間に取る｡手動 reload がすでに持っているのと
    /// 同じ経路､同じ短いプレースホルダーだ｡
    pub(super) fn present_poll(&mut self, pending: Pending, cx: &mut Context<'_, Self>) {
        let (top_item, offset_in_item) = self.list_scroll.logical_scroll_top();
        let loaded = matches!(self.state, TimelineState::Loaded(_));
        if follows(self.follow, loaded, at_top(top_item, offset_in_item)) {
            self.follow(pending, cx);
        } else {
            self.pending = Some(pending);
            cx.notify();
        }
    }

    /// 最後のポーリングが取ってきたものを届ける (#21)｡
    ///
    /// バッファがあれば [`Self::follow`] を呼ぶだけ｡読み手が最上部にいる
    /// ときに poll がそのまま流れ込むのと同じ経路で､同じ glide に合流する
    /// — 見せろと言われた post を最上部へ跳ばすのではなく､追いついて
    /// くる見た目にする｡
    ///
    /// バッファが空なら早期 return する｡2 回押しても何も起きない (#21)
    /// のはこの return のためで､`state` を無条件に置き換える形に書き直す
    /// と 2 回目の押下で画面が空になってしまう｡
    ///
    /// `ReloadNotice::Outcome` は上げない: pill がすでに何件あったかを
    /// 言っているし､pill が消えた瞬間にその件数を繰り返すバナーは､同じ
    /// 事実を 2 度言うことになる｡
    pub(super) fn apply_pending(&mut self, cx: &mut Context<'_, Self>) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        self.follow(pending, cx);
    }

    /// ポーリングが待たせていたものを捨てる (#21)｡
    ///
    /// バッファより新しい source から `state` を置き換える経路すべてから
    /// 呼ばれる: 終わった reload､終わった "Load older"､削除､サインイン｡
    /// 古いバッファは単に時代遅れなのではなく､作業を巻き戻す形で誤って
    /// いる — 削除の前に取ったものを適用すれば削除した post が画面へ戻り､
    /// "Load older" の前に取ったものは､いま足したばかりのページを落とす｡
    ///
    /// 件数も誤る: もう表示されていない timeline を基準に測ったものなので､
    /// pill はすでに見えている post を約束することになる｡
    ///
    /// glide も同じ古さのために捨てる (#22): その offset は置き換えられる
    /// 行を基準に測ったものなので､歩かせ続ければ古い距離のぶんだけ新しい
    /// リストを scroll してしまう｡toast の countdown も同じ行を数えたもの
    /// なので一緒に捨てる (#206)｡
    pub(super) fn clear_pending(&mut self) {
        self.pending = None;
        self.glide = None;
        self.unseen = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str) -> TimelineItem {
        TimelineItem {
            id: id.to_string(),
            text: format!("post {id}"),
            created_at: None,
            author_name: String::new(),
            author_username: "someone".to_string(),
            reposted_by: None,
            quoted: None,
            replied_to: None,
            metrics: None,
            links: Vec::new(),
            author_avatar_url: None,
            original_post_id: None,
            media: Vec::new(),
        }
    }

    #[test]
    fn a_poll_that_brought_nothing_new_leaves_nothing_waiting() {
        let displayed = ["3", "2", "1"];
        let incoming = vec![item("3"), item("2"), item("1")];

        assert!(pending_after_poll(&displayed, incoming).is_none());
    }

    #[test]
    fn a_poll_that_brought_new_posts_counts_them() {
        let displayed = ["3", "2", "1"];
        let incoming = vec![item("5"), item("4"), item("3"), item("2"), item("1")];

        let pending = pending_after_poll(&displayed, incoming).expect("two posts arrived");
        assert_eq!(pending.count, 2);
    }

    // バッファはマージ済みリストの全体なので､適用してもポーリングが取った
    // 下に "Load older" が足したページを落とすことは無い｡
    #[test]
    fn the_pending_buffer_holds_the_whole_timeline_not_just_the_new_rows() {
        let displayed = ["3", "2", "1"];
        let incoming = vec![item("4"), item("3"), item("2"), item("1")];

        let pending = pending_after_poll(&displayed, incoming).expect("one post arrived");
        assert_eq!(pending.count, 1);
        assert_eq!(pending.items.len(), 4);
    }

    // 数えるのは先頭の連続だけで､手動 reload の数え方とまったく同じだ —
    // もっと下にある id は移動した post であって到着した post ではないし､
    // pill は押しても現れない post を約束してはならない｡
    #[test]
    fn only_the_leading_run_of_new_ids_is_counted() {
        let displayed = ["2", "1"];
        let incoming = vec![item("4"), item("2"), item("3"), item("1")];

        let pending = pending_after_poll(&displayed, incoming).expect("one post arrived");
        assert_eq!(pending.count, 1);
    }

    // まだ画面に何も無いウィンドウ (失敗した起動､空のリスト) は､
    // ポーリングが持ち帰ったものをすべて新着として扱う｡実際そうだからだ｡
    #[test]
    fn everything_is_new_when_nothing_is_displayed_yet() {
        let pending =
            pending_after_poll(&[], vec![item("2"), item("1")]).expect("two posts arrived");
        assert_eq!(pending.count, 2);
    }

    #[test]
    fn one_new_post_is_not_reported_in_the_plural() {
        assert_eq!(pending_label(1), "1 new post");
    }

    #[test]
    fn several_new_posts_are() {
        assert_eq!(pending_label(6), "6 new posts");
    }
}
