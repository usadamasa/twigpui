//! 次の更新までの残り時間 (#214)｡
//!
//! ウィンドウには時計で動くものが 2 つある｡timeline の auto-refresh
//! ([`super::auto_refresh`]) と list sync ([`super::list_sync`]) だ｡
//! どちらも #214 まで「次はいつか」を画面に出していなかった｡5 分おきの
//! ポーリングは着いてはじめて分かり､6 時間おきの diff に至っては行が
//! 出るのは何かを負っているときだけで､定常状態の "up to date" は次が
//! いつかを言わない｡
//!
//! ここは 2 つの期限を同じ形で footer に出す — "Next refresh in 4m"､
//! "Next sync in 5h 12m"｡期限そのものはここで決めない｡auto-refresh の
//! 期限は [`poll_due_at`] が ([`super::auto_refresh::next_tick`] と同じ
//! 規則で) 出し､sync の期限は [`SyncStatus::Idle`] が tick から運んで
//! きた `until` をそのまま読む｡ここが持つのは残り秒数を言葉にする
//! [`countdown`] と､数字を進める ticker だけだ｡
//!
//! # なぜ分単位なのか
//!
//! 1 分より先は分だけ ("4m")､1 分を切ってから秒 ("42s")｡秒まで出すと
//! 文言が毎秒変わり､毎秒ウィンドウ全体を描き直すことになる｡#57 の
//! cooldown のバナーはそうしているが､あれは 60 秒で終わる｡こちらは
//! ウィンドウが開いている間ずっと続く｡分単位なら描き直しは 5 分に
//! 5 回で､残り 1 分だけ秒を刻めば､着く瞬間は見える｡
//!
//! ticker は 1 秒ごとに起きるが､`notify` するのは文言が変わったときだけ
//! だ｡起きること自体は何も描かず､文字列を 2 つ組むだけで済む｡

use super::auto_refresh::{Situation, poll_due_at};
use super::list_sync::SyncStatus;
use super::{Context, Duration, TimelineView, oauth};
use crate::activity::Activity;

/// 残り秒数を言葉にする｡
///
/// 1 時間以上は "5h 12m"､1 分以上は "4m"､それ未満は "42s"｡過ぎた期限は
/// "0s" — `cooldown_label` (#10) と同じ 0 下限で､負の数は決して出さない｡
/// 分と時間を丸めるのは切り捨てだ｡"4m" は 4 分 59 秒まで続き､切り上げる
/// と "5m" が 5 分より長く見えることになる｡
pub(super) fn countdown(remaining: i64) -> String {
    let remaining = remaining.max(0);
    let hours = remaining.div_euclid(3_600);
    let minutes = remaining.rem_euclid(3_600).div_euclid(60);
    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        format!("{remaining}s")
    }
}

/// auto-refresh の segment が言うこと｡ループが無ければ `None`｡
///
/// `situation` はループが直近の起床で写したもので､`last_reload_at` だけ
/// は view の今の値で上書きしてから期限を出す｡手動の reload は次の
/// ポーリングを丸ごと 1 interval 先へ押しやるが (#21)､ループがそれに
/// 気づくのは次に起きたとき — 最大 60 秒後 — で､その間 footer が古い
/// 期限を数え続ければ､0 になっても何も来ない｡
///
/// 画面がロックされている間 (#204) は期限が無い｡ループは 60 秒おきに
/// 見直すだけで､戻ってきた時刻から改めて 1 interval を数える｡ロック中に
/// 過ぎた期限を "0s" と出すと､戻ってきた読み手には今にも来るように
/// 読めるが､来るのは 1 interval 後だ｡
pub(super) fn refresh_label(
    situation: Option<&Situation>,
    last_reload_at: Option<i64>,
    now: i64,
) -> Option<String> {
    let situation = situation?;
    if matches!(situation.activity, Activity::Away) {
        return Some("Next refresh: paused".to_string());
    }
    let due = poll_due_at(&Situation {
        last_reload_at,
        ..*situation
    });
    Some(format!(
        "Next refresh in {}",
        countdown(due.saturating_sub(now))
    ))
}

/// sync が次に起きる時刻｡約束できる状態でなければ `None`｡
///
/// [`SyncStatus::Idle`] の `until` だけだ｡それが tick が返した `wake_at`
/// で､ループが実際に眠る先 ([`super::list_sync::status_of`] を見よ)｡
/// [`SyncStatus::RateLimited`] も `until` を持つが､あれは sync の行が
/// "resumes HH:MM JST" と言う (#205)｡2 か所で言えば､どちらかが古くなる｡
/// 残りの状態は次の時刻をそもそも持たない — 走っている最中か､gate で
/// 止まっているか､タイマーが切れているかだ｡
pub(super) fn sync_deadline(status: &SyncStatus) -> Option<i64> {
    match status {
        SyncStatus::Idle { until, .. } => Some(*until),
        SyncStatus::Off(_)
        | SyncStatus::Ready
        | SyncStatus::AwaitingAccount
        | SyncStatus::Working
        | SyncStatus::RateLimited { .. }
        | SyncStatus::Failed => None,
    }
}

/// sync の segment が言うこと｡[`refresh_label`] と同じ形にしてある｡
/// 並んだ 2 つの数字が別の文法で書かれていると､読み手は毎回読み直す｡
pub(super) fn sync_next_label(until: i64, now: i64) -> String {
    format!("Next sync in {}", countdown(until.saturating_sub(now)))
}

impl TimelineView {
    /// footer が今出す 2 つの文言 (#214): auto-refresh と list sync｡
    /// 出すものが無ければそれぞれ `None`｡
    pub(super) fn countdown_labels(&self, now: i64) -> (Option<String>, Option<String>) {
        (
            refresh_label(self.refresh_situation.as_ref(), self.last_reload_at, now),
            sync_deadline(&self.sync_status).map(|until| sync_next_label(until, now)),
        )
    }

    /// footer のカウントダウンを進める (#214)｡
    ///
    /// `start_cooldown_ticker` (#57) と同じ形だが､`notify` は文言が変わった
    /// ときだけだ｡理由はモジュールの doc を見よ｡数えるものが無くなれば
    /// 終わる — ループが止まり sync も次を持たないウィンドウで､1 秒ごとに
    /// 起きて何も見つけないタイマーを残さない｡
    ///
    /// 呼ぶのは期限が生まれる 2 か所: auto-refresh のループを始めたとき
    /// と､sync が次の時刻を持つ status になったとき｡代入し直すと前の
    /// ticker は drop され取り消されるので､2 つが並んで刻むことは無い｡
    pub(super) fn start_countdown_ticker(&mut self, cx: &mut Context<'_, Self>) {
        self.countdown_ticker = Some(cx.spawn(async move |this, cx| {
            let mut shown: Option<(Option<String>, Option<String>)> = None;
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;

                let Ok(keep_going) = this.update(cx, |this, cx| {
                    let labels = this.countdown_labels(oauth::unix_now());
                    if labels == (None, None) {
                        return false;
                    }
                    if shown.as_ref() != Some(&labels) {
                        cx.notify();
                        shown = Some(labels);
                    }
                    true
                }) else {
                    // view が drop された — 刻むものは何も残っていない｡
                    return;
                };

                if !keep_going {
                    return;
                }
            }
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::Activity;
    use crate::ui::auto_refresh::Situation;
    use crate::ui::list_sync::SyncStatus;

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

    // --- countdown ---

    #[test]
    fn seconds_under_a_minute_are_counted_one_by_one() {
        assert_eq!(countdown(42), "42s");
        assert_eq!(countdown(1), "1s");
    }

    #[test]
    fn minutes_are_whole_so_the_footer_does_not_redraw_every_second() {
        assert_eq!(countdown(60), "1m");
        assert_eq!(countdown(119), "1m");
        assert_eq!(countdown(299), "4m");
        assert_eq!(countdown(3_599), "59m");
    }

    #[test]
    fn hours_carry_their_minutes() {
        assert_eq!(countdown(3_600), "1h 0m");
        assert_eq!(countdown(18_720), "5h 12m");
    }

    #[test]
    fn a_deadline_already_passed_reads_as_zero_never_negative() {
        assert_eq!(countdown(0), "0s");
        assert_eq!(countdown(-5), "0s");
    }

    // --- refresh_label ---

    #[test]
    fn no_loop_means_no_label() {
        assert_eq!(refresh_label(None, None, 1_000), None);
    }

    #[test]
    fn a_fresh_window_counts_down_from_when_the_loop_started() {
        let label = refresh_label(Some(&situation(None, 1_000)), None, 1_000);
        assert_eq!(label.as_deref(), Some("Next refresh in 5m"));
    }

    #[test]
    fn the_count_moves_with_the_clock() {
        let label = refresh_label(Some(&situation(None, 1_000)), None, 1_258);
        assert_eq!(label.as_deref(), Some("Next refresh in 42s"));
    }

    // 手動の reload は次のポーリングを丸ごと 1 interval 先へ押しやる｡
    // ループが次に起きるまでの最大 60 秒､footer が古い期限を数え続けては
    // ならない — だから view の `last_reload_at` がループの写しに勝つ｡
    #[test]
    fn a_manual_reload_moves_the_count_before_the_loop_notices() {
        let stale = situation(None, 1_000);
        let label = refresh_label(Some(&stale), Some(1_200), 1_200);
        assert_eq!(label.as_deref(), Some("Next refresh in 5m"));
    }

    #[test]
    fn a_locked_screen_says_paused_rather_than_counting_a_dead_deadline() {
        let mut away = situation(None, 1_000);
        away.activity = Activity::Away;
        let label = refresh_label(Some(&away), None, 90_000);
        assert_eq!(label.as_deref(), Some("Next refresh: paused"));
    }

    #[test]
    fn a_deadline_the_loop_has_not_acted_on_yet_reads_as_zero() {
        let label = refresh_label(Some(&situation(None, 1_000)), None, 1_305);
        assert_eq!(label.as_deref(), Some("Next refresh in 0s"));
    }

    // --- sync_deadline / sync_next_label ---

    #[test]
    fn an_idle_sync_knows_when_it_wakes_next() {
        assert_eq!(
            sync_deadline(&SyncStatus::Idle {
                until: 5_000,
                pending: 0
            }),
            Some(5_000)
        );
        assert_eq!(
            sync_deadline(&SyncStatus::Idle {
                until: 5_000,
                pending: 3
            }),
            Some(5_000)
        );
    }

    // rate limit の解除予定は行のほうが JST で言う (#205)｡他の状態には
    // 次の時刻そのものが無い｡
    #[test]
    fn every_other_state_has_no_next_time_to_promise() {
        for status in [
            SyncStatus::Ready,
            SyncStatus::Working,
            SyncStatus::AwaitingAccount,
            SyncStatus::Failed,
            SyncStatus::Off(crate::ui::list_sync::SyncOff::NoList),
            SyncStatus::RateLimited {
                until: 5_000,
                pending: 3,
                refusals: 1,
            },
        ] {
            assert_eq!(sync_deadline(&status), None, "{status:?}");
        }
    }

    #[test]
    fn the_sync_count_uses_the_same_wording_as_the_refresh_count() {
        assert_eq!(sync_next_label(19_720, 1_000), "Next sync in 5h 12m");
        assert_eq!(sync_next_label(1_000, 1_000), "Next sync in 0s");
    }
}
