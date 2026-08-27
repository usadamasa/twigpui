//! 次の更新までの残り時間 (#214)｡

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
