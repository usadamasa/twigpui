//! ウィンドウが list sync をどう *見せ*､どう差し出すか (#205)｡
//!
//! [`super::list_sync`] は sync が何をしているかを持つ — [`SyncStatus`]､
//! それを書くループ､その言葉づかい｡このファイルはウィンドウ側の半分だ:
//! その status が画面のどこに､どれだけの間､どれだけ濃く出るか､そして
//! 1 回手で始めるための入口｡
//!
//! #174 では両方が 1 つのファイルの､画面上でも 1 つの要素だった｡footer の
//! 区画がラベルであり同時にボタンで､sync が何もしていない間もそこに座り
//! 続けていた｡#205 がそれを 3 つに分ける:
//!
//! - **行** — footer の 1 段上｡出るのは読み手が知る必要のあることが
//!   あるときだけで ([`wants_sync_row`])､出入りはフェードする
//!   ([`RowFade`])｡
//! - **入口** — footer に残る｡文言は状態によらず動かない｡
//! - **ダイアログ** — 入口を押すと開く｡どの list へ書くか､何が課金され
//!   るか､前の実行が残した計画があるかを言う｡
//!
//! 形は [`super::auto_refresh`] と [`super::list_sync`] に倣う: まず純粋な
//! 関数とそのテスト､次に `impl TimelineView` ブロック｡
//!
//! # なぜ [`super::list_sync`] と別のファイルなのか
//!
//! #174 の module doc は「status の enum と､それを書くループと､そのループを
//! 始めるボタンは 1 つの機構だ」と言った｡それは今も正しい｡ここで分けたのは
//! 機構ではなく､問いだ｡あちらが答えるのは「sync は何をしているか｡今 何を
//! 支払ってよいか」で､こちらが答えるのは「それを読み手にどう出すか」｡
//! [`super::reload_policy`] と [`super::render`] が分かれているのと同じ線で､
//! この線を引かずに #205 を足すと [`super::list_sync`] は 1,000 行近くに
//! なった｡

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

/// sync の行を今 出したいかどうか (#205)｡
///
/// #174 は sync の文言を footer に常設した｡走っていないときも､始められ
/// ないときも､やることが無いときも同じ場所を占め続けたので､読み手は
/// 「変わった」と「ずっとそこにある」を見分けられなかった｡#205 の答えは
/// 行を一時的なものにすることだ: 読み手が知る必要のあることがあるときだけ
/// 出す｡
///
/// 出さない側にも理由がある｡[`SyncStatus::Off`] の 3 つはどれも読み手が
/// やったことではないし､どれも他の場所に直し方がある — 欠けた scope なら
/// ヘッダーの "Re-authorize"､list 未設定なら toolbar の picker､未サイン
/// インならサインインのボタンだ｡[`SyncStatus::Ready`] と
/// `Idle { pending: 0 }` は定常状態そのもので､定常状態は報告ではない｡
pub(super) fn wants_sync_row(status: &SyncStatus) -> bool {
    match status {
        // 読み取りが飛んでいる｡このウィンドウで最も高くつく処理の最中だ｡
        SyncStatus::Working
        // 手動 sync を確認した直後に落ちうる先｡出さないと押下が無反応に
        // 見える｡
        | SyncStatus::AwaitingAccount
        | SyncStatus::RateLimited { .. }
        | SyncStatus::Failed => true,
        SyncStatus::Idle { pending, .. } => *pending > 0,
        SyncStatus::Off(_) | SyncStatus::Ready => false,
    }
}

/// sync の行の出入り (#205)｡
///
/// gpui の `AnimationExt::with_animation` を使わない｡あれの時計は要素が
/// mount された瞬間から動き､完了を知らせる口が無く､経過は要素の内側にしか
/// ないのでテストから触れない｡消えるほうのフェードは要素が mount された
/// ままでなければ描けないので､どのみち「いつ外すか」を自分で持つことに
/// なる｡それなら段階そのものを自分で持つほうが素直だ｡
///
/// 時計ではなく段の数で持つ｡進めるのは [`TimelineView::fade_sync_row`] の
/// タイマーで､1 tick が 1 段だ｡これで遷移は純粋関数になり､経過時間の
/// mock も要らない｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RowFade {
    /// 行は無い｡timeline がウィンドウの下端まで使う｡
    Hidden,
    /// 行は場所を取っていて､`1..FADE_STEPS` 段だけ濃い｡
    Rising(u8),
    /// 完全に見えている｡
    Shown,
    /// 行はまだ場所を取っていて､`1..FADE_STEPS` 段だけ薄い｡
    Falling(u8),
}

/// フェードを何段で渡りきるか (#205)｡
///
/// 1 段が [`TimelineView::FADE_STEP_MILLIS`] なので､端から端まで 180ms｡
/// macOS の小さな UI の遷移とだいたい同じで､読み手が「消えた」と気づく
/// には十分に速く､点滅と読まれるには十分に遅い｡
const FADE_STEPS: u8 = 6;

/// 1 tick 進んだフェード (#205)｡
///
/// 途中で向きが変わったら 0 からやり直さず､今の濃さのまま向きだけ変える｡
/// sync の状態は 1 tick で往復しうる — `Applied` から `Idle { pending: 0 }`
/// はまさにそれだ — ので､やり直す実装は行を点滅させる｡
pub(super) fn next_fade(fade: RowFade, wants: bool) -> RowFade {
    match (fade, wants) {
        (RowFade::Hidden, false) | (RowFade::Shown, true) => fade,
        (RowFade::Hidden, true) => rising(1),
        (RowFade::Shown, false) => falling(1),
        (RowFade::Rising(step), true) => rising(step.saturating_add(1)),
        (RowFade::Falling(step), false) => falling(step.saturating_add(1)),
        // 折り返し｡`FADE_STEPS - step` が､同じ濃さを反対向きの段で言い
        // 直したものになる (下の [`fade_opacity`] を見よ)｡
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
/// これが「中間状態で timeline を跳ねさせない」の全部だ: 高さも一緒に
/// 補間すると､フェードのフレームごとに上の timeline が押し上げられ､
/// 読んでいる行が指の下で滑る｡動くのは出現と消失の各 1 回だけにする｡
pub(super) fn fade_occupies(fade: RowFade) -> bool {
    !matches!(fade, RowFade::Hidden)
}

/// これ以上 tick しても変わらないかどうか (#205)｡
///
/// タイマーを止める条件だ｡落ち着いたフェードを叩き続けるのは､何も
/// 変わらないフレームを描き続けることでしかない｡
pub(super) fn fade_settled(fade: RowFade) -> bool {
    matches!(fade, RowFade::Hidden | RowFade::Shown)
}

/// 今 sync を始められない理由 — 始められるなら `None` (#205)｡
///
/// [`offers_sync`] の裏返しに言葉を付けたものだ｡ダイアログはどの状態から
/// でも開く — 押しても何も起きないボタンは､理由を出す場所を持たない — の
/// で､始められないときは代わりにここを出して確認ボタンを出さない｡
///
/// 2 つが食い違うと､押せない確認ボタンか理由の無い拒否のどちらかが出る｡
/// テストが 1 状態ずつ突き合わせているのはそのためだ｡
pub(super) fn sync_blocked_reason(status: &SyncStatus) -> Option<&'static str> {
    match status {
        SyncStatus::Off(SyncOff::NoList) => {
            Some("No list is configured, so there is nothing to mirror into.")
        }
        SyncStatus::Off(SyncOff::MissingScope) => {
            Some("This session predates the scope sync needs. Re-authorize from the header first.")
        }
        SyncStatus::Off(SyncOff::NotSignedIn) => Some("Sign in first."),
        // 走っている diff の上に 2 つ目を重ねると両側を 2 回払う｡タスク
        // スロットは守ってくれない — tick はバックグラウンドで同期的に
        // 走りきる｡
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

/// ダイアログが名指す書き込み先 (#205)｡
///
/// 名前は所有 list のキャッシュ (#164) から引く｡アプリの中で list の名前が
/// 存在する場所はそこだけだ — timeline の fetch は list の名前を返さない
/// ので､`owned_lists` にまだ何も無いウィンドウには名乗る材料が無い｡
///
/// そこで取りに行くことはしない｡`/2/users/:id/owned_lists` は返る list
/// ごとに課金されるので (`x-api-budget`)､ダイアログを開くだけで支払う
/// ことになり､cancel した人にも請求が行く｡issue の「cancel は API request
/// を送らない」はそこも含む｡
///
/// だから名前が無ければ id で名指す｡黙るより良い — どの list へ書くのかを
/// 押す前に確かめる手立ては他に無い｡
pub(super) fn sync_target_label(name: Option<&str>, list_id: &str) -> String {
    name.map_or_else(|| format!("list {list_id}"), ToString::to_string)
}

/// 前の実行が残した plan について､ダイアログが言うこと (#205)｡
///
/// [`SyncStatus`] からは取れない｡あれの `pending` を埋めるのは tick 1 回
/// で､その tick こそこのダイアログが尋ねている当のものだ｡セッションを
/// またいで残った plan は､ディスクを読むまでウィンドウには見えない｡
///
/// 0 なら黙る｡"0 changes" は答えの無い問いではなく壊れた件数のように
/// 読める — footer の post 件数が読み込み前に出ないのと同じ理屈だ｡
pub(super) fn sync_plan_label(pending: usize) -> Option<String> {
    (pending > 0)
        .then(|| format!("A plan from an earlier run still owes {pending} membership changes."))
}

/// list sync の見せ方のうち､ウィンドウの状態に触る半分 (#205)｡
impl TimelineView {
    /// ステータスバーの sync の入口 (#174, #205)｡
    ///
    /// #174 ではこれが状態のラベルそのもので､押せる状態のときだけ押せた｡
    /// #205 が 2 つを分ける｡状態は上の [`Self::sync_row`] へ移り､ここに
    /// 残るのはダイアログを開く入口だけになった｡
    ///
    /// 入口が footer に残るのは､それがいちばん効くのが状態の行の出て
    /// いないときだからだ — タイマーを切ったウィンドウ ([`SyncStatus::Ready`])
    /// では sync は何もしておらず､だから行は出ない｡入口を行の中に置けば､
    /// 手で始めたい人にはそもそも見えない｡
    ///
    /// どの状態からでも押せる｡始められない状態でもダイアログは開き､
    /// 代わりに理由を出す ([`sync_blocked_reason`])｡押しても何も起きない
    /// ボタンには理由を出す場所が無い｡
    pub(super) fn sync_segment(&self, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = self.theme;
        // 状態ではなく操作を名乗る｡文言は状態によらず動かない — 動く
        // 文字は上の行の担当で､footer に常設されるものは常設に耐える
        // 必要がある｡
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

    /// footer の 1 段上に座る sync の行 (#205)｡出す価値が無ければ `None`｡
    ///
    /// 高さは [`theme::SYNC_ROW_HEIGHT`] 固定で､フェードの最中も変わら
    /// ない — 理由は [`fade_occupies`] を見よ｡薄れていく間もラベルは
    /// 最後の状態を出したままにする｡消えるものが道中で口をつぐめば､
    /// 何が終わったのか読む間が無い｡
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

    /// 1 tick が [`RowFade`] を進める長さ (#205)｡
    ///
    /// [`FADE_STEPS`] 段で 180ms｡`auto_refresh` の glide と同じく
    /// background executor の timer で刻む — こちらは 1 段ずつ数えるので
    /// 経過時間を読まない｡
    const FADE_STEP_MILLIS: u64 = 30;

    /// 今の [`SyncStatus`] が求める向きへフェードを歩かせる (#205)｡
    ///
    /// `TimelineView::show_sync` から呼ばれる｡`sync_status` への書き込みは
    /// すべてあそこを通るので､行の出入りが status の変化から取り残される
    /// ことはない｡
    ///
    /// すでに目的地にいるならタイマーは持たない｡落ち着いたフェードを
    /// 叩き続けるのは､何も変わらないフレームを描き続けることでしかない｡
    /// 代入し直すと前のタイマーが drop されて取り消される — `auto_sync` と
    /// 同じ契約で､向きが変わったときに 2 つが逆向きに歩くことを防いで
    /// いる｡
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
        // 1 段目はタイマーを待たずにここで踏む｡待つと最初の 30ms が何も
        // 起きないフレームになり､現れるほうは「遅れて出た」､消えるほうは
        // 「一瞬固まった」と読める｡この 1 段のおかげで､status が変わった
        // フレームには必ず行が変わっている｡
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
                    // 目的地は毎段読み直す｡歩いている途中に status が
                    // 変われば `next_fade` が今の濃さのまま向きを変える｡
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
    /// このアプリには他にダイアログが無い (#72 の削除は 2 段構えのクリック
    /// で済ませている)｡ここで作るのは､それが要る最初の場面だからだ:
    /// 出すべきことが 3 つある — どの list へ書くか､何が課金されるか､
    /// 前の実行が残した計画があるか — し､footer の 24px にはその 1 つも
    /// 入らない｡
    ///
    /// backdrop は `occlude` する｡これはクリックを吸うためだけではない:
    /// 背後の timeline に届く hover と scroll も一緒に止める｡確認の最中に
    /// 背後が動くと､どちらを読めばよいのか分からなくなる｡
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
                    // `confirm_sync` が同じ gate で撥ねるだけで､押せる
                    // 見た目のまま何も起きないボタンになる｡
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
    /// `FIXTURE_ARRIVAL_SECONDS` と同じ役どころで､同じ理由の長さだ:
    /// ウィンドウを画面に出して眺め始めるには十分に長く､消えるのを待つのが
    /// 面倒にならない程度には短く｡
    const FIXTURE_SYNC_SECONDS: u64 = 8;

    /// フィクスチャが書いた sync の状態を画面に出す (#205)｡
    ///
    /// 出現のフェードは起動と同時に見える｡消えるほうは何かが落ち着かないと
    /// 見えないので､[`Self::FIXTURE_SYNC_SECONDS`] 後に一度だけ
    /// `Idle { pending: 0 }` — 定常状態 — へ落とす｡本物の追いつきが終わる
    /// ときに通るのと同じ状態だ｡
    ///
    /// リクエストは 1 本も飛ばない｡フィクスチャのウィンドウは `client` を
    /// 持たないので､どのみちここから課金へ至る経路が無い｡そして `auto_sync`
    /// のスロットを借りるのはそのためでもある: 本物のループはフィクスチャ
    /// では決して起動しないので､取り合いにならない｡
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
    /// #174 ではこれが行の文字を確認文言に置き換える 2 段構えの 1 段目
    /// だった｡#205 でダイアログになる｡理由は変わっていない — `x-api-budget`
    /// は､リクエストへ広がるクリックは受け取られる前に最悪のケースを画面に
    /// 出さなければならないと言い､これはこのウィンドウで押せるもののうち
    /// 桁違いにいちばん高い｡変わったのは出せる場所の広さだ｡24px の帯には
    /// 一文しか入らなかったので､どの list へ書くかも､前の実行が残した
    /// 計画があることも言えなかった｡
    ///
    /// 計画の件数はここで 1 回だけディスクから読む｡[`SyncStatus`] は
    /// tick を 1 回通るまでそれを知らないし､その tick こそこのダイアログが
    /// 尋ねている当のものだ｡毎フレーム読まないのは､これがファイル 1 つの
    /// 読み取りだからで､開いた瞬間の 1 回で十分だからである｡
    ///
    /// 始められない状態でも開く｡そのときダイアログは確認ボタンの代わりに
    /// [`sync_blocked_reason`] を出す｡リクエストは飛ばない｡
    pub(super) fn ask_to_sync(&mut self, cx: &mut Context<'_, Self>) {
        self.sync_plan_pending = sync::load_plan(&self.paths.sync_plan_file())
            .ok()
            .flatten()
            .map_or(0, |plan| {
                plan.pending_count(sync::Action::Add)
                    .saturating_add(plan.pending_count(sync::Action::Remove))
            });
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
    /// [`Self::ask_to_sync`] の判断を信じるのではなく status を確認し直す
    /// — ダイアログを読んでいる間に予定された tick が始まりうるし､その隙間は
    /// 確認を読むのにかかるだけの長さがある｡
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

    /// #205 が起票された理由: 何も起きていない sync が footer に文言を
    /// 常設していた｡出すのは読み手が知る必要のあることがあるときだけだ｡
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

    /// 行は消えきるまで場所を空けない｡これが timeline を跳ねさせない
    /// ための不変条件で､高さは [`theme::SYNC_ROW_HEIGHT`] 固定である｡
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

    /// 折り返しが飛ばしてよい濃さの幅｡`1.0 - 5.0/6.0` と `1.0/6.0` は
    /// 同じ段を指すが f32 では同じ値にならないので､等値ではなく「1 段
    /// 未満しか動いていない」で押さえる｡防いでいるのは 0 からのやり直しで､
    /// それは 1 段より桁違いに大きい｡
    const FADE_SLACK: f32 = 0.01;

    /// 落ちている途中で状態が戻ったら､0 からやり直さず今の濃さから戻る｡
    /// やり直すと点滅になる — sync の状態は 1 tick で往復しうる｡
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

    /// [`offers_sync`] と反対を向いてはならない｡確認ボタンの有無はこの
    /// 2 つが決めるので､食い違えば押せないボタンか理由の無い拒否が出る｡
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

    /// 前のセッションが残した plan は `sync_status` に載っていない — それを
    /// 埋めるのは tick 1 回で､その tick こそこのダイアログが尋ねている当の
    /// ものだ｡だから件数はディスクから読み､0 なら黙る｡
    #[test]
    fn a_plan_left_over_from_an_earlier_run_is_named_in_the_dialog() {
        let label = sync_plan_label(1_204).expect("a plan with work left has to be shown");
        assert!(label.contains("1204"), "{label}");
        assert_eq!(sync_plan_label(0), None);
    }

    #[test]
    fn the_dialog_names_the_list_it_would_write_to() {
        assert_eq!(sync_target_label(Some("Rustaceans"), "1750"), "Rustaceans");
        // 名前が cache に無ければ id で名指す｡黙るよりよい — 押す人が
        // どの list か確かめる手立ては他に無い｡
        assert_eq!(sync_target_label(None, "1750"), "list 1750");
    }
}
