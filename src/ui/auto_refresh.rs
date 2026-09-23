//! ウィンドウが自分の timeline をいつポーリングし､いつやめるか (#21)｡
//!
//! [`super::reload_policy`] と同じく `ui` から切り出した (#126) が､線の
//! 引き方は違う｡あちらは判断を持ち､動くところは `ui` に任せる｡こちらは
//! 純粋な判断 *と* それに基づいて動くループを､呼び出す関数の下に
//! `impl TimelineView` ブロックとして持つ｡判断とループの片方が `ui` の
//! 他の 3000 行の下に綴じられていたら､それは誰も見つけない半分だ｡
//!
//! #21 の機構は機能の単位で 3 つのファイルに分けてあり､どれも同じ対を
//! 持つ｡ここは cadence — いつ支払い､いつ止めるか｡取ってきたものを
//! バッファと pill で届けるのは [`super::pending`]｡読み手が最上部に
//! いるとき (#22) にバッファを飛ばして流し込み､glide で見せるのは
//! [`super::follow`]｡
//!
//! 分割が今も買っているのは､そもそもの狙いだ: どのファイルも `impl` より
//! 上はすべて純粋なので､auto-refresh を安くも高くもする判断を gpui 抜きで
//! ユニットテストできる｡
//!
//! # なぜこれが `since_id` ポーリングではないのか
//!
//! #21 は home timeline 向けに書かれた｡あちらなら差分取得は `since_id`
//! ひとつで済む｡#161 がウィンドウを List に載せ替え､
//! `GET /2/lists/:id/tweets` は `since_id` をまったく受け付けない —
//! `XClient::list_timeline` を見よ｡これより安いリクエストは無い:
//! ポーリングは先頭ページを読み直すか､走らないかのどちらかだ｡
//!
//! 聞こえるほど悪くはない｡read は返った resource ごとに課金され､UTC の
//! 1 日の中で重複排除される (`x-api-budget` スキルを見よ)｡だから午後中
//! 同じ先頭ページを読み直しても､課金されるのは本当に新しかった post
//! だけで､それはどう届こうと読むのにかかる分と変わらない｡繰り返し課金
//! されるのは UTC の各深夜のあとの先頭ページ 1 回分で､`max_results` が
//! 上限になる｡
//!
//! そこでここの設計は､リクエストではなく別のところに気を遣う: リクエスト
//! が持ち帰ったもので読み手を邪魔しないことに｡ポーリングは読み手が読んで
//! いる途中のものを決して置き換えない｡マージ済みの timeline を [`Pending`]
//! バッファに預け､ウィンドウはそれを読み手が押せる件数として差し出す —
//! #21 自身の言い回しだ — ただし読み手が follow を入れたまま最上部に座って
//! いる場合 (#22) は別だ｡そこでは「読んでいるものを動かすな」と「いちばん
//! 新しいものを見せろ」は同じ指示になる｡

// `use super::*` ではなく書き下す｡[`super::pending`] と [`super::follow`]
// も同じ形にしてある: 3 つに分けたあとの各ファイルが名指しする `ui` の
// import は､clippy の `wildcard_imports` が列挙できる程度に少ない｡
use super::{
    Activity, Context, Denial, Denied, Duration, TimelineView, activity, lane, log, oauth,
    rate_limit,
};

/// 読み手が始めた fetch がまだ飛んでいるとき､tick が次に見るまで待つ
/// 長さ｡
///
/// 短いのは､これが cadence ではなく再確認だからだ｡走っている fetch は
/// どれであれ `last_reload_at` を自分の開始時刻に動かし済みなので､次の
/// tick はそこから丸ごと 1 interval を計算する｡ここで数秒待った結果として
/// 二重にポーリングすることは無い｡
const BUSY_RECHECK_SECONDS: i64 = 5;

/// 終わった 1 回のポーリングのあと､ループが続くか終わるか (#239)｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Poll {
    Continue,
    /// 繰り返しても同じ答えしか返らない拒否だった｡[`halting_reason`] を見よ｡
    Halt,
}

/// auto-refresh ループの 1 回の起床が何をすべきか｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tick {
    /// まだ期限が来ていない｡この unix 時刻まで眠って判断し直す —
    /// ループはこの期限を信じるのではなく時計を読み直すので､期限を
    /// 寝過ごしたマシンは起きたときに素直にポーリングする｡
    Wait { until: i64 },
    /// 今ポーリングに支払う｡
    Poll,
}

/// [`next_tick`] が判断の材料にするものすべて｡
#[derive(Debug, Clone, Copy)]
pub(super) struct Situation {
    /// 種類を問わず最後の fetch が出ていった時刻 — ボタン､ショートカット､
    /// 前回のポーリング｡このセッションで何も取っていなければ `None` で､
    /// 起動時にキャッシュが当たった場合がそれだ: なぜ即ポーリングせず
    /// `started_at` に落とすかは [`next_tick`] を見よ｡
    pub last_reload_at: Option<i64>,
    /// ループが始まった時刻｡最初のポーリングの起点にする｡
    ///
    /// 起床ごとに計算する「今から 1 interval 後」ではなく､固定の
    /// timestamp にする: 後者は時計とともに動くので､期限はループが
    /// 近づくのとまったく同じ速さで遠ざかり､最初のポーリングは永遠に
    /// 来ない｡
    pub started_at: i64,
    pub interval_seconds: u32,
    /// すでに fetch が飛んでいるかどうか — [`BUSY_RECHECK_SECONDS`] を見よ｡
    pub busy: bool,
    /// 読み手が画面の前にいるかどうか (#204)｡ロックされた画面の向こうに
    /// 届く post には誰も気づかないので､この 1 ビットが他のすべてに
    /// 優先する｡どう知るかは [`crate::activity`] を見よ｡
    pub activity: Activity,
    /// 読み手が戻ってきたと分かった時刻 (#204)｡ロックが解けたことに
    /// 気づいた瞬間か､マシンが sleep から戻った瞬間で､まだ一度も
    /// 離れていなければ `None`｡
    ///
    /// なぜこれが anchor に混ざるのかは
    /// [`crate::activity::Presence::resumed_at`] を見よ｡
    pub resumed_at: Option<i64>,
}

/// この起床が何をすべきか｡
///
/// 起点は `last_reload_at`､無ければ `started_at`｡これが auto-refresh を
/// *cadence* に留め､アプリがどちらの端で支払う額をも変えないようにして
/// いる:
///
/// - 手動の reload は次のポーリングを丸ごと 1 interval 先へ押しやるので､
///   ボタンを押すことが数秒後のポーリングまで買うことにはならない｡
/// - キャッシュが答えたので何も支払わなかった起動 (#9) は､その後も
///   1 interval は何も支払わない｡auto-refresh は開けっぱなしのウィンドウ
///   にリズムを足すもので､起動時の判断への second opinion ではない｡
///
/// ロックされた画面は他のすべてに優先する (#204)｡そこには開けっぱなしの
/// ウィンドウが無いので､足すリズムも無い｡`busy` より先に見るのは､
/// 飛んでいる fetch を待つ理由がそもそも無いからだ — 待った先で
/// ポーリングするわけではない｡
pub(super) fn next_tick(situation: &Situation, now: i64) -> Tick {
    if matches!(situation.activity, Activity::Away) {
        return Tick::Wait {
            until: now.saturating_add(activity::AWAY_RECHECK_SECONDS),
        };
    }
    if situation.busy {
        return Tick::Wait {
            until: now.saturating_add(BUSY_RECHECK_SECONDS),
        };
    }
    let due = poll_due_at(situation);
    if due > now {
        Tick::Wait { until: due }
    } else {
        Tick::Poll
    }
}

/// 次のポーリングが期限を迎える時刻 — [`next_tick`] が `Poll` と答え
/// 始める瞬間で､footer のカウントダウン (#214) が数えるのもこれだ｡
///
/// 起点は `last_reload_at`､無ければ `started_at`｡どちらより後でも読み手が
/// 戻ってきた時刻 (#204) が勝つ｡規則の理由は [`next_tick`] の doc にある｡
/// ここに切り出したのは､ループと footer が別々の計算を持つと､数字が 0 に
/// なってもポーリングが来ないか､来たのに数字が残るかのどちらかになる
/// からだ｡
pub(super) fn poll_due_at(situation: &Situation) -> i64 {
    situation
        .last_reload_at
        .unwrap_or(situation.started_at)
        .max(situation.resumed_at.unwrap_or(i64::MIN))
        .saturating_add(i64::from(situation.interval_seconds))
}

/// 失敗したポーリングを繰り返す意味があるか (#239)｡`Some` を返したら
/// ループはそこで終わり､返した文言が常設バナーになる｡
///
/// #239 のログはこれが無かった姿だ: 3 分ごとの 403 が 30 行､続いて 401 が
/// 100 行以上｡どれも同じ理由で拒まれ続けていて､次の 1 回が違う答えを持ち
/// 帰る見込みはどこにも無かった｡
///
/// 止めるのは **X が答えたうえで断った** 場合だけだ｡ネットワークの瞬断も
/// 5xx も普通の rate limit も `None` を返す — そのどれも次の tick には
/// 直っていておかしくないし､ポーリングの失敗は黙って捨てるという
/// [`TimelineView::apply_poll`] の約束は､そちらにはそのまま当てはまる｡
pub(super) fn halting_reason(error: &anyhow::Error) -> Option<String> {
    if let Some(denied) = error.downcast_ref::<Denied>() {
        return Some(match denied.denial {
            // 401 が生き残るのは､更新した token まで拒まれたときだけだ
            // (#239 の Session が期限前に更新する)｡つまりセッションそのもの
            // が死んでいる｡
            Denial::Rejected => "X no longer accepts this sign-in session, so auto-refresh \
                 has stopped. Click \"Sign in with X\" to start a new session."
                .to_string(),
            Denial::Forbidden => format!(
                "X refused the auto-refresh poll ({}), so it has stopped: {}. Check the \
                 monthly spend cap and this app's permissions in the X developer portal, \
                 then restart twigpui.",
                denied.endpoint.key(),
                denied.detail
            ),
        });
    }
    if let Some(expired) = error.downcast_ref::<oauth::SessionExpired>() {
        return Some(format!(
            "auto-refresh has stopped because the X sign-in session could not be renewed \
             ({}). Click \"Sign in with X\" to start a new session.",
            expired.detail
        ));
    }
    if let Some(cap) = error.downcast_ref::<rate_limit::UsageCapExceeded>() {
        return Some(format!(
            "auto-refresh has stopped because the X API credit is used up: {}. Top up the \
             balance, then restart twigpui.",
            cap.detail
        ));
    }
    None
}

/// auto-refresh のうち純粋になれない半分: リクエストに支払うループ (#21)｡
/// 答えをウィンドウがどう扱うかは [`super::pending`] の `impl` が持つ｡
///
/// [`super::reload_policy`] や [`super::render`] がデータ上の自由関数で
/// あるのと違い､子モジュールに置いた `impl` ブロックだ｡理由は上の判断と
/// それを使うループを `ui` の他のメソッドの間に割ったら､どちらの半分も
/// 単独では読めなくなるからだ｡子モジュールは親の非公開項目を見られるので､
/// `TimelineView` のフィールドは `ui` に閉じたままで､これを可能にする
/// ために何かを広げることも無い｡
impl TimelineView {
    /// ウィンドウが開いている間､タイマーで timeline に新着 post を
    /// ポーリングする (#21)｡
    ///
    /// `config.auto_refresh` が off か､取得に使う client が無いときは､
    /// 何も spawn せずに返る｡この早期 return が #21 の「切ればアプリは
    /// 何も送らない」条件のすべてだ: このメソッドの他の部分には到達
    /// できないので､発火しないと信じるべき生き残ったタイマーは存在しない｡
    ///
    /// [`Self::start`] から､そして [`Self::sign_in`] からも始まる｡
    /// `start_auto_sync` と同じ 2 か所で､理由も同じ — client はその
    /// どちらかの後にしか存在しない｡代入し直すと走っていたループは
    /// キャンセルされるので､サインインし直してもループは 2 つでなく 1 つ｡
    ///
    /// tick が何を決めるかは [`auto_refresh::next_tick`] の担当､その結果を
    /// どう扱うかは [`pending_after_poll`] の担当で､どちらも純粋で隣で
    /// テストされている｡ここに残るのは純粋になれない部分だ:
    /// リクエストに支払い､答えをどこかに置くこと｡
    pub(super) fn start_auto_refresh(&mut self, cx: &mut Context<'_, Self>) {
        /// `timer` 1 回が待つ最長時間｡マシンが眠る前に計算した期限を
        /// 信じるのではなくループが時計を読み直すため —
        /// `start_auto_sync` の定数と同じで､理由も同じ｡
        const MAX_SLEEP_SECONDS: i64 = 60;
        /// 起床の最短間隔｡ループがキャンセル可能なままでいられるように｡
        const MIN_SLEEP_SECONDS: i64 = 1;

        // #214: 前のループの期限も同じく｡早期 return より前に消すのは､
        // 止まったループの下でカウントダウンだけが残らないようにするため｡
        self.refresh_situation = None;
        if !self.config.auto_refresh {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };

        // #239: 前のループが止まった理由は､新しいループより長生きできない｡
        // ここへ来る 2 つ目の経路は再サインインで､それはまさに 401 で
        // 止まったループへの答えだ｡
        self.auto_refresh_notice = None;

        let paths = self.paths.clone();
        // #43: N ソースなら 1 tick で N request になる (`lane::reload_all`
        // が直列に呼ぶ)。`Endpoint::ListTimeline` は全 list id で 1
        // バケット共有なので、on にする本数が多いほどそのバケットを速く
        // 消費する — 上限は設けていない (ponytail: `×N` の開示で足りる)。
        let sources = self.sources.clone();
        let max_results = self.config.max_results;
        let interval_seconds = self.config.auto_refresh_interval_seconds;
        let started_at = oauth::unix_now();
        log::info(&format!(
            "auto-refresh is on, polling every {interval_seconds}s"
        ));

        self.auto_refresh = Some(cx.spawn(async move |this, cx| {
            let mut presence = activity::Presence::present();

            loop {
                // 画面がロックされているかを尋ねるのに `ioreg` を spawn
                // するので､main thread ではなく background で待つ (#204)｡
                let probed = cx
                    .background_executor()
                    .spawn(async { activity::probe() })
                    .await;
                let now = oauth::unix_now();
                let activity = presence.observe(probed, now, interval_seconds);

                // `Err` はウィンドウが消えたということで､このループが
                // 終わる唯一の理由だ — `start_auto_sync` の約束｡
                let Ok(situation) = this.update(cx, |this, _| {
                    let situation = Situation {
                        last_reload_at: this.last_reload_at,
                        started_at,
                        interval_seconds,
                        busy: this.reloading,
                        activity,
                        resumed_at: presence.resumed_at(),
                    };
                    // #214: footer がこの判断を数え直せるように写す｡
                    // `notify` はしない — 数字を進めるのは countdown の
                    // ticker で､こちらは 1 分に 1 回しか起きない｡
                    this.refresh_situation = Some(situation);
                    situation
                }) else {
                    return;
                };

                let sleep_until = match next_tick(&situation, now) {
                    Tick::Wait { until } => until,
                    Tick::Poll => {
                        // `reload` とまったく同じく､リクエストが出ていく
                        // 前に記録する: fetch はもう決まったので､返って
                        // くるかどうかに関わらず､それが次の interval を
                        // 測る起点になる｡
                        let _ = this.update(cx, |this, _| this.last_reload_at = Some(now));

                        let result = {
                            let (paths, client, sources) =
                                (paths.clone(), client.clone(), sources.clone());
                            cx.background_executor()
                                .spawn(async move {
                                    lane::reload_all(
                                        &paths,
                                        &client,
                                        &sources,
                                        max_results,
                                        oauth::unix_now(),
                                    )
                                })
                                .await
                        };

                        // #239: `Err` はウィンドウが消えたということで､
                        // 下の `Halt` と同じくループを終える｡
                        let Ok(poll) = this.update(cx, |this, cx| this.apply_poll(result, cx))
                        else {
                            return;
                        };
                        if poll == Poll::Halt {
                            return;
                        }
                        now.saturating_add(i64::from(interval_seconds))
                    }
                };

                let wait = sleep_until
                    .saturating_sub(oauth::unix_now())
                    .clamp(MIN_SLEEP_SECONDS, MAX_SLEEP_SECONDS);
                // 期限は `sleep_until` ではなくここで読み直した時計から
                // 測る (#204)｡上の clamp は待つ長さを切り詰めるので､
                // 2 つは普段から食い違っている｡
                let expected_wake_at = oauth::unix_now().saturating_add(wait);
                cx.background_executor()
                    .timer(Duration::from_secs(u64::try_from(wait).unwrap_or(1)))
                    .await;
                presence.woke(expected_wake_at, oauth::unix_now(), interval_seconds);
            }
        }));
        // #214: ループが最初に起きた瞬間から footer が数えられるように｡
        self.start_countdown_ticker(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn situation(last_reload_at: Option<i64>, started_at: i64) -> Situation {
        Situation {
            last_reload_at,
            started_at,
            interval_seconds: 300,
            busy: false,
            activity: Activity::Present,
            resumed_at: None,
        }
    }

    #[test]
    fn the_first_poll_is_one_interval_after_the_window_opened() {
        assert_eq!(
            next_tick(&situation(None, 1_000), 1_000),
            Tick::Wait { until: 1_300 }
        );
    }

    // 期限は固定の瞬間でなければならない｡起床のたびに `now` から計算して
    // いたら､ループが近づくのと同じ速さで遠ざかり､auto-refresh は決して
    // 発火しないタイマーになる｡
    #[test]
    fn the_first_polls_deadline_does_not_move_as_the_loop_waits() {
        assert_eq!(
            next_tick(&situation(None, 1_000), 1_299),
            Tick::Wait { until: 1_300 }
        );
        assert_eq!(next_tick(&situation(None, 1_000), 1_300), Tick::Poll);
    }

    #[test]
    fn a_poll_is_due_once_the_interval_since_the_last_fetch_has_elapsed() {
        assert_eq!(next_tick(&situation(Some(1_000), 500), 1_300), Tick::Poll);
    }

    #[test]
    fn a_poll_is_not_due_before_the_interval_has_elapsed() {
        assert_eq!(
            next_tick(&situation(Some(1_000), 500), 1_299),
            Tick::Wait { until: 1_300 }
        );
    }

    // #10 の interval と #21 の cadence は､reload の値段について一致して
    // いなければならない: ボタンを押すのは fetch なので､直後にポーリング
    // が続くのではなく､次のポーリングを先へ押しやる｡
    #[test]
    fn a_manual_reload_pushes_the_next_poll_a_full_interval_out() {
        let mut situation = situation(Some(2_000), 500);
        situation.interval_seconds = 300;
        assert_eq!(next_tick(&situation, 2_001), Tick::Wait { until: 2_300 });
    }

    #[test]
    fn a_fetch_in_flight_defers_the_decision_rather_than_polling_beside_it() {
        let mut situation = situation(Some(1_000), 500);
        situation.busy = true;
        assert_eq!(
            next_tick(&situation, 9_000),
            Tick::Wait {
                until: 9_000 + BUSY_RECHECK_SECONDS
            }
        );
    }

    // --- #204: ロックされた画面と sleep ---

    // 「ロック中に何度 tick しても request が 0 回」｡期限をどれだけ過ぎて
    // いても `Poll` は返らないし､返らないので何も溜まらない｡
    #[test]
    fn a_locked_screen_never_becomes_due_however_long_it_stays_locked() {
        let mut situation = situation(Some(1_000), 500);
        situation.activity = Activity::Away;

        for now in [1_300, 2_000, 10_000, 1_000_000] {
            assert_eq!(
                next_tick(&situation, now),
                Tick::Wait {
                    until: now + activity::AWAY_RECHECK_SECONDS
                },
                "a locked screen must never poll, and {now} is well past the deadline"
            );
        }
    }

    // ロックは飛んでいる fetch より先に見る｡`busy` の再確認は数秒後に
    // ポーリングするために待つものだが､ロックされた画面にはその先が無い｡
    #[test]
    fn a_locked_screen_outranks_a_fetch_in_flight() {
        let mut situation = situation(Some(1_000), 500);
        situation.activity = Activity::Away;
        situation.busy = true;

        assert_eq!(
            next_tick(&situation, 9_000),
            Tick::Wait {
                until: 9_000 + activity::AWAY_RECHECK_SECONDS
            }
        );
    }

    // 「復帰後は最大 1 回だけ schedule される」｡ロックの間に interval は
    // 100 回ぶん過ぎているが､起点は最後の fetch ではなく復帰した瞬間だ｡
    #[test]
    fn coming_back_schedules_one_poll_an_interval_out_not_the_backlog() {
        let mut situation = situation(Some(1_000), 500);
        situation.resumed_at = Some(31_000);

        assert_eq!(next_tick(&situation, 31_000), Tick::Wait { until: 31_300 });
        assert_eq!(next_tick(&situation, 31_299), Tick::Wait { until: 31_300 });
        assert_eq!(next_tick(&situation, 31_300), Tick::Poll);

        // そしてその 1 回が `last_reload_at` を動かせば､次はまた丸ごと
        // 1 interval 先だ — 溜まっていた tick が続けて発火することは無い｡
        situation.last_reload_at = Some(31_300);
        assert_eq!(next_tick(&situation, 31_301), Tick::Wait { until: 31_600 });
    }

    // 復帰時刻は起点を **遅らせる** だけで､早めることはしない｡復帰した
    // 直後に読み手が自分で reload を押したなら､次のポーリングはその
    // reload から測る｡
    #[test]
    fn a_reload_after_coming_back_still_pushes_the_next_poll_out() {
        let mut situation = situation(Some(31_100), 500);
        situation.resumed_at = Some(31_000);

        assert_eq!(next_tick(&situation, 31_300), Tick::Wait { until: 31_400 });
    }

    // #214: footer が数える期限は､`next_tick` が `Poll` と答え始める時刻と
    // 同じ 1 つの規則から出る｡2 つが食い違えば､カウントダウンが 0 に
    // なってもポーリングが来ないか､来たのに数字が残る｡
    #[test]
    fn the_due_time_is_the_moment_the_tick_turns_into_a_poll() {
        let situation = situation(Some(1_500), 1_000);
        let due = poll_due_at(&situation);

        assert_eq!(due, 1_800);
        assert_eq!(next_tick(&situation, due - 1), Tick::Wait { until: due });
        assert_eq!(next_tick(&situation, due), Tick::Poll);
    }

    #[test]
    fn the_due_time_starts_from_the_loop_when_nothing_has_been_fetched() {
        assert_eq!(poll_due_at(&situation(None, 1_000)), 1_300);
    }

    #[test]
    fn the_due_time_starts_from_the_return_when_that_is_later() {
        let mut situation = situation(Some(31_100), 500);
        situation.resumed_at = Some(31_200);

        assert_eq!(poll_due_at(&situation), 31_500);
    }

    // --- #239: 繰り返す意味のない拒否 ---
    //
    // issue のログは同じ拒否を 130 行以上積み上げた｡下の 3 本が止める側で､
    // 続く 3 本が「止めすぎない」側だ｡後者が無いと､夜中のネットワークの
    // 瞬断ひとつで朝まで取得が死ぬ｡

    #[test]
    fn a_spend_cap_403_stops_the_poll_and_says_where_to_look() {
        let error = anyhow::Error::from(Denied {
            endpoint: rate_limit::Endpoint::ListTimeline,
            denial: Denial::Forbidden,
            detail: "Forbidden: Your monthly spend cap has been reached.".to_string(),
        });
        let reason = halting_reason(&error).unwrap();
        assert!(reason.contains("list_timeline"), "{reason}");
        assert!(reason.contains("monthly spend cap"), "{reason}");
    }

    #[test]
    fn a_401_stops_the_poll_and_points_at_signing_in_again() {
        let error = anyhow::Error::from(Denied {
            endpoint: rate_limit::Endpoint::ListTimeline,
            denial: Denial::Rejected,
            detail: "Unauthorized".to_string(),
        });
        let reason = halting_reason(&error).unwrap();
        assert!(reason.contains("Sign in with X"), "{reason}");
    }

    #[test]
    fn an_exhausted_credit_cap_stops_the_poll() {
        let error = anyhow::Error::from(rate_limit::UsageCapExceeded {
            detail: "Usage cap exceeded: Monthly product cap".to_string(),
        });
        assert!(halting_reason(&error).is_some());
    }

    #[test]
    fn a_session_that_cannot_be_renewed_stops_the_poll() {
        let error = anyhow::Error::from(oauth::SessionExpired {
            detail: "invalid_request".to_string(),
        });
        let reason = halting_reason(&error).unwrap();
        assert!(reason.contains("Sign in with X"), "{reason}");
    }

    #[test]
    fn an_ordinary_rate_limit_keeps_polling() {
        // これは待てば直る｡`decision` が次の tick を送らせないので､
        // ループを殺す理由が無い｡
        let error = anyhow::Error::from(rate_limit::RateLimited {
            reset_at: Some(1_700_000_000),
            opaque: false,
        });
        assert_eq!(halting_reason(&error), None);
    }

    #[test]
    fn a_dropped_connection_keeps_polling() {
        let error = anyhow::anyhow!("request to https://api.x.com/2/lists/1/tweets failed");
        assert_eq!(halting_reason(&error), None);
    }

    #[test]
    fn a_5xx_keeps_polling() {
        let error = anyhow::anyhow!("list_timeline: HTTP 503 — upstream unavailable");
        assert_eq!(halting_reason(&error), None);
    }
}
