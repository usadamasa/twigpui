//! ウィンドウが list sync をどう見せ､どう差し出すか (#205)｡
//!
//! - **行** — footer の 1 段上｡[`wants_sync_row`] が出す状態を選び､
//!   出入りは [`RowFade`] でフェードする｡
//! - **入口** — footer に残る｡文言は状態によらず動かない｡
//! - **ダイアログ** — 入口を押すと開く｡書き込む list､課金される内容､
//!   前の実行が残した計画を出す｡
//!
//! [`super::list_sync`] との線は問いで引いてある｡あちらは「sync は何を
//! しているか｡今 何を支払ってよいか」､こちらは「それを読み手にどう出すか」｡
//! [`super::reload_policy`] と [`super::render`] の間と同じ線｡

use super::list_sync::{
    SyncOff, SyncStatus, SyncTrigger, offers_sync, sync_confirm_label, sync_status_color,
    sync_status_label,
};
use super::render::Addressable as _;
use super::{
    AnyElement, Context, Duration, FluentBuilder as _, InteractiveElement as _, IntoElement as _,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, TimelineView, div, oauth,
    rgb, rgba, sync, theme,
};

/// sync の行を今 出すかどうか (#205)｡
///
/// 出さない状態にも根拠がある｡[`SyncStatus::Off`] の 3 つはどれも直し方が
/// 他の場所にある (ヘッダーの "Re-authorize"､toolbar の picker､サインイン
/// のボタン)｡[`SyncStatus::Ready`] と `Idle { pending: 0 }` は定常状態で､
/// 定常状態は報告にならない｡
pub(super) fn wants_sync_row(status: &SyncStatus) -> bool {
    match status {
        SyncStatus::Working
        // 手動 sync を確認した直後に落ちうる先｡出さないと押下が無反応に見える｡
        | SyncStatus::AwaitingAccount
        | SyncStatus::RateLimited { .. }
        | SyncStatus::Failed => true,
        SyncStatus::Idle { pending, .. } => *pending > 0,
        SyncStatus::Off(_) | SyncStatus::Ready => false,
    }
}

/// sync の行の出入り (#205)｡時計ではなく段の数で持つ｡
///
/// gpui の `AnimationExt::with_animation` を使わない｡時計が要素の mount
/// 起点で動き､完了を知らせる口が無く､経過を要素の外から読めない｡消える
/// ほうのフェードはどのみち「いつ外すか」を自前で持つ必要がある｡
///
/// 段で持てば遷移が純粋関数になり､経過時間を mock せずに済む｡進めるのは
/// [`TimelineView::fade_sync_row`] のタイマーで､1 tick が 1 段｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RowFade {
    /// 行が無く､timeline がウィンドウの下端まで使う｡
    Hidden,
    /// 場所を取っていて､`1..FADE_STEPS` 段だけ濃い｡
    Rising(u8),
    /// 完全に見えている｡
    Shown,
    /// まだ場所を取っていて､`1..FADE_STEPS` 段だけ薄い｡
    Falling(u8),
}

/// フェードを渡りきる段数 (#205)｡
///
/// 1 段が [`TimelineView::FADE_STEP_MILLIS`] なので端から端まで 180ms｡
/// 消えたと気づくには十分に速く､点滅と読まれるには十分に遅い｡
const FADE_STEPS: u8 = 6;

/// 1 tick 進んだフェード (#205)｡
///
/// 途中で向きが変わっても 0 からやり直さず､今の濃さのまま向きだけ変える｡
/// sync の状態は 1 tick で往復しうる (`Applied` → `Idle { pending: 0 }`)
/// ので､やり直すと行が点滅する｡
pub(super) fn next_fade(fade: RowFade, wants: bool) -> RowFade {
    match (fade, wants) {
        (RowFade::Hidden, false) | (RowFade::Shown, true) => fade,
        (RowFade::Hidden, true) => rising(1),
        (RowFade::Shown, false) => falling(1),
        (RowFade::Rising(step), true) => rising(step.saturating_add(1)),
        (RowFade::Falling(step), false) => falling(step.saturating_add(1)),
        // 折り返し｡`FADE_STEPS - step` が同じ濃さを反対向きの段で言い直す
        // ([`fade_opacity`] を参照)｡
        (RowFade::Rising(step), false) => falling(FADE_STEPS.saturating_sub(step)),
        (RowFade::Falling(step), true) => rising(FADE_STEPS.saturating_sub(step)),
    }
}

/// 濃くなる途中の段｡渡りきったら [`RowFade::Shown`]｡
fn rising(step: u8) -> RowFade {
    if step >= FADE_STEPS {
        RowFade::Shown
    } else {
        RowFade::Rising(step)
    }
}

/// 薄くなる途中の段｡渡りきったら [`RowFade::Hidden`]｡
fn falling(step: u8) -> RowFade {
    if step >= FADE_STEPS {
        RowFade::Hidden
    } else {
        RowFade::Falling(step)
    }
}

/// この段の不透明度 (#205)｡
pub(super) fn fade_opacity(fade: RowFade) -> f32 {
    match fade {
        RowFade::Hidden => 0.0,
        RowFade::Shown => 1.0,
        RowFade::Rising(step) => ratio(step),
        RowFade::Falling(step) => 1.0 - ratio(step),
    }
}

/// `step` 段目が [`FADE_STEPS`] のうち占める割合｡
fn ratio(step: u8) -> f32 {
    f32::from(step) / f32::from(FADE_STEPS)
}

/// 行が場所を取っているかどうか (#205)｡
///
/// 高さは [`theme::SYNC_ROW_HEIGHT`] 固定で､フェードの最中も変わらない｡
/// 高さも補間すると 1 フレームごとに timeline が押し上げられ､読んでいる行が
/// 指の下で滑る｡動かすのは出現と消失の各 1 回だけ｡
pub(super) fn fade_occupies(fade: RowFade) -> bool {
    !matches!(fade, RowFade::Hidden)
}

/// これ以上 tick しても変わらないかどうか (#205)｡タイマーを止める条件｡
pub(super) fn fade_settled(fade: RowFade) -> bool {
    matches!(fade, RowFade::Hidden | RowFade::Shown)
}

/// 今 sync を始められない理由 — 始められるなら `None` (#205)｡
///
/// [`offers_sync`] の裏返しに言葉を付けたもの｡ダイアログはどの状態からでも
/// 開き (押しても何も起きないボタンは理由を出す場所を持たない)､始められない
/// ときは確認ボタンの代わりにこれを出す｡
///
/// 2 つが食い違うと押せない確認ボタンか理由の無い拒否が出るので､テストが
/// 1 状態ずつ突き合わせる｡
pub(super) fn sync_blocked_reason(status: &SyncStatus) -> Option<&'static str> {
    match status {
        SyncStatus::Off(SyncOff::NoList) => {
            Some("No list is configured, so there is nothing to mirror into.")
        }
        SyncStatus::Off(SyncOff::MissingScope) => {
            Some("This session predates the scope sync needs. Re-authorize from the header first.")
        }
        SyncStatus::Off(SyncOff::NotSignedIn) => Some("Sign in first."),
        // 走っている diff に 2 つ目を重ねると両側を 2 回払う｡tick は
        // バックグラウンドで同期的に走りきるので､タスクスロットは守らない｡
        SyncStatus::Working => Some("A sync is already running."),
        SyncStatus::AwaitingAccount => {
            Some("Your account is still resolving, so there is nothing to compare against yet.")
        }
        SyncStatus::Ready
        | SyncStatus::Idle { .. }
        | SyncStatus::RateLimited { .. }
        | SyncStatus::Failed => None,
    }
}

/// ダイアログが名指す書き込み先 (#205)｡名前が無ければ id｡
///
/// 名前の在処は所有 list のキャッシュ (#164) だけ｡timeline の fetch は
/// list 名を返さないので､`owned_lists` が空のウィンドウには材料が無い｡
///
/// 取りに行かない｡`/2/users/:id/owned_lists` は返る list ごとに課金される
/// ので (`x-api-budget`)､開くだけで課金され､cancel した人にも請求が行く｡
pub(super) fn sync_target_label(name: Option<&str>, list_id: &str) -> String {
    name.map_or_else(|| format!("list {list_id}"), ToString::to_string)
}

/// 前の実行が残した plan について､ダイアログが言うこと (#205)｡
///
/// [`SyncStatus`] からは取れない｡あれの `pending` を埋めるのは tick 1 回で､
/// その tick こそダイアログが尋ねている当のもの｡
///
/// 0 なら黙る｡"0 changes" は壊れた件数のように読める｡
pub(super) fn sync_plan_label(pending: usize) -> Option<String> {
    (pending > 0)
        .then(|| format!("A plan from an earlier run still owes {pending} membership changes."))
}

/// `plan` が `list_id` に対してまだ負っている件数 (#205)｡別の list なら 0｡
///
/// `sync::apply` が適用前に照合するのと同じ理由｡plan が意味を持つのは diff
/// した相手の list に対してだけで､`list_id` を変えたあとに残っていた plan は
/// 今から書き込む list について何も言っていない｡数えると､ダイアログが片方の
/// 行で新しい list を名指しながら､もう片方で古い list の残件を報告する｡
pub(super) fn plan_pending_for(plan: &sync::Plan, list_id: &str) -> usize {
    if plan.list_id != list_id {
        return 0;
    }
    plan.pending_count(sync::Action::Add)
        .saturating_add(plan.pending_count(sync::Action::Remove))
}

/// list sync の見せ方のうち､ウィンドウの状態に触る半分 (#205)｡
impl TimelineView {
    /// ステータスバーの sync の入口 (#174, #205)｡状態は [`Self::sync_row`]
    /// へ移り､ここにはダイアログを開く入口だけが残る｡
    ///
    /// 入口が footer に残るのは､いちばん効くのが行の出ていないときだから｡
    /// タイマーを切ったウィンドウ ([`SyncStatus::Ready`]) では sync が何も
    /// しておらず行が出ないので､入口を行の中に置くと見えなくなる｡
    ///
    /// どの状態からでも押せる｡始められないときはダイアログが確認ボタンの
    /// 代わりに [`sync_blocked_reason`] を出す｡
    pub(super) fn sync_segment(&self, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = self.theme;
        // 状態ではなく操作を名乗る｡動く文字は行の担当｡
        div()
            .addressable("sync-open")
            .text_color(rgb(if offers_sync(&self.sync_status) {
                theme.accent
            } else {
                theme.text_tertiary
            }))
            .child("Sync list…")
            .on_click(cx.listener(|this, _event, _window, cx| this.ask_to_sync(cx)))
            .into_any_element()
    }

    /// footer の 1 段上に座る sync の行 (#205)｡出さないなら `None`｡
    ///
    /// 高さが固定な理由は [`fade_occupies`] を参照｡薄れていく間もラベルは
    /// 最後の状態を出したままにする｡道中で口をつぐむと何が終わったのか
    /// 読む間が無い｡
    pub(super) fn sync_row(&self) -> Option<AnyElement> {
        if !fade_occupies(self.sync_fade) {
            return None;
        }
        let theme = self.theme;
        Some(
            div()
                .addressable("sync-row")
                .flex()
                .items_center()
                .h(theme::SYNC_ROW_HEIGHT)
                .px(theme::ROW_PAD_X)
                .bg(rgb(theme.bg_header))
                .border_t_1()
                .border_color(rgb(theme.border))
                .text_size(theme::TEXT_META)
                .text_color(rgb(sync_status_color(&self.sync_status, theme)))
                .opacity(fade_opacity(self.sync_fade))
                .child(sync_status_label(&self.sync_status, oauth::unix_now()))
                .into_any_element(),
        )
    }

    /// 1 tick が [`RowFade`] を進める長さ (#205)｡[`FADE_STEPS`] 段で 180ms｡
    ///
    /// `auto_refresh` の glide と同じく background executor の timer で刻む｡
    /// ただし 1 段ずつ数えるので経過時間は読まない｡
    const FADE_STEP_MILLIS: u64 = 30;

    /// 今の [`SyncStatus`] が求める向きへフェードを歩かせる (#205)｡
    ///
    /// `TimelineView::show_sync` から呼ぶ｡`sync_status` への書き込みがすべて
    /// あそこを通るので､行の出入りが status から取り残されない｡
    ///
    /// 目的地に着いていればタイマーを持たない｡代入し直すと前のタイマーが
    /// drop されて取り消される (`auto_sync` と同じ契約) ので､2 つが逆向きに
    /// 歩くこともない｡
    pub(super) fn fade_sync_row(&mut self, cx: &mut Context<'_, Self>) {
        let target = if wants_sync_row(&self.sync_status) {
            RowFade::Shown
        } else {
            RowFade::Hidden
        };
        if self.sync_fade == target {
            self.sync_fade_task = None;
            return;
        }
        // 1 段目はタイマーを待たずに踏む｡待つと最初の 30ms が何も起きない
        // フレームになり､遅れて出たように読める｡
        self.sync_fade = next_fade(self.sync_fade, wants_sync_row(&self.sync_status));
        if fade_settled(self.sync_fade) {
            self.sync_fade_task = None;
            return;
        }
        self.sync_fade_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(Self::FADE_STEP_MILLIS))
                    .await;
                // `Err` はウィンドウが消えたということ｡
                let Ok(settled) = this.update(cx, |this, cx| {
                    // 目的地は毎段読み直す｡途中で status が変われば
                    // `next_fade` が今の濃さのまま向きを変える｡
                    let wants = wants_sync_row(&this.sync_status);
                    this.sync_fade = next_fade(this.sync_fade, wants);
                    cx.notify();
                    fade_settled(this.sync_fade)
                }) else {
                    return;
                };
                if settled {
                    return;
                }
            }
        }));
    }

    /// 手動 sync の確認ダイアログ (#205)｡開いていなければ `None`｡
    ///
    /// このアプリ初のダイアログ (#72 の削除は 2 段構えのクリックで済ませて
    /// いる)｡出すことが 3 つあり — 書き込む list､課金される内容､前の実行が
    /// 残した計画 — footer の 24px にはその 1 つも入らない｡
    ///
    /// backdrop の `occlude` はクリックだけでなく背後の hover と scroll も
    /// 止める｡確認の最中に背後が動くと､どちらを読むのか分からなくなる｡
    pub(super) fn sync_dialog(&self, cx: &mut Context<'_, Self>) -> Option<AnyElement> {
        if !self.pending_sync {
            return None;
        }
        let theme = self.theme;
        let blocked = sync_blocked_reason(&self.sync_status);
        let list_id = self.config.list_id.clone().unwrap_or_default();
        let target = sync_target_label(
            self.owned_lists
                .iter()
                .find(|list| list.id == list_id)
                .map(|list| list.name.as_str())
                .filter(|name| !name.is_empty()),
            &list_id,
        );

        let panel = div()
            .addressable("sync-dialog")
            .flex()
            .flex_col()
            .gap_2()
            .w(theme::SYNC_DIALOG_WIDTH)
            .p(theme::ROW_PAD_X)
            .bg(rgb(theme.bg_header))
            .border_1()
            .border_color(rgb(theme.border))
            .rounded(theme::RADIUS_CONTROL)
            .child(
                div()
                    .text_color(rgb(theme.text))
                    .child(format!("Sync your follows into {target}?")),
            )
            .child(
                div()
                    .text_size(theme::TEXT_META)
                    .text_color(rgb(theme.text_muted))
                    .child(sync_confirm_label()),
            )
            .when_some(sync_plan_label(self.sync_plan_pending), |panel, plan| {
                panel.child(
                    div()
                        .text_size(theme::TEXT_META)
                        .text_color(rgb(theme.text_muted))
                        .child(plan),
                )
            })
            .when_some(blocked, |panel, reason| {
                panel.child(
                    div()
                        .text_size(theme::TEXT_META)
                        .text_color(rgb(theme.danger))
                        .child(reason),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_3()
                    .child(
                        div()
                            .addressable("sync-cancel")
                            .text_color(rgb(theme.text_muted))
                            .child("Cancel")
                            .on_click(
                                cx.listener(|this, _event, _window, cx| this.cancel_sync(cx)),
                            ),
                    )
                    // 始められないときは確認ボタンを出さない｡出しても
                    // `confirm_sync` が同じ gate で撥ね､押せる見た目のまま
                    // 何も起きないボタンになる｡
                    .when(blocked.is_none(), |row| {
                        row.child(
                            div()
                                .addressable("sync-confirm")
                                .text_color(rgb(theme.danger))
                                .child("Sync")
                                .on_click(cx.listener(|this, _event, _window, cx| {
                                    this.confirm_sync(cx);
                                })),
                        )
                    }),
            );

        Some(
            div()
                .addressable("sync-backdrop")
                .occlude()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(theme::SCRIM))
                // backdrop を押すのは cancel と同じ｡`x-api-budget` の側から
                // 見て安全な向きへ倒れる: 逃げ道はリクエストを送らない｡
                .on_click(cx.listener(|this, _event, _window, cx| this.cancel_sync(cx)))
                .child(panel)
                .into_any_element(),
        )
    }

    /// フィクスチャの sync 状態が落ち着くまでの時間 (#205)｡
    ///
    /// `FIXTURE_ARRIVAL_SECONDS` と同じ役どころで長さの理由も同じ｡眺め始める
    /// には十分に長く､消えるのを待つのが面倒にならない程度には短く｡
    const FIXTURE_SYNC_SECONDS: u64 = 8;

    /// フィクスチャが書いた sync の状態を画面に出す (#205)｡
    ///
    /// 出現のフェードは起動と同時に見える｡消えるほうを見せるため
    /// [`Self::FIXTURE_SYNC_SECONDS`] 後に一度だけ `Idle { pending: 0 }` へ
    /// 落とす｡本物の追いつきが終わるときに通るのと同じ状態｡
    ///
    /// リクエストは飛ばない｡フィクスチャのウィンドウは `client` を持たない｡
    /// `auto_sync` のスロットを借りられるのも同じ理由で､本物のループが
    /// フィクスチャで起動しないので取り合いにならない｡
    pub(super) fn show_fixture_sync(
        &mut self,
        fixture: &crate::fixture::FixtureSync,
        cx: &mut Context<'_, Self>,
    ) {
        let now = oauth::unix_now();
        let status = if fixture.blocked_for_seconds > 0 {
            SyncStatus::RateLimited {
                until: now.saturating_add(fixture.blocked_for_seconds),
                pending: fixture.pending,
                refusals: fixture.refusals,
            }
        } else {
            SyncStatus::Idle {
                until: now,
                pending: fixture.pending,
            }
        };
        self.show_sync(status, cx);
        self.auto_sync = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_secs(Self::FIXTURE_SYNC_SECONDS))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.show_sync(
                    SyncStatus::Idle {
                        until: oauth::unix_now(),
                        pending: 0,
                    },
                    cx,
                );
            });
        }));
    }

    /// footer の入口へのクリック (#174, #205): 支払う前に尋ねる｡
    ///
    /// `x-api-budget` は､リクエストへ広がるクリックは受け取られる前に最悪の
    /// ケースを画面に出せと言う｡これはこのウィンドウで押せるもののうち桁違いに
    /// いちばん高い｡#174 の 2 段構えのクリックがダイアログになったのは､24px の
    /// 帯に一文しか入らず､書き込む list も前の実行が残した計画も言えなかった
    /// ため｡
    ///
    /// 計画の件数はここで 1 回だけディスクから読む｡[`SyncStatus`] は tick を
    /// 1 回通るまでそれを知らず､その tick こそダイアログが尋ねている当のもの｡
    ///
    /// 始められない状態でも開き､確認ボタンの代わりに [`sync_blocked_reason`]
    /// を出す｡リクエストは飛ばない｡
    pub(super) fn ask_to_sync(&mut self, cx: &mut Context<'_, Self>) {
        let list_id = self.config.list_id.clone().unwrap_or_default();
        self.sync_plan_pending = sync::load_plan(&self.paths.sync_plan_file())
            .ok()
            .flatten()
            .map_or(0, |plan| plan_pending_for(&plan, &list_id));
        self.pending_sync = true;
        cx.notify();
    }

    /// 尋ねたのを取り消す (#174)｡リクエストは 1 本も飛ばない｡
    pub(super) fn cancel_sync(&mut self, cx: &mut Context<'_, Self>) {
        self.pending_sync = false;
        cx.notify();
    }

    /// 確認のクリック: 実行を始める (#174)｡
    ///
    /// [`Self::ask_to_sync`] の判断を信じず status を確認し直す｡ダイアログを
    /// 読んでいる間に予定された tick が始まりうる｡
    pub(super) fn confirm_sync(&mut self, cx: &mut Context<'_, Self>) {
        self.pending_sync = false;
        if !offers_sync(&self.sync_status) {
            cx.notify();
            return;
        }
        self.start_sync(SyncTrigger::Manual, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- 行を出すかどうか ---

    /// #205 が起票された理由｡何も起きていない sync が footer に文言を
    /// 常設していた｡
    #[test]
    fn a_sync_with_nothing_to_report_keeps_its_row_off_the_screen() {
        for quiet in [
            SyncStatus::Off(SyncOff::NoList),
            SyncStatus::Off(SyncOff::MissingScope),
            SyncStatus::Off(SyncOff::NotSignedIn),
            SyncStatus::Ready,
            SyncStatus::Idle {
                until: 0,
                pending: 0,
            },
        ] {
            assert!(!wants_sync_row(&quiet), "{quiet:?} has nothing to say");
        }
    }

    #[test]
    fn a_sync_that_is_doing_or_owing_or_blocked_gets_its_row() {
        for loud in [
            SyncStatus::Working,
            SyncStatus::AwaitingAccount,
            SyncStatus::Idle {
                until: 0,
                pending: 7,
            },
            SyncStatus::RateLimited {
                until: 0,
                pending: 7,
                refusals: 1,
            },
            SyncStatus::Failed,
        ] {
            assert!(wants_sync_row(&loud), "{loud:?} has something to say");
        }
    }

    /// 手動 sync を確認した直後に落ちうる先なので､無反応に見せない｡
    #[test]
    fn waiting_for_the_account_is_visible_because_a_manual_sync_lands_there() {
        assert!(wants_sync_row(&SyncStatus::AwaitingAccount));
    }

    // --- フェード ---

    #[test]
    fn an_unwanted_hidden_row_stays_hidden_and_settled() {
        assert_eq!(next_fade(RowFade::Hidden, false), RowFade::Hidden);
        assert!(fade_settled(RowFade::Hidden));
        assert!(!fade_occupies(RowFade::Hidden));
    }

    #[test]
    fn a_row_that_is_wanted_rises_from_hidden_to_shown_in_bounded_steps() {
        let mut fade = RowFade::Hidden;
        let mut seen = vec![fade_opacity(fade)];
        for _ in 0..FADE_STEPS.saturating_add(2) {
            fade = next_fade(fade, true);
            seen.push(fade_opacity(fade));
        }
        assert_eq!(fade, RowFade::Shown);
        assert!(fade_settled(fade));
        // 単調に濃くなり､両端を外れない｡
        for pair in seen.windows(2) {
            let (before, after) = (pair[0], pair[1]);
            assert!(after >= before, "the fade went backwards: {seen:?}");
            assert!((0.0..=1.0).contains(&after), "out of range: {seen:?}");
        }
    }

    #[test]
    fn a_row_that_is_no_longer_wanted_falls_all_the_way_to_hidden() {
        let mut fade = RowFade::Shown;
        for _ in 0..FADE_STEPS.saturating_add(2) {
            fade = next_fade(fade, false);
        }
        assert_eq!(fade, RowFade::Hidden);
    }

    /// 行は消えきるまで場所を空けない｡timeline を跳ねさせないための
    /// 不変条件｡
    #[test]
    fn a_falling_row_keeps_its_place_until_it_is_gone() {
        let mut fade = RowFade::Shown;
        loop {
            fade = next_fade(fade, false);
            if fade == RowFade::Hidden {
                break;
            }
            assert!(fade_occupies(fade), "{fade:?} let the timeline jump early");
        }
    }

    /// 折り返しが飛ばしてよい濃さの幅｡`1.0 - 5.0/6.0` と `1.0/6.0` は同じ
    /// 段を指すが f32 では一致しないので､等値ではなく「1 段未満しか動いて
    /// いない」で押さえる｡防ぎたい 0 からのやり直しは 1 段より桁違いに大きい｡
    const FADE_SLACK: f32 = 0.01;

    /// 落ちている途中で状態が戻ったら､0 からやり直さず今の濃さから戻る｡
    /// やり直すと点滅する｡
    #[test]
    fn a_fade_reversed_midway_resumes_from_where_it_is() {
        let falling = next_fade(RowFade::Shown, false);
        let opacity = fade_opacity(falling);
        let reversed = next_fade(falling, true);
        assert!(
            fade_opacity(reversed) + FADE_SLACK >= opacity,
            "reversing dimmed the row: {opacity} -> {}",
            fade_opacity(reversed)
        );
        assert!(fade_occupies(reversed));
    }

    #[test]
    fn a_rise_reversed_midway_resumes_from_where_it_is() {
        let rising = next_fade(RowFade::Hidden, true);
        let opacity = fade_opacity(rising);
        let reversed = next_fade(rising, false);
        assert!(
            fade_opacity(reversed) <= opacity + FADE_SLACK,
            "reversing brightened the row: {opacity} -> {}",
            fade_opacity(reversed)
        );
    }

    #[test]
    fn a_settled_fade_needs_no_further_ticks() {
        assert!(fade_settled(RowFade::Shown));
        assert!(fade_settled(RowFade::Hidden));
        assert!(!fade_settled(next_fade(RowFade::Hidden, true)));
        assert!(!fade_settled(next_fade(RowFade::Shown, false)));
    }

    // --- ダイアログ ---

    #[test]
    fn the_dialog_names_the_gate_when_sync_cannot_run() {
        assert!(sync_blocked_reason(&SyncStatus::Off(SyncOff::NoList)).is_some());
        assert!(sync_blocked_reason(&SyncStatus::Working).is_some());
        assert!(sync_blocked_reason(&SyncStatus::AwaitingAccount).is_some());
        assert!(sync_blocked_reason(&SyncStatus::Ready).is_none());
    }

    /// [`offers_sync`] と反対を向かないこと｡食い違えば押せないボタンか
    /// 理由の無い拒否が出る｡
    #[test]
    fn the_gate_the_dialog_names_is_the_gate_that_refuses_the_click() {
        for status in [
            SyncStatus::Off(SyncOff::NoList),
            SyncStatus::Off(SyncOff::MissingScope),
            SyncStatus::Off(SyncOff::NotSignedIn),
            SyncStatus::Ready,
            SyncStatus::AwaitingAccount,
            SyncStatus::Working,
            SyncStatus::Idle {
                until: 0,
                pending: 0,
            },
            SyncStatus::RateLimited {
                until: 0,
                pending: 3,
                refusals: 1,
            },
            SyncStatus::Failed,
        ] {
            assert_eq!(
                sync_blocked_reason(&status).is_none(),
                offers_sync(&status),
                "{status:?}"
            );
        }
    }

    /// 前のセッションが残した plan は `sync_status` に載らない｡だから件数は
    /// ディスクから読み､0 なら黙る｡
    #[test]
    fn a_plan_left_over_from_an_earlier_run_is_named_in_the_dialog() {
        let label = sync_plan_label(1_204).expect("a plan with work left has to be shown");
        assert!(label.contains("1204"), "{label}");
        assert_eq!(sync_plan_label(0), None);
    }

    /// 適用済みの entry は負債ではない｡中断された apply の残りだけを数える｡
    #[test]
    fn only_the_entries_still_owed_are_counted() {
        let mut plan = plan_for("1750");
        plan.entries[0].applied = true;
        assert_eq!(plan_pending_for(&plan, "1750"), 1);
    }

    /// 別の list の plan は今から書き込む list について何も言っていない｡
    #[test]
    fn a_plan_for_another_list_owes_this_one_nothing() {
        let plan = plan_for("1750");
        assert_eq!(plan_pending_for(&plan, "1750"), 2);
        assert_eq!(plan_pending_for(&plan, "9999"), 0);
    }

    /// `Add` 1 件と `Remove` 1 件を負う `list_id` の plan｡
    fn plan_for(list_id: &str) -> sync::Plan {
        sync::Plan {
            list_id: list_id.to_string(),
            created_at: 0,
            members_total: 10,
            entries: vec![
                plan_entry("1", sync::Action::Add),
                plan_entry("2", sync::Action::Remove),
            ],
        }
    }

    fn plan_entry(user_id: &str, action: sync::Action) -> sync::PlanEntry {
        sync::PlanEntry {
            user_id: user_id.to_string(),
            username: format!("user{user_id}"),
            action,
            applied: false,
        }
    }

    #[test]
    fn the_dialog_names_the_list_it_would_write_to() {
        assert_eq!(sync_target_label(Some("Rustaceans"), "1750"), "Rustaceans");
        // 名前が cache に無ければ id で名指す｡押す人がどの list か確かめる
        // 手立ては他に無い｡
        assert_eq!(sync_target_label(None, "1750"), "list 1750");
    }
}
