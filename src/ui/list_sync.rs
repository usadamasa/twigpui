//! ウィンドウから見た list sync (#174): 何をしているか､それをどう伝える
//! か､そして手で 1 回始める方法｡
//!
//! `sync/` はこのアプリがフォローしているアカウントを List へミラーする｡
//! #174 までその機能はまるごとウィンドウから見えず､ウィンドウから触れ
//! なかった: ループはサインイン時に始まり､6 時間間隔で起き､
//! [`sync::notice`] が抑制しない 2 つの結果でしか何も言わなかった｡
//! 数千アカウント遅れている list を眺めている人には､追いつきが進行中な
//! のか何も起きていないのかを見分ける手立ても､1 回頼む手立ても無かった｡
//!
//! そこでこのファイルは issue が名指しする 2 つの半分を足す｡
//! [`SyncStatus`] はステータスバーが報告するもので､tick ごとの
//! [`sync::Outcome`] から更新される｡[`TimelineView::start_sync`] は
//! ループで､サインイン時だけでなく手でも始められるようになった｡
//!
//! 先例を作った [`super::auto_refresh`] と同じ並べ方だ: まず純粋な関数と
//! そのテスト､次に支払う部分のための `impl TimelineView` ブロック｡理由も
//! 同じ — status の enum と､それを書くループと､そのループを始めるボタン
//! は 1 つの機構だからだ｡
//!
//! #205 でその機構の *見せ方* が [`super::sync_row`] へ出た — 行､その
//! フェード､確認のダイアログ｡機構が 1 つであることは変わっていない｡
//! 分かれたのは問いのほうだ: ここが答えるのは「sync は何をしているか｡
//! 今 何を支払ってよいか」で､あちらが答えるのは「それを読み手にどう
//! 出すか」である｡
//!
//! # なぜ始めるのに確認が要るのか
//!
//! diff はフォローリスト全体と list のメンバー全体を読み､どちらも返った
//! アカウントごとに課金される (`x-api-budget`)｡数千フォローもあれば
//! 1 クリックで数ドルだ — このウィンドウで押せるもののうち､桁違いに
//! いちばん高い｡その場合のスキルの規則は､押す前に最悪のケースを画面に
//! 出すことで､[`sync_confirm_label`] はそのためにあり､クリック 1 回で
//! 始まらず [`super::sync_row`] のダイアログを通るのもそのためだ｡

// [`super::render`] や [`super::auto_refresh`] のような `use super::*`
// ではなく書き下す: このモジュールが名指しする `ui` の import は､clippy
// の `wildcard_imports` が列挙できる程度に少なく､だから glob を通さない｡
use super::{Context, Duration, ReloadNotice, Theme, TimelineView, log, oauth, sync};

/// ウィンドウが list sync について今知っていること (#174)｡
///
/// tick のたびにループが書き､読むのはステータスバーだけなので､sync を
/// 駆動するのではなく記述する: ここにリクエストが出るかどうかを決める
/// ものは無い｡
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SyncStatus {
    /// 走っていないし､始めることもできない — どの gate で止まっている
    /// かを持つ｡
    ///
    /// 理由こそが要点だ｡"List sync: off" は､scope より前のセッションを
    /// 持つ人にも､list を設定していない人にも同じだけしか伝えないが､
    /// 2 人が必要とするものは正反対だ｡
    Off(SyncOff),
    /// 走っていないが､クリックすれば始まる｡`auto_sync_list` が off で
    /// 他がすべて揃っているとき､ウィンドウはここに座る｡
    Ready,
    /// サインイン中の id がまだ着いていないので､フォローリストを突き
    /// 合わせる相手が無い｡[`SyncStatus::Working`] とは別物だ｡何も支払われ
    /// ておらず､何も飛んでいないから — ループは起動時の fetch を待って
    /// いて､それが失敗し続けるならここに留まる｡
    AwaitingAccount,
    /// tick が飛んでいる — read か write か､その両方｡
    Working,
    /// tick と tick の間｡`pending` はファイル上の計画がまだ負っている分｡
    /// ゼロが定常状態で､それ以外は間隔を空けられたか止められた追いつき｡
    Idle { until: i64, pending: usize },
    /// write が拒否されていて､ループは `until` まで後退している｡
    /// `pending` はまだ負っている分｡`refusals` は上限が続けて何回 no と
    /// 言ったか (#197) — 1 回なら一時停止､数回なら何時間も動いていない
    /// 追いつきで､ラベルと色がそう言う｡
    RateLimited {
        until: i64,
        pending: usize,
        refusals: u32,
    },
    /// 直前の tick が完全に失敗した — 取り消された scope､削除された list､
    /// parse できない計画ファイル｡ループはすでに丸ごと 1 interval を再試行
    /// のために取ってある｡これがあるのは､ウィンドウが最後の成功をあたかも
    /// 現在のことのように報告し続けないためだ｡
    Failed,
}

/// 止まった sync がどの gate で止まっているか — [`SyncStatus::Off`] を見よ｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SyncOff {
    /// `list_id` が無いので､ミラー先が無い｡手動で始めても越えられない
    /// 唯一の gate｡
    NoList,
    /// セッションが sync に必要な scope を持っていない
    /// ([`sync::missing_scope`])｡ヘッダーがすでに出している
    /// "Re-authorize" で､再起動なしに直る｡
    MissingScope,
    /// そもそもまだ credential が無い｡
    NotSignedIn,
}

/// ステータスバーの sync セグメントが押せるものかどうか (#174)｡
///
/// 3 つの状態が no と言い､理由はそれぞれ違う: [`SyncStatus::Off`] は
/// クリックが当たる gate が､すでに止まっている当のその gate だから｡
/// [`SyncStatus::Working`] は実行が飛んでいるから — その上に 2 つ目の
/// diff を始めるのが､これが守ろうとしている二重課金だ｡そして
/// [`SyncStatus::AwaitingAccount`] は `/me` が解決するまで突き合わせる
/// 相手が無いから｡
///
/// [`SyncStatus::RateLimited`] は *押せる*: ループはすでにその窓が明ける
/// のを待っていて､始め直してももう一度待つだけだ｡それはただだし､頼むのに
/// 無理は無い｡
pub(super) fn offers_sync(status: &SyncStatus) -> bool {
    match status {
        SyncStatus::Ready
        | SyncStatus::Idle { .. }
        | SyncStatus::RateLimited { .. }
        | SyncStatus::Failed => true,
        SyncStatus::Off(_) | SyncStatus::Working | SyncStatus::AwaitingAccount => false,
    }
}

/// sync の行が言うこと (#174, #205)｡
///
/// どの文字列も "List sync:" を前に付けて行が自分を名乗るようにする｡
/// #174 でこれを足した理由 — ステータスバーには他に無記名の数が 2 つ
/// あり､その隣の裸の "1,204 to go" は 3 つ目になってしまう — は #205 で
/// 消えた｡行は自分だけの段を持つ｡それでも前置きは残す: 22px の帯に
/// 数字が 1 つ浮いているだけでは､何の数字かを言う手がかりがどこにも
/// 無い｡
///
/// これを状態名ではなく進捗にしているのは件数だ｡6 時間の "Idle" と､
/// 1100 件の write が残っている追いつきの最中の "Idle" は､まったく違う
/// 状況に対する同じ言葉であり､それを見分けられないことこそ #174 が
/// 起票された理由だった｡
pub(super) fn sync_status_label(status: &SyncStatus, now: i64) -> String {
    match status {
        SyncStatus::Off(SyncOff::NoList) => "List sync: no list configured".to_string(),
        SyncStatus::Off(SyncOff::MissingScope) => "List sync: re-authorize to enable".to_string(),
        SyncStatus::Off(SyncOff::NotSignedIn) => "List sync: not signed in".to_string(),
        SyncStatus::Ready => "List sync: ready".to_string(),
        SyncStatus::AwaitingAccount => "List sync: waiting for your account".to_string(),
        SyncStatus::Working => "List sync: working…".to_string(),
        SyncStatus::Idle { pending: 0, .. } => "List sync: up to date".to_string(),
        SyncStatus::Idle { pending, .. } => format!("List sync: {pending} to go"),
        SyncStatus::RateLimited {
            until,
            pending,
            refusals,
        } => {
            let stuck = *refusals >= STUCK_AFTER_REFUSALS;
            let lifts = if *until <= now {
                // 期限が過ぎている｡過去の時刻をそのまま出すと､もう明けて
                // いるのにまだ待っているかのように読める｡`until` は API の
                // ヘッダー由来､`now` は時計由来で､どちらもこのコードが
                // 信じきってよいものではない — #174 の 0 下限と同じ用心を､
                // カウントダウンではなく時刻に対して置いたものだ｡
                "resuming".to_string()
            } else if stuck {
                format!("retry at {} JST", jst_hhmm(*until))
            } else {
                format!("resumes {} JST", jst_hhmm(*until))
            };
            if stuck {
                // #197: 20 時間続く "rate limited — 900s" は待っている
                // ように見えた｡連続は待っているのではなく､動いていない｡
                format!("List sync: refused {refusals}× in a row, {pending} to go — {lifts}")
            } else {
                format!("List sync: rate limited, {pending} to go — {lifts}")
            }
        }
        SyncStatus::Failed => "List sync: last attempt failed".to_string(),
    }
}

/// 手動の sync が支払うのを許す前に､確認が言うこと (#174)｡
///
/// リクエストへ広がるクリックについての `x-api-budget` の規則に従い､
/// ありそうなケースではなく最悪のケースを名指しする｡数字は名指しできない:
/// 両側にアカウントが何件あるかを知るためにこそ read があるのだし､前の
/// 計画から推測すれば､アプリが実際には知らない数字を画面に出すことになる｡
/// だから課金の形を名指しし､大きさは自分が何件フォローしているか知って
/// いる人に委ねる｡
pub(super) fn sync_confirm_label() -> &'static str {
    "Reads your whole follow list and the whole list, billed per account. Sync anyway?"
}

/// `unix` の時刻を JST の `HH:MM` で描く (#205)｡
///
/// 日付 crate は足さない｡JST は UTC+9 の固定オフセットで DST が無いので､
/// 必要なのは足し算 1 回と剰余だけだ — `log::format_utc` が同じ理由で
/// `civil_from_days` を手書きしているのと同じ判断で､あちらより桁違いに
/// 小さい｡
///
/// 日付を出さないのは､rate limit の窓が長くても数時間だからだ｡日付を
/// 足しても曖昧さは減らず､22px の行から読める文字だけが減る｡
fn jst_hhmm(unix: i64) -> String {
    /// JST は UTC より 9 時間進んでいる｡DST は無い｡
    const JST_OFFSET_SECONDS: i64 = 9 * 3_600;
    const SECONDS_PER_DAY: i64 = 24 * 3_600;

    let seconds_of_day = unix
        .saturating_add(JST_OFFSET_SECONDS)
        .rem_euclid(SECONDS_PER_DAY);
    let hour = seconds_of_day.div_euclid(3_600);
    let minute = seconds_of_day.div_euclid(60).rem_euclid(60);
    format!("{hour:02}:{minute:02}")
}

/// ステータスバーが rate limit と呼ぶのをやめて stuck と呼び始めるまでの
/// 連続拒否回数 (#197)｡2 回: 1 回の拒否は上限が仕事をし､ループが下限を
/// 待っているだけだ｡その待ちのあとの 2 回目は､上限が誰もが想定した予定
/// どおりには明けていないということだ｡
const STUCK_AFTER_REFUSALS: u32 = 2;

/// ステータスバーの sync セグメントを何色で塗るか (#174)｡
///
/// `danger` は本当に間違っている状態だけ — 失敗した tick､誰かが何かを
/// する必要のある gate､そして [`STUCK_AFTER_REFUSALS`] 回続けて拒否された
/// 追いつき (#197)｡1 回の rate limit はそこに入らない: ループが対処して
/// いるし､隣の件数はまだ正しい｡
pub(super) fn sync_status_color(status: &SyncStatus, theme: Theme) -> u32 {
    match status {
        SyncStatus::Failed
        | SyncStatus::Off(SyncOff::MissingScope)
        | SyncStatus::RateLimited {
            refusals: STUCK_AFTER_REFUSALS..,
            ..
        } => theme.danger,
        SyncStatus::Ready | SyncStatus::Idle { pending: 1.., .. } => theme.accent,
        SyncStatus::Off(_)
        | SyncStatus::AwaitingAccount
        | SyncStatus::Working
        | SyncStatus::Idle { .. }
        | SyncStatus::RateLimited { .. } => theme.text_tertiary,
    }
}

/// 終わった tick 1 回が残す [`SyncStatus`] (#174)｡
///
/// [`sync::Tick`] だけから読み取る — 結果と､それがディスクに残した状態
/// から — ので､ウィンドウは自前の記憶を持たない｡かつては 2 つ (期限と
/// 件数) を持っていて､それを tick から tick へ手渡すところで #198 は期限
/// を落とした: ループが 2 分ごとに送る間､status は毎分 "rate limited" と
/// "N to go" の間で切り替わった｡
///
/// 失敗した tick は [`SyncStatus::Failed`] だ: ループはまだ生きていて
/// 丸ごと 1 interval を自分に与えているが､ウィンドウは最後の成功を現在の
/// ものとして報告するのをやめなければならない｡
///
/// [`sync::Outcome::Diffed`] は見つけたものを吐き出しにそのまま戻って
/// くる (`wake_at` は now) ので､次の tick が同じ秒のうちに上書きする
/// idle 状態ではなく [`SyncStatus::Working`] に対応する｡
///
/// 状態が blocked で仕事が残っている最中の idle な tick は､拒否を待って
/// いる追いつきであり､[`SyncStatus::RateLimited`] と読む — 拒否した tick
/// が言ったのと同じことを､それが続く限り言う｡ループが今にも送るかの
/// ような "N to go" ではなく｡
pub(super) fn status_of(tick: &sync::Tick, now: i64) -> SyncStatus {
    let refused = |pending: usize| SyncStatus::RateLimited {
        until: tick.state.blocked_until.unwrap_or(tick.wake_at),
        pending,
        refusals: tick.state.refusals,
    };
    match &tick.outcome {
        Err(_) => SyncStatus::Failed,
        Ok(sync::Outcome::Diffed { .. }) => SyncStatus::Working,
        Ok(sync::Outcome::RateLimited { remaining, .. }) => refused(*remaining),
        Ok(sync::Outcome::Idle { pending, .. }) if *pending > 0 && tick.state.is_blocked(now) => {
            refused(*pending)
        }
        Ok(
            sync::Outcome::Idle { pending, .. }
            | sync::Outcome::Applied {
                remaining: pending, ..
            },
        ) => SyncStatus::Idle {
            until: tick.wake_at,
            pending: *pending,
        },
    }
}

/// なぜ実行が始まったか｡そしてそこから､何を飛ばしてよいか､いつ止まって
/// よいか (#174)｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SyncTrigger {
    /// サインイン時に始まるタイマー｡`config.auto_sync_list` を尊重し､
    /// ウィンドウが開いている間ずっと走る｡
    Scheduled,
    /// 誰かがステータスバーの sync セグメントを押した｡
    ///
    /// 違いは 2 つ､どちらも #174 の要点だ｡`auto_sync_list` が off でも
    /// 走る — タイマーを切って手で sync するというのが､そもそもボタンを
    /// 持つ理由だ — そして最初の tick は interval を無視する｡これが､
    /// 押したときに次の diff は 4 時間先だと報告するのではなく､実際に
    /// 何かをさせる｡
    Manual,
}

/// list sync のうち純粋になれない半分: 支払うループと､それを始める
/// ボタン (#174)｡
///
/// [`super::auto_refresh`] に倣い､子モジュールに置いた `impl` ブロック —
/// 機能全体が 1 ファイルにある理由はこのモジュールの doc を見よ｡
impl TimelineView {
    /// sync が今どの gate で止まっているか｡走れるなら `None` (#174)｡
    ///
    /// gate はもともとループの早期 return の中で確認されていた｡これは
    /// それを外へ出して､ステータスバーがどれなのかを *言える* ように
    /// するもので､issue が求めたことの半分だ｡それ以前は 3 つの失敗が
    /// ウィンドウからは同じに見えた: 何も見えない､だ｡
    ///
    /// `auto_sync_list` は意図的にそこに入れていない｡これはタイマーが
    /// 走るかを決めるのであって sync が可能かを決めるのではないし､
    /// タイマーを切ったウィンドウこそボタンがいちばん効くところだ｡
    fn sync_gate(&self) -> Option<SyncOff> {
        if self.config.list_id.is_none() {
            return Some(SyncOff::NoList);
        }
        if self.client.is_none() {
            return Some(SyncOff::NotSignedIn);
        }
        if sync::missing_scope(self.oauth_scope.as_deref()).is_some() {
            return Some(SyncOff::MissingScope);
        }
        None
    }

    /// list sync を始める: `config.list_id` のメンバーが､このアプリの
    /// フォローしているアカウントをミラーし続けるようにする｡
    ///
    /// gate は 3 つ｡どれもバナーとして上げるのではなく
    /// [`SyncStatus::Off`] 経由で報告する — どれも読み手がやったことでは
    /// ないし､頼んだ覚えの無いかもしれない機能についてのエラーメッセージ
    /// は､ウィンドウが開口一番に出すものではない｡ただし #174 以降は
    /// ステータスバーで *報告* される｡これが､off の機能と存在しないよう
    /// に見える機能との違いだ:
    ///
    /// - `list_id` が無く､ミラー先が無い､
    /// - まだ credential が無い､
    /// - sync が必要とする scope より前のセッションで､これは
    ///   [`sync::missing_scope`] が課金される read 1 回の前に捕まえる｡
    ///
    /// `config.auto_sync_list` は 4 つ目の gate で､
    /// [`SyncTrigger::Scheduled`] のときだけだ: タイマーが off なら
    /// ウィンドウは [`SyncStatus::Ready`] に座り､頼まれるのを待つ｡
    ///
    /// 自分で解決するのではなく､[`Self::start`] がすでに解決した
    /// credential を使い回す｡`oauth::resolve_credential` は refresh token
    /// を回して書き戻すので､2 つが競合すると保存されたセッションが死に
    /// かねない — 手元の access token が非常に長い実行の間に古くなること
    /// より､はるかに悪い結末だ｡後者はこのファイルの他の fetch 経路が
    /// すべてすでに受け入れている｡
    ///
    /// `self.auto_sync` への代入は走っていたループを落とすので､同じ計画
    /// ファイルを扱うループが 2 つ以上になることは無い｡手動の実行が
    /// タイマーに取って代わるのもこれだ: 同じスロットだからだ｡これが
    /// *しない* のは､すでに飛んでいる tick を止めることだ — tick は
    /// バックグラウンドスレッド上の同期的なポーリング 1 回で､落とした
    /// あとも完走する — だから手動の経路はタスクスロットではなく
    /// [`SyncStatus::Working`] で塞いである｡
    pub(super) fn start_sync(&mut self, trigger: SyncTrigger, cx: &mut Context<'_, Self>) {
        /// `timer` 1 回が待つ最長時間｡何時間も前に計算した期限を信じる
        /// のではなく､ループが時計を読み直す (そして眠ったマシンに
        /// 気づく) ようにするため｡
        const MAX_SLEEP_SECONDS: i64 = 60;
        /// tick の最短間隔｡到達するのは連続する apply バッチの間だけで､
        /// そこでの答えは本来「即座に」だ — 追いつきの最中でもループを
        /// キャンセル可能に保てるだけの間隔｡
        const MIN_SLEEP_SECONDS: i64 = 1;
        /// サインイン中の id がまだ着いていないときに待つ長さ｡
        /// `MIN_SLEEP_SECONDS` より長いのは､このループに急がせる手立てが
        /// 無いからだ: 起動時の fetch が失敗し続ければ `home_user_id` は
        /// いつまでも `None` のままで､ウィンドウの一生の間 1 秒ごとに
        /// それを見に行くのは､辛抱を装ったスピンでしかない｡
        const AWAITING_ID_SECONDS: i64 = 30;

        if let Some(off) = self.sync_gate() {
            self.show_sync(SyncStatus::Off(off), cx);
            return;
        }
        // どちらも上の `sync_gate` が確認済み｡`expect` ではなく `else`
        // で開くのは､あの関数を後で変えてもここが panic にならないように
        // するためだ｡
        let (Some(list_id), Some(client)) = (self.config.list_id.clone(), self.client.clone())
        else {
            return;
        };

        let scheduled = self.config.auto_sync_list;
        if matches!(trigger, SyncTrigger::Scheduled) && !scheduled {
            self.show_sync(SyncStatus::Ready, cx);
            return;
        }

        let paths = self.paths.clone();
        let interval = self.config.sync_interval_seconds;
        let prune_limit = self.config.sync_prune_limit_percent;
        let writes_per_batch = self.config.sync_writes_per_batch;
        log::info(&format!(
            "list sync started for {list_id} ({trigger:?}), interval {interval}s"
        ));
        self.show_sync(SyncStatus::Working, cx);

        self.auto_sync = Some(cx.spawn(async move |this, cx| {
            // ここで他に何も覚えないのは意図的だ: 期限と件数は tick が
            // 返す状態ファイルの中にあり､複製を持つことが #198 の原因
            // だった｡
            //
            // 最初の tick が消費する｡`last_diff_at: None` を `next_step`
            // は「diff は一度も走っていない」と読む｡これはまさに手動の
            // 開始が望む判断だ — そして優先順位には触らないので､生きて
            // いる rate limit は依然として勝つし､吐き出していない計画は
            // 何かを読み直す前に依然として吐き出される｡
            let mut forced = matches!(trigger, SyncTrigger::Manual);
            loop {
                // `Err` はウィンドウが消えたということで､終わった手動の
                // 実行以外にこのループが終わる唯一の理由だ｡
                let Ok(user_id) = this.update(cx, |this, _| this.home_user_id.clone()) else {
                    return;
                };

                let now = oauth::unix_now();
                let sleep_until = match user_id {
                    // 起動時の fetch がまだサインイン中の id を解決して
                    // いない｡解決するまで､フォローリストを突き合わせる
                    // 相手が無い｡
                    None => {
                        let _ = this.update(cx, |this, cx| {
                            this.show_sync(SyncStatus::AwaitingAccount, cx);
                        });
                        now.saturating_add(AWAITING_ID_SECONDS)
                    }
                    Some(user_id) => {
                        // await の前に立て､その間ずっと立てたままにする｡
                        // `offers_sync` はこの状態での開始を拒否し､その
                        // 拒否だけが､2 度目のクリックと両側の 2 度目の
                        // ページ全読みとの間に立っている — このタスクを
                        // 落としても下の tick は止まらない｡
                        let _ = this.update(cx, |this, cx| {
                            this.show_sync(SyncStatus::Working, cx);
                        });
                        let pacing = sync::Pacing {
                            interval_seconds: interval,
                            writes_per_batch,
                            forced,
                        };
                        // tick は失敗も含め自分の結果を自分でログに出す｡
                        let tick = {
                            let (paths, client, list_id) =
                                (paths.clone(), client.clone(), list_id.clone());
                            cx.background_executor()
                                .spawn(async move {
                                    sync::tick(
                                        &paths,
                                        &client,
                                        &user_id,
                                        &list_id,
                                        pacing,
                                        prune_limit,
                                        now,
                                    )
                                })
                                .await
                        };
                        // 消費するのは実際に動いた tick であって､拒否を
                        // 待っただけの tick ではない — さもないと後退中
                        // の押下が待ちに食われ､結局 interval が効いて
                        // しまう｡
                        forced = forced && matches!(tick.outcome, Ok(sync::Outcome::Idle { .. }));
                        let status = status_of(&tick, now);
                        let outcome = tick.outcome.as_ref().ok();
                        let notice = outcome.and_then(sync::notice);
                        let _ = this.update(cx, |this, cx| {
                            this.apply_tick(status, notice, cx);
                        });

                        // タイマーが off のウィンドウでの手動の実行は､
                        // やることが無くなった時点で止まる —
                        // `is_finished` は *何も負っていない* idle を
                        // 要求するので､rate limit で止まった追いつきは､
                        // 課金した diff が作った計画から立ち去るのでは
                        // なく待ち続ける｡
                        if !scheduled && sync::is_finished(outcome) {
                            let _ =
                                this.update(cx, |this, cx| this.show_sync(SyncStatus::Ready, cx));
                            return;
                        }
                        tick.wake_at
                    }
                };

                let wait = sleep_until
                    .saturating_sub(oauth::unix_now())
                    .clamp(MIN_SLEEP_SECONDS, MAX_SLEEP_SECONDS);
                cx.background_executor()
                    .timer(Duration::from_secs(u64::try_from(wait).unwrap_or(1)))
                    .await;
            }
        }));
    }

    /// `status` を画面に出す (#174)｡
    ///
    /// ループから抜き出した 1 行｡ループはこれを 4 回使っていて､その差の
    /// ぶんだけ `clippy::too_many_lines` を超えていた｡いずれにせよ名前を
    /// 付ける価値はある: `sync_status` への書き込みはすべて `notify` を
    /// 伴わなければならず､4 か所から書くループは忘れる機会が 4 回ある｡
    ///
    /// #205 以降はフェードもここが起こす｡`sync_status` への書き込みは
    /// すべてここを通るので､行の出入りが status から取り残されることは
    /// ない｡
    pub(super) fn show_sync(&mut self, status: SyncStatus, cx: &mut Context<'_, Self>) {
        self.sync_status = status;
        self.fade_sync_row(cx);
        cx.notify();
    }

    /// 終わった tick 1 回が画面に残すもの (#174) — その status と､
    /// [`sync::notice`] が口に出す価値ありと判断したもの｡
    fn apply_tick(
        &mut self,
        status: SyncStatus,
        notice: Option<String>,
        cx: &mut Context<'_, Self>,
    ) {
        // 空のスロットにだけ入れる: reload のバナーは読み手のもので､
        // 読み手が見ているクールダウンのカウントダウンを､背景タスクの
        // 知らせで置き換えてはならない｡
        if let Some(text) = notice
            && self.reload_notice.is_none()
        {
            self.reload_notice = Some(ReloadNotice::Outcome(text.into()));
        }
        self.show_sync(status, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stopped_sync_says_which_gate_it_is_stopped_at() {
        // `SyncOff` が裸の bool ではなく variant を持つ理由のすべて:
        // この 2 つは正反対の対処を必要とする｡
        assert_ne!(
            sync_status_label(&SyncStatus::Off(SyncOff::NoList), 0),
            sync_status_label(&SyncStatus::Off(SyncOff::MissingScope), 0)
        );
    }

    #[test]
    fn a_missing_scope_points_at_the_button_that_fixes_it() {
        let label = sync_status_label(&SyncStatus::Off(SyncOff::MissingScope), 0);
        assert!(label.contains("authorize"), "{label}");
    }

    // #174 が起票された理由の区別: 6 時間の "idle" と､1100 件の write を
    // まだ負っている "idle" は同じ状況ではないし､言葉だけでは見分けが
    // つかない｡
    #[test]
    fn an_idle_sync_with_work_left_says_how_much() {
        assert_eq!(
            sync_status_label(
                &SyncStatus::Idle {
                    until: 0,
                    pending: 1_100
                },
                0
            ),
            "List sync: 1100 to go"
        );
    }

    #[test]
    fn an_idle_sync_with_nothing_left_says_so_instead_of_a_zero() {
        let label = sync_status_label(
            &SyncStatus::Idle {
                until: 0,
                pending: 0,
            },
            0,
        );
        assert!(label.contains("up to date"), "{label}");
        assert!(
            !label.contains('0'),
            "a bare zero reads as a broken count: {label}"
        );
    }

    // #197: 20 時間続く "rate limited — 900s" は待っていると読めた｡連続は
    // 上限が明けていないということで､ラベルと色がそう言わなければならない｡
    // 文言そのものは #205 で JST の時刻になった —
    // `a_repeated_refusal_keeps_its_count_and_still_names_the_hour` を見よ｡
    #[test]
    fn a_repeated_refusal_reads_as_stuck() {
        let stuck = SyncStatus::RateLimited {
            until: 8_200,
            pending: 2_157,
            refusals: 3,
        };
        assert!(sync_status_label(&stuck, 1_000).contains("refused 3×"));
        let theme = Theme::light();
        assert_eq!(sync_status_color(&stuck, theme), theme.danger);
    }

    #[test]
    fn a_single_refusal_is_a_pause_not_a_problem() {
        let paused = SyncStatus::RateLimited {
            until: 1_900,
            pending: 2_157,
            refusals: 1,
        };
        let theme = Theme::light();
        assert_ne!(sync_status_color(&paused, theme), theme.danger);
        assert!(!sync_status_label(&paused, 1_000).contains("refused"));
    }

    #[test]
    fn every_label_identifies_which_number_it_is() {
        // ステータスバーはすでに無記名の件数を 2 つ載せている｡自分を
        // 名乗らない 3 つ目は､その隣では読めない｡
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
            SyncStatus::Idle {
                until: 0,
                pending: 7,
            },
            SyncStatus::RateLimited {
                until: 0,
                pending: 7,
                refusals: 1,
            },
            SyncStatus::RateLimited {
                until: 0,
                pending: 7,
                refusals: 4,
            },
            SyncStatus::Failed,
        ] {
            let label = sync_status_label(&status, 0);
            assert!(label.starts_with("List sync:"), "{label}");
        }
    }

    // 二重課金の防護｡diff は両側を全部読むので､走っている tick の上に
    // 2 つ目を始めるのは､このウィンドウがしうるいちばん高い間違いだ｡
    #[test]
    fn a_sync_already_working_is_not_offered_again() {
        assert!(!offers_sync(&SyncStatus::Working));
    }

    #[test]
    fn a_sync_stopped_at_a_gate_is_not_offered() {
        assert!(!offers_sync(&SyncStatus::Off(SyncOff::NoList)));
        assert!(!offers_sync(&SyncStatus::Off(SyncOff::MissingScope)));
        assert!(!offers_sync(&SyncStatus::Off(SyncOff::NotSignedIn)));
    }

    #[test]
    fn a_sync_waiting_on_the_signed_in_id_is_not_offered() {
        assert!(!offers_sync(&SyncStatus::AwaitingAccount));
    }

    #[test]
    fn an_idle_or_ready_sync_is_offered() {
        assert!(offers_sync(&SyncStatus::Ready));
        assert!(offers_sync(&SyncStatus::Idle {
            until: 0,
            pending: 0
        }));
    }

    // ループがすでに待っている窓へ始め直すのはただだし､それを頼むのは
    // 望んで無理の無いことだ｡
    #[test]
    fn a_rate_limited_sync_is_still_offered() {
        assert!(offers_sync(&SyncStatus::RateLimited {
            until: 0,
            pending: 7,
            refusals: 1,
        }));
    }

    #[test]
    fn a_failed_sync_is_offered_so_it_can_be_retried() {
        assert!(offers_sync(&SyncStatus::Failed));
    }

    /// ループから見た tick｡`state` の既定は穏やかな状態で､テストは status
    /// が関わるフィールドを設定する｡
    fn tick(outcome: Result<sync::Outcome, anyhow::Error>, wake_at: i64) -> sync::Tick {
        sync::Tick {
            outcome,
            state: sync::SyncState::default(),
            wake_at,
        }
    }

    #[test]
    fn a_failed_tick_stops_the_window_reporting_the_last_success() {
        let failed = tick(Err(anyhow::anyhow!("403 Forbidden")), 9_000);
        assert_eq!(status_of(&failed, 1_000), SyncStatus::Failed);
    }

    // diff はそのまま仕事へ戻ってくる (`wake_at` は now) ので､ここで idle
    // な status にしても同じ秒のうちに上書きされる｡
    #[test]
    fn a_diff_leaves_the_status_working_because_the_drain_is_next() {
        let diffed = tick(
            Ok(sync::Outcome::Diffed {
                adds: 3,
                removals: 1,
                members_total: 100,
                held: false,
            }),
            1_000,
        );
        assert_eq!(status_of(&diffed, 1_000), SyncStatus::Working);
    }

    #[test]
    fn a_batch_with_more_to_send_reports_what_is_left() {
        let applied = tick(
            Ok(sync::Outcome::Applied {
                sent: 2,
                remaining: 340,
            }),
            1_060,
        );
        assert_eq!(
            status_of(&applied, 1_000),
            SyncStatus::Idle {
                until: 1_060,
                pending: 340
            }
        );
    }

    // "working" が誤りになる唯一の瞬間: 追いつきが止まるとき｡
    #[test]
    fn the_last_batch_of_a_catch_up_reports_it_is_done() {
        let applied = tick(
            Ok(sync::Outcome::Applied {
                sent: 2,
                remaining: 0,
            }),
            1_000,
        );
        assert_eq!(
            status_of(&applied, 1_000),
            SyncStatus::Idle {
                until: 1_000,
                pending: 0
            }
        );
    }

    #[test]
    fn an_idle_tick_carries_its_pending_count_into_the_status() {
        let idle = tick(
            Ok(sync::Outcome::Idle {
                until: 9_000,
                pending: 12,
            }),
            9_000,
        );
        assert_eq!(
            status_of(&idle, 1_000),
            SyncStatus::Idle {
                until: 9_000,
                pending: 12
            }
        );
    }

    // 拒否した tick の status は､それが残した状態から来る — 後退が選んだ
    // 期限と連続回数だ — 結果自身が最初に当てた再試行時刻からではない｡
    #[test]
    fn a_refusal_reports_the_deadline_and_streak_from_the_state() {
        let mut refused = tick(
            Ok(sync::Outcome::RateLimited {
                until: 1_900,
                opaque: true,
                sent: 0,
                remaining: 2_157,
            }),
            4_600,
        );
        refused.state = sync::SyncState {
            last_diff_at: Some(500),
            blocked_until: Some(4_600),
            paused_until: None,
            refusals: 3,
        };
        assert_eq!(
            status_of(&refused, 1_000),
            SyncStatus::RateLimited {
                until: 4_600,
                pending: 2_157,
                refusals: 3,
            }
        );
    }

    // #198 の目に見えた症状: status が毎分 "rate limited" と "N to go" の
    // 間で切り替わった｡間に挟まる idle な起床が､拒否を待っていることを
    // 知らなかったからだ｡
    #[test]
    fn an_idle_wake_up_during_a_refusal_still_reads_as_rate_limited() {
        let mut idle = tick(
            Ok(sync::Outcome::Idle {
                until: 4_600,
                pending: 2_157,
            }),
            4_600,
        );
        idle.state = sync::SyncState {
            last_diff_at: Some(500),
            blocked_until: Some(4_600),
            paused_until: None,
            refusals: 2,
        };
        assert_eq!(
            status_of(&idle, 1_060),
            SyncStatus::RateLimited {
                until: 4_600,
                pending: 2_157,
                refusals: 2,
            }
        );
    }

    #[test]
    fn an_idle_wake_up_after_the_deadline_passed_is_plain_idle() {
        // block は明けた｡次の tick は送る｡カウントダウンするものは無い｡
        let mut idle = tick(
            Ok(sync::Outcome::Idle {
                until: 9_000,
                pending: 12,
            }),
            9_000,
        );
        idle.state.blocked_until = Some(900);
        assert_eq!(
            status_of(&idle, 1_000),
            SyncStatus::Idle {
                until: 9_000,
                pending: 12
            }
        );
    }

    #[test]
    fn an_idle_wake_up_with_nothing_owed_is_never_rate_limited() {
        // 失敗した tick は､空かもしれない計画とともに `blocked_until` を
        // 立てたまま残す｡それは待ちであって､拒否された追いつきではない｡
        let mut idle = tick(
            Ok(sync::Outcome::Idle {
                until: 22_600,
                pending: 0,
            }),
            22_600,
        );
        idle.state.blocked_until = Some(22_600);
        assert!(matches!(
            status_of(&idle, 1_000),
            SyncStatus::Idle { pending: 0, .. }
        ));
    }

    #[test]
    fn the_confirmation_says_what_the_click_will_be_billed_for() {
        let label = sync_confirm_label();
        assert!(label.contains("per account"), "{label}");
    }

    // --- #205: JST の解除予定 ---

    /// カウントダウンの秒数ではなく時刻を出す｡秒数は数時間続く block では
    /// 読めない — #197 の "20 時間続く 900s" と同じ読み違いだ｡
    #[test]
    fn a_rate_limit_says_when_it_lifts_in_jst() {
        // unix 0 は UTC の 1970-01-01 00:00､JST では同じ日の 09:00｡
        let label = sync_status_label(
            &SyncStatus::RateLimited {
                until: 0,
                pending: 40,
                refusals: 1,
            },
            -1,
        );
        assert_eq!(
            label,
            "List sync: rate limited, 40 to go — resumes 09:00 JST"
        );
    }

    #[test]
    fn a_repeated_refusal_keeps_its_count_and_still_names_the_hour() {
        let label = sync_status_label(
            &SyncStatus::RateLimited {
                until: 8_200,
                pending: 2_157,
                refusals: 3,
            },
            1_000,
        );
        assert_eq!(
            label,
            "List sync: refused 3× in a row, 2157 to go — retry at 11:16 JST"
        );
    }

    /// 過ぎた期限を過去の時刻として出すと､明けているのに待っているように
    /// 読める｡`until` は API のヘッダー由来なので信じきれない｡
    #[test]
    fn a_deadline_already_passed_reads_as_resuming_rather_than_a_past_hour() {
        let label = sync_status_label(
            &SyncStatus::RateLimited {
                until: 900,
                pending: 40,
                refusals: 1,
            },
            1_000,
        );
        assert!(label.ends_with("resuming"), "{label}");
        assert!(label.contains("40 to go"), "{label}");
    }
}
