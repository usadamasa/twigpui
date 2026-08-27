//! 次の更新までの残り時間 (#214)｡
//!
//! ウィンドウには時計で動くものが 2 つある｡timeline の auto-refresh
//! ([`super::auto_refresh`]) と list sync ([`super::list_sync`]) だ｡
//! どちらも #214 まで「次はいつか」を画面に出していなかった｡5 分おきの
//! ポーリングは着いてはじめて分かり､6 時間おきの diff に至っては行が
//! 出るのは何かを負っているときだけで､定常状態の "up to date" は次が
//! いつかを言わない｡
//!
//! ここは 2 つの期限を同じ形で出す — toolbar の reload アイコンの隣に
//! "Auto-refresh in 4m"､footer の "Sync list…" の隣に "next in 5h 12m"｡
//! 期限そのものはここで決めない｡auto-refresh の期限は [`poll_due_at`] が
//! ([`super::auto_refresh::next_tick`] と同じ規則で) 出し､sync の期限は
//! [`SyncStatus::Idle`] が tick から運んできた `until` をそのまま読む｡
//! ここが持つのは残り秒数を言葉にする [`countdown`] と､幅に合わせて
//! 文言を選ぶ [`Density`] と､数字を進める ticker だけだ｡
//!
//! # 置き場所と幅
//!
//! 最初は両方を footer に置いた｡550px の fixture ですら "posts kept" が
//! 右端から落ちた｡footer にはリクエスト数と sync の入口と post の数が
//! すでに並んでいて､本番で実際に使われている 429px では､それだけで
//! 幅の 9 割が埋まる｡
//!
//! だから auto-refresh の期限は toolbar へ — それが次に押すことになる
//! reload のアイコンの隣は､どのみち読みやすい場所だ — sync の期限は
//! footer に残し､文言を幅で選ぶ ([`density`])｡広ければ "next in 5h 12m"､
//! 狭ければ "5h 12m"｡post の数も同じ段で "posts kept" から "posts" へ
//! 縮む｡それでも入らないときは､右端の post の数を落とすのではなく
//! sync の期限を "…" で切る (`chrome::status_bar` の `truncate`)｡
//! 数字が読めないより､どこが読めていないか分かるほうがよい｡
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

use gpui::{Pixels, px};

use super::auto_refresh::{Situation, poll_due_at};
use super::list_sync::SyncStatus;
use super::{Context, Duration, TimelineView, oauth};
use crate::activity::Activity;

/// 枠 (toolbar と footer) の文言をどれだけ詰めるか (#214)｡
///
/// ウィンドウの幅から [`density`] が決め､描画のたびに読み直すので､
/// ウィンドウを引き伸ばせば文言も戻る｡2 段しか無いのは､3 段目に
/// 縮める先が無いからだ — "5h 12m" より短い残り時間の書き方は無い｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Density {
    /// 全部書く: "Auto-refresh in 4m"､"next in 5h 12m"､"posts kept"｡
    Wide,
    /// 主語を落とす: "in 4m"､"5h 12m"､"posts"｡
    Compact,
}

/// この幅を下回ると [`Density::Compact`] になる｡
///
/// 実測から置いた｡`Wide` の footer は本番のフォントで 478px 要る
/// (usage 140､入口 54､"next in 5h 59m" 82､"N / 500 posts kept" 118､
/// 余白 84)｡520 は `Wide` に 40px の余裕を残す — フォントや桁数の
/// 揺れで "…" が出たり消えたりする境目を､使う幅から離しておく｡
/// 既定のウィンドウ (560px) は `Wide`､本番の 429px は `Compact`｡
pub(super) const COMPACT_BELOW: Pixels = px(520.);

/// ウィンドウの幅から [`Density`] を選ぶ｡
pub(super) fn density(width: Pixels) -> Density {
    if width < COMPACT_BELOW {
        Density::Compact
    } else {
        Density::Wide
    }
}

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

/// toolbar の auto-refresh の segment が言うこと｡ループが無ければ `None`｡
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
///
/// `Compact` は主語を落とす｡reload のアイコンのすぐ左に座るので､
/// "in 4m" だけでもそのアイコンの話だと読める｡
pub(super) fn refresh_label(
    situation: Option<&Situation>,
    last_reload_at: Option<i64>,
    now: i64,
    density: Density,
) -> Option<String> {
    let situation = situation?;
    if matches!(situation.activity, Activity::Away) {
        return Some(match density {
            Density::Wide => "Auto-refresh paused".to_string(),
            Density::Compact => "paused".to_string(),
        });
    }
    let due = poll_due_at(&Situation {
        last_reload_at,
        ..*situation
    });
    let remaining = countdown(due.saturating_sub(now));
    Some(match density {
        Density::Wide => format!("Auto-refresh in {remaining}"),
        Density::Compact => format!("in {remaining}"),
    })
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

/// footer の sync の segment が言うこと｡"Sync list…" の入口のすぐ隣に
/// 座るので主語を繰り返さない — "Sync list… next in 5h 12m" と読める｡
/// `Compact` では "next in" も落とす｡入口の隣の薄い数字は､それだけで
/// 次の時刻だと読める｡
pub(super) fn sync_next_label(until: i64, now: i64, density: Density) -> String {
    let remaining = countdown(until.saturating_sub(now));
    match density {
        Density::Wide => format!("next in {remaining}"),
        Density::Compact => remaining,
    }
}

/// footer の右端､保持している post の数 (#95)｡`Compact` では "kept" を
/// 落とす｡"N / 500 posts" だけで上限に対する数だと読める｡
pub(super) fn kept_label(kept: usize, cap: usize, density: Density) -> String {
    match density {
        Density::Wide => format!("{kept} / {cap} posts kept"),
        Density::Compact => format!("{kept} / {cap} posts"),
    }
}

impl TimelineView {
    /// ウィンドウが今出す 2 つの文言 (#214): toolbar の auto-refresh と
    /// footer の list sync｡出すものが無ければそれぞれ `None`｡
    pub(super) fn countdown_labels(
        &self,
        now: i64,
        density: Density,
    ) -> (Option<String>, Option<String>) {
        (
            refresh_label(
                self.refresh_situation.as_ref(),
                self.last_reload_at,
                now,
                density,
            ),
            sync_deadline(&self.sync_status).map(|until| sync_next_label(until, now, density)),
        )
    }

    /// カウントダウンを進める (#214)｡
    ///
    /// `start_cooldown_ticker` (#57) と同じ形だが､`notify` は文言が変わった
    /// ときだけだ｡理由はモジュールの doc を見よ｡数えるものが無くなれば
    /// 終わる — ループが止まり sync も次を持たないウィンドウで､1 秒ごとに
    /// 起きて何も見つけないタイマーを残さない｡
    ///
    /// 呼ぶのは期限が生まれる 2 か所: auto-refresh のループを始めたとき
    /// と､sync が次の時刻を持つ status になったとき｡代入し直すと前の
    /// ticker は drop され取り消されるので､2 つが並んで刻むことは無い｡
    ///
    /// 文言は [`Density::Wide`] で比べる｡ticker はウィンドウの幅を知らない
    /// が､知る必要も無い — 2 つの密度は同じ [`countdown`] を包んでいる
    /// だけなので､片方が変わる瞬間はもう片方が変わる瞬間と同じだ｡
    pub(super) fn start_countdown_ticker(&mut self, cx: &mut Context<'_, Self>) {
        self.countdown_ticker = Some(cx.spawn(async move |this, cx| {
            let mut shown: Option<(Option<String>, Option<String>)> = None;
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;

                let Ok(keep_going) = this.update(cx, |this, cx| {
                    let labels = this.countdown_labels(oauth::unix_now(), Density::Wide);
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

    // --- density ---

    #[test]
    fn the_default_window_is_wide_and_the_production_window_is_compact() {
        assert_eq!(density(px(560.)), Density::Wide);
        assert_eq!(density(px(429.)), Density::Compact);
    }

    #[test]
    fn the_threshold_itself_is_still_wide() {
        assert_eq!(density(COMPACT_BELOW), Density::Wide);
        assert_eq!(density(COMPACT_BELOW - px(1.)), Density::Compact);
    }

    // --- refresh_label ---

    #[test]
    fn no_loop_means_no_label() {
        assert_eq!(refresh_label(None, None, 1_000, Density::Wide), None);
    }

    #[test]
    fn a_fresh_window_counts_down_from_when_the_loop_started() {
        let label = refresh_label(Some(&situation(None, 1_000)), None, 1_000, Density::Wide);
        assert_eq!(label.as_deref(), Some("Auto-refresh in 5m"));
    }

    #[test]
    fn the_count_moves_with_the_clock() {
        let label = refresh_label(Some(&situation(None, 1_000)), None, 1_258, Density::Wide);
        assert_eq!(label.as_deref(), Some("Auto-refresh in 42s"));
    }

    // 手動の reload は次のポーリングを丸ごと 1 interval 先へ押しやる｡
    // ループが次に起きるまでの最大 60 秒､footer が古い期限を数え続けては
    // ならない — だから view の `last_reload_at` がループの写しに勝つ｡
    #[test]
    fn a_manual_reload_moves_the_count_before_the_loop_notices() {
        let stale = situation(None, 1_000);
        let label = refresh_label(Some(&stale), Some(1_200), 1_200, Density::Wide);
        assert_eq!(label.as_deref(), Some("Auto-refresh in 5m"));
    }

    #[test]
    fn a_locked_screen_says_paused_rather_than_counting_a_dead_deadline() {
        let mut away = situation(None, 1_000);
        away.activity = Activity::Away;
        let label = refresh_label(Some(&away), None, 90_000, Density::Wide);
        assert_eq!(label.as_deref(), Some("Auto-refresh paused"));
        let label = refresh_label(Some(&away), None, 90_000, Density::Compact);
        assert_eq!(label.as_deref(), Some("paused"));
    }

    #[test]
    fn a_deadline_the_loop_has_not_acted_on_yet_reads_as_zero() {
        let label = refresh_label(Some(&situation(None, 1_000)), None, 1_305, Density::Wide);
        assert_eq!(label.as_deref(), Some("Auto-refresh in 0s"));
    }

    #[test]
    fn a_narrow_window_drops_the_subject_next_to_the_reload_icon() {
        let label = refresh_label(Some(&situation(None, 1_000)), None, 1_000, Density::Compact);
        assert_eq!(label.as_deref(), Some("in 5m"));
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
    fn the_sync_count_follows_its_entry_without_repeating_the_subject() {
        assert_eq!(
            sync_next_label(19_720, 1_000, Density::Wide),
            "next in 5h 12m"
        );
        assert_eq!(sync_next_label(1_000, 1_000, Density::Wide), "next in 0s");
    }

    #[test]
    fn a_narrow_window_keeps_only_the_number_by_the_sync_entry() {
        assert_eq!(sync_next_label(19_720, 1_000, Density::Compact), "5h 12m");
    }

    // --- kept_label ---

    #[test]
    fn the_post_count_drops_kept_when_the_window_is_narrow() {
        assert_eq!(kept_label(10, 500, Density::Wide), "10 / 500 posts kept");
        assert_eq!(kept_label(10, 500, Density::Compact), "10 / 500 posts");
    }
}
